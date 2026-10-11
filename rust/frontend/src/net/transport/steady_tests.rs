//! Tests of the steady loop over small in-memory streams: both directions progress together.

use super::*;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::mpsc::UnboundedSender;

struct Quiet;
impl Redraw for Quiet {
    fn request_redraw(&self) {}
}

/// The daemon's end of a steady connection, the window's requests going in and its events coming out. The loop runs
/// as production runs it: a `PendingGuard` and a session over the near end of a duplex stream of the given capacity.
struct Rig {
    out_tx: Option<UnboundedSender<OutgoingReq>>,
    events: std::sync::mpsc::Receiver<(HostKey, IncomingEvt)>,
    far_rd: tokio::io::BufReader<ReadHalf<DuplexStream>>,
    far_wr: WriteHalf<DuplexStream>,
    task: tokio::task::JoinHandle<Result<()>>,
    _env: crate::net::state::test_env::EnvGuard,
}

fn rig(capacity: usize, ping_every: Duration, first_id: u64) -> Rig {
    let env = crate::net::state::test_env::set_test_env();
    let (near, far) = tokio::io::duplex(capacity);
    let (events_tx, events) = std::sync::mpsc::channel();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let host: HostKey = "steady-test".to_string();
        let mut pending = PendingGuard { map: HashMap::new(), evt_tx: &events_tx, host: host.clone() };
        let mut session = SessionState {
            host: host.clone(),
            memory: crate::net::state::SessionMemory::fresh(),
            gate: StateSaveGate::new(),
        };
        let (rx, tx) = tokio::io::split(near);
        steady::steady_loop(
            codec::buffered(rx),
            tx,
            first_id,
            &mut pending,
            &mut session,
            host,
            &events_tx,
            &mut out_rx,
            &Quiet,
            ping_every,
        )
        .await
    });
    let (far_rd, far_wr) = tokio::io::split(far);
    Rig { out_tx: Some(out_tx), events, far_rd: codec::buffered(far_rd), far_wr, task, _env: env }
}

const NEVER: Duration = Duration::from_secs(3600);
const WAIT: Duration = Duration::from_secs(10);

fn upload(offset: u64, kib: usize, eof: bool) -> OutgoingReq {
    OutgoingReq::FileUpload {
        dir: "/d".into(),
        name: "f".into(),
        offset,
        total: 1,
        eof,
        bytes: vec![7; kib * 1024],
    }
}

fn event_frame(n: usize) -> Frame {
    Frame::evt("test.event", serde_json::json!({ "n": n, "pad": "x".repeat(300) }))
}

