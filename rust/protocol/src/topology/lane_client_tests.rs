//! Tests of the lane dial against a stub daemon: refusal mapping, bad-token and no-bridge replies, timeouts and cancel.

use super::*;
use interprocess::local_socket::{prelude::*, GenericFilePath, Listener, ListenerOptions, Stream};
use std::io::{Read, Write};
use std::time::Duration;

/// A fake daemon on a fresh local socket (a pipe on Windows): the one transport the lane client dials besides
/// ssh, portable, no daemon process needed. On Unix the dial reaches it through `connect_own`, which speaks only to a
/// socket in a folder private to this account, so the socket sits in a folder made 0700 here, never left to the umask.
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
        DaemonLaneEndpoint { dial: LaneDial::Local(self.path.clone()), token: None }
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
    let result = endpoint.dial("row-1", "supervisor", None).map(|c| (c.peer.pid, c.peer.created));
    handle.join().unwrap();
    result
}

/// Reads the two frames a dial writes in one write, its hello and its `lane.connect`, and answers the hello as an
/// accepting daemon does. Returns both frames as sent.
fn serve_hello(conn: &mut Stream) -> (Frame, Frame) {
    let mut reader = std::io::BufReader::new(&*conn);
    let hello = crate::codec::read_frame_blocking(&mut reader).expect("the dial's hello");
    let request = crate::codec::read_frame_blocking(&mut reader).expect("the dial's lane.connect");
    let ok = serde_json::json!({ "session_id": "s", "revision": 0, "snapshot_pending": false });
    let mut line = serde_json::to_vec(&Frame::res(hello.id, op::HELLO, ok)).unwrap();
    line.push(b'\n');
    conn.write_all(&line).unwrap();
    (hello, request)
}

fn respond_with(mut conn: Stream, payload: serde_json::Value) {
    serve_hello(&mut conn);
    let res = crate::Frame::res(1, op::LANE_CONNECT, payload);
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
    let client = endpoint.dial("row-1", "supervisor", None).expect("handshake succeeds");
    drop(client);
    let (hello, request) = handle.join().unwrap();
    assert_eq!((hello.kind, hello.op.as_str()), (Kind::Req, op::HELLO));
    let hello: crate::HelloReq = serde_json::from_value(hello.payload).expect("a HelloReq");
    assert_eq!(hello.role, crate::HANDOFF_ROLE);
    assert_eq!(hello.protocol, crate::PROTOCOL_VERSION);
    assert_eq!(hello.os_user, sot_log::identity::os_account::own_account_id());
    assert_eq!(request.op, op::LANE_CONNECT);
}

fn respond_with_ok_lane(conn: &mut Stream) {
    let res = crate::Frame::res(2, op::LANE_CONNECT, serde_json::json!({ "ok": true, "pid": 1u32, "created": 1u64 }));
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
            let hello = crate::codec::read_frame_blocking(&mut reader).expect("the dial's hello");
            let refusal = crate::Frame::res(hello.id, op::HELLO, serde_json::json!({ "error": "no thanks", "code": code }));
            let mut line = serde_json::to_vec(&refusal).unwrap();
            line.push(b'\n');
            conn.write_all(&line).unwrap();
        });
        let result = endpoint.dial("row-1", "supervisor", None).map(|_| ());
        handle.join().unwrap();
        match result {
            Err(TransportError::Refused { code: got, detail }) => assert_eq!((got.as_str(), detail.as_str()), (code, "no thanks")),
            other => panic!("expected Refused{{{code}}}, got {other:?}"),
        }
    }
}

