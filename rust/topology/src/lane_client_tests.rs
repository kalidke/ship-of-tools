//! Tests of the lane dial against a stub daemon: refusal mapping, bad-token and no-bridge replies, timeouts and cancel.

use super::*;
use std::sync::atomic::Ordering;
use interprocess::local_socket::{prelude::*, GenericFilePath, Listener, ListenerOptions, Stream};
use std::io::{Read, Write};
use sot_platform::test_wait::within;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

/// A fake daemon on a fresh local socket (a pipe on Windows): the one transport the lane client dials besides
/// ssh, portable, no daemon process needed. On Unix the socket sits in a folder made 0700 here, never left to the
/// umask, so no other account on a shared host reaches it.
struct FakeDaemon {
    path: std::path::PathBuf,
    listener: Listener,
}

impl FakeDaemon {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        // A literal `/tmp` on Unix: `sun_path` is short on macOS and `$TMPDIR` there is not.
        #[cfg(unix)]
        let path = {
            use std::os::unix::fs::DirBuilderExt;
            let dir = std::path::PathBuf::from(format!("/tmp/sot-lane-test-{}-{n}", std::process::id()));
            // A folder a crashed run of the same pid left must not make `create` panic.
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
            dir.join("d.sock")
        };
        #[cfg(windows)]
        let path = std::path::PathBuf::from(format!(r"\\.\pipe\sot-lane-test-{}-{n}", std::process::id()));
        let name = path.to_str().unwrap().to_fs_name::<GenericFilePath>().unwrap();
        let listener = ListenerOptions::new().name(name).create_sync().unwrap();
        Self { path, listener }
    }

    fn endpoint(&self) -> DaemonLaneEndpoint {
        DaemonLaneEndpoint::new(LaneDial::Local(self.path.clone()), None)
    }

    fn accept(&self) -> Stream {
        self.listener.accept().unwrap()
    }
}

#[cfg(unix)]
impl Drop for FakeDaemon {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.path.parent().unwrap());
    }
}

/// A fake daemon plays the daemon for every refusal/uncertainty case `dial` must classify (ADR 0045 decision 4).
fn dial_against<F>(respond: F) -> Result<(u32, u64), TransportError>
where
    F: FnOnce(Stream) + Send + 'static,
{
    let daemon = FakeDaemon::new();
    let endpoint = daemon.endpoint();
    let handle = std::thread::spawn(move || respond(daemon.accept()));
    let result = endpoint.dial("row-1").map(|c| (c.peer.pid, c.peer.created));
    within(BOUND, "the fake daemon", move || handle.join()).unwrap();
    result
}

/// Reads the two frames a dial writes in one write, its hello and its `lane.connect`, and answers the hello as an
/// accepting daemon does. Returns both frames as sent.
fn serve_hello(conn: &mut Stream) -> (Frame, Frame) {
    let mut reader = std::io::BufReader::new(&*conn);
    let hello = sot_protocol::codec::read_frame_blocking(&mut reader).expect("the dial's hello");
    let request = sot_protocol::codec::read_frame_blocking(&mut reader).expect("the dial's lane.connect");
    let ok = serde_json::json!({ "session_id": "s", "revision": 0, "snapshot_pending": false });
    let mut line = serde_json::to_vec(&Frame::res(hello.id, op::HELLO, ok)).unwrap();
    line.push(b'\n');
    conn.write_all(&line).unwrap();
    (hello, request)
}

fn respond_with(mut conn: Stream, payload: serde_json::Value) {
    serve_hello(&mut conn);
    let res = sot_protocol::Frame::res(1, op::LANE_CONNECT, payload);
    let mut line = serde_json::to_vec(&res).unwrap();
    line.push(b'\n');
    conn.write_all(&line).unwrap();
}

