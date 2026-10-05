//! Tests: The stall harness, held inputs, take-queue drops, the status-probe keystroke and the held-handshake gate.

use crate::identity::challenge::{ChallengeOutcome, PeerAuthOutcome};
use crate::lane::client::{Client, Endpoint};
use crate::attach_client::rules::{OutstandingSlot, QuitDispatcher, ReconnectState, TakeTransaction};
use crate::lane::wire::{self, AttachClient, AttachServer, DecodedFrame, SupervisorPhase, SupervisorReply};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use super::support_tests::*;

/// Shared state behind [`StallClient`]: what was written, whether a
/// read is parked, and whether `cancel` released it.
#[derive(Default)]
struct StallState {
    writes: Mutex<Vec<Vec<u8>>>,
    read_entered: AtomicBool,
    cancelled: Mutex<bool>,
    released: Condvar,
}

/// A peer that never answers: `write_all` records and succeeds, `read`
/// parks until `cancel` -- a stalled supervisor behind a live socket.
struct StallClient(Arc<StallState>);
impl Client for StallClient {
    fn write_all(&self, bytes: &[u8]) -> Result<(), crate::lane::transport::TransportError> {
        self.0.writes.lock().unwrap().push(bytes.to_vec());
        Ok(())
    }
    fn read(&self, _buf: &mut [u8]) -> Result<usize, crate::lane::transport::TransportError> {
        self.0.read_entered.store(true, Ordering::SeqCst);
        let mut cancelled = self.0.cancelled.lock().unwrap();
        while !*cancelled {
            cancelled = self.0.released.wait(cancelled).unwrap();
        }
        Err(crate::lane::transport::TransportError::Cancelled)
    }
    fn cancel(&self) {
        *self.0.cancelled.lock().unwrap() = true;
        self.0.released.notify_all();
    }
}

/// Dials nothing: the re-dial after the stalled probe's deadline fails.
struct StallEndpoint;
impl Endpoint for StallEndpoint {
    type Client = StallClient;
    type Process = TestProcess;

    fn connect_voyage_unchallenged(&self, _lane: &str, _voyage_id: &str) -> Result<Self::Client, crate::lane::transport::TransportError> {
        unreachable!("the steady state never dials the voyage lane")
    }
    fn connect_supervisor_unchallenged(&self, _lane: &str) -> Result<Self::Client, crate::lane::transport::TransportError> {
        Err(crate::lane::transport::TransportError::Cancelled)
    }
    fn challenge(&self, _conn: &Self::Client, _exchange: &mut dyn crate::identity::exchange::IdentityExchange, _deadline: Instant) -> ChallengeOutcome<Self::Process> {
        unreachable!("the re-dial fails before any challenge")
    }
    fn authenticate_server(&self, _conn: &Self::Client) -> PeerAuthOutcome {
        unreachable!("the steady state never authenticates")
    }
}

/// F6: a keystroke goes out while a `Status` probe is outstanding on a
/// supervisor that does not answer. The inline probe made it wait the
/// probe's whole `STATUS_BUDGET` (5 s); the bound here is ten times
/// below that and five ticks of slack above an idle worker.
/// Runs `run_steady_state` on the stall endpoint at `attached_gen`,
/// with the liveness probe due at once.
fn spawn_stall_steady(
    rx: Receiver<WorkerMsg>,
    attach_conn: Arc<StallClient>,
    sup_conn: StallClient,
    attached_gen: u64,
    discarded: Arc<AtomicUsize>,
) -> thread::JoinHandle<SteadyOutcome> {
    thread::spawn(move || {
        let mut take = TakeTransaction::new();
        let mut take_intent = TakeIntent::Ordinary;
        let mut outstanding = OutstandingSlot::new();
        let mut quit = QuitDispatcher::new();
        let mut reconnect = ReconnectState::new();
        let (mut cols, mut rows, mut take_epoch) = (80u16, 24u16, 0u64);
        let mut last_poll = Instant::now().checked_sub(LIVENESS_POLL_INTERVAL).unwrap_or_else(Instant::now);
        run_steady_state::<StallEndpoint>(
            &StallEndpoint,
            &rx,
            &|_e| {},
            "h",
            &attach_conn,
            sup_conn,
            FrameReader::new(),
            &mut take,
            &mut take_intent,
            &mut outstanding,
            &mut quit,
            &mut reconnect,
            &mut cols,
            &mut rows,
            &mut take_epoch,
            "controller-1",
            "voyage-1",
            &mut last_poll,
            &Arc::new(AtomicU64::new(0)),
            &Arc::new(Mutex::new(None)),
            &Arc::new(AtomicU64::new(0)),
            attached_gen,
            &discarded,
        )
    })
}

