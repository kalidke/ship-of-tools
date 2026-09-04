// capsule_workspace.rs — ADR 0042 slice L1a: the daemon's capsule
// workspace runtime (Windows-first). Every NEW workspace on Windows is a
// capsule: one `sot-capsule supervise <state-dir>` authority per
// workspace, spawned DETACHED so it survives the daemon's own exit — the
// daemon is never its kill domain. `runtime: "tmux"` rows stay exactly
// what they are today; this module never touches them.
//
// Split deliberately into PURE helpers (no OS call: the state-dir path
// arithmetic, the phase-to-wire-string mapping, the agent argv choice)
// and the WINDOWS-ONLY runtime (spawning, watching, querying, ending a
// supervisor over `sot_log::supervisor_client`). The pure half is
// compiled and unit-tested on every platform — ADR 0042 L1a's own gate
// runs `cargo test --workspace` on Linux, and gating path/string
// arithmetic behind `#[cfg(windows)]` would only prevent that gate from
// ever exercising it. On non-Windows hosts nothing in this module is
// called at all: `workspace.create` keeps today's tmux path unchanged
// (see `workspaces.rs`/`handlers.rs`).

use std::path::{Path, PathBuf};

/// `<state-root>/workspaces/<workspace_id>/` — the capsule's own state
/// directory (ADR 0041/0042: `supervisor.lock`, `drawer.voyage`, the
/// journal, the voyages — all owned and created by `sot-capsule
/// supervise` itself, as its own first act after it actually runs; rule
/// C, shrink round: this daemon never creates it — a synchronous spawn
/// failure then leaves nothing on disk at all). `state_root` is
/// `sot_log::state_dir::sot_state_dir()`, injected rather than resolved
/// here so this stays a pure function of its inputs (real callers
/// resolve it once; a test supplies a tempdir root).
pub fn state_dir_for(state_root: &Path, workspace_id: &str) -> PathBuf {
    state_root.join("workspaces").join(workspace_id)
}

/// The agent argv `sot-capsule supervise` spawns as its producer (ADR
/// 0042 L1a: "the same launcher the drawer's autostart uses when
/// `autostart_claude` is set; otherwise the platform shell"). No Windows
/// equivalent of the Unix `ccb`/`ccx` launchers exists anywhere in this
/// repo (both are bash scripts — `comm/adapters/claude/bin/ccb`,
/// `comm/adapters/codex/bin/ccx` — with no `.ps1`/`.exe` counterpart, and
/// ADR 0041's own drawer capsule is deliberately a RAW TERMINAL voyage,
/// not yet wired to any launcher either — U4, the drawer cutover, is
/// still unbuilt). `"claude"` gets the closest honest equivalent: the
/// same flags `ccb` itself execs with (`claude --permission-mode auto
/// /sot-session-start`), relying on `claude` being on the
/// daemon's own PATH — a detached child inherits it, same as any spawned
/// process. `"none"` is the explicit bare platform shell. Every other
/// kind (`"codex"` included — no known Windows launcher exists) is
/// REFUSED (ADR 0042 L1a, Codex review finding 9): silently substituting
/// `cmd.exe` for a kind the caller explicitly asked for would launch
/// something the caller never requested and never learn about it.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn agent_argv(agent_kind: &str) -> Result<Vec<String>, String> {
    match agent_kind {
        "none" => Ok(vec!["cmd.exe".to_string()]),
        "claude" => Ok(vec![
            "claude".to_string(),
            "--permission-mode".to_string(),
            "auto".to_string(),
            "/sot-session-start".to_string(),
        ]),
        other => Err(format!(
            "agent {other:?} has no Windows capsule launcher yet (only \"claude\" and \"none\" are supported on this host)"
        )),
    }
}

/// `sot-capsule supervise`'s own start-mode flag.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn mode_flag(mode: StartMode) -> &'static str {
    match mode {
        StartMode::Start => "--start",
        StartMode::Resume => "--resume",
    }
}

/// Mirrors `sot_log::supervisor::StartMode` (portable re-statement: that
/// type lives in a `#![cfg(windows)]` module, and this crate's own pure
/// tests need to name a mode without pulling in a Windows-only type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum StartMode {
    Start,
    Resume,
}

/// The wire phase string `workspace.list` reports (`WorkspaceListEntry.phase`)
/// for a capsule workspace whose supervisor lane could not be reached at
/// all — connect refused, the challenge proving foreign/undetermined, or a
/// timeout (ADR 0042 L1a: "failure -> unreachable"). Distinct from every
/// [`sot_log::wire::SupervisorPhase`] variant, which are all states of a
/// lane that DID answer.
pub const UNREACHABLE_PHASE: &str = "unreachable";

/// The wire phase string for a capsule workspace with no published
/// voyage pointer (`<state_dir>/drawer.voyage`, `sot_log::pointer` —
/// ADR 0041 Lifecycle's write-once durable fact that a voyage exists).
/// Rule B (shrink round): the POINTER, not directory presence, is the
/// discriminator — a state directory can exist with no pointer ever
/// published to it (the exact pre-pointer crash window ADR 0041 names,
/// or simply a row `resume_all` correctly never touched because it had
/// none), and that reads identically to a workspace whose directory was
/// never created at all: neither has ever had a real run. Distinct from
/// [`UNREACHABLE_PHASE`]: a workspace WITH a published pointer means a
/// supervisor did reach a real run at least once, so a lane that fails to
/// answer against it stays "unreachable" — `query_status`'s own doc
/// deliberately folds every such failure (connect refused, a foreign/
/// undetermined challenge, a timeout) into one `Err` without saying
/// which, so a query against a workspace WITH a pointer can never be
/// reclassified as "never started" either. One narrow race this accepts
/// (Codex round, PR #172): a `workspace.list` landing in the brief window
/// where a resumed supervisor's pointer is still being (re-)published
/// reads "stopped" too — bounded by the spawn call itself and
/// self-correcting on the very next list once the pointer (and the
/// supervisor behind it) exists.
#[cfg_attr(not(windows), allow(dead_code))]
pub const NEVER_STARTED_PHASE: &str = "stopped";

/// Whether a capsule workspace's supervisor lane is even worth querying,
/// given whether its voyage pointer exists — pure, no I/O itself (the
/// caller supplies `pointer_exists`, e.g. `phase_of`'s own
/// `sot_log::pointer::pointer_path(state_dir).is_file()`). `None` means
/// "query it, we can't tell from this alone"; `Some(..)` short-circuits a
/// connect attempt that cannot possibly succeed — no pipe was ever bound
/// for a workspace whose pointer was never published, so `phase_of` skips
/// straight to [`NEVER_STARTED_PHASE`] rather than waiting out a connect
/// budget destined to fail. The pointer lives INSIDE the state dir, so
/// its absence subsumes "no state dir at all" (the check this replaces)
/// as well as "a state dir exists but nothing was ever durably published
/// to it" — both read as never started.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn phase_for_missing_pointer(pointer_exists: bool) -> Option<&'static str> {
    (!pointer_exists).then_some(NEVER_STARTED_PHASE)
}

