//! One lifecycle observer per capsule row, the single writer of a row's phase cell: it holds the row's watch on its
//! supervisor, feeds each report to the cell, and reads the watch's end.

use super::probe::{fence_file_exists, local_phase, phase_for_missing_fence};
use super::watchdog::{install_watchdog, First};
use crate::rows::spawn::state_root::state_dir_for;
use crate::rows::workspace::{Observation, Phase, SupervisorIdentity};
use crate::rows::{Workspace, Workspaces};
use sot_capsule::attach_client::supervisor_client::{watch, CancelHandle, StatusReport, WatchStep};
use sot_platform::host::redial::Redial;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// How long a held watch waits for one report before it looks at whether its row is still registered.
const LOOK: Duration = Duration::from_secs(1);
/// How long a watch's end waits for the supervisor's process to end: a told exit ends within the supervisor's own
/// bounded wait for its watchers, so a process alive at this bound was not exiting (a cut watcher, a rebound listener).
const EXIT_WAIT: Duration = Duration::from_secs(5);
/// The wait before the observer looks again at a row it could not watch: from the floor, doubling to the cap, as every
/// redial in the tree does.
const REDIAL_FLOOR: Duration = Duration::from_millis(250);
const REDIAL_CAP: Duration = Duration::from_secs(4);

/// The ONE call site that feeds an observation into `ws`'s phase cell; a rejection logs at debug, never an error.
pub(crate) fn observe(ws: &Workspace, observation: Observation) {
    if !ws.apply_phase_observation(observation) {
        tracing::debug!(workspace_id = %ws.workspace_id, "capsule workspace observer: observation rejected (stale, or a latched terminal phase)");
    }
}

fn supervisor_of(peer: (u32, u64)) -> SupervisorIdentity {
    SupervisorIdentity { pid: peer.0, created: peer.1 }
}

fn observation_of(report: &StatusReport, supervisor: SupervisorIdentity) -> Observation {
    Observation::Phase {
        phase: local_phase(report.phase),
        supervisor,
        run: report.run.as_deref().and_then(|r| r.parse().ok()),
    }
}

/// A watch's first reply, from a round that began at spawn generation `generation`: the watch record names the supervisor
/// the hello named, `ready` as the reply said, and the cell takes it (a new epoch when it is not the cell's). `false`
/// when a spawn has begun a newer generation since the round began: the reply may come from the supervisor that spawn
/// replaces, so it is neither recorded nor fed. The row that is registered under the id is the one fed.
fn feed_first(ws: &Workspace, generation: u64, supervisor: SupervisorIdentity, report: &StatusReport) -> bool {
    if !ws.record_watch(generation, supervisor, local_phase(report.phase) == Phase::Ready) {
        return false;
    }
    if ws.current_supervisor() != Some(supervisor) {
        ws.begin_supervisor_epoch(supervisor);
    }
    observe(ws, observation_of(report, supervisor));
    true
}

/// Whether an exit a round saw is still this observer's to hand to a watchdog, under the row's guard: nothing owns the
/// row, and no spawn has begun since the round did (a spawn's own watchdog handled that exit, and may have finished).
fn exit_is_unhandled(ws: &Workspace, generation: u64) -> bool {
    ws.spawn_generation() == generation && ws.watchdog_owner().is_none()
}

/// A `row.phase` of the watched supervisor.
fn feed_event(ws: &Workspace, supervisor: SupervisorIdentity, report: &StatusReport) {
    if local_phase(report.phase) == Phase::Ready {
        ws.note_watch_ready(supervisor);
    }
    observe(ws, observation_of(report, supervisor));
}

/// Idempotent: ensures a lifecycle-observer task runs for `ws`'s id, never from `Workspaces::insert`; one already running
/// is woken, for a supervisor has just been settled that it should watch. The task keeps the id, not `ws`: a same-slug
/// `workspace.create` replaces the row under its id and finds this task running.
pub fn ensure_running(workspaces: &Workspaces, ws: &Workspace) {
    if workspaces.has_observer(&ws.workspace_id) {
        workspaces.wake_observer(&ws.workspace_id);
        return;
    }
    let Some(state_root) = sot_platform::host::state_dir::sot_state_dir() else {
        return;
    };
    let state_dir = state_dir_for(&state_root, &ws.workspace_id);
    // Obtained outside the loop so removal can interrupt a watch blocked in `spawn_blocking`.
    let cancel_handle = CancelHandle::new();
    let cancel: Arc<dyn Fn() + Send + Sync> = {
        let cancel_handle = cancel_handle.clone();
        Arc::new(move || cancel_handle.cancel())
    };
    let wake = Arc::new(Notify::new());
    let handle = tokio::spawn(run(ws.workspace_id.clone(), state_dir, cancel_handle, Arc::clone(&wake), workspaces.clone()));
    workspaces.install_observer(&ws.workspace_id, handle, cancel, wake);
}

/// What one round of the observer found.
enum Round {
    /// No fence file, or a dial that failed: observed as such, and nothing to read.
    Unwatched,
    /// A watch ended: the code `row.exiting` carried if it did, whether the supervisor's process ended within
    /// [`EXIT_WAIT`], how long the watch lived, and the row's spawn generation when the round began.
    Ended { code: Option<i32>, process_ended: bool, lasted: Duration, generation: u64 },
}

