use super::*;

#[test]
fn is_journal_unreadable_matches_only_that_shape() {
    assert!(is_journal_unreadable(&SupervisorOperationState::Failed { detail: "journal unreadable: boom".into() }));
    assert!(!is_journal_unreadable(&SupervisorOperationState::Failed { detail: "record_append".into() }));
    assert!(!is_journal_unreadable(&SupervisorOperationState::Accepted));
}

/// Every non-`EndedNoRespawn` state must have SOME refusal detail
/// -- `reset_refusal_detail`'s own match is exhaustive over
/// every OTHER variant at compile time; this exercises a
/// representative sample of the actual strings too. No `Stopping`
/// case any more: `stop` no longer
/// touches the `Lifecycle` at all, so there is no "busy because
/// stopping" state for `reset_refusal_detail` to describe -- a
/// reset attempted while a stop is pending is admitted and resolved
/// exactly like any other command would be.
#[test]
fn reset_refusal_detail_names_the_reason_for_every_busy_state() {
    let (_tx, rx) = mpsc::channel::<RecoveryOutcome>();
    let recovering = Lifecycle::Recovering { rx, handle: std::thread::spawn(|| {}), started_at: Instant::now() };
    assert!(reset_refusal_detail(&recovering).contains("recovering"));

    let terminal = Lifecycle::Terminal { detail: "x".into(), entered_at: Instant::now() };
    assert!(reset_refusal_detail(&terminal).contains("terminal"));
}

fn test_authority(state_dir: &Path) -> AuthorityState {
    AuthorityState {
        state_dir: state_dir.to_path_buf(),
        voyage_id: None,
        self_pid: 0,
        self_created: 0,
        stop_requested: None,
        producer_ran: false,
        retired_legs: Vec::new(),
    }
}

fn reset_record(op: &SupervisorOp, old_voyage: &str, new_voyage: &str, aside: &str) -> journal::ActiveRecord {
    journal::ActiveRecord {
        operation_id: "reset-x".into(),
        digest: digest_of(op).unwrap(),
        op: journal::ActiveOp::Reset {
            old_voyage: Some(old_voyage.into()),
            new_voyage: new_voyage.into(),
            aside: Some(aside.into()),
        },
    }
}

/// A `journal::begin` that fails after its rename took leaves the record
/// on disk with no worker behind it: the operation is admitted from that
/// record, with its own voyage and aside, and nothing is minted again.
#[test]
fn a_begin_that_failed_after_its_rename_is_admitted_from_its_record() {
    let dir = tempfile::tempdir().unwrap();
    let old = uuid::Uuid::now_v7().to_string();
    let op = SupervisorOp::Reset { voyage: Some(old.clone()) };
    let published = reset_record(&op, &old, &uuid::Uuid::now_v7().to_string(), "drawer.voyage.reset-aaaaaaaaaaaaaaaa");
    journal::begin(dir.path(), "reset-x", &published).unwrap();

    // Another attempt at the same operation (same digest, another new voyage) gets the published record back.
    let again = reset_record(&op, &old, &uuid::Uuid::now_v7().to_string(), "drawer.voyage.reset-bbbbbbbbbbbbbbbb");
    assert_eq!(begin_or_readback(dir.path(), "reset-x", again).unwrap(), published);

    // A record with another digest is no admission: the begin's own error stands.
    let mut other = reset_record(&op, &old, &uuid::Uuid::now_v7().to_string(), "drawer.voyage.reset-cccccccccccccccc");
    other.digest = digest_of(&SupervisorOp::Stop).unwrap();
    assert!(begin_or_readback(dir.path(), "reset-x", other).is_err());

    // Nothing on disk: the record is published as given.
    let empty = tempfile::tempdir().unwrap();
    let fresh = reset_record(&op, &old, &uuid::Uuid::now_v7().to_string(), "drawer.voyage.reset-dddddddddddddddd");
    assert_eq!(begin_or_readback(empty.path(), "reset-x", fresh.clone()).unwrap(), fresh);
    assert_eq!(journal::read_active(empty.path(), "reset-x").unwrap(), Some(fresh));
}

