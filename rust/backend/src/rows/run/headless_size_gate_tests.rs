// Size-gate tests and attach-failure tests against a state dir with no
// lane, and the enter-outcome mapping.
#[cfg(target_os = "linux")]
use super::screen_of;
use super::{enter_outcome, type_into, write_and_enter, HeadlessError};
use sot_protocol::PtyEnter;
use std::path::Path;
use std::time::{Duration, Instant};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

fn oversized_detail() -> String {
    let cap = sot_log::attach_client::rules::TAKE_QUEUE_CAP;
    format!("payload is {} bytes, exceeding the take queue cap of {} bytes", cap + 1, cap)
}

#[test]
fn enter_outcome_tells_unknown_from_not_sent() {
    let stale = HeadlessError { phase: "input", detail: "stale".into(), submitted: true };
    let unknown = HeadlessError { phase: "record", detail: "unknown".into(), submitted: true };
    let take = HeadlessError { phase: "take", detail: "dead".into(), submitted: true };
    assert_eq!(enter_outcome(Ok(())), PtyEnter::Sent);
    assert_eq!(enter_outcome(Err(stale)), PtyEnter::NotSent);
    assert_eq!(enter_outcome(Err(unknown)), PtyEnter::Unknown);
    assert_eq!(enter_outcome(Err(take)), PtyEnter::Unknown);
}

#[test]
fn oversized_payload_is_refused_before_any_attach() {
    let bytes = vec![b'x'; sot_log::attach_client::rules::TAKE_QUEUE_CAP + 1];
    let err = type_into(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &bytes, deadline())
        .expect_err("oversized payload must be refused");
    assert_eq!(err.phase, "size");
    assert!(!err.submitted);
    assert_eq!(err.detail, oversized_detail());
}

#[test]
fn exactly_the_cap_is_not_oversized() {
    // The cap itself is legal — only `CAP + 1` is refused. This
    // would attempt a real attach (and fail on the nonexistent path
    // some other way), which is enough to prove the size gate did
    // NOT reject it — that failure is expected and not asserted on
    // further than "it is not the size-gate error."
    let bytes = vec![b'x'; sot_log::attach_client::rules::TAKE_QUEUE_CAP];
    let err = type_into(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &bytes, deadline())
        .expect_err("a nonexistent state dir cannot succeed");
    assert_ne!(err.phase, "size", "the cap itself must not trip the size gate");
}

#[test]
fn empty_payload_succeeds_with_no_attach() {
    let n = type_into(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &[], deadline())
        .expect("an empty payload succeeds trivially, with nothing to attach for");
    assert_eq!(n, 0);
}

#[test]
fn write_and_enter_oversized_payload_is_refused_before_any_attach() {
    let bytes = vec![b'x'; sot_log::attach_client::rules::TAKE_QUEUE_CAP + 1];
    let budget = Duration::from_secs(5);
    let err = write_and_enter(Path::new("/nonexistent/sot-lu6c-test-state-dir"), "ctrl", &bytes, budget, budget, budget)
        .expect_err("oversized payload must be refused");
    assert_eq!(err.phase, "size");
    assert!(!err.submitted);
    assert_eq!(err.detail, oversized_detail());
}

// Observed on Linux only: the attach client's worker is the same code elsewhere, but no host here runs it.
#[cfg(target_os = "linux")]
#[test]
fn an_attach_without_a_lane_is_phase_checkpoint() {
    let dir = Path::new("/nonexistent/sot-lu6c-test-state-dir");
    let budget = Duration::from_secs(5);
    let errs = [
        type_into(dir, "ctrl", b"x", deadline()).expect_err("no lane to type into"),
        write_and_enter(dir, "ctrl", b"x", budget, budget, budget).expect_err("no lane to write to"),
        screen_of(dir, "ctrl", deadline()).map(|_| ()).expect_err("no lane to read"),
    ];
    for err in errs {
        assert_eq!(err.phase, "checkpoint");
        assert!(!err.submitted);
    }
}
