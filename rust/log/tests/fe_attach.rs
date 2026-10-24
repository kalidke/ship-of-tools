#![cfg(target_os = "linux")]
//! `FeAttachClient` over the row socket (pass 6c-1a2): the window's and the wake's one client, against a row served
//! the way the supervisor serves it. A fake main loop owns a parser and answers the requests, as the real loop does.

use sot_log::attach_client::client::FeAttachClient;
use sot_log::endpoint::PlatformEndpoint;
use sot_log::row::{row_socket_path, RowRequest, RowRun, RowServer};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

const BOUND: Duration = Duration::from_secs(5);

/// One private runtime folder for the whole binary; each test names its own row by a hash of its own.
fn runtime_dir() -> PathBuf {
    static ONCE: Once = Once::new();
    static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
    ONCE.call_once(|| {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::Builder::new().prefix("sot-f").tempdir_in("/tmp").unwrap().keep();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::env::set_var("SOT_RUNTIME_DIR", &dir);
        // The challenge refuses a peer that started in the clock tick it is dialed in, and a test binary is both ends.
        std::thread::sleep(Duration::from_millis(50));
        *DIR.lock().unwrap() = Some(dir);
    });
    DIR.lock().unwrap().clone().unwrap()
}

/// A row hash no other test in this binary uses.
fn fresh_row() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    runtime_dir();
    format!("{:016x}", 0xfe00_0000_0000_0000u64 + NEXT.fetch_add(1, Ordering::SeqCst))
}

