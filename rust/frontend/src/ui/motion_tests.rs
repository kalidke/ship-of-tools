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

/// Frames every `step` from the ask until the motion rests, in whole rows: what each frame shows.
fn row_frames(m: &mut ScrollMotion, t0: Instant, step: Duration, cap: f32) -> Vec<i32> {
    let mut shown = Vec::new();
    let mut i = 1;
    while m.moving() {
        shown.push(m.spend_rows(t0 + step * i, cap));
        i += 1;
        assert!(step * i < Duration::from_secs(10), "a motion that never rests");
    }
    shown
}

/// One frame's period at `hz` frames a second.
fn per(hz: u32) -> Duration { Duration::from_nanos(1_000_000_000 / hz as u64) }

#[test]
fn whole_rows_move_the_first_row_at_once_and_end_with_the_last() {
    for (asked, step) in [(8_i32, 8_u64), (8, 16), (8, 33), (1, 8), (3, 100), (-8, 8), (-1, 16), (-5, 33)] {
        let t0 = Instant::now();
        let mut m = ScrollMotion::default();
        assert_eq!(m.ask_rows(asked, t0, LENGTH), 0, "nothing at the event itself");
        let shown = row_frames(&mut m, t0, Duration::from_millis(step), NEVER);
        assert_eq!(shown.iter().sum::<i32>(), asked, "{asked} at {step} ms: {shown:?}");
        // The first row comes at the first frame, in the direction of travel, and no frame turns back.
        assert_eq!(shown[0].signum(), asked.signum(), "{asked} at {step} ms: {shown:?}");
        assert!(shown.iter().all(|r| r.signum() != -asked.signum()), "{shown:?}");
        // The motion ends with the frame that shows its last row: no frame is drawn for a roll with nothing more to
        // show, and none comes later than the length allows (stretched to 25 ms a row up to 600 ms, so the roll starts
        // no faster than a row a frame at 60 frames a second).
        assert!(shown.last().is_some_and(|r| *r != 0), "{asked} at {step} ms: {shown:?}");
        let length_ms = 150.max((asked.unsigned_abs() as u64 * 25).min(600));
        assert!(shown.len() as u64 <= (length_ms + step - 1) / step, "{asked} at {step} ms: {shown:?}");
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
    assert_eq!(first + row_frames(&mut m, t0, Duration::from_millis(8), 20.0).iter().sum::<i32>(), -100);
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
    assert_eq!(row_frames(&mut m, at(t0, 200), Duration::from_millis(16), NEVER).iter().sum::<i32>(), 4);
    assert_eq!(m.finish_rows(), 0);
}

#[test]
fn a_page_rolls_about_a_row_a_frame_from_one_row_to_rest() {
    // A third of a 45-row pane, its half and a third of a 135-row pane, each way, at 60, 120 and 144 frames a second.
    let pages = [(15_i32, 45.0, 60_u32), (22, 45.0, 60), (45, 135.0, 60), (-15, 45.0, 60), (-45, 135.0, 60), (15, 45.0, 120),
        (45, 135.0, 144)];
    for (page, cap, hz) in pages {
        let t0 = Instant::now();
        let mut m = ScrollMotion::default();
        assert_eq!(m.ask_rows(page, t0, LENGTH), 0);
        let shown = row_frames(&mut m, t0, per(hz), cap);
        assert_eq!(shown.iter().sum::<i32>(), page, "{page} at {hz}: {shown:?}");
        // The first frame shows one row while the pace fits under the ceiling (24 rows); a longer page, at most two.
        let first = if page.abs() <= 24 { 1 } else { 2 };
        assert!(shown[0].signum() == page.signum() && shown[0].abs() <= first, "{page} at {hz}: {shown:?}");
        assert!(shown.iter().all(|r| r.abs() <= 2 && r.signum() != -page.signum()), "{page} at {hz}: {shown:?}");
        // About a row a frame: no fewer frames than rows at 60 a second, or half a second's under the ceiling, and it
        // eases to rest (the later half of the frames shows no more rows than the earlier).
        assert!(shown.len() as u32 * 60 / hz >= page.unsigned_abs().min(30), "{page} at {hz}: {shown:?}");
        let (early, late) = shown.split_at(shown.len() / 2);
        assert!(early.iter().sum::<i32>().abs() >= late.iter().sum::<i32>().abs(), "{page} at {hz}: {shown:?}");
        assert!(shown.last().is_some_and(|r| *r != 0) && !m.moving() && m.owed() == 0.0);
    }
    // A second page asked while the first rolls keeps the pace.
    let t0 = Instant::now();
    let mut m = ScrollMotion::default();
    m.ask_rows(15, t0, LENGTH);
    let mut shown: Vec<i32> = (1..=6).map(|i| m.spend_rows(t0 + per(60) * i, 45.0)).collect();
    let t1 = t0 + per(60) * 6 + Duration::from_millis(1);
    m.ask_rows(15, t1, LENGTH);
    shown.extend(row_frames(&mut m, t1, per(60), 45.0));
    assert_eq!(shown.iter().sum::<i32>(), 30, "{shown:?}");
    assert!(shown.iter().all(|r| (0..=2).contains(r)), "{shown:?}");
}

#[test]
fn a_wheel_tick_and_a_one_row_step_roll_as_quick_as_before() {
    for (asked, want) in [(3_i32, vec![1, 1, 1]), (-3, vec![-1, -1, -1]), (1, vec![1]), (-1, vec![-1])] {
        let t0 = Instant::now();
        let mut m = ScrollMotion::default();
        m.ask_rows(asked, t0, LENGTH);
        assert_eq!(row_frames(&mut m, t0, per(60), 45.0), want, "{asked}");
    }
}

#[test]
fn a_page_with_a_zero_length_moves_at_once() {
    for page in [15_i32, -15, 45] {
        let t0 = Instant::now();
        let mut m = ScrollMotion::default();
        assert_eq!(m.ask_rows(page, t0, Duration::ZERO), page);
        assert!(!m.moving() && m.rows_owed() == 0);
    }
}

#[test]
fn a_held_page_key_or_a_burst_comes_to_rest_within_the_ceiling_after_its_last_ask() {
    // (rows asked, how many asks, ms between them, the pane): Page Down held 2 s at a 31 Hz repeat in a 135-row and
    // a 200-row pane, 40 wheel ticks in 600 ms, 100 ticks queued behind one stalled frame, a trackpad fling.
    for (rows, asks, gap_ms, cap) in [(45_i32, 62_u64, 32_u64, 135.0), (66, 62, 32, 200.0), (3, 40, 15, 90.0),
        (3, 100, 0, 45.0), (-150, 1, 0, 45.0)] {
        let t0 = Instant::now();
        let mut m = ScrollMotion::default();
        let (mut shown, mut asked, mut rest) = (0, 0, t0);
        let last_ask = at(t0, (asks - 1) * gap_ms);
        let mut i = 1;
        while asked < asks || m.moving() {
            let frame = t0 + per(60) * i;
            while asked < asks && at(t0, asked * gap_ms) <= frame {
                shown += m.ask_rows(rows, at(t0, asked * gap_ms), LENGTH);
                asked += 1;
            }
            let r = m.spend_rows(frame, cap);
            if r != 0 { rest = frame; }
            shown += r;
            i += 1;
            assert!(i < 100_000, "a roll that never rests");
        }
        assert_eq!(shown, rows * asks as i32, "{rows} x {asks}");
        let after = rest.saturating_duration_since(last_ask);
        assert!(after <= Duration::from_millis(600) + per(60) * 2, "{rows} x {asks}: rest {after:?} after the last ask");
    }
}

#[test]
fn the_whole_row_curve_is_the_same_cubic_at_every_frame_rate() {
    // Spent in floats up to 40% of the length, at four frame rates and one irregular clock: what is spent is the
    // cubic from the ask's start speed to rest, `rows * (1 - w^2 (1 + (2 - start) (1 - w)))` with `w` the time left.
    let irregular = [16_u64, 17, 33, 8, 16, 50, 4, 17, 16, 25];
    for rows in [45_i32, 15, 6, 4, -15] {
        let length = LENGTH.max(Duration::from_millis(25 * rows.unsigned_abs() as u64).min(Duration::from_millis(600)));
        let start = (60.0 * length.as_secs_f32() / rows.abs() as f32).clamp(1.5, 3.0);
        let w = 0.6_f32;
        let want = rows as f32 * (1.0 - w * w * (1.0 + (2.0 - start) * (1.0 - w)));
        let probe = length.mul_f32(1.0 - w);
        for clock in [30_u32, 60, 120, 144, 0] {
            let t0 = Instant::now();
            let mut m = ScrollMotion::default();
            m.ask_rows(rows, t0, LENGTH);
            let (mut spent, mut now, mut k) = (0.0, Duration::ZERO, 0);
            loop {
                now += if clock == 0 { Duration::from_millis(irregular[k % irregular.len()]) } else { per(clock) };
                k += 1;
                if now >= probe { break; }
                spent += m.spend(t0 + now, NEVER);
            }
            spent += m.spend(t0 + probe, NEVER);
            assert!((spent - want).abs() < 1e-3, "{rows} at {clock}: {spent} against {want}");
        }
    }
}

/// The pixel panes' spend as it was before the whole-row pace: the cubic `owed * left^3`.
#[derive(Default)]
struct OldCubic { owed: f32, deadline: Option<Instant>, last: Option<Instant> }

impl OldCubic {
    fn ask(&mut self, delta: f32, now: Instant, length: Duration) {
        self.owed += delta;
        if self.owed == 0.0 {
            (self.owed, self.deadline) = (0.0, None);
        } else {
            (self.deadline, self.last) = (Some(now + length), Some(now));
        }
    }

    fn spend(&mut self, now: Instant, cap: f32) -> f32 {
        let Some(deadline) = self.deadline else { return 0.0 };
        let cap = cap.max(0.0);
        let mut spent = 0.0;
        if self.owed.abs() > cap {
            spent = self.owed - self.owed.signum() * cap;
            self.owed -= spent;
        }
        if now >= deadline {
            let owed = std::mem::take(&mut self.owed);
            self.deadline = None;
            return spent + owed;
        }
        let last = self.last.unwrap_or(now).min(now);
        let left = (deadline - now).as_secs_f32() / (deadline - last).as_secs_f32();
        let keep = self.owed * left * left * left;
        spent += self.owed - keep;
        self.owed = keep;
        self.last = Some(now);
        spent
    }
}

#[test]
fn a_pixel_spend_is_the_old_cubic_to_the_bit() {
    let irregular = [16_u64, 17, 33, 8, 16, 50, 4, 17, 16, 25];
    for asked in [0.37_f32, 3.0, 12.0, 100.0, 777.7, 12_345.0, -55.5] {
        for length in [90_u64, 150, 1000] {
            for cap in [NEVER, 40.0] {
                for clock in [7_u64, 16, 33, 0] {
                    let t0 = Instant::now();
                    let length = Duration::from_millis(length);
                    let (mut m, mut old) = (ScrollMotion::default(), OldCubic::default());
                    m.ask(asked, t0, length);
                    old.ask(asked, t0, length);
                    let (mut now, mut k) = (0_u64, 0);
                    while old.deadline.is_some() {
                        now += if clock == 0 { irregular[k % irregular.len()] } else { clock };
                        k += 1;
                        if k == 3 {
                            m.ask(asked / 3.0, at(t0, now - 1), length);
                            old.ask(asked / 3.0, at(t0, now - 1), length);
                        }
                        let (new, was) = (m.spend(at(t0, now), cap), old.spend(at(t0, now), cap));
                        assert_eq!(new.to_bits(), was.to_bits(), "{asked} over {length:?}, cap {cap}, clock {clock}: {new} {was}");
                    }
                    assert!(!m.moving());
                }
            }
        }
    }
    // A frame a nanosecond before the deadline leaves the curve finite.
    let t0 = Instant::now();
    let mut m = ScrollMotion::default();
    m.ask(10.0, t0, LENGTH);
    m.spend(t0 + LENGTH - Duration::from_nanos(1), NEVER);
    assert!(m.start.is_finite() && m.owed().is_finite(), "{} {}", m.start, m.owed());
}
