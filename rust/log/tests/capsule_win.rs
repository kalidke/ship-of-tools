#![cfg(windows)]
//! Integration tests for the Windows capsule runtime (`src/capsule_win.rs`,
//! ADR 0041 step 4). Lives in `tests/` for the same reason `tests/conpty.rs`
//! does: one of these (the flood test) needs `env!("CARGO_BIN_EXE_...")` to
//! find its helper binary, which Cargo only wires up for integration test
//! binaries, and the rest are kept here too for one home and one
//! `cargo test -p sot-log --test capsule_win` filter.
//!
//! The host-handshake byte state machine's own unit tests
//! (`host_handshake.rs`) are pure and run everywhere already; what these
//! tests add is proof the WIRING is correct on a real ConPTY — that a real
//! DA1 answer becomes a well-formed, exactly-once `request`/`response`/
//! `outcome` triple, that a real resize commits a real `outcome` and calls
//! `ResizePseudoConsole` exactly when it should, and that a real spawn
//! failure and a real requested kill both seal a verifiable voyage with
//! `producer_dead` as the last frame in it.
//!
//! Discharge round (Codex review, finding 7): the previous version's flood
//! test asserted byte-count equality across a lossy transform boundary
//! (`hOutput` is conhost's own rendered VT stream, not raw child stdout)
//! and ran `run` on the test's own thread, so a teardown deadlock consumed
//! the whole CI job's timeout instead of failing locally; the handshake
//! test checked membership, not a bijection; the resize test could pass
//! even if every outcome targeted the same request or `ResizePseudoConsole`
//! were never actually gated. All four are fixed below.

use sot_log::attach_proto::ConnId;
use sot_log::capsule_win::{self, CapsuleWinConfig, Command, ExitKind, Transport, TransportEvent};
use sot_log::segment::{RetentionClass, SegmentReader};
use sot_log::verify::verify_voyage;
use sot_log::wire::{self, Survival};
use sot_log::{Class, Envelope, RefKind};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

fn config(dir: &std::path::Path, name: &str, argv: Vec<String>, cols: u16, rows: u16) -> CapsuleWinConfig {
    CapsuleWinConfig {
        voyage_root: dir.join(name),
        voyage_id: name.to_string(),
        retention: RetentionClass::Discard,
        producer_kind: "test-shell".into(),
        argv,
        cols,
        rows,
        survival: Survival::Normal,
    }
}

/// A `Transport` with no connections at all — for every test that only
/// needs `run` to work with the wire lane sitting idle (nothing in
/// `transport_events`, nothing ever calls `send`/`close`).
struct NoopTransport;
impl Transport for NoopTransport {
    fn send(&mut self, _conn: ConnId, _bytes: Vec<u8>) -> u64 {
        0
    }
    fn close(&mut self, _conn: ConnId) {}
    fn shutdown_all(&mut self) {}
}

/// Constructs an already-disconnected `(Sender, Receiver)` pair's receiving
/// half plus a `NoopTransport` in one call, for the common "just run it, the
/// wire lane is irrelevant to this test" case.
fn no_transport() -> (mpsc::Receiver<TransportEvent>, NoopTransport) {
    let (_tx, rx) = mpsc::channel();
    (rx, NoopTransport)
}

/// A synthetic transport driving the SAME `TransportEvent`/`Transport` seam
/// U3's real named pipe will (ADR 0041 step 5). `send` reports its
/// completion back through the event channel immediately by default — an
/// ordinary channel send picked up on the loop's next poll, not a
/// same-stack callback into it (`Transport::send`'s own doc) — except while
/// `hold` is set, when completions queue in `held` for the test to release
/// on its own schedule (needed to prove send-before-teardown ordering).
///
/// Transport contract (finding 3): both `sent`/`held` are plain per-
/// connection FIFO queues — `send` always appends, `release_held` always
/// drains front-to-back — because U3's real named pipe delivers everything
/// written to one connection in write order with no reordering. Any test
/// that depends on ordering (a checkpoint transfer followed by queued
/// post-watermark output, in particular) relies on that guarantee holding
/// here exactly as it holds for the real transport.
#[derive(Clone)]
struct TestTransport {
    events_tx: mpsc::Sender<TransportEvent>,
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
    shutdown_all_called: bool,
}

impl TestTransport {
    fn new() -> (Self, mpsc::Receiver<TransportEvent>) {
        let (tx, rx) = mpsc::channel();
        (
            Self { events_tx: tx, inner: Arc::new(Mutex::new(TestInner::default())) },
            rx,
        )
    }
    fn open(&self, conn: ConnId) {
        let _ = self.events_tx.send(TransportEvent::ConnectionOpened(conn));
    }
    fn feed(&self, conn: ConnId, bytes: Vec<u8>) {
        let _ = self.events_tx.send(TransportEvent::Bytes(conn, bytes));
    }
    #[allow(dead_code)] // exercised by tests that simulate a peer-initiated EOF
    fn close_conn(&self, conn: ConnId) {
        let _ = self.events_tx.send(TransportEvent::ConnectionClosed(conn));
    }
    fn set_hold_for(&self, conn: ConnId, on: bool) {
        let mut inner = self.inner.lock().unwrap();
        if on {
            inner.hold_for.insert(conn);
        } else {
            inner.hold_for.remove(&conn);
        }
    }
    /// Releases every send that queued while held, for every connection, in
    /// order.
    fn release_held(&self) {
        let held = std::mem::take(&mut self.inner.lock().unwrap().held);
        for (conn, id) in held {
            let _ = self.events_tx.send(TransportEvent::Sent(conn, id));
        }
    }
    fn sent_frames(&self) -> Vec<(ConnId, Vec<u8>)> {
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
    fn sent_frames_from(&self, start: usize) -> Vec<(ConnId, Vec<u8>)> {
        let inner = self.inner.lock().unwrap();
        if start >= inner.sent.len() {
            Vec::new()
        } else {
            inner.sent[start..].to_vec()
        }
    }
    fn closed_conns(&self) -> Vec<ConnId> {
        self.inner.lock().unwrap().closed.clone()
    }
    fn shutdown_all_was_called(&self) -> bool {
        self.inner.lock().unwrap().shutdown_all_called
    }
}

impl Transport for TestTransport {
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
    fn shutdown_all(&mut self) {
        self.inner.lock().unwrap().shutdown_all_called = true;
    }
}

/// Encode helpers for the attach lane's client frames and the mgmt lane's
/// requests — thin wrappers so tests read as protocol steps, not byte
/// plumbing.
mod frame {
    use super::wire;

