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
    end_run_and_expect_record_closed(&conn, op_id, "integration test", voyage.clone());

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
    end_run_and_expect_record_closed(&conn, "cleanup-end", "cleanup", voyage);
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

/// F2 (review round), reproduced: `Lifecycle::Ready { process }` →
/// `Ending { .. }` used to drop the ONLY handle able to reap a leg that
/// exits on its OWN while `end_run` is in flight — the worker's own
/// re-challenge over the leg's lane can lose that race (the leg's own
/// socket having already torn down by the time it runs), leaving
/// `finish_end_run_without_process`'s path with no proven handle for
/// this leg at all, and the retained `Ready` handle already silently
/// dropped at the very transition that admitted `end_run` — the leg was
/// then a zombie for the rest of the supervisor's life. Constructed with
/// a producer that exits ON ITS OWN shortly after `Ready` and an
/// `end_run` sent IMMEDIATELY once `Ready` is observed, racing the leg's
/// own natural exit against the worker's own processing — not provably
/// deterministic (which side of the race actually resolves first is a
/// real timing question), but the ~1s window is generous over the
/// worker's own near-instant admission, so the race is exercised on
/// every real run.
///
/// G4 (Codex review round 2): this race has more than one legitimate
/// resolution, and asserting a single one of them made the test flaky —
/// (1) the main loop's own `Ready` tick can observe the leg's natural
/// exit BEFORE `end_run`'s admission check ever runs, refusing it
/// SYNCHRONOUSLY with "no leg is currently running" (never journaled at
/// all — a later `query` for this id reads `UnknownOperation` forever,
/// correctly, since the operation never began); (2) admission wins the
/// race (Lifecycle is still `Ready`), but the worker's own re-challenge
/// still finds the leg already gone with no end-of-run marker to verify
/// — `PreBarrierFailed`, journaled as a terminal `Failed` record via
/// `journal::finish` REGARDLESS of whether this exact connection is ever
/// notified (`Ending`'s own resolution arms only ever signal
/// `pending_reply` on the `RecordClosed` path — see that variant's own
/// doc); (3) the ordinary deterministic path, `RecordClosed` then
/// `RecordVerified`. Only (3) has a further `record_closed -> terminal`
/// step left to observe over the wire; both (1) and (2) are already this
/// operation's own final word (an immediate synchronous reply for (1), a
/// journaled-but-unnotified terminal record for (2), read back via
/// `query` instead of waited for over this connection). FIX: send the
/// command with a bounded, NON-panicking read (a bare `command()` would
/// panic on the exact timeout (2) produces), branch on what actually came
/// back, and assert the PROPERTY every schedule shares — no zombie child
/// while the supervisor is still alive — rather than one specific path.
/// Logs which schedule was observed, useful for anyone debugging a CI
/// flake later. The ORDINARY (non-racing) end-run test keeps its
/// deterministic `RecordClosed` assertion unchanged — this loosening is
/// scoped to the race this test alone constructs.
// Linux-only: the property is single-owner REAPING (a zombie is a Unix
// notion); the Windows twin has nothing to assert, and the windows-latest
// runner once lost the race's fallback query to a lane the supervisor had
// already closed.
#[cfg(target_os = "linux")]
#[test]
fn end_run_racing_a_self_exiting_leg_leaves_no_zombie() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    const SELF_EXITING_SOON: &[&str] = &["/bin/sh", "-c", "sleep 1; exit 0"];

    let mut guard = spawn_supervisor(&state_dir, "--start", SELF_EXITING_SOON);

    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    // Immediately -- racing the leg's own ~1s self-exit against the
    // worker's own processing (the exact window F2 names). The SAME
    // 30s deadline `command`'s own bound uses (B3's own per-op budget
    // reasoning) -- but read directly, not through `command`, so
    // schedule (2)'s genuine "no wire reply at all" never panics this
    // test; it falls through to the journal-backed `query` poll below
    // instead.
    let op_id = "test-end-run-race";
    let request = SupervisorRequest::Command {
        operation_id: op_id.to_string(),
        op: SupervisorOp::EndRun { reason: "race test".into(), voyage },
    };
    let immediate = request_for_test(&conn.client(), &request, Instant::now() + Duration::from_secs(30));
    let immediate_reply = match immediate {
        Ok(SupervisorReply::Operation(state)) => Some(state),
        Ok(other) => panic!("expected Operation, got {other:?}"),
        Err(e) => {
            eprintln!(
                "end_run_racing_a_self_exiting_leg_leaves_no_zombie: no wire reply within the bound ({e}) \
                 -- falling through to a query poll (schedule (2): admitted, PreBarrierFailed, never notified)"
            );
            None
        }
    };

    let final_state = match immediate_reply {
        Some(SupervisorOperationState::RecordClosed) => poll_to_terminal(&conn, op_id, Duration::from_secs(60)),
        Some(other) => other,
        None => poll_until(
            // The supervisor may close this lane connection once the run has
            // ended (idle eviction): a closed lane is not a verdict -- `query`
            // reconnects and asks the journal-backed operation state again.
            || match query(&conn, op_id) {
                SupervisorOperationState::Accepted => None,
                other => Some(other),
            },
            Duration::from_secs(60),
            "the operation to settle (journal-backed, since no wire reply ever arrived)",
        ),
    };
    eprintln!("end_run_racing_a_self_exiting_leg_leaves_no_zombie: observed schedule -> {final_state:?}");

    // Single-owner reaping (review round 2, F2/F7): asserted WHILE the
    // supervisor authority is still alive (before Stop/exit) -- the ONE
    // property every legitimate schedule above shares, regardless of
    // which one actually resolved this run (G4).
    #[cfg(target_os = "linux")]
    assert_eq!(
        zombie_children_of(guard.id()),
        0,
        "the raced leg must not be left an unreaped zombie while the supervisor is still alive (schedule: {final_state:?})"
    );

    // Stop is admitted regardless of lifecycle (no gate) -- ends the
    // authority whether this race left it EndedNoRespawn, Terminal, or
    // respawned into a fresh Ready.
    let _ = command(&conn, "test-stop-race", SupervisorOp::Stop);
    let _status = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// ADR 0041 start-mode table: "`--resume` | sealed, carrying its own
