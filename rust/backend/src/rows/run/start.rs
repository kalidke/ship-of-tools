//! Starting a capsule row's supervisor: spawn and hand to a watchdog, the run gate, and the post-spawn settle.

use super::observer::observe_with_adoption;
use super::probe::probe;
use super::watchdog::{identity_of, install_watchdog};
use super::UNREACHABLE_PHASE;
use crate::rows::spawn::detach::{sot_capsule_exe, spawn_detached_supervisor, StartMode};
use crate::rows::gate::StartPermit;
use crate::rows::Workspaces;
use std::path::Path;
use std::time::{Duration, Instant};

/// Spawn a capsule's supervisor authority AND hand it to a watchdog
/// task together (ADR 0042 L1a, Codex review finding 6: "hand every
/// spawned Child to a waiter task") — the daemon has become ADR
/// 0041's own launcher for every capsule workspace it creates or
/// resumes. Returns synchronously once the FIRST spawn attempt is
/// known to have succeeded or failed, so a caller (`workspace.create`,
/// finding 1) can roll back on a synchronous failure; the watchdog
/// itself then runs entirely in the background.
///
/// ADR 0043 decision 33: no claim to release any more — the CALLER
/// holds this workspace's row guard (`Workspaces::capsule_guard`) for
/// the whole spawn attempt (`start_supervisor`'s own doc), so nothing
/// here needs to signal "the launch is no longer in flight" the way
/// the old `starting` claim did.
///
/// Order (supervisor-epoch ruling): spawn, SETTLE, adopt, install
/// the watchdog — the settle moved inward by one frame from
/// [`start_supervisor`], where it used to sit. Same 2s bound, same
/// caller's guard, same total latency; what changes is that the
/// window between spawn and first answered status now contains no
/// watchdog at all, so no terminal mark and no second spawn can
/// land inside it. The phase this settles to is what the caller
/// reports.
pub fn spawn_and_watch(
    permit: &StartPermit,
    sot_capsule_exe: &Path,
    state_dir: &Path,
    mode: StartMode,
    agent_argv: &[String],
    cwd: &Path,
    agent_name: &str,
    workspace_id: String,
    slug: String,
    workspaces: Workspaces,
) -> std::io::Result<&'static str> {
    // Accounts brief: resolved from the registry HERE, where every
    // spawn path (create, resume, start-on-attach) already converges
    // with both `workspace_id` and `workspaces` in hand -- rather than
    // threading two more scalars through every caller up the chain
    // (`start_supervisor`, `resume_locked`, `ensure_started`, …), which
    // never otherwise need to know an agent's KIND, only its
    // already-resolved argv. The pair is read at the moment of each
    // spawn and never captured, so the watchdog's own crash-restart
    // resolves it again rather than carrying this one (`workspace.
    // reauth` can move the account under a parked watchdog).
    // `unwrap_or_default` (kind "", account "") on a row gone by now
    // degrades to `account_env`'s own empty-account no-op below --
    // never worse than the row simply not existing.
    let (agent_kind, account) = workspaces
        .resolve(Some(&workspace_id))
        .map(|ws| (ws.agent(), ws.account()))
        .unwrap_or_default();
    let child = spawn_detached_supervisor(
        permit, sot_capsule_exe, state_dir, mode, agent_argv, cwd, agent_name, &workspace_id, &slug, &agent_kind, &account,
    )?;
    // The supervisor authors its own identity; this daemon only
    // LEARNS it, here, from the first status the settle draws out.
    // A settle that yields no `Phase` leaves the row unclaimed --
    // best-effort by design, since the background observer adopts
    // whenever the lane does answer.
    let (phase, observation) = settle_after_spawn(state_dir, &workspace_id);
    let identity = identity_of(&observation);
    if let Some(ws) = workspaces.resolve(Some(&workspace_id)) {
        observe_with_adoption(&ws, observation);
    }
    install_watchdog(
        workspace_id,
        sot_capsule_exe.to_path_buf(),
        state_dir.to_path_buf(),
        agent_argv.to_vec(),
        cwd.to_path_buf(),
        agent_name.to_string(),
        slug,
        child,
        identity,
        workspaces,
    );
    Ok(phase)
}