    pub fn hello() -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Hello { proto: wire::ATTACH_PROTO_V1 }).unwrap()
    }
    pub fn attach(controller_id: &str) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Attach { controller_id: controller_id.into() }).unwrap()
    }
    pub fn take(controller_id: &str) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Take { controller_id: controller_id.into() }).unwrap()
    }
    pub fn input(controller_id: &str, take_epoch: u64, idem_key: [u8; 16], payload: &[u8]) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Input {
            controller_id: controller_id.into(),
            take_epoch,
            idem_key,
            payload: payload.to_vec(),
        })
        .unwrap()
    }
    pub fn resize(cols: u16, rows: u16) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Resize { cols, rows }).unwrap()
    }
    pub fn mgmt_probe() -> Vec<u8> {
        wire::encode_mgmt_request(&wire::MgmtRequest::Probe).unwrap()
    }
    pub fn mgmt_status() -> Vec<u8> {
        wire::encode_mgmt_request(&wire::MgmtRequest::Status).unwrap()
    }
    pub fn mgmt_shutdown(reason: &str) -> Vec<u8> {
        wire::encode_mgmt_request(&wire::MgmtRequest::Shutdown { reason: reason.into() }).unwrap()
    }
}

/// Polls a `TestTransport`'s sent frames (cursor-based: never re-scans
/// already-seen entries) for one matching `pred`, bounded — a protocol
/// reply is asynchronous relative to the test's own thread, so this is the
/// same "poll with a bound, never a fixed sleep" discipline the existing
/// flood/resize tests already use for `run`'s own completion.
struct FrameWatcher<'a> {
    transport: &'a TestTransport,
    /// One cursor PER CONNECTION into the shared send log. A single
    /// shared cursor was the first version of this fix and regressed
    /// both windows legs deterministically: a wait for conn B that
    /// matches at log position N would advance the shared cursor past
    /// conn A's frames interleaved before N, so a later wait for A
    /// could never see them — the pre-fix full-rescan was
    /// order-tolerant, a single cursor is not. Per-connection cursors
    /// keep the O(new bytes) poll cost while preserving the rescan's
    /// semantics for interleaved streams.
    next_idx: std::collections::HashMap<ConnId, usize>,
}

impl<'a> FrameWatcher<'a> {
    fn new(transport: &'a TestTransport) -> Self {
        Self { transport, next_idx: std::collections::HashMap::new() }
    }

    fn wait_for<T>(
        &mut self,
        conn: ConnId,
        timeout: Duration,
        mut pred: impl FnMut(&wire::DecodedFrame) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + timeout;
        loop {
            // `sent_frames_from`, not `sent_frames`: this is a hot 10ms
            // poll loop, and a full clone on every iteration turns into
            // O(total accumulated bytes) PER POLL once a connection stays
            // subscribed through a large volume (a flood's own driver,
            // once correctly exempt from queue-overflow eviction) --
            // exactly what turned this wait into a real CI timeout despite
            // the underlying protocol machine being correct (PR #139
            // discharge round; see `sent_frames_from`'s own doc).
            let start = *self.next_idx.get(&conn).unwrap_or(&0);
            let new_frames = self.transport.sent_frames_from(start);
            let mut pos = start;
            for (c, bytes) in &new_frames {
                pos += 1;
                if *c != conn {
                    continue;
                }
                let mut s = wire::FrameSplitter::new();
                let (decoded, err) = s.feed(bytes);
                assert_eq!(err, None, "unexpected wire error in a self-encoded frame");
                for f in &decoded {
                    if let Some(v) = pred(f) {
                        // Consume up to and including the matched entry
                        // for THIS connection only; other connections'
                        // cursors are untouched, so their interleaved
                        // earlier frames stay findable.
                        self.next_idx.insert(conn, pos);
                        return v;
                    }
                }
            }
            self.next_idx.insert(conn, pos);
            if Instant::now() >= deadline {
                panic!("timed out waiting for an expected frame on conn {conn}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Collects a full checkpoint transfer's bytes for `conn` (one or more
    /// `checkpoint_chunk` frames, concatenated through `last`).
    fn collect_checkpoint(&mut self, conn: ConnId, timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
            let (last, bytes) = self.wait_for(conn, remaining, |f| {
                if let wire::DecodedFrame::AttachServer(wire::AttachServer::CheckpointChunk { last, bytes }) = f {
                    Some((*last, bytes.clone()))
                } else {
                    None
                }
            });
            out.extend(bytes);
            if last {
                return out;
            }
        }
    }
}

/// Every sealed frame across every `.sotseg` in `root/seg`, in segment
/// order — mirrors `capsule.rs`'s own test helper of the same name (not
/// shared: see `capsule_win.rs`'s module doc on duplication).
fn sealed_frames(root: &std::path::Path, voyage: &str) -> Vec<Envelope> {
    let seg_dir = root.join("seg");
    let mut out = Vec::new();
    let mut names: Vec<String> = std::fs::read_dir(&seg_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for n in names {
        if n.ends_with(".sotseg") {
            let r = SegmentReader::read(&seg_dir.join(&n), true).unwrap();
            assert_eq!(r.header.voyage_id, voyage);
            out.extend(r.frames);
        }
    }
    out
}

/// Test-only base64 decoder for `capsule_win.rs`'s encode-only engine —
/// duplicated from `capsule.rs`'s own test helper.
fn decode_b64(s: &str) -> Vec<u8> {
    let val = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => 0,
        }
    };
    let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c) << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    out
}

/// Bounded join: `run` blocks until the run ends, and a bug in the
/// teardown sequence's own ordering is exactly the class of bug that would
/// hang it forever — a test must fail loud within a bounded wait, never
/// hang the suite (or, worse, the whole CI job's own timeout — review
/// finding on the flood test specifically).
fn wait_for_join<T: Send + 'static>(handle: std::thread::JoinHandle<T>, timeout: Duration) -> Option<T> {
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Some(handle.join().unwrap())
}