/// Map the supervisor lane's own phase to the wire string
/// `workspace.list` reports — snake_case, matching every other
/// wire-enum-as-string in this protocol (`repl_state`, `agent_state`).
/// Portable: [`sot_log::wire`] has no OS dependency (see that crate's own
/// module doc), so this needs no `#[cfg(windows)]` either, and the pure
/// unit tests below exercise it directly on Linux.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn phase_str(phase: sot_log::wire::SupervisorPhase) -> &'static str {
    use sot_log::wire::SupervisorPhase;
    match phase {
        SupervisorPhase::Starting => "starting",
        SupervisorPhase::Ready => "ready",
        SupervisorPhase::Ending => "ending",
        SupervisorPhase::EndedNoRespawn => "ended_no_respawn",
        SupervisorPhase::Terminal => "terminal",
    }
}

/// ADR 0042 L1a (Codex review finding 6): the daemon's own watchdog
/// restart budget for a capsule supervisor — ADR 0041's own launcher
/// restart sequence ("restart with `--resume` on the launcher's shipped
/// 1/3/7/15/30 s sequence, at most 5 restarts in 60 s, then stop and
/// report"). The daemon has become that launcher for every capsule
/// workspace it creates or resumes, so this is the ADR's own row, not
/// new policy.
#[cfg_attr(not(windows), allow(dead_code))]
pub const RESTART_BACKOFFS: [std::time::Duration; 5] = [
    std::time::Duration::from_secs(1),
    std::time::Duration::from_secs(3),
    std::time::Duration::from_secs(7),
    std::time::Duration::from_secs(15),
    std::time::Duration::from_secs(30),
];
#[cfg_attr(not(windows), allow(dead_code))]
pub const MAX_RESTARTS_PER_WINDOW: usize = 5;
#[cfg_attr(not(windows), allow(dead_code))]
pub const RESTART_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// ADR 0042 L1a (Codex review findings 10/11): "a small semaphore over
/// spawns" — the SAME fixed-width bound reused for both the startup
/// resume-scan's concurrent spawns and `workspace.list`'s concurrent
/// lane queries, rather than two independently-invented numbers.
#[cfg_attr(not(windows), allow(dead_code))]
pub const LANE_CONCURRENCY: usize = 4;

/// ADR 0042 L1a, Codex review finding 11: ONE absolute deadline over
/// `workspace.list`'s WHOLE lane-query gather — never a fresh budget per
/// row, which let total call time grow with row count. Generous over a
/// single `query_status` call's own worst case (connect 2s + hello 2s +
/// status 5s ~= 9s) to give `LANE_CONCURRENCY`-wide batches room to
/// drain; a row not yet resolved when this expires simply reports
/// "unreachable" — never blocks the ones that did answer.
#[cfg_attr(not(windows), allow(dead_code))]
pub const LIST_LANE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);

/// Environment variables scrubbed from the spawned supervisor's (and
/// hence its capsule leg's) environment before launch — the exact list
/// `comm/adapters/claude/bin/ccb` unsets, for the identical reason: a
/// spawning parent's own Claude Code nesting markers make a fresh
/// `claude` mis-detect itself as nested/forked and exit silently.
/// `CLAUDECODE`/`AI_AGENT`/`CLAUDE_CODE_SESSION_ID` make it think it is
/// running INSIDE another claude; `CLAUDE_CODE_FORK_SUBAGENT`/
/// `CLAUDE_CODE_CHILD_SESSION`/`CLAUDE_CODE_TEAMMATE_MODE`/
/// `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS` make it think it is a forked/
/// teammate session. A daemon started from within a claude session (or
/// restarted by one) would otherwise propagate every one of these into
/// every capsule it spawns (ADR 0042 L1a, Codex review finding 9).
#[cfg_attr(not(windows), allow(dead_code))]
pub const NESTING_ENV_VARS_TO_SCRUB: &[&str] = &[
    "CLAUDE_CODE_FORK_SUBAGENT",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_TEAMMATE_MODE",
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
    "CLAUDECODE",
    "AI_AGENT",
    "CLAUDE_CODE_SESSION_ID",
];

/// The `SOT_COMM_HOME` value (forward-slash string form — a git-bash/MSYS
/// shell, not this native Windows process, is what reads it; MSYS accepts
/// either slash spelling for a drive-letter-absolute path) to hand a
/// capsule's producer env, so its own comm scripts resolve the EXACT SAME
/// home this builds `SOT_COMM_SELF_FILE` from below — Codex round finding
/// 8: ONE resolver ([`crate::paths::sot_comm_home`], also what the
/// daemon's own registry reads use, `handlers::comm_registry_path`),
/// injected into the child explicitly rather than left to each side's own
/// HOME/USERPROFILE guess landing on two different answers. `None` when
/// the resolver itself found nothing (matches comm-lib.sh: nothing to
/// pin).
#[cfg_attr(not(windows), allow(dead_code))]
fn capsule_comm_home_str() -> Option<String> {
    Some(crate::paths::sot_comm_home()?.to_string_lossy().replace('\\', "/"))
}

