//! Starting a capsule row's supervisor: spawn and hand to a watchdog, the run gate, and the post-spawn settle.

use super::admission::{is_pending, pending, PENDING_AUTHORITY};
use super::probe::phase_of;
use super::watchdog::{install_watchdog, First};
use super::UNREACHABLE_PHASE;
use crate::rows::gate::StartPermit;
use crate::rows::spawn::detach::{sot_capsule_exe, spawn_detached_supervisor, Spawn};
use crate::rows::Workspaces;
use sot_capsule::supervisor::StartMode;
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
/// Order: spawn, SETTLE, install the watchdog, wake the observer. The settle is a bound on how long the caller's guard
/// waits for the supervisor to be reachable; the window between spawn and first answered status contains no watchdog at
/// all, so no terminal mark and no second spawn can land inside it. The phase this settles to is what the caller reports;
/// the row's cell is the observer's to write, from the supervisor's watch.
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
    // A new generation begins: the supervisor about to start has been watched by no one, and every outcome of an earlier
    // one (an observer's round, a watchdog's exit) is now stale.
    let generation = workspaces.resolve(Some(&workspace_id)).map_or(0, |ws| ws.begin_spawn());
    let child = match spawn_detached_supervisor(
        permit, sot_capsule_exe, state_dir, mode, agent_argv, cwd, agent_name, &workspace_id, &slug, &agent_kind, &account,
    )? {
        Spawn::Started(child) => child,
        // The row's fence is claimed: nothing was started, and no watchdog is installed for a launch that never was.
        Spawn::Contended => return Err(pending()),
    };
    let phase = settle_after_spawn(state_dir, &workspace_id);
    install_watchdog(
        workspace_id.clone(),
        sot_capsule_exe.to_path_buf(),
        state_dir.to_path_buf(),
        agent_argv.to_vec(),
        cwd.to_path_buf(),
        agent_name.to_string(),
        slug,
        First::Child(child, generation),
        workspaces.clone(),
    );
    // The supervisor authors its own identity and this daemon learns it from the observer's watch, which the settle's end
    // wakes: the settle itself writes nothing.
    workspaces.wake_observer(&workspace_id);
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
/// act, `rust/capsule/src/supervisor/`) once it actually runs, so a
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
    .map_err(|e| {
        if is_pending(&e) {
            PENDING_AUTHORITY.to_string()
        } else {
            format!("capsule supervisor spawn failed: {e}")
        }
    })
}

/// Resets the row's live authority into a new run — a run start, so
/// it passes the gate first. The ONLY `supervisor_client::reset` call
/// outside tests; the retire arm of [`ensure_started`] and
/// `reauth::reset_replacement_run` both come through here.
pub(crate) fn reset_run(workspaces: &Workspaces, workspace_id: &str, state_dir: &Path) -> Result<(), String> {
    let _permit = workspaces.begin_start(workspace_id)?;
    sot_capsule::attach_client::supervisor_client::reset(state_dir).map_err(|e| e.to_string())
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

/// Waits under the caller's guard for a spawn to settle: until its supervisor answers at all (`starting` included), polling
/// until [`spawn_settle_deadline`] (timeout WARNS). BLOCKING.
pub(super) fn settle_after_spawn(state_dir: &Path, workspace_id: &str) -> &'static str {
    let settled = settle_probe(state_dir, workspace_id);
    // Test seam: a settle that takes this many more milliseconds, so a test proves the restart budget does not depend on
    // how long settles take. Inert unless named.
    if let Some(ms) = std::env::var("SOT_TEST_SETTLE_DELAY_MS").ok().and_then(|s| s.parse::<u64>().ok()) {
        std::thread::sleep(Duration::from_millis(ms));
    }
    settled
}

/// Whether a probe's phase ends the settle: any supervisor's answer does, `starting` included.
fn settled(phase: &str) -> bool {
    phase != UNREACHABLE_PHASE
}

fn settle_probe(state_dir: &Path, workspace_id: &str) -> &'static str {
    let settle = spawn_settle_deadline();
    let deadline = Instant::now() + settle;
    loop {
        let phase = phase_of(state_dir);
        if settled(phase) {
            return phase;
        }
        if Instant::now() >= deadline {
            tracing::warn!(
                workspace_id = %workspace_id, phase, deadline = ?settle,
                "capsule workspace: lane did not settle within the post-spawn deadline"
            );
            return phase;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
#[path = "start_tests.rs"]
mod tests;