fn input_msg(bytes: &[u8], gen: u64) -> WorkerMsg {
    let reservation = IngressReservation { ingress_bytes: Arc::new(AtomicUsize::new(bytes.len())), n: bytes.len() };
    WorkerMsg::Input(bytes.to_vec(), reservation, gen)
}

/// An input stamped before this attach is discarded and counted,
/// never sent; the first current input is sent and clears the count.
#[test]
fn an_input_stamped_before_the_attach_is_counted_not_sent_and_a_current_one_is_sent() {
    let attach_state = Arc::new(StallState::default());
    let sup_state = Arc::new(StallState::default());
    let attach_conn = Arc::new(StallClient(Arc::clone(&attach_state)));
    let sup_conn = StallClient(Arc::clone(&sup_state));
    let (tx, rx) = mpsc::channel::<WorkerMsg>();
    let discarded = Arc::new(AtomicUsize::new(0));
    let worker = spawn_stall_steady(rx, attach_conn, sup_conn, 2, Arc::clone(&discarded));

    tx.send(input_msg(b"x", 1)).unwrap();
    let started = Instant::now();
    while discarded.load(Ordering::SeqCst) != 1 {
        assert!(started.elapsed() < WORKER_TICK * 5, "the stale input was never counted");
        thread::sleep(Duration::from_millis(5));
    }
    assert!(attach_state.writes.lock().unwrap().is_empty(), "a stale input must write nothing");

    tx.send(input_msg(b"y", 2)).unwrap();
    let sent = Instant::now();
    while attach_state.writes.lock().unwrap().is_empty() {
        assert!(sent.elapsed() < WORKER_TICK * 5, "the current input was never sent");
        thread::sleep(Duration::from_millis(5));
    }
    // The worker clears the count after its write, so wait for the clear.
    while discarded.load(Ordering::SeqCst) != 0 {
        assert!(sent.elapsed() < WORKER_TICK * 5, "a current input clears the count");
        thread::sleep(Duration::from_millis(5));
    }

    tx.send(WorkerMsg::Shutdown).unwrap();
    let outcome = worker.join().expect("the worker thread must not panic");
    assert!(matches!(outcome, SteadyOutcome::Shutdown));
}

/// A current input that finds the take queue full is dropped whole and
/// counted, so no typed key vanishes uncounted.
#[test]
fn an_input_dropped_whole_by_a_full_take_queue_is_counted() {
    let attach_state = Arc::new(StallState::default());
    let sup_state = Arc::new(StallState::default());
    let attach_conn = Arc::new(StallClient(Arc::clone(&attach_state)));
    let sup_conn = StallClient(Arc::clone(&sup_state));
    let (tx, rx) = mpsc::channel::<WorkerMsg>();
    let discarded = Arc::new(AtomicUsize::new(0));
    let worker = spawn_stall_steady(rx, attach_conn, sup_conn, 0, Arc::clone(&discarded));

    tx.send(input_msg(&vec![b'a'; crate::attach_client::rules::TAKE_QUEUE_CAP], 0)).unwrap();
    tx.send(input_msg(b"z", 0)).unwrap();
    let started = Instant::now();
    while discarded.load(Ordering::SeqCst) != 1 {
        assert!(started.elapsed() < WORKER_TICK * 5, "the dropped input was never counted");
        thread::sleep(Duration::from_millis(5));
    }

    // Back-to-back whole drops accumulate; none wipes the count.
    tx.send(input_msg(b"y", 0)).unwrap();
    let started = Instant::now();
    while discarded.load(Ordering::SeqCst) != 2 {
        assert!(started.elapsed() < WORKER_TICK * 5, "the second dropped input was not added to the count");
        thread::sleep(Duration::from_millis(5));
    }

    tx.send(WorkerMsg::Shutdown).unwrap();
    let outcome = worker.join().expect("the worker thread must not panic");
    assert!(matches!(outcome, SteadyOutcome::Shutdown));
}