/// `producer_dead` must be the LAST frame ever appended to the last
/// segment before it was sealed — the order the verifier itself does not
/// enforce (review finding). Returns its `detail` payload for further
/// assertions.
fn assert_producer_dead_is_last(frames: &[Envelope]) -> serde_json::Value {
    let last = frames.last().expect("no frames at all");
    assert_eq!(last.class, Class::Lifecycle, "producer_dead is not the last frame in the segment");
    let payload = last.payload.as_ref().unwrap();
    assert_eq!(payload["kind"], "producer_dead", "last frame is not producer_dead: {payload:?}");
    payload["detail"].clone()
}

/// Test 1: E2E. `cmd.exe /d /c echo <marker>` runs to completion (a
/// natural producer exit — no `Kill` ever sent); the resulting voyage
/// verifies, carries the marker in its producer frames, never carries a
/// turn frame (raw terminal), and ends with `producer_dead`. The
/// host-handshake exchange is a BIJECTION, not membership (review
/// finding): at most one request, matched by exactly one response and
/// exactly one outcome, linked correctly — but tolerating zero, since DA1
/// presence is host/build-version-dependent (same reasoning as
/// `tests/conpty.rs`'s own DA1-presence finding).
#[test]
fn e2e_records_and_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let marker = "SOT_CAPSULE_WIN_E2E_9f31";
    let argv = vec!["cmd.exe".to_string(), "/d".to_string(), "/c".to_string(), format!("echo {marker}")];
    let cfg = config(dir.path(), "e2e1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let (trx, mut transport) = no_transport();
    let summary = capsule_win::run(cfg, rx, trx, &mut transport).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    assert_eq!(summary.exit_code, Some(0));
    assert_eq!(summary.segments_sealed, 1);
    verify_voyage(&root, "e2e1").unwrap();

    let frames = sealed_frames(&root, "e2e1");
    let mut all = Vec::new();
    for f in &frames {
        if f.class == Class::Producer {
            let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
            all.extend(decode_b64(b64));
        }
    }
    let text = String::from_utf8_lossy(&all);
    assert!(text.contains(marker), "got: {text:?}");
    assert!(frames.iter().all(|f| f.class != Class::TurnOpen && f.class != Class::TurnClose));
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["exit_code"], 0);

    let phase_is = |f: &&Envelope, kind_ns: &str, phase: &str| {
        f.class == Class::ControlExchange
            && f.payload.as_ref().unwrap()["kind_ns"] == kind_ns
            && f.payload.as_ref().unwrap()["phase"] == phase
    };
    let hh_reqs: Vec<&Envelope> =
        frames.iter().filter(|f| phase_is(f, "conpty/host-handshake", "request")).collect();
    let hh_resps: Vec<&Envelope> =
        frames.iter().filter(|f| phase_is(f, "conpty/host-handshake", "response")).collect();
    let hh_outcomes: Vec<&Envelope> =
        frames.iter().filter(|f| phase_is(f, "conpty/host-handshake", "outcome")).collect();
    eprintln!(
        "capsule_win e2e finding: host-handshake requests={}, responses={}, outcomes={}",
        hh_reqs.len(),
        hh_resps.len(),
        hh_outcomes.len()
    );
    assert!(hh_reqs.len() <= 1, "ADR 0041's model answers the handshake at most once per run");
    assert_eq!(hh_reqs.len(), hh_resps.len(), "bijection: every request has exactly one response");
    assert_eq!(hh_resps.len(), hh_outcomes.len(), "bijection: every response has exactly one outcome");
    assert_eq!(summary.handshake_answered, !hh_reqs.is_empty());
    if let Some(&req) = hh_reqs.first() {
        let resp = hh_resps[0];
        let outcome = hh_outcomes[0];
        let responds_to = resp
            .refs
            .iter()
            .find(|r| r.kind == RefKind::RespondsTo)
            .map(|r| r.frame)
            .expect("host-handshake response missing responds_to");
        assert_eq!(responds_to, req.seq, "response must respond to ITS OWN request");
        let target = outcome.payload.as_ref().unwrap()["target"].as_str().unwrap().to_string();
        assert_eq!(target, format!("{}:{}", req.seq.epoch, req.seq.n), "outcome must target ITS OWN request");
        assert_eq!(outcome.payload.as_ref().unwrap()["body"]["disposition"], "ok");
    }
}

/// Test 2: spawn failure (a nonexistent executable) is compensated, not
/// escaped unsealed (the Linux capsule's own known gap, deliberately not
/// inherited here), and `producer_dead` is still the last frame recorded.
#[test]
fn spawn_failure_is_compensated() {
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["Z:\\sot_capsule_win_test_no_such_exe_9f31.exe".to_string()];
    let cfg = config(dir.path(), "fail1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let (trx, mut transport) = no_transport();
    let summary = capsule_win::run(cfg, rx, trx, &mut transport).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::SpawnFailed);
    assert_eq!(summary.exit_code, None);
    assert_eq!(summary.segments_sealed, 1);
    verify_voyage(&root, "fail1").unwrap();

    let frames = sealed_frames(&root, "fail1");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["spawn_failed"], true);
    assert!(dead["exit_code"].is_null());
}

/// Test 2b: an out-of-budget INITIAL geometry is treated the same way —
/// "Initial geometry is validated by the same rule" a resize is (ADR 0041).
#[test]
fn spawn_failure_from_out_of_budget_initial_geometry() {
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["cmd.exe".to_string(), "/d".to_string(), "/c".to_string(), "exit 0".to_string()];
    let cfg = config(dir.path(), "fail2", argv, 1, 25); // cols=1 < the 2-column floor
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let (trx, mut transport) = no_transport();
    let summary = capsule_win::run(cfg, rx, trx, &mut transport).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::SpawnFailed);
    verify_voyage(&root, "fail2").unwrap();
    let frames = sealed_frames(&root, "fail2");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["spawn_failed"], true);
}

/// Test 3: a requested kill (`Command::Kill`) tears down a still-running
/// producer through the ONE orchestrator and still seals a verifiable
/// voyage, with a real (job-imposed) exit code recorded as the last frame.
#[test]
fn requested_kill_tears_down_and_seals() {
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["cmd.exe".to_string()]; // bare interactive shell — stays open until killed
    let cfg = config(dir.path(), "kill1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let (trx, mut transport) = no_transport();
        capsule_win::run(cfg, rx, trx, &mut transport)
    });
    std::thread::sleep(Duration::from_millis(500));
    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    assert!(summary.exit_code.is_some());
    verify_voyage(&root, "kill1").unwrap();

    let frames = sealed_frames(&root, "kill1");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["exit_code"], summary.exit_code.unwrap());
}

