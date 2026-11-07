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
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use crate::hold::{GIVE_UP, WAIT};

/// Where a test's arms live: this process, or a folder shared with the child processes started with it.
pub struct Holds(Option<PathBuf>);

impl Holds {
    pub fn here() -> Self {
        Holds(None)
    }

    /// A folder (made if absent) for arms that hold child processes: pass `env()` to each child before it starts.
    /// One folder holds one arm of a point for its life, so a test uses a folder of its own.
    pub fn folder(dir: &Path) -> Self {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("hold folder {}: {e}", dir.display()));
        Holds(Some(dir.to_path_buf()))
    }

    /// The variable a child needs to be held by this folder's arms.
    pub fn env(&self) -> (&'static str, &Path) {
        ("SOT_TEST_HOLDS", self.0.as_deref().expect("Holds::here() has no folder: a child needs Holds::folder"))
    }

    pub fn arm(&self, point: &'static str) -> ArmSpec {
        ArmSpec { point, key: None, give_up: GIVE_UP, dir: self.0.clone() }
    }
}

/// An arm before it is installed.
pub struct ArmSpec {
    point: &'static str,
    key: Option<String>,
    give_up: Duration,
    dir: Option<PathBuf>,
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
        match self.dir {
            None => Arm { point: self.point, id: hold::install(self.point, self.key, count, self.give_up), folder: None },
            Some(dir) => {
                let arm = hold::folder_install(&dir, self.point, self.key, count, self.give_up);
                Arm { point: self.point, id: 0, folder: Some(arm) }
            }
        }
    }
}

pub struct Arm {
    point: &'static str,
    /// The table's id for an arm of this process; unused when `folder` is set.
    id: u64,
    folder: Option<hold::FolderArm>,
}

impl Arm {
    /// Returns once arrival `ticket` exists (held or passed); fails the test by name at `WAIT`.
    pub fn reached(&self, ticket: usize) {
        self.reached_within(ticket, WAIT)
    }

    pub fn reached_within(&self, ticket: usize, bound: Duration) {
        assert!(bound <= WAIT, "hold point {}: reached_within({bound:?}) is longer than the {WAIT:?} bound", self.point);
        let waited = match &self.folder {
            Some(arm) => hold::folder_wait(arm, ticket, bound),
            None => hold::wait_arrival(self.id, ticket, bound),
        };
        if let Err(failure) = waited {
            panic!("{failure}");
        }
    }

    /// `reached` for async tests: the wait is on `spawn_blocking`, so the runtime's thread stays free.
    pub async fn reached_async(&self, ticket: usize) {
        let (id, folder) = (self.id, self.folder.clone());
        let wait = move || match &folder {
            Some(arm) => hold::folder_wait(arm, ticket, WAIT),
            None => hold::wait_arrival(id, ticket, WAIT),
        };
        match tokio::task::spawn_blocking(wait).await {
            Ok(Ok(())) => {
                // The runtime may have been held until a give-up (a sync point on its thread): read it now.
                self.arrivals();
            }
            Ok(Err(failure)) => panic!("{failure}"),
            Err(joined) => panic!("hold point {}: the wait ended abnormally: {joined}", self.point),
        }
    }

    /// Lets held arrival `ticket` go on. Refused if it has not arrived or is not held.
    pub fn release(&self, ticket: usize) {
        let released = match &self.folder {
            Some(arm) => hold::folder_release(arm, ticket),
            None => hold::release(self.id, ticket),
        };
        if let Err(failure) = released {
            panic!("{failure}");
        }
    }

    /// How many arrivals the arm has seen, held or passed.
    pub fn arrivals(&self) -> usize {
        let seen = match &self.folder {
            Some(arm) => hold::folder_arrivals(arm),
            None => hold::arrivals(self.id),
        };
        seen.unwrap_or_else(|failure| panic!("{failure}"))
    }
}

impl Drop for Arm {
    fn drop(&mut self) {
        let failure = match &self.folder {
            Some(arm) => hold::folder_uninstall(arm),
            None => hold::uninstall(self.id),
        };
        if let Some(failure) = failure {
            if std::thread::panicking() {
                eprintln!("{failure}");
            } else {
                panic!("{failure}");
            }
        }
    }
}

#[cfg(test)]
#[path = "hold_tests.rs"]
mod tests;