/// The `SOT_*` awareness env a capsule supervisor spawn stamps on its
/// producer — capsule-comm-identity fix: a capsule has no tmux pane, so
/// `comm-context.sh`'s pane-keyed self-file slot never applies to it, and
/// without `SOT_COMM_HOME`/`SOT_COMM_SELF_FILE` it fell back to the
/// shared per-host `__nopane` slot, colliding with any other no-pane
/// session on the same host (e.g. the frontend itself — the field bug
/// this fixes). `SOT_WORKSPACE` (and `SOT_WORKSPACE_ROOT`/`SOT_SESSION`/
/// `SOT_MANUAL`) reuse [`crate::pty::awareness_env`] verbatim — ONE
/// builder, not a second copy that could drift — keyed on `slug` (Codex
/// round finding 1: the frontend keys results and the active workspace by
/// SLUG, not the internal `ws-<slug>-<hex>` id; `workspace_id` is used
/// ONLY below, for the state dir and self-file paths, where stability
/// and uniqueness matter more than the display shape). `SOT_COMM_NAME` is
/// set ONLY for an explicitly requested `agent_name` (Codex round finding
/// 2: a synthesized default here would become an explicit pin that
/// OVERWRITES any existing registry row of that name — exactly what
/// PROTOCOL.md's "never reuse a handle" forbids, and a hand-started
/// session in the same repo on the same host derives precisely
/// `<slug>-<host>` on its own). `SOT_COMM_SELF_FILE`
/// (`<comm_home>/self/<host>__<workspace_id>.txt`, comm-lib.sh's EXISTING
/// pin-the-self-file-path seam — already used by its own test suite, and
/// already honoured unchanged by both `comm-context.sh`, the reader, and
/// `comm-join.sh`, the writer) is what actually gives the capsule its own
/// slot: `comm-join.sh`'s #148 auto-disambiguating derivation decides the
/// handle and writes it there; the daemon later reads that same file's
/// first line back to learn it (`handlers::capsule_comm_handle`). Pure
/// (no I/O beyond env reads): exercised by the cross-platform test suite
/// even though [`windows_runtime::spawn_detached_supervisor`], its only
/// caller, is Windows-only.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn capsule_supervisor_env(workspace_id: &str, slug: &str, cwd: &Path, agent_name: &str) -> Vec<(String, String)> {
    let mut env = crate::pty::awareness_env(Some(slug), Some(cwd));
    if !agent_name.is_empty() {
        env.push(("SOT_COMM_NAME".to_string(), agent_name.to_string()));
    }
    if let Some(comm_home) = capsule_comm_home_str() {
        let host = crate::workspaces::state_host();
        let self_file = format!("{}/self/{}__{}.txt", comm_home.trim_end_matches('/'), host, workspace_id);
        env.push(("SOT_COMM_HOME".to_string(), comm_home));
        env.push(("SOT_COMM_SELF_FILE".to_string(), self_file));
    }
    env
}

#[cfg(windows)]
mod windows_runtime {
    use super::{
        agent_argv, capsule_supervisor_env, mode_flag, StartMode, LANE_CONCURRENCY, LIST_LANE_DEADLINE,
        MAX_RESTARTS_PER_WINDOW, NESTING_ENV_VARS_TO_SCRUB, NEVER_STARTED_PHASE, RESTART_BACKOFFS,
        RESTART_WINDOW, UNREACHABLE_PHASE,
    };
    use crate::workspaces::Workspaces;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use std::process::Stdio;
    use tokio::process::{Child, Command};

    /// `DETACHED_PROCESS` (Win32): the child gets no console of its own —
    /// right for a background authority that is never an interactive
    /// console session.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    /// `CREATE_NEW_PROCESS_GROUP`: the supervisor becomes its own process
    /// group, so a Ctrl+C delivered to the daemon's own console (if any)
    /// never propagates to a process the daemon just detached from itself.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    /// `CREATE_BREAKAWAY_FROM_JOB`: per MSDN, ignored when the calling
    /// process is not itself in a job — so there is nothing to probe
    /// first for the common case. When the daemon IS in a job whose limit
    /// flags lack `JOB_OBJECT_LIMIT_BREAKAWAY_OK`, `CreateProcess` fails
    /// `ERROR_ACCESS_DENIED` rather than silently dropping the flag —
    /// exactly the signal [`spawn_detached_supervisor`] uses to fall back
    /// to a DEGRADED, still-in-job spawn (ADR 0042 L1a: "breakaway attempt
    /// if the daemon is in a job, DEGRADED otherwise").
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    /// Win32 `ERROR_ACCESS_DENIED` — what a denied breakaway attempt
    /// reports on `CreateProcess`.
    const ERROR_ACCESS_DENIED: i32 = 5;
    /// `sot-capsule supervise`'s own clean-exit code (`EXIT_CLEAN`).
    const EXIT_CLEAN: i32 = 0;
    /// `sot-capsule supervise`'s own terminal-failure exit code
    /// (`EXIT_TERMINAL`) — unconditionally terminal to
    /// [`wait_and_classify`], never restarted (rule F, shrink round).
    const EXIT_TERMINAL: i32 = 69;

    /// `sot-capsule[.exe]`, resolved next to the daemon's own executable —
    /// "the `sot-capsule` binary path: next to the daemon's own
    /// executable (`current_exe().parent()`), which is where the install
    /// layout puts it" (ADR 0042 L1a).
    pub fn sot_capsule_exe() -> std::io::Result<PathBuf> {
        let exe = std::env::current_exe()?;
        let dir = exe.parent().ok_or_else(|| {
            std::io::Error::new(ErrorKind::NotFound, "daemon executable has no parent directory")
        })?;
        Ok(dir.join("sot-capsule.exe"))
    }

    /// What [`spawn_detached_supervisor`] reports about how the spawn went.
    struct SpawnedSupervisor {
        child: Child,
        /// `true` iff the breakaway attempt was denied and this
        /// supervisor was spawned still inside the daemon's own job — it
        /// will die if that job is ever closed with
        /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Recorded on the wire via
        /// `--survival degraded` (ADR 0042 L1a, Codex review finding 7),
        /// so the capsule's own status/record are truthful, not merely
        /// logged here.
        degraded: bool,
    }

