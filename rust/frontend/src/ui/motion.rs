//! The rule of a person's scroll that eases: a distance owed is spent over the next frames on an ease-out cubic and
//! the last of it lands exactly `length` after the last ask. The owner knows no pane; a pane keeps its own position and
//! edges, asks here for what a person wanted, and adds what each frame spends to its position.

use std::time::{Duration, Instant};

#[derive(Default)]
pub(in crate::ui) struct ScrollMotion {
    /// Distance asked and not yet spent, signed, in the pane's unit: the view comes to rest where the person asked.
    owed: f32,
    /// When the last of it is spent: the motion is bounded in time, so idle stays idle. `None` when nothing is owed.
    deadline: Option<Instant>,
    /// The last spend's time (or the ask's), so the same curve runs at 60 and 120 frames a second.
    last: Option<Instant>,
}

impl ScrollMotion {
    /// A person asked for `delta` at `now`. With a zero `length` the answer is what to apply at once (everything
    /// owed with it); otherwise nothing is applied now, and what was owed and `delta` come to rest `length` after `now`.
    pub(in crate::ui) fn ask(&mut self, delta: f32, now: Instant, length: Duration) -> f32 {
        if length.is_zero() {
            return delta + self.finish();
        }
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
        let keep = self.owed * left * left * left;
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
    }

    /// What is owed, for a caller that asks for a target rather than a distance.
    pub(in crate::ui) fn owed(&self) -> f32 { self.owed }

    /// Whether frames must keep coming.
    pub(in crate::ui) fn moving(&self) -> bool { self.deadline.is_some() }
}

#[cfg(test)]
#[path = "motion_tests.rs"]
mod tests;
