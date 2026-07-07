// session.rs — in-memory per-session state.
//
// One Ship of Tools backend process owns one Session. The Session holds the
// monotonic revision counter (per ADR 0010 reconnect: client sends
// `last_seen_revision`, backend either replays from the ring or sends a
// snapshot) and the bounded event ring for replay.
//
// `bump()` increments the revision and pushes a `RingEntry` onto the back of
// the ring; oldest entries fall off when capacity is reached. The session
// itself isn't aware of clients — those live in the connection task.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::sync::Mutex;

const RING_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub struct RingEntry {
    pub revision: u64,
    pub op: String,
    pub payload: Value,
}

#[derive(Debug)]
pub struct SessionInner {
    pub session_id: String,
    pub revision: u64,
    pub ring: VecDeque<RingEntry>,
    /// Smallest revision still in the ring. Anything older than this requires
    /// a snapshot rather than replay.
    pub ring_low_water: u64,
}

#[derive(Clone)]
pub struct Session {
    inner: Arc<Mutex<SessionInner>>,
}

impl Session {
    pub fn new() -> Self {
        let session_id = generate_id("sess");
        Self {
            inner: Arc::new(Mutex::new(SessionInner {
                session_id,
                revision: 0,
                ring: VecDeque::with_capacity(RING_CAPACITY),
                ring_low_water: 0,
            })),
        }
    }

    pub async fn snapshot(&self) -> (String, u64) {
        let g = self.inner.lock().await;
        (g.session_id.clone(), g.revision)
    }

    /// Record an event the connection layer can later replay. Returns the new
    /// revision so callers can attach it to the response or evt that prompted
    /// the bump.
    pub async fn bump(&self, op: impl Into<String>, payload: Value) -> u64 {
        let mut g = self.inner.lock().await;
        g.revision += 1;
        let revision = g.revision;
        let op = op.into();
        g.ring.push_back(RingEntry {
            revision,
            op,
            payload,
        });
        if g.ring.len() > RING_CAPACITY {
            if let Some(dropped) = g.ring.pop_front() {
                g.ring_low_water = dropped.revision + 1;
            }
        }
        revision
    }

    /// Events strictly newer than `since`. Used when a client reconnects with
    /// `last_seen_revision`. Returns `None` if `since` is older than the ring
    /// — caller must send a snapshot instead.
    pub async fn replay_after(&self, since: u64) -> Option<Vec<RingEntry>> {
        let g = self.inner.lock().await;
        if since < g.ring_low_water.saturating_sub(1) && since != 0 {
            return None;
        }
        Some(g.ring.iter().filter(|e| e.revision > since).cloned().collect())
    }
}

pub fn generate_id(prefix: &str) -> String {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    let salt = std::process::id() as u64;
    format!("{}-{:016x}-{:08x}", prefix, micros, salt)
}