/// Test 4: resize is an ordered request+outcome exchange (no response
/// phase — ADR 0041), rejecting out-of-budget requests rather than
/// clamping them, with the outcome's `target` naming ITS OWN request (not
/// just some real request — review finding), and `ResizePseudoConsole`
/// actually invoked exactly once (the in-budget request only — review
/// finding: the disposition string alone doesn't prove the OS call was
/// really gated). Step 5 deletes `Command::Resize` (ADR 0041 spec gate: the
/// wire lane replaces it) — this test now drives resize the same way a real
/// driver would: hello -> attach -> wait for the attach checkpoint -> take
/// -> three `resize` wire frames -> `resize_ok`/`resize_refused` replies.
#[test]
fn resize_ordered_exchange_commits_and_rejects() {
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["cmd.exe".to_string()];
    let cfg = config(dir.path(), "resize1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (transport, trx) = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule_win::run(cfg, rx, trx, &mut t)
    });

    const CONN: ConnId = 1;
    transport.open(CONN);
    transport.feed(CONN, frame::hello());
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for(CONN, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(CONN, frame::attach("driver"));
    watcher.collect_checkpoint(CONN, Duration::from_secs(10));
    transport.feed(CONN, frame::take("driver"));
    watcher.wait_for(CONN, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { .. })).then_some(())
    });

    transport.feed(CONN, frame::resize(100, 40)); // in budget
    let ok1 = watcher.wait_for(CONN, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
        _ => None,
    });
    transport.feed(CONN, frame::resize(9999, 40)); // > 512 cols
    let ok2 = watcher.wait_for(CONN, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
        _ => None,
    });
    transport.feed(CONN, frame::resize(40, 1)); // < 2 rows
    let ok3 = watcher.wait_for(CONN, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
        _ => None,
    });
    assert!(ok1 && !ok2 && !ok3, "expected ok, refused, refused, got {ok1} {ok2} {ok3}");

    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    assert_eq!(summary.resize_os_calls, 1, "expected exactly one ResizePseudoConsole call (the valid request only)");
    verify_voyage(&root, "resize1").unwrap();

    let frames = sealed_frames(&root, "resize1");
    let phase_is = |f: &&Envelope, phase: &str| {
        f.class == Class::ControlExchange
            && f.payload.as_ref().unwrap()["kind_ns"] == "conpty/resize"
            && f.payload.as_ref().unwrap()["phase"] == phase
    };
    let requests: Vec<&Envelope> = frames.iter().filter(|f| phase_is(f, "request")).collect();
    let outcomes: Vec<&Envelope> = frames.iter().filter(|f| phase_is(f, "outcome")).collect();
    assert_eq!(requests.len(), 3, "expected 3 resize requests, got {}", requests.len());
    assert_eq!(outcomes.len(), 3, "expected 3 resize outcomes, got {}", outcomes.len());
    assert_eq!(outcomes[0].payload.as_ref().unwrap()["body"]["disposition"], "ok");
    assert_eq!(outcomes[1].payload.as_ref().unwrap()["body"]["disposition"], "failed");
    assert_eq!(outcomes[2].payload.as_ref().unwrap()["body"]["disposition"], "failed");

    // Each outcome must target its OWN request (by emission order, since
    // request[i] and outcome[i] commit as one uninterrupted pair) — not
    // just "some" real request, which the previous version's `.any(...)`
    // would have let a misattribution bug slip through undetected.
    for (req, outcome) in requests.iter().zip(outcomes.iter()) {
        let target = outcome.payload.as_ref().unwrap()["target"].as_str().unwrap().to_string();
        let expected = format!("{}:{}", req.seq.epoch, req.seq.n);
        assert_eq!(target, expected, "outcome does not target its own request");
    }
}

/// Test 5: flood. A producer emits well beyond the 8 MiB output budget;
/// the run must drain it all to a sealed, verify-green voyage without
/// deadlocking. Whether the budget ever actually BLOCKED during the flood
/// is deliberately NOT asserted here — engagement depends on conhost's
/// burst pacing on the runner, which nothing here controls (a runner-image
/// change turned exactly that assertion red on unchanged code); the
/// blocking property is proven deterministically by OutputBudget's own
/// unit tests in capsule_win.rs. Run on a background thread with a LOCAL
/// bounded wait: a teardown regression here is exactly a deadlock, and
/// this test must fail loud within its own bound rather than consume the
/// whole CI job's timeout.
#[test]
fn flood_drains_to_a_sealed_voyage_without_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let helper = env!("CARGO_BIN_EXE_sot-conpty-helper").to_string();
    let total: usize = 20 * 1024 * 1024; // > the 8 MiB producer-channel budget
    let argv = vec![helper, "--flood".to_string(), total.to_string()];
    let cfg = config(dir.path(), "flood1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let start = Instant::now();
    let handle = std::thread::spawn(move || {
        let (trx, mut transport) = no_transport();
        capsule_win::run(cfg, rx, trx, &mut transport)
    });
    let summary = wait_for_join(handle, Duration::from_secs(60))
        .expect("run did not return within the local deadline (deadlock?)")
        .unwrap();
    eprintln!("capsule_win flood finding: {total} bytes in {:?}", start.elapsed());
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    assert_eq!(summary.exit_code, Some(0));
    verify_voyage(&root, "flood1").unwrap();

    // The right side of the transform boundary (review finding): hOutput
    // is conhost's own rendered VT stream, not a byte-for-byte copy of
    // what the child wrote to its own stdout — startup sequences and
    // line-wrap/scroll handling can legitimately change the total length,
    // so exact equality against `total` proves nothing; the half-of-total
    // bound below is the honest platform-behavior assertion.

    let frames = sealed_frames(&root, "flood1");
    let mut total_decoded = 0usize;
    for f in &frames {
        if f.class == Class::Producer {
            let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
            total_decoded += decode_b64(b64).len();
        }
    }
    assert!(
        total_decoded > total / 2,
        "captured far less output than the flood emitted: {total_decoded} of {total}"
    );
}

