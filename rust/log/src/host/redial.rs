//! How soon a long-lived connection or child that ended is started again: the window's control transport, the
//! daemon's hub link, the attach worker and the kernel supervisor share this one rule, each with its own floor and cap.

use std::time::Duration;

/// A connection or child that lasted this long was a working one: the next wait starts over at the floor. A shorter
/// one, a hello answered, an attach completed, a bare connect or a kernel generation and then a drop, keeps the
/// doubling.
pub const STABLE: Duration = Duration::from_secs(60);

/// The wait before restarting a connection or child that has just ended: from `floor`, doubling to `cap`, and back
/// to `floor` only after one that lasted [`STABLE`], or when its caller calls [`Redial::reset`] at a person's request
/// (the window's F5).
#[derive(Debug)]
pub struct Redial {
    floor: Duration,
    cap: Duration,
    next: Duration,
}

impl Redial {
    pub fn new(floor: Duration, cap: Duration) -> Self {
        Self { floor, cap, next: floor }
    }

    /// The wait before the next attempt, given how long the attempt that just ended lasted.
    pub fn after(&mut self, lasted: Duration) -> Duration {
        if lasted >= STABLE {
            self.next = self.floor;
        }
        let wait = self.next;
        self.next = self.next.saturating_mul(2).min(self.cap);
        wait
    }

    /// Start over at the floor: the person asked to retry now.
    pub fn reset(&mut self) {
        self.next = self.floor;
    }
}

#[cfg(test)]
#[path = "redial_tests.rs"]
mod tests;