#[test]
fn a_keystroke_is_written_while_a_status_probe_is_outstanding() {
    let attach_state = Arc::new(StallState::default());
    let sup_state = Arc::new(StallState::default());
    let attach_conn = Arc::new(StallClient(Arc::clone(&attach_state)));
    let sup_conn = StallClient(Arc::clone(&sup_state));
    let (tx, rx) = mpsc::channel::<WorkerMsg>();
    let worker = thread::spawn(move || {
        let mut take = TakeTransaction::new();
        let mut take_intent = TakeIntent::Ordinary;
        let mut outstanding = OutstandingSlot::new();
        let mut quit = QuitDispatcher::new();
        let mut reconnect = ReconnectState::new();
        let (mut cols, mut rows, mut take_epoch) = (80u16, 24u16, 0u64);
        // Due at once: the first tick launches the probe.
        let mut last_poll = Instant::now().checked_sub(LIVENESS_POLL_INTERVAL).unwrap_or_else(Instant::now);
        run_steady_state::<StallEndpoint>(
            &StallEndpoint,
            &rx,
            &|_e| {},
            "h",
            &attach_conn,
            sup_conn,
            FrameReader::new(),
            &mut take,
            &mut take_intent,
            &mut outstanding,
            &mut quit,
            &mut reconnect,
            &mut cols,
            &mut rows,
            &mut take_epoch,
            "controller-1",
            "voyage-1",
            &mut last_poll,
            &Arc::new(AtomicU64::new(0)),
            &Arc::new(Mutex::new(None)),
            &Arc::new(AtomicU64::new(0)),
            1,
            &Arc::new(AtomicUsize::new(0)),
        )
    });

    // The Status request is outstanding: the probe's read is parked.
    let started = Instant::now();
    while !sup_state.read_entered.load(Ordering::SeqCst) {
        assert!(started.elapsed() < LIVENESS_POLL_INTERVAL * 2, "the liveness probe never started");
        thread::sleep(Duration::from_millis(5));
    }

    // Watching: the keystroke's `take` goes out on the attach connection.
    let reservation = IngressReservation { ingress_bytes: Arc::new(AtomicUsize::new(1)), n: 1 };
    tx.send(WorkerMsg::Input(b"x".to_vec(), reservation, 1)).unwrap();
    let sent = Instant::now();
    while attach_state.writes.lock().unwrap().is_empty() {
        assert!(sent.elapsed() < WORKER_TICK * 5, "the keystroke waited behind the outstanding Status probe");
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !*sup_state.cancelled.lock().unwrap(),
        "the probe must still be outstanding when the keystroke goes out"
    );

    tx.send(WorkerMsg::Shutdown).unwrap();
    let outcome = worker.join().expect("the worker thread must not panic");
    assert!(matches!(outcome, SteadyOutcome::Shutdown), "Shutdown must end the steady state");
}

// -------------------------------------------------------------------
// The link-gate lane's required pair: keys typed while a re-attach's
// handshake is held are never delivered once it completes. The whole
// worker runs against a scripted endpoint; the re-attach's hello
// reply is held on a gate the test releases, so no clock decides the
// outcome.
// -------------------------------------------------------------------

const GATE_V1: &str = "11111111-1111-1111-1111-111111111111";
const GATE_V2: &str = "22222222-2222-2222-2222-222222222222";

#[derive(Default)]
struct GateConnState {
    out: VecDeque<u8>,
    closed: bool,
    held_hello: Option<Vec<u8>>,
}

