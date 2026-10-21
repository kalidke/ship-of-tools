use super::*;
use crate::supervisor::probe::{DummyConn, DummyProcess, DummySpawnedChild, ScriptedProbeOps};
use std::time::Duration;

const ATTEMPT: Duration = Duration::from_millis(1);
const KILL_WAIT: Duration = Duration::from_millis(1);

fn unused_cmd() -> std::process::Command {
    std::process::Command::new("unused")
}

#[test]
fn a1_spawn_failed() {
    let ops = ScriptedProbeOps::new();
    ops.push_spawn(SpawnOutcome::Failed(std::io::Error::other("boom")));
    let readiness = ops.now() + Duration::from_secs(60);
    let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::SpawnFailed(_)));
    assert!(ops.all_exhausted());
}

#[test]
fn a2_leg_ended() {
    let ops = ScriptedProbeOps::new();
    ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
    ops.push_wait_child(WaitOutcome::Exited);
    let readiness = ops.now() + Duration::from_secs(60);
    let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::LegEnded(None)));
    assert!(ops.all_exhausted());
}

#[test]
fn a3_wait_failed_then_kill_and_wait_succeeds() {
    let ops = ScriptedProbeOps::new();
    ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
    ops.push_wait_child(WaitOutcome::WaitFailed);
    ops.push_kill_child(Ok(()));
    ops.push_wait_child(WaitOutcome::Exited);
    let readiness = ops.now() + Duration::from_secs(60);
    let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::KilledAfterTimeout));
    assert!(ops.all_exhausted());
}

#[test]
fn a3_readiness_cutoff_expired_then_kill_fails() {
    let ops = ScriptedProbeOps::new();
    ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
    ops.push_wait_child(WaitOutcome::StillRunning);
    let readiness = ops.now(); // already at/over the cutoff
    ops.push_kill_child(Err(std::io::Error::other("kill failed")));
    let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::KillOrWaitFailed(_)));
    assert!(ops.all_exhausted());
}

#[test]
fn a3_kill_succeeds_but_wait_never_confirms_exit() {
    let ops = ScriptedProbeOps::new();
    ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
    ops.push_wait_child(WaitOutcome::StillRunning);
    let readiness = ops.now();
    ops.push_kill_child(Ok(()));
    ops.push_wait_child(WaitOutcome::StillRunning);
    let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::KillOrWaitFailed(_)));
    assert!(ops.all_exhausted());
}

#[test]
fn a4_ready() {
    let ops = ScriptedProbeOps::new();
    ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
    ops.push_wait_child(WaitOutcome::StillRunning);
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    ops.push_challenge(ChallengeOutcome::Proven(DummyProcess));
    // The challenged process's identity matches the child this
    // episode itself spawned -- A4's own identity check passes.
    ops.push_spawned_identity(Ok((111, 222)));
    ops.push_proven_identity((111, 222));
    let readiness = ops.now() + Duration::from_secs(60);
    let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::Ready(_)));
    assert!(ops.all_exhausted());
}

/// A2 hands back how the owned child ended, and A4 hands the supervisor
/// the child itself (exactly one `retain_owned_child`), so the exit
/// status of an owned leg reaches the lifecycle.
#[test]
fn stage_a_hands_back_the_exit_status_and_the_owned_child() {
    let ops = ScriptedProbeOps::new();
    ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
    ops.push_wait_child(WaitOutcome::Exited);
    ops.set_child_exit_status(Some(ExitStatus::Code(71)));
    let readiness = ops.now() + Duration::from_secs(60);
    let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::LegEnded(Some(ExitStatus::Code(71)))), "{outcome:?}");
    assert_eq!(ops.retained_children(), 0, "an ended child is not retained");

    let ops = ScriptedProbeOps::new();
    ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
    ops.push_wait_child(WaitOutcome::StillRunning);
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    ops.push_challenge(ChallengeOutcome::Proven(DummyProcess));
    ops.push_spawned_identity(Ok((111, 222)));
    ops.push_proven_identity((111, 222));
    let readiness = ops.now() + Duration::from_secs(60);
    let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::Ready(_)));
    assert_eq!(ops.retained_children(), 1, "Ready hands back the owned child exactly once");
    assert!(ops.all_exhausted());
}

#[test]
fn a5_pending_then_a4_ready() {
    let ops = ScriptedProbeOps::new();
    ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
    // First iteration: still running, but not yet bound.
    ops.push_wait_child(WaitOutcome::StillRunning);
    ops.push_connect(ConnectOutcome::FileNotFound);
    // Second iteration: bound and proven, and the identity matches.
    ops.push_wait_child(WaitOutcome::StillRunning);
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    ops.push_challenge(ChallengeOutcome::Proven(DummyProcess));
    ops.push_spawned_identity(Ok((111, 222)));
    ops.push_proven_identity((111, 222));
    let readiness = ops.now() + Duration::from_secs(60);
    let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::Ready(_)));
    assert!(ops.all_exhausted());
}