/// `run_end_requested` | do not spawn." The SECOND supervisor must NOT
/// exit immediately -- it serves ended-no-respawn (so a client polling
/// `query` for the operation that ended it, right after the
/// crash-restart, still finds a supervisor to ask) and only exits once
/// explicitly told to `stop`.
#[test]
fn resume_after_a_requested_end_serves_ended_no_respawn_then_exits_on_stop() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut child = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));
    end_run_and_expect_record_closed(&conn, "req-end", "test", voyage);
    let final_state = poll_to_terminal(&conn, "req-end", Duration::from_secs(60));
    assert_eq!(final_state, SupervisorOperationState::RecordVerified);
    assert_eq!(command(&conn, "req-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let status1 = wait_for_exit(&mut child, Duration::from_secs(30));
    assert_eq!(status1.code(), Some(sot_log::supervisor::EXIT_CLEAN));

    // A SECOND supervisor, `--resume`, against the SAME state-dir.
    let mut guard2 = spawn_supervisor(&state_dir, "--resume", SHELL);
    let conn2 = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage2, leg2, phase2) = poll_until(
        || {
            let (voyage, leg, phase) = status(&conn2);
            (phase == SupervisorPhase::EndedNoRespawn).then_some((voyage, leg, phase))
        },
        Duration::from_secs(30),
        "the resumed supervisor to report ended-no-respawn",
    );
    assert_eq!(phase2, SupervisorPhase::EndedNoRespawn);
    assert!(leg2.is_none(), "no leg is running once ended-no-respawn");
    assert!(voyage2.is_some(), "the voyage id survives recovery");
    assert_eq!(
        query(&conn2, "req-end"),
        SupervisorOperationState::RecordVerified,
        "a query for the ORIGINAL operation id, against a brand-new process, still answers"
    );

    assert_eq!(command(&conn2, "req-stop-2", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let status2 = wait_for_exit(&mut guard2, Duration::from_secs(30));
    assert_eq!(status2.code(), Some(sot_log::supervisor::EXIT_CLEAN));
}

/// ADR 0041 "the flap bound": a leg that reaches READY and then dies
/// increments the ONE counter to its threshold. Explicitly PROVES Ready
/// was observed before relying on the shell's own timed self-exit (the
/// test's own name claims it); logs the observed phase at every poll so
/// a future CI failure names the stuck state instead of just timing out
/// (coordinator's round-4 addendum: the pre-rewrite version of this test
/// wedged past its own grace bound on real Windows CI twice).
#[test]
fn a_shell_that_dies_shortly_after_ready_trips_the_anti_flap_bound() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SELF_EXITING_PRODUCER);

    // Ready observed: connect and poll status, logging every phase.
    // `poll_until` itself already proves this (a successful return can
    // only ever be `true` here, and a timeout panics on its own) —
    // Codex review round 4 deletion candidate: the former separate
    // `observed_ready` binding plus `assert!` never added anything.
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    poll_until(
        || {
            let (_voyage, _leg, phase) = status(&conn);
            eprintln!("[flap test] observed phase: {phase:?}");
            (phase == SupervisorPhase::Ready).then_some(true)
        },
        Duration::from_secs(60),
        "the leg to reach Ready at least once before its own timed self-exit",
    );
    drop(conn); // the lane's own 5s idle eviction would close it anyway once flapping starts

    // Shell killed (by its own script) -> flap accounting -> Terminal ->
    // process exit within TERMINAL_EXIT_GRACE of reaching it. Diagnostic
    // on timeout: report whatever `status` still claims. The child stays
    // OWNED BY `guard` throughout (N13, above) -- borrowed here, never
    // taken out -- so a timeout panic still leaves `guard`'s own Drop to
    // kill and wait it rather than leaking an orphaned supervisor.
    //
    // F1 (Codex review round 4): 360s, not 120s -- the implementation's
    // OWN legal worst case, with legal per-op teardown delays and two
    // respawns each reaching Ready near their own 60s readiness cutoff,
    // runs to roughly 276-306s (three legs' own ping+detection+reap+
    // drain+aggregate teardown, two full 60s stability windows before a
    // respawn resets the counter, plus the 2s terminal grace) -- 120s
    // was below the bound this test's own implementation is allowed to
    // legally take, not a bug in the implementation itself.
    let status =
        wait_for_exit_with_diagnostics(guard.child_mut(), &h, Duration::from_secs(360));
    assert_eq!(status.code(), Some(sot_log::supervisor::EXIT_TERMINAL), "three unstable legs must terminate the supervisor");
}