/// Test 6: a high-bit (NTSTATUS-shaped) exit code is preserved raw and
/// unsigned all the way through `ExitSummary` AND the sealed
/// `producer_dead` frame's JSON — the review finding that a `u32`-to-`i32`
/// cast anywhere in this path would turn it negative for no reason. Same
/// value `tests/conpty.rs` pins at the primitives layer; this proves the
/// capsule runtime doesn't reintroduce the cast above it.
#[test]
fn exit_code_high_bit_status_preserved_through_producer_dead() {
    let dir = tempfile::tempdir().unwrap();
    let argv =
        vec!["cmd.exe".to_string(), "/d".to_string(), "/c".to_string(), "exit -1073741819".to_string()];
    let cfg = config(dir.path(), "exitcode1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let (trx, mut transport) = no_transport();
    let summary = capsule_win::run(cfg, rx, trx, &mut transport).unwrap();
    assert_eq!(summary.exit_code, Some(0xC000_0005));
    verify_voyage(&root, "exitcode1").unwrap();
    let frames = sealed_frames(&root, "exitcode1");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["exit_code"], 0xC000_0005u32);
}

// ---------------------------------------------------------------------
// ADR 0041 step 5 (U2): the pipe protocol.
// ---------------------------------------------------------------------

/// Test 7: attach mid-stream, on a producer emitting escape sequences and
/// multibyte UTF-8 continuously, reproduces a from-scratch replay
/// byte-for-byte.
///
/// Rebuilt (finding 13 — the review's own diagnosis of the prior version's
/// CI failure): the producer is now `sot-conpty-helper --script`, a
/// deterministic byte-emitting helper (see its module doc), not a
/// `cmd.exe /d /c for /l ... echo` loop whose own startup latency and
/// console rendering the test could not predict or control. One-byte
/// pacing across 1000 repeats of a block containing a CSI pair, a
/// BEL-terminated OSC, an ST-terminated DCS, and a 3-byte codepoint
/// immediately followed by a 4-byte one (no ASCII separator) makes it all
/// but certain some ConPTY read lands inside every one of those sequence
/// classes across the run — "forced" by sheer repetition, not engineered to
/// land at one exact byte. The vt100 fork's own unit tests already prove
/// `is_ground` is safe to cut any of them at any byte boundary (U0); what
/// THIS test proves is the WIRING.
///
/// Two things get proved, both stronger than the prior version's:
///
/// 1. Finding 3 (queuing, not dropping): this connection's sends are held
///    from before `attach` through a window where the producer keeps
///    emitting, so whichever chunk is last never completes yet — proving
///    newly committed output queues behind an in-flight checkpoint transfer
///    (`sent_frames` must show zero `output` frames for this connection
///    while held) rather than being sent ahead of it or dropped. Releasing
///    then delivers the transfer followed immediately by every queued
///    frame, in order — the FIFO contract documented on `TestTransport`
///    above.
/// 2. Finding 13 (the U0 oracle): rather than compare rendered
///    `Screen::contents()` strings — which cannot see cursor position,
///    attributes, or mode bits that aren't in the current viewport — this
///    compares raw bytes twice: the exact tail-byte-equality of what this
///    connection received against the voyage's own recorded producer
///    bytes, and the wire checkpoint against an independently computed
///    `Screen::checkpoint()` of the exact same prefix. Checkpoint bytes are
///    a pure function of screen state (magic/version/geometry/modes/attrs/
///    grid — see `vt100_ctt::Screen::checkpoint`), so two parsers fed
///    identical byte prefixes must produce identical checkpoints; anything
///    else is either a wiring bug or a checkpoint-format non-determinism
///    this crate depends on not existing.
#[test]
fn attach_mid_stream_checkpoint_reproduces_reference_screen() {
    let dir = tempfile::tempdir().unwrap();
    let helper = env!("CARGO_BIN_EXE_sot-conpty-helper").to_string();
    // Large enough that the helper is still running long after every step
    // below — this test ends the run with an explicit `Kill`, exactly like
    // the prior version did, never by waiting for the producer to finish on
    // its own.
    let argv = vec![helper, "--script".to_string(), "1000".to_string()];
    let (rows, cols) = (25u16, 80u16);
    let cfg = config(dir.path(), "midattach1", argv, cols, rows);
    let root = cfg.voyage_root.clone();
    let (transport, trx) = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule_win::run(cfg, rx, trx, &mut t)
    });

    // Attach WHILE the producer is still actively emitting -- no attempt to
    // engineer a precise cut point; is_ground's own unit tests already
    // cover that. Real elapsed time only, no fixed assumption about where
    // the loop's ground boundary lands.
    std::thread::sleep(Duration::from_millis(150));

    const CONN: ConnId = 1;
    transport.open(CONN);
    transport.feed(CONN, frame::hello());
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for(CONN, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });

    // Finding 3: hold every send to this connection from before `attach`
    // through a window where the producer keeps emitting, so the
    // checkpoint transfer's own completion(s) never get reported while
    // more output is committed behind it.
    transport.set_hold_for(CONN, true);
    transport.feed(CONN, frame::attach("watcher"));
    // A throwaway cursor: only confirms a checkpoint chunk was actually
    // constructed and queued (`sent_frames` records the bytes at `send`
    // time, held or not) without disturbing `watcher`'s own cursor -- which
    // still needs to find that SAME chunk itself, below, once released.
    FrameWatcher::new(&transport).wait_for(CONN, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::CheckpointChunk { .. })).then_some(())
    });

    // The producer keeps emitting while the transfer sits unconfirmed.
    std::thread::sleep(Duration::from_millis(500));
    let output_frames_while_held = transport
        .sent_frames()
        .into_iter()
        .filter(|(c, _)| *c == CONN)
        .flat_map(|(_, bytes)| wire::FrameSplitter::new().feed(&bytes).0)
        .filter(|f| matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::Output { .. })))
        .count();
    assert_eq!(
        output_frames_while_held, 0,
        "post-watermark output must queue behind an unconfirmed checkpoint transfer, never be sent ahead of it"
    );

    transport.set_hold_for(CONN, false);
    transport.release_held();
    let checkpoint_bytes = watcher.collect_checkpoint(CONN, Duration::from_secs(10));

    // Let more output flow post-watermark, then end the run.
    std::thread::sleep(Duration::from_millis(300));
    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    verify_voyage(&root, "midattach1").unwrap();

    // Every `output` frame this connection ever received, in arrival order
    // (the FrameWatcher's cursor already sits right after the checkpoint).
    let mut post_watermark = Vec::new();
    for (c, bytes) in transport.sent_frames() {
        if c != CONN {
            continue;
        }
        let mut s = wire::FrameSplitter::new();
        let (decoded, _) = s.feed(&bytes);
        for f in decoded {
            if let wire::DecodedFrame::AttachServer(wire::AttachServer::Output { bytes }) = f {
                post_watermark.push(bytes);
            }
        }
    }
    assert!(!post_watermark.is_empty(), "expected at least some post-watermark output");
    let suffix: Vec<u8> = post_watermark.into_iter().flatten().collect();

    // The full, from-scratch reference: every producer byte the voyage
    // ever recorded, in order.
    let frames = sealed_frames(&root, "midattach1");
    let mut total = Vec::new();
    for f in &frames {
        if f.class == Class::Producer {
            let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
            total.extend(decode_b64(b64));
        }
    }

    // Finding 13, part 1: the post-watermark stream this connection
    // received must be the EXACT byte-for-byte tail of the voyage's total
    // producer bytes -- proves nothing was dropped, duplicated, or
    // reordered across the watermark boundary.
    assert!(suffix.len() <= total.len(), "received more post-watermark bytes than the voyage ever recorded");
    let split = total.len() - suffix.len();
    assert_eq!(
        &total[split..],
        suffix.as_slice(),
        "post-watermark output must be the exact byte-for-byte tail of the voyage"
    );
    let prefix = &total[..split];

    // Finding 13, part 2 (the U0 oracle): the wire checkpoint must be
    // byte-identical to one computed independently by feeding a fresh
    // reference parser exactly the prefix.
    let mut reference_at_watermark = vt100_ctt::Parser::new(rows, cols, 0);
    reference_at_watermark.process(prefix);
    let reference_checkpoint =
        reference_at_watermark.screen().checkpoint().expect("prefix screen must be representable");
    assert_eq!(
        checkpoint_bytes, reference_checkpoint,
        "the wire checkpoint must be byte-identical to an independently computed checkpoint of the same prefix"
    );

    // And the full round trip, at the same checkpoint-byte granularity: a
    // fresh parser restored from the wire checkpoint and replayed with the
    // exact suffix must reach a state whose OWN checkpoint is
    // byte-identical to a from-scratch parser's, fed the entire voyage.
    let mut restored = vt100_ctt::Parser::new(rows, cols, 0);
    restored.restore_screen(&checkpoint_bytes).expect("checkpoint must decode");
    restored.process(&suffix);

    let mut reference = vt100_ctt::Parser::new(rows, cols, 0);
    reference.process(&total);

    assert_eq!(
        restored.screen().checkpoint().expect("restored screen must be representable"),
        reference.screen().checkpoint().expect("reference screen must be representable"),
        "checkpoint + subsequent stream must reproduce the reference session byte-for-byte"
    );
    assert_eq!(summary.exit_kind, ExitKind::Requested);
}

