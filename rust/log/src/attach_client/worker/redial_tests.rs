//! Tests: the redial pace against a lane that accepts and then drops. The episode reconnect and the steady-state
//! supervisor re-dial each wait the doubling `Redial`, which only an attach or a lane that lasted `STABLE` restarts.

use crate::attach_client::rules::{OutstandingSlot, QuitDispatcher, ReconnectState, TakeTransaction};
use crate::identity::challenge::{ChallengeOutcome, PeerAuthOutcome};
use crate::lane::client::{Client, Endpoint};
use crate::lane::transport::TransportError;
use crate::lane::wire::{self, AttachClient, AttachServer, DecodedFrame, SupervisorPhase, SupervisorReply};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::support_tests::*;
use super::*;

const VOYAGE: &str = "33333333-3333-3333-3333-333333333333";

/// One end of a lane that accepts and then drops. `Ready` is a supervisor lane that answers every `Status` with
/// `Ready`; `Dropped` is a lane whose every read finds the connection closed; `Voyage` answers the attach hello and the
/// attach with a one-chunk checkpoint, then its reads find the connection closed.
enum DropClient {
    Ready,
    Dropped,
    Voyage { out: Mutex<VecDeque<u8>>, splitter: Mutex<wire::FrameSplitter> },
}

impl Client for DropClient {
    fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError> {
        let DropClient::Voyage { out, splitter } = self else { return Ok(()) };
        let (frames, _) = splitter.lock().unwrap().feed(bytes);
        for frame in frames {
            let reply = match frame {
                DecodedFrame::AttachClient(AttachClient::Hello { proto }) => AttachServer::HelloOk { proto },
                DecodedFrame::AttachClient(AttachClient::Attach { .. }) => {
                    AttachServer::CheckpointChunk { last: true, bytes: b"$ ".to_vec() }
                }
                _ => continue,
            };
            out.lock().unwrap().extend(wire::encode_attach_server(&reply).expect("encode"));
        }
        Ok(())
    }
    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let bytes: Vec<u8> = match self {
            DropClient::Ready => {
                let reply = SupervisorReply::StatusOk {
                    pid: 1,
                    created: 1,
                    voyage: Some(VOYAGE.to_string()),
                    leg: Some(1),
                    phase: SupervisorPhase::Ready,
                };
                wire::encode_supervisor_reply(&reply).expect("encode")
            }
            DropClient::Dropped => Vec::new(),
            DropClient::Voyage { out, .. } => out.lock().unwrap().drain(..).collect(),
        };
        buf[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
    fn cancel(&self) {}
}

/// Accepts every dial and counts it; `sup_drops` picks whether the supervisor lane it hands out answers or drops.
struct DropEndpoint {
    sup_drops: bool,
    supervisor_dials: Arc<AtomicUsize>,
    voyage_dials: Arc<AtomicUsize>,
}
impl Endpoint for DropEndpoint {
    type Client = DropClient;
    type Process = TestProcess;

