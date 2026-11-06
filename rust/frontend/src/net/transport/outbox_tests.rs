//! A request that must not run twice is written on one connection at most, and its caller gets one outcome. The three
//! disconnect tests run the real reconnect loop (`spawn`) against a fixture daemon on a local socket that counts the
//! `page.reply` requests it receives, whole, on each connection.

use super::*;
use interprocess::local_socket::{tokio::prelude::*, GenericFilePath, ListenerOptions};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

#[derive(Clone, Copy)]
struct NoWindow;
impl Redraw for NoWindow {
    fn request_redraw(&self) {}
}

fn hello_ok() -> serde_json::Value {
    serde_json::json!({ "session_id": "sess-1", "revision": 0, "snapshot_pending": false })
}

type Events = std::sync::mpsc::Receiver<(HostKey, IncomingEvt)>;

/// What a fixture daemon's connection does after it has answered the hello and the two preamble requests.
#[derive(Clone, Copy)]
enum Then {
    /// Answers every request, the `page.reply` included, until the window ends it.
    Serve,
    /// Waits for the test's release, then closes without reading anything more.
    CloseOnRelease,
    /// Reads 100 kB of what follows, then closes: a request too large to be written whole is cut part way.
    CloseMidRequest,
    /// Reads one whole request, never answers it, and closes on release.
    AcceptThenCloseOnRelease,
}

struct Fixture {
    /// `page.reply` requests received whole, by the index of the connection that carried them.
    page_replies: Arc<std::sync::Mutex<Vec<usize>>>,
    /// Connections that completed their handshake.
    up: Arc<AtomicUsize>,
    release: Arc<tokio::sync::Notify>,
    path: std::path::PathBuf,
}

