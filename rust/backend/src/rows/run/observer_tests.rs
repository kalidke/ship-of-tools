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

/// The same row with NO epoch yet -- the empty cell the supervisor-epoch
/// rule's addition (a) is about: a row the daemon has spawned for but whose
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
