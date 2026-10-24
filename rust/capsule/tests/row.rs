#![cfg(unix)]
//! The row socket (pass 6c-1a1): a row's attach through the blocking row client, against the run side the supervisor's
//! main loop drives. A fake main loop stands in for the loop here: it owns a parser, ticks the run and answers the
//! requests, as the real one does.

use sot_capsule::row::{
    socket_path_in, RowClient, RowError, RowEvent, RowRequest, RowRun, RowServer, ROW_PROTOCOL,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BOUND: Duration = Duration::from_secs(5);

/// What the fake main loop does with a request.
#[derive(Clone, Default)]
struct Behavior {
    /// Takes input and never answers it (an agent that reads nothing).
    hold_input_replies: bool,
    /// Fails an OS resize to this many columns.
    fail_resize_cols: Option<u16>,
    /// Starts paused: nothing is serviced until `resume`.
    paused: bool,
}

struct FakeLoop {
    run: Arc<RowRun>,
    parser: Arc<Mutex<vt100_ctt::Parser>>,
    written: Arc<Mutex<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeLoop {
    fn new(screen: &[u8]) -> Self {
        Self::with(screen, Behavior::default())
    }

    fn with(screen: &[u8], behavior: Behavior) -> Self {
        let run = RowRun::new("run-one", "ready");
        let parser = Arc::new(Mutex::new(vt100_ctt::Parser::new(24, 80, 200)));
        parser.lock().unwrap().process(screen);
        let written = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(behavior.paused));
        let thread = {
            let (run, parser, written, stop, paused) =
                (Arc::clone(&run), Arc::clone(&parser), Arc::clone(&written), Arc::clone(&stop), Arc::clone(&paused));
            std::thread::spawn(move || {
                let mut held = Vec::new();
                while !stop.load(Ordering::Acquire) {
                    if !paused.load(Ordering::Acquire) {
                        let mut p = parser.lock().unwrap();
                        // The screen is served before the pass's requests, as the loop may: an attach's own resize
                        // must not depend on the order.
                        run.tick(&p);
                        for req in run.take_requests(16) {
                            match req {
                                RowRequest::Input { bytes, reply, .. } => {
                                    written.lock().unwrap().extend_from_slice(&bytes);
                                    if behavior.hold_input_replies {
                                        held.push(reply);
                                    } else {
                                        let _ = reply.send(Ok(bytes.len()));
                                    }
                                }
                                RowRequest::Resize { cols, rows, reply } => {
                                    if behavior.fail_resize_cols == Some(cols) {
                                        let _ = reply.send(Err("the terminal refused the resize".into()));
                                    } else {
                                        p.screen_mut().set_size(rows, cols);
                                        let _ = reply.send(Ok(()));
                                    }
                                }
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
        };
        Self { run, parser, written, stop, paused, thread: Some(thread) }
    }

    fn resume(&self) {
        self.paused.store(false, Ordering::Release);
    }

    fn feed(&self, bytes: &[u8]) {
        let mut p = self.parser.lock().unwrap();
        p.process(bytes);
        self.run.output(bytes, &p);
    }

    fn written(&self) -> Vec<u8> {
        self.written.lock().unwrap().clone()
    }
}

impl Drop for FakeLoop {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The challenge refuses a peer that started in the clock tick it connects in (a pid reused inside one tick cannot be
/// told apart), and a test binary's one process is both ends: the first test waits until the process is older.
fn socket_in(dir: &Path) -> PathBuf {
    static AGED: std::sync::Once = std::sync::Once::new();
    AGED.call_once(|| std::thread::sleep(Duration::from_millis(50)));
    socket_path_in(dir, "0123456789abcdef").expect("a short path fits")
}

/// Reads events until the snapshot ends; the screen it restores.
fn snapshot_screen(client: &mut RowClient, within: Duration) -> vt100_ctt::Parser {
    let deadline = Instant::now() + within;
    let mut bytes = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match client.next_event(left).expect("the connection holds") {
            Some(RowEvent::Snapshot { bytes: b, last }) => {
                bytes.extend_from_slice(&b);
                if last {
                    break;
                }
            }
            Some(other) => panic!("an event before the snapshot ended: {other:?}"),
            None => panic!("no snapshot within {within:?}"),
        }
    }
    let mut parser = vt100_ctt::Parser::new(24, 80, 200);
    parser.restore_screen(&bytes).expect("the snapshot restores");
    parser
}

fn expect_event(client: &mut RowClient, want: impl Fn(&RowEvent) -> bool, what: &str) {
    let deadline = Instant::now() + BOUND;
    while Instant::now() < deadline {
        if let Some(ev) = client.next_event(Duration::from_millis(100)).expect("the connection holds") {
            if want(&ev) {
                return;
            }
        }
    }
    panic!("no {what} within {BOUND:?}");
}

#[test]
fn attach_shows_the_screen_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_in(dir.path());
    let looped = FakeLoop::new(b"\x1b[2J\x1b[Hthe screen that was already there\r\n$ ");
    let server = RowServer::bind(&path, Arc::clone(&looped.run)).expect("bind");
    let mut client = RowClient::connect(&path, "test", BOUND).expect("connect");
    let started = Instant::now();
    client.attach(80, 24, true).expect("attach");
    let screen = snapshot_screen(&mut client, Duration::from_secs(1));
    assert!(started.elapsed() < Duration::from_secs(1), "the screen shows within a second with no new output");
    assert!(screen.screen().contents().contains("the screen that was already there"), "{}", screen.screen().contents());
    // Output after the snapshot arrives in order.
    looped.feed(b"one ");
    looped.feed(b"two");
    let mut got = Vec::new();
    let deadline = Instant::now() + BOUND;
    while got.len() < 7 && Instant::now() < deadline {
        if let Some(RowEvent::Output(b)) = client.next_event(Duration::from_millis(100)).unwrap() {
            got.extend_from_slice(&b);
        }
    }
    assert_eq!(got, b"one two");
    server.shutdown();
}

#[test]
fn a_watcher_cannot_type() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_in(dir.path());
    let looped = FakeLoop::new(b"");
    let server = RowServer::bind(&path, Arc::clone(&looped.run)).expect("bind");
    let mut watcher = RowClient::connect(&path, "test", BOUND).expect("connect");
    watcher.attach(80, 24, false).expect("attach as a watcher");
    let err = watcher.send_input(b"x").unwrap_err();
    assert!(matches!(err, RowError::Refused(_)), "a watcher's input is refused by name: {err:?}");
    assert!(matches!(watcher.resize(100, 30).unwrap_err(), RowError::Refused(_)), "a watcher cannot resize");
    std::thread::sleep(Duration::from_millis(100));
    assert!(looped.written().is_empty(), "a refused input writes nothing");
    // Taking the pen makes it the driver.
    watcher.take().expect("take");
    assert_eq!(watcher.send_input(b"y").expect("a driver types"), 1);
    assert_eq!(looped.written(), b"y");
    server.shutdown();
}

#[test]
fn a_new_driver_demotes_the_old() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_in(dir.path());
    let looped = FakeLoop::new(b"");
    let server = RowServer::bind(&path, Arc::clone(&looped.run)).expect("bind");
    let mut first = RowClient::connect(&path, "test", BOUND).expect("connect");
    first.attach(80, 24, true).expect("attach as driver");
    let mut second = RowClient::connect(&path, "test", BOUND).expect("connect");
    second.attach(80, 24, true).expect("a second driver attaches");
    expect_event(&mut first, |e| *e == RowEvent::Demoted, "demotion for the first driver");
    assert!(matches!(first.send_input(b"a").unwrap_err(), RowError::Refused(_)), "a demoted driver's input is refused");
    assert!(looped.written().is_empty());
    assert_eq!(second.send_input(b"b").expect("the new driver types"), 1);
    // Taking back demotes the other way.
    first.take().expect("take");
    expect_event(&mut second, |e| *e == RowEvent::Demoted, "demotion for the second driver");
    assert_eq!(first.send_input(b"c").expect("the first types again"), 1);
    assert_eq!(looped.written(), b"bc");
    server.shutdown();
}

/// A row stand-in that speaks the hello with a chosen row number and every envelope at `v: 3`.
fn spawn_test_row(path: &Path, row_number: u32, wire_v: u32) -> std::thread::JoinHandle<()> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    let listener = UnixListener::bind(path).expect("bind the stand-in");
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut out = stream;
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let hello: sot_protocol::Frame = serde_json::from_str(line.trim_end()).expect("a hello frame");
        assert_eq!(hello.op, "row.hello");
        let (pid, created) = sot_capsule::row::self_identity().expect("own identity");
        let mut res = sot_protocol::Frame::res(
            hello.id,
            "row.hello",
            serde_json::json!({"pid": pid, "created": created, "phase": "ready", "run": "run-one", "row": row_number}),
        );
        res.v = wire_v;
        let mut text = serde_json::to_vec(&res).unwrap();
        text.push(b'\n');
        out.write_all(&text).unwrap();
        line.clear();
        if reader.read_line(&mut line).unwrap() == 0 {
            return;
        }
        let attach: sot_protocol::Frame = serde_json::from_str(line.trim_end()).expect("an attach frame");
        let mut res = sot_protocol::Frame::res(attach.id, "row.attach", serde_json::json!({"run": "run-one"}));
        res.v = wire_v;
        let mut parser = vt100_ctt::Parser::new(24, 80, 0);
        parser.process(b"seen through an older wire integer");
        let checkpoint = parser.screen().checkpoint().expect("a checkpoint");
        let mut evt = sot_protocol::Frame::evt(
            "row.snapshot",
            serde_json::json!({"last": true, "blob": {"len": checkpoint.len(), "mime": "application/octet-stream"}}),
        );
        evt.v = wire_v;
        for f in [&res, &evt] {
            let mut text = serde_json::to_vec(f).unwrap();
            text.push(b'\n');
            out.write_all(&text).unwrap();
        }
        out.write_all(&checkpoint).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    })
}

#[test]
fn a_row_that_declares_an_older_wire_integer_and_the_current_row_number_still_attaches() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_in(dir.path());
    let older = sot_protocol::PROTOCOL_VERSION - 1;
    let row = spawn_test_row(&path, ROW_PROTOCOL, older);
    let mut client = RowClient::connect(&path, "test", BOUND).expect("the current row number connects whatever v says");
    client.attach(80, 24, true).expect("attach");
    let screen = snapshot_screen(&mut client, BOUND);
    assert!(screen.screen().contents().contains("seen through an older wire integer"));
    row.join().unwrap();
}

#[test]
fn a_row_with_a_newer_row_number_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_in(dir.path());
    let row = spawn_test_row(&path, ROW_PROTOCOL + 1, sot_protocol::PROTOCOL_VERSION + 1);
    let started = Instant::now();
    let err = RowClient::connect(&path, "test", Duration::from_secs(3)).err().expect("a newer row is refused");
    assert!(started.elapsed() < Duration::from_secs(3), "refused within the hello bound, not by timeout");
    assert!(
        matches!(err, RowError::RowMismatch { ours, theirs } if ours == ROW_PROTOCOL && theirs == ROW_PROTOCOL + 1),
        "named: {err:?}"
    );
    row.join().unwrap();
}