/// A4's own identity check: a
/// challenge can prove SOME process without that process being the
/// one this episode itself spawned — a stale, orphaned capsule from a
/// prior crash, still bound under the same voyage id, answering
/// before this new child's own pipe came up. A mismatch is `Foreign`,
/// and the now-untracked spawned child is terminated rather than left
/// running.
#[test]
fn a4_identity_mismatch_is_foreign_and_kills_the_untracked_child() {
    let ops = ScriptedProbeOps::new();
    ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
    ops.push_wait_child(WaitOutcome::StillRunning);
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    ops.push_challenge(ChallengeOutcome::Proven(DummyProcess));
    ops.push_spawned_identity(Ok((111, 222)));
    ops.push_proven_identity((999, 888)); // a DIFFERENT process answered
    // The untracked child must be terminated AND its death confirmed
    // (kill alone is not enough).
    ops.push_kill_child(Ok(()));
    ops.push_wait_child(WaitOutcome::Exited);
    let readiness = ops.now() + Duration::from_secs(60);
    let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::Foreign));
    assert!(ops.all_exhausted());
}

/// A kill/wait failure during the
/// A4 identity-mismatch cleanup is AT LEAST as severe as A3's own
/// KILL+WAIT row (an unconfirmed kill leaves an untracked process
/// that might still be alive) — it escalates to `KillOrWaitFailed`,
/// never silently reported as the milder `Foreign`.
#[test]
fn a4_identity_mismatch_kill_failure_escalates_to_kill_or_wait_failed() {
    let ops = ScriptedProbeOps::new();
    ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
    ops.push_wait_child(WaitOutcome::StillRunning);
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    ops.push_challenge(ChallengeOutcome::Proven(DummyProcess));
    ops.push_spawned_identity(Ok((111, 222)));
    ops.push_proven_identity((999, 888));
    ops.push_kill_child(Err(std::io::Error::other("TerminateProcess failed")));
    let readiness = ops.now() + Duration::from_secs(60);
    let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::KillOrWaitFailed(_)), "expected KillOrWaitFailed, got {outcome:?}");
    assert!(ops.all_exhausted());
}

/// The child's own identity could not even be READ (an OS call
/// failed) -- fail closed exactly like a proven mismatch: `Foreign`,
/// and the untracked child is still terminated.
#[test]
fn a4_identity_unreadable_is_foreign_and_kills_the_untracked_child() {
    let ops = ScriptedProbeOps::new();
    ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
    ops.push_wait_child(WaitOutcome::StillRunning);
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    ops.push_challenge(ChallengeOutcome::Proven(DummyProcess));
    ops.push_spawned_identity(Err(std::io::Error::other("GetProcessId failed")));
    ops.push_kill_child(Ok(()));
    ops.push_wait_child(WaitOutcome::Exited);
    let readiness = ops.now() + Duration::from_secs(60);
    let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::Foreign));
    assert!(ops.all_exhausted());
}

#[test]
fn a5_foreign_challenge_on_owned_child_is_still_pending_not_terminal() {
    let ops = ScriptedProbeOps::new();
    ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
    ops.push_wait_child(WaitOutcome::StillRunning);
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    ops.push_challenge(ChallengeOutcome::Foreign);
    // Cutoff hits on the very next observation -- proves the earlier
    // Foreign was folded to PENDING and polling continued, not
    // returned as this module's own `Foreign` outcome.
    ops.push_wait_child(WaitOutcome::WaitFailed);
    ops.push_kill_child(Ok(()));
    ops.push_wait_child(WaitOutcome::Exited);
    let readiness = ops.now() + Duration::from_secs(60);
    let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::KilledAfterTimeout));
    assert!(ops.all_exhausted());
}

#[test]
fn b0_wedged() {
    let ops = ScriptedProbeOps::new();
    let deadline = ops.now(); // already expired
    let dir = tempfile::tempdir().unwrap();
    let outcome = probe_adopt_only(&ops, "voy", dir.path(), deadline, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::Wedged));
    assert!(ops.all_exhausted());
}

#[test]
fn b1_adopted() {
    let ops = ScriptedProbeOps::new();
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    ops.push_challenge(ChallengeOutcome::Proven(DummyProcess));
    let deadline = ops.now() + Duration::from_secs(60);
    let dir = tempfile::tempdir().unwrap();
    let outcome = probe_adopt_only(&ops, "voy", dir.path(), deadline, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::Adopted(_)));
    assert!(ops.all_exhausted());
}