/// Count events of the two kinds the tests wait for, until `want` are in or the wait ends.
async fn collect(rig: &Rig, want_events: usize, want_acks: usize) -> (usize, usize) {
    let (mut events, mut acks) = (0, 0);
    let t0 = std::time::Instant::now();
    while (events < want_events || acks < want_acks) && t0.elapsed() < WAIT {
        while let Ok((_, evt)) = rig.events.try_recv() {
            match evt {
                IncomingEvt::Event { .. } => events += 1,
                IncomingEvt::FileUploadAck { .. } => acks += 1,
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    (events, acks)
}

/// The peer sends a multi-frame download first and does not read the upload until it has finished sending; both
/// transfers and the matching ack still complete on a stream far smaller than either.
#[tokio::test]
async fn upload_and_download_progress_on_a_small_duplex() {
    let mut rig = rig(128, NEVER, 1);
    rig.out_tx.as_ref().unwrap().send(upload(0, 4, true)).unwrap();
    for n in 0..8 {
        let bytes = frame_bytes(&event_frame(n));
        let write = rig.far_wr.write_all(&bytes);
        tokio::time::timeout(WAIT, write).await.expect("the daemon's download must drain into the reader").unwrap();
    }
    let (req, _) = tokio::time::timeout(WAIT, codec::read_frame(&mut rig.far_rd))
        .await
        .expect("the upload must reach the daemon")
        .unwrap();
    assert_eq!(req.op, op::FILE_UPLOAD);
    let ack = serde_json::json!({ "offset": 0, "done": true, "final_name": "f" });
    put(&mut rig.far_wr, &&Frame::res(req.id, op::FILE_UPLOAD, ack)).await;
    assert_eq!(collect(&rig, 8, 1).await, (8, 1), "every download frame and the upload's ack");
}

/// Write one frame to the loop, failing the test when the loop does not take it within the wait.
async fn put(wr: &mut WriteHalf<DuplexStream>, frame: &Frame) {
    let bytes = frame_bytes(frame);
    tokio::time::timeout(WAIT, wr.write_all(&bytes)).await.expect("the loop must keep reading").unwrap();
}

fn frame_bytes(frame: &Frame) -> Vec<u8> {
    let mut out = serde_json::to_vec(frame).unwrap();
    out.push(b'\n');
    out
}

/// A ping that cannot be written yet holds nothing else up: the daemon's events arrive while it is not reading, and
/// the ping then arrives whole.
#[tokio::test]
async fn ping_write_does_not_block_reads() {
    let mut rig = rig(16, Duration::from_millis(20), 1);
    // Let the first ping come due; with the daemon not reading, it cannot be written whole.
    tokio::time::sleep(Duration::from_millis(100)).await;
    for n in 0..3 {
        put(&mut rig.far_wr, &&event_frame(n)).await;
    }
    assert_eq!(collect(&rig, 3, 0).await.0, 3, "events must arrive while the ping write is blocked");
    let (ping, _) = tokio::time::timeout(WAIT, codec::read_frame(&mut rig.far_rd)).await.unwrap().unwrap();
    assert_eq!(ping.op, op::PING);
    // The ping never overtook bytes: a request queued behind it arrives whole and in order.
    rig.out_tx.as_ref().unwrap().send(OutgoingReq::FePresence).unwrap();
    let mut ops = Vec::new();
    while ops.last().map(String::as_str) != Some(op::FE_PRESENCE) {
        let (f, _) = tokio::time::timeout(WAIT, codec::read_frame(&mut rig.far_rd)).await.unwrap().unwrap();
        ops.push(f.op);
    }
    assert!(ops.iter().all(|o| o == op::PING || o == op::FE_PRESENCE), "{ops:?}");
}

/// Sustained reads and writes on a stream of seven bytes: a continuously ready reader does not starve the writer,
/// every frame arrives whole, and requests keep their order and ids.
#[tokio::test]
async fn sustained_traffic_in_both_directions_keeps_frames_whole_and_ordered() {
    let mut rig = rig(7, NEVER, 10);
    for _ in 0..20 {
        rig.out_tx.as_ref().unwrap().send(OutgoingReq::FePresence).unwrap();
    }
    let feed = async {
        for n in 0..40 {
            put(&mut rig.far_wr, &&event_frame(n)).await;
        }
    };
    let read = async {
        let mut ids = Vec::new();
        for _ in 0..20 {
            let (f, _) = tokio::time::timeout(WAIT, codec::read_frame(&mut rig.far_rd)).await.expect("a request").unwrap();
            assert_eq!(f.op, op::FE_PRESENCE);
            ids.push(f.id);
        }
        ids
    };
    let ((), ids) = tokio::join!(feed, read);
    assert_eq!(ids, (10..30).collect::<Vec<_>>());
    assert_eq!(collect(&rig, 40, 0).await.0, 40);
}

/// A reply that arrives while its request is still being written finds its pending entry: the entry is registered
/// before the first network byte.
#[tokio::test]
async fn a_response_before_the_request_is_fully_written_is_still_correlated() {
    let mut rig = rig(32, NEVER, 7);
    rig.out_tx.as_ref().unwrap().send(upload(0, 2, true)).unwrap();
    let ack = serde_json::json!({ "offset": 0, "done": true, "final_name": "f" });
    put(&mut rig.far_wr, &&Frame::res(7, op::FILE_UPLOAD, ack)).await;
    assert_eq!(collect(&rig, 0, 1).await.1, 1, "the ack of a request the daemon has not finished reading");
}

/// A request whose encoding fails after it recorded a FigureGet still gets its one failure, and the loop ends with
/// the encoding error.
#[tokio::test]
async fn an_encoding_failure_still_reports_a_recorded_figure_once() {
    let rig = rig(1024, NEVER, 1);
    let huge = "x".repeat(sot_protocol::codec::MAX_ENVELOPE_BYTES + 10);
    rig.out_tx
        .as_ref()
        .unwrap()
        .send(OutgoingReq::FigureGet { url: "fig://one".into(), node_id: huge, workspace_id: None })
        .unwrap();
    let result = tokio::time::timeout(WAIT, rig.task).await.expect("the loop must end").unwrap();
    assert!(result.is_err());
    let failed = std::iter::from_fn(|| rig.events.try_recv().ok())
        .filter(|(_, e)| matches!(e, IncomingEvt::FigureGetFailed { url } if url == "fig://one"))
        .count();
    assert_eq!(failed, 1);
}

/// A disconnect during a blocked write ends the loop, and a FigureGet in flight is reported once.
#[tokio::test]
async fn a_disconnect_during_a_write_ends_the_loop_and_flushes_the_figure() {
    let rig = rig(16, NEVER, 1);
    let tx = rig.out_tx.as_ref().unwrap();
    tx.send(OutgoingReq::FigureGet { url: "fig://two".into(), node_id: "n".into(), workspace_id: None }).unwrap();
    tx.send(upload(0, 2, true)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    drop((rig.far_rd, rig.far_wr));
    let result = tokio::time::timeout(WAIT, rig.task).await.expect("the loop must end").unwrap();
    assert!(result.is_err());
    let failed = std::iter::from_fn(|| rig.events.try_recv().ok())
        .filter(|(_, e)| matches!(e, IncomingEvt::FigureGetFailed { url } if url == "fig://two"))
        .count();
    assert_eq!(failed, 1);
}

/// Closing the outgoing side mid-write finishes the frame being written and keeps reading until the daemon closes.
#[tokio::test]
async fn closing_the_outgoing_side_finishes_the_write_and_drains_reads() {
    let mut rig = rig(32, NEVER, 1);
    rig.out_tx.as_ref().unwrap().send(upload(0, 2, true)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    rig.out_tx = None;
    put(&mut rig.far_wr, &&event_frame(1)).await;
    let (req, _) = tokio::time::timeout(WAIT, codec::read_frame(&mut rig.far_rd)).await.expect("the whole frame").unwrap();
    assert_eq!(req.op, op::FILE_UPLOAD);
    put(&mut rig.far_wr, &&event_frame(2)).await;
    assert_eq!(collect(&rig, 2, 0).await.0, 2, "reads continue after the outgoing side closed");
    drop((rig.far_rd, rig.far_wr));
    let result = tokio::time::timeout(WAIT, rig.task).await.expect("the drain ends at disconnect").unwrap();
    assert!(result.is_err());
}

/// Cancelling the connection task in the middle of a write drops the pending guard once.
#[tokio::test]
async fn cancelling_during_a_write_flushes_the_figure_once() {
    let rig = rig(16, NEVER, 1);
    let tx = rig.out_tx.as_ref().unwrap();
    tx.send(OutgoingReq::FigureGet { url: "fig://three".into(), node_id: "n".into(), workspace_id: None }).unwrap();
    tx.send(upload(0, 2, true)).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    rig.task.abort();
    let _ = rig.task.await;
    let failed = std::iter::from_fn(|| rig.events.try_recv().ok())
        .filter(|(_, e)| matches!(e, IncomingEvt::FigureGetFailed { url } if url == "fig://three"))
        .count();
    assert_eq!(failed, 1);
}