impl Fixture {
    /// The first connection does `first`; every later one serves.
    fn start(rt: &tokio::runtime::Runtime, tag: &str, first: Then) -> Self {
        #[cfg(windows)]
        let path = std::path::PathBuf::from(format!(r"\\.\pipe\sot-{tag}-{}", std::process::id()));
        #[cfg(not(windows))]
        let path = {
            use std::os::unix::fs::DirBuilderExt;
            let dir = std::path::PathBuf::from(format!("/tmp/sot-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::DirBuilder::new().mode(0o700).create(&dir).expect("private folder");
            dir.join("s.sock")
        };
        let fixture = Self {
            page_replies: Arc::default(),
            up: Arc::default(),
            release: Arc::new(tokio::sync::Notify::new()),
            path,
        };
        let (seen, up, release) = (fixture.page_replies.clone(), fixture.up.clone(), fixture.release.clone());
        let name = fixture.path.to_str().unwrap().to_fs_name::<GenericFilePath>().unwrap();
        let listener = rt.block_on(async { ListenerOptions::new().name(name).create_tokio().expect("bind the fixture") });
        rt.spawn(async move {
            for index in 0.. {
                let Ok(conn) = listener.accept().await else { return };
                let then = if index == 0 { first } else { Then::Serve };
                let (seen, up, release) = (seen.clone(), up.clone(), release.clone());
                tokio::spawn(async move {
                    let (rx, mut tx) = conn.split();
                    let mut rx = codec::buffered(rx);
                    for n in 0..3 {
                        let Ok((req, _)) = codec::read_frame(&mut rx).await else { return };
                        let reply = if n == 0 { hello_ok() } else { serde_json::json!({ "error": "no", "code": "io" }) };
                        if codec::write_frame(&mut tx, &Frame::res(req.id, &req.op, reply).with_rev(0), None).await.is_err() {
                            return;
                        }
                    }
                    up.fetch_add(1, Ordering::SeqCst);
                    match then {
                        Then::Serve => {
                            while let Ok((req, _)) = codec::read_frame(&mut rx).await {
                                if req.op == op::PAGE_REPLY {
                                    seen.lock().unwrap().push(index);
                                }
                                let reply = if req.op == op::PAGE_REPLY {
                                    serde_json::json!({ "outcome": "sent", "detail": "typed" })
                                } else {
                                    serde_json::json!({ "ok": true })
                                };
                                if codec::write_frame(&mut tx, &Frame::res(req.id, &req.op, reply), None).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Then::CloseOnRelease => release.notified().await,
                        Then::CloseMidRequest => {
                            let (mut got, mut buf) = (0usize, vec![0u8; 16 * 1024]);
                            while got < 100_000 {
                                match rx.read(&mut buf).await {
                                    Ok(0) | Err(_) => return,
                                    Ok(n) => got += n,
                                }
                            }
                        }
                        Then::AcceptThenCloseOnRelease => {
                            while let Ok((req, _)) = codec::read_frame(&mut rx).await {
                                if req.op == op::PAGE_REPLY {
                                    seen.lock().unwrap().push(index);
                                    break;
                                }
                            }
                            release.notified().await;
                        }
                    }
                });
            }
        });
        fixture
    }

    fn page_replies(&self) -> Vec<usize> {
        self.page_replies.lock().unwrap().clone()
    }
}

fn start_window(rt: &tokio::runtime::Runtime, fixture: &Fixture, host: &str) -> (UnboundedSender<OutgoingReq>, Events) {
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let (out_tx, out_rx) = outgoing_channel();
    let config = TransportConfig { dial: Dial::Pipe(fixture.path.clone()), token: None };
    spawn(
        rt,
        host.to_string(),
        config,
        evt_tx,
        out_rx,
        NoWindow,
        Arc::new(tokio::sync::Notify::new()),
        sot_topology::link::LinkHandle::default(),
        crate::lease::Leases::new(true, Vec::new()),
    );
    (out_tx, evt_rx)
}

/// Events until `stop` matches one, within 10 s; the matching event is the last returned.
fn until(rx: &Events, stop: impl Fn(&IncomingEvt) -> bool) -> Vec<IncomingEvt> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    loop {
        let (_, evt) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("an event within 10 s");
        let done = stop(&evt);
        seen.push(evt);
        if done {
            return seen;
        }
    }
}

fn told(events: &[IncomingEvt]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            IncomingEvt::PageReplied { handle, outcome, .. } => Some((handle.clone(), outcome.clone())),
            _ => None,
        })
        .collect()
}

/// Waits for the second connection's handshake and then long enough for anything queued to be written on it, and
/// returns every `PageReplied` that came meanwhile as (handle, outcome).
fn replies_after_the_reconnect(fixture: &Fixture, rx: &Events) -> Vec<(String, String)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while fixture.up.load(Ordering::SeqCst) < 2 {
        assert!(Instant::now() < deadline, "the window never reconnected");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(500));
    rx.try_iter()
        .filter_map(|(_, e)| match e {
            IncomingEvt::PageReplied { handle, outcome, .. } => Some((handle, outcome)),
            _ => None,
        })
        .collect()
}