    /// Spawn `sot-capsule.exe supervise <state_dir> <--start|--resume>
    /// --survival <normal|degraded> --assume-no-rollback-target --
    /// <agent argv>` DETACHED, so the supervisor authority survives the
    /// daemon's own exit — the daemon must not be its kill domain (ADR
    /// 0042 L1a). `--assume-no-rollback-target` is mandatory:
    /// `sot_log::supervisor::supervise` itself refuses (exit 69) without
    /// it pre-U4. The nesting env vars are scrubbed and `SOT_COMM_NAME`
    /// exported (Codex review finding 9) — the same contract
    /// `boot_wrapper_command`'s tmux path already gives every autostart
    /// workspace.
    ///
    /// Attempts `CREATE_BREAKAWAY_FROM_JOB` unconditionally alongside
    /// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`: a denied breakaway
    /// reports `ERROR_ACCESS_DENIED` distinctly from every other spawn
    /// failure (a missing binary, an invalid argv, …), which is exactly
    /// the signal that separates "retry without it, DEGRADED" from
    /// "propagate the real error".
    fn spawn_detached_supervisor(
        sot_capsule_exe: &Path,
        state_dir: &Path,
        mode: StartMode,
        agent_argv: &[String],
        cwd: &Path,
        agent_name: &str,
        workspace_id: &str,
        slug: &str,
    ) -> std::io::Result<SpawnedSupervisor> {
        let base_flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        let build = |flags: u32, survival: &str| -> Command {
            // `tokio::process::Command` re-exposes `.creation_flags()`
            // natively (no `std::os::windows::process::CommandExt`
            // import needed, unlike `std::process::Command`).
            let mut cmd = Command::new(sot_capsule_exe);
            cmd.arg("supervise")
                .arg(state_dir)
                .arg(mode_flag(mode))
                .arg("--survival")
                .arg(survival)
                .arg("--assume-no-rollback-target")
                .arg("--")
                .args(agent_argv)
                .current_dir(cwd)
                .creation_flags(flags)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            for var in NESTING_ENV_VARS_TO_SCRUB {
                cmd.env_remove(var);
            }
            for (k, v) in capsule_supervisor_env(workspace_id, slug, cwd, agent_name) {
                cmd.env(k, v);
            }
            cmd
        };
        match build(base_flags | CREATE_BREAKAWAY_FROM_JOB, "normal").spawn() {
            Ok(child) => Ok(SpawnedSupervisor { child, degraded: false }),
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                tracing::warn!(
                    state_dir = ?state_dir,
                    "capsule supervisor breakaway denied — spawning DEGRADED (still in the daemon's job)"
                );
                let child = build(base_flags, "degraded").spawn()?;
                Ok(SpawnedSupervisor { child, degraded: true })
            }
            Err(e) => Err(e),
        }
    }

    /// One capsule workspace's supervisor-lane status, as the daemon's
    /// own wire vocabulary — never the raw `sot_log` types, so
    /// `handlers.rs` has nothing Windows-specific to import. BLOCKING —
    /// callers run it via `spawn_blocking`.
    ///
    /// Rule B (shrink round): a workspace with no published voyage
    /// pointer (`sot_log::pointer::pointer_path`) — no state dir at all,
    /// or one that exists but nothing was ever durably published to —
    /// short-circuits to `NEVER_STARTED_PHASE` ("stopped") BEFORE
    /// attempting a connect that cannot possibly succeed; only a
    /// workspace WITH a published pointer falls through to the real
    /// query, where a failure stays `UNREACHABLE_PHASE`.
    pub fn phase_of(state_dir: &Path) -> &'static str {
        if let Some(phase) =
            super::phase_for_missing_pointer(sot_log::pointer::pointer_path(state_dir).is_file())
        {
            return phase;
        }
        match sot_log::supervisor_client::query_status(state_dir) {
            Ok(report) => super::phase_str(report.phase),
            Err(e) => {
                tracing::debug!(state_dir = ?state_dir, error = %e, "capsule workspace: supervisor lane unreachable");
                super::UNREACHABLE_PHASE
            }
        }
    }

    /// `workspace.delete` on a capsule workspace: send `end_run {reason,
    /// voyage}` on the lane (bounded by `sot_log::supervisor_client::
    /// end_run`'s own ADR-pinned 90s cutoff — see that function's own
    /// doc), reporting the outcome honestly. `Ok(None)` means the
    /// workspace never had a leg to end (no voyage observed yet —
    /// nothing to do). The state directory is NEVER deleted here: the
    /// record persists by design. BLOCKING — callers run it via
    /// `spawn_blocking`.
    pub fn end_run(
        state_dir: &Path,
        reason: &str,
    ) -> std::io::Result<Option<sot_log::supervisor_client::EndRunOutcome>> {
        let status = sot_log::supervisor_client::query_status(state_dir)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let Some(voyage) = status.voyage else {
            return Ok(None);
        };
        let outcome = sot_log::supervisor_client::end_run(state_dir, &voyage, reason)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(Some(outcome))
    }

    /// Spawn a capsule's supervisor authority AND hand it to a watchdog
    /// task together (ADR 0042 L1a, Codex review finding 6: "hand every
    /// spawned Child to a waiter task") — the daemon has become ADR
    /// 0041's own launcher for every capsule workspace it creates or
    /// resumes. Returns synchronously once the FIRST spawn attempt is
    /// known to have succeeded or failed, so a caller (`workspace.create`,
    /// finding 1) can roll back on a synchronous failure; the watchdog
    /// itself then runs entirely in the background. Clears any prior
    /// `capsule_terminal` mark for this workspace — a fresh spawn is a
    /// fresh chance.
    ///
    /// Rule D: releases `start_supervisor`'s `starting` claim
    /// (`Workspaces::end_capsule_start`) via TWO independent,
    /// idempotently-converging mechanisms — "removes when the lane first
    /// answers or the child exits": [`spawn_starting_release_poll`]
    /// releases it the moment the lane first answers (bounded, so a leg
    /// that never binds its lane at all doesn't wedge it open forever),
    /// and [`spawn_watchdog`]'s own loop releases it on every leg-exit
    /// (first leg AND every restart) as a second, unconditional path.
    /// `HashSet::remove` is idempotent, so whichever fires first actually
    /// clears the entry; the other is a harmless no-op.
    pub fn spawn_and_watch(
        sot_capsule_exe: &Path,
        state_dir: &Path,
        mode: StartMode,
        agent_argv: &[String],
        cwd: &Path,
        agent_name: &str,
        workspace_id: String,
        slug: String,
        workspaces: Workspaces,
    ) -> std::io::Result<bool> {
        let spawned =
            spawn_detached_supervisor(sot_capsule_exe, state_dir, mode, agent_argv, cwd, agent_name, &workspace_id, &slug)?;
        let degraded = spawned.degraded;
        workspaces.clear_capsule_terminal(&workspace_id);
        spawn_starting_release_poll(workspace_id.clone(), state_dir.to_path_buf(), workspaces.clone());
        spawn_watchdog(
            workspace_id,
            sot_capsule_exe.to_path_buf(),
            state_dir.to_path_buf(),
            agent_argv.to_vec(),
            cwd.to_path_buf(),
            agent_name.to_string(),
            slug,
            spawned.child,
            workspaces,
        );
        Ok(degraded)
    }

    /// Rule D half 1: release this workspace's `starting` claim the
    /// moment its lane first answers a status query, bounded by
    /// [`LIST_LANE_DEADLINE`] so a leg that never binds a lane at all
    /// (crashes before `PipeServer::bind_supervisor`) doesn't wedge the
    /// flag open forever — [`spawn_watchdog`]'s own child-exit release
    /// (half 2) still applies in that case.
    fn spawn_starting_release_poll(workspace_id: String, state_dir: PathBuf, workspaces: Workspaces) {
        tokio::spawn(async move {
            let deadline = Instant::now() + LIST_LANE_DEADLINE;
            loop {
                let dir = state_dir.clone();
                let answered = tokio::task::spawn_blocking(move || sot_log::supervisor_client::query_status(&dir).is_ok())
                    .await
                    .unwrap_or(false);
                if answered {
                    workspaces.end_capsule_start(&workspace_id);
                    return;
                }
                if Instant::now() >= deadline {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
    }

    /// The spawn path shared by `workspace.create` (mode `Start` always — a
    /// brand new workspace has no state dir yet), `pty.open`'s
    /// start-on-attach ([`ensure_started`], mode picked by
    /// [`start_mode_needed`]), and `resume_all`: locate `sot-capsule.exe`
    /// and spawn-and-watch it. ADR 0042 L1a Codex review finding 1's
    /// synchronous-failure contract applies to every caller: an `Err`
    /// here means no supervisor is running, and the caller must refuse
    /// its own op with this text rather than silently proceeding.
    ///
    /// Rule C (shrink round): does NOT create the state directory —
    /// `sot-capsule supervise` creates its own (`supervise_inner`'s first
    /// act, `rust/log/src/supervisor.rs`) once it actually runs, so a
    /// synchronous spawn failure here leaves nothing behind at all, not
    /// even an empty directory a later `phase_of` could misread.
    ///
    /// Rule D (shrink round): AT MOST ONE launch per workspace at a
    /// time — `Workspaces::try_begin_capsule_start` claims the slot
    /// ATOMICALLY (insert-if-absent under the SAME `Workspaces` mutex, no
    /// new lock type) as this function's very first act; a caller that
    /// loses the race gets `Ok(None)` — spawns nothing, not an error —
    /// so `pty.open`'s caller can still answer `attach_direct` and let
    /// the frontend's own attach-retry find the lane once it comes up.
    /// The slot is released by [`spawn_and_watch`]'s own two independent,
    /// idempotently-converging mechanisms (see that function's doc) —
    /// released HERE only on a failure that never got that far.
    pub fn start_supervisor(
        state_root: &Path,
        workspace_id: &str,
        mode: StartMode,
        agent_argv: &[String],
        project_root: &Path,
        agent_name: &str,
        slug: &str,
        workspaces: Workspaces,
    ) -> Result<Option<bool>, String> {
        if !workspaces.try_begin_capsule_start(workspace_id) {
            return Ok(None);
        }
        let state_dir = super::state_dir_for(state_root, workspace_id);
        let exe = match sot_capsule_exe() {
            Ok(exe) => exe,
            Err(e) => {
                workspaces.end_capsule_start(workspace_id);
                return Err(format!("could not locate sot-capsule.exe next to this daemon: {e}"));
            }
        };
        match spawn_and_watch(
            &exe,
            &state_dir,
            mode,
            agent_argv,
            project_root,
            agent_name,
            workspace_id.to_string(),
            slug.to_string(),
            workspaces.clone(),
        ) {
            Ok(degraded) => Ok(Some(degraded)),
            Err(e) => {
                workspaces.end_capsule_start(workspace_id);
                Err(format!("capsule supervisor spawn failed: {e}"))
            }
        }
    }

    /// Whether a capsule workspace's supervisor needs starting before
    /// `pty.open` can honestly answer `attach_direct` — and if so, with
    /// which start mode. Reuses [`phase_of`]'s own two failure phases
    /// rather than a new probe: [`NEVER_STARTED_PHASE`] (no published
    /// pointer — nothing has ever run) means `Start`; [`UNREACHABLE_PHASE`]
    /// (a pointer exists but its lane does not answer — the same "dead
    /// supervisor" case the watchdog/`resume_all` already resume with
    /// `--resume`) means `Resume`. Any answered lifecycle phase means a
    /// supervisor is already up — `None`, nothing to do. BLOCKING
    /// (`phase_of` itself is).
    fn start_mode_needed(state_dir: &Path) -> Option<StartMode> {
        match phase_of(state_dir) {
            NEVER_STARTED_PHASE => Some(StartMode::Start),
            UNREACHABLE_PHASE => Some(StartMode::Resume),
            _ => None,
        }
    }

    /// `pty.open` on a capsule workspace: start its supervisor if it
    /// isn't already running, sharing [`start_supervisor`] — the exact
    /// spawn path `workspace.create` uses — rather than a second spawn
    /// implementation (field finding, v0.6.0-rc.2 shakedown:
    /// `workspace.create` was the ONLY path that ever started one, so a
    /// workspace registered but never created through it — the Windows
    /// default/home row, ADR 0042 L1a Codex finding 5 — answered
    /// `attach_direct` against a supervisor that was never spawned,
    /// parking the frontend on an empty pane forever). `Ok(None)` = a
    /// supervisor already answered, OR another launch is already in
    /// flight (rule D); nothing started either way. `Ok(Some(degraded))`
    /// = a fresh spawn succeeded (mode from [`start_mode_needed`]). `Err`
    /// mirrors `start_supervisor`'s own failure, so a caller's error
    /// payload can match `workspace.create`'s.
    ///
    /// Rule I: this DOES block on a real lane probe (`start_mode_needed`
    /// -> `phase_of` -> `query_status`) when a pointer exists and no
    /// start is in flight — the same bounded worst case `query_status`'s
    /// own doc names (connect 2s + hello 2s + status 5s ~= 9s) for a row
    /// whose supervisor died without a trace. That cost is accepted here
    /// rather than avoided: it is the only way to tell "already running"
    /// (skip) from "pointer exists but the lane is dead" (needs
    /// `--resume`) apart from just guessing one or the other. The
    /// `is_capsule_starting` check below is what SKIPS that probe when a
    /// launch is already in flight — an early exit, not the correctness
    /// guarantee (that's `start_supervisor`'s own atomic claim, closing
    /// the tiny window between this check and the call below). BLOCKING
    /// — callers run it via `spawn_blocking`.
    pub fn ensure_started(
        state_root: &Path,
        workspace_id: &str,
        agent_kind: &str,
        agent_name: &str,
        slug: &str,
        project_root: &Path,
        workspaces: Workspaces,
    ) -> Result<Option<bool>, String> {
        if workspaces.is_capsule_starting(workspace_id) {
            return Ok(None);
        }
        let state_dir = super::state_dir_for(state_root, workspace_id);
        let Some(mode) = start_mode_needed(&state_dir) else {
            return Ok(None);
        };
        let argv = agent_argv(agent_kind)?;
        start_supervisor(state_root, workspace_id, mode, &argv, project_root, agent_name, slug, workspaces)
    }

    /// What one leg's exit means for the watchdog's own decision —
    /// ADR 0042 L1a, Codex review finding 6; rule F (shrink round)
    /// simplified this from three outcomes to two.
    enum LegOutcome {
        /// Exit 0 (`EXIT_CLEAN`): the run ended normally. Never
        /// restarted — the lane (or its absence) already says
        /// everything a client needs.
        Clean,
        /// Exit 69 (`EXIT_TERMINAL`): terminal, UNCONDITIONALLY — never
        /// restarted, regardless of whether the lane still answers. Rule
        /// F: the OLD "does the lane still answer" discriminator (a
        /// dropped `ForeignFence` outcome) tried to tell apart "lost the
        /// race for `supervisor.lock`" from "a genuinely exhausted
        /// producer", but `sot-capsule supervise` already runs its OWN
        /// internal flap/retry budget (`FLAP_THRESHOLD`,
        /// `respawn_or_terminal` in `rust/log/src/supervisor.rs`) before
        /// it ever chooses to exit 69 — so a second restart layer on top,
        /// here, is always redundant at best. At worst it actively hid a
        /// real failure: a producer that will NEVER recover (e.g.
        /// `claude` missing from the daemon's PATH) burned the WHOLE
        /// daemon-side restart budget (`MAX_RESTARTS_PER_WINDOW` attempts
        /// against `RESTART_WINDOW`) before finally reaching this same
        /// terminal mark anyway — "the supervisor's own three legs, not a
        /// rolling restart loop."
        Terminal,
        /// Anything else: a genuine crash needing the restart sequence.
        Crash,
    }

    /// Waits for `child` to exit and classifies the result.
    async fn wait_and_classify(child: &mut Child, workspace_id: &str) -> LegOutcome {
        let status = match child.wait().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: wait() failed; treating as a crash");
                return LegOutcome::Crash;
            }
        };
        match status.code() {
            Some(EXIT_CLEAN) => LegOutcome::Clean,
            Some(EXIT_TERMINAL) => LegOutcome::Terminal,
            _ => LegOutcome::Crash,
        }
    }

    /// The watchdog itself: waits for the leg to exit, classifies it, and
    /// on a crash restarts with `--resume` under ADR 0041's own launcher
    /// restart sequence (`RESTART_BACKOFFS`, at most `MAX_RESTARTS_PER_
    /// WINDOW` within `RESTART_WINDOW`), then stops and marks the
    /// workspace `capsule_terminal` — LOUDLY, via `Workspaces::
    /// mark_capsule_terminal`, which `workspace.list` reads before ever
    /// touching the (confirmed-gone) lane again. A `Terminal` leg (rule
    /// F) marks terminal immediately, on its very first occurrence, with
    /// no restart attempt at all.
    ///
    /// Rule D half 2: releases the `starting` claim
    /// (`Workspaces::end_capsule_start`) on EVERY leg's outcome —
    /// idempotent (harmless once [`spawn_starting_release_poll`], half 1,
    /// already cleared it for the success case) and correct regardless:
    /// once a leg's fate is known, "a launch is in flight" is no longer
    /// true for THIS leg (a restart, if one follows, is this SAME
    /// launch's own retry, not a new external request).
    fn spawn_watchdog(
        workspace_id: String,
        sot_capsule_exe: PathBuf,
        state_dir: PathBuf,
        argv: Vec<String>,
        cwd: PathBuf,
        agent_name: String,
        slug: String,
        child: Child,
        workspaces: Workspaces,
    ) {
        tokio::spawn(async move {
            let mut child_opt = Some(child);
            let mut restart_times: Vec<Instant> = Vec::new();
            loop {
                let outcome = match child_opt.as_mut() {
                    Some(c) => wait_and_classify(c, &workspace_id).await,
                    // A previous restart attempt itself failed to spawn
                    // (no live child to wait on) -- counts as another
                    // crash against the same budget.
                    None => LegOutcome::Crash,
                };
                child_opt = None;
                workspaces.end_capsule_start(&workspace_id);
                match outcome {
                    LegOutcome::Clean => return,
                    LegOutcome::Terminal => {
                        tracing::warn!(
                            workspace_id = %workspace_id,
                            "capsule supervisor watchdog: leg exited terminal (69) -- marking terminal, no restart"
                        );
                        workspaces.mark_capsule_terminal(&workspace_id);
                        return;
                    }
                    LegOutcome::Crash => {
                        let now = Instant::now();
                        restart_times.retain(|t| now.duration_since(*t) < RESTART_WINDOW);
                        if restart_times.len() >= MAX_RESTARTS_PER_WINDOW {
                            tracing::error!(
                                workspace_id = %workspace_id, window = ?RESTART_WINDOW, max = MAX_RESTARTS_PER_WINDOW,
                                "capsule supervisor watchdog: restart budget exhausted -- giving up, marking terminal"
                            );
                            workspaces.mark_capsule_terminal(&workspace_id);
                            return;
                        }
                        let backoff = RESTART_BACKOFFS[restart_times.len().min(RESTART_BACKOFFS.len() - 1)];
                        tracing::warn!(
                            workspace_id = %workspace_id, backoff = ?backoff, attempt = restart_times.len() + 1,
                            "capsule supervisor watchdog: crashed, restarting with --resume"
                        );
                        tokio::time::sleep(backoff).await;
                        restart_times.push(Instant::now());
                        match spawn_detached_supervisor(
                            &sot_capsule_exe,
                            &state_dir,
                            StartMode::Resume,
                            &argv,
                            &cwd,
                            &agent_name,
                            &workspace_id,
                            &slug,
                        ) {
                            Ok(spawned) => child_opt = Some(spawned.child),
                            Err(e) => {
                                tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: restart spawn failed");
                            }
                        }
                    }
                }
            }
        });
    }

    /// On daemon startup: resume every REGISTERED capsule workspace's
    /// supervisor whose voyage pointer has ALREADY been published (rule
    /// B, shrink round) — `--resume`, letting the supervisor's own
    /// start-mode table decide adopt-vs-spawn. A row with NO published
    /// pointer — never started at all, OR a leg that crashed before ever
    /// publishing one (the exact pre-pointer crash window ADR 0041
    /// names) — is SKIPPED entirely: `pty.open`'s start-on-attach
    /// (`ensure_started`) is what starts those now, not this scan.
    /// Before rule B this scan launched `--resume` unconditionally for
    /// EVERY registered row, including ones with no pointer at all —
    /// `sot-capsule supervise --resume` against a workspace with no leg
    /// to adopt and no pointer to found one against simply fails (exit
    /// 69), leaving a bare state directory behind and reading back as a
    /// misleading row on the owner's own box (the field finding behind
    /// this shrink round).
    ///
    /// A state directory with NO matching registry entry is left
    /// COMPLETELY untouched, logged once (ADR 0042: "the daemon's
    /// workspace list is the list" — an orphan is not addressable
    /// through any op, so resuming it would create a live, unaddressable
    /// process; deleted, the bare-shell fallback an earlier version used
    /// here).
    ///
    /// Rule D: each candidate goes through [`start_supervisor`] — the
    /// SAME atomically-claimed spawn path `pty.open`'s start-on-attach
    /// uses — so a candidate this scan races against a concurrent
    /// `pty.open` (the exact race the field finding's own timeline could
    /// produce) spawns nothing twice; the loser gets `Ok(None)`, logged
    /// at debug (expected, not an error).
    ///
    /// Runs off the startup critical path (finding 10): `server.rs`
    /// calls this via `tokio::spawn`, never awaited, and every spawn
    /// inside it is bounded to `LANE_CONCURRENCY` concurrent attempts via
    /// a semaphore — thousands of preserved workspaces cannot turn this
    /// into an unbounded synchronous fan-out before the listener binds.
    pub async fn resume_all(state_root: PathBuf, workspaces: Workspaces) {
        if sot_capsule_exe().is_err() {
            tracing::warn!("capsule workspace resume-scan: could not locate sot-capsule.exe next to this daemon");
            return;
        }
        let candidates: Vec<(String, Vec<String>, PathBuf, String, String)> = workspaces
            .list()
            .into_iter()
            .filter(|ws| ws.runtime == "capsule")
            .filter(|ws| {
                let state_dir = super::state_dir_for(&state_root, &ws.workspace_id);
                sot_log::pointer::pointer_path(&state_dir).is_file()
            })
            .filter_map(|ws| match agent_argv(&ws.agent) {
                Ok(argv) => Some((
                    ws.workspace_id.clone(),
                    argv,
                    ws.project_root.clone(),
                    ws.agent_name.clone(),
                    ws.slug.clone(),
                )),
                Err(e) => {
                    tracing::warn!(
                        workspace_id = %ws.workspace_id, error = %e,
                        "capsule workspace resume-scan: registry entry has an unsupported agent kind; skipping"
                    );
                    None
                }
            })
            .collect();

        let semaphore = Arc::new(tokio::sync::Semaphore::new(LANE_CONCURRENCY));
        let mut joins = Vec::with_capacity(candidates.len());
        for (workspace_id, argv, cwd, agent_name, slug) in candidates {
            let permit = semaphore.clone();
            let state_root = state_root.clone();
            let workspaces = workspaces.clone();
            joins.push(tokio::spawn(async move {
                let _permit = permit.acquire_owned().await;
                match start_supervisor(&state_root, &workspace_id, StartMode::Resume, &argv, &cwd, &agent_name, &slug, workspaces) {
                    Ok(Some(degraded)) => {
                        tracing::info!(workspace_id = %workspace_id, degraded, "capsule workspace supervisor resumed");
                    }
                    Ok(None) => {
                        tracing::debug!(workspace_id = %workspace_id, "capsule workspace resume-scan: launch already in flight; skipping");
                    }
                    Err(e) => {
                        tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule workspace supervisor resume spawn failed");
                    }
                }
            }));
        }
        for j in joins {
            let _ = j.await;
        }

        log_registryless_state_dirs(&state_root, &workspaces);
    }

    /// One log line naming every `<state-root>/workspaces/*` directory
    /// with no matching registry entry — diagnostic only, never acted on
    /// (see [`resume_all`]'s own doc).
    fn log_registryless_state_dirs(state_root: &Path, workspaces: &Workspaces) {
        let dir = state_root.join("workspaces");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == ErrorKind::NotFound => return,
            Err(e) => {
                tracing::debug!(dir = ?dir, error = %e, "capsule workspace resume-scan: could not read the state root for the registryless-directory log sweep");
                return;
            }
        };
        let known: std::collections::HashSet<String> =
            workspaces.list().into_iter().map(|ws| ws.workspace_id.clone()).collect();
        let orphans: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|id| !known.contains(id))
            .collect();
        if !orphans.is_empty() {
            tracing::warn!(
                count = orphans.len(), ids = ?orphans,
                "capsule workspace resume-scan: state directories with no matching registry entry -- \
                 left untouched (ADR 0042: the workspace list is the list)"
            );
        }
    }
}

