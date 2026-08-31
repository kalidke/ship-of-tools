//! ADR 0041 "The probe": the classifier the `probe` module deliberately
//! ships without (see that module's own doc: "which observation MEANS
//! `READY`/`ABSENT`/`FOREIGN`/`PENDING`/... is the classifier's own call
//! ... and is deliberately absent here"). One episode, one monotonic
//! deadline, evaluated as a typed transition table in TWO stages: the
//! owned child (if this episode spawned one) is resolved FIRST AND
//! COMPLETELY, including its own challenge, before the episode deadline
//! is ever consulted — "every terminal answer [in Stage A] has a child
//! to dispose of," so nothing there may be cut short by a timer the way
//! Stage B's `WEDGED` legitimately is.
//!
//! This module is generic over [`ProbeOps`] exactly like `probe.rs`
//! itself, so the SAME transition logic is driven scripted-only by a
//! model test (no real OS object touched) and for real by
//! [`crate::probe::RealProbeOps`] — see `tests/supervisor_win.rs`.
//!
//! Cadence and deadlines are the CALLER's job (this module invents no
//! constant of its own — see ADR 0041's own "the numbers, pinned here so
//! no implementation invents them"): [`probe_owned_spawn`] takes
//! `readiness_cutoff` (Stage A's own boundary) and `kill_wait_bound`
//! (the post-kill wait); [`probe_adopt_only`] and Stage B's own retry
//! loop inside [`probe_owned_spawn`] take `episode_deadline` (B0) and
//! `attempt_interval` (the 500ms spacing between attempts). The
//! per-attempt challenge deadline is computed HERE as `min(now + 2s,
//! stage_boundary)` — "2s, clamped to the episode's remaining wall
//! time" — because that clamp is part of the transition logic itself,
//! not a policy choice a caller could reasonably vary.

#![cfg(windows)]

use crate::challenge::ChallengeOutcome;
use crate::probe::{ConnectOutcome, FenceProbe, ProbeOps, SpawnOutcome, WaitOutcome};
use std::path::Path;
use std::time::{Duration, Instant};

/// The fixed 2s challenge budget every probe attempt clamps to the
/// remaining wall time of whichever stage boundary is active (ADR 0041
/// bounds table: "challenge | 2s, clamped to the episode's remaining
/// wall time").
const CHALLENGE_BUDGET: Duration = Duration::from_secs(2);

fn clamped_challenge_deadline(now: Instant, stage_boundary: Instant) -> Instant {
    (now + CHALLENGE_BUDGET).min(stage_boundary)
}

/// What one probe episode concluded — the merged Stage A/B outcome a
/// caller (the supervisor's own spawn/adopt decision) acts on. `Process`
/// is [`ProbeOps::Process`] — a live, retained identity for `Ready`/
/// `Adopted`, the two rows that carry one (ADR 0041: "READY and ADOPTED
/// are the same evidence from different provenance").
#[derive(Debug)]
pub enum ProbeOutcome<Process> {
    /// A4: the owned child is alive, within its readiness cutoff, and
    /// its challenge proved it.
    Ready(Process),
    /// B1: no owned spawn attempt this episode; a proven, already-live
    /// server.
    Adopted(Process),
    /// B2/B3/B4/B6 (Stage B only — see this module's own doc for why
    /// Stage A folds an equivalent observation into `Pending` instead):
    /// a well-formed wrong answer, or an access-denied connect. Never
    /// retried as if it might still be legitimate.
    Foreign,
    /// B7: no owned child, connect `FILE_NOT_FOUND`, writer fence FREE.
    Absent,
    /// A2: the owned child exited before ever proving anything. Carries
    /// no exit code — `WaitOutcome::Exited` has none to give; the
    /// diagnostic detail (the sealed `producer_dead` record, or the
    /// store never having opened at all) lives elsewhere, per the ADR's
    /// own "which way a leg was unstable is DIAGNOSTIC ... never a
    /// second counter."
    LegEnded,
    /// A1: `CreateProcess` itself failed.
    SpawnFailed(std::io::Error),
    /// A3's KILL+WAIT succeeded (the child was killed and its exit
    /// confirmed within `kill_wait_bound`): counts one unstable leg: the
    /// caller may re-enter (spawn again, subject to the anti-flap
    /// counter).
    KilledAfterTimeout,
    /// A3's kill call failed, or the post-kill wait did not confirm exit
    /// within its own bound — TERMINAL for the whole supervisor (ADR
    /// 0041: "a child able to bind later is exactly what this row
    /// prevents"), never merely counted and retried.
    KillOrWaitFailed(std::io::Error),
    /// B0: the episode deadline expired with nothing owned to clean up.
    Wedged,
}