/// An EndRun whose `record_closed` lands after the supervisor lane's idle deadline still answers
/// the client that waits for it in silence (ADR 0041: the command reply arrives at `record_closed`).
/// The delay is the leg's own: four plain connections that never send a frame fill its pre-admission
/// cap, so the supervisor's delivery is refused until the leg's 10 s pre-admission timeout closes them.
#[cfg(target_os = "linux")]
#[test]
fn an_end_run_reply_owed_past_the_lane_idle_deadline_reaches_the_waiting_client() {
    use std::io::Read as _;
    use std::os::unix::net::UnixStream;
    use sot_log::supervisor::LANE_IDLE_DEADLINE;
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let socket = sot_log::lane::socket_unix::voyage_socket_path(&voyage).expect("the voyage socket path");
    let held: Vec<UnixStream> = (0..4).map(|_| UnixStream::connect(&socket).expect("a plain connection")).collect();
    // The premise, observed: a fifth is closed with no frame, and the four are still open.
    let mut fifth = UnixStream::connect(&socket).expect("a fifth plain connection");
    fifth.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    assert_eq!(fifth.read(&mut [0u8; 1]).expect("the leg refuses the fifth"), 0, "the leg's pre-admission cap is full");
    for mut c in &held {
        c.set_nonblocking(true).unwrap();
        let open = matches!(c.read(&mut [0u8; 1]), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock);
        assert!(open, "each held connection is still admitted, unclassified");
    }

    let sent = Instant::now();
    end_run_and_expect_record_closed(&conn, "late-end", "late", voyage);
    let waited = sent.elapsed();
    assert!(
        waited >= LANE_IDLE_DEADLINE + Duration::from_secs(1),
        "record_closed landed a second past the idle deadline, as this test needs: {waited:?}"
    );
    drop(held);
    let _ = poll_to_terminal(&conn, "late-end", Duration::from_secs(60));
    let _ = command(&conn, "late-stop", SupervisorOp::Stop);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
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
    end_run_and_expect_record_closed(&conn, "idle-end", "cleanup", voyage);
    let _ = poll_to_terminal(&conn, "idle-end", Duration::from_secs(60));
    let _ = command(&conn, "idle-stop", SupervisorOp::Stop);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// ADR 0041: a client timeout abandons the connection, never the operation. Every journal publish of this
/// authority is held 3 s (`SOT_TEST_JOURNAL_PUBLISH_DELAY_MS`), so its Stop admission (two publishes) outlasts the
/// 5 s reply budget; `supervisor_client::stop` still reports the stop, because the authority's exit is its outcome.
#[test]
fn a_stop_answered_after_the_reply_budget_still_stops() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let mut guard =
        spawn_supervisor_with_env(&state_dir, "--start", SHELL, &[("SOT_TEST_JOURNAL_PUBLISH_DELAY_MS", "3000")]);
    let conn = wait_for_lane(&state_dir_hash(&state_dir), Duration::from_secs(30));
    wait_for_ready(&conn, Duration::from_secs(90));
    drop(conn);

    let started = Instant::now();
    sot_log::attach_client::supervisor_client::stop(&state_dir).expect("a Stop answered late still stops");
    assert!(
        started.elapsed() >= Duration::from_secs(5),
        "the Stop's admission outlasted the 5 s reply budget, so the late-reply path ran"
    );
    assert!(guard.child_mut().try_wait().unwrap().is_some(), "the authority has exited");
}

