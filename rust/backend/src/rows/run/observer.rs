//! One lifecycle observer per capsule row, the single writer of a row's phase cell.

use super::probe::{local_phase, phase_for_missing_pointer};
use crate::rows::spawn::state_root::state_dir_for;
use crate::rows::workspace::{Observation, SupervisorIdentity};
use crate::rows::{Workspace, Workspaces};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Matches the attach client's own liveness poll interval.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// The ONE call site that feeds an observation into `ws`'s phase cell; a rejection logs at debug, never an error.
pub(crate) fn observe(ws: &Workspace, observation: Observation) {
    if !ws.apply_phase_observation(observation) {
        tracing::debug!(workspace_id = %ws.workspace_id, "capsule workspace observer: observation rejected (stale, or a latched terminal phase)");
    }
}

/// Adopts the observation's supervisor as this row's epoch if it differs, then feeds it (guarded callers only).
pub(super) fn observe_with_adoption(ws: &crate::rows::Workspace, observation: crate::rows::workspace::Observation) {
    if let crate::rows::workspace::Observation::Phase { supervisor, .. } = &observation {
        if ws.current_supervisor() != Some(*supervisor) {
            ws.begin_supervisor_epoch(*supervisor);
        }
    }
    super::observer::observe(ws, observation);
}

/// Idempotent: ensures a lifecycle-observer task runs for `ws`'s id, never from `Workspaces::insert`. The task keeps
/// the id, not `ws`: a same-slug `workspace.create` replaces the row under its id and finds this task running.
pub fn ensure_running(workspaces: &Workspaces, ws: &Workspace) {
    if workspaces.has_observer(&ws.workspace_id) {
        return;
    }
    let Some(state_root) = sot_log::host::state_dir::sot_state_dir() else {
        return;
    };
    let state_dir = state_dir_for(&state_root, &ws.workspace_id);
    let persistent = sot_log::attach_client::supervisor_client::Persistent::new(&state_dir);
    // Obtained outside the loop so removal can interrupt a round blocked in `spawn_blocking`.
    let cancel_handle = persistent.cancel_handle();
    let cancel: Arc<dyn Fn() + Send + Sync> = Arc::new(move || cancel_handle.cancel());
    let handle = tokio::spawn(run(ws.workspace_id.clone(), state_dir, persistent, workspaces.clone()));
    workspaces.install_observer(&ws.workspace_id, handle, cancel);
}

/// Immediate first round, then every `POLL_INTERVAL`; exits once its row is gone. Each round feeds the row registered
/// under `workspace_id` when the round ends, and holds it no longer.
async fn run(
    workspace_id: String,
    state_dir: PathBuf,
    mut persistent: sot_log::attach_client::supervisor_client::Persistent,
    workspaces: Workspaces,
) {
    loop {
        let dir = state_dir.clone();
        let (returned, observation) = tokio::task::spawn_blocking(move || {
            let obs = poll_once(&dir, &mut persistent);
            (persistent, obs)
        })
        .await
        .unwrap_or_else(|_join_err| {
            (sot_log::attach_client::supervisor_client::Persistent::new(&state_dir), Observation::Failed)
        });
        persistent = returned;
        match workspaces.resolve(Some(&workspace_id)) {
            Some(ws) => observe(&ws, observation),
            None => return,
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One BLOCKING round: no pointer -> `Stopped`; otherwise one `status` call via [`local_phase`].
fn poll_once(state_dir: &Path, persistent: &mut sot_log::attach_client::supervisor_client::Persistent) -> Observation {
    if phase_for_missing_pointer(sot_log::supervisor::journal::pointer::pointer_path(state_dir).is_file()).is_some() {
        return Observation::Stopped;
    }
    match persistent.status() {
        Ok(report) => Observation::Phase {
            phase: local_phase(report.phase),
            supervisor: SupervisorIdentity { pid: report.pid, created: report.created },
            voyage: report.voyage.as_deref().and_then(|v| v.parse().ok()),
        },
        Err(sot_log::Error::VersionSkew) => Observation::Foreign,
        Err(_) => Observation::Failed,
    }
}

#[cfg(test)]
#[path = "observer_tests.rs"]
mod observer_tests;