#[test]
fn a_peer_of_another_uid_is_dropped_at_accept() {
    use std::io::{Read, Write};
    let dir = tempfile::tempdir().unwrap();
    let path = socket_in(dir.path());
    let looped = FakeLoop::new(b"");
    // The server takes another uid for its own, so this process, which connects, reads as a foreign peer.
    let own = sot_platform::host::state_dir::current_uid();
    let server = RowServer::bind_as_uid_for_test(&path, Arc::clone(&looped.run), own.wrapping_add(1)).expect("bind");
    let mut raw = std::os::unix::net::UnixStream::connect(&path).expect("the socket accepts the connect");
    raw.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let _ = raw.write_all(b"{\"v\":3,\"id\":1,\"kind\":\"req\",\"op\":\"row.hello\",\"payload\":{\"build\":\"t\",\"row\":1}}\n");
    let mut buf = [0u8; 64];
    match raw.read(&mut buf) {
        Ok(0) => {}
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
        other => panic!("a foreign peer is dropped at accept (closed, nothing served), not left waiting: {other:?}"),
    }
    server.shutdown();
}

#[test]
fn a_row_socket_path_past_sun_path_is_refused_by_name() {
    let long = PathBuf::from(format!("/{}", "d".repeat(300)));
    let err = socket_path_in(&long, "0123456789abcdef").unwrap_err();
    assert!(matches!(err, RowError::PathTooLong(_)), "{err:?}");
    let looped = FakeLoop::new(b"");
    let err = RowServer::bind(&long.join("row-x.sock"), Arc::clone(&looped.run)).err().expect("bind refuses");
    assert!(matches!(err, RowError::PathTooLong(_)), "bind names it before touching the filesystem: {err:?}");
    // The `row-` stem and the hash fit macOS's 104 bytes under a folder of 60.
    let fits = PathBuf::from(format!("/{}", "d".repeat(59)));
    let path = socket_path_in(&fits, "0123456789abcdef").expect("fits");
    assert!(path.as_os_str().len() <= 104, "{}", path.as_os_str().len());
}

