//! The link's guarantees, end to end in one process: a window's `Link` and the router's `serve` over an in-memory
//! stream, the router's connections made to in-memory daemons. A raw peer that writes the link's frames by hand
//! stands in for a far end that breaks the rules.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sot_platform::transport::TransportError;
use sot_protocol::ops::link::{LinkCloseReason, CHUNK, MAX_CHANNELS, WINDOW};
use sot_protocol::{codec, op, Frame, Kind};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

use super::io::{Connector, Duplex};
use super::router::Served;
use super::*;

/// The blocking face, in a module of its own: with `Client` in scope its `read` and `write_all` would shadow the async
/// ones on a `Channel`.
mod blocking {
    use sot_platform::transport::{Client, TransportError};

    pub(super) fn write(channel: &super::Channel, bytes: &[u8]) -> Result<(), TransportError> {
        channel.write_all(bytes)
    }

    pub(super) fn read(channel: &super::Channel, buf: &mut [u8]) -> Result<usize, TransportError> {
        channel.read(buf)
    }
}

const BOUND: Duration = Duration::from_secs(10);

/// What a fake daemon does with one connection the router made.
#[derive(Clone, Copy)]
enum Daemon {
    /// Echoes every byte back.
    Echo,
    /// Writes without end and reads nothing.
    Flood,
}

/// A connector whose `n`th connection (from 0, channel 0's included when `connect` makes it) behaves as `plan[n]`, or
/// echoes past the plan; `made` counts the connections.
fn connector(plan: Vec<Daemon>, made: Arc<AtomicUsize>) -> Connector {
    Arc::new(move || {
        let n = made.fetch_add(1, Ordering::SeqCst);
        let behavior = plan.get(n).copied().unwrap_or(Daemon::Echo);
        Box::pin(async move {
            let (near, far) = tokio::io::duplex(1 << 16);
            tokio::spawn(fake_daemon(far, behavior));
            Ok(Box::new(near) as Box<dyn Duplex>)
        })
    })
}

