use super::arm_idle;
use std::time::{Duration, Instant};

/// One slot, driven through the wakes `about_to_wait` sees: the armed repaint does not move later.
#[test]
fn idle_repaint_deadline_is_not_postponed_by_wakes() {
    let start = Instant::now();
    let second = Duration::from_secs(1);
    let mut slot = None;
    let armed = arm_idle(&mut slot, start, second, None);
    assert_eq!(armed, start + second);
    // A wake every 50 ms leaves the armed deadline where it is.
    for n in 1..20u32 {
        let now = start + Duration::from_millis(50) * n;
        assert_eq!(arm_idle(&mut slot, now, second, None), armed, "a wake at {n} x 50 ms moved the idle repaint");
    }
    // A shorter cadence (a fade) and a coming help fade bring it forward; a passed deadline is armed again.
    let fade = arm_idle(&mut slot, start + Duration::from_millis(100), Duration::from_millis(80), None);
    assert_eq!(fade, start + Duration::from_millis(180));
    let peek = arm_idle(&mut slot, start + Duration::from_millis(120), second, Some(start + Duration::from_millis(150)));
    assert_eq!(peek, start + Duration::from_millis(150));
    let late = start + Duration::from_millis(1500);
    assert_eq!(arm_idle(&mut slot, late, second, Some(start)), late + second);
    assert_eq!(slot, Some(late + second), "the slot holds the deadline it returned");
}
