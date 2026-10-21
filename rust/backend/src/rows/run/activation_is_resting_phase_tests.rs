use super::*;

#[test]
fn resting_phases_never_wait_regardless_of_watchdog() {
    for phase in [NEVER_STARTED_PHASE, FOREIGN_PHASE, "ready", "ended_no_respawn", "terminal"] {
        assert!(is_resting_phase(phase, true), "{phase} must be resting with a watchdog");
        assert!(is_resting_phase(phase, false), "{phase} must be resting with no watchdog");
    }
}

#[test]
fn unreachable_rests_only_with_no_watchdog() {
    assert!(is_resting_phase(UNREACHABLE_PHASE, false), "nobody else will ever act -- this caller must");
    assert!(!is_resting_phase(UNREACHABLE_PHASE, true), "a live watchdog owns the restart -- wait for it");
}

#[test]
fn transient_phases_always_wait_even_with_no_watchdog() {
    // Codex review round 6/7 BLOCKERs: a settle timeout (the
    // watchdog's own, OR this call's own fresh spawn) can return
    // "starting" -- this must NEVER be treated as a decision
    // point, watchdog or not (an adopted authority with no
    // watchdog at all is still mid-flight here too).
    for phase in ["starting", "ending"] {
        assert!(!is_resting_phase(phase, true), "{phase} is transient even with a watchdog");
        assert!(!is_resting_phase(phase, false), "{phase} is transient even with no watchdog");
    }
}