/// Test 8: the wire input WAL folds every legal `idem_key` chain exactly,
/// including a stale refusal (a demoted connection's replay) and a
/// duplicate `idem_key` answered deterministically WITHOUT appending any
/// new frame — and the SAME determinism holds across a capsule restart
/// (reopen the voyage; the dedupe index is rebuilt from the retained
/// segments, not started empty — ADR 0041 decision 5's whole point).
#[test]
fn wire_input_wal_chains_including_refused_stale_and_duplicate_idem_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let name = "inputwal1";
    let root = dir.path().join(name);
    let k1 = [0x11u8; 16];
    let k2 = [0x22u8; 16];

    // --- Incarnation 1 -------------------------------------------------
    {
        let argv = vec!["cmd.exe".to_string()]; // stays open until killed
        let cfg = config(dir.path(), name, argv, 80, 25);
        let (transport, trx) = TestTransport::new();
        let (tx, rx) = mpsc::channel();
        let run_transport = transport.clone();
        let handle = std::thread::spawn(move || {
            let mut t = run_transport;
            capsule_win::run(cfg, rx, trx, &mut t)
        });

        // conn A attaches and takes -- the first driver ever, a pipe take.
        const A: ConnId = 1;
        transport.open(A);
        transport.feed(A, frame::hello());
        let mut watcher = FrameWatcher::new(&transport);
        watcher.wait_for(A, Duration::from_secs(10), |f| {
            matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
        });
        transport.feed(A, frame::attach("alice"));
        watcher.collect_checkpoint(A, Duration::from_secs(10));
        transport.feed(A, frame::take("alice"));
        let epoch = watcher.wait_for(A, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
            _ => None,
        });

        // K1: fresh input while authorized -- recorded.
        transport.feed(A, frame::input("alice", epoch, k1, b"echo one\r\n"));
        let outcome1 = watcher.wait_for(A, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded) => Some(true),
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale) => Some(false),
            _ => None,
        });
        assert!(outcome1, "expected the fresh K1 input to be recorded");

        // K1 AGAIN, same idem_key: chain is already {input,intent,forwarded}
        // -- must replay the SAME recorded outcome, appending nothing new
        // (checked after this incarnation seals, via the sealed frame count
        // for K1's idem_key, below).
        transport.feed(A, frame::input("alice", epoch, k1, b"echo one\r\n"));
        let outcome1_replay = watcher.wait_for(A, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded) => Some(true),
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale) => Some(false),
            _ => None,
        });
        assert!(outcome1_replay, "duplicate K1 must replay input_recorded");

        // conn B attaches and takes, demoting A.
        const B: ConnId = 2;
        transport.open(B);
        transport.feed(B, frame::hello());
        watcher.wait_for(B, Duration::from_secs(10), |f| {
            matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
        });
        transport.feed(B, frame::attach("bob"));
        watcher.collect_checkpoint(B, Duration::from_secs(10));
        transport.feed(B, frame::take("bob"));
        watcher.wait_for(B, Duration::from_secs(10), |f| {
            matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { .. })).then_some(())
        });

        // A tries a NEW key (K2) with its now-stale claim: demoted, so this
        // is refused -- folded into the SAME "stale" wire reply the ADR
        // defines for a durable epoch mismatch (a demoted connection is
        // indistinguishable from one on the wire).
        transport.feed(A, frame::input("alice", epoch, k2, b"echo two\r\n"));
        let outcome2 = watcher.wait_for(A, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded) => Some(true),
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale) => Some(false),
            _ => None,
        });
        assert!(!outcome2, "a demoted connection's input must be refused stale");

        tx.send(Command::Kill).unwrap();
        wait_for_join(handle, Duration::from_secs(30))
            .expect("run did not return within the teardown bound")
            .unwrap();
        verify_voyage(&root, name).unwrap();
    }

    let frames = sealed_frames(&root, name);
    let input_frames_for = |key: [u8; 16]| -> Vec<&Envelope> {
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        frames
            .iter()
            .filter(|f| f.class == Class::Input && f.payload.as_ref().unwrap()["idem_key"] == hex)
            .collect()
    };
    assert_eq!(input_frames_for(k1).len(), 1, "K1's retry must not append a second `input` frame");
    let k2_facts: Vec<&Envelope> = frames
        .iter()
        .filter(|f| {
            f.class == Class::Lifecycle
                && f.payload.as_ref().unwrap()["kind"] == "input_fact"
                && f.payload.as_ref().unwrap()["fact"]["fact"] == "refused_stale_epoch"
        })
        .collect();
    assert_eq!(k2_facts.len(), 1, "K2 must have exactly one refused_stale_epoch fact");

    // --- Incarnation 2 (a "successor capsule") --------------------------
    {
        let argv = vec!["cmd.exe".to_string()];
        let cfg = config(dir.path(), name, argv, 80, 25);
        let (transport, trx) = TestTransport::new();
        let (tx, rx) = mpsc::channel();
        let run_transport = transport.clone();
        let handle = std::thread::spawn(move || {
            let mut t = run_transport;
            capsule_win::run(cfg, rx, trx, &mut t)
        });

        const C: ConnId = 1;
        transport.open(C);
        transport.feed(C, frame::hello());
        let mut watcher = FrameWatcher::new(&transport);
        watcher.wait_for(C, Duration::from_secs(10), |f| {
            matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
        });
        transport.feed(C, frame::attach("carol"));
        watcher.collect_checkpoint(C, Duration::from_secs(10));
        transport.feed(C, frame::take("carol"));
        let epoch2 = watcher.wait_for(C, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
            _ => None,
        });

        // K1 again, from a BRAND NEW capsule incarnation, a brand new
        // connection, and a brand new controller identity: the dedupe
        // index was rebuilt from the RETAINED voyage at open, so this must
        // still replay deterministically -- exactly decision 5's point ("a
        // successor capsule starting with an empty index would let a
        // pre-crash forwarded key re-forward").
        transport.feed(C, frame::input("carol", epoch2, k1, b"echo one\r\n"));
        let replay_after_restart = watcher.wait_for(C, Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded) => Some(true),
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale) => Some(false),
            _ => None,
        });
        assert!(replay_after_restart, "K1 must still replay input_recorded after a capsule restart");

        tx.send(Command::Kill).unwrap();
        wait_for_join(handle, Duration::from_secs(30))
            .expect("run did not return within the teardown bound")
            .unwrap();
        verify_voyage(&root, name).unwrap();
    }

    // K1 must STILL have exactly one `input` frame across BOTH incarnations
    // -- the restart never re-forwarded it.
    let frames = sealed_frames(&root, name);
    let input_frames_for = |key: [u8; 16]| -> Vec<Envelope> {
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        frames
            .iter()
            .filter(|f| f.class == Class::Input && f.payload.as_ref().unwrap()["idem_key"] == hex)
            .cloned()
            .collect()
    };
    assert_eq!(input_frames_for(k1).len(), 1, "K1 must never gain a second `input` frame across a restart");
}