fn page_reply(handle: &str, line: String) -> OutgoingReq {
    OutgoingReq::PageReply { workspace_id: "w".into(), handle: handle.into(), line }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

fn finish(rt: tokio::runtime::Runtime, fixture: &Fixture) {
    rt.shutdown_timeout(Duration::from_secs(2));
    #[cfg(not(windows))]
    let _ = std::fs::remove_dir_all(fixture.path.parent().unwrap());
    #[cfg(windows)]
    let _ = fixture;
}

/// A reply queued while no connection is current (the connection just ended, the next not yet up) is never written on
/// the next one, and its caller is told once.
#[test]
fn a_reply_queued_between_connections_is_not_written_on_the_next() {
    let _env = crate::net::state::test_env::set_test_env();
    let rt = runtime();
    let fixture = Fixture::start(&rt, "scope-between", Then::CloseOnRelease);
    let (out_tx, rx) = start_window(&rt, &fixture, "scope-between");
    until(&rx, |e| matches!(e, IncomingEvt::Connected { .. }));
    fixture.release.notify_one();
    until(&rx, |e| matches!(e, IncomingEvt::Disconnected { .. }));
    out_tx.send(page_reply("repo__box", "hello".into())).unwrap();
    let replied = replies_after_the_reconnect(&fixture, &rx);
    println!("between: daemon received {:?}, caller told {replied:?}", fixture.page_replies());
    assert!(fixture.page_replies().is_empty(), "the daemon received the reply on the next connection");
    assert_eq!(replied, [("repo__box".to_string(), "unknown".to_string())], "one outcome for the reply");
    finish(rt, &fixture);
}

/// A reply cut part way by the connection's end, and one queued behind it, reach the daemon never whole, and each
/// caller is told once.
#[test]
fn a_reply_cut_during_its_write_and_one_behind_it_are_not_written_again() {
    let _env = crate::net::state::test_env::set_test_env();
    let rt = runtime();
    let fixture = Fixture::start(&rt, "scope-cut", Then::CloseMidRequest);
    let (out_tx, rx) = start_window(&rt, &fixture, "scope-cut");
    until(&rx, |e| matches!(e, IncomingEvt::Connected { .. }));
    out_tx.send(page_reply("first__box", "x".repeat(900_000))).unwrap();
    out_tx.send(page_reply("second__box", "short".into())).unwrap();
    let mut replied = told(&until(&rx, |e| matches!(e, IncomingEvt::Disconnected { .. })));
    replied.extend(replies_after_the_reconnect(&fixture, &rx));
    replied.sort();
    println!("cut: daemon received {:?}, callers told {replied:?}", fixture.page_replies());
    assert!(fixture.page_replies().is_empty(), "the daemon received a reply whole");
    assert_eq!(
        replied,
        [("first__box".to_string(), "unknown".to_string()), ("second__box".to_string(), "unknown".to_string())],
        "one outcome for each reply"
    );
    finish(rt, &fixture);
}

/// A reply the daemon accepted and never answered was received once, on the first connection, and its caller is told
/// it is unknown once.
#[test]
fn an_accepted_reply_with_no_answer_is_unknown_once_and_not_written_again() {
    let _env = crate::net::state::test_env::set_test_env();
    let rt = runtime();
    let fixture = Fixture::start(&rt, "scope-accepted", Then::AcceptThenCloseOnRelease);
    let (out_tx, rx) = start_window(&rt, &fixture, "scope-accepted");
    until(&rx, |e| matches!(e, IncomingEvt::Connected { .. }));
    out_tx.send(page_reply("repo__box", "hello".into())).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while fixture.page_replies().is_empty() {
        assert!(Instant::now() < deadline, "the daemon never received the reply");
        std::thread::sleep(Duration::from_millis(10));
    }
    fixture.release.notify_one();
    let mut replied = told(&until(&rx, |e| matches!(e, IncomingEvt::Disconnected { .. })));
    replied.extend(replies_after_the_reconnect(&fixture, &rx));
    println!("accepted: daemon received {:?}, caller told {replied:?}", fixture.page_replies());
    assert_eq!(fixture.page_replies(), [0], "received once, on the first connection");
    assert_eq!(replied, [("repo__box".to_string(), "unknown".to_string())], "one outcome for the reply");
    finish(rt, &fixture);
}

/// A retirement ends the requests that must not be replayed, each with its kind's one connection-ended outcome and
/// nothing written, and keeps the others in the order they were sent.
#[tokio::test]
async fn a_retirement_ends_the_unreplayable_and_keeps_the_rest_in_order() {
    let (out_tx, out_rx) = outgoing_channel();
    let mut outbox = Outbox::new(out_rx);
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let read = |id: &str| OutgoingReq::FileRead { node_id: id.into(), workspace_id: None };
    out_tx.send(read("a")).unwrap();
    out_tx.send(page_reply("repo__box", "hello".into())).unwrap();
    out_tx.send(OutgoingReq::ReplEval { eval_id: 7, code: "1".into(), mode: None, workspace_id: None }).unwrap();
    out_tx.send(read("b")).unwrap();
    outbox.retire(&evt_tx, &"h".to_string()).await;
    let mut ended: Vec<String> = evt_rx.try_iter().map(|(_, e)| format!("{e:?}")).collect();
    ended.sort();
    assert!(
        matches!(ended.as_slice(), [a, b] if a.starts_with("PageReplied { handle: \"repo__box\", outcome: \"unknown\"") && b.starts_with("ReplEvalDone { eval_id: 7")),
        "{ended:?}"
    );
    drop(out_tx);
    let mut kept = Vec::new();
    while let Some(req) = outbox.recv().await {
        match req {
            OutgoingReq::FileRead { node_id, .. } => kept.push(node_id),
            other => panic!("kept {other:?}"),
        }
    }
    assert_eq!(kept, ["a", "b"]);
}