#[test]
fn refusal_codes_map_to_typed_errors() {
    for code in ["unknown_workspace", "not_capsule", "bad_lane", "foreign", "voyage_mismatch"] {
        let code = code.to_string();
        let result = dial_against(move |conn| {
            respond_with(conn, serde_json::json!({ "error": "refused", "code": code }));
        });
        match result {
            Err(TransportError::Refused { code: got, .. }) => assert_ne!(got, "no_bridge"),
            other => panic!("expected Refused, got {other:?}"),
        }
    }
}

/// Pins the wire shape: an `unauthenticated` reply whose message is not
/// the old control-loop gate's text stays `unauthenticated` (a daemon
/// older than this tree could send it; none here does) — distinct from
/// the old-daemon case below, which shares the wire code but not the
/// message.
#[test]
fn a_bridge_daemons_own_bad_token_stays_unauthenticated() {
    let result = dial_against(|conn| {
        respond_with(conn, serde_json::json!({ "error": "bad or missing token", "code": "unauthenticated" }));
    });
    match result {
        Err(TransportError::Refused { code, .. }) => assert_eq!(code, "unauthenticated"),
        other => panic!("expected Refused{{code: unauthenticated}}, got {other:?}"),
    }
}

/// ADR 0049 `## User isolation`: the dial says hello first, as a handoff naming this process's OS account, and
/// writes it together with `lane.connect`, so the hello costs no round trip.
#[test]
fn the_dial_says_a_handoff_hello_first_in_the_same_write() {
    let daemon = FakeDaemon::new();
    let endpoint = daemon.endpoint();
    let handle = std::thread::spawn(move || {
        let mut conn = daemon.accept();
        let (hello, request) = serve_hello(&mut conn);
        respond_with_ok_lane(&mut conn);
        (hello, request)
    });
    let client = endpoint.dial("row-1").expect("handshake succeeds");
    drop(client);
    let (hello, request) = within(BOUND, "the fake daemon", move || handle.join()).unwrap();
    assert_eq!((hello.kind, hello.op.as_str()), (Kind::Req, op::HELLO));
    let hello: sot_protocol::HelloReq = serde_json::from_value(hello.payload).expect("a HelloReq");
    assert_eq!(hello.role, sot_protocol::HANDOFF_ROLE);
    assert_eq!(hello.protocol, sot_protocol::PROTOCOL_VERSION);
    assert_eq!(hello.os_user, sot_platform::identity::os_account::own_account_id());
    assert_eq!(request.op, op::LANE_CONNECT);
}

fn respond_with_ok_lane(conn: &mut Stream) {
    let res = sot_protocol::Frame::res(2, op::LANE_CONNECT, serde_json::json!({ "ok": true, "pid": 1u32, "created": 1u64 }));
    let mut line = serde_json::to_vec(&res).unwrap();
    line.push(b'\n');
    conn.write_all(&line).unwrap();
}

/// A daemon that refuses the dial's hello (an older protocol, a second account on the host) is terminal: `Refused`
/// with the daemon's own code and message, never the next op's end of file.
#[test]
fn a_refused_hello_is_refused_with_its_code_and_message() {
    for code in ["protocol_mismatch", "os_user_conflict", "identity_missing"] {
        let daemon = FakeDaemon::new();
        let endpoint = daemon.endpoint();
        let handle = std::thread::spawn(move || {
            let mut conn = daemon.accept();
            let mut reader = std::io::BufReader::new(&conn);
            let hello = sot_protocol::codec::read_frame_blocking(&mut reader).expect("the dial's hello");
            let refusal = sot_protocol::Frame::res(hello.id, op::HELLO, serde_json::json!({ "error": "no thanks", "code": code }));
            let mut line = serde_json::to_vec(&refusal).unwrap();
            line.push(b'\n');
            conn.write_all(&line).unwrap();
        });
        let result = endpoint.dial("row-1").map(|_| ());
        within(BOUND, "the fake daemon", move || handle.join()).unwrap();
        match result {
            Err(TransportError::Refused { code: got, detail }) => assert_eq!((got.as_str(), detail.as_str()), (code, "no thanks")),
            other => panic!("expected Refused{{{code}}}, got {other:?}"),
        }
    }
}