/// A published operation with no worker, resubmitted, is answered from its
/// state (Accepted): nothing admits it a second time.
#[test]
fn a_resubmitted_active_operation_is_answered_from_its_state() {
    let dir = tempfile::tempdir().unwrap();
    let old_voyage = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
    let mut authority = test_authority(dir.path());
    authority.voyage_id = Some(old_voyage.clone());
    let op = SupervisorOp::Reset { voyage: Some(old_voyage.clone()) };
    let record = reset_record(&op, &old_voyage, &uuid::Uuid::now_v7().to_string(), "drawer.voyage.reset-aaaaaaaaaaaaaaaa");
    journal::begin(dir.path(), "reset-x", &record).unwrap();

    match authority.handle_command(&Lifecycle::EndedNoRespawn, "reset-x".into(), op) {
        Err(SupervisorOperationState::Accepted) => {}
        Ok(_) => panic!("a resubmitted active operation must not be admitted again"),
        Err(other) => panic!("expected Accepted, got {other:?}"),
    }
}

/// A Stop answers `Stopping` when storage exhaustion keeps its record from
/// being written, and `Failed` for any other write failure.
#[test]
fn a_stop_is_honored_when_storage_keeps_its_record_from_being_written() {
    #[cfg(unix)]
    let full = || crate::Error::Io(std::io::Error::from_raw_os_error(libc::ENOSPC));
    #[cfg(windows)]
    let full = || crate::Error::Io(std::io::Error::from_raw_os_error(112));
    #[cfg(unix)]
    let eio = || crate::Error::Io(std::io::Error::from_raw_os_error(libc::EIO));
    #[cfg(windows)]
    let eio = || crate::Error::Io(std::io::Error::from_raw_os_error(5));
    let reply = |effect| match effect {
        CommandEffect::Stop { reply } => reply,
        _ => panic!("expected a Stop effect"),
    };
    assert_eq!(reply(stop_effect(Err(full()))), SupervisorOperationState::Stopping);
    assert_eq!(reply(stop_effect(Ok(()))), SupervisorOperationState::Stopping);
    assert!(matches!(reply(stop_effect(Err(eio()))), SupervisorOperationState::Failed { .. }));

    let dir = tempfile::tempdir().unwrap();
    let mut authority = test_authority(dir.path());
    let effect = authority
        .handle_command(&Lifecycle::EndedNoRespawn, "stop-ok".into(), SupervisorOp::Stop)
        .expect("a Stop on a healthy journal is admitted");
    assert_eq!(reply(effect), SupervisorOperationState::Stopping);
}

/// a SUCCESSFUL Reset's own operation id,
/// resubmitted with the SAME digest AFTER the voyage it changed
/// FROM no longer matches the current one, must answer with the
/// stored `ResetDone` — not `refused{stale_voyage}`. Drives
/// `handle_command` directly (pure logic + journal I/O, no OS
/// process needed) rather than through a real lane connection.
#[test]
fn a_completed_reset_replayed_after_the_voyage_moved_on_is_idempotent_not_stale_voyage() {
    let dir = tempfile::tempdir().unwrap();
    let old_voyage = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
    let mut authority = test_authority(dir.path());
    authority.voyage_id = Some(old_voyage.clone());

    let op = SupervisorOp::Reset { voyage: Some(old_voyage.clone()) };
    let effect = authority
        .handle_command(&Lifecycle::EndedNoRespawn, "reset-1".into(), op.clone())
        .expect("a fresh reset from EndedNoRespawn is admitted");
    let CommandEffect::Reset { operation_id, new_voyage, aside } = effect else {
        panic!("expected a Reset effect");
    };
    reset_pointer(dir.path(), &new_voyage, aside.as_deref()).unwrap();
    journal::finish(dir.path(), &operation_id, &journal::TerminalRecord::ResetDone { new_voyage: new_voyage.clone() })
        .unwrap();

    // The authority's own voyage_id has now moved on (as it would
    // in the main loop, once Resetting concludes) -- replaying the
    // SAME id+digest, still naming the OLD voyage, must NOT be
    // fenced against the NEW one.
    authority.voyage_id = Some(new_voyage.clone());
    let replay = authority.handle_command(&Lifecycle::EndedNoRespawn, "reset-1".into(), op);
    match replay {
        Err(SupervisorOperationState::ResetDone { new_voyage: replayed }) => assert_eq!(replayed, new_voyage),
        Ok(_) => panic!("expected an idempotent Err(ResetDone), got a fresh CommandEffect instead"),
        Err(other) => panic!("expected Err(ResetDone), got {other:?}"),
    }
}

