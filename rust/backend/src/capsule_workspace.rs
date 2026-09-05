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

/// Outcome of [`windows_runtime::end_run`] — the daemon's own portable
/// vocabulary over `sot_log::supervisor_client::EndRunOutcome` (never
/// that raw, Windows-only type crossing into `handlers.rs`). Defined
/// here, outside `windows_runtime`, so `handlers.rs`'s outcome→response
/// mapping stays plain and unit-testable on every platform; `end_run`'s
/// own real lane call is the only Windows-only step.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone)]
pub enum EndRunOutcome {
    /// The run ended and its record verified green.
    RecordVerified,
    /// The marker committed but the O(retained history) verify walk
    /// hadn't finished within the ADR's 90s cutoff — still safe to treat
    /// as "ended" (the marker itself is the irrevocable acceptance).
    RecordClosed,
    /// The lane was ALREADY resting in `EndedNoRespawn` — recovered via
    /// the leg's own end-marker alone, NEVER via `verify_voyage`, so
    /// this must not be reported as `RecordVerified`/`RecordClosed`.
    AlreadyEnded,
    /// The authority had already reached `Terminal` before this call
    /// ever reached it — its own internal flap/retry budget exhausted
    /// (`FLAP_THRESHOLD`, `rust/log/src/supervisor.rs`), most often an
    /// agent argv that can never launch (e.g. `claude` missing from
    /// PATH). There is no leg left to end, and a `Terminal` authority
    /// admits no fresh `EndRun` anyway (`supervisor.rs`'s
    /// `handle_command` gates `EndRun` on `Lifecycle::Ready`) — so this
    /// sends `stop` instead (admitted unconditionally, regardless of
    /// lifecycle: `SupervisorOp::Stop`'s own admission has no lifecycle
    /// gate) and waits for its confirmed exit. Without this arm the row
    /// was UNENDABLE: `workspace.destroy` kept reporting `NotEnded`
    /// forever, because nothing ever told the stuck authority to stop.
    /// Safe to treat as `Removable` — a `Terminal` authority has no live
    /// leg left to orphan.
    Terminal,
    /// The lane answered `phase: Starting` (voyage may still be `None`
    /// — only set once Recovering completes) — NEVER "not running"; a
    /// run may be about to (or already did) start. Retry.
    Starting,
    /// `end_run` reported the operation failed, was refused, or its
    /// outcome is unknown — no confirmed end in any case.
    NotEnded(String),
}

/// The pair invariant, decided from what `sot-capsule.exe build-id`
/// printed (`reported`, `None` when it printed nothing usable -- a binary
/// that predates the subcommand answers with its usage on stderr and exit
/// 2) against this daemon's own lane build id. Both halves of the pair
/// carry `sot_log::exchange::SUPERVISOR_LANE_BUILD_ID`; the supervisor
/// hello refuses a mismatch as `version_skew` (ADR 0041 build boundary),
/// so a capsule spawned from a binary of another build could never be
/// attached, adopted, ended or destroyed by this daemon -- a dead row that
/// looked started (field day 2026-09-05: a launcher pair rebuild that had
/// to leave a pinned `sot-capsule.exe` behind produced exactly that for
/// every capsule spawned afterwards, with no error anywhere until attach).
/// Portable so the verdict text is unit-tested on every host.
pub(crate) fn pair_verdict(reported: Option<&str>, own: &str) -> Result<(), String> {
    match reported {
        Some(got) if got == own => Ok(()),
        got => Err(format!(
            "sot-capsule.exe build {} does not match this daemon's build {own}: rebuild the pair \
             (cargo build --release -p sot-backend -p sot-log; a sot-capsule.exe pinned by running \
             supervisors must be renamed aside first). Rows still held by supervisors of the old \
             build: end them from a frontend of that build, or kill only their `sot-capsule \
             supervise` process and attach the row again -- the run leg and the agent in it \
             survive, and the new supervisor adopts them (the management exchange is not \
             build-gated); this daemon never adopts or respawns a foreign supervisor on its own",
            got.unwrap_or("unknown (binary predates `build-id`)")
        )),
    }
}