/// A window's link whose far router connects every channel to `daemon` (a fake daemon's socket), in this process on
/// `rt`, with the handle the link's session would hold up. The router's channel 0 goes to a daemon that says nothing;
/// `made` counts the channels' connections.
pub(super) fn link_to(rt: &tokio::runtime::Runtime, daemon: std::path::PathBuf, made: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> (crate::link::Link, crate::link::LinkHandle) {
    use crate::link::router::{Connector, Duplex};
    let connect: Connector = std::sync::Arc::new(move || {
        let (daemon, made) = (daemon.clone(), made.clone());
        Box::pin(async move {
            made.fetch_add(1, Ordering::SeqCst);
            crate::link::connect_local(&daemon).await.map(|s| Box::new(s) as Box<dyn Duplex>)
        })
    });
    rt.block_on(async move {
        let (near, far) = tokio::io::duplex(1 << 20);
        tokio::spawn(async move {
            let (rx, tx) = tokio::io::split(far);
            let mut rx = tokio::io::BufReader::new(rx);
            let first = sot_protocol::codec::read_envelope(&mut rx).await.expect("the link's hello");
            let (control, _silent) = tokio::io::duplex(1 << 16);
            let _ = crate::link::router::serve(first, rx, tx, Box::new(control), connect).await;
        });
        let (rx, tx) = tokio::io::split(near);
        let link = crate::link::Link::over(rx, tx).await.expect("the link starts");
        let handle = crate::link::LinkHandle::default();
        handle.set_up(&link);
        (link, handle)
    })
}

pub(super) fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

/// Topology 6 and 7: a lane dial over a host's link that is down fails at once with `LinkDown` and sends nothing.
#[test]
fn a_link_dial_on_a_down_link_is_link_down_at_once() {
    let endpoint = DaemonLaneEndpoint::new(LaneDial::Link(crate::link::LinkHandle::default()), None);
    assert!(!endpoint.link_up());
    let t0 = Instant::now();
    let result = endpoint.dial("row");
    assert!(matches!(result, Err(TransportError::LinkDown)), "got {:?}", result.err());
    assert!(t0.elapsed() < Duration::from_millis(50));
}

/// Topology 7: a link dial opens a channel, and the daemon behind the router sees the handoff hello and the
/// `lane.connect` on it; the identity is the daemon's report.
#[test]
fn a_link_dial_opens_a_channel_and_reaches_the_daemon() {
    let daemon = FakeDaemon::new();
    let rt = runtime();
    let made = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (_link, handle) = link_to(&rt, daemon.path.clone(), made.clone());
    let endpoint = DaemonLaneEndpoint::new(LaneDial::Link(handle), None);
    assert!(endpoint.link_up());
    let served = std::thread::spawn(move || {
        let mut conn = daemon.accept();
        let (hello, request) = serve_hello(&mut conn);
        let ok = serde_json::json!({ "ok": true, "pid": 41, "created": 7 });
        let mut line = serde_json::to_vec(&Frame::res(2, op::LANE_CONNECT, ok)).unwrap();
        line.push(b'\n');
        conn.write_all(&line).unwrap();
        (hello.payload["role"].clone(), request.op)
    });
    let client = endpoint.dial("row-1").expect("a link dial reaches the daemon");
    assert_eq!((client.peer.pid, client.peer.created), (41, 7));
    let (role, op_name) = within(BOUND, "the fake daemon", move || served.join()).unwrap();
    assert_eq!((role, op_name.as_str()), (serde_json::json!(sot_protocol::HANDOFF_ROLE), op::LANE_CONNECT));
    assert_eq!(made.load(Ordering::SeqCst), 1, "one dial, one channel");
}

/// Topology 6: a dial in flight when its link ends is `LinkDown` at once, not at its bound: the attach worker then
/// waits for the link (`link_up`) and dials over the next one.
#[test]
fn a_link_dial_in_flight_ends_when_its_link_ends() {
    let daemon = FakeDaemon::new();
    let rt = runtime();
    let made = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (link, handle) = link_to(&rt, daemon.path.clone(), made);
    let endpoint = DaemonLaneEndpoint::new(LaneDial::Link(handle), None);
    let (accepted, release) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
    let (taken, released) = (accepted.0, release.1);
    let held = std::thread::spawn(move || {
        let conn = daemon.accept();
        taken.send(()).unwrap();
        let _ = released.recv_timeout(BOUND);
        drop(conn);
    });
    let dial = std::thread::spawn(move || {
        let result = endpoint.dial("row-1").map(|_| ());
        (result, Instant::now())
    });
    accepted.1.recv_timeout(BOUND).expect("the dial's channel reached the daemon");
    let ended = Instant::now();
    drop(link);
    let (result, at) = within(BOUND, "the dial", move || dial.join()).unwrap();
    let _ = release.0.send(());
    within(BOUND, "the fake daemon", move || held.join()).unwrap();
    let after = at.duration_since(ended);
    println!("lane dial in flight at a link's end: result={result:?} after_end_ms={}", after.as_millis());
    assert!(matches!(result, Err(TransportError::LinkDown)) && after < Duration::from_secs(5), "{result:?} {after:?} after the end");
}

/// Topology 7: a link dial is bounded: a daemon that takes the channel and says nothing is `Unreachable` at the bound
/// (here one second, a test build's), the open and the handshake counted together.
#[test]
fn a_silent_daemon_over_a_link_is_unreachable_at_its_bound() {
    let daemon = FakeDaemon::new();
    let rt = runtime();
    let made = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (_link, handle) = link_to(&rt, daemon.path.clone(), made);
    let endpoint = DaemonLaneEndpoint::new(LaneDial::Link(handle), None).with_test_handshake_bound(Duration::from_secs(1));
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let held = std::thread::spawn(move || {
        let conn = daemon.accept();
        let _ = release_rx.recv_timeout(BOUND);
        drop(conn);
    });
    let started = Instant::now();
    let result = endpoint.dial("row-1").map(|_| ());
    let took = started.elapsed();
    let _ = release_tx.send(());
    within(BOUND, "the fake daemon", move || held.join()).unwrap();
    assert!(matches!(result, Err(TransportError::Unreachable(_))), "got {result:?}");
    assert!(took >= Duration::from_secs(1) && took < Duration::from_secs(3), "the dial ended after {took:?}");
}

/// `dial_failed` is the daemon's OWN dial/authenticate step failing
/// on the far side — uncertain transport, not a confirmed absence,
/// so it must classify as `Unreachable` (retried, clock cleared),
/// never a generic `Io` a caller's absence-window accounting could
/// charge (ADR 0045 lane B4a Codex review blocker).
#[test]
fn dial_failed_is_unreachable_not_generic_io() {
    let result = dial_against(|conn| {
        respond_with(conn, serde_json::json!({ "error": "connection refused dialing the voyage socket", "code": "dial_failed" }));
    });
    assert!(matches!(result, Err(TransportError::Unreachable(_))), "got {result:?}");
}

#[test]
fn lane_absent_decodes_to_endpoint_absent() {
    let result = dial_against(|conn| {
        respond_with(
            conn,
            serde_json::json!({ "error": "the row is terminal", "code": "lane_absent", "kind": "ConnectionRefused" }),
        );
    });
    match result {
        Err(e @ TransportError::Io { .. }) => assert!(e.is_endpoint_absent()),
        other => panic!("expected an absent Io error, got {other:?}"),
    }
}

#[test]
fn an_unknown_op_reply_is_no_bridge() {
    let result = dial_against(|conn| {
        respond_with(conn, serde_json::json!({ "error": "unknown op: lane.connect" }));
    });
    match result {
        Err(TransportError::Refused { code, .. }) => assert_eq!(code, "no_bridge"),
        other => panic!("expected Refused{{code: no_bridge}}, got {other:?}"),
    }
}

#[test]
fn an_undetermined_reply_is_undetermined() {
    let result = dial_against(|conn| {
        respond_with(conn, serde_json::json!({ "error": "could not authenticate", "code": "undetermined" }));
    });
    assert!(matches!(result, Err(TransportError::Undetermined { .. })), "got {result:?}");
}

#[test]
fn a_silent_daemon_is_unreachable_within_two_seconds() {
    // Case 1: accepts the connect, then never speaks — the
    // handshake bound. A real listener that accepts and stays
    // silent (held open in a thread until this case is done)
    // exercises the "no daemon answers" outcome.
    let daemon = FakeDaemon::new();
    let endpoint = daemon.endpoint();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let conn = daemon.accept();
        let _ = release_rx.recv_timeout(Duration::from_secs(10));
        drop(conn);
    });
    let started = Instant::now();
    // `.map(|_| ())`: `DaemonLaneClient` carries no `Debug` impl (a
    // live socket/pipe handle has no useful one), so the success
    // side is dropped before the failure assertion formats `result`.
    let result = endpoint.dial("row-1").map(|_| ());
    assert!(matches!(result, Err(TransportError::Unreachable(_))), "got {result:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    let _ = release_tx.send(());
    within(BOUND, "the fake daemon", move || handle.join()).unwrap();

    // Case 2: accepts, then never answers — the handshake bound.
    let started = Instant::now();
    let result = dial_against(|conn| {
        // Hold the connection open, answering nothing, until the
        // client's own read timeout gives up.
        std::thread::sleep(std::time::Duration::from_millis(2500));
        drop(conn);
    });
    assert!(matches!(result, Err(TransportError::Unreachable(_))), "got {result:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
}

/// A cancel mid-read must be what unblocks it, not an eventual peer
/// close racing ahead of the cancel — so the peer stays open for up
/// to 10 s (far past any real cancel latency) while the test asserts
/// the read actually completed in well under 2 s (ADR 0045 lane B4a
/// Codex review SHOULD-FIX: the previous version's peer closed after
/// 500 ms, so the test could pass even if `cancel()` did nothing).
#[test]
fn cancel_unblocks_a_pending_read() {
    let daemon = FakeDaemon::new();
    let endpoint = daemon.endpoint();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let mut conn = daemon.accept();
        serve_hello(&mut conn);
        let res = sot_protocol::Frame::res(1, op::LANE_CONNECT, serde_json::json!({ "ok": true, "pid": 4242u32, "created": 99u64 }));
        let mut line = serde_json::to_vec(&res).unwrap();
        line.push(b'\n');
        conn.write_all(&line).unwrap();
        // Held open until the test says the cancelled read already
        // completed -- see this test's own doc.
        let _ = release_rx.recv_timeout(Duration::from_secs(10));
    });
    let client = endpoint.dial("row-1").expect("handshake succeeds");

    let client = std::sync::Arc::new(client);
    let reader = std::sync::Arc::clone(&client);
    let read_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        let started = Instant::now();
        let result = reader.read(&mut buf);
        (result, started.elapsed())
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    client.cancel();
    let (result, elapsed) = within(BOUND, "the parked read", move || read_thread.join()).unwrap();
    let _ = release_tx.send(()); // only now may the peer close
    within(BOUND, "the fake daemon", move || handle.join()).unwrap();

    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "cancel() itself must unblock the read (peer stayed open 10s) -- took {elapsed:?}"
    );
    // `shutdown(Both)` on a Unix stream unblocks a pending LOCAL
    // read as ordered EOF (`Ok(0)`) — it marks this end fully closed,
    // it does not raise an error the way `PipeClient::cancel`'s own
    // OVERLAPPED cancel does. Either outcome proves cancel() (not
    // the still-open peer) produced the completion.
    match result {
        Ok(0) | Err(TransportError::Cancelled) | Err(TransportError::Io { .. }) => {}
        other => panic!("expected cancel to unblock the read as EOF or an error, got {other:?}"),
    }
}