async fn fake_daemon(conn: DuplexStream, behavior: Daemon) {
    let (mut rx, mut tx) = tokio::io::split(conn);
    match behavior {
        Daemon::Echo => {
            let mut buf = vec![0u8; 8192];
            loop {
                match rx.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if tx.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
        Daemon::Flood => {
            let chunk = vec![7u8; 8192];
            while tx.write_all(&chunk).await.is_ok() {}
        }
    }
}

/// A window's link and the router serving it, over one in-memory stream. Channel 0 goes to an echoing daemon of its
/// own; `connect` makes every other channel's connection.
struct Pair {
    link: Link,
    handle: LinkHandle,
    router: tokio::task::JoinHandle<Served>,
}

async fn pair(connect: Connector, silence: Duration) -> Pair {
    let (near, far) = tokio::io::duplex(1 << 20);
    let router = tokio::spawn(async move {
        let (rx, tx) = tokio::io::split(far);
        let mut rx = tokio::io::BufReader::new(rx);
        let first = codec::read_envelope(&mut rx).await.map_err(|e| e.to_string())?;
        let (control, daemon) = tokio::io::duplex(1 << 16);
        tokio::spawn(fake_daemon(daemon, Daemon::Echo));
        router::serve_within(first, rx, tx, Box::new(control), connect, silence).await
    });
    let (rx, tx) = tokio::io::split(near);
    let link = Link::over_within(rx, tx, silence).await.expect("the link starts");
    let handle = LinkHandle::default();
    handle.set_up(&link);
    Pair { link, handle, router }
}

/// Writes `bytes` on `channel` and reads the same count back, within `BOUND`.
async fn echo(channel: &mut Channel, bytes: &[u8]) -> Vec<u8> {
    channel.write_all(bytes).await.expect("write on the channel");
    channel.flush().await.expect("flush the channel");
    let mut back = vec![0u8; bytes.len()];
    tokio::time::timeout(BOUND, channel.read_exact(&mut back)).await.expect("the echo within the bound").expect("read the echo");
    back
}

async fn until<F: Fn() -> bool>(what: &str, done: F) {
    let deadline = tokio::time::Instant::now() + BOUND;
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "{what}: not within {BOUND:?}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Topology 5: control, three rows and two pages are six channels of one link, each reaching a connection of its own
/// on the far side, each one's bytes only on its own.
#[tokio::test]
async fn one_link_carries_control_rows_and_pages_as_channels() {
    let made = Arc::new(AtomicUsize::new(0));
    let mut pair = pair(connector(Vec::new(), Arc::clone(&made)), SILENCE_BOUND).await;
    let mut control = pair.link.take_control().expect("channel 0");
    assert_eq!(control.id(), 0);
    let mut opened = Vec::new();
    for _ in 0..5 {
        opened.push(pair.handle.open().await.expect("a channel opens"));
    }
    assert_eq!(opened.iter().map(Channel::id).collect::<Vec<_>>(), [1, 2, 3, 4, 5], "numbers count up from 1");
    assert_eq!(echo(&mut control, b"control hello").await, b"control hello");
    for (n, channel) in opened.iter_mut().enumerate() {
        let said = format!("channel {n} says its own words");
        assert_eq!(echo(channel, said.as_bytes()).await, said.as_bytes());
    }
    assert_eq!(made.load(Ordering::SeqCst), 5, "one far connection per opened channel");
    // A bulk transfer bigger than a window crosses whole, its credit returned as it is read: written and read at once,
    // since the echo stops when nobody reads its answer.
    let bulk: Vec<u8> = (0..(3 * WINDOW as usize + 17)).map(|i| (i % 251) as u8).collect();
    let (mut from, mut to) = opened.remove(0).into_split();
    let sent = bulk.clone();
    let writer = tokio::spawn(async move {
        to.write_all(&sent).await.expect("write the bulk");
        to.flush().await.expect("flush the bulk");
        to
    });
    let mut back = vec![0u8; bulk.len()];
    tokio::time::timeout(BOUND, from.read_exact(&mut back)).await.expect("the bulk within the bound").expect("read the bulk");
    assert!(back == bulk, "a transfer past one window comes back whole");
    drop(writer.await.unwrap());
    drop(opened);
    drop(control);
    drop(pair.link);
    let served = tokio::time::timeout(BOUND, pair.router).await.expect("the router ends with the link").expect("the router task");
    assert!(served.is_ok(), "a closed window is a clean end: {served:?}");
}

/// Topology 6: an open on a down link fails at once and sends nothing: a default handle, a handle the session put
/// down, and a handle whose link has ended.
#[tokio::test]
async fn an_open_on_a_down_link_fails_at_once_and_sends_nothing() {
    let started = std::time::Instant::now();
    assert_eq!(LinkHandle::default().open().await.err(), Some(OpenError::LinkDown));
    assert_eq!(LinkHandle::default().open_blocking(BOUND).err(), Some(OpenError::LinkDown));
    let made = Arc::new(AtomicUsize::new(0));
    let pair = pair(connector(Vec::new(), Arc::clone(&made)), SILENCE_BOUND).await;
    let kept = pair.handle.clone();
    pair.handle.set_down();
    assert!(!kept.is_up(), "a clone sees the session's down");
    assert_eq!(kept.open().await.err(), Some(OpenError::LinkDown));
    pair.handle.set_up(&pair.link);
    assert!(kept.is_up());
    drop(pair.link);
    assert!(!kept.is_up(), "an ended link is down whatever the handle last heard");
    assert_eq!(kept.open().await.err(), Some(OpenError::LinkDown));
    assert_eq!(kept.open_blocking(BOUND).err(), Some(OpenError::LinkDown));
    assert!(started.elapsed() < Duration::from_secs(2), "every down open failed at once: {:?}", started.elapsed());
    assert_eq!(made.load(Ordering::SeqCst), 0, "no open reached the router");
}

/// Topology 6: the link's end fails every channel: a reader gets the bytes already queued and then the end, a writer
/// the end, on the async face and on the blocking one.
#[tokio::test]
async fn a_link_end_fails_every_channel_with_link_down() {
    let made = Arc::new(AtomicUsize::new(0));
    let pair = pair(connector(vec![Daemon::Echo, Daemon::Echo], Arc::clone(&made)), SILENCE_BOUND).await;
    let mut queued = pair.handle.open().await.expect("open");
    let blocking = pair.handle.open().await.expect("open");
    // Bytes that come back while nobody reads: they are queued in the window's table when the link ends.
    queued.write_all(b"queued before the end").await.unwrap();
    queued.flush().await.unwrap();
    blocking::write(&blocking, b"blocking queued").unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    pair.router.abort();
    let link = &pair.link;
    until("the link's end", || link.ended().is_some()).await;
    let mut got = vec![0u8; b"queued before the end".len()];
    queued.read_exact(&mut got).await.expect("the queued bytes first");
    assert_eq!(got, b"queued before the end");
    let after = queued.read(&mut [0u8; 8]).await.expect_err("then the end");
    assert_eq!(after.kind(), std::io::ErrorKind::NotConnected, "{after}");
    let wrote = queued.write_all(b"x").await.expect_err("a write after the end");
    assert_eq!(wrote.kind(), std::io::ErrorKind::NotConnected, "{wrote}");
    let blocking_read = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 64];
        let n = blocking::read(&blocking, &mut buf).expect("the queued bytes first");
        let first = buf[..n].to_vec();
        let end = blocking::read(&blocking, &mut buf);
        let write = blocking::write(&blocking, b"x");
        (first, end, write)
    })
    .await
    .unwrap();
    assert_eq!(blocking_read.0, b"blocking queued");
    assert!(matches!(blocking_read.1, Err(TransportError::LinkDown)), "{:?}", blocking_read.1);
    assert!(matches!(blocking_read.2, Err(TransportError::LinkDown)), "{:?}", blocking_read.2);
    assert!(!pair.handle.is_up());
}

