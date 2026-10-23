//! Supervisor lifecycle tests: a full run, and the status probe.

use super::*;

/// A real daemon-shaped client dials the row socket of a real spawned `sot-capsule supervise`, reads status, ends the
/// run, and observes the supervisor's own clean exit after a stop.
#[test]
fn full_lifecycle_status_end_run_and_clean_exit() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);

    // `wait_for_lane` already ran the dial's hello, which names the row number: a second hello here would be a
    // protocol violation.
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (_voyage, run) = wait_for_ready(&conn, Duration::from_secs(90));

    end_run_and_expect_ended(&conn, &run, "integration test");
    let rested = status(&conn);
    assert_eq!(rested.phase, SupervisorPhase::EndedNoRespawn, "the run's end is answered once the supervisor rests");
    assert_eq!(rested.run, None, "a resting supervisor has no run");

    // Single-owner reaping: asserted HERE, while the supervisor is still alive (its own `/proc/<pid>` entry, hence this
    // check, only exists until it exits), right after the run ended and before `stop`. Proves the agent is reaped as
    // part of ending the run, not merely once the whole supervisor exits and reparents a leftover zombie to init.
    #[cfg(target_os = "linux")]
    assert_eq!(
        zombie_children_of(guard.id()),
        0,
        "the ended agent must already be reaped while the supervisor is still alive"
    );

    // An end asked of a run that is over finds none live.
    let again = end_run(&conn, &run, "integration test");
    assert_eq!(again["outcome"], "failed", "{again}");

    stop(&conn);
    let status = wait_for_exit(&mut guard, Duration::from_secs(30));
    assert_eq!(status.code(), Some(sot_log::supervisor::EXIT_CLEAN), "a clean end_run and stop must exit 0");
}

/// Switch-latency Phase 1 (b): once a supervisor's own main loop has had
/// nothing to do for well over `MAIN_LOOP_POLL` (its own 100ms idle
/// cadence) -- so it is genuinely parked in the loop's own tail wait, not
/// mid-tick -- a status probe through the SAME production entry point
/// `rows::run::probe::phase_of`/`rows::run::activation::ensure_started` use
/// (`supervisor_client::query_status`: a fresh connect, the identity
/// challenge, one status exchange) must complete near-instantly rather
/// than risk paying the OLD worst case (a connection landing right after
/// a tick, sitting unaccepted until the next `sleep(MAIN_LOOP_POLL)`
/// wakes the loop).
///
/// Codex round on #227 (P2 discharge): a SINGLE sample against a 200ms
/// bound does not reliably fail on the OLD, poll-only code -- its own
/// worst case is bounded by `MAIN_LOOP_POLL` itself (100ms) plus a small
/// connect/challenge/status overhead, so a lucky tick-phase alignment
/// can land comfortably under 200ms even without this fix. `TRIALS`
/// independent probes, each idled past `MAIN_LOOP_POLL` first, asked to
/// ALL land under HALF of it (50ms), is the mechanism proof instead: old
/// code's own per-trial success chance under a bound that tight is at
/// best a coin flip (uniformly distributed by connection-arrival phase),
/// so all `TRIALS` succeeding by chance is astronomically unlikely
/// (~0.5^20), while an immediate, connection-triggered wake makes every
/// single trial land near-instantly, every time, deterministically.
#[test]
fn a_status_probe_against_an_idle_supervisor_is_not_poll_bound() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);

    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (_voyage, run) = wait_for_ready(&conn, Duration::from_secs(90));
    drop(conn); // only the timed trials below are measured

    const TRIALS: usize = 20;
    const IDLE_BEFORE_PROBE: Duration = Duration::from_millis(150); // > MAIN_LOOP_POLL (100ms)
    const TIGHT_BOUND: Duration = Duration::from_millis(50); // half of MAIN_LOOP_POLL
    let mut elapsed_all = Vec::with_capacity(TRIALS);
    for _ in 0..TRIALS {
        // Let the lane go genuinely idle before each trial -- comfortably
        // longer than `MAIN_LOOP_POLL` -- so the main loop is parked in
        // its own tail wait when this trial's probe connects, not
        // mid-tick from the PREVIOUS trial's own connect/challenge.
        std::thread::sleep(IDLE_BEFORE_PROBE);
        let started = Instant::now();
        let (report, _process) =
            sot_log::attach_client::supervisor_client::query_status(&state_dir).expect("status probe against an idle supervisor");
        elapsed_all.push(started.elapsed());
        assert_eq!(report.phase, SupervisorPhase::Ready);
    }

    // Cleanup BEFORE the mechanism assertion below (Codex round on #227):
    // a failing bound must not strand the shell leg behind a panic --
    // end the run (never just kill the authority out from under it) so
    // nothing is left running once `guard` drops.
    let conn = wait_for_lane(&h, Duration::from_secs(5));
    end_run_and_expect_ended(&conn, &run, "cleanup");
    stop(&conn);
    let status = wait_for_exit(&mut guard, Duration::from_secs(30));
    assert_eq!(status.code(), Some(sot_log::supervisor::EXIT_CLEAN), "a clean end_run and stop must exit 0");

    println!(
        "idle-supervisor query_status over {TRIALS} trials: max={:?}, all={elapsed_all:?}",
        elapsed_all.iter().max().unwrap()
    );
    assert!(
        elapsed_all.iter().all(|e| *e < TIGHT_BOUND),
        "expected every one of {TRIALS} status probes against an idle supervisor to complete in well \
         under {TIGHT_BOUND:?} (half of MAIN_LOOP_POLL) -- got {elapsed_all:?}. A single probe passing \
         this bound could be luck; ALL {TRIALS} passing is only possible if the main loop wakes on the \
         incoming connection immediately rather than waiting out its own poll cadence"
    );
}