#[test]
fn reattaching_as_a_watcher_gives_up_the_driver() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_in(dir.path());
    let looped = FakeLoop::new(b"");
    let server = RowServer::bind(&path, Arc::clone(&looped.run)).expect("bind");
    let mut client = RowClient::connect(&path, "test", BOUND).expect("connect");
    client.attach(80, 24, true).expect("attach as driver");
    assert_eq!(client.send_input(b"a").expect("a driver types"), 1);
    client.attach(80, 24, false).expect("reattach as a watcher");
    assert!(matches!(client.send_input(b"b").unwrap_err(), RowError::Refused(_)), "a watcher cannot type");
    assert!(matches!(client.resize(100, 30).unwrap_err(), RowError::Refused(_)), "a watcher cannot resize");
    assert_eq!(looped.written(), b"a");
    server.shutdown();
}

#[test]
fn a_failed_os_resize_is_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_in(dir.path());
    let looped = FakeLoop::with(b"", Behavior { fail_resize_cols: Some(99), ..Behavior::default() });
    let server = RowServer::bind(&path, Arc::clone(&looped.run)).expect("bind");
    let mut client = RowClient::connect(&path, "test", BOUND).expect("connect");
    client.attach(80, 24, true).expect("attach as driver");
    client.resize(100, 30).expect("a resize the terminal took");
    let err = client.resize(99, 20).unwrap_err();
    assert!(matches!(err, RowError::Refused(_)), "a resize the terminal refused is refused: {err:?}");
    server.shutdown();
}