#[cfg(windows)]
mod windows_runtime {
    use super::{
        agent_argv, capsule_supervisor_env, mode_flag, phase_str, StartMode, LANE_CONCURRENCY,
        LIST_LANE_DEADLINE, MAX_RESTARTS_PER_WINDOW, NESTING_ENV_VARS_TO_SCRUB, NEVER_STARTED_PHASE,
        RESTART_BACKOFFS, RESTART_WINDOW, UNREACHABLE_PHASE,
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
    /// `sot-capsule supervise`'s own fence-contention exit code
    /// (`sot_log::supervisor::EXIT_CONTENDED` — see that const's own doc
    /// for the full reasoning): the authority fence was already held by
    /// a LIVE supervisor. Distinct from [`EXIT_TERMINAL`] in
    /// [`wait_and_classify`] — NEVER a failure of this workspace's own
    /// run, only proof some other leg (almost always the previous
    /// authority for this SAME state dir, still finishing its own
    /// teardown) currently holds the fence.
    const EXIT_CONTENDED: i32 = 70;

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

    /// Refuse to spawn a capsule from a binary of another build -- see
    /// `super::pair_verdict`. Runs `sot-capsule.exe build-id` (one line,
    /// exits at once; no window) under a hard bound: the probe is killed
    /// and reaped if it has not exited within `PAIR_PROBE_BOUND`, so a
    /// wedged binary can never hold a runtime worker or a `starting`
    /// claim open. A mismatch is `ErrorKind::Unsupported` -- the one kind
    /// the watchdog treats as terminal at once rather than a crash to
    /// retry. Spawns are rare (attach, boot resume, watchdog restart), so
    /// the extra process per spawn is not worth a cache.
    const PAIR_PROBE_BOUND: Duration = Duration::from_secs(5);
    fn check_pair(sot_capsule_exe: &Path) -> std::io::Result<()> {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut child = std::process::Command::new(sot_capsule_exe)
            .arg("build-id")
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let deadline = Instant::now() + PAIR_PROBE_BOUND;
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    ErrorKind::TimedOut,
                    format!("sot-capsule.exe build-id did not answer within {PAIR_PROBE_BOUND:?}"),
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut stdout = String::new();
        let mut stderr = String::new();
        if let Some(mut o) = child.stdout.take() {
            let _ = std::io::Read::read_to_string(&mut o, &mut stdout);
        }
        if let Some(mut e) = child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut e, &mut stderr);
        }
        let reported = stdout.trim();
        if !status.success() && !stderr.contains("usage:") {
            // Not the pre-`build-id` binary answering with its usage line:
            // a real failure to run the probe -- report it as such.
            return Err(std::io::Error::other(format!(
                "sot-capsule.exe build-id failed ({status}): {}",
                stderr.trim()
            )));
        }
        let reported = (status.success() && !reported.is_empty()).then_some(reported);
        super::pair_verdict(reported, sot_log::exchange::SUPERVISOR_LANE_BUILD_ID)
            .map_err(|m| std::io::Error::new(ErrorKind::Unsupported, m))
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
        check_pair(sot_capsule_exe)?;
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
            // The retained process handle (the second element) is not
            // this caller's concern -- a one-shot phase probe, dropped
            // (closing the handle) the instant this returns.
            Ok((report, _process)) => super::phase_str(report.phase),
            Err(e) => {
                note_if_foreign(state_dir, &e);
                tracing::debug!(state_dir = ?state_dir, error = %e, "capsule workspace: supervisor lane unreachable");
                super::UNREACHABLE_PHASE
            }
        }
    }

    /// A supervisor of ANOTHER build answers the hello with `version_skew`
    /// and the client reports it as `foreign` (`query_status` erases the
    /// typed `ChallengeOutcome::Foreign` into a state string, hence the
    /// text match). `phase_of` then reads "unreachable" and every start
    /// decision treats the row as restartable, but a fresh spawn only
    /// exits contended against the old fence -- the row is a dead end
    /// until an operator acts. Say so ONCE per row per daemon lifetime
    /// (`phase_of` is also the list poll's probe), with the recovery.
    fn note_if_foreign(state_dir: &Path, e: &dyn std::fmt::Display) {
        use std::sync::{Mutex, OnceLock};
        let text = e.to_string();
        if !text.contains("foreign") {
            return;
        }
        static NOTED: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
        let mut noted = NOTED.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner());
        if noted.insert(state_dir.to_path_buf()) {
            tracing::warn!(
                state_dir = ?state_dir, error = %text,
                "capsule row is held by a supervisor from ANOTHER build; this daemon cannot attach, adopt, end \
                 or destroy it. Recovery: end it from a frontend of that build, or kill only its `sot-capsule \
                 supervise` process and attach the row again (the run leg and its agent survive and are adopted)"
            );
        }
    }

    /// `workspace.delete` on a capsule workspace (and the default row's
    /// end-run path): send `end_run {reason, voyage}` on the lane,
    /// reporting the outcome via [`super::EndRunOutcome`] (see its own
    /// variant docs for the Starting/AlreadyEnded/Terminal honesty
    /// rules), THEN `stop` the authority once confirmed there is no more
    /// leg to run — including a `Terminal` authority, which has no leg
    /// to end but still needs `stop` to actually go away (see
    /// [`super::EndRunOutcome::Terminal`]'s own doc: without this arm a
    /// capsule row whose agent argv can never launch cycled
    /// Starting -> Terminal forever and was never endable from the UI).
    /// `stop` now WAITS for confirmed process exit; a failure there is
    /// only a logged warning, never a destroy failure — the outcome
    /// stays the confirmed one (`stop` only ends the AUTHORITY, never
    /// the capsule LEG, ADR 0041 adoption). The state directory is NEVER
    /// deleted here. BLOCKING — callers run it via `spawn_blocking`.
    pub fn end_run(state_dir: &Path, reason: &str) -> std::io::Result<super::EndRunOutcome> {
        use super::EndRunOutcome as R;
        use sot_log::supervisor_client::EndRunOutcome as O;
        use sot_log::wire::SupervisorPhase;

        let status = sot_log::supervisor_client::query_status(state_dir)
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .0;

        match status.phase {
            SupervisorPhase::Starting => return Ok(R::Starting),
            SupervisorPhase::EndedNoRespawn => {
                // A PRIOR end already landed here and was never stopped
                // (exactly the leak this whole function closes) — a
                // fresh `EndRun` command would only be refused
                // (`Failed{"no leg is currently running"}`, since
                // `EndRun` requires `Lifecycle::Ready`; see
                // `supervisor.rs`'s `handle_command`). Skip the doomed
                // round trip; retry the stop instead of fabricating a
                // verified outcome this call never actually observed.
                stop_and_warn(state_dir, "already ended (EndedNoRespawn) before this call");
                return Ok(R::AlreadyEnded);
            }
            SupervisorPhase::Terminal => {
                // No leg is running and no fresh `EndRun` would ever be
                // admitted here (`Lifecycle::Terminal` isn't `Ready`) —
                // the honest confirmed end is stopping the stuck
                // authority itself. See `EndRunOutcome::Terminal`'s own
                // doc for why this arm exists.
                stop_and_warn(state_dir, "the authority was terminal before this call reached it");
                return Ok(R::Terminal);
            }
            SupervisorPhase::Ready | SupervisorPhase::Ending => {}
        }

        // Ready/Ending are only reachable once Recovering's own Done arm
        // has set `authority.voyage_id` (`supervisor.rs`), so this is
        // always populated here.
        let voyage = status
            .voyage
            .expect("Ready/Ending implies a voyage_id (supervisor.rs's own recovery transition)");
        let outcome = sot_log::supervisor_client::end_run(state_dir, &voyage, reason)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(match outcome {
            O::RecordVerified => {
                stop_and_warn(state_dir, "end_run confirmed verified");
                R::RecordVerified
            }
            O::RecordClosed => {
                stop_and_warn(state_dir, "end_run confirmed closed");
                R::RecordClosed
            }
            O::Failed(detail) => R::NotEnded(format!("end_run failed: {detail}")),
            O::Refused(detail) => R::NotEnded(format!("end_run refused: {detail}")),
            O::OutcomeUnknown => R::NotEnded(
                "end_run outcome unknown (the ADR's own 90s cutoff elapsed with no terminal reply)"
                    .to_string(),
            ),
        })
    }

    /// Best-effort `stop` after [`end_run`] confirms there is no more
    /// leg to run — see that function's own doc for why this exists and
    /// why a failure here is only ever logged, never propagated.
    fn stop_and_warn(state_dir: &Path, why: &'static str) {
        if let Err(e) = sot_log::supervisor_client::stop(state_dir) {
            tracing::warn!(
                state_dir = ?state_dir, error = %e, why,
                "capsule workspace: stop after end_run failed (resident supervisor leaked)"
            );
        }
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
        install_watchdog(
            workspace_id,
            sot_capsule_exe.to_path_buf(),
            state_dir.to_path_buf(),
            agent_argv.to_vec(),
            cwd.to_path_buf(),
            agent_name.to_string(),
            slug,
            WatchedLeg::Spawned(spawned.child),
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
    ///
    /// `resume_all` (daemon boot) now matches on this SAME function
    /// (round-2 Codex finding: one decision oracle, not a second one) —
    /// `None` means adopt (a lane already answers), `Some(Resume)` means
    /// spawn (pointer published, lane dead), `Some(Start)` means the boot
    /// scan's own defensive no-op (it should never see this — its own
    /// candidate filter already requires a published pointer).
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
        let spawned = match start_mode_needed(&state_dir) {
            Some(mode) => {
                let argv = agent_argv(agent_kind)?;
                let result = start_supervisor(
                    state_root,
                    workspace_id,
                    mode,
                    &argv,
                    project_root,
                    agent_name,
                    slug,
                    workspaces,
                )?;
                if mode == StartMode::Resume {
                    // A resumed authority whose leg already carries the
                    // end marker settles into `EndedNoRespawn` almost
                    // instantly (a marker read, no spawn) — give it
                    // that window before the reset check below. A
                    // crash-resume (spawning a fresh leg) just stays
                    // `Starting` past it and falls through unaffected.
                    let deadline = Instant::now() + Duration::from_secs(2);
                    loop {
                        if let Ok((report, _process)) =
                            sot_log::supervisor_client::query_status(&state_dir)
                        {
                            if !matches!(report.phase, sot_log::wire::SupervisorPhase::Starting) {
                                break;
                            }
                        }
                        if Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
                result
            }
            None => None,
        };
        // One answered phase still has no live leg to attach to:
        // `EndedNoRespawn` (`--resume`/`--start` deliberately never
        // resurrect it — ADR 0041's own no-resurrection rule). `reset`
        // is the ONE operation that phase admits; proceed as for a
        // fresh start (the reset transaction mints the new voyage and
        // spawns).
        if phase_of(&state_dir) == super::phase_str(sot_log::wire::SupervisorPhase::EndedNoRespawn)
        {
            return sot_log::supervisor_client::reset(&state_dir)
                .map(|_new_voyage| Some(false))
                .map_err(|e| format!("capsule workspace reset (after an ended run) failed: {e}"));
        }
        Ok(spawned)
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
        /// Exit 70 (`EXIT_CONTENDED`) — round-2 Codex finding, daemon-
        /// boot-adopts-supervisor fix: the authority fence was already
        /// held by a LIVE supervisor when this leg tried to acquire it.
        /// NEVER treated as [`Terminal`] (that would mark a perfectly
        /// healthy workspace terminal out from under a run some OTHER
        /// leg is still actively serving) and never blindly retried with
        /// `--resume` either (which would likely race the SAME
        /// still-tearing-down fence again) — [`install_watchdog`] instead
        /// re-probes the lane for adoption over a short bound.
        Contended,
        /// Anything else: a genuine crash needing the restart sequence.
        Crash,
    }

    /// One capsule leg's process handle, unified so the SAME watchdog
    /// loop ([`install_watchdog`]) can wait on and classify EITHER a leg
    /// this daemon just spawned itself (`Spawned`, an owned [`Child`]) OR
    /// one it ADOPTED already alive from a previous daemon lifetime
    /// (`Adopted`, the retained process handle the supervisor lane's own
    /// challenge proved — `sot_log::supervisor_client::query_status`'s
    /// second return value). Round-2 Codex finding: an adopted leg had NO
    /// watchdog at all before this — its own eventual crash was never
    /// restarted, a 69 was never recorded, and a Terminal lane simply
    /// went quiet ("unreachable") once its process actually exited.
    enum WatchedLeg {
        Spawned(Child),
        Adopted(sot_log::supervisor_client::ChallengedProcess),
    }

    /// How long an adopted leg's blocking wait blocks before looping to
    /// check again — purely a "how often does a blocking-pool thread
    /// wake up for no reason" knob, never a correctness bound:
    /// [`sot_log::supervisor_client::ChallengedProcess::wait`] returns
    /// the INSTANT the process actually dies, so a longer interval only
    /// means fewer wasted wakeups, never slower death detection.
    const ADOPTED_LEG_WAIT_POLL: Duration = Duration::from_secs(300);

    /// Waits for `leg` to end and classifies the result — the SAME
    /// classification whether the leg is a child this daemon just
    /// spawned or one it adopted. A spawned child is awaited async, the
    /// normal way; an adopted process has no async-awaitable primitive
    /// (`ChallengedProcess::wait` is a synchronous, bounded
    /// `WaitForSingleObject`), so it is waited on ONE blocking task that
    /// owns it for the wait's whole duration, looping
    /// [`ADOPTED_LEG_WAIT_POLL`] at a time until it reports exit, then
    /// its exit code is read the same way
    /// `sot_log::challenge::ChallengedProcess::exit_code_after_confirmed_exit`
    /// documents its own precondition: only after `wait` has already
    /// confirmed death.
    async fn wait_and_classify(leg: WatchedLeg, workspace_id: &str) -> LegOutcome {
        let code = match leg {
            WatchedLeg::Spawned(mut child) => match child.wait().await {
                Ok(status) => status.code(),
                Err(e) => {
                    tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: wait() failed; treating as a crash");
                    return LegOutcome::Crash;
                }
            },
            WatchedLeg::Adopted(process) => {
                let result = tokio::task::spawn_blocking(move || loop {
                    match process.wait(ADOPTED_LEG_WAIT_POLL) {
                        Ok(true) => {
                            return process
                                .exit_code_after_confirmed_exit()
                                .map(|c| c as i32)
                                .map_err(|e| e.to_string());
                        }
                        Ok(false) => continue,
                        Err(e) => return Err(e.to_string()),
                    }
                })
                .await;
                match result {
                    Ok(Ok(code)) => Some(code),
                    Ok(Err(e)) => {
                        tracing::warn!(
                            workspace_id = %workspace_id, error = %e,
                            "capsule supervisor watchdog: waiting on the adopted process failed; treating as a crash"
                        );
                        None
                    }
                    Err(e) => {
                        tracing::warn!(
                            workspace_id = %workspace_id, error = %e,
                            "capsule supervisor watchdog: adopted-process wait task did not complete; treating as a crash"
                        );
                        None
                    }
                }
            }
        };
        match code {
            Some(EXIT_CLEAN) => LegOutcome::Clean,
            Some(EXIT_TERMINAL) => LegOutcome::Terminal,
            Some(EXIT_CONTENDED) => LegOutcome::Contended,
            _ => LegOutcome::Crash,
        }
    }

    /// Bound on how long [`install_watchdog`] keeps re-probing a
    /// [`LegOutcome::Contended`] leg for adoption before giving up —
    /// matches [`sot_log::pipe_win::TEARDOWN_AGGREGATE_DEADLINE`] (the
    /// authority's own documented worst-case teardown budget: it drops
    /// its lane before releasing the fence) plus margin for this
    /// process's own connect/challenge round trip on top of that.
    const CONTENTION_RETRY_BOUND: Duration = Duration::from_secs(25);
    /// How often [`install_watchdog`] re-probes within
    /// [`CONTENTION_RETRY_BOUND`] — matches the cadence
    /// `spawn_starting_release_poll` and `workspace.list`'s own lane
    /// gather already use elsewhere in this file for "is the lane up
    /// yet" polling, just slower (contention resolving is rarer and the
    /// bound is longer).
    const CONTENTION_RETRY_INTERVAL: Duration = Duration::from_secs(2);

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
    /// ONE function for both origins (round-2 Codex finding): [`leg`]
    /// starts as either a [`WatchedLeg::Spawned`] child ([`spawn_and_watch`]'s
    /// own call) or a [`WatchedLeg::Adopted`] process
    /// ([`watch_adopted_leg`]'s), and every SUBSEQUENT leg — a crash
    /// restart, or an adoption that resolves a [`LegOutcome::Contended`]
    /// — can be either kind too, decided fresh each time by what actually
    /// happens.
    ///
    /// A [`LegOutcome::Contended`] leg is neither terminal nor a normal
    /// crash: it means some OTHER process currently holds the fence
    /// (almost always the previous authority for this SAME state dir,
    /// still finishing its own teardown), so this re-probes the lane for
    /// up to [`CONTENTION_RETRY_BOUND`], at [`CONTENTION_RETRY_INTERVAL`]
    /// — the instant that OTHER lane answers, it is ADOPTED (a fresh
    /// [`WatchedLeg::Adopted`], watched exactly like any other leg); if
    /// the bound elapses with nothing answering, this simply RETURNS —
    /// no terminal mark, no restart — leaving the row to read whatever
    /// [`phase_of`] naturally reports (`unreachable`, if truly nothing is
    /// left) and the next `pty.open` attach's [`ensure_started`] to
    /// re-probe and start fresh.
    ///
    /// Rule D half 2: releases the `starting` claim
    /// (`Workspaces::end_capsule_start`) on EVERY leg's outcome —
    /// idempotent (harmless once [`spawn_starting_release_poll`], half 1,
    /// already cleared it for the success case, and ALWAYS harmless for
    /// a leg that started life adopted, which never held the claim in
    /// the first place) and correct regardless: once a leg's fate is
    /// known, "a launch is in flight" is no longer true for THIS leg (a
    /// restart, if one follows, is this SAME watchdog's own retry, not a
    /// new external request).
    fn install_watchdog(
        workspace_id: String,
        sot_capsule_exe: PathBuf,
        state_dir: PathBuf,
        argv: Vec<String>,
        cwd: PathBuf,
        agent_name: String,
        slug: String,
        leg: WatchedLeg,
        workspaces: Workspaces,
    ) {
        tokio::spawn(async move {
            let mut leg_opt = Some(leg);
            let mut restart_times: Vec<Instant> = Vec::new();
            loop {
                let outcome = match leg_opt.take() {
                    Some(l) => wait_and_classify(l, &workspace_id).await,
                    // A previous restart/adoption attempt itself found
                    // nothing to wait on -- counts as another crash
                    // against the same budget.
                    None => LegOutcome::Crash,
                };
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
                    LegOutcome::Contended => {
                        tracing::warn!(
                            workspace_id = %workspace_id, bound = ?CONTENTION_RETRY_BOUND,
                            "capsule supervisor watchdog: leg exited contended (70) -- re-probing for adoption, no terminal mark"
                        );
                        let deadline = Instant::now() + CONTENTION_RETRY_BOUND;
                        let mut adopted = None;
                        while Instant::now() < deadline {
                            tokio::time::sleep(CONTENTION_RETRY_INTERVAL).await;
                            let dir = state_dir.clone();
                            let probe = tokio::task::spawn_blocking(move || {
                                sot_log::supervisor_client::query_status(&dir)
                            })
                            .await;
                            match probe {
                                Ok(Ok((status, process))) => {
                                    tracing::info!(
                                        workspace_id = %workspace_id, phase = phase_str(status.phase),
                                        "capsule supervisor watchdog: contention resolved -- adopted the surviving lane"
                                    );
                                    adopted = Some(process);
                                    break;
                                }
                                // A foreign holder is first met HERE when the row was
                                // never probed before this leg (a fresh spawn that
                                // exited contended) -- note it the same once-per-row way.
                                Ok(Err(e)) => note_if_foreign(&state_dir, &e),
                                Err(_) => {}
                            }
                        }
                        match adopted {
                            Some(process) => {
                                leg_opt = Some(WatchedLeg::Adopted(process));
                                continue;
                            }
                            None => {
                                tracing::warn!(
                                    workspace_id = %workspace_id, bound = ?CONTENTION_RETRY_BOUND,
                                    "capsule supervisor watchdog: still contended after the bound -- \
                                     leaving the row for the next attach, no terminal mark"
                                );
                                return;
                            }
                        }
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
                            Ok(spawned) => leg_opt = Some(WatchedLeg::Spawned(spawned.child)),
                            Err(e) if e.kind() == ErrorKind::Unsupported => {
                                // `check_pair` refused: the binary next to this daemon
                                // is another build. No retry can change that -- mark
                                // terminal now (the error names the recovery).
                                tracing::error!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: pair mismatch -- marking terminal, no restart");
                                workspaces.mark_capsule_terminal(&workspace_id);
                                return;
                            }
                            Err(e) => {
                                tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule supervisor watchdog: restart spawn failed");
                            }
                        }
                    }
                }
            }
        });
    }

    /// [`resume_all`]'s own watchdog entry point for a workspace it
    /// ADOPTED (found a lane already answering, spawned nothing) — the
    /// `WatchedLeg::Adopted` counterpart of [`spawn_and_watch`]'s
    /// `WatchedLeg::Spawned` one, sharing [`install_watchdog`] rather
    /// than a second implementation. Deliberately skips
    /// `clear_capsule_terminal`/`spawn_starting_release_poll`: those are
    /// bookkeeping for a SPAWN ATTEMPT this daemon itself just made
    /// (`try_begin_capsule_start`'s claim, a fresh chance after a stale
    /// terminal mark) — adoption never takes that claim and, at boot,
    /// this daemon's OWN `capsule_terminal` set is fresh regardless, so
    /// there is nothing here for either to do.
    fn watch_adopted_leg(
        workspace_id: String,
        sot_capsule_exe: PathBuf,
        state_dir: PathBuf,
        argv: Vec<String>,
        cwd: PathBuf,
        agent_name: String,
        slug: String,
        process: sot_log::supervisor_client::ChallengedProcess,
        workspaces: Workspaces,
    ) {
        install_watchdog(
            workspace_id,
            sot_capsule_exe,
            state_dir,
            argv,
            cwd,
            agent_name,
            slug,
            WatchedLeg::Adopted(process),
            workspaces,
        );
    }

    /// On daemon startup: resume every REGISTERED capsule workspace's
    /// supervisor whose voyage pointer has ALREADY been published (rule
    /// B, shrink round). A row with NO published pointer — never started
    /// at all, OR a leg that crashed before ever publishing one (the
    /// exact pre-pointer crash window ADR 0041 names) — is SKIPPED
    /// entirely: `pty.open`'s start-on-attach (`ensure_started`) is what
    /// starts those now, not this scan. Before rule B this scan launched
    /// `--resume` unconditionally for EVERY registered row, including
    /// ones with no pointer at all — `sot-capsule supervise --resume`
    /// against a workspace with no leg to adopt and no pointer to found
    /// one against simply fails (exit 69), leaving a bare state directory
    /// behind and reading back as a misleading row on the owner's own box
    /// (the field finding behind this shrink round).
    ///
    /// PROBE before spawning (the daemon-boot-adopts-supervisor fix):
    /// each candidate is decided by [`start_mode_needed`] — the SAME
    /// function `pty.open`'s start-on-attach uses, over a real
    /// [`phase_of`] query of its lane, not the pointer's mere existence
    /// (round-2 Codex finding: one decision oracle, not a second
    /// `LaneDecision` type). `None` means a supervisor from a PREVIOUS
    /// daemon lifetime is still up and answering (the daemon restarted —
    /// e.g. an FE relaunch rebooting the local daemon — while its capsule
    /// supervisors, spawned detached by ADR 0042 design, outlived it):
    /// this scan ADOPTS it — logs one line, fetches the lane's own
    /// retained process handle via a second `query_status` call, and
    /// hands it to [`watch_adopted_leg`] — rather than racing a second
    /// `--resume` leg against the live one's `supervisor.lock`. That
    /// race is exactly what this fix closes: before it, the new leg
    /// always lost the fence, exited 69 (`EXIT_TERMINAL`), and the
    /// watchdog marked the row `capsule_terminal` — wrongly, since the
    /// old supervisor was still actually running the voyage the whole
    /// time (a genuinely still-tearing-down old lane instead gets the
    /// new `EXIT_CONTENDED` — see that const's own doc — never
    /// `EXIT_TERMINAL`, so a fresh spawn's own watchdog re-probes for
    /// adoption instead of marking terminal either). An adopted row
    /// reporting `EndedNoRespawn` (a resting authority) is STILL adopted
    /// here, unconditionally — no spawn either way — because a resting
    /// authority is stopped by the end flow and restarted by reset, both
    /// #182's territory, not this scan's. Only `Some(StartMode::Resume)`
    /// (pointer published, lane doesn't answer — its supervisor really
    /// is gone) still spawns `--resume` here. `Some(StartMode::Start)`
    /// shouldn't occur (the candidate filter below already requires a
    /// published pointer) but is handled defensively as a skip, same as
    /// before this fix.
    ///
    /// A state directory with NO matching registry entry is left
    /// COMPLETELY untouched, logged once (ADR 0042: "the daemon's
    /// workspace list is the list" — an orphan is not addressable
    /// through any op, so resuming it would create a live, unaddressable
    /// process; deleted, the bare-shell fallback an earlier version used
    /// here).
    ///
    /// Rule D: a candidate that still needs a spawn goes through
    /// [`start_supervisor`] — the SAME atomically-claimed spawn path
    /// `pty.open`'s start-on-attach uses — so a candidate this scan races
    /// against a concurrent `pty.open` (the exact race the field
    /// finding's own timeline could produce) spawns nothing twice; the
    /// loser gets `Ok(None)`, logged at debug (expected, not an error).
    ///
    /// Runs off the startup critical path (finding 10): `server.rs`
    /// calls this via `tokio::spawn`, never awaited, and every probe/spawn
    /// inside it is bounded to `LANE_CONCURRENCY` concurrent attempts via
    /// a semaphore — thousands of preserved workspaces cannot turn this
    /// into an unbounded synchronous fan-out before the listener binds.
    pub async fn resume_all(state_root: PathBuf, workspaces: Workspaces) {
        // Resolved ONCE here, not re-resolved per candidate below: cheap
        // either way (`std::env::current_exe()`), but a single resolved
        // `PathBuf` cloned into each task is simpler than a fallible call
        // repeated inside every spawned closure.
        let exe = match sot_capsule_exe() {
            Ok(e) => e,
            Err(_) => {
                tracing::warn!("capsule workspace resume-scan: could not locate sot-capsule.exe next to this daemon");
                return;
            }
        };
        // 2026-09-04 amendment: the inert default anchor is never resumed
        // here either, even on the rare box where a pointer already exists
        // for it (a hand-edited toml that dropped its agent after a prior
        // real run). Every OTHER `agent == "none"` capsule row still
        // resumes: `agent_argv("none")` is a real leg (the bare platform
        // shell). The predicate — and why its runtime term matters — is
        // `Workspaces::is_inert_default_anchor`.
        let candidates: Vec<(String, Vec<String>, PathBuf, String, String)> = workspaces
            .list()
            .into_iter()
            .filter(|ws| ws.runtime == "capsule")
            .filter(|ws| !workspaces.is_inert_default_anchor(ws))
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
            let exe = exe.clone();
            joins.push(tokio::spawn(async move {
                let _permit = permit.acquire_owned().await;
                let state_dir = super::state_dir_for(&state_root, &workspace_id);
                // `start_mode_needed` is documented BLOCKING (`phase_of`
                // itself is a real lane connect+challenge+status round
                // trip) -- run it the same way `workspace.list`'s own
                // lane gather already does, not directly inside this
                // async task.
                let mode = {
                    let dir = state_dir.clone();
                    tokio::task::spawn_blocking(move || start_mode_needed(&dir)).await.unwrap_or(Some(StartMode::Resume))
                };
                match mode {
                    None => {
                        // Alive: fetch the SAME lane's retained process
                        // handle via a second, explicit `query_status`
                        // call -- `start_mode_needed`'s own probe already
                        // proved it answers but (by design, shared with
                        // `ensure_started`) doesn't retain the handle this
                        // scan specifically needs to install a watchdog.
                        // A benign race: the lane could go quiet between
                        // the two calls (the old supervisor finally
                        // exiting on its own) -- this second call then
                        // simply fails and this arm logs at debug and
                        // does nothing further; the row reads whatever
                        // `phase_of` next reports, and the next attach's
                        // `ensure_started` starts it fresh if needed.
                        let dir = state_dir.clone();
                        let probe = tokio::task::spawn_blocking(move || {
                            sot_log::supervisor_client::query_status(&dir)
                        })
                        .await;
                        match probe {
                            Ok(Ok((status, process))) => {
                                tracing::info!(
                                    workspace_id = %workspace_id, phase = phase_str(status.phase),
                                    "capsule supervisor adopted (alive from a previous daemon)"
                                );
                                watch_adopted_leg(workspace_id, exe, state_dir, argv, cwd, agent_name, slug, process, workspaces);
                            }
                            Ok(Err(e)) => {
                                tracing::debug!(
                                    workspace_id = %workspace_id, error = %e,
                                    "capsule workspace resume-scan: lane went quiet between the adopt probe and the follow-up query; skipping"
                                );
                            }
                            Err(e) => {
                                tracing::warn!(workspace_id = %workspace_id, error = %e, "capsule workspace resume-scan: adopt probe task did not complete");
                            }
                        }
                    }
                    Some(StartMode::Resume) => {
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
                    }
                    Some(StartMode::Start) => {
                        // Shouldn't happen -- the candidate filter above
                        // already required a published pointer -- but
                        // stays a no-op rather than a Start: fresh starts
                        // are ensure_started's job, not this scan's.
                        tracing::debug!(
                            workspace_id = %workspace_id,
                            "capsule workspace resume-scan: no published pointer at probe time; skipping (ensure_started starts it fresh)"
                        );
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

#[cfg(test)]
mod pair_verdict_tests {
    use super::pair_verdict;

    #[test]
    fn matching_build_ids_pass() {
        assert!(pair_verdict(Some("abc123"), "abc123").is_ok());
    }

    #[test]
    fn a_different_build_is_refused_with_the_recovery() {
        let e = pair_verdict(Some("97deece7"), "9f774f74").unwrap_err();
        assert!(e.contains("97deece7") && e.contains("9f774f74"), "{e}");
        assert!(e.contains("renamed aside") && e.contains("attach the row again") && e.contains("only their"), "{e}");
    }

    #[test]
    fn a_binary_that_predates_the_subcommand_is_refused_too() {
        let e = pair_verdict(None, "9f774f74").unwrap_err();
        assert!(e.contains("predates"), "{e}");
    }
}
