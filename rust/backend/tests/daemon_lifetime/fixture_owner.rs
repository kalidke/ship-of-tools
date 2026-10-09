//! The outside fixture owner: it watches every process a case learns of, holds termination authority over the ones its
//! own code spawned, saves what the product did BEFORE it cleans anything, then ends only those within a separate
//! reserve. A pid a product printed, a peer credential reported, /proc showed or a parent link reached is watched and
//! never signalled. Cleanup never calls product code, never signals a number, and cannot change a saved result.

use crate::native::Identity;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// One case at a time in this binary. A gated child holds a copy of every descriptor its parent had open when it was
/// forked until it execs or ends, so a case that reads a pipe to its EOF would otherwise wait on another case's child.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// The reserve cleanup gets, apart from the case's own deadline.
pub const CLEANUP_RESERVE: Duration = Duration::from_secs(10);

pub struct Fixture {
    case: String,
    /// Every process the case holds, watched or its own (`Identity::is_own`); cleanup signals only its own.
    identities: Vec<Identity>,
    saved: Vec<(String, String)>,
    cleaned: bool,
    _alone: MutexGuard<'static, ()>,
}

/// What cleanup found.
#[derive(Debug)]
pub struct Cleanup {
    /// Labels of the case's own identities that were still alive when cleanup began.
    pub killed: Vec<String>,
    /// Labels of the case's own identities still alive at the end of the reserve.
    pub survivors: Vec<String>,
    /// Labels of watched identities still alive when cleanup ended: not the case's to end (the daemon's guard, the
    /// scope of the job that runs the suite or the product's own mechanism ends them).
    pub watched_alive: Vec<String>,
}

impl Cleanup {
    pub fn complete(&self) -> bool {
        self.survivors.is_empty()
    }
}

impl Fixture {
    pub fn new(case: &str) -> Fixture {
        let alone = ONE_AT_A_TIME
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Fixture {
            case: case.to_string(),
            identities: Vec::new(),
            saved: Vec::new(),
            cleaned: false,
            _alone: alone,
        }
    }

    /// Watch `pid` (checked against `created` when the process reported its own start time), under `label`. The case did
    /// not start it: it is observed through its identity and never signalled.
    pub fn watch(&mut self, pid: i32, created: Option<u64>, label: &str) -> std::io::Result<usize> {
        let identity = Identity::acquire(pid, created, label)?;
        self.identities.push(identity);
        Ok(self.identities.len() - 1)
    }

    /// Hold the process this case's own code got `pid` back for from its own spawn and has not reaped: cleanup may end it.
    pub fn own(&mut self, pid: i32, label: &str) -> std::io::Result<usize> {
        let identity = Identity::acquire_own(pid, label)?;
        self.identities.push(identity);
        Ok(self.identities.len() - 1)
    }

    pub fn identity(&self, index: usize) -> &Identity {
        &self.identities[index]
    }

    /// Record an observation of the product, to be asserted on after cleanup.
    pub fn save(&mut self, key: &str, value: impl std::fmt::Display) {
        self.saved.push((key.to_string(), value.to_string()));
    }

    pub fn saved(&self, key: &str) -> Option<&str> {
        self.saved
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// End what is still alive among the case's own identities, through the retained identities only, and report; prints
    /// `cleanup: complete` when every own identity is gone. Watched identities are only looked at. Idempotent.
    pub fn cleanup(&mut self) -> Cleanup {
        let mut killed = Vec::new();
        for identity in self.identities.iter().filter(|i| i.is_own()) {
            if !identity.exited(Duration::ZERO) {
                killed.push(format!(
                    "{} (pid {}, start {})",
                    identity.label, identity.pid, identity.created
                ));
                let _ = identity.kill();
            }
        }
        let deadline = Instant::now() + CLEANUP_RESERVE;
        let mut survivors = Vec::new();
        for identity in self.identities.iter().filter(|i| i.is_own()) {
            let left = deadline.saturating_duration_since(Instant::now());
            if !identity.exited(left) {
                survivors.push(format!(
                    "{} (pid {}, start {})",
                    identity.label, identity.pid, identity.created
                ));
            }
        }
        let watched_alive = self
            .identities
            .iter()
            .filter(|i| !i.is_own() && !i.exited(Duration::ZERO))
            .map(|i| format!("{} (pid {}, start {})", i.label, i.pid, i.created))
            .collect();
        self.cleaned = true;
        let report = Cleanup {
            killed,
            survivors,
            watched_alive,
        };
        if report.complete() {
            eprintln!(
                "{}: cleanup: complete (ended {:?}; watched, still alive: {:?})",
                self.case, report.killed, report.watched_alive
            );
        } else {
            eprintln!(
                "{}: cleanup: INCOMPLETE, still alive: {:?}",
                self.case, report.survivors
            );
        }
        report
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.cleaned {
            self.cleanup();
        }
    }
}
