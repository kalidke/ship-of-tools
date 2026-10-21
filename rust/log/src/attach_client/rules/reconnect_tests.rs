use super::*;

// ---- (d) ReconnectState ---------------------------------------------

#[test]
fn backoff_doubles_from_a_fresh_state_and_only_a_stable_session_restarts_it() {
    // ADR 0043 decision 28: every failed dial doubles, whether or not the
    // row has ever attached — over an ssh lane each dial is a login.
    let mut r = ReconnectState::new();
    let waits: Vec<Duration> = (0..6).map(|_| r.retry_after(Duration::ZERO)).collect();
    assert_eq!(
        waits,
        vec![
            Duration::from_millis(250),
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(4),
        ]
    );
    let short = crate::host::redial::STABLE - Duration::from_secs(1);
    assert_eq!(r.retry_after(short), Duration::from_secs(4), "an attach that drops sooner keeps the doubling");
    assert_eq!(r.retry_after(crate::host::redial::STABLE), Duration::from_millis(250));
}

#[test]
fn post_attach_backoff_doubles_and_caps_at_4s() {
    let mut r = ReconnectState::new();
    let mut waits = vec![r.retry_after(crate::host::redial::STABLE)];
    for _ in 0..5 {
        waits.push(r.retry_after(Duration::ZERO));
    }
    assert_eq!(
        waits,
        vec![
            Duration::from_millis(250),
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(4),
        ]
    );
}

#[test]
fn terminal_cases_are_immediately_terminal() {
    let mut r = ReconnectState::new();
    assert_eq!(
        r.classify_hello_refused_version_skew(),
        ReconnectDecision::Terminal(TerminalReason::HelloRefusedVersionSkew)
    );
    assert_eq!(r.classify_foreign(), ReconnectDecision::Terminal(TerminalReason::ForeignPipe));
    assert_eq!(
        r.classify_access_denied(),
        ReconnectDecision::Terminal(TerminalReason::AccessDenied)
    );
    assert_eq!(
        r.classify_operator_cancel(),
        ReconnectDecision::Terminal(TerminalReason::OperatorCancel)
    );
}

#[test]
fn supervisor_ended_no_respawn_and_terminal_are_terminal_ready_is_not() {
    let mut r = ReconnectState::new();
    assert_eq!(
        r.classify_supervisor_phase(SupervisorPhase::EndedNoRespawn),
        ReconnectDecision::Terminal(TerminalReason::SupervisorPhase(SupervisorPhase::EndedNoRespawn))
    );
    assert_eq!(
        r.classify_supervisor_phase(SupervisorPhase::Terminal),
        ReconnectDecision::Terminal(TerminalReason::SupervisorPhase(SupervisorPhase::Terminal))
    );
    assert_eq!(r.classify_supervisor_phase(SupervisorPhase::Ready), ReconnectDecision::Retry);
    assert_eq!(r.classify_supervisor_phase(SupervisorPhase::Starting), ReconnectDecision::Retry);
    assert_eq!(r.classify_supervisor_phase(SupervisorPhase::Ending), ReconnectDecision::Retry);
}

#[test]
fn unresponsive_retries_until_the_health_window_then_goes_terminal() {
    let mut r = ReconnectState::new();
    let t0 = Instant::now();
    assert_eq!(r.classify_unresponsive(t0), ReconnectDecision::Retry);
    assert_eq!(r.classify_unresponsive(t0 + Duration::from_secs(60)), ReconnectDecision::Retry);
    assert_eq!(
        r.classify_unresponsive(t0 + HEALTH_WINDOW),
        ReconnectDecision::Terminal(TerminalReason::HealthWindowExpired)
    );
}

#[test]
fn clearing_unresponsive_starts_a_fresh_window_next_time() {
    let mut r = ReconnectState::new();
    let t0 = Instant::now();
    r.classify_unresponsive(t0);
    r.clear_unresponsive();
    // A LATER unresponsive spell, well past the first window's own
    // deadline, must not inherit the old clock.
    let t1 = t0 + HEALTH_WINDOW + Duration::from_secs(1);
    assert_eq!(r.classify_unresponsive(t1), ReconnectDecision::Retry);
}
