//! Tests: the redial pace against a lane that accepts and then drops, against one whose re-dial fails, and for a lane
//! that lasted `STABLE`. Each records the instant of every dial and checks the gaps against the doubling `Redial` wait,
//! which only an attach or a lane that lasted `STABLE` restarts.

use crate::attach_client::rules::{OutstandingSlot, QuitDispatcher, ReconnectState, TakeTransaction};
use crate::host::redial::{Redial, STABLE};
use crate::identity::challenge::{ChallengeOutcome, PeerAuthOutcome};
use crate::lane::client::{Client, Endpoint};
use crate::lane::transport::TransportError;
use crate::lane::wire::{self, AttachClient, AttachServer, DecodedFrame, SupervisorPhase, SupervisorReply};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::support_tests::*;
use super::*;

const VOYAGE: &str = "33333333-3333-3333-3333-333333333333";

/// The probe reads the clock just before it dials, and the endpoint stamps the dial just after: two dials' stamps can
/// fall short of the wait between them by that lead. 100 ms covers it.
const STAMP_LEAD: Duration = Duration::from_millis(100);

/// One end of a lane. `Ready` is a supervisor lane that answers every `Status` with `Ready`; `Dropped` is a lane whose
/// every read finds the connection closed; `Voyage` answers the attach hello and the attach with a one-chunk checkpoint,
/// then its reads find the connection closed.
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

/// What a supervisor dial gets.
#[derive(Clone, Copy)]
enum Supervisor {
    /// A lane that answers every `Status` with `Ready`.
    Answers,
    /// A lane that accepts and then drops.
    Drops,
    /// No lane: the dial fails.
    Refuses,
}

/// Records the instant of every dial.
struct DropEndpoint {
    supervisor: Supervisor,
    supervisor_dials: Arc<Mutex<Vec<Instant>>>,
    voyage_dials: Arc<Mutex<Vec<Instant>>>,
}
impl DropEndpoint {
    fn new(supervisor: Supervisor) -> Self {
        Self { supervisor, supervisor_dials: Arc::default(), voyage_dials: Arc::default() }
    }
}
impl Endpoint for DropEndpoint {
    type Client = DropClient;
    type Process = TestProcess;