#[test]
fn a_drivers_attach_gets_its_own_geometry_in_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_in(dir.path());
    let looped = FakeLoop::new(b"\x1b[2J\x1b[Hsized for the attaching window");
    let server = RowServer::bind(&path, Arc::clone(&looped.run)).expect("bind");
    let mut client = RowClient::connect(&path, "test", BOUND).expect("connect");
    client.attach(100, 30, true).expect("attach as driver of another size");
    let deadline = Instant::now() + BOUND;
    let mut bytes = Vec::new();
    loop {
        match client.next_event(Duration::from_millis(100)).unwrap() {
            Some(RowEvent::Snapshot { bytes: b, last }) => {
                bytes.extend_from_slice(&b);
                if last {
                    break;
                }
            }
            _ => assert!(Instant::now() < deadline, "no snapshot"),
        }
    }
    let mut parser = vt100_ctt::Parser::new(24, 80, 0);
    parser.restore_screen(&bytes).expect("restore");
    assert_eq!(parser.screen().size(), (30, 100), "the snapshot is cut after the attach's own resize");
    server.shutdown();
}

#[test]
fn input_that_times_out_is_delivery_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_in(dir.path());
    let looped = FakeLoop::with(b"", Behavior { hold_input_replies: true, ..Behavior::default() });
    looped.run.set_bounds_for_test(Duration::from_secs(5), Duration::from_millis(300));
    let server = RowServer::bind(&path, Arc::clone(&looped.run)).expect("bind");
    let mut client = RowClient::connect(&path, "test", BOUND).expect("connect");
    client.attach(80, 24, true).expect("attach");
    let err = client.send_input(b"x").unwrap_err();
    assert!(matches!(err, RowError::DeliveryUnknown), "an unanswered input is delivery unknown, never refused: {err:?}");
    server.shutdown();
}

#[test]
fn a_request_left_over_when_the_run_changes_is_answered_run_ended() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_in(dir.path());
    let looped = FakeLoop::with(b"", Behavior { paused: true, ..Behavior::default() });
    let server = RowServer::bind(&path, Arc::clone(&looped.run)).expect("bind");
    let mut client = RowClient::connect(&path, "test", BOUND).expect("connect");
    client.take().expect("take");
    // The attach of a driver waits on the loop, so this client takes the pen as a watcher would.
    let typed = std::thread::spawn(move || client.send_input(b"late"));
    std::thread::sleep(Duration::from_millis(200));
    looped.run.set_run("run-two");
    looped.resume();
    let err = typed.join().unwrap().unwrap_err();
    assert!(err.to_string().contains("run ended"), "{err}");
    std::thread::sleep(Duration::from_millis(100));
    assert!(looped.written().is_empty(), "input sent to one run is never written into the next");
    server.shutdown();
}

#[test]
fn a_peer_that_cannot_be_proven_is_not_reported_as_a_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_in(dir.path());
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let closer = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        drop(stream);
    });
    let err = RowClient::connect(&path, "test", Duration::from_secs(3)).err().expect("no proof");
    assert!(matches!(err, RowError::Unproven), "an immediate failure is not a deadline: {err:?}");
    closer.join().unwrap();
}

#[test]
fn a_peer_that_trickles_bytes_before_hello_is_dropped_at_the_deadline() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = socket_in(dir.path());
    let looped = FakeLoop::new(b"");
    looped.run.set_bounds_for_test(Duration::from_millis(800), Duration::from_secs(2));
    let server = RowServer::bind(&path, Arc::clone(&looped.run)).expect("bind");
    let mut raw = std::os::unix::net::UnixStream::connect(&path).unwrap();
    let started = Instant::now();
    let mut dropped_at = None;
    while started.elapsed() < Duration::from_secs(4) {
        if raw.write_all(b" ").is_err() {
            dropped_at = Some(started.elapsed());
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    let at = dropped_at.expect("a peer that trickles is dropped, not held");
    assert!(at < Duration::from_millis(2500), "dropped at the hello's one deadline, not after each read: {at:?}");
    server.shutdown();
}
