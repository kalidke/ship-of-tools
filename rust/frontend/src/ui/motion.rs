//! The rule of a person's scroll that eases: a distance owed is spent over the next frames on an ease-out cubic and
//! the last of it lands exactly `length` after the last ask. A cell pane's whole rows start no faster than a row a
//! frame. The owner knows no pane; a pane keeps its own position and
//! edges, asks here for what a person wanted, and adds what each frame spends to its position.

use std::time::{Duration, Instant};

/// Float noise in a sum of spends never shows a row the curve has not reached.
const ROW_EPS: f32 = 1.0e-3;

/// The start speed of the cubic ease-out, `1 - (1 - u)^3`, as a multiple of its mean speed: a pixel pane's curve.
const CUBIC: f32 = 3.0;

/// The slowest start the whole-row curve takes, as a multiple of its mean speed: the least at which it only slows.
const SLOWEST_START: f32 = 1.5;

/// A whole-row motion starts at no more than a row per frame at 60 frames a second, so a page rolls rather than jumps.
const ROWS_PER_SECOND: f32 = 60.0;

/// The longest the pace stretches a whole-row motion: a held page key or a burst of wheel ticks owes more than a pane,
/// and the view comes to rest this long after the last ask, starting faster than the pace.
const PACE_CEILING: Duration = Duration::from_millis(600);

pub(in crate::ui) struct ScrollMotion {
    /// Distance asked and not yet spent, signed, in the pane's unit: the view comes to rest where the person asked.
    owed: f32,
    /// When the last of it is spent: the motion is bounded in time, so idle stays idle. `None` when nothing is owed.
    deadline: Option<Instant>,
    /// The last spend's time (or the ask's), so the same curve runs at 60 and 120 frames a second.
    last: Option<Instant>,
    /// A cell pane's whole-row spend: the distance spent minus the rows shown, so the first row shows at the first
    /// frame (the rows run ahead of the curve by less than one) and the asked total still lands.
    carry: f32,
    /// The curve's speed now, as a multiple of its mean speed over the time left: `CUBIC` for a pixel pane, from
    /// `SLOWEST_START` to `CUBIC` for whole rows. The curve is the cubic that starts at this speed and ends at rest, and
    /// what is left of it is again such a cubic, so a spend needs only this and the time left.
    start: f32,
}

impl Default for ScrollMotion {
    fn default() -> Self { Self { owed: 0.0, deadline: None, last: None, carry: 0.0, start: CUBIC } }
}

impl ScrollMotion {
    /// A person asked for `delta` at `now`. With a zero `length` the answer is what to apply at once (everything
    /// owed with it); otherwise nothing is applied now, and what was owed and `delta` come to rest `length` after `now`.
    pub(in crate::ui) fn ask(&mut self, delta: f32, now: Instant, length: Duration) -> f32 {
        if length.is_zero() {
            return delta + self.finish();
        }
        self.start = CUBIC;
        self.owed += delta;
        if self.owed == 0.0 {
            self.stop();
        } else {
            self.deadline = Some(now + length);
            self.last = Some(now);
        }
        0.0
    }

    /// What this frame at `now` moves the view by: everything past `cap` (a pane's height) at once, and of the rest
    /// the part the curve has covered since the last spend, all of it at the deadline. Time never runs backward, and
    /// a slow frame shows fewer, larger steps, never a longer motion.
    pub(in crate::ui) fn spend(&mut self, now: Instant, cap: f32) -> f32 {
        let Some(deadline) = self.deadline else { return 0.0 };
        let cap = cap.max(0.0);
        let mut spent = 0.0;
        if self.owed.abs() > cap {
            spent = self.owed - self.owed.signum() * cap;
            self.owed -= spent;
        }
        if now >= deadline {
            return spent + self.finish();
        }
        let last = self.last.unwrap_or(now).min(now);
        let left = (deadline - now).as_secs_f32() / (deadline - last).as_secs_f32();
        // The cubic from speed `start` to rest keeps `left^2 * (1 + bend * (1 - left))` of what is owed (`left^3` for
        // `CUBIC`, which stays itself), and what is left of it starts at the speed computed after; `bend` is above -1
        // there, so the division is by more than zero.
        let keep = if self.start == CUBIC {
            self.owed * left * left * left
        } else {
            let (bend, gone) = (2.0 - self.start, 1.0 - left);
            let next = (self.start + 3.0 * bend * gone) / (1.0 + bend * gone);
            self.start = if next.is_finite() { next.clamp(SLOWEST_START, CUBIC) } else { CUBIC };
            self.owed * left * left * (1.0 + bend * gone)
        };
        spent += self.owed - keep;
        self.owed = keep;
        self.last = Some(now);
        spent
    }