/// ADR 0045 decision 4: a lane dial over ssh while the host's link is
/// down fails at once with `LinkDown` and starts no child.
#[test]
fn an_ssh_dial_with_a_down_gate_is_link_down_at_once() {
    let gate = crate::topology::ssh_bridge::LinkGate::default();
    gate.set_up(false);
    let recipe = crate::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap();
    let endpoint = DaemonLaneEndpoint { dial: LaneDial::Ssh(recipe, gate), token: None };
    let t0 = Instant::now();
    let result = endpoint.dial("row", "supervisor", None);
    assert!(matches!(result, Err(TransportError::LinkDown)), "got {:?}", result.err());
    assert!(t0.elapsed() < Duration::from_millis(50));
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
    assert!(matches!(result, Err(TransportError::Undetermined { via: "bridge", .. })), "got {result:?}");
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
    let result = endpoint.dial("row-1", "supervisor", None).map(|_| ());
    assert!(matches!(result, Err(TransportError::Unreachable(_))), "got {result:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    let _ = release_tx.send(());
    handle.join().unwrap();

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
        let res = crate::Frame::res(1, op::LANE_CONNECT, serde_json::json!({ "ok": true, "pid": 4242u32, "created": 99u64 }));
        let mut line = serde_json::to_vec(&res).unwrap();
        line.push(b'\n');
        conn.write_all(&line).unwrap();
        // Held open until the test says the cancelled read already
        // completed -- see this test's own doc.
        let _ = release_rx.recv_timeout(Duration::from_secs(10));
    });
    let client = endpoint.dial("row-1", "supervisor", None).expect("handshake succeeds");

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
    let (result, elapsed) = read_thread.join().unwrap();
    let _ = release_tx.send(()); // only now may the peer close
    handle.join().unwrap();

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
        let res = crate::Frame::res(1, op::LANE_CONNECT, serde_json::json!({ "ok": true, "pid": 111u32, "created": 1u64 }));
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

    let client = endpoint.dial("row-1", "supervisor", None).expect("handshake succeeds");

    struct FixedExchange;
    impl IdentityExchange for FixedExchange {
        fn encode_request(&self) -> Vec<u8> {
            b"status".to_vec()
        }
        fn feed(&mut self, _bytes: &[u8]) -> sot_log::identity::exchange::ExchangeDecode {
            sot_log::identity::exchange::ExchangeDecode::Identity { pid: 222, created: 1 }
        }
    }
    let mut exchange = FixedExchange;
    let outcome = endpoint.challenge(&client, &mut exchange, Instant::now() + std::time::Duration::from_secs(1));
    handle.join().unwrap();
    assert!(matches!(outcome, ChallengeOutcome::Foreign), "a mismatched pid must never be Proven");
}

/// A portable stand-in for the real `ssh` child: `cat` echoes stdin
/// back on stdout and outlives its parent until killed, exactly the
/// two properties `BridgedClient::cancel()`'s kill→EOF mechanism
/// needs (C3 as amended §2; §7's test table, "Linux + Windows").
#[cfg(unix)]
fn spawn_stub_child() -> std::process::Child {
    std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("`cat` must be on PATH for this test")
}

#[cfg(windows)]
fn spawn_stub_child() -> std::process::Child {
    // `more` with no filename argument reads stdin and copies it to
    // stdout, the same echo shape `cat` gives on Unix — no unix-only
    // tool required. It is `more.com`, and Command looks up only `.exe`
    // without an extension.
    std::process::Command::new("more.com")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("`more.com` must be on PATH for this test")
}

/// `BridgedClient::cancel()`'s own contract: the kill it issues must
/// be what unblocks a read already parked on the child's stdout, not
/// an eventual natural exit racing ahead of it — same property
/// `cancel_unblocks_a_pending_read` proves for the local-socket client above, one
/// mechanism for both.
#[test]
fn bridged_cancel_unblocks_a_parked_read_via_kill() {
    let client = std::sync::Arc::new(BridgedClient::wrap(spawn_stub_child()).expect("wrap"));
    let reader = std::sync::Arc::clone(&client);
    let read_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        let started = Instant::now();
        let result = reader.read(&mut buf);
        (result, started.elapsed())
    });
    // Give the read a moment to actually park before cancelling —
    // `cat`/`more` never write anything unprompted, so the read has
    // nothing to return until either bytes arrive or the child dies.
    std::thread::sleep(std::time::Duration::from_millis(100));
    client.cancel();
    let (result, elapsed) = read_thread.join().unwrap();
    assert!(elapsed < std::time::Duration::from_secs(2), "cancel() must unblock the read promptly, took {elapsed:?}");
    match result {
        Ok(0) | Err(TransportError::Cancelled) | Err(TransportError::Io { .. }) => {}
        other => panic!("expected cancel to unblock the read as EOF or an error, got {other:?}"),
    }
}

/// An ssh login stood in for by `sh`: it writes one line to stderr, reads the hello and the request, and answers with
/// the given reply lines (none at all for a login that dies first).
#[cfg(unix)]
fn ssh_stand_in(replies: &[serde_json::Value]) -> std::process::Child {
    let lines: Vec<String> = replies.iter().map(|v| serde_json::to_string(v).unwrap()).collect();
    let mut script = String::from("echo 'a line ssh wrote to stderr' >&2; read a; read b;");
    for i in 0..lines.len() {
        script.push_str(&format!(" printf '%s\\n' \"$REPLY_{i}\";"));
    }
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(script).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    for (i, line) in lines.iter().enumerate() {
        cmd.env(format!("REPLY_{i}"), line);
    }
    cmd.spawn().expect("`sh` must be on PATH for this test")
}

#[cfg(unix)]
fn handshake_over_ssh_stand_in(replies: &[serde_json::Value]) -> Result<DaemonLaneClient, TransportError> {
    let hello = Frame::req(1, op::HELLO, serde_json::json!({}));
    let request = Frame::req(2, op::LANE_CONNECT, serde_json::json!({}));
    handshake(LaneStream::Bridged(BridgedClient::wrap(ssh_stand_in(replies)).expect("wrap")), &hello, &request)
}