/// The spawn path shared by `workspace.create` (mode `Start` always — a
/// brand new workspace has no state dir yet), `pty.open`'s
/// start-on-attach ([`ensure_started`], mode picked by
/// [`start_mode_for_phase`]), and `resume_all` (via [`resume_locked`]):
/// locate `sot-capsule.exe` and spawn-and-watch it. ADR 0042 L1a Codex
/// review finding 1's
/// synchronous-failure contract applies to every caller: an `Err`
/// here means no supervisor is running, and the caller must refuse
/// its own op with this text rather than silently proceeding.
///
/// Rule C (shrink round): does NOT create the state directory —
/// `sot-capsule supervise` creates its own (`supervise_inner`'s first
/// act, `rust/log/src/supervisor/`) once it actually runs, so a
/// synchronous spawn failure here leaves nothing behind at all, not
/// even an empty directory a later `phase_of` could misread.
///
/// ADR 0043 decision 33: the CALLER holds this workspace's row guard
/// (`Workspaces::capsule_guard`) for this whole call — every one does:
/// `ensure_started` and `resume_if_absent`/`resume_locked` take it at
/// their own entry, `workspace.create` and `resume_all` take it
/// around their own call site (`rows/ops/create.rs`, this module's
/// `resume_all`). With the guard already held by every caller, at
/// most one spawn attempt per row can ever be in flight.
///
/// Codex review (2026-09-11): establishes lane REACHABILITY before
/// returning, not merely a successful spawn — [`settle_after_spawn`],
/// still under the caller's own guard, now performed one frame
/// inward by [`spawn_and_watch`] and simply handed back here. Every
/// spawner converges on this ONE wait: fresh attach
/// (`ensure_started`'s Start arm), a resume (`resume_locked`),
/// `workspace.create`, and `resume_all` all call this function and
/// get it for free. The watchdog's own
/// restart is the one spawner that does NOT — it never installs a
/// SECOND watchdog on top of its own loop, so it calls
/// [`spawn_detached_supervisor`] directly and then
/// [`settle_after_spawn`] itself, the same shared wait.
pub fn start_supervisor(
    state_root: &Path,
    workspace_id: &str,
    mode: StartMode,
    agent_argv: &[String],
    project_root: &Path,
    agent_name: &str,
    slug: &str,
    workspaces: Workspaces,
) -> Result<&'static str, String> {
    // The run gate first, held to return: a refused start locates and
    // spawns nothing.
    let permit = workspaces.begin_start(workspace_id)?;
    let state_dir = super::state_dir_for(state_root, workspace_id);
    let exe = match sot_capsule_exe() {
        Ok(exe) => exe,
        Err(e) => return Err(format!("could not locate sot-capsule.exe next to this daemon: {e}")),
    };
    spawn_and_watch(
        &permit,
        &exe,
        &state_dir,
        mode,
        agent_argv,
        project_root,
        agent_name,
        workspace_id.to_string(),
        slug.to_string(),
        workspaces.clone(),
    )
    .map_err(|e| format!("capsule supervisor spawn failed: {e}"))
}

/// Mints a fresh voyage on the row's live authority — a run start, so
/// it passes the gate first. The ONLY `supervisor_client::reset` call
/// outside tests; the retire arm of [`ensure_started`] and
/// `reauth::mint_replacement_voyage` both come through here.
pub(crate) fn reset_run(workspaces: &Workspaces, workspace_id: &str, state_dir: &Path) -> Result<String, String> {
    let _permit = workspaces.begin_start(workspace_id)?;
    sot_log::attach_client::supervisor_client::reset(state_dir).map_err(|e| e.to_string())
}

/// Bound for [`settle_after_spawn`] — the ONE deadline every spawn
/// path shares (Codex review, 2026-09-11): fresh attach, boot resume,
/// create, and the watchdog's own restart all wait this long, no
/// more and no less, for a freshly spawned authority to become
/// observable before the row's guard (held by every one of them for
/// this whole wait) is released.
const SPAWN_SETTLE_DEADLINE: Duration = Duration::from_secs(2);

/// `SOT_TEST_SPAWN_SETTLE_MS` overrides [`SPAWN_SETTLE_DEADLINE`] for tests, read once per process (the
/// `shutdown::shutdown_bound` convention); only tests set it. Every spawn path still shares it.
fn spawn_settle_deadline() -> Duration {
    static OVERRIDE_MS: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let override_ms =
        *OVERRIDE_MS.get_or_init(|| std::env::var("SOT_TEST_SPAWN_SETTLE_MS").ok().and_then(|s| s.parse().ok()));
    override_ms.map(Duration::from_millis).unwrap_or(SPAWN_SETTLE_DEADLINE)
}