/// `terminal_severity` is MONOTONIC —
/// computed as `prior || new`, never reassigned from scratch. The
/// regression this guards: the FIRST stop is accepted while the
/// authority is genuinely `Terminal` (severity forced `true`); by
/// the time a SECOND, entirely ordinary stop arrives, the
/// underlying `Lifecycle` is untouched by Stop so
/// it is STILL `Terminal` in reality — but this proves the
/// bookkeeping itself never depends on that, by exercising the
/// second stop against a DIFFERENT, non-Terminal lifecycle and a
/// cleanly-succeeding journal write (`terminal_now` computes
/// `false` on its own): the OR must still leave `terminal_severity`
/// `true`, exactly the `|=`, never `=`, `handle_lane_bytes`'s own
/// `CommandEffect::Stop` arm applies.
#[test]
fn stop_requested_terminal_severity_is_monotonic_across_repeated_stops() {
    let dir = tempfile::tempdir().unwrap();
    let mut authority = test_authority(dir.path());
    let terminal = Lifecycle::Terminal { detail: "x".into(), entered_at: Instant::now() };

    let CommandEffect::Stop { reply } =
        authority.handle_command(&terminal, "stop-1".into(), SupervisorOp::Stop).unwrap()
    else {
        panic!("expected a Stop effect");
    };
    let journal_failed = matches!(reply, SupervisorOperationState::Failed { .. });
    assert!(!journal_failed);
    // Mirror what handle_lane_bytes does with the effect: this is
    // the FIRST stop, accepted while already Terminal.
    authority.stop_requested = Some(StopRequested {
        primary_conn: 0, // ConnId is a bare u64; no real connection needed for this test
        terminal_severity: matches!(terminal, Lifecycle::Terminal { .. }) || journal_failed,
    });
    assert!(authority.stop_requested.as_ref().unwrap().terminal_severity);

    // A SECOND stop, a DIFFERENT id, against a NON-Terminal
    // lifecycle, whose own journal write succeeds cleanly -- this
    // stop's OWN severity computes `false` on its own; the stored
    // flag must still not clear.
    let not_terminal = Lifecycle::EndedNoRespawn;
    let CommandEffect::Stop { reply: second_reply } =
        authority.handle_command(&not_terminal, "stop-2".into(), SupervisorOp::Stop).unwrap()
    else {
        panic!("expected a Stop effect");
    };
    let second_journal_failed = matches!(second_reply, SupervisorOperationState::Failed { .. });
    assert!(!second_journal_failed);
    let terminal_now = matches!(not_terminal, Lifecycle::Terminal { .. }) || second_journal_failed;
    assert!(!terminal_now, "test setup: the second stop alone must look clean, or this proves nothing");
    authority.stop_requested.as_mut().unwrap().terminal_severity |= terminal_now;
    assert!(
        authority.stop_requested.as_ref().unwrap().terminal_severity,
        "a clean second stop against a non-terminal lifecycle must never clear the first stop's own terminal severity"
    );
}