/// Topology 6: channel 0's end ends the link on both ends. The router's daemon closing channel 0 ends the window's
/// link and fails its other channels; the window closing channel 0 ends the router, cleanly.
#[tokio::test]
async fn the_end_of_channel_zero_ends_the_link() {
    // The daemon side closes channel 0: the connector's first connection is channel 1, and channel 0 is the pair's
    // own echo daemon, so this pair's control is closed by dropping the router's daemon end through a raw router.
    let (near, far) = tokio::io::duplex(1 << 20);
    let (control, daemon) = tokio::io::duplex(1 << 16);
    let made = Arc::new(AtomicUsize::new(0));
    let connect = connector(Vec::new(), Arc::clone(&made));
    let router = tokio::spawn(async move {
        let (rx, tx) = tokio::io::split(far);
        let mut rx = tokio::io::BufReader::new(rx);
        let first = codec::read_envelope(&mut rx).await.map_err(|e| e.to_string())?;
        router::serve_within(first, rx, tx, Box::new(control), connect, SILENCE_BOUND).await
    });
    let (rx, tx) = tokio::io::split(near);
    let link = Link::over_within(rx, tx, SILENCE_BOUND).await.expect("the link starts");
    let handle = LinkHandle::default();
    handle.set_up(&link);
    let mut row = handle.open().await.expect("open");
    assert_eq!(echo(&mut row, b"before").await, b"before");
    drop(daemon);
    until("the window's link ends with channel 0", || link.ended().is_some()).await;
    let failed = row.read(&mut [0u8; 8]).await.expect_err("the row's channel ends with the link");
    assert_eq!(failed.kind(), std::io::ErrorKind::NotConnected, "{failed}");
    let served = tokio::time::timeout(BOUND, router).await.expect("the router ends").unwrap();
    assert!(served.is_ok(), "channel 0's end is a clean end: {served:?}");

    // The window closes channel 0: the router ends at once, though the window's link value still stands.
    let made = Arc::new(AtomicUsize::new(0));
    let mut pair = pair(connector(Vec::new(), made), SILENCE_BOUND).await;
    let row = pair.handle.open().await.expect("open");
    drop(pair.link.take_control());
    let served = tokio::time::timeout(BOUND, pair.router).await.expect("the router ends when channel 0 closes").unwrap();
    assert!(served.is_ok(), "{served:?}");
    drop(row);
}

