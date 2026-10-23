use super::*;

// ---- (a) QuitDispatcher --------------------------------------------

#[test]
fn quit_then_record_closed_then_record_verified_is_ended() {
    let mut q = QuitDispatcher::new();
    let t0 = Instant::now();
    assert!(q.request_quit("op-1".into(), t0));
    assert!(matches!(q.state(), QuitState::Ending { .. }));
    assert_eq!(q.message(), Some("ending session\u{2026}".to_string()));
    q.on_operation_state(SupervisorOperationState::RecordClosed);
    assert!(matches!(q.state(), QuitState::Verifying { .. }));
    assert!(!q.should_exit());
    assert_eq!(q.operation_id(), Some("op-1"));
    q.on_operation_state(SupervisorOperationState::RecordVerified);
    assert!(q.should_exit());
}

#[test]
fn record_verified_directly_from_ending_also_exits() {
    // A fast authority may verify before this dispatcher ever
    // observes record_closed as a separate step.
    let mut q = QuitDispatcher::new();
    q.request_quit("op-1".into(), Instant::now());
    q.on_operation_state(SupervisorOperationState::RecordVerified);
    assert!(q.should_exit());
}

#[test]
fn failed_and_refused_surface_immediately_never_waiting_for_the_cutoff() {
    let mut q = QuitDispatcher::new();
    let t0 = Instant::now();
    q.request_quit("op-1".into(), t0);
    q.on_operation_state(SupervisorOperationState::Failed { detail: "disk full".into() });
    assert!(!q.should_exit());
    assert_eq!(q.message(), Some("ending the session failed: disk full".to_string()));
    // Ticking well before the 90s cutoff must not overwrite this
    // already-terminal outcome with "outcome unknown".
    q.tick(t0 + Duration::from_secs(1));
    assert_eq!(q.message(), Some("ending the session failed: disk full".to_string()));

    let mut q2 = QuitDispatcher::new();
    q2.request_quit("op-2".into(), t0);
    q2.on_operation_state(SupervisorOperationState::Refused { reason: SupervisorRefusedReason::StaleVoyage });
    assert!(!q2.should_exit());
    assert!(q2.message().unwrap().contains("refused"));
}

#[test]
fn a_second_quit_press_does_not_refire_end_run() {
    let mut q = QuitDispatcher::new();
    let t0 = Instant::now();
    assert!(q.request_quit("op-1".into(), t0));
    assert!(!q.request_quit("op-2".into(), t0));
    assert!(matches!(q.state(), QuitState::Ending { operation_id } if operation_id == "op-1"));
}

#[test]
fn cutoff_is_90s_and_expiry_shows_outcome_unknown_and_never_exits() {
    let mut q = QuitDispatcher::new();
    let t0 = Instant::now();
    q.request_quit("op-1".into(), t0);
    // Well within the cutoff, even after record_closed (now
    // Verifying), ticking must not expire early.
    q.on_operation_state(SupervisorOperationState::RecordClosed);
    q.tick(t0 + Duration::from_secs(60));
    assert!(matches!(q.state(), QuitState::Verifying { .. }));
    q.tick(t0 + QUIT_CUTOFF);
    assert!(matches!(q.state(), QuitState::OutcomeUnknown));
    assert!(!q.should_exit());
    assert_eq!(
        q.message(),
        Some("ending the session did not complete \u{2014} outcome unknown".to_string())
    );
    // A record_verified arriving late (after the window already
    // gave up) must not resurrect it into Ended -- the ADR's window
    // stays in "outcome unknown", it does not flip back.
    q.on_operation_state(SupervisorOperationState::RecordVerified);
    assert!(!q.should_exit());
}

#[test]
fn idle_and_ended_show_no_message() {
    let q = QuitDispatcher::new();
    assert_eq!(q.message(), None);
    let mut q2 = QuitDispatcher::new();
    q2.request_quit("op".into(), Instant::now());
    q2.on_operation_state(SupervisorOperationState::RecordVerified);
    assert_eq!(q2.message(), None);
}