    fn connect_voyage_unchallenged(&self, _lane: &str, _voyage_id: &str) -> Result<DropClient, TransportError> {
        self.voyage_dials.lock().unwrap().push(Instant::now());
        Ok(DropClient::Voyage { out: Mutex::new(VecDeque::new()), splitter: Mutex::new(wire::FrameSplitter::new()) })
    }
    fn connect_supervisor_unchallenged(&self, _lane: &str) -> Result<DropClient, TransportError> {
        self.supervisor_dials.lock().unwrap().push(Instant::now());
        match self.supervisor {
            Supervisor::Answers => Ok(DropClient::Ready),
            Supervisor::Drops => Ok(DropClient::Dropped),
            Supervisor::Refuses => Err(TransportError::Unreachable(std::io::Error::other("no supervisor lane here"))),
        }
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

/// The instants in `dials` once there are `n` of them, or whatever is there when `bound` has passed.
fn wait_for_dials(dials: &Mutex<Vec<Instant>>, n: usize, bound: Duration) -> Vec<Instant> {
    let began = Instant::now();
    loop {
        let seen = dials.lock().unwrap().clone();
        if seen.len() >= n || began.elapsed() >= bound {
            return seen;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// An attach that completes and then drops at once is redialed on the doubling wait, 250 ms to 4 s, not every
/// 250 ms: a completed attach is not a working session (`ReconnectState::retry_after`). The whole worker runs against
/// the endpoint; each voyage dial is one episode, and each episode waits at least the doubling wait after the one before
/// it ends, so a slow machine can only widen a gap. The bound only fails a worker that stops redialing.
#[test]
fn an_attach_that_drops_at_once_is_redialed_on_the_doubling_wait() {
    let endpoint = DropEndpoint::new(Supervisor::Answers);
    let voyage_dials = Arc::clone(&endpoint.voyage_dials);
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
    let dials = wait_for_dials(&voyage_dials, 4, Duration::from_secs(20));
    drop(worker);
    let gaps: Vec<Duration> = dials.windows(2).map(|w| w[1] - w[0]).collect();
    println!("redial: {} attach episodes, gaps {gaps:?}, against a voyage that drops after its checkpoint", dials.len());
    assert!(dials.len() >= 4, "the worker stopped redialing: {} attach episodes in 20 s", dials.len());
    for (gap, wait) in gaps.iter().zip([250, 500, 1_000]) {
        assert!(*gap >= Duration::from_millis(wait), "a gap of {gap:?} where the doubling waits {wait} ms: a completed attach restarted the wait");
    }
}

/// Runs the steady state against a supervisor lane that `supervisor` decides, its first probe 2 s in as the worker sets
/// it, until two supervisor dials or 20 s, and returns the dials' instants.
fn supervisor_dials(supervisor: Supervisor) -> Vec<Instant> {
    let endpoint = DropEndpoint::new(supervisor);
    let dials = Arc::clone(&endpoint.supervisor_dials);
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
    let seen = wait_for_dials(&dials, 2, Duration::from_secs(20));
    cmd_tx.send(WorkerMsg::Shutdown).unwrap();
    assert!(matches!(steady.join().unwrap(), SteadyOutcome::Shutdown), "the steady state did not end on Shutdown");
    seen
}

/// A supervisor lane that accepts every dial and then drops is re-dialed on the doubling wait, 2 s then 4 s, not at
/// every probe: a bare connect is not a working lane (`probe_supervisor_lane`). A slow machine can only widen the gap.
#[test]
fn a_supervisor_lane_that_accepts_and_drops_is_redialed_on_the_doubling_wait() {
    let dials = supervisor_dials(Supervisor::Drops);
    let gaps: Vec<Duration> = dials.windows(2).map(|w| w[1] - w[0]).collect();
    println!("redial: {} supervisor re-dials, gaps {gaps:?}, against a lane that accepts and drops", dials.len());
    assert!(dials.len() >= 2, "the steady state stopped re-dialing its supervisor lane: {} re-dials in 20 s", dials.len());
    assert!(gaps[0] >= Duration::from_secs(4) - STAMP_LEAD, "a gap of {:?} where the doubling waits 4 s: a bare connect restarted the wait", gaps[0]);
}

/// A supervisor re-dial that fails waits the doubling before the next one, not the next 2 s probe
/// (`probe_supervisor_lane`'s failed arm). A slow machine can only widen the gap.
#[test]
fn a_failed_supervisor_redial_waits_the_doubling() {
    let dials = supervisor_dials(Supervisor::Refuses);
    let gaps: Vec<Duration> = dials.windows(2).map(|w| w[1] - w[0]).collect();
    println!("redial: {} failed supervisor re-dials, gaps {gaps:?}", dials.len());
    assert!(dials.len() >= 2, "the steady state stopped re-dialing its supervisor lane: {} re-dials in 20 s", dials.len());
    assert!(gaps[0] >= Duration::from_secs(4) - STAMP_LEAD, "a gap of {:?} where the doubling waits 4 s: a failed re-dial was retried at the next probe", gaps[0]);
}

/// A supervisor lane that lasted `STABLE` re-dials at its first missed probe, at once, and its wait starts over: 4 s
/// after that re-dial fails the next is due, not at the 30 s the doubling had climbed to. A lane dialed just now waits.
/// `probe_supervisor_lane` is called directly, and 4 s pass by moving the lane's recorded instants back, so nothing
/// sleeps.
#[test]
fn a_supervisor_lane_that_lasted_stable_redials_at_once_and_starts_its_wait_over() {
    let lane_aged = |age: Duration| {
        let mut redial = Redial::new(SUPERVISOR_REDIAL_INITIAL, SUPERVISOR_REDIAL_MAX);
        for _ in 0..5 {
            redial.after(Duration::ZERO);
        }
        SupLane {
            conn: DropClient::Dropped,
            reader: FrameReader::new(),
            dialed_at: Instant::now().checked_sub(age).expect("the monotonic clock has run for a minute"),
            redial_at: None,
            redial,
        }
    };
    let back = |at: Instant| at.checked_sub(Duration::from_secs(4)).expect("the monotonic clock has run for a minute");
    let endpoint = DropEndpoint::new(Supervisor::Refuses);
    let dials = || endpoint.supervisor_dials.lock().unwrap().len();
    let mut fresh = lane_aged(Duration::ZERO);
    assert!(probe_supervisor_lane::<DropEndpoint>(&endpoint, "h", &mut fresh).is_err(), "the dropped lane answered");
    assert_eq!(dials(), 0, "a lane dialed just now re-dialed at its first missed probe");
    let mut lasted = lane_aged(STABLE);
    assert!(probe_supervisor_lane::<DropEndpoint>(&endpoint, "h", &mut lasted).is_err(), "the dropped lane answered");
    assert_eq!(dials(), 1, "a lane that lasted STABLE did not re-dial at its first missed probe");
    lasted.dialed_at = back(lasted.dialed_at);
    lasted.redial_at = lasted.redial_at.map(back);
    assert!(probe_supervisor_lane::<DropEndpoint>(&endpoint, "h", &mut lasted).is_err(), "the dropped lane answered");
    assert_eq!(dials(), 2, "4 s after the re-dial failed the next was not due: the wait did not start over");
}
