//! Supervisor lifecycle tests: full run, status probe, end_run races, resume, anti-flap bound.

use super::*;

/// A real launcher-shaped client connects the supervisor lane of a real
/// spawned `sot-capsule supervise`, runs hello/status/end_run/query, and
/// observes the supervisor's own clean exit.
#[test]
fn full_lifecycle_hello_status_end_run_query_and_clean_exit() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);

    // `wait_for_lane` already ran the FULL same-connection challenge,
    // whose own `hello`/`hello_ok` round trip IS this connection's hello
    // — a second, explicit `hello` here would be a protocol violation.
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    assert_eq!(query(&conn, "unused-op-id"), SupervisorOperationState::UnknownOperation);

    let op_id = "test-end-run-1";
    end_run_and_expect_ended(&conn, op_id, "integration test", voyage.clone());

    let final_state = poll_to_terminal(&conn, op_id, Duration::from_secs(60));
    assert_eq!(final_state, SupervisorOperationState::RecordVerified);

    // Single-owner reaping (review round 2, F7): asserted HERE, WHILE
    // the supervisor authority is still alive (its own `/proc/<pid>`
    // entry, hence this check, only exists until it exits) — right
    // after the run ended, before `Stop` is ever sent. Proves the leg
    // `finish_end_run_with_process` ended is reaped as PART of ending
    // the run, not merely "eventually, once the whole supervisor process
    // itself exits and reparents any leftover zombie to init."
    #[cfg(target_os = "linux")]
    assert_eq!(
        zombie_children_of(guard.id()),
        0,
        "the ended leg must already be reaped while the supervisor is still alive"
    );

    // Resubmitting the SAME operation id with the SAME digest is
    // idempotent -- it must answer with the current state, not
    // re-execute.
    let resubmit = command(&conn, op_id, SupervisorOp::EndRun { reason: "integration test".into(), voyage: voyage.clone() });
    assert_eq!(resubmit, SupervisorOperationState::RecordVerified);

    // A DIFFERENT digest under the same id is an id_conflict.
    let conflict = command(&conn, op_id, SupervisorOp::Stop);
    assert_eq!(conflict, SupervisorOperationState::Refused { reason: sot_log::lane::wire::SupervisorRefusedReason::IdConflict });

    let stop_reply = command(&conn, "test-stop-1", SupervisorOp::Stop);
    assert_eq!(stop_reply, SupervisorOperationState::Stopping);

    let status = wait_for_exit(&mut guard, Duration::from_secs(30));
    assert_eq!(status.code(), Some(sot_log::supervisor::EXIT_CLEAN), "a clean EndRun+Stop must exit 0");
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
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));
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
    end_run_and_expect_ended(&conn, "cleanup-end", "cleanup", voyage);
    let _ = poll_to_terminal(&conn, "cleanup-end", Duration::from_secs(60));
    let stop_reply = command(&conn, "cleanup-stop", SupervisorOp::Stop);
    assert_eq!(stop_reply, SupervisorOperationState::Stopping);
    let status = wait_for_exit(&mut guard, Duration::from_secs(30));
    assert_eq!(status.code(), Some(sot_log::supervisor::EXIT_CLEAN), "a clean EndRun+Stop must exit 0");

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

/// A helper request on a connection the supervisor has closed as idle reconnects once and is answered: a host that
/// stalls a test for `LANE_IDLE_DEADLINE` between two requests costs a reconnect, not the test.
#[test]
fn a_request_after_an_idle_close_reconnects_once_and_is_answered() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, leg) = wait_for_ready(&conn, Duration::from_secs(90));
    // The premise, observed: a client that sends nothing has its connection closed by the supervisor.
    expect_connection_closes(&conn.client(), sot_log::supervisor::LANE_IDLE_DEADLINE + Duration::from_secs(5));

    assert_eq!(
        status(&conn),
        (Some(voyage.clone()), Some(leg), SupervisorPhase::Ready),
        "status after the idle close reconnects and is answered"
    );
    end_run_and_expect_ended(&conn, "idle-end", "cleanup", voyage);
    let _ = poll_to_terminal(&conn, "idle-end", Duration::from_secs(60));
    let _ = command(&conn, "idle-stop", SupervisorOp::Stop);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}
