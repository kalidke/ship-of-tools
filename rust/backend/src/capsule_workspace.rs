// capsule_workspace.rs — ADR 0042 slice L1a: the daemon's capsule
// workspace runtime (Windows-first). Every NEW workspace on Windows is a
// capsule: one `sot-capsule supervise <state-dir>` authority per
// workspace, spawned DETACHED so it survives the daemon's own exit — the
// daemon is never its kill domain. `runtime: "tmux"` rows stay exactly
// what they are today; this module never touches them.
//
// Split deliberately into PURE helpers (no OS call: the state-dir path
// arithmetic, the phase-to-wire-string mapping, the agent argv choice)
// and the WINDOWS-ONLY runtime (spawning, querying, ending a supervisor
// over `sot_log::supervisor_client`). The pure half is compiled and
// unit-tested on every platform — ADR 0042 L1a's own gate runs
// `cargo test --workspace` on Linux, and gating path/string arithmetic
// behind `#[cfg(windows)]` would only prevent that gate from ever
// exercising it. On non-Windows hosts nothing in this module is called at
// all: `workspace.create` keeps today's tmux path unchanged (see
// `workspaces.rs`/`handlers.rs`).

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
/// same flags `ccb` itself execs with (`claude --dangerously-skip-
/// permissions /sot-session-start`), relying on `claude` being on the
/// daemon's own PATH — a detached child inherits it, same as any spawned
/// process. Anything else (`"none"`, and `"codex"`, which has no known
/// Windows launcher either) falls back to the bare platform shell rather
/// than guessing at a command that does not exist — an argued limitation,
/// not a silent one; a Windows codex launcher is future work.
// `#[cfg_attr(not(windows), allow(dead_code))]` on the four items below,
// matching `sot_log::lib.rs`'s own `host_handshake`/`deadline` precedent:
// each is portable and pure (so its OWN unit tests run on every
// platform), but its only PRODUCTION caller lives inside this module's
// `#[cfg(windows)] mod windows_runtime` — so a non-Windows, non-test build
// (`cargo check`/`cargo clippy` without `--tests`) genuinely has none,
// and would otherwise warn on code that is real, not dead, on the
// platform it exists for.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn agent_argv(agent_kind: &str) -> Vec<String> {
    match agent_kind {
        "claude" => vec![
            "claude".to_string(),
            "--dangerously-skip-permissions".to_string(),
            "/sot-session-start".to_string(),
        ],
        _ => vec!["cmd.exe".to_string()],
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

#[cfg(windows)]
mod windows_runtime {
    use super::{agent_argv, mode_flag, StartMode};
    use crate::workspaces::Workspaces;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};

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
    pub struct SpawnedSupervisor {
        pub child: Child,
        /// `true` iff the breakaway attempt was denied and this
        /// supervisor was spawned still inside the daemon's own job — it
        /// will die if that job is ever closed with
        /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Logged at spawn time;
        /// nothing in ADR 0042 L1a's scope asks a client to see this.
        pub degraded: bool,
    }

    /// Spawn `sot-capsule.exe supervise <state_dir> <--start|--resume>
    /// --assume-no-rollback-target -- <agent argv>` DETACHED, so the
    /// supervisor authority survives the daemon's own exit — the daemon
    /// must not be its kill domain (ADR 0042 L1a). `--assume-no-rollback-
    /// target` is mandatory: `sot_log::supervisor::supervise` itself
    /// refuses (exit 69) without it pre-U4 (no release-apply transaction
    /// exists yet to supply real rollout evidence — see that function's
    /// own doc).
    ///
    /// Attempts `CREATE_BREAKAWAY_FROM_JOB` unconditionally alongside
    /// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`: a denied breakaway
    /// reports `ERROR_ACCESS_DENIED` distinctly from every other spawn
    /// failure (a missing binary, an invalid argv, …), which is exactly
    /// the signal that separates "retry without it, DEGRADED" from
    /// "propagate the real error".
    pub fn spawn_detached_supervisor(
        sot_capsule_exe: &Path,
        state_dir: &Path,
        mode: StartMode,
        agent_argv: &[String],
        cwd: &Path,
    ) -> std::io::Result<SpawnedSupervisor> {
        let base_flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        let build = |flags: u32| -> Command {
            use std::os::windows::process::CommandExt;
            let mut cmd = Command::new(sot_capsule_exe);
            cmd.arg("supervise")
                .arg(state_dir)
                .arg(mode_flag(mode))
                .arg("--assume-no-rollback-target")
                .arg("--")
                .args(agent_argv)
                .current_dir(cwd)
                .creation_flags(flags)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            cmd
        };
        match build(base_flags | CREATE_BREAKAWAY_FROM_JOB).spawn() {
            Ok(child) => Ok(SpawnedSupervisor { child, degraded: false }),
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                tracing::warn!(
                    state_dir = ?state_dir,
                    "capsule supervisor breakaway denied — spawning DEGRADED (still in the daemon's job)"
                );
                let child = build(base_flags).spawn()?;
                Ok(SpawnedSupervisor { child, degraded: true })
            }
            Err(e) => Err(e),
        }
    }

    /// One capsule workspace's supervisor-lane status, as the daemon's
    /// own wire vocabulary — never the raw `sot_log` types, so
    /// `handlers.rs` has nothing Windows-specific to import.
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
    /// voyage}` on the lane and wait (bounded — ADR 0042 L1a's own daemon-
    /// side 30s ceiling, inside the ADR's FE-quit 90s bound), reporting
    /// the outcome honestly. `Ok(None)` means the workspace never had a
    /// leg to end (no voyage observed yet — nothing to do). The state
    /// directory is NEVER deleted here: the record persists by design.
    pub fn end_run(
        state_dir: &Path,
        reason: &str,
        budget: std::time::Duration,
    ) -> std::io::Result<Option<sot_log::supervisor_client::EndRunOutcome>> {
        let status = sot_log::supervisor_client::query_status(state_dir)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let Some(voyage) = status.voyage else {
            return Ok(None);
        };
        let outcome = sot_log::supervisor_client::end_run(state_dir, &voyage, reason, budget)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(Some(outcome))
    }

    /// On daemon startup: scan `<state-root>/workspaces/*/` (ADR 0042
    /// L1a) and, for each state dir with a valid `drawer.voyage` pointer,
    /// spawn `sot-capsule supervise <dir> --resume …` DETACHED — the
    /// supervisor itself decides adopt-vs-spawn (ADR 0041's start-mode
    /// table); the daemon never does. A dir with no valid pointer yet
    /// (a workspace whose FIRST leg never got that far) is skipped — its
    /// own `--start` already ran once at create time and either finished
    /// publishing the pointer or the workspace was never really live.
    ///
    /// Agent argv is reconstructed from `workspaces`' own registry (the
    /// SAME persisted metadata `workspace.create` wrote) when a matching
    /// entry exists — needed because `--resume` may still have to spawn a
    /// fresh leg (ADR 0041: "`--resume` | open or recovering, no live
    /// capsule | RECOVER and spawn a new leg"), which requires the same
    /// producer argv `--start` used. A state dir with NO matching
    /// registry entry (an orphan — e.g. its toml was lost) is still
    /// resumed with a safe bare-shell fallback argv, logged loudly: an
    /// adopted-but-nameless capsule is recoverable and inspectable; an
    /// abandoned live one is not.
    pub fn resume_all(state_root: &Path, workspaces: &Workspaces) {
        let dir = state_root.join("workspaces");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!(dir = ?dir, error = %e, "capsule workspace resume-scan: could not read state root");
                return;
            }
        };
        let Ok(sot_capsule) = sot_capsule_exe() else {
            tracing::warn!("capsule workspace resume-scan: could not locate sot-capsule.exe next to this daemon");
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let workspace_id = match path.file_name().and_then(|n| n.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            match sot_log::pointer::validate(&path) {
                sot_log::pointer::PointerState::Valid(_) => {}
                _ => continue, // no adoptable voyage yet -- nothing to resume
            }
            let ws = workspaces.resolve(Some(&workspace_id));
            let (argv, cwd) = match &ws {
                Some(ws) if ws.runtime == "capsule" => {
                    (agent_argv(&ws.agent), ws.project_root.clone())
                }
                _ => {
                    tracing::warn!(
                        workspace_id = %workspace_id,
                        "capsule workspace resume-scan: no matching registry entry -- \
                         resuming with a bare-shell fallback argv"
                    );
                    (agent_argv("none"), std::env::temp_dir())
                }
            };
            match spawn_detached_supervisor(&sot_capsule, &path, StartMode::Resume, &argv, &cwd) {
                Ok(spawned) => {
                    // Detached on purpose (ADR 0042 L1a: the daemon must
                    // not be its kill domain) -- forget the handle rather
                    // than reap it, exactly like a fresh `workspace.create`
                    // spawn does.
                    std::mem::drop(spawned.child);
                    tracing::info!(
                        workspace_id = %workspace_id, degraded = spawned.degraded,
                        "capsule workspace supervisor resumed"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        workspace_id = %workspace_id, error = %e,
                        "capsule workspace supervisor resume spawn failed"
                    );
                }
            }
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
            agent_argv("claude"),
            vec!["claude", "--dangerously-skip-permissions", "/sot-session-start"]
        );
    }

    #[test]
    fn agent_argv_falls_back_to_the_platform_shell_for_none_and_unknown_kinds() {
        assert_eq!(agent_argv("none"), vec!["cmd.exe"]);
        assert_eq!(agent_argv("codex"), vec!["cmd.exe"]);
        assert_eq!(agent_argv("bogus"), vec!["cmd.exe"]);
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
}
