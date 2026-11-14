//! The rule of a person's eased scroll, with explicit instants and no window.

use super::*;

const LENGTH: Duration = Duration::from_millis(150);
const NEVER: f32 = 1.0e9;

fn at(t0: Instant, ms: u64) -> Instant { t0 + Duration::from_millis(ms) }

/// Frames every `step` ms from the ask until the motion rests; the distances spent, in order.
fn frames(m: &mut ScrollMotion, t0: Instant, step: u64, cap: f32) -> Vec<f32> {
    let mut spent = Vec::new();
    let mut ms = step;
    while m.moving() {
        spent.push(m.spend(at(t0, ms), cap));
        ms += step;
        assert!(ms < 10_000, "a motion that never rests");
    }
    spent
}

#[test]
fn an_ask_comes_to_rest_exactly_its_length_after_it_on_the_asked_total() {
    for step in [4_u64, 8, 16, 33, 100] {
        let t0 = Instant::now();
        let mut m = ScrollMotion::default();
        assert_eq!(m.ask(10.0, t0, LENGTH), 0.0);
        assert!(m.moving());
        let spent = frames(&mut m, t0, step, NEVER);
        let total: f32 = spent.iter().sum();
        assert!((total - 10.0).abs() < 1e-4, "step {step}: {total}");
        // Rest comes at the first frame at or past the deadline, never before it.
        let rest_ms = (150 + step - 1) / step * step;
        assert_eq!(spent.len() as u64, rest_ms / step, "step {step}");
        assert!(spent.iter().all(|s| *s >= 0.0), "ease-out never turns back: {spent:?}");
        assert!(!m.moving() && m.owed() == 0.0);
    }
    let t0 = Instant::now();
    let mut m = ScrollMotion::default();
    m.ask(-7.0, t0, LENGTH);
    let total: f32 = frames(&mut m, t0, 16, NEVER).iter().sum();
    assert!((total + 7.0).abs() < 1e-4, "{total}");
}

#[test]
fn the_first_frame_already_moves_and_the_steps_shrink() {
    let t0 = Instant::now();
    let mut m = ScrollMotion::default();
    m.ask(12.0, t0, LENGTH);
    let spent = frames(&mut m, t0, 8, NEVER);
    assert!(spent[0] > 1.0, "the first frame after the event moves: {spent:?}");
    assert!(spent[0] < 12.0 * 0.5, "and does not jump: {spent:?}");
    assert!(spent.windows(2).all(|w| w[0] >= w[1]), "ease-out: {spent:?}");
    // The same ask at two frame rates covers the same distance by the same instant.
    let covered = |step: u64| {
        let mut m = ScrollMotion::default();
        m.ask(12.0, t0, LENGTH);
        (1..=(72 / step)).map(|i| m.spend(at(t0, i * step), NEVER)).sum::<f32>()
    };
    assert!((covered(8) - covered(24)).abs() < 1e-3);
}

#[test]
fn a_second_ask_continues_from_where_the_view_is_and_restarts_the_length() {
    let t0 = Instant::now();
    let mut m = ScrollMotion::default();
    m.ask(10.0, t0, LENGTH);
    let first = m.spend(at(t0, 50), NEVER);
    let owed = m.owed();
    assert!((first + owed - 10.0).abs() < 1e-4);
    // The second ask adds to what is owed; the position it left is the one spent so far.
    assert_eq!(m.ask(5.0, at(t0, 60), LENGTH), 0.0);
    assert!((m.owed() - (owed + 5.0)).abs() < 1e-4);
    // The old deadline (150 ms) passes with the motion still going; the new one is 60 + 150.
    m.spend(at(t0, 160), NEVER);
    assert!(m.moving());
    m.spend(at(t0, 210), NEVER);
    assert!(!m.moving());
    // Counting every spend, the asked total is what arrives.
    let mut m = ScrollMotion::default();
    m.ask(10.0, t0, LENGTH);
    let mut sum = m.spend(at(t0, 50), NEVER);
    m.ask(5.0, at(t0, 60), LENGTH);
    let mut ms = 76;
    while m.moving() {
        sum += m.spend(at(t0, ms), NEVER);
        ms += 16;
    }
    assert!((sum - 15.0).abs() < 1e-4, "{sum}");
}

#[test]
fn what_is_owed_past_a_pane_is_spent_at_once_at_the_next_frame() {
    let t0 = Instant::now();
    let mut m = ScrollMotion::default();
    m.ask(100.0, t0, LENGTH);
    let first = m.spend(at(t0, 8), 20.0);
    assert!(first > 80.0, "the excess goes at once: {first}");
    assert!(m.owed() <= 20.0);
    let total = first + frames(&mut m, t0, 8, 20.0).iter().sum::<f32>();
    assert!((total - 100.0).abs() < 1e-3, "{total}");
    // Both directions.
    let mut m = ScrollMotion::default();
    m.ask(-100.0, t0, LENGTH);
    assert!(m.spend(at(t0, 8), 20.0) < -80.0);
}

#[test]
fn a_zero_length_applies_at_once_and_owes_nothing() {
    let t0 = Instant::now();
    let mut m = ScrollMotion::default();
    assert_eq!(m.ask(3.0, t0, Duration::ZERO), 3.0);
    assert!(!m.moving() && m.owed() == 0.0);
    assert_eq!(m.spend(at(t0, 16), NEVER), 0.0);
    // A motion already owing takes what it owes with the new ask.
    m.ask(4.0, t0, LENGTH);
    assert_eq!(m.ask(-1.0, at(t0, 10), Duration::ZERO), 3.0);
    assert!(!m.moving());
}

#[test]
fn finish_and_stop_end_the_motion_and_an_ask_that_cancels_owes_nothing() {
    let t0 = Instant::now();
    let mut m = ScrollMotion::default();
    m.ask(6.0, t0, LENGTH);
    m.spend(at(t0, 30), NEVER);
    let owed = m.owed();
    assert_eq!(m.finish(), owed);
    assert!(!m.moving() && m.owed() == 0.0);
    assert_eq!(m.finish(), 0.0);
    m.ask(6.0, t0, LENGTH);
    m.stop();
    assert!(!m.moving() && m.owed() == 0.0);
    assert_eq!(m.spend(at(t0, 40), NEVER), 0.0);
    m.ask(2.0, t0, LENGTH);
    m.ask(-2.0, at(t0, 5), LENGTH);
    assert!(!m.moving(), "an ask that undoes what was owed leaves nothing to draw");
}

#[test]
fn a_frame_at_or_before_the_ask_moves_nothing_and_a_late_one_rests() {
    let t0 = Instant::now();
    let mut m = ScrollMotion::default();
    m.ask(4.0, at(t0, 20), LENGTH);
    assert_eq!(m.spend(at(t0, 20), NEVER), 0.0);
    assert_eq!(m.spend(t0, NEVER), 0.0);
    assert!(m.moving());
    assert!((m.spend(at(t0, 5_000), NEVER) - 4.0).abs() < 1e-6);
    assert!(!m.moving());
}