    fn connect_voyage_unchallenged(&self, _lane: &str, _voyage_id: &str) -> Result<DropClient, TransportError> {
        self.voyage_dials.fetch_add(1, Ordering::AcqRel);
        Ok(DropClient::Voyage { out: Mutex::new(VecDeque::new()), splitter: Mutex::new(wire::FrameSplitter::new()) })
    }
    fn connect_supervisor_unchallenged(&self, _lane: &str) -> Result<DropClient, TransportError> {
        self.supervisor_dials.fetch_add(1, Ordering::AcqRel);
        Ok(if self.sup_drops { DropClient::Dropped } else { DropClient::Ready })
    }
    fn challenge(
        &self,
        _conn: &DropClient,
        _exchange: &mut dyn crate::identity::exchange::IdentityExchange,
        _deadline: Instant,
    ) -> ChallengeOutcome<TestProcess> {
        ChallengeOutcome::Proven(TestProcess)
    }
    fn authenticate_server(&self, _conn: &DropClient) -> PeerAuthOutcome {
        PeerAuthOutcome::Authenticated(crate::identity::challenge::PeerAuthenticated { pid: 7, created: 7 })
    }
}

/// An attach that completes and then drops at once is redialed on the doubling wait, 250 ms to 4 s, not every
/// 250 ms: a completed attach is not a working session (`ReconnectState::retry_after_session`). The whole worker runs
/// against the endpoint; each voyage dial is one episode. The waits are lower bounds, so a slow machine can only lower
/// the count.
#[test]
fn an_attach_that_drops_at_once_is_redialed_on_the_doubling_wait() {
    const WINDOW: Duration = Duration::from_secs(3);
    let voyage_dials = Arc::new(AtomicUsize::new(0));
    let endpoint =
        DropEndpoint { sup_drops: false, supervisor_dials: Arc::new(AtomicUsize::new(0)), voyage_dials: Arc::clone(&voyage_dials) };
    let worker = AttachWorker::<DropEndpoint>::spawn(
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
        |_e| {},
    )
    .expect("spawn the worker");
    thread::sleep(WINDOW);
    let dials = voyage_dials.load(Ordering::Acquire);
    drop(worker);
    println!("redial: {dials} attach episodes in {WINDOW:?} against a voyage that drops after its checkpoint");
    // The doubling wait from 250 ms attaches at 0, 0.25, 0.75 and 1.75 s, then not before 3.75 s.
    assert!(dials >= 1, "the worker never attached");
    assert!(dials <= 4, "{dials} attach episodes in {WINDOW:?}: a completed attach restarted the wait");
}

/// A supervisor lane that accepts every dial and then drops is re-dialed on the doubling wait, 2 s to 30 s, not at
/// every probe: a bare connect is not a working lane (`probe_supervisor_lane`). The steady state runs against the
/// endpoint with its first probe 2 s in, as the worker sets it; each supervisor dial is one re-dial. The waits are
/// lower bounds, so a slow machine can only lower the count.
#[test]
fn a_supervisor_lane_that_accepts_and_drops_is_redialed_on_the_doubling_wait() {
    const WINDOW: Duration = Duration::from_secs(11);
    let supervisor_dials = Arc::new(AtomicUsize::new(0));
    let endpoint =
        DropEndpoint { sup_drops: true, supervisor_dials: Arc::clone(&supervisor_dials), voyage_dials: Arc::new(AtomicUsize::new(0)) };
    let (cmd_tx, cmd_rx) = mpsc::channel::<WorkerMsg>();
    let steady = thread::spawn(move || {
        let mut take = TakeTransaction::new();
        let mut take_intent = TakeIntent::Ordinary;
        let mut outstanding = OutstandingSlot::new();
        let mut quit = QuitDispatcher::new();
        let mut reconnect = ReconnectState::new();
        let (mut cols, mut rows, mut take_epoch) = (80u16, 24u16, 0u64);
        let mut last_poll = Instant::now();
        run_steady_state::<DropEndpoint>(
            &endpoint,
            &cmd_rx,
            &|_e| {},
            "h",
            &Arc::new(DropClient::Dropped),
            DropClient::Dropped,
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
            VOYAGE,
            &mut last_poll,
            &Arc::new(AtomicU64::new(0)),
            &Arc::new(Mutex::new(None)),
            &Arc::new(AtomicU64::new(0)),
            1,
            &Arc::new(AtomicUsize::new(0)),
        )
    });
    thread::sleep(WINDOW);
    let dials = supervisor_dials.load(Ordering::Acquire);
    cmd_tx.send(WorkerMsg::Shutdown).unwrap();
    assert!(matches!(steady.join().unwrap(), SteadyOutcome::Shutdown), "the steady state did not end on Shutdown");
    println!("redial: {dials} supervisor re-dials in {WINDOW:?} against a lane that accepts and drops");
    // The doubling wait from 2 s re-dials at 2 and 6 s, then not before 14 s.
    assert!(dials >= 1, "the steady state never re-dialed its supervisor lane");
    assert!(dials <= 2, "{dials} supervisor re-dials in {WINDOW:?}: a bare connect restarted the wait");
}