#[test]
fn b2_b3_b4_foreign_challenge() {
    let ops = ScriptedProbeOps::new();
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    ops.push_challenge(ChallengeOutcome::Foreign);
    let deadline = ops.now() + Duration::from_secs(60);
    let dir = tempfile::tempdir().unwrap();
    let outcome = probe_adopt_only(&ops, "voy", dir.path(), deadline, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::Foreign));
    assert!(ops.all_exhausted());
}

#[test]
fn b5_undetermined_then_b1_adopted() {
    let ops = ScriptedProbeOps::new();
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    ops.push_challenge(ChallengeOutcome::Undetermined);
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    ops.push_challenge(ChallengeOutcome::Proven(DummyProcess));
    let deadline = ops.now() + Duration::from_secs(60);
    let dir = tempfile::tempdir().unwrap();
    let outcome = probe_adopt_only(&ops, "voy", dir.path(), deadline, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::Adopted(_)));
    assert!(ops.all_exhausted());
}

#[test]
fn b6_access_denied_is_foreign() {
    let ops = ScriptedProbeOps::new();
    ops.push_connect(ConnectOutcome::AccessDenied);
    let deadline = ops.now() + Duration::from_secs(60);
    let dir = tempfile::tempdir().unwrap();
    let outcome = probe_adopt_only(&ops, "voy", dir.path(), deadline, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::Foreign));
    assert!(ops.all_exhausted());
}

#[test]
fn b7_file_not_found_fence_free_is_absent() {
    let ops = ScriptedProbeOps::new();
    ops.push_connect(ConnectOutcome::FileNotFound);
    ops.push_writer_fence_probe(crate::supervisor::probe::FenceProbe::Free);
    let deadline = ops.now() + Duration::from_secs(60);
    let dir = tempfile::tempdir().unwrap();
    let outcome = probe_adopt_only(&ops, "voy", dir.path(), deadline, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::Absent));
    assert!(ops.all_exhausted());
}

#[test]
fn b8_file_not_found_fence_held_is_pending_then_b7_absent() {
    let ops = ScriptedProbeOps::new();
    ops.push_connect(ConnectOutcome::FileNotFound);
    ops.push_writer_fence_probe(crate::supervisor::probe::FenceProbe::Held);
    ops.push_connect(ConnectOutcome::FileNotFound);
    ops.push_writer_fence_probe(crate::supervisor::probe::FenceProbe::Free);
    let deadline = ops.now() + Duration::from_secs(60);
    let dir = tempfile::tempdir().unwrap();
    let outcome = probe_adopt_only(&ops, "voy", dir.path(), deadline, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::Absent));
    assert!(ops.all_exhausted());
}

#[test]
fn b8_fence_probe_error_is_pending() {
    // `ScriptedProbeOps`'s injected clock only ever advances when the
    // test tells it to (see its own doc) -- `sleep_until_next_attempt`'s
    // REAL sleep between attempts never moves it, so a deadline set to
    // "just past `now`" can never actually be observed as expired on
    // a later loop iteration: the retry would run forever, consuming
    // scripted ops past the two pushed below and panicking on the
    // exhausted queue rather than ever reaching a deadline check that
    // trips. Proving B8 folds to PENDING (a retry happens at all,
    // never an immediate terminal return) therefore needs a SECOND
    // scripted attempt that reaches a real terminal outcome -- if B8
    // were wrongly treated as terminal, this second pair would never
    // be consumed and `all_exhausted` would catch it.
    let ops = ScriptedProbeOps::new();
    ops.push_connect(ConnectOutcome::FileNotFound);
    ops.push_writer_fence_probe(crate::supervisor::probe::FenceProbe::Error(std::io::Error::other("x")));
    ops.push_connect(ConnectOutcome::FileNotFound);
    ops.push_writer_fence_probe(crate::supervisor::probe::FenceProbe::Free);
    let deadline = ops.now() + Duration::from_secs(60); // never hit -- the retry itself is the assertion
    let dir = tempfile::tempdir().unwrap();
    let outcome = probe_adopt_only(&ops, "voy", dir.path(), deadline, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::Absent));
    assert!(ops.all_exhausted());
}

#[test]
fn b9_pipe_busy_and_other_io_are_pending() {
    let ops = ScriptedProbeOps::new();
    ops.push_connect(ConnectOutcome::PipeBusy);
    ops.push_connect(ConnectOutcome::OtherIo(std::io::Error::other("x")));
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    ops.push_challenge(ChallengeOutcome::Proven(DummyProcess));
    let deadline = ops.now() + Duration::from_secs(60);
    let dir = tempfile::tempdir().unwrap();
    let outcome = probe_adopt_only(&ops, "voy", dir.path(), deadline, ATTEMPT);
    assert!(matches!(outcome, ProbeOutcome::Adopted(_)));
    assert!(ops.all_exhausted());
}