#[test]
fn a_wire_identity_that_differs_from_the_reported_peer_is_foreign() {
    let daemon = FakeDaemon::new();
    let endpoint = daemon.endpoint();
    let handle = std::thread::spawn(move || {
        let mut conn = daemon.accept();
        serve_hello(&mut conn);
        let res = sot_protocol::Frame::res(1, op::LANE_CONNECT, serde_json::json!({ "ok": true, "pid": 111u32, "created": 1u64 }));
        let mut line = serde_json::to_vec(&res).unwrap();
        line.push(b'\n');
        conn.write_all(&line).unwrap();
        // The wire hello: read whatever `exchange_identity` sends,
        // answer with anything — `FixedExchange::feed` below ignores
        // the bytes and always decodes pid=222, deliberately NOT the
        // 111 just reported, which is the mismatch this test proves.
        let mut hello_buf = [0u8; 256];
        let _ = conn.read(&mut hello_buf);
        let _ = conn.write_all(b"irrelevant");
    });

    let client = endpoint.dial("row-1").expect("handshake succeeds");

    struct FixedExchange;
    impl IdentityExchange for FixedExchange {
        fn encode_request(&self) -> Vec<u8> {
            b"status".to_vec()
        }
        fn feed(&mut self, _bytes: &[u8]) -> sot_platform::identity::exchange::ExchangeDecode {
            sot_platform::identity::exchange::ExchangeDecode::Identity { pid: 222, created: 1 }
        }
    }
    let mut exchange = FixedExchange;
    let outcome = endpoint.challenge(&client, &mut exchange, Instant::now() + std::time::Duration::from_secs(1));
    within(BOUND, "the fake daemon", move || handle.join()).unwrap();
    assert!(matches!(outcome, ChallengeOutcome::Foreign), "a mismatched pid must never be Proven");
}

