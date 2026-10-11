//! Display-free behavioral proofs of the paths used by the native drivers.

use super::*;

#[test]
#[cfg(feature = "test-pane-timing")]
fn pane_child_layout_has_agent_at_any_monitor_shape() {
    use crate::ui::persist::settings::Slot;
    let settings = super::super::native_pane_tests::pane_child_settings();
    for aspect in [4.0 / 3.0, 0.75, 1.5, 1.6] {
        assert!(settings.resolve_preset(aspect).columns.contains(&Slot::Llm),
            "pane child has no agent column at aspect {aspect}");
    }
    assert!(!Settings::default().resolve_preset(0.75).columns.contains(&Slot::Llm));
}

#[test]
fn native_drivers_ignore_unmarked_wakes() {
    use super::super::native_children::HarnessTick;
    for driver in ["progress", "close", "pane"] {
        let tick = HarnessTick::default();
        let mut trace = Vec::new();
        for _ in 0..100 {
            tick.dispatch(&mut trace, |t| t.push("forward"), |t| t.push("work"));
        }
        assert_eq!(trace.len(), 100, "{driver}: unmarked wakes advanced harness work");
        tick.post();
        tick.dispatch(&mut trace, |t| t.push("forward"), |t| t.push("work"));
        assert_eq!(&trace[100..], &["forward", "work"]);
        for _ in 0..10 {
            tick.dispatch(&mut trace, |t| t.push("forward"), |t| t.push("work"));
        }
        assert_eq!(trace.iter().filter(|&&v| v == "work").count(), 1);
        if driver == "pane" {
            tick.post();
            for _ in ["window-event", "about-to-wait"] {
                HarnessTick::forward(&mut trace, |t| t.push("forward"));
            }
            assert_eq!(trace.iter().filter(|&&v| v == "work").count(), 1,
                "pane forwarding callbacks advanced the harness");
            tick.dispatch(&mut trace, |_| {}, |t| t.push("work"));
            assert_eq!(trace.iter().filter(|&&v| v == "work").count(), 2);
        }
    }
}

#[test]
fn pending_tick_is_consumed_once() {
    use super::super::native_children::HarnessTick;
    let tick = HarnessTick::default();
    let mut work = 0;
    for _ in 0..3 { tick.post(); }
    tick.dispatch(&mut work, |_| {}, |w| *w += 1);
    tick.dispatch(&mut work, |_| {}, |w| *w += 1);
    assert_eq!(work, 1, "coalesced tick advanced twice");
    tick.post();
    tick.dispatch(&mut work, |_| {}, |w| { *w += 1; tick.post(); });
    tick.dispatch(&mut work, |_| {}, |w| *w += 1);
    tick.dispatch(&mut work, |_| {}, |w| *w += 1);
    assert_eq!(work, 3, "post during consumed work was lost or duplicated");
}
