//! Server-local transport checkpoints. Normal builds carry only a zero-sized no-op.

#[cfg(any(test, feature = "test-support"))]
use std::{
    collections::VecDeque,
    fmt,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex, MutexGuard, TryLockError,
    },
    time::Instant,
};

#[cfg(any(test, feature = "test-support"))]
const CAPACITY: usize = 256;

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug)]
pub struct Checkpoint {
    pub conn: Option<u64>,
    pub step: &'static str,
    pub elapsed_ms: u128,
    pub result: String,
}

#[cfg(any(test, feature = "test-support"))]
impl fmt::Display for Checkpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let conn = self
            .conn
            .map_or_else(|| "pending".into(), |id| id.to_string());
        write!(
            f,
            "transport-progress transport=socket conn={conn} step={} elapsed_ms={} result={}",
            self.step, self.elapsed_ms, self.result
        )
    }
}

#[cfg(any(test, feature = "test-support"))]
struct Ring {
    records: VecDeque<Checkpoint>,
    overwritten: usize,
}

#[cfg(any(test, feature = "test-support"))]
pub struct Progress {
    started: Instant,
    ring: Mutex<Ring>,
    skipped: AtomicUsize,
}

#[cfg(not(any(test, feature = "test-support")))]
#[derive(Default)]
pub struct Progress;

#[cfg(any(test, feature = "test-support"))]
impl Default for Progress {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            skipped: AtomicUsize::new(0),
            ring: Mutex::new(Ring {
                records: VecDeque::with_capacity(CAPACITY),
                overwritten: 0,
            }),
        }
    }
}

impl Progress {
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn note(&self, conn: Option<u64>, step: &'static str, result: impl fmt::Display) {
        let record = Checkpoint {
            conn,
            step,
            elapsed_ms: self.started.elapsed().as_millis(),
            result: result.to_string(),
        };
        let mut ring = match self.ring.try_lock() {
            Ok(ring) => ring,
            Err(_) => {
                self.skipped.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        if ring.records.len() == CAPACITY {
            ring.records.pop_front();
            ring.overwritten += 1;
        }
        ring.records.push_back(record);
    }

    #[cfg(not(any(test, feature = "test-support")))]
    #[inline]
    pub(crate) fn note(
        &self,
        _conn: Option<u64>,
        _step: &'static str,
        _result: impl std::fmt::Display,
    ) {
    }

    /// Deliberate fixture hold; passive operations never use this blocking acquisition.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn hold(&self) -> Hold<'_> {
        Hold {
            _held: self.ring.lock().expect("hold the recorder fixture"),
        }
    }

    /// Copies the ring without waiting for its lock, independently of connection state.
    #[cfg(any(test, feature = "test-support"))]
    pub fn snapshot(&self) -> Snapshot {
        match self.ring.try_lock() {
            Ok(ring) => Snapshot {
                records: ring.records.iter().cloned().collect(),
                overwritten: Some(ring.overwritten),
                skipped: self.skipped.load(Ordering::Relaxed),
                unavailable: false,
                reason: None,
            },
            Err(error) => Snapshot {
                records: Vec::new(),
                overwritten: None,
                skipped: self.skipped.load(Ordering::Relaxed),
                unavailable: true,
                reason: Some(match error {
                    TryLockError::WouldBlock => "busy",
                    TryLockError::Poisoned(_) => "poisoned",
                }),
            },
        }
    }
}

/// Opaque test fixture holding only a server's recorder.
#[cfg(any(test, feature = "test-support"))]
pub struct Hold<'a> {
    _held: MutexGuard<'a, Ring>,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
pub struct Snapshot {
    pub records: Vec<Checkpoint>,
    pub overwritten: Option<usize>,
    pub skipped: usize,
    pub reason: Option<&'static str>,
    pub unavailable: bool,
}

#[cfg(any(test, feature = "test-support"))]
impl fmt::Display for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.unavailable {
            return writeln!(
                f,
                "transport-progress snapshot unavailable skipped={} reason={}",
                self.skipped,
                self.reason.unwrap()
            );
        }
        writeln!(
            f,
            "transport-progress snapshot records={} overwritten={} skipped={}",
            self.records.len(),
            self.overwritten.unwrap(),
            self.skipped
        )?;
        for record in &self.records {
            writeln!(f, "{record}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_checkpoint_admission_is_skipped_and_counted() {
        let progress = std::sync::Arc::new(Progress::default());
        progress.note(Some(1), "registered", "ok");
        let before = progress.snapshot().to_string();
        let held = progress.ring.lock().unwrap();
        let (entered, entry) = std::sync::mpsc::channel();
        let (finished, completion) = std::sync::mpsc::channel();
        let producer = std::thread::spawn({
            let progress = std::sync::Arc::clone(&progress);
            move || {
                entered.send(()).unwrap();
                for _ in 0..7 {
                    progress.note(Some(2), "registered", "ok");
                }
                finished.send(()).unwrap();
            }
        });
        let observed_entry = entry
            .recv_timeout(std::time::Duration::from_secs(2))
            .is_ok();
        let completed_while_held = completion
            .recv_timeout(std::time::Duration::from_secs(2))
            .is_ok();
        drop(held);
        producer.join().unwrap();
        assert!(observed_entry, "checkpoint producer did not enter");
        assert!(
            completed_while_held,
            "checkpoint admission waited for the busy recorder"
        );
        let after = progress.snapshot().to_string();
        assert!(
            after.contains("skipped=7"),
            "known checkpoint attempts were not counted: {after}"
        );
        assert_eq!(
            before.lines().skip(1).collect::<Vec<_>>(),
            after.lines().skip(1).collect::<Vec<_>>()
        );
        assert!(
            after.contains("records=1 overwritten=0"),
            "busy admission changed the ring: {after}"
        );
        eprintln!("recorder-proof attempts=7 skipped=7 completed=while-held bodies=1");
    }

    #[test]
    fn busy_snapshot_does_not_wait() {
        let progress = Progress::default();
        let _held = progress.ring.lock().unwrap();
        let snapshot = progress.snapshot();
        assert!(snapshot.unavailable);
        assert_eq!(
            snapshot.to_string(),
            "transport-progress snapshot unavailable skipped=0 reason=busy\n"
        );
    }

    #[test]
    fn poisoned_snapshot_reports_unavailable_without_ring_counts() {
        let progress = Progress::default();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = progress.ring.lock().unwrap();
            panic!("poison the test-owned recorder");
        }));
        progress.note(None, "registered", "skipped");
        let snapshot = progress.snapshot();
        assert!(snapshot.unavailable);
        assert_eq!(snapshot.overwritten, None);
        assert_eq!(snapshot.skipped, 1);
        assert_eq!(
            snapshot.to_string(),
            "transport-progress snapshot unavailable skipped=1 reason=poisoned\n"
        );
    }

    #[test]
    fn overwrite_keeps_the_last_256_even_after_a_connection_ends() {
        let progress = Progress::default();
        for id in 0..300 {
            progress.note(Some(id), "closed.enqueue", "ok");
        }
        let snapshot = progress.snapshot();
        assert_eq!(snapshot.overwritten, Some(44));
        assert_eq!(snapshot.skipped, 0);
        assert_eq!(snapshot.records.len(), 256);
        assert_eq!(snapshot.records[0].conn, Some(44));
        assert_eq!(snapshot.records[255].conn, Some(299));
    }
}