/// Stage A: run one probe episode over an OWNED spawn attempt (A1-A5).
/// `command` is what to run; the caller supplies it fully configured
/// (argv, env — including any parent-lease fields — cwd), this function
/// has no opinion on it. `readiness_cutoff` bounds Stage A's own poll
/// loop (measured from spawn, per the ADR); `kill_wait_bound` is A3's
/// post-kill wait (10s today); `attempt_interval` is the polling cadence
/// (500ms today) between challenge attempts while the child is alive but
/// unproven (A5).
pub fn probe_owned_spawn<O: ProbeOps>(
    ops: &O,
    command: &mut std::process::Command,
    voyage_id: &str,
    readiness_cutoff: Instant,
    kill_wait_bound: Duration,
    attempt_interval: Duration,
) -> ProbeOutcome<O::Process> {
    let child = match ops.spawn(command) {
        SpawnOutcome::Failed(e) => return ProbeOutcome::SpawnFailed(e), // A1
        SpawnOutcome::Spawned(child) => child,
    };

    loop {
        // A2: has the child already exited?
        match ops.wait_child(&child, Duration::ZERO) {
            WaitOutcome::Exited => return ProbeOutcome::LegEnded,
            WaitOutcome::WaitFailed => {
                return kill_and_wait(ops, &child, kill_wait_bound); // A3 (wait_failed half)
            }
            WaitOutcome::StillRunning => {}
        }

        // A3, readiness-cutoff half: expired before ever proving the
        // child, regardless of what the NEXT connect/challenge attempt
        // below would have found.
        if ops.now() >= readiness_cutoff {
            return kill_and_wait(ops, &child, kill_wait_bound);
        }

        // A4/A5: alive, within cutoff -> attempt the challenge on its
        // own pipe. A connect failure of any kind is folded into A5
        // (PENDING) exactly like an Undetermined/Foreign challenge
        // reply — see this module's own doc for why Stage A, unlike
        // Stage B, gives a foreign-looking answer no separate terminal
        // row: the object IS the process this episode itself created.
        match ops.connect(voyage_id) {
            ConnectOutcome::Connected(conn) => {
                let deadline = clamped_challenge_deadline(ops.now(), readiness_cutoff);
                if let ChallengeOutcome::Proven(process) = ops.challenge(&conn, deadline) {
                    return ProbeOutcome::Ready(process); // A4
                }
                // A5 (Foreign or Undetermined): fall through to the poll
                // sleep and try again.
            }
            ConnectOutcome::PipeBusy
            | ConnectOutcome::FileNotFound
            | ConnectOutcome::AccessDenied
            | ConnectOutcome::OtherIo(_) => {} // A5
        }

        sleep_until_next_attempt(ops, attempt_interval, readiness_cutoff);
    }
}

fn kill_and_wait<O: ProbeOps>(
    ops: &O,
    child: &O::SpawnedChild,
    kill_wait_bound: Duration,
) -> ProbeOutcome<O::Process> {
    if let Err(e) = ops.kill_child(child) {
        return ProbeOutcome::KillOrWaitFailed(e);
    }
    match ops.wait_child(child, kill_wait_bound) {
        WaitOutcome::Exited => ProbeOutcome::KilledAfterTimeout,
        WaitOutcome::StillRunning => ProbeOutcome::KillOrWaitFailed(std::io::Error::other(
            "child did not exit within the post-kill wait bound",
        )),
        WaitOutcome::WaitFailed => {
            ProbeOutcome::KillOrWaitFailed(std::io::Error::other("post-kill wait failed"))
        }
    }
}