/// The same rule for a Reset: with every journal publish held 6 s, the Reset's admission outlasts the reply budget,
/// and `supervisor_client::reset` follows its operation on a fresh connection to the new voyage.
#[test]
fn a_reset_answered_after_the_reply_budget_completes() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _guard =
        spawn_supervisor_with_env(&state_dir, "--start", SHELL, &[("SOT_TEST_JOURNAL_PUBLISH_DELAY_MS", "6000")]);
    let conn = wait_for_lane(&state_dir_hash(&state_dir), Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));
    drop(conn);

    let ended = sot_log::attach_client::supervisor_client::end_run(&state_dir, &voyage, "test end").expect("end_run");
    assert!(
        matches!(ended, sot_log::attach_client::supervisor_client::EndRunOutcome::RecordVerified),
        "the run ended: {ended:?}"
    );
    let started = Instant::now();
    let new_voyage =
        sot_log::attach_client::supervisor_client::reset(&state_dir).expect("a Reset answered late still completes");
    assert!(
        started.elapsed() >= Duration::from_secs(5),
        "the Reset's admission outlasted the 5 s reply budget, so the late-reply path ran"
    );
    assert_ne!(new_voyage, voyage, "the reset minted a new voyage");
}

/// A Reset whose authority dies before answering ends at once with the no-listener error, well inside
/// `RESET_BUDGET`, instead of polling a lane nobody serves. Linux only: the test ends the supervisor it spawned with
/// SIGKILL while the slow admission (6 s per journal publish) holds the reply.
#[cfg(target_os = "linux")]
#[test]
fn a_reset_whose_supervisor_dies_ends_at_once() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let guard =
        spawn_supervisor_with_env(&state_dir, "--start", SHELL, &[("SOT_TEST_JOURNAL_PUBLISH_DELAY_MS", "6000")]);
    let conn = wait_for_lane(&state_dir_hash(&state_dir), Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));
    drop(conn);
    let ended = sot_log::attach_client::supervisor_client::end_run(&state_dir, &voyage, "test end").expect("end_run");
    assert!(
        matches!(ended, sot_log::attach_client::supervisor_client::EndRunOutcome::RecordVerified),
        "the run ended: {ended:?}"
    );

    let own = guard.id() as libc::pid_t;
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(5500));
        // SAFETY: SIGKILL to the supervisor this test spawned (the guard's own child), a plain syscall.
        unsafe { libc::kill(own, libc::SIGKILL) };
    });
    let started = Instant::now();
    let error = sot_log::attach_client::supervisor_client::reset(&state_dir)
        .expect_err("a reset whose authority died cannot complete");
    killer.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the reset ended soon after its authority died, not at its budget: {:?}",
        started.elapsed()
    );
    assert!(error.to_string().contains("went away"), "the no-listener error: {error}");
    drop(guard);
}
