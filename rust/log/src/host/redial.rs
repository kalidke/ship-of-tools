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
mod tests {
    use super::*;

    const FLOOR: Duration = Duration::from_millis(200);
    const CAP: Duration = Duration::from_secs(30);

    #[test]
    fn a_short_connection_keeps_the_doubling() {
        let mut redial = Redial::new(FLOOR, CAP);
        let waits: Vec<_> = (0..10).map(|_| redial.after(STABLE - Duration::from_secs(1)).as_millis()).collect();
        assert_eq!(waits, [200, 400, 800, 1_600, 3_200, 6_400, 12_800, 25_600, 30_000, 30_000]);
    }

    #[test]
    fn a_connection_that_lasted_stable_starts_over() {
        let mut redial = Redial::new(FLOOR, CAP);
        for _ in 0..5 {
            redial.after(Duration::ZERO);
        }
        assert_eq!(redial.after(STABLE), FLOOR);
        assert_eq!(redial.after(Duration::ZERO), FLOOR * 2);
    }

    #[test]
    fn a_reset_starts_over() {
        let mut redial = Redial::new(FLOOR, CAP);
        for _ in 0..5 {
            redial.after(Duration::ZERO);
        }
        redial.reset();
        assert_eq!(redial.after(Duration::ZERO), FLOOR);
    }
}
