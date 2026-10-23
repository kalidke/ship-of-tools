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
    Observation::Phase { phase, supervisor, run: None }
}

#[test]
fn begin_supervisor_epoch_resets_phase_run_and_failures() {
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

/// RA blocker 1: `Terminal` is supervisor-scoped, not run-scoped -- applies regardless of phase/run.
#[test]
fn a_terminal_observation_with_no_run_applies_even_over_ended_no_respawn() {
    let a = identity(1, 100);
    let ws = seeded_capsule_row(a);
    let v1 = uuid::Uuid::from_u128(1);
    observe(&ws, Observation::Phase { phase: Phase::EndedNoRespawn, supervisor: a, run: Some(v1) });
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

/// `EndedNoRespawn` latches for its RUN within the epoch; a strictly newer run clears it.
#[test]
fn ended_no_respawn_latches_for_its_run() {
    let a = identity(1, 100);
    let ws = seeded_capsule_row(a);
    let v1 = uuid::Uuid::from_u128(1);
    let v2 = uuid::Uuid::from_u128(2);

    observe(&ws, Observation::Phase { phase: Phase::EndedNoRespawn, supervisor: a, run: Some(v1) });
    assert_eq!(ws.phase(), Phase::EndedNoRespawn);

    observe(&ws, Observation::Phase { phase: Phase::Ready, supervisor: a, run: Some(v1) });
    assert_eq!(ws.phase(), Phase::EndedNoRespawn, "the same run reported again must never clear the latch");

    observe(&ws, Observation::Phase { phase: Phase::Ready, supervisor: a, run: Some(v2) });
    assert_eq!(ws.phase(), Phase::Ready, "a strictly newer run supersedes the latch");
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

/// The watchdog's give-up, with its restart budget spent, latches a cell claimed by an earlier supervisor (the last
/// one died before it answered, so no observation can name it); the next start's epoch clears the latch.
#[test]
fn the_watchdogs_give_up_latches_a_claimed_cell_and_a_later_start_clears_it() {
    let a = identity(1, 100);
    let ws = seeded_capsule_row(a);
    observe(&ws, phase_obs(Phase::Ready, a));
    ws.latch_terminal();
    assert_eq!(ws.phase(), Phase::Terminal);
    assert_eq!(ws.current_supervisor(), Some(a));

    let b = identity(2, 200);
    ws.begin_supervisor_epoch(b);
    observe(&ws, phase_obs(Phase::Ready, b));
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

/// The watch record: a supervisor reached `ready` iff its watch said so, in the first reply or in a `row.phase` of the
/// same supervisor, never another's; a spawn clears it, and an event with no record names nobody.
#[test]
fn a_supervisor_was_told_ready_only_if_its_watch_said_so() {
    let a = identity(1, 100);
    let b = identity(2, 200);
    let ws = unclaimed_capsule_row();
    assert!(!ws.watch_said_ready(), "no watch, no record");
    ws.record_watch(a, false);
    assert!(!ws.watch_said_ready(), "a first reply that said starting");
    ws.note_watch_ready(b);
    assert!(!ws.watch_said_ready(), "another supervisor's event is not this record's");
    ws.note_watch_ready(a);
    assert!(ws.watch_said_ready(), "the same supervisor told ready");
    ws.clear_watched();
    assert!(!ws.watch_said_ready(), "a spawn clears the record");
    ws.note_watch_ready(a);
    assert!(!ws.watch_said_ready(), "an event with no record names nobody");
    ws.record_watch(b, true);
    assert!(ws.watch_said_ready(), "a first reply that said ready");
}

/// With no wake sent, a fence file that appears under a running observer is still found: the observer's own wait rule,
/// not a settle's wake, ends its wait on an absent fence.
#[tokio::test]
async fn a_fence_that_appears_without_a_wake_is_observed() {
    use crate::rows::run::observer::ensure_running;
    let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = ["XDG_STATE_HOME", "LOCALAPPDATA"].map(crate::paths::EnvGuard::capture);
    let state = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_STATE_HOME", state.path());
    std::env::set_var("LOCALAPPDATA", state.path());
    let registry = Workspaces::new();
    let ws = registry.insert(Workspace::from_label(
        "fence-appears",
        PathBuf::from("/tmp/sot-fence-appears"),
        false,
        "none".to_string(),
        String::new(),
        String::new(),
    ));
    ensure_running(&registry, &ws);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(ws.phase(), Phase::Stopped, "no fence file: never started");

    let root = sot_platform::host::state_dir::sot_state_dir().expect("a state root");
    let dir = crate::rows::spawn::state_root::state_dir_for(&root, &ws.workspace_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(sot_log::supervisor::journal::fence::supervisor_lock_path(&dir), b"").unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while ws.phase() != Phase::Unreachable && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(ws.phase(), Phase::Unreachable, "a fence file with nothing answering reads unreachable within 10 s");
}

/// A supervisor whose process ended is not answering, whatever its last report said: a rest does not outlive it. A
/// `terminal` mark does (the watchdog or the next start owns it), and a new supervisor's first report begins its epoch.
#[test]
fn a_supervisor_that_is_gone_reads_unreachable_over_a_rest_but_not_over_terminal() {
    let a = identity(1, 100);
    let ws = seeded_capsule_row(a);
    let r1 = uuid::Uuid::from_u128(1);
    observe(&ws, Observation::Phase { phase: Phase::EndedNoRespawn, supervisor: a, run: Some(r1) });
    assert_eq!(ws.phase(), Phase::EndedNoRespawn);
    observe(&ws, Observation::Gone);
    assert_eq!(ws.phase(), Phase::Unreachable, "the rest ended with its supervisor");

    let b = identity(2, 200);
    ws.begin_supervisor_epoch(b);
    observe(&ws, phase_obs(Phase::Terminal, b));
    observe(&ws, Observation::Gone);
    assert_eq!(ws.phase(), Phase::Terminal, "a terminal mark outlives its supervisor");
}
