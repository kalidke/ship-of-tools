//! `NoopTransport`/`TestTransport`: synthetic [`Transport`] implementations
//! shared by every capsule-runtime integration-test binary that drives
//! `capsule::run` without a real named pipe or Unix socket underneath it.
//! Import only neutral items (`attach_proto`/`transport`, `std`) — no
//! producer, no platform `cfg` — so this file is usable unmodified from
//! `tests/capsule.rs` (Windows, `ConptyProducer`) and, later,
//! `tests/e2e_socket.rs` (Unix, `producer_pty`) alike. `#[path]`-included,
//! not a crate: see each including file's own `mod` declaration.

use sot_log::attach_proto::ConnId;
use sot_log::transport::{Transport, TransportEvent};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

/// A `Transport` with no connections at all — for every test that only
/// needs `run` to work with the wire lane sitting idle (nothing in
/// `transport_events`, nothing ever calls `send`/`close`).
pub struct NoopTransport;
impl Transport for NoopTransport {
    fn bind(&mut self, _voyage_id: &str) -> sot_log::Result<()> {
        Ok(())
    }
    fn try_recv_event(&mut self) -> Option<TransportEvent> {
        None
    }
    fn send(&mut self, _conn: ConnId, _bytes: Vec<u8>) -> u64 {
        0
    }
    fn close(&mut self, _conn: ConnId) {}
    fn shutdown_all(&mut self, _deadline: Instant) -> bool {
        true
    }
}

/// A `NoopTransport` in one call, for the common "just run it, the wire
/// lane is irrelevant to this test" case.
pub fn no_transport() -> NoopTransport {
    NoopTransport
}

/// A synthetic transport driving the SAME `TransportEvent`/`Transport` seam
/// a real transport will (ADR 0041 step 5). `send` reports its
/// completion back through the event channel immediately by default — an
/// ordinary channel send picked up on the loop's next poll, not a
/// same-stack callback into it (`Transport::send`'s own doc) — except while
/// `hold` is set, when completions queue in `held` for the test to release
/// on its own schedule (needed to prove send-before-teardown ordering).
///
/// Transport contract (finding 3): both `sent`/`held` are plain per-
/// connection FIFO queues — `send` always appends, `release_held` always
/// drains front-to-back — because a real transport delivers everything
/// written to one connection in write order with no reordering. Any test
/// that depends on ordering (a checkpoint transfer followed by queued
/// post-watermark output, in particular) relies on that guarantee holding
/// here exactly as it holds for the real transport.
#[derive(Clone)]
pub struct TestTransport {
    events_tx: mpsc::Sender<TransportEvent>,
    events_rx: Arc<Mutex<mpsc::Receiver<TransportEvent>>>,
    inner: Arc<Mutex<TestInner>>,
}

#[derive(Default)]
struct TestInner {
    next_id: u64,
    sent: Vec<(ConnId, Vec<u8>)>,
    /// Connections whose sends currently queue in `held` instead of
    /// completing immediately -- per-connection, so holding one watcher's
    /// output does not also starve an unrelated driver's own replies.
    hold_for: std::collections::HashSet<ConnId>,
    held: Vec<(ConnId, u64)>,
    closed: Vec<ConnId>,
    /// U1a Codex round-1, minor cluster: a COUNT, not a bool -- proves
    /// `run`'s explicit call (once the ack grace resolves) AND
    /// `ShutdownGuard::drop`'s own unconditional call both actually
    /// happen, rather than merely "at least once".
    shutdown_all_call_count: u32,
    /// Codex round-1 Blocker 3 discharge: when set, `shutdown_all`
    /// reports EXPIRY (`false`) instead of success -- simulates a real
    /// transport's own aggregate join failing, so `capsule::run`'s
    /// "expiry is terminal" contract is testable without needing to
    /// genuinely wedge a real OS thread.
    force_shutdown_expiry: bool,
}

