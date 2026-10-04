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

/// Idempotent: ensures a lifecycle-observer task runs for `ws`, never from `Workspaces::insert`.
pub fn ensure_running(workspaces: &Workspaces, ws: &Arc<Workspace>) {
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
    let ws_for_task = ws.clone();
    let workspaces_for_task = workspaces.clone();
    let workspace_id = ws.workspace_id.clone();
    let handle = tokio::spawn(run(ws_for_task, state_dir, persistent, workspaces_for_task));
    workspaces.install_observer(&workspace_id, handle, cancel);
}

/// Immediate first round, then every `POLL_INTERVAL`; exits once its row is gone.
async fn run(
    ws: Arc<Workspace>,
    state_dir: PathBuf,
    mut persistent: sot_log::attach_client::supervisor_client::Persistent,
    workspaces: Workspaces,
) {
    loop {
        if workspaces.resolve(Some(&ws.workspace_id)).is_none() {
            return;
        }
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
        observe(&ws, observation);
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
mod observer_tests {
    use super::observe;
    use crate::rows::workspace::{Observation, Phase, SupervisorIdentity};
    use crate::rows::{Workspace, Workspaces};
    use std::path::PathBuf;

    /// A registered capsule row with its epoch begun at `supervisor` (RA).
    fn seeded_capsule_row(supervisor: SupervisorIdentity) -> std::sync::Arc<Workspace> {
        let ws = unclaimed_capsule_row();
        ws.begin_supervisor_epoch(supervisor);
        ws
    }

    /// The same row with NO epoch yet -- the empty cell the ruling's
    /// addition (a) is about: a row the daemon has spawned for but whose
    /// lane has not yet answered, or one only ever read as stopped.
    fn unclaimed_capsule_row() -> std::sync::Arc<Workspace> {
        let reg = Workspaces::new();
        let mut ws = Workspace::from_label(
            "observer-test",
            PathBuf::from("/tmp/sot-observer-test"),
            false,
            "none".to_string(),
            String::new(),
            String::new(),
        );
        ws.runtime = "capsule".to_string();
        reg.insert(ws)
    }

    fn identity(pid: u32, created: u64) -> SupervisorIdentity {
        SupervisorIdentity { pid, created }
    }

    fn phase_obs(phase: Phase, supervisor: SupervisorIdentity) -> Observation {
        Observation::Phase { phase, supervisor, voyage: None }
    }

    #[test]
    fn begin_supervisor_epoch_resets_phase_voyage_and_failures() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        assert_eq!(ws.phase(), Phase::Stopped, "a fresh epoch starts Stopped");
        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Ready);

        ws.begin_supervisor_epoch(identity(2, 200));
        assert_eq!(ws.phase(), Phase::Stopped, "a new epoch resets phase");
    }

    /// RA blocker 2: judged by identity EQUALITY, never by timestamp -- a same-tick stranger is never "newer."
    #[test]
    fn a_different_supervisor_is_rejected_even_with_an_equal_or_newer_timestamp() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Ready);

        observe(&ws, phase_obs(Phase::Ending, identity(2, 100)));
        assert_eq!(ws.phase(), Phase::Ready, "a same-tick stranger must never be accepted");

        observe(&ws, phase_obs(Phase::Ending, identity(3, 500)));
        assert_eq!(ws.phase(), Phase::Ready, "a newer-timestamped stranger must never be accepted");
    }

    #[test]
    fn unreachable_needs_two_consecutive_failed_rounds() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Ready);

        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Ready, "a single failed round must not move the phase");

        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Unreachable);

        observe(&ws, phase_obs(Phase::Ready, a));
        observe(&ws, Observation::Failed);
        observe(&ws, phase_obs(Phase::Ready, a));
        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Ready, "a success between two failures resets the count");
    }

    /// RA blocker 1: `Terminal` is supervisor-scoped, not voyage-scoped -- applies regardless of phase/voyage.
    #[test]
    fn a_terminal_observation_with_no_voyage_applies_even_over_ended_no_respawn() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        let v1 = uuid::Uuid::from_u128(1);
        observe(&ws, Observation::Phase { phase: Phase::EndedNoRespawn, supervisor: a, voyage: Some(v1) });
        assert_eq!(ws.phase(), Phase::EndedNoRespawn);

        observe(&ws, phase_obs(Phase::Terminal, a));
        assert_eq!(ws.phase(), Phase::Terminal, "Terminal must not be hidden behind an EndedNoRespawn latch");
    }

    #[test]
    fn a_terminal_observation_latches_and_only_a_fresh_epoch_clears_it() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        observe(&ws, phase_obs(Phase::Ready, a));
        observe(&ws, phase_obs(Phase::Terminal, a));
        assert_eq!(ws.phase(), Phase::Terminal);

        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Terminal, "terminal latches within its own epoch");
        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Terminal, "terminal latches across a failed round too");

        // Only a fresh epoch (a new spawn or adoption) clears it.
        let b = identity(2, 200);
        ws.begin_supervisor_epoch(b);
        observe(&ws, phase_obs(Phase::Ready, b));
        assert_eq!(ws.phase(), Phase::Ready);
    }

    /// `EndedNoRespawn` latches for its VOYAGE within the epoch; a strictly newer voyage clears it.
    #[test]
    fn ended_no_respawn_latches_for_its_voyage() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        let v1 = uuid::Uuid::from_u128(1);
        let v2 = uuid::Uuid::from_u128(2);

        observe(&ws, Observation::Phase { phase: Phase::EndedNoRespawn, supervisor: a, voyage: Some(v1) });
        assert_eq!(ws.phase(), Phase::EndedNoRespawn);

        observe(&ws, Observation::Phase { phase: Phase::Ready, supervisor: a, voyage: Some(v1) });
        assert_eq!(ws.phase(), Phase::EndedNoRespawn, "the same voyage reported again must never clear the latch");

        observe(&ws, Observation::Phase { phase: Phase::Ready, supervisor: a, voyage: Some(v2) });
        assert_eq!(ws.phase(), Phase::Ready, "a strictly newer voyage supersedes the latch");
    }

    /// Addition (a): an empty cell takes the FIRST prover -- and only
    /// the first. The rejection half is the same rule RA blocker 2
    /// asserts, re-run against the adopting branch to prove the adoption
    /// is scoped to `None` and never widens who may claim a claimed row.
    #[test]
    fn an_empty_cell_adopts_its_first_prover_and_then_judges_strangers() {
        let a = identity(1, 100);
        let ws = unclaimed_capsule_row();
        assert_eq!(ws.current_supervisor(), None, "an unclaimed row has no epoch");

        // Two failed rounds first: an empty cell that is already
        // `Unreachable` must still adopt, and adoption must reset the
        // failure count, exactly as `begin_supervisor_epoch` does.
        observe(&ws, Observation::Failed);
        observe(&ws, Observation::Failed);
        assert_eq!(ws.phase(), Phase::Unreachable);

        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.current_supervisor(), Some(a), "the first prover becomes the epoch");
        assert_eq!(ws.phase(), Phase::Ready);

        observe(&ws, phase_obs(Phase::Ending, identity(2, 100)));
        assert_eq!(ws.phase(), Phase::Ready, "a claimed cell still rejects a stranger");
        assert_eq!(ws.current_supervisor(), Some(a));
    }

    /// Addition (b): a bootstrap-failing leg latches its row `terminal`
    /// without an identity, and a later genuine authority adopts and
    /// clears it -- a row whose lane answers is not terminal.
    #[test]
    fn terminal_unclaimed_latches_an_empty_cell_and_a_later_authority_clears_it() {
        let ws = unclaimed_capsule_row();
        assert!(ws.apply_phase_observation(Observation::TerminalUnclaimed));
        assert_eq!(ws.phase(), Phase::Terminal);
        assert_eq!(ws.current_supervisor(), None, "the mark leaves the cell claimable");

        observe(&ws, Observation::Stopped);
        assert_eq!(ws.phase(), Phase::Terminal, "the latch holds against a plain stopped read");

        let a = identity(7, 700);
        observe(&ws, phase_obs(Phase::Ready, a));
        assert_eq!(ws.phase(), Phase::Ready, "an authority that actually answers clears the latch");
        assert_eq!(ws.current_supervisor(), Some(a));
    }

    /// The other half of (b): a cell claimed at any point in this
    /// daemon's life is closed to it, so a stale watchdog can never
    /// latch a row it no longer owns.
    #[test]
    fn terminal_unclaimed_is_refused_by_a_claimed_cell() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        observe(&ws, phase_obs(Phase::Ready, a));

        assert!(!ws.apply_phase_observation(Observation::TerminalUnclaimed));
        assert_eq!(ws.phase(), Phase::Ready);
    }

    /// `activation_error` is retained until the next attempt; orthogonal to phase.
    #[test]
    fn activation_error_is_independent_of_phase_and_clears_on_the_next_attempt() {
        let a = identity(1, 100);
        let ws = seeded_capsule_row(a);
        assert_eq!(ws.activation_error(), None);

        ws.set_activation_error(Some("capsule spawn failed: boom".to_string()));
        assert_eq!(ws.phase(), Phase::Stopped, "an activation error must never move phase on its own");

        observe(&ws, phase_obs(Phase::Terminal, a));
        assert_eq!(ws.phase(), Phase::Terminal);
        assert_eq!(
            ws.activation_error(),
            Some("capsule spawn failed: boom".to_string()),
            "a phase observation must never clear a pending activation_error -- only the next attempt does"
        );

        ws.set_activation_error(None);
        assert_eq!(ws.activation_error(), None);
    }
}
