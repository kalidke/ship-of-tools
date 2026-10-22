//! Supervisor authority tests: adoption, proto and build-id checks, endrun/reset rules, voyage identity.

use super::*;

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
        matches!(outcome, sot_platform::identity::challenge::ChallengeOutcome::Proven(_)),
        "a different build id (same proto) must be Proven -- ADR 0045 decision 7: the gate is proto alone, got {outcome:?}"
    );
    let conn = Lane::new(&h, conn);

    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));
    end_run_and_expect_ended(&conn, "cleanup-end", "cleanup", voyage);
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
        matches!(outcome, sot_platform::identity::challenge::ChallengeOutcome::Foreign),
        "a wrong lane proto must be classified Foreign (refused{{version_skew}}), got {outcome:?}"
    );
    expect_connection_closes(&conn, Duration::from_secs(5));

    // F3: the authority itself must still be alive and serving after the
    // version-skew refusal -- a FRESH connection with the RIGHT proto
    // proves it, then ends the run and stops the authority before the
    // guard kills anything, so no leg is stranded.
    let conn2 = wait_for_lane(&h, Duration::from_secs(10));
    let (voyage, _leg) = wait_for_ready(&conn2, Duration::from_secs(90));
    end_run_and_expect_ended(&conn2, "cleanup-end", "cleanup", voyage);
    let _ = poll_to_terminal(&conn2, "cleanup-end", Duration::from_secs(60));
    assert_eq!(command(&conn2, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
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
        build: sot_log::lane::exchange::SUPERVISOR_LANE_BUILD_ID.to_string(),
    })
    .unwrap();
    conn.client().write_all(&second_hello).unwrap();
    expect_connection_closes(&conn.client(), Duration::from_secs(5));

    // The authority itself must have survived: a FRESH connection still
    // gets a normal, correct answer.
    let conn2 = wait_for_lane(&h, Duration::from_secs(10));
    let (voyage2, _leg2, phase2) = status(&conn2);
    assert_eq!(phase2, SupervisorPhase::Ready, "the authority must still be alive and serving after the protocol violation");

    end_run_and_expect_ended(&conn2, "cleanup-end", "cleanup", voyage2.unwrap());
    let _ = poll_to_terminal(&conn2, "cleanup-end", Duration::from_secs(60));
    assert_eq!(command(&conn2, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
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

    end_run_and_expect_ended(&conn, "stale-1", "test", voyage);
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

    end_run_and_expect_ended(&conn, "cleanup-end", "cleanup", voyage);
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

    end_run_and_expect_ended(&conn, "end-before-reset", "test", old_voyage.clone());
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

    end_run_and_expect_ended(&conn, "cleanup-end", "cleanup", new_voyage);
    let _ = poll_to_terminal(&conn, "cleanup-end", Duration::from_secs(60));
    assert_eq!(command(&conn, "cleanup-stop", SupervisorOp::Stop), SupervisorOperationState::Stopping);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}
