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
//! This module is generic over [`ProbeOps`] exactly like `supervisor/probe/`
//! itself, so the SAME transition logic is driven scripted-only by a
//! model test (no real OS object touched) and for real by
//! `crate::supervisor::probe::win::RealProbeOps` (Windows) / `crate::supervisor::probe::unix::RealProbeOps` (Linux) — see `tests/supervisor/`.
//! Portable: this module makes no OS call of its own —
//! everything platform-specific is behind [`ProbeOps`] — so its unit
//! tests (against `ScriptedProbeOps`) now run on every CI platform, not
//! merely on Windows.
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

use crate::capsule::producer::ExitStatus;
use sot_platform::identity::challenge::ChallengeOutcome;
use crate::supervisor::probe::{ConnectOutcome, FenceProbe, ProbeOps, SpawnOutcome, WaitOutcome};
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
    /// B2/B3/B4/B6, or A4's own identity mismatch: a well-formed wrong answer, an access-denied connect,
    /// or a challenge that proved SOME process but not the one this
    /// episode's own spawn produced — a stale, orphaned capsule from a
    /// prior crash, still bound under the same voyage id, answering
    /// before this new child's own pipe came up. Never retried as if it
    /// might still be legitimate; the A4 row also terminates the
    /// now-untracked spawned child before returning this (see
    /// `probe_owned_spawn`'s own A4 arm) rather than leaving an unproven
    /// process running with nothing left to supervise it.
    Foreign,
    /// B7: no owned child, connect `FILE_NOT_FOUND`, writer fence FREE.
    Absent,
    /// A2: the owned child exited before ever proving anything. Carries how
    /// it ended when the OS gave it (`Some(Code)` or `Some(Signal)`, read
    /// from the owned child), `None` when unknown; the diagnostic detail
    /// (the sealed `producer_dead` record, or the store never having
    /// opened at all) lives elsewhere, per the ADR's own "which way a leg
    /// was unstable is DIAGNOSTIC ... never a second counter."
    LegEnded(Option<ExitStatus>),
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
            WaitOutcome::Exited => return ProbeOutcome::LegEnded(ops.child_exit_status(&child)),
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
                    // A4's own identity check: a `Proven` challenge only proves the
                    // reply's pid/creation matched what the OS
                    // independently observed on THAT connection — it says
                    // nothing about whether the answering server is the
                    // process THIS episode itself spawned, versus a
                    // stale, orphaned capsule from a prior crash still
                    // bound under the same voyage id while this new
                    // child's own pipe hadn't come up yet. Compare
                    // against the owned child's own identity before
                    // trusting it.
                    return match ops.spawned_identity(&child) {
                        Ok(child_identity) if child_identity == ops.proven_identity(&process) => {
                            // A4: the supervisor keeps the child itself.
                            ProbeOutcome::Ready(ops.retain_owned_child(process, child))
                        }
                        _ => {
                            // Wrong identity, or the child's own identity
                            // could not even be read: fail closed. The
                            // spawned child is now UNTRACKED (this
                            // function is about to return without ever
                            // handing it back) — terminate it AND CONFIRM
                            // it actually died. Reuses A3's own
                            // KILL+WAIT escalation — an unconfirmed kill
                            // here is exactly as severe as it is there.
                            match kill_and_wait(ops, &child, kill_wait_bound) {
                                ProbeOutcome::KilledAfterTimeout => ProbeOutcome::Foreign,
                                other => other, // KillOrWaitFailed
                            }
                        }
                    };
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
#[path = "classify_tests.rs"]
mod tests;