/// Test 9: a slow (never-draining) watcher's queued live-output bytes
/// overflow the 4 MiB per-subscriber budget and it is closed -- no wire
/// frame exists for that eviction, by design -- while the DRIVER, a
/// separate connection under the SAME flood, stays live and fully
/// functional throughout.
#[test]
fn slow_watcher_overflow_closes_while_driver_stays_live() {
    let dir = tempfile::tempdir().unwrap();
    let helper = env!("CARGO_BIN_EXE_sot-conpty-helper").to_string();
    let total: usize = 6 * 1024 * 1024; // > the 4 MiB per-watcher budget
    let argv = vec![helper, "--flood".to_string(), total.to_string()];
    let cfg = config(dir.path(), "slowwatcher1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (transport, trx) = TestTransport::new();
    let (_tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule_win::run(cfg, rx, trx, &mut t)
    });

    const DRIVER: ConnId = 1;
    const WATCHER: ConnId = 2;
    let mut watcher = FrameWatcher::new(&transport);

    transport.open(DRIVER);
    transport.feed(DRIVER, frame::hello());
    watcher.wait_for(DRIVER, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(DRIVER, frame::attach("driver"));
    watcher.collect_checkpoint(DRIVER, Duration::from_secs(10));
    transport.feed(DRIVER, frame::take("driver"));
    watcher.wait_for(DRIVER, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { .. })).then_some(())
    });

    transport.open(WATCHER);
    transport.feed(WATCHER, frame::hello());
    watcher.wait_for(WATCHER, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(WATCHER, frame::attach("watcher"));
    watcher.collect_checkpoint(WATCHER, Duration::from_secs(10));
    // Never drains from here on: every future send to WATCHER queues
    // forever, simulating a client that stopped reading its pipe.
    transport.set_hold_for(WATCHER, true);

    // Bounded poll for the watcher's own close -- the flood alone drives
    // this; no fixed sleep assumes when the budget actually trips.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if transport.closed_conns().contains(&WATCHER) {
            break;
        }
        assert!(Instant::now() < deadline, "watcher was never closed under a 6 MiB flood");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!transport.closed_conns().contains(&DRIVER), "the driver must stay live");

    // The driver is still fully functional: a resize still completes.
    transport.feed(DRIVER, frame::resize(100, 40));
    let resize_ok = watcher.wait_for(DRIVER, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
        _ => None,
    });
    assert!(resize_ok, "the driver must still be able to resize after the watcher's eviction");

    let summary = wait_for_join(handle, Duration::from_secs(60))
        .expect("run did not return within the local deadline")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    verify_voyage(&root, "slowwatcher1").unwrap();
}