/// Topology 13: a channel whose consumer never reads holds its window of credit and nothing else: another channel's
/// one-byte round trip completes within 100 ms while the far daemon floods the stalled one.
#[tokio::test]
async fn a_stalled_channel_never_delays_another_channel() {
    let made = Arc::new(AtomicUsize::new(0));
    let mut pair = pair(connector(vec![Daemon::Flood, Daemon::Echo], Arc::clone(&made)), SILENCE_BOUND).await;
    let _stalled = pair.handle.open().await.expect("open the flooded channel");
    let mut live = pair.handle.open().await.expect("open the live channel");
    // Long enough for the flood to use the stalled channel's whole credit.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut control = pair.link.take_control().unwrap();
    let mut worst = Duration::ZERO;
    for round in 0..20u8 {
        let started = tokio::time::Instant::now();
        assert_eq!(echo(&mut live, &[round]).await, [round]);
        assert_eq!(echo(&mut control, &[round]).await, [round]);
        worst = worst.max(started.elapsed());
    }
    println!("stalled channel beside: worst round trip {worst:?}");
    assert!(worst < Duration::from_millis(100), "a stalled channel delayed another by {worst:?}");
    assert!(pair.link.ended().is_none(), "a stalled consumer never ends the link");
}

/// A far end that writes the link's frames by hand: it answers the window's hello, then does what the test says.
struct RawPeer {
    rx: tokio::io::BufReader<tokio::io::ReadHalf<DuplexStream>>,
    tx: tokio::io::WriteHalf<DuplexStream>,
}

impl RawPeer {
    async fn send(&mut self, frame: Frame, blob: Option<&[u8]>) {
        // The envelope line itself, so a frame that breaks the rules can be written.
        let mut line = serde_json::to_vec(&frame).unwrap();
        line.push(b'\n');
        if let Some(blob) = blob {
            line.extend_from_slice(blob);
        }
        let _ = self.tx.write_all(&line).await;
        let _ = self.tx.flush().await;
    }

    async fn data(&mut self, ch: u32, bytes: &[u8]) {
        let payload = serde_json::json!({ "ch": ch, "blob": { "len": bytes.len(), "mime": "application/octet-stream" } });
        self.send(Frame::evt(op::LINK_DATA, payload), Some(bytes)).await;
    }

    async fn next(&mut self) -> (Frame, Option<Vec<u8>>) {
        tokio::time::timeout(BOUND, codec::read_frame(&mut self.rx)).await.expect("a frame from the window").expect("read a frame")
    }
}

/// A window's link whose far end is a raw peer that has answered its hello.
async fn raw_pair(silence: Duration) -> (Link, LinkHandle, RawPeer) {
    let (near, far) = tokio::io::duplex(1 << 22);
    let (rx, tx) = tokio::io::split(far);
    let mut peer = RawPeer { rx: tokio::io::BufReader::new(rx), tx };
    let (wrx, wtx) = tokio::io::split(near);
    let start = tokio::spawn(Link::over_within(wrx, wtx, silence));
    let (hello, _) = peer.next().await;
    assert_eq!((hello.op.as_str(), hello.kind), (op::LINK_HELLO, Kind::Req));
    peer.send(Frame::res(hello.id, op::LINK_HELLO, serde_json::json!({ "link": 1, "build": "raw" })), None).await;
    let link = start.await.unwrap().expect("the link starts");
    let handle = LinkHandle::default();
    handle.set_up(&link);
    (link, handle, peer)
}

async fn ends_with(link: &Link, words: &str) {
    until("the link's end", || link.ended().is_some()).await;
    let why = link.ended().unwrap();
    assert!(why.contains(words), "the link ended for {why:?}, not {words:?}");
}

