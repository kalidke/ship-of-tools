use super::*;
use std::time::{Duration, Instant};

/// The review's sequence: a person works in A, clicks into B, comes back to A and types. The daemon stamps each
/// report as it arrives; the window last stamped must be A once the held report goes out.
#[test]
fn a_throttled_report_is_sent_by_the_tick_so_the_window_in_use_wins() {
    let t0 = Instant::now();
    let at = |secs| t0 + Duration::from_secs(secs);
    let (mut a, mut b) = (PresenceClock::default(), PresenceClock::default());
    assert!(!presence_due(&mut a, true, at(58)));
    assert!(presence_due(&mut a, true, at(60)));
    assert!(!presence_due(&mut b, true, at(60)));
    assert!(presence_due(&mut b, true, at(61)));
    let stamp_b = 61;
    for t in [62, 63] {
        assert!(!presence_due(&mut a, true, at(t)), "throttled");
    }
    assert!(!presence_held_due(&mut a, at(64)), "the throttle has not passed");
    assert!(presence_held_due(&mut a, at(65)), "the held report goes out on the tick");
    let stamp_a = 65;
    assert!(!presence_held_due(&mut a, at(66)), "once");
    assert!(!presence_held_due(&mut b, at(66)), "a window with nothing held sends nothing");
    assert!(stamp_a > stamp_b, "an open at 120 s goes to A");
}

#[test]
fn a_lone_press_or_a_release_reports_nothing_a_second_press_reports_a_person() {
    let t0 = Instant::now();
    let at = |secs| t0 + Duration::from_secs(secs);
    let mut clock = PresenceClock::default();
    assert!(!presence_due(&mut clock, false, at(0)), "a release is not a press");
    assert!(!presence_due(&mut clock, true, at(1)), "one stray press reports nothing");
    assert!(!presence_due(&mut clock, false, at(2)), "nor does its release");
    assert!(presence_due(&mut clock, true, at(3)), "a second press within five minutes reports a person");
    assert!(!presence_due(&mut clock, true, at(4)), "throttled");
    assert!(presence_due(&mut clock, true, at(8)), "past the throttle");
    assert!(!presence_due(&mut clock, true, at(8 + 301)), "after five idle minutes a press is lone again");
    assert!(presence_due(&mut clock, true, at(8 + 302)), "and the next one reports");
}