/// Waits under the caller's guard for a spawn to settle, polling until [`spawn_settle_deadline`] (timeout WARNS). BLOCKING.
pub(super) fn settle_after_spawn(state_dir: &Path, workspace_id: &str) -> (&'static str, crate::rows::workspace::Observation) {
    let settle = spawn_settle_deadline();
    let starting_phase = super::phase_str(sot_log::lane::wire::SupervisorPhase::Starting);
    let deadline = Instant::now() + settle;
    loop {
        let (phase, observation) = probe(state_dir);
        if phase != UNREACHABLE_PHASE && phase != starting_phase {
            return (phase, observation);
        }
        if Instant::now() >= deadline {
            tracing::warn!(
                workspace_id = %workspace_id, phase, deadline = ?settle,
                "capsule workspace: lane did not settle within the post-spawn deadline"
            );
            return (phase, observation);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::spawn::state_root::state_dir_for;

    /// `source` without its `#[cfg(test)]` modules and its comment lines.
    /// A module ends at the first `}` line at its own indentation; counting
    /// braces would miscount the ones inside string literals.
    fn without_test_modules(source: &str) -> String {
        let mut out = String::new();
        let mut lines = source.lines().peekable();
        while let Some(line) = lines.next() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            let next_is_mod = lines.peek().is_some_and(|next| {
                let next = next.trim_start();
                next.starts_with("mod ") || next.starts_with("pub mod ") || next.starts_with("pub(crate) mod ")
            });
            if trimmed == "#[cfg(test)]" && next_is_mod {
                let header = lines.next().unwrap_or_default();
                if header.trim_end().ends_with(';') || header.trim_end().ends_with('}') {
                    continue;
                }
                let close = format!("{}}}", &header[..header.len() - header.trim_start().len()]);
                for skipped in lines.by_ref() {
                    if skipped.trim_end() == close {
                        break;
                    }
                }
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
        out
    }

    // #8: every run start passes the gate. The compiler enumerates spawns
    // through `spawn_detached_supervisor`'s permit parameter; this pins
    // what it cannot see — that the one reset is behind the gate, that
    // every spawn call hands a permit, and that a closing gate refuses
    // before anything is located or dialled.
    #[test]
    fn every_run_start_path_takes_a_permit() {
        let mut faults = Vec::new();

        let reg = crate::rows::Workspaces::new();
        assert!(reg.close_gate_and_settle(std::time::Instant::now()));
        let root = tempfile::tempdir().unwrap();
        let refusal = "workspace ws-gate-1 cannot start: this computer's backend is shutting down".to_string();
        let started = start_supervisor(
            root.path(), "ws-gate-1", StartMode::Start, &["true".to_string()], root.path(), "", "gate", reg.clone(),
        );
        if started != Err(refusal.clone()) {
            faults.push(format!("start_supervisor with the gate closing answered {started:?}"));
        }
        let reset = reset_run(&reg, "ws-gate-1", &state_dir_for(root.path(), "ws-gate-1"));
        if reset != Err(refusal.clone()) {
            faults.push(format!("reset_run with the gate closing answered {reset:?}"));
        }

        let reset_needle = "supervisor_client::reset(";
        let spawn_needle = "spawn_detached_supervisor(";
        let mut resets = 0;
        let mut spawn_calls = 0;
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        let mut pending = vec![src.clone()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    files.push(path);
                }
            }
        }
        files.sort();
        let mut files_read = 0;
        for path in files {
            // Test files have no `#[cfg(test)] mod` wrapper for `without_test_modules` to strip.
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let in_tests_folder = path.strip_prefix(&src).is_ok_and(|rel| {
                rel.parent().is_some_and(|dirs| dirs.components().any(|c| c.as_os_str() == "tests"))
            });
            if in_tests_folder
                || name.ends_with("_tests.rs")
                || name == "tests.rs"
                || name == "test_support.rs"
                || name.starts_with("tests_")
                || name.contains("_tests_")
            {
                continue;
            }
            files_read += 1;
            let text = without_test_modules(&std::fs::read_to_string(&path).unwrap());
            for (pos, _) in text.match_indices(reset_needle) {
                resets += 1;
                let enclosing = text[..pos].rfind("fn ").map(|at| &text[at + 3..]).unwrap_or("");
                if !enclosing.starts_with("reset_run(") {
                    faults.push(format!("{}: supervisor_client::reset called outside reset_run", path.display()));
                }
            }
            for (pos, _) in text.match_indices(spawn_needle) {
                let args = text[pos + spawn_needle.len()..].trim_start();
                if text[..pos].ends_with("fn ") {
                    if !args.starts_with("_permit: &StartPermit,") {
                        faults.push(format!("{}: spawn_detached_supervisor does not take a permit first", path.display()));
                    }
                    continue;
                }
                spawn_calls += 1;
                let first = args.split(',').next().unwrap_or("");
                if !first.contains("permit") {
                    faults.push(format!("{}: a spawn_detached_supervisor call passes {first:?} first", path.display()));
                }
            }
        }
        if files_read == 0 {
            faults.push(format!("the scan read no source files under {}", src.display()));
        }
        if resets != 1 {
            faults.push(format!("supervisor_client::reset occurs {resets} times outside tests, not once"));
        }
        if spawn_calls < 2 {
            faults.push(format!("found {spawn_calls} spawn_detached_supervisor calls; the scan is not seeing the spawns"));
        }
        assert!(faults.is_empty(), "{faults:#?}");
    }
}