/// Stage B: run one probe episode with NO owned spawn attempt (B0-B9) —
/// the classifier's own entry point for an adopt-only probe (a
/// `--start`/`--resume` finding a live capsule to adopt, or a periodic
/// liveness re-check). `episode_deadline` is B0's own bound;
/// `attempt_interval` is the polling cadence between connect attempts.
pub fn probe_adopt_only<O: ProbeOps>(
    ops: &O,
    voyage_id: &str,
    voyage_root: &Path,
    episode_deadline: Instant,
    attempt_interval: Duration,
) -> ProbeOutcome<O::Process> {
    loop {
        if ops.now() >= episode_deadline {
            return ProbeOutcome::Wedged; // B0
        }
        match ops.connect(voyage_id) {
            ConnectOutcome::Connected(conn) => {
                let deadline = clamped_challenge_deadline(ops.now(), episode_deadline);
                match ops.challenge(&conn, deadline) {
                    ChallengeOutcome::Proven(process) => return ProbeOutcome::Adopted(process), // B1
                    ChallengeOutcome::Foreign => return ProbeOutcome::Foreign, // B2/B3/B4
                    ChallengeOutcome::Undetermined => {} // B5 -> retry
                }
            }
            ConnectOutcome::AccessDenied => return ProbeOutcome::Foreign, // B6
            ConnectOutcome::FileNotFound => {
                match ops.writer_fence_probe(voyage_root) {
                    FenceProbe::Free => return ProbeOutcome::Absent,     // B7
                    FenceProbe::Held | FenceProbe::Error(_) => {}        // B8 -> retry
                }
            }
            ConnectOutcome::PipeBusy | ConnectOutcome::OtherIo(_) => {} // B9 -> retry
        }
        sleep_until_next_attempt(ops, attempt_interval, episode_deadline);
    }
}

/// A real sleep, bounded so the LAST attempt before a deadline is never
/// delayed past it for no reason — `ProbeOps::now()` is the injected
/// clock a scripted test drives without ever calling this at all (see
/// its own doc: "only ever advances when the test tells it to"), so this
/// function's real `std::thread::sleep` is dead code under
/// `ScriptedProbeOps` (which never lets `now()` reach a value this
/// function would need to wait for) and live code under `RealProbeOps`.
fn sleep_until_next_attempt<O: ProbeOps>(ops: &O, attempt_interval: Duration, boundary: Instant) {
    let now = ops.now();
    if now >= boundary {
        return;
    }
    std::thread::sleep(attempt_interval.min(boundary - now));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{DummyConn, DummyProcess, DummySpawnedChild, ScriptedProbeOps};
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
        assert!(matches!(outcome, ProbeOutcome::LegEnded));
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
        let readiness = ops.now() + Duration::from_secs(60);
        let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
        assert!(matches!(outcome, ProbeOutcome::Ready(_)));
        assert!(ops.all_exhausted());
    }

    #[test]
    fn a5_pending_then_a4_ready() {
        let ops = ScriptedProbeOps::new();
        ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
        // First iteration: still running, but not yet bound.
        ops.push_wait_child(WaitOutcome::StillRunning);
        ops.push_connect(ConnectOutcome::FileNotFound);
        // Second iteration: bound and proven.
        ops.push_wait_child(WaitOutcome::StillRunning);
        ops.push_connect(ConnectOutcome::Connected(DummyConn));
        ops.push_challenge(ChallengeOutcome::Proven(DummyProcess));
        let readiness = ops.now() + Duration::from_secs(60);
        let outcome = probe_owned_spawn(&ops, &mut unused_cmd(), "voy", readiness, KILL_WAIT, ATTEMPT);
        assert!(matches!(outcome, ProbeOutcome::Ready(_)));
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
        ops.push_writer_fence_probe(crate::probe::FenceProbe::Free);
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
        ops.push_writer_fence_probe(crate::probe::FenceProbe::Held);
        ops.push_connect(ConnectOutcome::FileNotFound);
        ops.push_writer_fence_probe(crate::probe::FenceProbe::Free);
        let deadline = ops.now() + Duration::from_secs(60);
        let dir = tempfile::tempdir().unwrap();
        let outcome = probe_adopt_only(&ops, "voy", dir.path(), deadline, ATTEMPT);
        assert!(matches!(outcome, ProbeOutcome::Absent));
        assert!(ops.all_exhausted());
    }

    #[test]
    fn b8_fence_probe_error_is_pending() {
        let ops = ScriptedProbeOps::new();
        ops.push_connect(ConnectOutcome::FileNotFound);
        ops.push_writer_fence_probe(crate::probe::FenceProbe::Error(std::io::Error::other("x")));
        // Deadline hits before the retry -- proves this folded to
        // PENDING (kept polling) rather than returning immediately.
        let deadline = ops.now() + ATTEMPT;
        let dir = tempfile::tempdir().unwrap();
        let outcome = probe_adopt_only(&ops, "voy", dir.path(), deadline, ATTEMPT);
        assert!(matches!(outcome, ProbeOutcome::Wedged));
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
}
