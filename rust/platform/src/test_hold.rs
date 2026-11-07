//! Test-only (feature `test-support`, debug assertions): the test half of the hold tool. A test arms a point the product
//! marked with `hold_point!`, waits for an arrival to reach it, does the other thing, and releases it (guarantee 16 of
//! `CLAUDE.md` is the contract).
//!
//! From the arm on, each arrival whose key matches (an arm with no key matches every arrival) takes the next ticket,
//! 0, 1, 2, in the order arrivals claim them; tickets below the hold count wait at the line, the rest pass and are
//! counted. An arm is exclusive per point: a second arm waits for the first one's drop. In a binary whose other tests
//! reach the same point, the arm names a key only this test's arrivals carry. Every wait is bounded: `WAIT` for the
//! test's, `GIVE_UP` for a held arrival, which then goes on and fails its arm by name. Waits use the system clock.

use crate::hold;
use std::fmt::Display;
use std::time::Duration;

pub use crate::hold::{GIVE_UP, WAIT};

/// Where a test's arms live: this process. (A child process's folder is a second constructor.)
pub struct Holds(());

impl Holds {
    pub fn here() -> Self {
        Holds(())
    }

    pub fn arm(&self, point: &'static str) -> ArmSpec {
        ArmSpec { point, key: None, give_up: GIVE_UP }
    }
}

/// An arm before it is installed.
pub struct ArmSpec {
    point: &'static str,
    key: Option<String>,
    give_up: Duration,
}

impl ArmSpec {
    pub fn key(mut self, key: impl Display) -> Self {
        self.key = Some(key.to_string());
        self
    }

    /// A held arrival's own bound; never more than `GIVE_UP`.
    pub fn give_up_after(mut self, bound: Duration) -> Self {
        assert!(bound <= GIVE_UP, "hold point {}: give_up_after({bound:?}) is longer than the {GIVE_UP:?} bound", self.point);
        self.give_up = bound;
        self
    }

    /// Installs the arm: the first `count` arrivals are held. Waits up to `WAIT` for another arm of the point to end.
    pub fn hold(self, count: usize) -> Arm {
        Arm { point: self.point, id: hold::install(self.point, self.key, count, self.give_up) }
    }
}

pub struct Arm {
    point: &'static str,
    id: u64,
}

impl Arm {
    /// Returns once arrival `ticket` exists (held or passed); fails the test by name at `WAIT`.
    pub fn reached(&self, ticket: usize) {
        self.reached_within(ticket, WAIT)
    }

    pub fn reached_within(&self, ticket: usize, bound: Duration) {
        assert!(bound <= WAIT, "hold point {}: reached_within({bound:?}) is longer than the {WAIT:?} bound", self.point);
        if let Err(failure) = hold::wait_arrival(self.id, ticket, bound) {
            panic!("{failure}");
        }
    }

    /// `reached` for async tests: the wait is on `spawn_blocking`, so the runtime's thread stays free.
    pub async fn reached_async(&self, ticket: usize) {
        let id = self.id;
        match tokio::task::spawn_blocking(move || hold::wait_arrival(id, ticket, WAIT)).await {
            Ok(Ok(())) => {}
            Ok(Err(failure)) => panic!("{failure}"),
            Err(joined) => panic!("hold point {}: the wait ended abnormally: {joined}", self.point),
        }
    }

    /// Lets held arrival `ticket` go on. Refused if it has not arrived or is not held.
    pub fn release(&self, ticket: usize) {
        if let Err(failure) = hold::release(self.id, ticket) {
            panic!("{failure}");
        }
    }

    /// How many arrivals the arm has seen, held or passed.
    pub fn arrivals(&self) -> usize {
        hold::arrivals(self.id).unwrap_or_else(|failure| panic!("{failure}"))
    }
}

impl Drop for Arm {
    fn drop(&mut self) {
        if let Some(failure) = hold::uninstall(self.id) {
            if !std::thread::panicking() {
                panic!("{failure}");
            }
        }
    }
}

#[cfg(test)]
#[path = "hold_tests.rs"]
mod tests;