/// One scripted voyage connection: answers the attach lane in-process.
/// The second connection (the re-attach) withholds its hello reply
/// until [`GateConn::release`].
struct GateConn {
    gated: bool,
    state: Mutex<GateConnState>,
    cv: Condvar,
    splitter: Mutex<crate::lane::wire::FrameSplitter>,
    entered: Sender<()>,
    inputs: Sender<Vec<u8>>,
}
impl GateConn {
    fn push(&self, frame: AttachServer) {
        let bytes = wire::encode_attach_server(&frame).expect("encode");
        self.state.lock().unwrap().out.extend(bytes);
        self.cv.notify_all();
    }
    fn release(&self) {
        let held = self.state.lock().unwrap().held_hello.take();
        if let Some(bytes) = held {
            self.state.lock().unwrap().out.extend(bytes);
            self.cv.notify_all();
        }
    }
    fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.cv.notify_all();
    }
}
/// A voyage connection, or the supervisor lane (every `Status` answered
/// `Ready` on the voyage the test currently names).
enum GateClient {
    Voyage(Arc<GateConn>),
    Sup(Arc<Mutex<String>>),
}
impl Client for GateClient {
    fn write_all(&self, bytes: &[u8]) -> Result<(), crate::lane::transport::TransportError> {
        let GateClient::Voyage(conn) = self else { return Ok(()) };
        let (frames, _) = conn.splitter.lock().unwrap().feed(bytes);
        for f in frames {
            match f {
                DecodedFrame::AttachClient(AttachClient::Hello { proto }) => {
                    if conn.gated {
                        let held = wire::encode_attach_server(&AttachServer::HelloOk { proto }).expect("encode");
                        conn.state.lock().unwrap().held_hello = Some(held);
                        let _ = conn.entered.send(());
                    } else {
                        conn.push(AttachServer::HelloOk { proto });
                    }
                }
                DecodedFrame::AttachClient(AttachClient::Attach { .. }) => {
                    conn.push(AttachServer::CheckpointChunk { last: true, bytes: b"$ ".to_vec() });
                }
                DecodedFrame::AttachClient(AttachClient::Take { .. }) => conn.push(AttachServer::TakeOk { take_epoch: 1 }),
                DecodedFrame::AttachClient(AttachClient::Input { payload, .. }) => {
                    let _ = conn.inputs.send(payload);
                    conn.push(AttachServer::InputRecorded);
                }
                DecodedFrame::AttachClient(AttachClient::Resize { .. }) => conn.push(AttachServer::ResizeOk),
                _ => {}
            }
        }
        Ok(())
    }
    fn read(&self, buf: &mut [u8]) -> Result<usize, crate::lane::transport::TransportError> {
        let conn = match self {
            GateClient::Voyage(conn) => conn,
            GateClient::Sup(voyage) => {
                let reply = SupervisorReply::StatusOk {
                    pid: 1,
                    created: 1,
                    voyage: Some(voyage.lock().unwrap().clone()),
                    leg: Some(1),
                    phase: SupervisorPhase::Ready,
                };
                let frame = wire::encode_supervisor_reply(&reply).expect("encode");
                buf[..frame.len()].copy_from_slice(&frame);
                return Ok(frame.len());
            }
        };
        let mut st = conn.state.lock().unwrap();
        loop {
            if st.closed {
                return Err(crate::lane::transport::TransportError::Cancelled);
            }
            if !st.out.is_empty() {
                let n = st.out.len().min(buf.len());
                for slot in buf.iter_mut().take(n) {
                    *slot = st.out.pop_front().unwrap();
                }
                return Ok(n);
            }
            st = conn.cv.wait(st).unwrap();
        }
    }
    fn cancel(&self) {
        if let GateClient::Voyage(conn) = self {
            conn.close();
        }
    }
}

struct GateEndpoint {
    voyage: Arc<Mutex<String>>,
    conns: Arc<Mutex<Vec<Arc<GateConn>>>>,
    entered: Sender<()>,
    inputs: Sender<Vec<u8>>,
}
impl Endpoint for GateEndpoint {
    type Client = GateClient;
    type Process = TestProcess;

    fn connect_voyage_unchallenged(&self, _lane: &str, _voyage_id: &str) -> Result<Self::Client, crate::lane::transport::TransportError> {
        let mut conns = self.conns.lock().unwrap();
        let conn = Arc::new(GateConn {
            gated: !conns.is_empty(),
            state: Mutex::new(GateConnState::default()),
            cv: Condvar::new(),
            splitter: Mutex::new(crate::lane::wire::FrameSplitter::new()),
            entered: self.entered.clone(),
            inputs: self.inputs.clone(),
        });
        conns.push(Arc::clone(&conn));
        Ok(GateClient::Voyage(conn))
    }
    fn connect_supervisor_unchallenged(&self, _lane: &str) -> Result<Self::Client, crate::lane::transport::TransportError> {
        Ok(GateClient::Sup(Arc::clone(&self.voyage)))
    }
    fn challenge(&self, _conn: &Self::Client, _exchange: &mut dyn crate::identity::exchange::IdentityExchange, _deadline: Instant) -> ChallengeOutcome<Self::Process> {
        ChallengeOutcome::Proven(TestProcess)
    }
    fn authenticate_server(&self, _conn: &Self::Client) -> PeerAuthOutcome {
        PeerAuthOutcome::Authenticated(crate::identity::challenge::PeerAuthenticated { pid: 7, created: 7 })
    }
}