impl TestTransport {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            events_tx: tx,
            events_rx: Arc::new(Mutex::new(rx)),
            inner: Arc::new(Mutex::new(TestInner::default())),
        }
    }
    pub fn open(&self, conn: ConnId) {
        let _ = self.events_tx.send(TransportEvent::ConnectionOpened(conn));
    }
    pub fn feed(&self, conn: ConnId, bytes: Vec<u8>) {
        let _ = self.events_tx.send(TransportEvent::Bytes(conn, bytes));
    }
    #[allow(dead_code)] // exercised by tests that simulate a peer-initiated EOF
    pub fn close_conn(&self, conn: ConnId) {
        let _ = self.events_tx.send(TransportEvent::ConnectionClosed(conn));
    }
    pub fn set_hold_for(&self, conn: ConnId, on: bool) {
        let mut inner = self.inner.lock().unwrap();
        if on {
            inner.hold_for.insert(conn);
        } else {
            inner.hold_for.remove(&conn);
        }
    }
    /// Releases every send that queued while held, for every connection, in
    /// order.
    pub fn release_held(&self) {
        let held = std::mem::take(&mut self.inner.lock().unwrap().held);
        for (conn, id) in held {
            let _ = self.events_tx.send(TransportEvent::Sent(conn, id));
        }
    }
    pub fn sent_frames(&self) -> Vec<(ConnId, Vec<u8>)> {
        self.inner.lock().unwrap().sent.clone()
    }
    /// PR #139 discharge round (second CI failure, `slow_watcher_overflow_
    /// closes_while_driver_stays_live`): once the driver is correctly
    /// exempt from the per-watcher queue-overflow eviction (the fix for
    /// the FIRST failure), it stays subscribed for the entire flood
    /// instead of being evicted early alongside the watcher, so `sent`
    /// grows to the flood's full size (here, several MiB across dozens of
    /// entries). `FrameWatcher::wait_for` polls every 10ms; cloning the
    /// WHOLE vector on every single poll -- most of which is already-seen
    /// history the caller is about to skip via its own cursor -- turns an
    /// O(1)-per-poll wait into an O(total accumulated bytes)-per-poll one,
    /// for every poll across the whole wait. Slicing from `start` (the
    /// caller's own cursor) makes each poll's cost track only what is
    /// actually NEW since the last one, which is what made the driver's
    /// post-flood `resize` reply wait (the exact one that timed out in CI)
    /// newly expensive purely as a side effect of the driver eviction fix
    /// being correct -- a WIRING/harness bug, not an `AttachProto` one
    /// (confirmed by `attach_proto::tests::
    /// replay_slow_watcher_flood_the_driver_still_answers_a_resize`, which
    /// replays the identical sequence at the state-machine level with no
    /// wall-clock cost and proves the machine's own action stream is
    /// already correct).
    pub fn sent_frames_from(&self, start: usize) -> Vec<(ConnId, Vec<u8>)> {
        let inner = self.inner.lock().unwrap();
        if start >= inner.sent.len() {
            Vec::new()
        } else {
            inner.sent[start..].to_vec()
        }
    }
    pub fn closed_conns(&self) -> Vec<ConnId> {
        self.inner.lock().unwrap().closed.clone()
    }
    pub fn shutdown_all_was_called(&self) -> bool {
        self.inner.lock().unwrap().shutdown_all_call_count > 0
    }
    /// U1a Codex round-1, minor cluster: the exact call count, so a test
    /// can prove BOTH the explicit ack-grace call and `ShutdownGuard::
    /// drop`'s own later call happened (`== 2`), not merely that
    /// `shutdown_all` ran at least once.
    pub fn shutdown_all_call_count(&self) -> u32 {
        self.inner.lock().unwrap().shutdown_all_call_count
    }
    /// Codex round-1 Blocker 3 discharge: make every future `shutdown_all`
    /// call on this transport report expiry (`false`), simulating a real
    /// transport whose own aggregate join could not prove every worker
    /// stopped in time.
    pub fn force_shutdown_expiry(&self) {
        self.inner.lock().unwrap().force_shutdown_expiry = true;
    }
}

impl Transport for TestTransport {
    fn bind(&mut self, _voyage_id: &str) -> sot_log::Result<()> {
        // The synthetic transport under test here has nothing to bind --
        // `open`/`feed`/`close_conn` already drive its event channel
        // directly, standing in for what a real transport's own `bind`
        // would have wired up.
        Ok(())
    }
    fn try_recv_event(&mut self) -> Option<TransportEvent> {
        self.events_rx.lock().unwrap().try_recv().ok()
    }
    fn send(&mut self, conn: ConnId, bytes: Vec<u8>) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        inner.next_id += 1;
        let id = inner.next_id;
        inner.sent.push((conn, bytes));
        if inner.hold_for.contains(&conn) {
            inner.held.push((conn, id));
            id
        } else {
            drop(inner);
            let _ = self.events_tx.send(TransportEvent::Sent(conn, id));
            id
        }
    }
    fn close(&mut self, conn: ConnId) {
        self.inner.lock().unwrap().closed.push(conn);
    }
    fn shutdown_all(&mut self, _deadline: Instant) -> bool {
        let mut inner = self.inner.lock().unwrap();
        inner.shutdown_all_call_count += 1;
        !inner.force_shutdown_expiry
    }
}
