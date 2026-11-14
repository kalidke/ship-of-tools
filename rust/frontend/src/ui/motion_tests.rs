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

#[test]
fn the_curve_is_a_cubic_ease_out() {
    let t0 = Instant::now();
    let mut m = ScrollMotion::default();
    m.ask(8.0, t0, LENGTH);
    // Halfway through the time, one eighth of the distance is left: 8 * (1 - 0.5^3).
    assert!((m.spend(at(t0, 75), NEVER) - 7.0).abs() < 1e-4);
    assert!((m.owed() - 1.0).abs() < 1e-4);
}

/// Frames every `step` ms from the ask until the motion rests, in whole rows: what each frame shows.
fn row_frames(m: &mut ScrollMotion, t0: Instant, step: u64, cap: f32) -> Vec<i32> {
    let mut shown = Vec::new();
    let mut ms = step;
    while m.moving() {
        shown.push(m.spend_rows(at(t0, ms), cap));
        ms += step;
        assert!(ms < 10_000, "a motion that never rests");
    }
    shown
}

#[test]
fn whole_rows_move_the_first_row_at_once_and_end_with_the_last() {
    for (asked, step) in [(8_i32, 8_u64), (8, 16), (8, 33), (1, 8), (3, 100), (-8, 8), (-1, 16), (-5, 33)] {
        let t0 = Instant::now();
        let mut m = ScrollMotion::default();
        assert_eq!(m.ask_rows(asked, t0, LENGTH), 0, "nothing at the event itself");
        let shown = row_frames(&mut m, t0, step, NEVER);
        assert_eq!(shown.iter().sum::<i32>(), asked, "{asked} at {step} ms: {shown:?}");
        // The first row comes at the first frame, in the direction of travel, and no frame turns back.
        assert_eq!(shown[0].signum(), asked.signum(), "{asked} at {step} ms: {shown:?}");
        assert!(shown.iter().all(|r| r.signum() != -asked.signum()), "{shown:?}");
        // The motion ends with the frame that shows its last row: no frame is drawn for a roll with nothing more to
        // show, and none comes later than the length allows.
        assert!(shown.last().is_some_and(|r| *r != 0), "{asked} at {step} ms: {shown:?}");
        assert!(shown.len() as u64 <= (150 + step - 1) / step, "{asked} at {step} ms: {shown:?}");
        assert!(!m.moving() && m.owed() == 0.0);
    }
    // A one-row step is the whole of its first frame, and nothing is drawn after it.
    let t0 = Instant::now();
    let mut m = ScrollMotion::default();
    m.ask_rows(1, t0, LENGTH);
    assert_eq!(m.spend_rows(at(t0, 8), NEVER), 1);
    assert!(!m.moving() && m.rows_owed() == 0);
    assert_eq!(m.spend_rows(at(t0, 16), NEVER), 0);
    // The cap spends a pane's excess at once, in rows, and the total still lands.
    let mut m = ScrollMotion::default();
    m.ask_rows(-100, t0, LENGTH);
    let first = m.spend_rows(at(t0, 8), 20.0);
    assert!(first < -80, "{first}");
    assert_eq!(first + row_frames(&mut m, t0, 8, 20.0).iter().sum::<i32>(), -100);
}

#[test]
fn a_zero_length_in_rows_applies_at_once_and_stop_drops_the_carry() {
    let t0 = Instant::now();
    let mut m = ScrollMotion::default();
    assert_eq!(m.ask_rows(3, t0, Duration::ZERO), 3);
    assert!(!m.moving());
    // A motion already owing, with a row shown ahead of the curve, hands over exactly what is left of the total.
    m.ask_rows(5, t0, LENGTH);
    let shown = m.spend_rows(at(t0, 8), NEVER);
    assert_eq!(shown + m.ask_rows(2, at(t0, 9), Duration::ZERO), 7);
    assert!(!m.moving());
    // A stop forgets the row shown ahead, so the next motion counts from its own ask.
    m.ask_rows(5, t0, LENGTH);
    m.spend_rows(at(t0, 8), NEVER);
    m.stop();
    m.ask_rows(4, at(t0, 200), LENGTH);
    assert_eq!(row_frames(&mut m, at(t0, 200), 16, NEVER).iter().sum::<i32>(), 4);
    assert_eq!(m.finish_rows(), 0);
}
