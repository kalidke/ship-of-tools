use super::{arm_idle, earliest, has_passed};
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
        |_| (None, ()), |_| false, |s, route| match route {
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

#[test]
fn peek_deadlines_wake_fade_and_expire_without_postponement() {
    use crate::ui::{DrawerContent, PaneFocus, FocusPrompts};
    use crate::ui::input::{help, layout_help::LayoutHelp};
    let start = Instant::now();
    let mut layout = LayoutHelp::new(DrawerContent::Closed, false, false, false);
    layout.start_peek(help::Context::default(), start);
    let fade = start + Duration::from_secs(5);
    let fade_span = Duration::from_millis(240);
    assert_eq!(layout.peek_deadline(), Some(fade));
    let mut focus = FocusPrompts::new(PaneFocus::Preview);
    focus.show_hint("Fixture+Q", || start);
    let deadline = || layout.peek_deadline().into_iter().chain(focus.hint_deadline()).min();
    let mut slot = None;
    assert_eq!(arm_idle(&mut slot, start, Duration::from_secs(20), deadline()), start + Duration::from_secs(4));
    let mut slot = None;
    for n in 0..100 {
        let now = start + Duration::from_millis(40) * n;
        assert_eq!(arm_idle(&mut slot, now, Duration::from_secs(20), layout.peek_deadline()), fade);
    }
    assert!(!layout.peek_fading(|| fade - Duration::from_nanos(1)));
    assert!(layout.peek_fading(|| fade));
    assert_eq!(super::idle_interval(false, false), Duration::from_secs(1));
    assert_eq!(super::idle_interval(false, true), Duration::from_millis(80));
    for flashes in [false, true] {
        let interval = super::idle_interval(layout.peek_fading(|| fade), flashes);
        assert_eq!(interval, Duration::from_millis(16));
        assert_eq!(arm_idle(&mut slot, fade, interval, layout.peek_deadline()), fade + interval);
    }
    assert_eq!(layout.peek().unwrap().opacity(fade), 1.0);
    assert!((layout.peek().unwrap().opacity(fade + fade_span / 2) - 0.5).abs() < 0.01);
    assert!(!layout.expire_peek(|| fade + fade_span - Duration::from_millis(1)));
    assert!(layout.expire_peek(|| fade + fade_span));
    assert!(layout.peek_deadline().is_none());
    assert!(!layout.expire_peek(|| panic!("absent peek sampled the clock")));
}

/// The chat's next ask joins the idle deadlines: it neither delays nor drops an earlier one, and an earlier ask is the one
/// the loop sleeps to.
#[test]
fn the_chats_next_ask_joins_the_idle_deadlines() {
    let start = Instant::now();
    let second = Duration::from_secs(1);
    let at = |ms: u64| start + Duration::from_millis(ms);
    let arm = |deadlines: [Option<Instant>; 4]| arm_idle(&mut None, start, second, earliest(deadlines));
    // Help peek 100 ms, hint 300 ms, reply bar 200 ms, and the chat at 250 ms: the peek still wakes the loop first.
    assert_eq!(arm([Some(at(100)), Some(at(300)), Some(at(200)), Some(at(250))]), at(100), "an earlier deadline is not delayed");
    assert_eq!(arm([None, Some(at(300)), Some(at(200)), Some(at(250))]), at(200), "nor is one dropped for the chat's");
    // The chat's ask at 250 ms is the earliest: the loop sleeps to it, and to nothing later.
    assert_eq!(arm([None, Some(at(300)), None, Some(at(250))]), at(250), "an earlier chat ask is honored");
    assert_eq!(arm([None, None, None, Some(at(250))]), at(250), "alone it wakes the idle loop before its second");
    assert_eq!(arm([None, None, None, None]), start + second);
    // An ask already past is not slept to.
    assert_eq!(arm_idle(&mut None, at(500), second, earliest([None, None, None, Some(at(250))])), at(500) + second);
}

/// An ask whose instant passed before the loop went to sleep is not slept to (`arm_idle` takes only a deadline ahead), so it
/// is a redraw now.
#[test]
fn an_ask_that_has_passed_is_a_redraw_now() {
    let start = Instant::now();
    let at = start + Duration::from_millis(250);
    assert!(!has_passed(None, at));
    assert!(!has_passed(Some(at), at - Duration::from_millis(1)), "not yet");
    assert!(has_passed(Some(at), at), "at its instant");
    assert!(has_passed(Some(at), at + Duration::from_millis(1)), "after it");
}
