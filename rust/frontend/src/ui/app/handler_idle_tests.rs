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

#[test]
fn quit_hint_deadlines_wake_and_expire_without_taking_input() {
    use crate::ui::{FocusPrompts, PaneFocus, Key};
    use crate::ui::input::keypress::{input_continuation, InputRoute};
    let start = Instant::now();
    let mut p = FocusPrompts::new(PaneFocus::Preview);
    p.show_hint("Fixture+Q", || start);
    assert_eq!(p.hint_deadline(), Some(start + Duration::from_secs(4)));
    p.show_hint("Replacement+Q", || start + Duration::from_secs(1));
    let until = start + Duration::from_secs(5);
    assert_eq!(p.hint_deadline(), Some(until));
    let hint = p.hint().unwrap().to_string();
    assert!(hint.starts_with("Replacement+Q "));
    let mut slot = None;
    assert_eq!(arm_idle(&mut slot, start, Duration::from_secs(20), p.hint_deadline()), until);
    assert!(!p.expire_hint(until - Duration::from_nanos(1)));
    p.set_focus(PaneFocus::Llm);
    let keep = p.quit_choice();
    let mut seen = (p, 0);
    input_continuation(&mut seen, &Key::Character("x".into()), false, false, true, keep, false,
        |_| (None, ()), |s, route| match route {
            InputRoute::Next(_, ()) => s.1 += 1,
            InputRoute::Prompt(_) => panic!("a hint took input"),
        });
    assert_eq!(seen.1, 1);
    assert_eq!(seen.0.hint(), Some(hint.as_str()));
    assert_eq!(seen.0.hint_deadline(), Some(until));
    assert!(seen.0.expire_hint(until));
    assert!(seen.0.hint().is_none() && seen.0.hint_deadline().is_none());
    assert!(!seen.0.expire_hint(until + Duration::from_secs(1)));
    seen.0.show_hint("Again+Q", || start);
    assert!(seen.0.expire_hint(start + Duration::from_secs(6)));
}