/// Watches the row's supervisor round after round until the row is gone. Each report feeds the row registered under
/// `workspace_id` when it arrives, and the task holds it no longer.
async fn run(workspace_id: String, state_dir: PathBuf, cancel: CancelHandle, wake: Arc<Notify>, workspaces: Workspaces) {
    let mut redial = Redial::new(REDIAL_FLOOR, REDIAL_CAP);
    loop {
        let round = {
            let (id, dir, cancel, workspaces) = (workspace_id.clone(), state_dir.clone(), cancel.clone(), workspaces.clone());
            tokio::task::spawn_blocking(move || watch_round(&id, &dir, &cancel, &workspaces)).await.unwrap_or(Round::Unwatched)
        };
        let Some(ws) = workspaces.resolve(Some(&workspace_id)) else {
            return;
        };
        let lasted = match round {
            Round::Unwatched => Duration::ZERO,
            Round::Ended { code, process_ended, lasted, generation } => {
                // A watch that ended with its supervisor gone and nobody to restart it is an adopted supervisor's exit;
                // exit 0 is a stop, which nothing restarts.
                if process_ended && code != Some(0) && exit_is_unhandled(&ws, generation) {
                    adopt_exit(&workspaces, &workspace_id, &state_dir, code, generation).await;
                }
                lasted
            }
        };
        drop(ws);
        tokio::select! {
            () = tokio::time::sleep(redial.after(lasted)) => {}
            () = wake.notified() => {}
        }
    }
}

/// One BLOCKING round: no fence file -> `Stopped`; otherwise one watch, held until it ends.
fn watch_round(workspace_id: &str, state_dir: &Path, cancel: &CancelHandle, workspaces: &Workspaces) -> Round {
    // The spawn generation this round is bound to, read before anything is dialed.
    let Some(generation) = workspaces.resolve(Some(workspace_id)).map(|ws| ws.spawn_generation()) else {
        return Round::Unwatched;
    };
    let feed = |observation: Observation| {
        if let Some(ws) = workspaces.resolve(Some(workspace_id)) {
            observe(&ws, observation);
        }
    };
    if phase_for_missing_fence(fence_file_exists(state_dir)).is_some() {
        feed(Observation::Stopped);
        return Round::Unwatched;
    }
    let mut held = match watch(state_dir, cancel) {
        Ok(held) => held,
        Err(sot_capsule::Error::VersionSkew) => {
            feed(Observation::Foreign);
            return Round::Unwatched;
        }
        Err(_) => {
            feed(Observation::Failed);
            return Round::Unwatched;
        }
    };
    let began = Instant::now();
    let supervisor = supervisor_of(held.peer());
    match workspaces.resolve(Some(workspace_id)) {
        Some(ws) if feed_first(&ws, generation, supervisor, held.first()) => {}
        _ => return Round::Unwatched,
    }
    let mut code = None;
    loop {
        match held.next(LOOK) {
            WatchStep::Phase(report) => {
                if let Some(ws) = workspaces.resolve(Some(workspace_id)) {
                    feed_event(&ws, supervisor, &report);
                }
            }
            WatchStep::Exiting(c) => code = Some(c),
            WatchStep::Quiet => {
                if workspaces.resolve(Some(workspace_id)).is_none() {
                    break;
                }
            }
            WatchStep::Ended => break,
        }
    }
    if workspaces.resolve(Some(workspace_id)).is_none() {
        return Round::Unwatched;
    }
    let process_ended = held.end(EXIT_WAIT);
    // The process is gone, whatever its last report said, unless a spawn has replaced it since.
    if process_ended && workspaces.resolve(Some(workspace_id)).is_some_and(|ws| ws.spawn_generation() == generation) {
        feed(Observation::Gone);
    }
    Round::Ended { code, process_ended, lasted: began.elapsed(), generation }
}

/// An adopted supervisor's exit: nothing of this daemon's launch watches it, so the observer hands it to a watchdog whose
/// first outcome is the exit the watch carried (none for a kill), exactly as a child's exit, to be restarted within rows'
/// budget. Under the row's guard, where it checks again that nothing owns the row; the watchdog announces its ownership
/// before this returns, and its own recheck decides whether anything is restarted.
async fn adopt_exit(workspaces: &Workspaces, workspace_id: &str, state_dir: &Path, code: Option<i32>, generation: u64) {
    let Some(guard) = workspaces.capsule_guard(workspace_id) else {
        return;
    };
    let _held = guard.lock().await;
    let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
        return;
    };
    if !exit_is_unhandled(&ws, generation) || workspaces.is_inert_default_anchor(&ws) {
        return;
    }
    let argv = match crate::agents::argv::agent_argv(&ws.agent(), Some(&ws.project_root)) {
        Ok(argv) => argv,
        Err(error) => {
            tracing::warn!(workspace_id = %workspace_id, %error, "capsule workspace observer: an adopted supervisor ended and the row's recipe could not be built; not restarting");
            return;
        }
    };
    let exe = match crate::rows::spawn::detach::sot_capsule_exe() {
        Ok(exe) => exe,
        Err(error) => {
            tracing::warn!(workspace_id = %workspace_id, %error, "capsule workspace observer: an adopted supervisor ended and sot-capsule could not be located; not restarting");
            return;
        }
    };
    tracing::info!(workspace_id = %workspace_id, ?code, "capsule workspace observer: an adopted supervisor ended; handing the row to a watchdog");
    install_watchdog(
        workspace_id.to_string(),
        exe,
        state_dir.to_path_buf(),
        argv,
        ws.project_root.clone(),
        ws.agent_name(),
        ws.slug.clone(),
        First::Exited(code, generation),
        workspaces.clone(),
    );
}

#[cfg(test)]
#[path = "observer_tests.rs"]
mod observer_tests;