/// Drives the full worker through attach, a dropped lane and a re-attach
/// whose hello reply is held on a gate; types four keys while it is
/// held, then releases it. Once the pane reports attached again, a
/// current key is sent; no write up to and including it may carry a
/// key typed during the handshake.
fn keys_typed_during_a_held_handshake_are_not_delivered(change_voyage: bool) {
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (inputs_tx, inputs_rx) = mpsc::channel::<Vec<u8>>();
    let (events_tx, events_rx) = mpsc::channel::<WorkerEvent>();
    let voyage = Arc::new(Mutex::new(GATE_V1.to_string()));
    let conns: Arc<Mutex<Vec<Arc<GateConn>>>> = Arc::new(Mutex::new(Vec::new()));
    let endpoint = GateEndpoint { voyage: Arc::clone(&voyage), conns: Arc::clone(&conns), entered: entered_tx, inputs: inputs_tx };
    let events_tx = Mutex::new(events_tx);
    let worker = AttachWorker::<GateEndpoint>::spawn(
        endpoint,
        "row".to_string(),
        80,
        24,
        "controller-1".to_string(),
        "handle".to_string(),
        None,
        false,
        64 * 1024,
        Arc::new(AtomicU64::new(0)),
        Arc::new(Mutex::new(None)),
        Arc::new(AtomicU64::new(0)),
        move |e| {
            let _ = events_tx.lock().unwrap().send(e);
        },
    )
    .expect("spawn the worker");
    // Bounds only: a failure to progress must fail the test, never decide it.
    let bound = Duration::from_secs(30);
    let mut attached = 0usize;
    let mut wait_attached = |want: usize| {
        while attached < want {
            match events_rx.recv_timeout(bound).expect("the worker never reported attached") {
                WorkerEvent::Status(s) if s == "attached" => attached += 1,
                WorkerEvent::Terminal(t) => panic!("the worker went terminal: {t}"),
                _ => {}
            }
        }
    };

    wait_attached(1);
    if change_voyage {
        *voyage.lock().unwrap() = GATE_V2.to_string();
    }
    let first = Arc::clone(&conns.lock().unwrap()[0]);
    first.close();
    entered_rx.recv_timeout(bound).expect("the re-attach handshake was never entered");
    for i in 0..4 {
        worker.send_input(format!("stale-{i}").into_bytes()).expect("ingress");
    }
    let second = Arc::clone(&conns.lock().unwrap()[1]);
    second.release();
    wait_attached(2);

    // The worker reads the four stale inputs once it is attached;
    // the poll is bounded only so a lost count fails the test.
    let started = Instant::now();
    while worker.inputs_discarded() != 4 {
        assert!(started.elapsed() < bound, "the stale inputs were never counted: {}", worker.inputs_discarded());
        thread::sleep(Duration::from_millis(5));
    }

    worker.send_input(b"current".to_vec()).expect("ingress");
    let mut written: Vec<Vec<u8>> = Vec::new();
    loop {
        let payload = inputs_rx.recv_timeout(bound).expect("the current key never reached the capsule");
        let is_current = payload.windows(7).any(|w| w == b"current");
        written.push(payload);
        if is_current {
            break;
        }
    }
    let delivered: Vec<String> = written.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect();
    assert!(
        delivered.iter().all(|p| !p.contains("stale-")),
        "keys typed while the pane re-attached were delivered: {delivered:?}"
    );
    assert_eq!(worker.inputs_discarded(), 0, "a current input clears the count");

    for c in conns.lock().unwrap().iter() {
        c.close();
    }
    drop(worker);
}

#[test]
fn keys_typed_while_a_re_attach_handshake_is_held_are_never_delivered() {
    keys_typed_during_a_held_handshake_are_not_delivered(false);
}

#[test]
fn keys_typed_while_a_re_attach_handshake_is_held_are_never_delivered_into_a_restarted_voyage() {
    keys_typed_during_a_held_handshake_are_not_delivered(true);
}