#[cfg(windows)]
pub use windows_runtime::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_joins_workspaces_and_the_id() {
        let root = Path::new("/tmp/sot-state-root");
        assert_eq!(
            state_dir_for(root, "ws-alpha-1a2b"),
            PathBuf::from("/tmp/sot-state-root/workspaces/ws-alpha-1a2b")
        );
    }

    #[test]
    fn agent_argv_claude_matches_ccbs_own_flags() {
        assert_eq!(
            agent_argv("claude").unwrap(),
            vec!["claude", "--permission-mode", "auto", "/sot-session-start"]
        );
    }

    #[test]
    fn agent_argv_none_is_the_bare_shell() {
        assert_eq!(agent_argv("none").unwrap(), vec!["cmd.exe"]);
    }

    #[test]
    fn agent_argv_rejects_unsupported_kinds() {
        assert!(agent_argv("codex").is_err());
        assert!(agent_argv("bogus").is_err());
    }

    #[test]
    fn mode_flag_matches_the_sot_capsule_cli() {
        assert_eq!(mode_flag(StartMode::Start), "--start");
        assert_eq!(mode_flag(StartMode::Resume), "--resume");
    }

    #[test]
    fn phase_str_is_total_and_snake_case() {
        use sot_log::wire::SupervisorPhase;
        assert_eq!(phase_str(SupervisorPhase::Starting), "starting");
        assert_eq!(phase_str(SupervisorPhase::Ready), "ready");
        assert_eq!(phase_str(SupervisorPhase::Ending), "ending");
        assert_eq!(phase_str(SupervisorPhase::EndedNoRespawn), "ended_no_respawn");
        assert_eq!(phase_str(SupervisorPhase::Terminal), "terminal");
    }

    #[test]
    fn never_started_phase_is_distinct_from_unreachable_and_every_answered_phase() {
        // First live shakedown fix: a capsule workspace nobody has ever
        // started must read as quietly "stopped", not as the loud
        // "unreachable" a query FAILURE reports — the two must never
        // collide with each other or with any answered lifecycle phase.
        use sot_log::wire::SupervisorPhase;
        assert_ne!(NEVER_STARTED_PHASE, UNREACHABLE_PHASE);
        for p in [
            SupervisorPhase::Starting,
            SupervisorPhase::Ready,
            SupervisorPhase::Ending,
            SupervisorPhase::EndedNoRespawn,
            SupervisorPhase::Terminal,
        ] {
            assert_ne!(phase_str(p), NEVER_STARTED_PHASE);
        }
    }

    #[test]
    fn phase_for_missing_pointer_only_fires_when_the_pointer_is_absent() {
        // Rule B: a published pointer means a supervisor reached a real
        // run at least once — that case defers to the real query (`None`)
        // rather than guessing; only a genuinely absent pointer
        // short-circuits to `NEVER_STARTED_PHASE`.
        assert_eq!(
            phase_for_missing_pointer(false),
            Some(NEVER_STARTED_PHASE)
        );
        assert_eq!(phase_for_missing_pointer(true), None);
    }

    #[test]
    fn unreachable_phase_is_distinct_from_every_answered_phase() {
        use sot_log::wire::SupervisorPhase;
        for p in [
            SupervisorPhase::Starting,
            SupervisorPhase::Ready,
            SupervisorPhase::Ending,
            SupervisorPhase::EndedNoRespawn,
            SupervisorPhase::Terminal,
        ] {
            assert_ne!(phase_str(p), UNREACHABLE_PHASE);
        }
    }

    #[test]
    fn restart_budget_numbers_match_adr_0041s_own_launcher_table() {
        assert_eq!(RESTART_BACKOFFS.len(), 5);
        assert_eq!(MAX_RESTARTS_PER_WINDOW, 5);
        assert_eq!(RESTART_WINDOW, std::time::Duration::from_secs(60));
        assert_eq!(
            RESTART_BACKOFFS.map(|d| d.as_secs()),
            [1, 3, 7, 15, 30]
        );
    }

    #[test]
    fn nesting_env_scrub_list_matches_ccb() {
        // Mirrors comm/adapters/claude/bin/ccb's own `unset` line
        // exactly -- see that file for the reasoning per variable.
        assert_eq!(
            NESTING_ENV_VARS_TO_SCRUB,
            &[
                "CLAUDE_CODE_FORK_SUBAGENT",
                "CLAUDE_CODE_CHILD_SESSION",
                "CLAUDE_CODE_TEAMMATE_MODE",
                "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
                "CLAUDECODE",
                "AI_AGENT",
                "CLAUDE_CODE_SESSION_ID",
            ]
        );
    }

    // Serialized under the crate-wide `paths::ENV_TEST_LOCK` (mirrors
    // `workspaces.rs`'s own `EnvGuard` exactly — see that module's
    // comment: `cargo test` runs in parallel within one process, and
    // several modules' resolvers read the SAME env vars, HOME included).
    struct SelfFileEnvGuard {
        _serial: std::sync::MutexGuard<'static, ()>,
        home: Option<std::ffi::OsString>,
        userprofile: Option<std::ffi::OsString>,
        sot_comm_home: Option<std::ffi::OsString>,
        sot_state_host: Option<std::ffi::OsString>,
    }

    impl Drop for SelfFileEnvGuard {
        fn drop(&mut self) {
            for (key, val) in [
                ("HOME", &self.home),
                ("USERPROFILE", &self.userprofile),
                ("SOT_COMM_HOME", &self.sot_comm_home),
                ("SOT_STATE_HOST", &self.sot_state_host),
            ] {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn self_file_env_guarded() -> SelfFileEnvGuard {
        let serial = crate::paths::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        SelfFileEnvGuard {
            home: std::env::var_os("HOME"),
            userprofile: std::env::var_os("USERPROFILE"),
            sot_comm_home: std::env::var_os("SOT_COMM_HOME"),
            sot_state_host: std::env::var_os("SOT_STATE_HOST"),
            _serial: serial,
        }
    }

    #[test]
    fn capsule_supervisor_env_carries_slug_workspace_and_comm_home() {
        // Codex round: SOT_WORKSPACE is keyed on SLUG (finding 1 — the
        // frontend keys results/the active workspace by slug, not the
        // internal ws-<slug>-<hex> id), while the id is used only for the
        // self-file path. SOT_COMM_HOME and SOT_COMM_SELF_FILE
        // (comm-lib.sh's existing pin-the-self-file seam) are stamped
        // unconditionally so a capsule never falls back to the shared
        // per-host __nopane slot.
        let _guard = self_file_env_guarded();
        std::env::set_var("SOT_STATE_HOST", "testhost");
        std::env::set_var("SOT_COMM_HOME", "/fake-home/.sot-comm");
        let env = capsule_supervisor_env(
            "ws-myrepo-1a2b",
            "myrepo",
            Path::new("/home/me/myrepo"),
            "myrepo-myhost",
        );
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("SOT_WORKSPACE"), Some("myrepo"));
        assert_eq!(get("SOT_WORKSPACE_ROOT"), Some("/home/me/myrepo"));
        assert_eq!(get("SOT_COMM_NAME"), Some("myrepo-myhost"));
        assert_eq!(get("SOT_SESSION"), Some("1"));
        assert_eq!(get("SOT_COMM_HOME"), Some("/fake-home/.sot-comm"));
        assert_eq!(
            get("SOT_COMM_SELF_FILE"),
            Some("/fake-home/.sot-comm/self/testhost__ws-myrepo-1a2b.txt")
        );
    }

    #[test]
    fn capsule_supervisor_env_omits_comm_name_when_unnamed() {
        // Codex round finding 2: NO synthesized default here — an
        // un-pinned autostart (no explicit agent_name in the request)
        // gets no SOT_COMM_NAME at all. comm-join.sh's own #148
        // auto-disambiguating derivation decides the handle instead
        // (reading SOT_COMM_SELF_FILE to know where to write it), exactly
        // as a hand-started shell would — a synthesized <slug>-<host>
        // pin would become an explicit overwrite of any existing row of
        // that name, which PROTOCOL.md's "never reuse a handle" forbids.
        // SOT_WORKSPACE/SOT_COMM_HOME/SOT_COMM_SELF_FILE stay
        // unconditional regardless.
        let _guard = self_file_env_guarded();
        std::env::set_var("SOT_STATE_HOST", "testhost");
        std::env::set_var("SOT_COMM_HOME", "/fake-home/.sot-comm");
        let env = capsule_supervisor_env("ws-anon-9f9f", "anon", Path::new("/home/me/anon"), "");
        let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
        assert_eq!(get("SOT_WORKSPACE"), Some("anon"));
        assert_eq!(get("SOT_COMM_NAME"), None);
        assert_eq!(
            get("SOT_COMM_SELF_FILE"),
            Some("/fake-home/.sot-comm/self/testhost__ws-anon-9f9f.txt")
        );
    }

    #[test]
    fn capsule_comm_home_str_none_when_no_home_var_is_set() {
        let _guard = self_file_env_guarded();
        std::env::remove_var("SOT_COMM_HOME");
        std::env::remove_var("HOME");
        std::env::remove_var("USERPROFILE");
        assert_eq!(capsule_comm_home_str(), None);
    }
}