struct FakeLoop {
    run: Arc<RowRun>,
    written: Arc<Mutex<Vec<u8>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeLoop {
    fn new(screen: &[u8]) -> Self {
        Self::with_run(screen, "run-one")
    }

    fn with_run(screen: &[u8], run_id: &str) -> Self {
        let run = RowRun::new(run_id, "ready");
        let parser = Arc::new(Mutex::new(vt100_ctt::Parser::new(24, 80, 200)));
        parser.lock().unwrap().process(screen);
        let written = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (run, written, stop) = (Arc::clone(&run), Arc::clone(&written), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    {
                        let mut p = parser.lock().unwrap();
                        run.tick(&p);
                        for req in run.take_requests(16) {
                            match req {
                                RowRequest::Input { bytes, reply, .. } => {
                                    written.lock().unwrap().extend_from_slice(&bytes);
                                    let _ = reply.send(Ok(bytes.len()));
                                }
                                RowRequest::Resize { cols, rows, reply } => {
                                    p.screen_mut().set_size(rows, cols);
                                    let _ = reply.send(Ok(()));
                                }
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
        };
        Self { run, written, stop, thread: Some(thread) }
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

fn window(row: &str) -> FeAttachClient {
    FeAttachClient::attach(PlatformEndpoint::default(), row.to_string(), 80, 24, Box::new(|| {})).expect("start the window's client")
}

fn wake(row: &str) -> FeAttachClient {
    FeAttachClient::attach_headless(PlatformEndpoint::default(), row.to_string()).expect("start the wake's client")
}

fn pump_until(client: &mut FeAttachClient, what: &str, mut done: impl FnMut(&mut FeAttachClient) -> bool) {
    let deadline = Instant::now() + BOUND;
    loop {
        client.pump();
        if done(client) {
            return;
        }
        assert!(Instant::now() < deadline, "no {what} within {BOUND:?}; status {:?}", client.status_line());
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn attach_shows_the_screen_at_once() {
    let row = fresh_row();
    let looped = FakeLoop::new(b"\x1b[2J\x1b[Hthe screen that was already there\r\n$ ");
    let server = RowServer::bind(&row_socket_path(&row).unwrap(), Arc::clone(&looped.run)).expect("bind");
    let mut client = window(&row);
    let started = Instant::now();
    pump_until(&mut client, "checkpoint", |c| c.is_checkpointed());
    assert!(started.elapsed() < Duration::from_secs(2), "the screen shows with no new output: {:?}", started.elapsed());
    assert!(client.screen().contents().contains("the screen that was already there"), "{}", client.screen().contents());
    pump_until(&mut client, "attached", |c| c.status_line() == "attached");
    client.shutdown(Duration::from_millis(500));
    server.shutdown();
}

#[test]
fn a_demoted_window_types_again_after_a_wake() {
    let row = fresh_row();
    let looped = FakeLoop::new(b"");
    let server = RowServer::bind(&row_socket_path(&row).unwrap(), Arc::clone(&looped.run)).expect("bind");
    let mut win = window(&row);
    pump_until(&mut win, "the window's checkpoint", |c| c.is_checkpointed());
    // The wake attaches as a watcher, takes the pen to type, and leaves.
    let mut waker = wake(&row);
    pump_until(&mut waker, "the wake's checkpoint", |c| c.is_checkpointed());
    waker.send_input(b"w");
    pump_until(&mut waker, "the wake's input recorded", |c| c.recorded_bytes() == 1);
    waker.shutdown(Duration::from_millis(500));
    // The window was demoted by the wake's take; its next keystroke takes the pen back and is written.
    win.send_input(b"x");
    pump_until(&mut win, "the window's input recorded", |c| c.recorded_bytes() == 1);
    assert_eq!(looped.written(), b"wx");
    assert_eq!(win.status_line(), "attached", "the window never left the row");
    win.shutdown(Duration::from_millis(500));
    server.shutdown();
}

#[test]
fn a_redial_waits_on_the_doubling_pace() {
    use std::os::unix::net::UnixListener;
    let row = fresh_row();
    // A row that accepts and hangs up: every attempt fails the same way, so the gaps are the pace.
    let listener = UnixListener::bind(row_socket_path(&row).unwrap()).expect("bind the stand-in");
    let accepts = Arc::new(Mutex::new(Vec::<Instant>::new()));
    {
        let accepts = Arc::clone(&accepts);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
                accepts.lock().unwrap().push(Instant::now());
            }
        });
    }
    let mut client = window(&row);
    let deadline = Instant::now() + Duration::from_secs(10);
    while accepts.lock().unwrap().len() < 4 {
        client.pump();
        assert!(Instant::now() < deadline, "the client redialed {} times in 10 s", accepts.lock().unwrap().len());
        std::thread::sleep(Duration::from_millis(20));
    }
    let at = accepts.lock().unwrap().clone();
    let gaps: Vec<Duration> = at.windows(2).map(|w| w[1] - w[0]).collect();
    for (gap, floor) in gaps.iter().zip([250u64, 500, 1000]) {
        assert!(*gap >= Duration::from_millis(floor * 9 / 10), "redialed after {gap:?}, before the {floor} ms pace");
        assert!(*gap < Duration::from_millis(floor * 3 + 500), "redialed after {gap:?}, past the {floor} ms pace");
    }
    client.shutdown(Duration::from_millis(500));
}

#[test]
fn input_queued_for_an_earlier_run_is_never_written_into_the_next() {
    let row = fresh_row();
    let path = row_socket_path(&row).unwrap();
    let first = FakeLoop::new(b"");
    let server = RowServer::bind(&path, Arc::clone(&first.run)).expect("bind");
    let mut win = window(&row);
    pump_until(&mut win, "the checkpoint of the first run", |c| c.is_checkpointed());
    server.shutdown();
    pump_until(&mut win, "the client noticing the row is gone", |c| c.status_line().contains("redialing"));
    // Typed while the connection is down: it was typed into the first run.
    win.send_input(b"stale");
    let second = FakeLoop::with_run(b"", "run-two");
    let server = RowServer::bind(&path, Arc::clone(&second.run)).expect("bind the replacement");
    pump_until(&mut win, "the attach to the second run", |c| c.status_line() == "attached" && c.run() == "run-two");
    pump_until(&mut win, "the stale input counted", |c| c.inputs_discarded() >= 1);
    assert!(second.written().is_empty(), "input typed into the first run reached the second: {:?}", second.written());
    // Input typed in the second run goes through.
    win.send_input(b"fresh");
    pump_until(&mut win, "the fresh input recorded", |c| c.recorded_bytes() == 5);
    assert_eq!(second.written(), b"fresh");
    win.shutdown(Duration::from_millis(500));
    server.shutdown();
    drop(first);
}

/// A dial that takes `delay` before it hands the connection over, as a slow ssh login does.
struct SlowEndpoint {
    inner: PlatformEndpoint,
    delay: Duration,
}

impl sot_log::endpoint::Endpoint for SlowEndpoint {
    type Client = <PlatformEndpoint as sot_log::endpoint::Endpoint>::Client;
    type Process = <PlatformEndpoint as sot_log::endpoint::Endpoint>::Process;

    fn connect_row_unchallenged(&self, row: &str) -> Result<Self::Client, sot_platform::transport::TransportError> {
        std::thread::sleep(self.delay);
        self.inner.connect_row_unchallenged(row)
    }

    fn challenge(
        &self,
        conn: &Self::Client,
        exchange: &mut dyn sot_platform::identity::exchange::IdentityExchange,
        reply_deadline: Instant,
    ) -> sot_platform::identity::challenge::ChallengeOutcome<Self::Process> {
        self.inner.challenge(conn, exchange, reply_deadline)
    }

    fn authenticate_server(&self, conn: &Self::Client) -> sot_platform::identity::challenge::PeerAuthOutcome {
        self.inner.authenticate_server(conn)
    }
}

#[test]
fn a_login_that_takes_twelve_seconds_still_attaches() {
    use sot_log::endpoint::Endpoint as _;
    let _ = PlatformEndpoint::default().link_up();
    let row = fresh_row();
    let looped = FakeLoop::new(b"slow but there");
    let server = RowServer::bind(&row_socket_path(&row).unwrap(), Arc::clone(&looped.run)).expect("bind");
    let slow = SlowEndpoint { inner: PlatformEndpoint::default(), delay: Duration::from_secs(12) };
    let mut client = FeAttachClient::attach(slow, row.clone(), 80, 24, Box::new(|| {})).expect("start");
    let deadline = Instant::now() + Duration::from_secs(40);
    while !client.is_checkpointed() {
        client.pump();
        assert!(Instant::now() < deadline, "a 12 s login never attached; status {:?}", client.status_line());
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(client.screen().contents().contains("slow but there"));
    client.shutdown(Duration::from_millis(500));
    server.shutdown();
}

/// A row stand-in that demotes the attaching driver before it sends the snapshot, and records the ops it is sent.
fn demoting_standin(path: &std::path::Path) -> Arc<Mutex<Vec<String>>> {
    use sot_protocol::codec::{read_frame_blocking_blob, write_frame_blocking_blob};
    use sot_protocol::Frame;
    let listener = std::os::unix::net::UnixListener::bind(path).expect("bind the stand-in");
    let ops = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&ops);
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept");
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut out = stream;
        while let Ok((frame, blob)) = read_frame_blocking_blob(&mut reader) {
            seen.lock().unwrap().push(frame.op.clone());
            let id = frame.id;
            let mut send = |f: Frame, blob: Option<&[u8]>| write_frame_blocking_blob(&mut out, &f, blob).is_ok();
            let ok = match frame.op.as_str() {
                "row.hello" => {
                    let (pid, created) = sot_log::row::self_identity().expect("own identity");
                    send(
                        Frame::res(
                            id,
                            "row.hello",
                            serde_json::json!({"pid": pid, "created": created, "phase": "ready", "run": "run-one", "row": sot_log::row::ROW_PROTOCOL}),
                        ),
                        None,
                    )
                }
                "row.attach" => {
                    let mut parser = vt100_ctt::Parser::new(24, 80, 0);
                    parser.process(b"taken while the snapshot was on its way");
                    let checkpoint = parser.screen().checkpoint().expect("a checkpoint");
                    send(Frame::res(id, "row.attach", serde_json::json!({"run": "run-one"})), None)
                        && send(Frame::evt("row.demoted", serde_json::json!({})), None)
                        && send(
                            Frame::evt(
                                "row.snapshot",
                                serde_json::json!({"last": true, "blob": {"len": checkpoint.len(), "mime": "application/octet-stream"}}),
                            ),
                            Some(&checkpoint),
                        )
                }
                "row.input" => send(Frame::res(id, "row.input", serde_json::json!({"written": blob.map_or(0, |b| b.len())})), None),
                _ => send(Frame::res(id, &frame.op, serde_json::json!({})), None),
            };
            if !ok {
                break;
            }
        }
    });
    ops
}

#[test]
fn a_demotion_during_attach_leaves_a_watcher_whose_next_key_takes_the_pen() {
    let row = fresh_row();
    let ops = demoting_standin(&row_socket_path(&row).unwrap());
    let mut win = window(&row);
    pump_until(&mut win, "the checkpoint", |c| c.is_checkpointed());
    win.send_input(b"k");
    pump_until(&mut win, "the key recorded", |c| c.recorded_bytes() == 1);
    let ops = ops.lock().unwrap().clone();
    let take = ops.iter().position(|o| o == "row.take").unwrap_or(usize::MAX);
    let input = ops.iter().position(|o| o == "row.input").expect("the key was sent");
    assert!(take < input, "the client was demoted before its snapshot, so its key takes the pen first: {ops:?}");
    win.shutdown(Duration::from_millis(500));
}

#[test]
fn a_row_socket_path_too_long_ends_the_attach_with_a_named_error() {
    let mut client = window(&"x".repeat(300));
    let deadline = Instant::now() + BOUND;
    while !client.is_dead() {
        client.pump();
        assert!(Instant::now() < deadline, "a path that can never fit was retried: {:?}", client.status_line());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(client.status_line().contains("too long"), "{:?}", client.status_line());
}
