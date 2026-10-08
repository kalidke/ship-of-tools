//! How soon a long-lived connection to a daemon is dialed again after it ends: the window's control transport, the
//! daemon's hub link and the attach worker share this one rule, each with its own floor and cap.

use std::time::Duration;

/// A connection that lasted this long was a working one: the next wait starts over at the floor. A shorter one, a
/// hello answered, an attach completed or a bare connect and then a drop, keeps the doubling.
pub const STABLE: Duration = Duration::from_secs(60);

/// The wait before redialing a connection that has just ended: from `floor`, doubling to `cap`, and back to `floor`
/// only after a connection that lasted [`STABLE`].
#[derive(Debug, Clone)]
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
