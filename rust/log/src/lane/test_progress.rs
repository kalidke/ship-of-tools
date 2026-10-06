//! Server-local transport checkpoints. Normal builds carry only a zero-sized no-op.

#[cfg(any(test, feature = "test-support"))]
use std::{collections::VecDeque, fmt, sync::Mutex, time::Instant};

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
}

#[cfg(not(any(test, feature = "test-support")))]
#[derive(Default)]
pub struct Progress;

#[cfg(any(test, feature = "test-support"))]
impl Default for Progress {
    fn default() -> Self {
        Self {
            started: Instant::now(),
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
        let mut ring = self.ring.lock().unwrap();
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

    /// Copies the ring without waiting for its lock, independently of connection state.
    #[cfg(any(test, feature = "test-support"))]
    pub fn snapshot(&self) -> Snapshot {
        match self.ring.try_lock() {
            Ok(ring) => Snapshot {
                records: ring.records.iter().cloned().collect(),
                overwritten: ring.overwritten,
                unavailable: false,
            },
            Err(_) => Snapshot {
                records: Vec::new(),
                overwritten: 0,
                unavailable: true,
            },
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
pub struct Snapshot {
    pub records: Vec<Checkpoint>,
    pub overwritten: usize,
    pub unavailable: bool,
}

#[cfg(any(test, feature = "test-support"))]
impl fmt::Display for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.unavailable {
            return writeln!(f, "transport-progress snapshot unavailable");
        }
        writeln!(
            f,
            "transport-progress snapshot records={} overwritten={}",
            self.records.len(),
            self.overwritten
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
    fn busy_snapshot_does_not_wait() {
        let progress = Progress::default();
        let _held = progress.ring.lock().unwrap();
        let snapshot = progress.snapshot();
        assert!(snapshot.unavailable);
        assert_eq!(
            snapshot.to_string(),
            "transport-progress snapshot unavailable\n"
        );
    }

    #[test]
    fn overwrite_keeps_the_last_256_even_after_a_connection_ends() {
        let progress = Progress::default();
        for id in 0..300 {
            progress.note(Some(id), "closed.enqueue", "ok");
        }
        let snapshot = progress.snapshot();
        assert_eq!(snapshot.overwritten, 44);
        assert_eq!(snapshot.records.len(), 256);
        assert_eq!(snapshot.records[0].conn, Some(44));
        assert_eq!(snapshot.records[255].conn, Some(299));
    }
}
