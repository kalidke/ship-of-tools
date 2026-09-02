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
/// supervise` itself; this daemon only creates the directory itself
/// before spawning, per ADR 0042 L1a's own "create the dir" instruction).
/// `state_root` is `sot_log::state_dir::sot_state_dir()`, injected rather
/// than resolved here so this stays a pure function of its inputs (real
/// callers resolve it once; a test supplies a tempdir root).
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

#[cfg(windows)]
mod windows_runtime {
    use super::{
        agent_argv, mode_flag, StartMode, LANE_CONCURRENCY, MAX_RESTARTS_PER_WINDOW, NESTING_ENV_VARS_TO_SCRUB,
        RESTART_BACKOFFS, RESTART_WINDOW,
    };
    use crate::workspaces::Workspaces;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Instant;
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
    /// (`EXIT_TERMINAL`) — ambiguous on its own (see [`wait_and_classify`]).
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
            if !agent_name.is_empty() {
                cmd.env("SOT_COMM_NAME", agent_name);
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
    pub fn phase_of(state_dir: &Path) -> &'static str {
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
    pub fn spawn_and_watch(
        sot_capsule_exe: &Path,
        state_dir: &Path,
        mode: StartMode,
        agent_argv: &[String],
        cwd: &Path,
        agent_name: &str,
        workspace_id: String,
        workspaces: Workspaces,
    ) -> std::io::Result<bool> {
        let spawned = spawn_detached_supervisor(sot_capsule_exe, state_dir, mode, agent_argv, cwd, agent_name)?;
        let degraded = spawned.degraded;
        workspaces.clear_capsule_terminal(&workspace_id);
        spawn_watchdog(
            workspace_id,
            sot_capsule_exe.to_path_buf(),
            state_dir.to_path_buf(),
            agent_argv.to_vec(),
            cwd.to_path_buf(),
            agent_name.to_string(),
            spawned.child,
            workspaces,
        );
        Ok(degraded)
    }

    /// What one leg's exit means for the watchdog's own decision —
    /// ADR 0042 L1a, Codex review finding 6.
    enum LegOutcome {
        /// Exit 0 (`EXIT_CLEAN`): the run ended normally. Never
        /// restarted — the lane (or its absence) already says
        /// everything a client needs.
        Clean,
        /// Exit 69 (`EXIT_TERMINAL`) but the lane still answers: this
        /// leg lost the race for `supervisor.lock` against another
        /// already-running authority — expected, not a crash.
        ForeignFence,
        /// Anything else: a genuine crash needing the restart sequence.
        Crash,
    }

    /// Waits for `child` to exit and classifies the result. Exit 69 is
    /// ambiguous on its own (a losing race for the fence against another
    /// live authority ALSO exits 69, indistinguishably from a genuine
    /// terminal failure), so this queries the lane ONCE before deciding —
    /// an answering lane means another authority holds the fence
    /// (expected, logged at debug); a silent one is treated as the crash
    /// it looks like. The query is BLOCKING sot_log I/O, run via
    /// `spawn_blocking` so it never stalls the async runtime.
    async fn wait_and_classify(child: &mut Child, state_dir: &Path, workspace_id: &str) -> LegOutcome {
        let status = match child.wait().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: wait() failed; treating as a crash");
                return LegOutcome::Crash;
            }
        };
        match status.code() {
            Some(EXIT_CLEAN) => LegOutcome::Clean,
            Some(EXIT_TERMINAL) => {
                let dir = state_dir.to_path_buf();
                let answered = tokio::task::spawn_blocking(move || sot_log::supervisor_client::query_status(&dir).is_ok())
                    .await
                    .unwrap_or(false);
                if answered {
                    tracing::debug!(workspace_id = %workspace_id, "capsule supervisor exited 69 but the lane still answers -- another authority holds the fence");
                    LegOutcome::ForeignFence
                } else {
                    LegOutcome::Crash
                }
            }
            _ => LegOutcome::Crash,
        }
    }

    /// The watchdog itself: waits for the leg to exit, classifies it, and
    /// on a crash restarts with `--resume` under ADR 0041's own launcher
    /// restart sequence (`RESTART_BACKOFFS`, at most `MAX_RESTARTS_PER_
    /// WINDOW` within `RESTART_WINDOW`), then stops and marks the
    /// workspace `capsule_terminal` — LOUDLY, via `Workspaces::
    /// mark_capsule_terminal`, which `workspace.list` reads before ever
    /// touching the (confirmed-gone) lane again.
    fn spawn_watchdog(
        workspace_id: String,
        sot_capsule_exe: PathBuf,
        state_dir: PathBuf,
        argv: Vec<String>,
        cwd: PathBuf,
        agent_name: String,
        child: Child,
        workspaces: Workspaces,
    ) {
        tokio::spawn(async move {
            let mut child_opt = Some(child);
            let mut restart_times: Vec<Instant> = Vec::new();
            loop {
                let outcome = match child_opt.as_mut() {
                    Some(c) => wait_and_classify(c, &state_dir, &workspace_id).await,
                    // A previous restart attempt itself failed to spawn
                    // (no live child to wait on) -- counts as another
                    // crash against the same budget.
                    None => LegOutcome::Crash,
                };
                child_opt = None;
                match outcome {
                    LegOutcome::Clean | LegOutcome::ForeignFence => return,
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
                        match spawn_detached_supervisor(&sot_capsule_exe, &state_dir, StartMode::Resume, &argv, &cwd, &agent_name) {
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
    /// supervisor (ADR 0042 L1a) — `--resume` unconditionally, letting
    /// the supervisor's own start-mode table decide adopt-vs-spawn.
    /// Codex review finding 8: a workspace whose FIRST leg crashed
    /// BEFORE ever publishing `drawer.voyage` (the exact pre-pointer
    /// crash window) still gets `--resume` here — gating on the pointer
    /// first, as an earlier version did, silently abandoned exactly that
    /// workspace forever, when `--resume` with no leg is defined to
    /// spawn (ADR 0041's own table). A state directory with NO matching
    /// registry entry is left COMPLETELY untouched, logged once (ADR
    /// 0042: "the daemon's workspace list is the list" — an orphan is
    /// not addressable through any op, so resuming it would create a
    /// live, unaddressable process; deleted, the bare-shell fallback an
    /// earlier version used here).
    ///
    /// Runs off the startup critical path (finding 10): `server.rs`
    /// calls this via `tokio::spawn`, never awaited, and every spawn
    /// inside it is bounded to `LANE_CONCURRENCY` concurrent attempts via
    /// a semaphore — thousands of preserved workspaces cannot turn this
    /// into an unbounded synchronous fan-out before the listener binds.
    pub async fn resume_all(state_root: PathBuf, workspaces: Workspaces) {
        let Ok(sot_capsule) = sot_capsule_exe() else {
            tracing::warn!("capsule workspace resume-scan: could not locate sot-capsule.exe next to this daemon");
            return;
        };
        let candidates: Vec<(String, PathBuf, Vec<String>, PathBuf, String)> = workspaces
            .list()
            .into_iter()
            .filter(|ws| ws.runtime == "capsule")
            .filter_map(|ws| match agent_argv(&ws.agent) {
                Ok(argv) => Some((
                    ws.workspace_id.clone(),
                    super::state_dir_for(&state_root, &ws.workspace_id),
                    argv,
                    ws.project_root.clone(),
                    ws.agent_name.clone(),
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
        for (workspace_id, state_dir, argv, cwd, agent_name) in candidates {
            let permit = semaphore.clone();
            let sot_capsule = sot_capsule.clone();
            let workspaces = workspaces.clone();
            joins.push(tokio::spawn(async move {
                let _permit = permit.acquire_owned().await;
                match spawn_and_watch(&sot_capsule, &state_dir, StartMode::Resume, &argv, &cwd, &agent_name, workspace_id.clone(), workspaces) {
                    Ok(degraded) => {
                        tracing::info!(workspace_id = %workspace_id, degraded, "capsule workspace supervisor resumed");
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
}