    /// Everything owed, now; nothing stays owed.
    pub(in crate::ui) fn finish(&mut self) -> f32 {
        let owed = self.owed;
        self.stop();
        owed
    }

    /// Drop what is owed: the position moved by some other rule, or met an edge.
    pub(in crate::ui) fn stop(&mut self) {
        self.owed = 0.0;
        self.deadline = None;
        self.carry = 0.0;
    }

    /// `ask` for a pane that moves in whole rows: what to apply now, which with a zero `length` is the asked rows and
    /// all that was owed. The roll starts no faster than `ROWS_PER_SECOND` and only slows: a short step keeps the cubic
    /// over `length`, and a longer one starts slower and takes longer than `length`, up to `PACE_CEILING`, so a page
    /// rolls a row a frame and a burst still comes to rest soon after its last ask.
    pub(in crate::ui) fn ask_rows(&mut self, delta: i32, now: Instant, length: Duration) -> i32 {
        if length.is_zero() {
            return delta + self.finish_rows();
        }
        let rows = (self.owed + delta as f32).abs();
        let length = length.max(Duration::from_secs_f32(SLOWEST_START * rows / ROWS_PER_SECOND).min(PACE_CEILING));
        self.ask(delta as f32, now, length);
        if self.moving() {
            self.start = (ROWS_PER_SECOND * length.as_secs_f32() / rows).clamp(SLOWEST_START, CUBIC);
        }
        0
    }

    /// `spend` for a pane that moves in whole rows: the rows this frame shows. Rows are the spent distance rounded up
    /// in the direction of travel, so the first row shows at the first frame; the last frame rounds, so the asked
    /// total is what is shown, never a row more.
    pub(in crate::ui) fn spend_rows(&mut self, now: Instant, cap: f32) -> i32 {
        let carry = self.carry;
        let spent = self.spend(now, cap);
        let total = carry + spent;
        let rows = if !self.moving() {
            total.round()
        } else if spent > 0.0 {
            (total - ROW_EPS).ceil().max(0.0)
        } else if spent < 0.0 {
            (total + ROW_EPS).floor().min(0.0)
        } else {
            0.0
        };
        self.carry = if self.moving() { total - rows } else { 0.0 };
        // The motion ends once its last row is shown: no frame is drawn for a roll that has nothing more to show.
        if self.moving() && self.rows_owed() == 0 {
            self.stop();
        }
        rows as i32
    }

    /// The rows still to show: what is owed, less the rows already shown ahead of the curve.
    pub(in crate::ui) fn rows_owed(&self) -> i32 { (self.owed + self.carry).round() as i32 }

    /// `finish` in whole rows: the rows still to show.
    pub(in crate::ui) fn finish_rows(&mut self) -> i32 {
        let rest = self.rows_owed();
        self.stop();
        rest
    }

    /// What is owed, for a caller that asks for a target rather than a distance.
    pub(in crate::ui) fn owed(&self) -> f32 { self.owed }

    /// Whether frames must keep coming.
    pub(in crate::ui) fn moving(&self) -> bool { self.deadline.is_some() }
}

#[cfg(test)]
#[path = "motion_tests.rs"]
mod tests;