/// Over an ssh login a daemon's answer is returned as the daemon gave it, never replaced by the login's stderr (BLOCKER 2
/// of review round 1: a stderr line used to turn a refused hello into a line of ssh's own); only a failure in which the
/// daemon sent nothing takes the login's last line, after the error's own words.
#[cfg(unix)]
#[test]
fn a_daemons_answer_over_ssh_is_never_replaced_by_its_stderr() {
    let refused_hello = Frame::res(1, op::HELLO, serde_json::json!({ "error": "no thanks", "code": "os_user_conflict" }));
    match handshake_over_ssh_stand_in(&[serde_json::to_value(&refused_hello).unwrap()]) {
        Err(TransportError::Refused { code, detail }) => assert_eq!((code.as_str(), detail.as_str()), ("os_user_conflict", "no thanks")),
        other => panic!("a refused hello must stay Refused, got {:?}", other.map(|_| ())),
    }

    let accepted_hello = Frame::res(1, op::HELLO, serde_json::json!({ "ok": true }));
    let refused_lane = Frame::res(2, op::LANE_CONNECT, serde_json::json!({ "error": "no such row", "code": "unknown_workspace" }));
    match handshake_over_ssh_stand_in(&[serde_json::to_value(&accepted_hello).unwrap(), serde_json::to_value(&refused_lane).unwrap()]) {
        Err(TransportError::Refused { code, .. }) => assert_eq!(code, "unknown_workspace"),
        other => panic!("a refused lane.connect must stay Refused, got {:?}", other.map(|_| ())),
    }

    match handshake_over_ssh_stand_in(&[]) {
        Err(TransportError::Unreachable(e)) => {
            assert!(e.to_string().ends_with(": a line ssh wrote to stderr"), "the login's line follows the error's own words: {e}");
        }
        other => panic!("a login that died without a word is Unreachable, got {:?}", other.map(|_| ())),
    }
}

/// A failed write over an ssh login names the error's own words and the login's last line, each once (review round 2,
/// NOTE 3: the line used to replace the error's words and then be added a second time).
#[cfg(unix)]
#[test]
fn a_failed_lane_write_names_its_error_and_the_ssh_line_once() {
    // The stand-in closes its stdin, then writes its line and exits: the handshake's write finds no reader.
    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exec 0<&-; echo 'a line ssh wrote to stderr' >&2")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("`sh` must be on PATH for this test");
    let client = BridgedClient::wrap(child).expect("wrap");
    client.child.lock().unwrap().wait().expect("the stand-in exits");
    let hello = Frame::req(1, op::HELLO, serde_json::json!({}));
    let request = Frame::req(2, op::LANE_CONNECT, serde_json::json!({}));
    match handshake(LaneStream::Bridged(client), &hello, &request) {
        Err(TransportError::Unreachable(e)) => {
            // Normally the write fails (`lane write: <the io error>: <the line>`). A test running beside others can
            // have the pipe's read end held for an instant by a sibling's forked child, so the write succeeds and the
            // read ends before a frame instead (`<the read's words>: <the line>`): either way the error keeps its
            // words and the line follows them, once.
            let text = e.to_string();
            let line = "a line ssh wrote to stderr";
            assert_eq!(text.matches(line).count(), 1, "the login's line is named once: {text}");
            let words = text.strip_prefix("lane write: ").unwrap_or(&text).strip_suffix(&format!(": {line}"));
            assert!(words.is_some_and(|w| !w.is_empty()), "want `<the error's words>: <the line>`, got: {text}");
        }
        other => panic!("a write to a login that has gone is Unreachable, got {:?}", other.map(|_| ())),
    }
}

/// `diagnose` puts the ssh child's last line after the io error's own words, never in their place (review round 3,
/// NOTE 1: the handshake test above may take its read path, where `diagnose` is not called).
#[cfg(unix)]
#[test]
fn diagnose_appends_the_ssh_line_to_the_error() {
    let child = std::process::Command::new("sh")
        .arg("-c")
        .arg("echo 'a line ssh wrote to stderr' >&2")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("`sh` must be on PATH for this test");
    let client = BridgedClient::wrap(child).expect("wrap");
    client.child.lock().unwrap().wait().expect("the stand-in exits");
    // The drainer has the line before `diagnose` looks (a hang guard, not a speed bound).
    let guard = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while client.last_stderr.lock().unwrap().is_none() {
        assert!(std::time::Instant::now() < guard, "the stderr drainer never took the line");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let source = std::io::Error::from(std::io::ErrorKind::BrokenPipe);
    let want = format!("{source}: a line ssh wrote to stderr");
    let got = client.diagnose(source);
    assert_eq!(got.to_string(), want);
    assert_eq!(got.kind(), std::io::ErrorKind::BrokenPipe, "the error keeps its kind");
}
