//! Supervisor authority tests: adoption, proto and build-id checks, endrun/reset rules, voyage identity.

use super::*;

/// ADR 0041 "an ADOPTED leg ends correctly" / the nightly composite's own
/// premise: the supervisor dying leaves the capsule headless, and the
/// NEXT start adopts it rather than spawning a duplicate or silently
/// losing it.
#[test]
fn a_second_supervisor_adopts_a_leg_left_behind_by_a_killed_first_one() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut first_guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, leg) = wait_for_ready(&conn, Duration::from_secs(90));
    drop(conn);

    // Kill the SUPERVISOR only -- the leg is deliberately NOT in its
    // job/kill-domain (ADR 0041/0043: "the supervisor dying must be
    // harmless to the run"), so the shell must still be alive and
    // answering afterward.
    first_guard.child_mut().kill().unwrap();
    first_guard.child_mut().wait().unwrap();

    let mut second_guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn2 = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage2, leg2) = wait_for_ready(&conn2, Duration::from_secs(30));
    assert_eq!(voyage2, voyage, "the SAME voyage, never a re-mint");
    assert_eq!(leg2, leg, "the SAME leg epoch -- adopted, not a fresh spawn");

    // Clean up: end the run through the SECOND (now authoritative)
    // supervisor, then stop it, before letting the guards kill anything.
    end_run_and_expect_record_closed(&conn2, "cleanup-end", "test cleanup", voyage2);
    let _ = poll_to_terminal(&conn2, "cleanup-end", Duration::from_secs(60));
    assert_eq!(command(&conn2, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let _ = wait_for_exit(&mut second_guard, Duration::from_secs(60));
}

/// ADR 0045 decision 7/9 (retiring ADR 0041 Lifecycle "Build boundary",
/// which used to refuse exactly this as `refused {version_skew}`): the
/// lane gate is the protocol integer alone -- `build` rides the wire as
/// information only and is never compared, so
/// adopting a supervisor (or, as here, being adopted BY one) of another
/// build is ordinary, not foreign. Renamed from the old (pre-ADR-0045)
/// `a_mismatched_build_id_is_refused_and_the_connection_closes`, whose
/// own F3 fixture (Codex review round) this reuses: same rig -- a
/// connection that echoes a DIFFERENT build id is now `Proven`, not
/// refused, and stays open -- so this test drives the row straight
/// through it (wait for Ready, end the run, stop the authority) rather
/// than needing a second "reconnect with the right build" connection.
#[test]
fn a_different_build_id_with_the_same_proto_is_proven() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let (conn, outcome) = poll_until(
        || sot_log::supervisor::connect_and_challenge_with_build_for_test(&h, "some-other-build").ok(),
        Duration::from_secs(30),
        "the supervisor lane to accept a connection",
    );
    assert!(
        matches!(outcome, sot_log::identity::challenge::ChallengeOutcome::Proven(_)),
        "a different build id (same proto) must be Proven -- ADR 0045 decision 7: the gate is proto alone, got {outcome:?}"
    );

    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));
    end_run_and_expect_record_closed(&conn, "cleanup-end", "cleanup", voyage);
    let _ = poll_to_terminal(&conn, "cleanup-end", Duration::from_secs(60));
    assert_eq!(command(&conn, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// ADR 0045 decision 7: the ONE thing a lane peer still refuses is a
/// mismatched PROTOCOL integer -- `hello_refused{version_skew}`, closing
/// the connection, exactly the shape [`a_different_build_id_with_the_same_proto_is_proven`]
/// above proves a mismatched BUILD no longer triggers. Reuses that test's
/// F3 fixture (Codex review round): the SUPERVISOR (and the leg it
/// already spawned) must still be alive and serving after the refusal, so
/// this reconnects with the RIGHT proto (this build's own), waits for
/// Ready, then ends the run and stops the authority before the
/// `CapsuleGuard` ever runs -- otherwise a `CapsuleGuard` that only kills the
/// supervisor at scope exit strands the leg (a live `SHELL`) for however
/// long it takes the temp state dir to be reclaimed.
#[test]
fn a_mismatched_lane_proto_is_refused_and_the_connection_closes() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let (conn, outcome) = poll_until(
        || sot_log::supervisor::connect_and_challenge_with_proto_for_test(&h, 999).ok(),
        Duration::from_secs(30),
        "the supervisor lane to accept a connection",
    );
    assert!(
        matches!(outcome, sot_log::identity::challenge::ChallengeOutcome::Foreign),
        "a wrong lane proto must be classified Foreign (refused{{version_skew}}), got {outcome:?}"
    );
    expect_connection_closes(conn, Duration::from_secs(5));

    // F3: the authority itself must still be alive and serving after the
    // version-skew refusal -- a FRESH connection with the RIGHT proto
    // proves it, then ends the run and stops the authority before the
    // guard kills anything, so no leg is stranded.
    let conn2 = wait_for_lane(&h, Duration::from_secs(10));
    let (voyage, _leg) = wait_for_ready(&conn2, Duration::from_secs(90));
    end_run_and_expect_record_closed(&conn2, "cleanup-end", "cleanup", voyage);
    let _ = poll_to_terminal(&conn2, "cleanup-end", Duration::from_secs(60));
    assert_eq!(command(&conn2, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// ADR 0041 no-supervisor capability matrix: "proven ABSENT: reset only"
/// -- both exercised with NO supervisor running at all, driving
/// `sot_log::supervisor::{endrun, reset}` directly (the fence-acquiring
/// in-process callers).
///
/// `endrun` against a voyage that was reset but never actually started
/// (N2, Codex review round 4): a raw pipe-NotFound is proven, via
/// `writer.lock`, to be a GENUINE absence here -- but genuine absence
/// with no leg and no requested-end marker still means this end was
/// NEVER ACTUALLY DELIVERED. Reporting that as success (the old
/// behavior) would let a later `--resume` respawn as if nothing had
/// happened, which is exactly the false-positive N2 exists to close --
/// so the loud refusal (69), not a silent EXIT_CLEAN, is correct here.
#[test]
fn endrun_and_reset_without_a_running_supervisor() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    assert_eq!(sot_log::supervisor::endrun(&state_dir, None, "no drawer yet".into()), sot_log::supervisor::EXIT_TERMINAL);

    assert_eq!(sot_log::supervisor::reset(&state_dir, None), sot_log::supervisor::EXIT_CLEAN);
    let minted = match sot_log::supervisor::journal::pointer::validate(&state_dir) {
        sot_log::supervisor::journal::pointer::PointerState::Valid(id) => id,
        other => panic!("expected a valid pointer after reset, got {other:?}"),
    };

    // N2 (Codex review round 4): this voyage was minted by the reset
    // above but never actually started -- no leg, no requested-end
    // marker. A GENUINELY absent writer (proven via writer.lock) with
    // nothing ever delivered is a loud refusal, never a silent
    // EXIT_CLEAN success -- see this test's own doc comment.
    assert_eq!(
        sot_log::supervisor::endrun(&state_dir, Some(minted.clone()), "still nothing running".into()),
        sot_log::supervisor::EXIT_TERMINAL
    );

    assert_eq!(sot_log::supervisor::reset(&state_dir, Some(minted.clone())), sot_log::supervisor::EXIT_CLEAN);
    match sot_log::supervisor::journal::pointer::validate(&state_dir) {
        sot_log::supervisor::journal::pointer::PointerState::Valid(id) => assert_ne!(id, minted, "reset must mint a NEW identity, never reuse the old one"),
        other => panic!("expected a valid pointer after the second reset, got {other:?}"),
    }
}

/// A SECOND `hello` on an already-challenged connection is a plain
/// protocol violation -- this connection closes, but the supervisor
/// PROCESS survives and keeps answering everyone else.
#[test]
fn a_second_hello_closes_the_connection_but_the_authority_survives() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    wait_for_ready(&conn, Duration::from_secs(90));

    let second_hello = sot_log::lane::wire::encode_supervisor_request(&SupervisorRequest::Hello {
        proto: sot_log::lane::wire::SUPERVISOR_PROTO_V1,
        build: sot_log::identity::exchange::SUPERVISOR_LANE_BUILD_ID.to_string(),
    })
    .unwrap();
    conn.write_all(&second_hello).unwrap();
    expect_connection_closes(conn, Duration::from_secs(5));

    // The authority itself must have survived: a FRESH connection still
    // gets a normal, correct answer.
    let conn2 = wait_for_lane(&h, Duration::from_secs(10));
    let (voyage2, _leg2, phase2) = status(&conn2);
    assert_eq!(phase2, SupervisorPhase::Ready, "the authority must still be alive and serving after the protocol violation");

    end_run_and_expect_record_closed(&conn2, "cleanup-end", "cleanup", voyage2.unwrap());
    let _ = poll_to_terminal(&conn2, "cleanup-end", Duration::from_secs(60));
    assert_eq!(command(&conn2, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// A supervisor that journaled `end_run` as `accepted` (durable, under
/// `supervisor.lock`, BEFORE the first irreversible act) and then died
/// BEFORE its own worker ever reached the capsule must still have that
/// `end_run` DELIVERED, not merely waited for, by a fresh supervisor's
/// recovery pass — the defect this test proves fixed:
/// `reconcile_journal_on_startup`'s `EndRun` arm used to jump straight
/// to a wait-only reconcile, which never resolves for a capsule nobody
/// ever actually told to end.
///
/// Constructed DETERMINISTICALLY from the crash-durable state itself,
/// never by racing a live kill against a real submission's own worker
/// thread. A prior version submitted `end_run` over the wire and raced
/// a watcher's `query` poll against the kill to catch a momentary
/// `Accepted`/`RecordClosed` sample first — PR #171 review: that
/// worker's whole pipeline (mgmt exchange, capsule teardown, marker
/// check, `verify_voyage`) runs on one thread with no built-in pause
/// anywhere in between, and on fast CI hardware reliably raced straight
/// through to a terminal record before even the FIRST watcher sample,
/// so the poll timed out waiting for a window that no longer existed —
/// a coin flip this rewrite removes by never depending on it. This test
/// instead starts a capsule through a first supervisor exactly like
/// every other test here, kills that supervisor WITHOUT ever submitting
/// `end_run` (the capsule is untouched by its supervisor's death — ADR
/// 0041 Lifecycle: "any exit code, an FE crash, supervisor death — all
/// are FE loss; the capsule is untouched" — so it stays alive,
/// orphaned), then hand-journals the EXACT `ActiveOp::EndRun` record a
/// live admission would have written, via the crate's own public
/// `journal::begin` — no `run_end_requested` marker exists yet, exactly
/// the state a worker killed before its first mgmt-lane exchange
/// leaves. This IS the crash state, not a simulation raced into
/// existence. The race-based version added nothing over
/// `full_lifecycle_hello_status_end_run_query_and_clean_exit` (already
/// proves live wire admission: command -> journal -> record_closed ->
/// record_verified, resubmit idempotency, id_conflict) beyond
/// recovering a genuinely in-flight operation — exactly what this
/// version proves, without the race.
#[test]
fn a_crashed_supervisor_s_end_run_is_recovered_and_queryable_by_a_fresh_one() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    // A running capsule, exactly as any other test here starts one.
    let mut first_guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, leg) = wait_for_ready(&conn, Duration::from_secs(90));

    // Kill the supervisor WITHOUT ever submitting end_run. The LEG
    // process is not in the supervisor's job/kill-domain (module doc),
    // so it stays alive, orphaned — the "crash after admission, before
    // the capsule was ever told" state this test constructs directly
    // rather than racing a kill against a live submission to land there.
    first_guard.child_mut().kill().unwrap();
    first_guard.child_mut().wait().unwrap();

    // Hand-journal the SAME `ActiveOp::EndRun` record a live admission
    // would have written (`supervisor/authority/mod.rs`'s own `handle_command`:
    // `ActiveOp::EndRun { voyage: voyage_id, epoch: leg_epoch_of(...) }`
    // — `leg` here IS that epoch, per `status_ok`'s own `leg` field),
    // via the crate's own public `journal::begin` — the exact API a
    // fresh supervisor's recovery consumes. The digest need not match a
    // real wire encoding: recovery reads `active.op` directly and never
    // compares digests (those exist only for the WIRE's own
    // idempotent-resubmit check, never exercised here);
    // `ActiveRecord::validate` only checks the field's SHAPE (64
    // lowercase hex chars) — the same `"0".repeat(64)` placeholder
    // `sot-fault-writer.rs`'s own fixture uses for an unchecked digest.
    let op_id = "op-recover";
    let record = journal::ActiveRecord {
        operation_id: op_id.to_string(),
        digest: "0".repeat(64),
        op: journal::ActiveOp::EndRun { voyage: voyage.clone(), epoch: Some(leg) },
    };
    journal::begin(&state_dir, op_id, &record).unwrap();

    // Non-vacuous: recovery genuinely has work to do before the fresh
    // supervisor ever starts, proven by reading the same durable state
    // its own recovery pass will.
    assert_eq!(
        journal::active_operations(&state_dir).unwrap(),
        vec![op_id.to_string()],
        "the hand-journaled operation must be active before the fresh supervisor starts"
    );

    // A fresh supervisor, `--resume`: recovery must DELIVER the
    // end_run to the still-live orphaned capsule (this test's own fix),
    // not merely wait for a writer nobody ever told to go away.
    let mut second_guard = spawn_supervisor(&state_dir, "--resume", SHELL);
    let conn2 = wait_for_lane(&h, Duration::from_secs(30));

    let final_state = poll_to_terminal(&conn2, op_id, Duration::from_secs(120));
    assert_eq!(final_state, SupervisorOperationState::RecordVerified);

    // The orphaned capsule's own mgmt lane is gone -- its teardown
    // removes the endpoint NAME before final writes/seal/writer-lock
    // release (`capsule/`), so this proves the capsule this test
    // started is no longer serving, external to and independent of
    // whatever the supervisor's own recovery believes.
    let mgmt_gone = matches!(connect_voyage_mgmt(&voyage), Err(e) if e.is_endpoint_absent());
    assert!(mgmt_gone, "the orphaned capsule's own mgmt lane must be gone once its end_run is recovered");

    // Recovering an end_run for the CURRENT voyage means no leg is ever
    // spawned -- eventually straight to ended-no-respawn (not necessarily
    // immediately: the lane is phase-total, so a status mid-Starting is a
    // legitimate observation while recovery is still resolving via the
    // ordinary adopt-probe/start-mode path).
    let (_voyage2, leg2, phase2) = poll_until(
        || {
            let (voyage, leg, phase) = status(&conn2);
            (phase == SupervisorPhase::EndedNoRespawn).then_some((voyage, leg, phase))
        },
        Duration::from_secs(90),
        "the resumed supervisor to reach ended-no-respawn",
    );
    assert_eq!(phase2, SupervisorPhase::EndedNoRespawn);
    assert!(leg2.is_none(), "no leg is running once ended-no-respawn");

    assert_eq!(command(&conn2, "op-recover-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let _ = wait_for_exit(&mut second_guard, Duration::from_secs(30));
}

/// ADR 0041 voyage-fencing: a mismatch is refused `stale_voyage` with NO
/// mutation, checked before the journal is ever touched -- so the SAME
/// operation id, resubmitted with the CORRECT voyage, is still
/// admissible afterward.
#[test]
fn a_command_naming_the_wrong_voyage_is_refused_stale_voyage() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let wrong_voyage = "00000000-0000-0000-0000-000000000000".to_string();
    assert_ne!(wrong_voyage, voyage);
    let reply = command(&conn, "stale-1", SupervisorOp::EndRun { reason: "test".into(), voyage: wrong_voyage });
    assert_eq!(reply, SupervisorOperationState::Refused { reason: sot_log::lane::wire::SupervisorRefusedReason::StaleVoyage });

    end_run_and_expect_record_closed(&conn, "stale-1", "test", voyage);
    let _ = poll_to_terminal(&conn, "stale-1", Duration::from_secs(60));
    assert_eq!(command(&conn, "stale-1-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// ADR 0041 (Codex review round 2, B2): `reset` is admissible ONLY from
/// `EndedNoRespawn` -- refused while a leg is live, through the generic
/// `Failed{detail}` shape (there is no dedicated wire refusal reason for
/// it); `Reset{voyage: None}` while a live voyage exists is refused as
/// `stale_voyage`. Neither mutates the pointer.
#[test]
fn reset_is_refused_while_a_leg_is_live() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let reply = command(&conn, "reset-while-live", SupervisorOp::Reset { voyage: Some(voyage.clone()) });
    match reply {
        SupervisorOperationState::Failed { detail } => {
            assert!(detail.to_lowercase().contains("live"), "expected a live-leg refusal, got {detail:?}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }

    let reply2 = command(&conn, "reset-none-while-live", SupervisorOp::Reset { voyage: None });
    assert_eq!(reply2, SupervisorOperationState::Refused { reason: sot_log::lane::wire::SupervisorRefusedReason::StaleVoyage });

    match sot_log::supervisor::journal::pointer::validate(&state_dir) {
        sot_log::supervisor::journal::pointer::PointerState::Valid(id) => assert_eq!(id, voyage, "the pointer must be unchanged after both refusals"),
        other => panic!("expected the pointer to still be valid and unchanged, got {other:?}"),
    }

    end_run_and_expect_record_closed(&conn, "cleanup-end", "cleanup", voyage);
    let _ = poll_to_terminal(&conn, "cleanup-end", Duration::from_secs(60));
    assert_eq!(command(&conn, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}

/// ADR 0041 (Codex review round 2, B2): once `EndedNoRespawn`, `reset`
/// IS admissible and produces a genuinely NEW voyage the authority then
/// spawns a fresh leg for.
#[test]
fn reset_from_ended_no_respawn_mints_a_new_voyage_and_spawns_for_it() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (old_voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    end_run_and_expect_record_closed(&conn, "end-before-reset", "test", old_voyage.clone());
    let _ = poll_to_terminal(&conn, "end-before-reset", Duration::from_secs(60));
    poll_until(
        || matches!(status(&conn), (_, _, SupervisorPhase::EndedNoRespawn)).then_some(()),
        Duration::from_secs(30),
        "ended-no-respawn before submitting reset",
    );

    let reset_reply = command(&conn, "do-reset", SupervisorOp::Reset { voyage: Some(old_voyage.clone()) });
    assert_eq!(reset_reply, SupervisorOperationState::Accepted);
    let reset_final = poll_to_terminal(&conn, "do-reset", Duration::from_secs(30));
    match reset_final {
        SupervisorOperationState::ResetDone { new_voyage } => assert_ne!(new_voyage, old_voyage),
        other => panic!("expected ResetDone, got {other:?}"),
    }

    // The authority spawns a fresh leg for the NEW voyage.
    let (new_voyage, _leg2) = wait_for_ready(&conn, Duration::from_secs(90));
    assert_ne!(new_voyage, old_voyage);

    end_run_and_expect_record_closed(&conn, "cleanup-end", "cleanup", new_voyage);
    let _ = poll_to_terminal(&conn, "cleanup-end", Duration::from_secs(60));
    assert_eq!(command(&conn, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}