/// Test 10: a refused `hello` (unsupported proto) closes only that
/// connection -- mgmt stays available (a fresh mgmt connection, per the
/// ADR: "the ADR's 'mgmt remains available' is satisfied by a fresh mgmt
/// connection"), and a LATER, protocol-compatible attach on a separate
/// connection still succeeds normally.
#[test]
fn hello_refusal_leaves_mgmt_and_later_attach_working() {
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["cmd.exe".to_string()];
    let cfg = config(dir.path(), "hellorefuse1", argv, 80, 25);
    let (transport, trx) = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule_win::run(cfg, rx, trx, &mut t)
    });
    let mut watcher = FrameWatcher::new(&transport);

    const MGMT: ConnId = 1;
    const BAD_HELLO: ConnId = 2;
    const GOOD: ConnId = 3;

    transport.open(MGMT);
    transport.feed(MGMT, frame::mgmt_probe());
    watcher.wait_for(MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ProbeOk)).then_some(())
    });

    transport.open(BAD_HELLO);
    transport.feed(
        BAD_HELLO,
        wire::encode_attach_client(&wire::AttachClient::Hello { proto: 999 }).unwrap(),
    );
    watcher.wait_for(BAD_HELLO, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloRefused { .. })).then_some(())
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if transport.closed_conns().contains(&BAD_HELLO) {
            break;
        }
        assert!(Instant::now() < deadline, "the refused hello connection was never closed");
        std::thread::sleep(Duration::from_millis(10));
    }

    // Mgmt still works on its own connection -- probe AND status, the
    // latter carrying this process's own pid/creation-time/survival.
    transport.feed(MGMT, frame::mgmt_probe());
    watcher.wait_for(MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ProbeOk)).then_some(())
    });
    transport.feed(MGMT, frame::mgmt_status());
    let (pid, survival) = watcher.wait_for(MGMT, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::MgmtReply(wire::MgmtReply::StatusOk { pid, survival, .. }) => Some((*pid, *survival)),
        _ => None,
    });
    assert_eq!(pid, std::process::id(), "status.pid must be the capsule's OWN process id");
    assert_eq!(survival, wire::Survival::Normal);

    // A fresh, compatible attach still succeeds.
    transport.open(GOOD);
    transport.feed(GOOD, frame::hello());
    watcher.wait_for(GOOD, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(GOOD, frame::attach("late"));
    watcher.collect_checkpoint(GOOD, Duration::from_secs(10));

    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
}

/// Test 11: the mgmt `shutdown_ok` ack is physically written BEFORE
/// teardown begins -- proven by holding its send completion and observing
/// `run` is still blocked, then releasing it and observing EndRun actually
/// proceeds. The reason string travels into `producer_dead`'s detail.
#[test]
fn shutdown_ack_sent_before_teardown() {
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["cmd.exe".to_string()]; // stays open until EndRun
    let cfg = config(dir.path(), "shutdownseq1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (transport, trx) = TestTransport::new();
    let (_tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule_win::run(cfg, rx, trx, &mut t)
    });

    const MGMT: ConnId = 1;
    transport.open(MGMT);
    transport.set_hold_for(MGMT, true); // hold BEFORE the request that matters
    transport.feed(MGMT, frame::mgmt_shutdown("integration-test-reason"));

    // The ack's bytes are constructed and queued...
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for(MGMT, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::MgmtReply(wire::MgmtReply::ShutdownOk)).then_some(())
    });
    // ...but `run` must NOT have begun tearing down yet: nothing has
    // reported it physically sent.
    std::thread::sleep(Duration::from_millis(200));
    assert!(!handle.is_finished(), "EndRun must not begin before the shutdown ack is reported sent");

    transport.release_held();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound after the ack was released")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    verify_voyage(&root, "shutdownseq1").unwrap();

    let frames = sealed_frames(&root, "shutdownseq1");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["reason"], "integration-test-reason");
}

/// Test 12 (finding 7, Codex review rework): `Transport::shutdown_all` is
/// actually invoked before `run` returns, on every exit path -- the one
/// piece of the teardown rework that is new wiring, not a restatement of
/// something the pure-logic `AttachProto` tests already cover. The reduced
/// legal action set teardown enforces (mgmt served, producer-bound
/// admission revoked, no lockstep leak from an ignored request) is proven
/// exhaustively and race-free at that level already
/// (`attach_proto::teardown_ignores_producer_bound_requests_but_not_mgmt_or_attach`)
/// — reproving it here against a real ConPTY would only buy a race between
/// the test thread's writes and whichever loop iteration observes them
/// first, without adding coverage `execute_teardown_actions!`'s own
/// `unreachable!` arms don't already give at compile time.
#[test]
fn shutdown_all_is_called_before_run_returns_on_every_exit_path() {
    let dir = tempfile::tempdir().unwrap();

    // Path 1: a natural producer exit.
    {
        let argv = vec!["cmd.exe".to_string(), "/d".to_string(), "/c".to_string(), "exit 0".to_string()];
        let cfg = config(dir.path(), "shutdownall1", argv, 80, 25);
        let (transport, trx) = TestTransport::new();
        let (_tx, rx) = mpsc::channel();
        let mut run_transport = transport.clone();
        let summary = capsule_win::run(cfg, rx, trx, &mut run_transport).unwrap();
        assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
        assert!(
            transport.shutdown_all_was_called(),
            "shutdown_all must be called even on a natural producer exit"
        );
    }

    // Path 2: a requested kill, with an attached connection still open --
    // proving `shutdown_all` runs even when the pipe has real state on it,
    // not only in the no-connections-ever-opened case above.
    {
        let argv = vec!["cmd.exe".to_string()]; // stays open until killed
        let cfg = config(dir.path(), "shutdownall2", argv, 80, 25);
        let (transport, trx) = TestTransport::new();
        let (tx, rx) = mpsc::channel();
        let run_transport = transport.clone();
        let handle = std::thread::spawn(move || {
            let mut t = run_transport;
            capsule_win::run(cfg, rx, trx, &mut t)
        });

        const CONN: ConnId = 1;
        transport.open(CONN);
        transport.feed(CONN, frame::hello());
        let mut watcher = FrameWatcher::new(&transport);
        watcher.wait_for(CONN, Duration::from_secs(10), |f| {
            matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
        });
        transport.feed(CONN, frame::attach("watcher"));
        watcher.collect_checkpoint(CONN, Duration::from_secs(10));

        tx.send(Command::Kill).unwrap();
        let summary = wait_for_join(handle, Duration::from_secs(30))
            .expect("run did not return within the teardown bound")
            .unwrap();
        assert_eq!(summary.exit_kind, ExitKind::Requested);
        assert!(transport.shutdown_all_was_called(), "shutdown_all must be called on a requested kill too");
    }
}
