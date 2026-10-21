//! Tests: the redial pace against a lane that accepts and then drops, against one whose re-dial fails, and for a lane
//! that lasted `STABLE`. Each records the instant of every dial and checks the gaps against the doubling `Redial` wait,
//! which only an attach or a lane that lasted `STABLE` restarts. Last, the logins one host sees while the user steps
//! through a window's parked rows: a client off screen does not redial.

use crate::attach_client::rules::{OutstandingSlot, QuitDispatcher, ReconnectState, TakeTransaction};
use sot_platform::host::redial::{Redial, STABLE};
use sot_platform::identity::challenge::{ChallengeOutcome, PeerAuthOutcome};
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
        _exchange: &mut dyn sot_platform::identity::exchange::IdentityExchange,
        _deadline: Instant,
    ) -> ChallengeOutcome<TestProcess> {
        ChallengeOutcome::Proven(TestProcess)
    }
    fn authenticate_server(&self, _conn: &DropClient) -> PeerAuthOutcome {
        PeerAuthOutcome::Authenticated(sot_platform::identity::challenge::PeerAuthenticated { pid: 7, created: 7 })
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

/// `n` workers, row-0 to row-(n-1), each on its own endpoint from `endpoint`.
fn spawn_rows<E: Endpoint + Send + Sync + 'static>(n: usize, endpoint: impl Fn(usize) -> E) -> Vec<AttachWorker<E>>
where
    E::Client: 'static,
{
    (0..n)
        .map(|i| {
            AttachWorker::<E>::spawn(
                endpoint(i),
                format!("row-{i}"),
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
            .expect("spawn the worker")
        })
        .collect()
}

/// One host as a window's attach workers reach it, each supervisor dial one login. While the link is down a dial is
/// answered `LinkDown` at once, as the host's link gate answers; otherwise its login stays open until the test refuses
/// it, so the count of logins open at once does not depend on how fast the machine is.
#[derive(Default)]
struct Host {
    state: Mutex<HostState>,
    changed: std::sync::Condvar,
}

#[derive(Default)]
struct HostState {
    link_up: bool,
    link_down_answers: usize,
    /// One entry per open login, naming its row.
    open: Vec<String>,
    most_open: usize,
    /// Refusals granted and not yet taken, by row.
    refusals: Vec<String>,
    /// Every login is refused at once: the test is ending.
    closing: bool,
}

impl Host {
    fn update(&self, change: impl FnOnce(&mut HostState)) {
        change(&mut self.state.lock().unwrap());
        self.changed.notify_all();
    }

    /// Whether `done` held within 10 s.
    fn wait_for(&self, done: impl Fn(&HostState) -> bool) -> bool {
        let (_state, waited) = self.changed.wait_timeout_while(self.state.lock().unwrap(), Duration::from_secs(10), |s| !done(s)).unwrap();
        !waited.timed_out()
    }

    /// Whether the link is up; a dial that finds it down is counted and answered at once.
    fn answer_link(&self) -> bool {
        let mut s = self.state.lock().unwrap();
        if !s.link_up {
            s.link_down_answers += 1;
            self.changed.notify_all();
        }
        s.link_up
    }

    /// Opens a login for `lane` and holds it until the test refuses it (or ends).
    fn hold_login(&self, lane: &str) {
        let mut s = self.state.lock().unwrap();
        s.open.push(lane.to_string());
        s.most_open = s.most_open.max(s.open.len());
        self.changed.notify_all();
        while !s.closing {
            if let Some(i) = s.refusals.iter().position(|r| r == lane) {
                s.refusals.remove(i);
                break;
            }
            s = self.changed.wait(s).unwrap();
        }
        let i = s.open.iter().position(|r| r == lane).expect("the login's own entry");
        s.open.remove(i);
        self.changed.notify_all();
    }

    /// Refuses `row`'s open login and waits until it has closed.
    fn refuse(&self, row: &str) {
        let open = |s: &HostState| s.open.iter().filter(|r| *r == row).count();
        let before = open(&self.state.lock().unwrap());
        self.update(|s| s.refusals.push(row.to_string()));
        assert!(self.wait_for(|s| open(s) < before), "{row}'s refused login did not close");
    }
}

struct HostEndpoint(Arc<Host>);
impl Endpoint for HostEndpoint {
    type Client = TestClient;
    type Process = TestProcess;

    fn connect_supervisor_unchallenged(&self, lane: &str) -> Result<TestClient, TransportError> {
        if !self.0.answer_link() {
            return Err(TransportError::LinkDown);
        }
        self.0.hold_login(lane);
        Err(TransportError::Unreachable(std::io::Error::other("kex_exchange_identification: Connection reset by peer")))
    }
    fn connect_voyage_unchallenged(&self, _lane: &str, _voyage_id: &str) -> Result<TestClient, TransportError> {
        unreachable!("no supervisor dial succeeds, so no voyage is dialed")
    }
    fn challenge(
        &self,
        _conn: &TestClient,
        _exchange: &mut dyn sot_platform::identity::exchange::IdentityExchange,
        _deadline: Instant,
    ) -> ChallengeOutcome<TestProcess> {
        unreachable!("no supervisor dial succeeds")
    }
    fn authenticate_server(&self, _conn: &TestClient) -> PeerAuthOutcome {
        unreachable!("no supervisor dial succeeds")
    }
    fn link_up(&self) -> bool {
        self.0.state.lock().unwrap().link_up
    }
}

/// The record of 2026-10-13 (SSH-BURST) on one host: the window wakes with its rows parked and the host's link down,
/// the link comes back, the user steps through the rows, and every row's login fails after the user has left it. A
/// client off screen finishes the attempt in flight and starts no other until it is viewed (ADR 0045 decision 11), so
/// the host sees one login at a time, the viewed row's, however many rows the user passed. Twelve rows is more than
/// sshd's default `MaxStartups` (10) admits.
#[test]
fn parked_rows_open_no_login_while_the_user_steps_through_them() {
    const ROWS: usize = 12;
    let host = Arc::new(Host::default());
    let mut workers = spawn_rows(ROWS, |_| HostEndpoint(Arc::clone(&host)));
    // Each worker's first dial meets the down link and pauses; the window then parks every row.
    assert!(host.wait_for(|s| s.link_down_answers == ROWS), "not every worker dialed into the down link");
    for worker in &workers {
        worker.set_viewed(false);
    }
    host.update(|s| s.link_up = true);
    for (i, worker) in workers.iter().enumerate() {
        let row = format!("row-{i}");
        worker.set_viewed(true);
        assert!(host.wait_for(|s| s.open.contains(&row)), "{row} was viewed and opened no login");
        if i + 1 < ROWS {
            worker.set_viewed(false);
        }
        host.refuse(&row);
    }
    // Every retry the doubling allows after one failed attempt (250 ms) has started by now.
    thread::sleep(Duration::from_secs(3));
    let (most_open, open_at_end) = {
        let s = host.state.lock().unwrap();
        (s.most_open, s.open.clone())
    };
    println!("login burst: rows={ROWS} most_open={most_open} open_at_end={open_at_end:?}");
    host.update(|s| s.closing = true);
    for worker in &mut workers {
        assert!(worker.shutdown(Duration::from_secs(5)), "a worker did not end on Shutdown");
    }
    assert_eq!(most_open, 1, "the host saw {most_open} logins open at once: a parked row dialed while off screen");
    assert_eq!(open_at_end, [format!("row-{}", ROWS - 1)], "only the viewed row may still be dialing");
}

/// The same host with its supervisor answering `Ready` and each voyage dial one login, held open until the test refuses
/// it with `failure`. While the link is down a supervisor dial is answered `LinkDown` at once.
struct VoyageHostEndpoint {
    host: Arc<Host>,
    failure: fn() -> TransportError,
}
impl Endpoint for VoyageHostEndpoint {
    type Client = DropClient;
    type Process = TestProcess;

    fn connect_supervisor_unchallenged(&self, _lane: &str) -> Result<DropClient, TransportError> {
        if self.host.answer_link() { Ok(DropClient::Ready) } else { Err(TransportError::LinkDown) }
    }
    fn connect_voyage_unchallenged(&self, lane: &str, _voyage_id: &str) -> Result<DropClient, TransportError> {
        self.host.hold_login(lane);
        Err((self.failure)())
    }
    fn challenge(
        &self,
        _conn: &DropClient,
        _exchange: &mut dyn sot_platform::identity::exchange::IdentityExchange,
        _deadline: Instant,
    ) -> ChallengeOutcome<TestProcess> {
        ChallengeOutcome::Proven(TestProcess)
    }
    fn authenticate_server(&self, _conn: &DropClient) -> PeerAuthOutcome {
        PeerAuthOutcome::Authenticated(sot_platform::identity::challenge::PeerAuthenticated { pid: 7, created: 7 })
    }
    fn link_up(&self) -> bool {
        self.host.state.lock().unwrap().link_up
    }
}

/// The user steps through twelve parked rows whose supervisor answers and whose voyage login fails after the user has
/// left the row, as `failure` fails it. A client off screen waits out the failure unviewed and dials no voyage login
/// until it is viewed (ADR 0045 decision 11), so the host sees one voyage login at a time, the viewed row's.
fn parked_rows_open_no_voyage_login_after_it_fails(failure: fn() -> TransportError) {
    const ROWS: usize = 12;
    let host = Arc::new(Host::default());
    let mut workers = spawn_rows(ROWS, |_| VoyageHostEndpoint { host: Arc::clone(&host), failure });
    assert!(host.wait_for(|s| s.link_down_answers == ROWS), "not every worker dialed into the down link");
    for worker in &workers {
        worker.set_viewed(false);
    }
    host.update(|s| s.link_up = true);
    for (i, worker) in workers.iter().enumerate() {
        let row = format!("row-{i}");
        worker.set_viewed(true);
        assert!(host.wait_for(|s| s.open.contains(&row)), "{row} was viewed and opened no voyage login");
        if i + 1 < ROWS {
            worker.set_viewed(false);
        }
        host.refuse(&row);
    }
    // Every retry the doubling allows after one failed attempt (250 ms) has started by now.
    thread::sleep(Duration::from_secs(3));
    let (most_open, open_at_end) = {
        let s = host.state.lock().unwrap();
        (s.most_open, s.open.clone())
    };
    println!("voyage login burst: rows={ROWS} most_open={most_open} open_at_end={open_at_end:?}");
    host.update(|s| s.closing = true);
    for worker in &mut workers {
        assert!(worker.shutdown(Duration::from_secs(5)), "a worker did not end on Shutdown");
    }
    assert_eq!(most_open, 1, "the host saw {most_open} voyage logins open at once: a parked row re-dialed its voyage while off screen");
    assert_eq!(open_at_end, [format!("row-{}", ROWS - 1)], "only the viewed row may still be dialing");
}

/// Each arm of `converge_on_ready` that waits to redial a voyage: the login fails `Unreachable`, and it fails with an
/// I/O error. Reverting either arm to a wait that ignores the view makes the parked rows dial again.
#[test]
fn parked_rows_re_dial_no_voyage_after_it_fails() {
    parked_rows_open_no_voyage_login_after_it_fails(|| {
        TransportError::Unreachable(std::io::Error::other("kex_exchange_identification: Connection reset by peer"))
    });
    parked_rows_open_no_voyage_login_after_it_fails(|| TransportError::Io {
        op: "connect voyage pipe",
        source: std::io::Error::from(std::io::ErrorKind::NotFound),
    });
}