/// ADR 0049, User isolation: a lane dial refuses a socket another OS account listens on, and that listener gets no
/// byte.
#[cfg(unix)]
#[test]
fn a_socket_another_account_listens_on_is_refused_by_the_lane_dial() {
    if !sot_platform::test_isolated::run_isolated("lane_client::tests::a_socket_another_account_listens_on_is_refused_by_the_lane_dial") {
        return;
    }
    let Some(foreign) = sot_platform::test_foreign::ForeignListener::start(false) else {
        return;
    };
    let endpoint = DaemonLaneEndpoint::new(LaneDial::Local(foreign.path.clone()), None);
    let refused = match endpoint.dial("row-1") {
        Ok(_) => String::from("connected"),
        Err(e) => e.to_string(),
    };
    assert_eq!(
        foreign.finish(),
        0,
        "the lane dial sent another account's listener bytes"
    );
    assert!(
        refused.contains("another OS account listens on this socket"),
        "{refused}"
    );
}

/// ADR 0049, User isolation: a lane dial to a socket whose backlog another OS account has filled returns within
/// `CONNECT_BOUND` plus 2 s of slack, and its error is not the account refusal, so the backlog was full.
#[cfg(unix)]
#[test]
fn a_full_foreign_backlog_ends_the_lane_dial_within_its_bound() {
    if !sot_platform::test_isolated::run_isolated(
        "lane_client::tests::a_full_foreign_backlog_ends_the_lane_dial_within_its_bound",
    ) {
        return;
    }
    let Some(foreign) = sot_platform::test_foreign::ForeignListener::start(true) else {
        return;
    };
    let endpoint = DaemonLaneEndpoint::new(LaneDial::Local(foreign.path.clone()), None);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(endpoint.dial("row-1").map(|_| ()));
    });
    let result = rx
        .recv_timeout(CONNECT_BOUND + Duration::from_secs(2))
        .expect("the lane dial did not end within its bound");
    let err = result.expect_err("connected through a full backlog");
    assert!(
        !err.to_string().contains("not connecting"),
        "the connect went through, so the backlog was not full: {err}"
    );
    assert_eq!(
        foreign.finish(),
        0,
        "the lane dial sent another account's listener bytes"
    );
}