/// Topology 13: a peer that sends past a channel's credit ends the link.
#[tokio::test]
async fn data_beyond_its_credit_ends_the_link() {
    let (link, _handle, mut peer) = raw_pair(SILENCE_BOUND).await;
    // Channel 0's window, unread by anyone, and one byte more.
    let chunk = vec![1u8; CHUNK];
    for _ in 0..(WINDOW as usize / CHUNK) {
        peer.data(0, &chunk).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(link.ended().is_none(), "a full window is within the rules");
    peer.data(0, b"!").await;
    ends_with(&link, "beyond its credit").await;
}

/// Topology 13: a blob over the chunk ends the link before a byte of it is read.
#[tokio::test]
async fn a_blob_over_the_chunk_ends_the_link() {
    let (link, _handle, mut peer) = raw_pair(SILENCE_BOUND).await;
    let payload = serde_json::json!({ "ch": 0, "blob": { "len": CHUNK + 1, "mime": "application/octet-stream" } });
    // Only the envelope: the window must refuse the frame on its declared length.
    peer.send(Frame::evt(op::LINK_DATA, payload), None).await;
    ends_with(&link, "over the").await;
}

/// Topology 13: a frame for a channel already closed is dropped and the link stays up; a frame for a channel never
/// opened ends it.
#[tokio::test]
async fn a_frame_for_a_closed_channel_is_dropped_and_one_never_opened_ends_the_link() {
    let (link, handle, mut peer) = raw_pair(SILENCE_BOUND).await;
    let opening = tokio::spawn(async move { handle.open().await });
    let (open, _) = peer.next().await;
    assert_eq!((open.op.as_str(), open.kind, open.payload["ch"].as_u64()), (op::LINK_OPEN, Kind::Req, Some(1)));
    peer.send(Frame::res(open.id, op::LINK_OPEN, serde_json::json!({ "ch": 1, "ok": true })), None).await;
    let channel = opening.await.unwrap().expect("channel 1 opens");
    drop(channel);
    let (close, _) = peer.next().await;
    assert_eq!((close.op.as_str(), close.payload["ch"].as_u64(), close.payload["reason"].as_str()), (op::LINK_CLOSE, Some(1), Some("eof")));
    peer.data(1, b"late bytes for a closed channel").await;
    peer.send(Frame::evt(op::LINK_CREDIT, serde_json::json!({ "ch": 1, "bytes": 10 })), None).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(link.ended().is_none(), "a frame for a closed channel is dropped: {:?}", link.ended());
    peer.data(5, b"a channel nobody opened").await;
    ends_with(&link, "never opened").await;
}

/// Topology 13: the open past `MAX_CHANNELS` (channel 0 counted) is refused `too_many_channels`, and the link stays up.
#[tokio::test]
async fn the_open_past_the_cap_is_refused() {
    let made = Arc::new(AtomicUsize::new(0));
    let pair = pair(connector(Vec::new(), Arc::clone(&made)), SILENCE_BOUND).await;
    let mut held = Vec::new();
    for _ in 1..MAX_CHANNELS {
        held.push(pair.handle.open().await.expect("an open under the cap"));
    }
    match pair.handle.open().await {
        Err(OpenError::Refused { code, .. }) => assert_eq!(code, "too_many_channels"),
        other => panic!("the open past the cap: {other:?}"),
    }
    assert_eq!(made.load(Ordering::SeqCst), MAX_CHANNELS - 1, "the refused open made no connection");
    assert!(pair.link.ended().is_none());
    // A channel's close frees its place.
    drop(held.pop());
    let deadline = tokio::time::Instant::now() + BOUND;
    loop {
        match pair.handle.open().await {
            Ok(channel) => {
                held.push(channel);
                break;
            }
            Err(OpenError::Refused { .. }) if tokio::time::Instant::now() < deadline => tokio::time::sleep(Duration::from_millis(10)).await,
            other => panic!("the open after a close: {other:?}"),
        }
    }
}

/// Topology 14: a link that brings no byte for its silence bound ends, at the window and at the router.
#[tokio::test]
async fn a_silent_link_ends_at_its_bound() {
    let silence = Duration::from_millis(400);
    let started = tokio::time::Instant::now();
    let (link, _handle, _peer) = raw_pair(silence).await;
    ends_with(&link, "nothing from the far end").await;
    let after = started.elapsed();
    assert!(after >= silence && after < silence + Duration::from_secs(2), "the window's link ended after {after:?}");

    // The router, against a window that says hello and then nothing.
    let (near, far) = tokio::io::duplex(1 << 16);
    let (control, daemon) = tokio::io::duplex(1 << 16);
    tokio::spawn(fake_daemon(daemon, Daemon::Echo));
    let made = Arc::new(AtomicUsize::new(0));
    let first = Frame::req(1, op::LINK_HELLO, serde_json::json!({ "link": 1 }));
    let (rx, tx) = tokio::io::split(far);
    let started = tokio::time::Instant::now();
    let served = tokio::time::timeout(BOUND, router::serve_within(first, rx, tx, Box::new(control), connector(Vec::new(), made), silence))
        .await
        .expect("the router ends at its bound");
    let after = started.elapsed();
    drop(near);
    let why = served.expect_err("silence is a failure");
    assert!(why.contains("nothing from the far end"), "{why}");
    assert!(after >= silence && after < silence + Duration::from_secs(2), "the router ended after {after:?}");
}

/// Wire 9, the old reader: a far bridge from before the link hands `link.hello` to its daemon, which refuses a first
/// frame that is not a hello; the window reads that as a backend to update, and a router of another link number as
/// the numbers that differ.
#[tokio::test]
async fn an_old_bridge_and_another_link_number_are_refusals_a_person_reads() {
    for (answer, code, words) in [
        (serde_json::json!({ "error": "send a hello first", "code": "unauthenticated" }), "predates_link", "predates the link"),
        (serde_json::json!({ "error": "no", "code": "link_mismatch", "link": 2 }), "link_mismatch", "speaks link 2"),
        (serde_json::json!({ "link": 2, "build": "later" }), "link_mismatch", "speaks link 2"),
    ] {
        let (near, far) = tokio::io::duplex(1 << 16);
        let (rx, tx) = tokio::io::split(far);
        let mut peer = RawPeer { rx: tokio::io::BufReader::new(rx), tx };
        let (wrx, wtx) = tokio::io::split(near);
        let start = tokio::spawn(Link::over_within(wrx, wtx, SILENCE_BOUND));
        let (hello, _) = peer.next().await;
        peer.send(Frame::res(hello.id, op::LINK_HELLO, answer.clone()), None).await;
        match start.await.unwrap() {
            Err(LinkStartError::Refused { code: got, message }) => {
                assert_eq!(got, code, "{answer}");
                assert!(message.contains(words), "{message}");
            }
            other => panic!("{answer}: {:?}", other.map(|_| ())),
        }
    }
}

/// An opener that gives up before its answer leaves nothing open: the answer, when it comes, closes the channel.
#[tokio::test]
async fn an_abandoned_open_closes_the_channel_its_answer_opens() {
    let (link, handle, mut peer) = raw_pair(SILENCE_BOUND).await;
    let gave_up = tokio::task::spawn_blocking(move || handle.open_blocking(Duration::from_millis(100)).map(|_| ()));
    let (open, _) = peer.next().await;
    assert_eq!(gave_up.await.unwrap(), Err(OpenError::TimedOut));
    peer.send(Frame::res(open.id, op::LINK_OPEN, serde_json::json!({ "ch": 1, "ok": true })), None).await;
    let (close, _) = peer.next().await;
    assert_eq!((close.op.as_str(), close.payload["ch"].as_u64()), (op::LINK_CLOSE, Some(1)));
    assert_eq!(close.payload["reason"], serde_json::json!(LinkCloseReason::Reset));
    assert!(link.ended().is_none());
}

/// The window's one login, ended before its answer: the start fails with the error's own words and ssh's last stderr
/// line after them.
#[cfg(unix)]
#[tokio::test]
async fn a_login_that_ends_before_its_answer_names_its_stderr_line() {
    let script = "echo 'Permission denied (publickey).' >&2; exit 255";
    let recipe = crate::ssh_bridge::SshRecipe::new("hub", None).unwrap().with_test_command("sh", ["-c", script]);
    match Link::start(&LinkRoute::Ssh(recipe)).await {
        Err(LinkStartError::Failed(why)) => assert!(why.starts_with("link.hello") && why.ends_with("(ssh: Permission denied (publickey).)"), "{why}"),
        other => panic!("a login that ended: {:?}", other.map(|_| ())),
    }
}

/// The login's stderr is drained to its last non-empty line, read past a byte that is not UTF-8 (a far host's banner).
#[tokio::test]
async fn the_stderr_drain_keeps_the_last_non_empty_line() {
    async fn drained(input: &'static [u8]) -> Option<String> {
        let cell = super::window::drain_stderr(input);
        tokio::time::timeout(BOUND, async {
            while Arc::strong_count(&cell) != 1 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the drain task ends at end of input");
        let line = cell.lock().unwrap().clone();
        line
    }
    assert_eq!(drained(b"first\n\n  \nsecond\n \t \n").await.as_deref(), Some("second"));
    assert_eq!(drained(b"\n  \n").await, None);
    assert_eq!(drained(b"Banner \xe9t\xe9\nPermission denied (publickey).\n").await.as_deref(), Some("Permission denied (publickey)."));
}
