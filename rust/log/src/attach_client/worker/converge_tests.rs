//! Tests: Health probe, absence clock, first attach, link-down pause, dial backoff and attach refusal.

use crate::identity::challenge::{ChallengeOutcome, PeerAuthOutcome};
use crate::lane::client::{Client, Endpoint};
use crate::attach_client::rules::{self, OutstandingSlot, QuitDispatcher, ReconnectDecision, ReconnectState};
use crate::lane::wire::{self, AttachServer, SupervisorPhase, SupervisorReply};
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use super::support_tests::*;

#[test]
fn health_probe_uses_the_last_reported_voyage_id() {
    let ep = TestEndpoint { last_lane_probed: Mutex::new(None), last_voyage_probed: Mutex::new(None) };
    let mut reconnect = ReconnectState::new();
    let now = Instant::now();
    let lane = "sot-capsule-row-1";
    let voyage_id = "11111111-1111-1111-1111-111111111111";

    let decision = on_supervisor_absent_or_unresponsive::<TestEndpoint>(&ep, &mut reconnect, lane, Some(voyage_id), now);
    assert_eq!(decision, ReconnectDecision::Retry, "a reachable voyage pipe must retry, never go terminal");
    assert_eq!(
        ep.last_lane_probed.lock().unwrap().as_deref(),
        Some(lane),
        "the probe must connect through the WORKER'S OWN row, never the voyage id in that slot"
    );
    assert_eq!(
        ep.last_voyage_probed.lock().unwrap().as_deref(),
        Some(voyage_id),
        "the probe must connect to the id its caller passed in, not one it reads itself"
    );

    // `None`: no id to probe with — the health clock starts exactly
    // as it would for an absent supervisor, and a second call
    // `HEALTH_WINDOW` later is `Terminal`.
    let decision = on_supervisor_absent_or_unresponsive::<TestEndpoint>(&ep, &mut reconnect, lane, None, now);
    assert_eq!(decision, ReconnectDecision::Retry, "the clock merely starting is never itself terminal");
    let later = now + rules::HEALTH_WINDOW + Duration::from_secs(1);
    let decision = on_supervisor_absent_or_unresponsive::<TestEndpoint>(&ep, &mut reconnect, lane, None, later);
    assert_eq!(
        decision,
        ReconnectDecision::Terminal(rules::TerminalReason::HealthWindowExpired),
        "the clock started by the first None call must expire after HEALTH_WINDOW"
    );
}

/// A scripted [`Endpoint::connect_voyage_unchallenged`] — one queued
/// outcome per call, so a test can drive the probe through an exact
/// absence/uncertainty/absence sequence.
struct ScriptedEndpoint {
    script: Mutex<std::collections::VecDeque<Result<(), crate::lane::transport::TransportError>>>,
}
impl Endpoint for ScriptedEndpoint {
    type Client = TestClient;
    type Process = TestProcess;

    fn connect_voyage_unchallenged(&self, _lane: &str, _voyage_id: &str) -> Result<Self::Client, crate::lane::transport::TransportError> {
        match self.script.lock().unwrap().pop_front().expect("script exhausted before the test finished driving it") {
            Ok(()) => Ok(TestClient),
            Err(e) => Err(e),
        }
    }
    fn connect_supervisor_unchallenged(&self, _lane: &str) -> Result<Self::Client, crate::lane::transport::TransportError> {
        unreachable!("uncertainty_clears_the_absence_clock never drives the supervisor lane")
    }
    fn challenge(&self, _conn: &Self::Client, _exchange: &mut dyn crate::identity::exchange::IdentityExchange, _deadline: Instant) -> ChallengeOutcome<Self::Process> {
        unreachable!("uncertainty_clears_the_absence_clock never challenges")
    }
    fn authenticate_server(&self, _conn: &Self::Client) -> PeerAuthOutcome {
        unreachable!("uncertainty_clears_the_absence_clock never authenticates")
    }
}

/// ADR 0045 decision 4: `Unreachable`/`Undetermined` must never be
/// charged to the health window — they clear its clock exactly like
/// a reachable voyage pipe would, so a genuine absence that follows
/// gets a FRESH window rather than inheriting time an outage already
/// spent. Sequence: absence starts the clock (Retry, clock running);
/// an `Unreachable` probe clears it (Retry); a later absence starts
/// its OWN clock (Retry, not yet Terminal even though the ORIGINAL
/// clock would have expired by now); that fresh clock still expires
/// on its own after a full `HEALTH_WINDOW` (Terminal) — proving the
/// clear is real, not a permanent bypass.
#[test]
fn uncertainty_clears_the_absence_clock() {
    fn absent() -> crate::lane::transport::TransportError {
        crate::lane::transport::TransportError::Io {
            op: "test",
            source: std::io::Error::new(ErrorKind::NotFound, "absent"),
        }
    }
    fn unreachable_err() -> crate::lane::transport::TransportError {
        crate::lane::transport::TransportError::Unreachable(std::io::Error::new(ErrorKind::TimedOut, "unreachable"))
    }

    let ep = ScriptedEndpoint {
        script: Mutex::new(std::collections::VecDeque::from([Err(absent()), Err(unreachable_err()), Err(absent()), Err(absent())])),
    };
    let mut reconnect = ReconnectState::new();
    let lane = "sot-capsule-row-1";
    let voyage_id = "11111111-1111-1111-1111-111111111111";
    let t0 = Instant::now();

    // 1) A genuine absence starts the clock; not yet terminal.
    let d = on_supervisor_absent_or_unresponsive::<ScriptedEndpoint>(&ep, &mut reconnect, lane, Some(voyage_id), t0);
    assert_eq!(d, ReconnectDecision::Retry, "a clock that just started is never itself terminal");

    // 2) An `Unreachable` probe, well within what would have been
    // the original window, clears the clock instead of merely
    // retrying on top of it.
    let t1 = t0 + rules::HEALTH_WINDOW - Duration::from_secs(10);
    let d = on_supervisor_absent_or_unresponsive::<ScriptedEndpoint>(&ep, &mut reconnect, lane, Some(voyage_id), t1);
    assert_eq!(d, ReconnectDecision::Retry, "Unreachable must retry, never go terminal on its own");

    // 3) Past where the ORIGINAL (t0) clock would have expired --
    // still Retry, because step 2 cleared it: this absence starts
    // its OWN fresh window at t2, not inheriting t0's age.
    let t2 = t0 + rules::HEALTH_WINDOW + Duration::from_secs(1);
    let d = on_supervisor_absent_or_unresponsive::<ScriptedEndpoint>(&ep, &mut reconnect, lane, Some(voyage_id), t2);
    assert_eq!(
        d,
        ReconnectDecision::Retry,
        "the clock step 2 cleared must not let this absence appear to have been running since t0"
    );

    // 4) The FRESH window from step 3 (t2) does eventually expire on
    // its own -- proving step 2/3 cleared and restarted the clock
    // rather than disabling it.
    let t3 = t2 + rules::HEALTH_WINDOW + Duration::from_secs(1);
    let d = on_supervisor_absent_or_unresponsive::<ScriptedEndpoint>(&ep, &mut reconnect, lane, Some(voyage_id), t3);
    assert_eq!(
        d,
        ReconnectDecision::Terminal(rules::TerminalReason::HealthWindowExpired),
        "the fresh window started at t2 must still expire after its own full HEALTH_WINDOW"
    );
}

/// Serves a scripted sequence of already wire-encoded supervisor-lane
/// reply frames, one per `read()` call.
struct ScriptedReadyClient {
    replies: Mutex<VecDeque<Vec<u8>>>,
}
impl ScriptedReadyClient {
    fn new(replies: Vec<SupervisorReply>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().map(|r| wire::encode_supervisor_reply(&r).expect("encode")).collect()),
        }
    }
}
impl Client for ScriptedReadyClient {
    fn write_all(&self, _bytes: &[u8]) -> Result<(), crate::lane::transport::TransportError> {
        Ok(())
    }
    fn read(&self, buf: &mut [u8]) -> Result<usize, crate::lane::transport::TransportError> {
        let mut q = self.replies.lock().unwrap();
        let frame = q.pop_front().expect("scripted supervisor replies exhausted before the test finished driving it");
        buf[..frame.len()].copy_from_slice(&frame);
        Ok(frame.len())
    }
    fn cancel(&self) {}
}

/// Scripts [`Endpoint::connect_voyage_unchallenged`] with one queued
/// outcome per call — first `Unreachable`, then success.
struct ScriptedVoyageEndpoint {
    voyage_connects: Mutex<VecDeque<Result<(), crate::lane::transport::TransportError>>>,
}
impl Endpoint for ScriptedVoyageEndpoint {
    type Client = ScriptedReadyClient;
    type Process = TestProcess;

    fn connect_voyage_unchallenged(&self, _lane: &str, _voyage_id: &str) -> Result<Self::Client, crate::lane::transport::TransportError> {
        match self.voyage_connects.lock().unwrap().pop_front().expect("voyage-connect script exhausted") {
            Ok(()) => Ok(ScriptedReadyClient::new(Vec::new())),
            Err(e) => Err(e),
        }
    }
    fn connect_supervisor_unchallenged(&self, _lane: &str) -> Result<Self::Client, crate::lane::transport::TransportError> {
        unreachable!("converge_on_ready never reconnects the supervisor lane itself")
    }
    fn challenge(
        &self,
        _conn: &Self::Client,
        _exchange: &mut dyn crate::identity::exchange::IdentityExchange,
        _deadline: Instant,
    ) -> ChallengeOutcome<Self::Process> {
        unreachable!("converge_on_ready never challenges")
    }
    fn authenticate_server(&self, _conn: &Self::Client) -> PeerAuthOutcome {
        unreachable!("converge_on_ready never authenticates")
    }
}

/// A first attach that sees `Ready`, then fails mid-connect while the
/// row resets underneath it, still reaches the NEW voyage — only
/// because `EndedNoRespawn` is tolerated rather than terminal here.
#[test]
fn a_first_attach_that_fails_mid_connect_still_reaches_the_voyage_a_reset_mints_next() {
    let v1 = "11111111-1111-1111-1111-111111111111";
    let v2 = "22222222-2222-2222-2222-222222222222";

    let conn = ScriptedReadyClient::new(vec![
        SupervisorReply::StatusOk { pid: 1, created: 1, voyage: Some(v1.to_string()), leg: Some(1), phase: SupervisorPhase::Ready },
        SupervisorReply::StatusOk { pid: 1, created: 1, voyage: Some(v1.to_string()), leg: Some(1), phase: SupervisorPhase::EndedNoRespawn },
        SupervisorReply::StatusOk { pid: 2, created: 2, voyage: Some(v2.to_string()), leg: Some(1), phase: SupervisorPhase::Ready },
    ]);
    let ep = ScriptedVoyageEndpoint {
        voyage_connects: Mutex::new(VecDeque::from([
            Err(crate::lane::transport::TransportError::Unreachable(std::io::Error::new(ErrorKind::TimedOut, "leg not up yet"))),
            Ok(()),
        ])),
    };

    let (_msg_tx, cmd_rx) = mpsc::channel::<WorkerMsg>();
    let mut reconnect = ReconnectState::new();
    let mut held = Held { quit: None, resize: None, discarded: Arc::new(AtomicUsize::new(0)) };
    let mut quit = QuitDispatcher::new();
    let mut outstanding = OutstandingSlot::new();
    // Generous relative to the scripted backoff waits; the deadline's
    // own expiry isn't what this test exercises.
    let first_attach_deadline = Some(Instant::now() + Duration::from_secs(30));

    let outcome = converge_on_ready::<ScriptedVoyageEndpoint>(
        &ep,
        conn,
        FrameReader::new(),
        "sot-capsule-row-r4d",
        &cmd_rx,
        &mut reconnect,
        &mut held,
        &mut quit,
        &mut outstanding,
        first_attach_deadline,
        &AtomicBool::new(true),
        &|_e| {},
    );

    match outcome {
        ReadyOutcome::Ready { voyage_id, .. } => {
            assert_eq!(voyage_id, v2, "a first attach that failed mid-connect, across a reset, must still land on the NEW voyage");
        }
        ReadyOutcome::Terminal(msg) => panic!("expected Ready(v2), got Terminal({msg})"),
        ReadyOutcome::LaneDown => panic!("expected Ready(v2), got LaneDown"),
        ReadyOutcome::ShouldExit => panic!("expected Ready(v2), got ShouldExit"),
        ReadyOutcome::Shutdown => panic!("expected Ready(v2), got Shutdown"),
    }
}

/// Counts [`Endpoint::connect_voyage_unchallenged`] calls and fails every one.
struct CountingUnreachableEndpoint {
    voyage_dials: AtomicUsize,
}
impl Endpoint for CountingUnreachableEndpoint {
    type Client = ScriptedReadyClient;
    type Process = TestProcess;

    fn connect_voyage_unchallenged(&self, _lane: &str, _voyage_id: &str) -> Result<Self::Client, crate::lane::transport::TransportError> {
        self.voyage_dials.fetch_add(1, Ordering::AcqRel);
        Err(crate::lane::transport::TransportError::Unreachable(std::io::Error::new(ErrorKind::TimedOut, "leg not up yet")))
    }
    fn connect_supervisor_unchallenged(&self, _lane: &str) -> Result<Self::Client, crate::lane::transport::TransportError> {
        unreachable!("converge_on_ready never reconnects the supervisor lane itself")
    }
    fn challenge(
        &self,
        _conn: &Self::Client,
        _exchange: &mut dyn crate::identity::exchange::IdentityExchange,
        _deadline: Instant,
    ) -> ChallengeOutcome<Self::Process> {
        unreachable!("converge_on_ready never challenges")
    }
    fn authenticate_server(&self, _conn: &Self::Client) -> PeerAuthOutcome {
        unreachable!("converge_on_ready never authenticates")
    }
}

/// A voyage dial that answers `LinkDown`, with a flag standing in for the
/// host's link gate.
struct LinkDownEndpoint {
    link_up: Arc<AtomicBool>,
    voyage_dials: Arc<AtomicUsize>,
}
impl Endpoint for LinkDownEndpoint {
    type Client = ScriptedReadyClient;
    type Process = TestProcess;

    fn connect_voyage_unchallenged(&self, _lane: &str, _voyage_id: &str) -> Result<Self::Client, crate::lane::transport::TransportError> {
        self.voyage_dials.fetch_add(1, Ordering::AcqRel);
        Err(crate::lane::transport::TransportError::LinkDown)
    }
    fn connect_supervisor_unchallenged(&self, _lane: &str) -> Result<Self::Client, crate::lane::transport::TransportError> {
        unreachable!("converge_on_ready never reconnects the supervisor lane itself")
    }
    fn challenge(
        &self,
        _conn: &Self::Client,
        _exchange: &mut dyn crate::identity::exchange::IdentityExchange,
        _deadline: Instant,
    ) -> ChallengeOutcome<Self::Process> {
        unreachable!("converge_on_ready never challenges")
    }
    fn authenticate_server(&self, _conn: &Self::Client) -> PeerAuthOutcome {
        unreachable!("converge_on_ready never authenticates")
    }
    fn link_up(&self) -> bool {
        self.link_up.load(Ordering::Acquire)
    }
}

/// A worker whose dial answers `LinkDown` pauses after that one dial:
/// no further dial while the link is down, none while it is up but the
/// client is not viewed, one within a few ticks of both holding, and a
/// `Resize` read during the pause is kept for the next attach.
#[test]
fn a_link_down_dial_pauses_until_the_link_is_up_and_the_client_is_viewed() {
    let v = "11111111-1111-1111-1111-111111111111";
    let ready = SupervisorReply::StatusOk { pid: 1, created: 1, voyage: Some(v.to_string()), leg: Some(1), phase: SupervisorPhase::Ready };
    let conn = ScriptedReadyClient::new((0..60).map(|_| ready.clone()).collect());
    let link_up = Arc::new(AtomicBool::new(false));
    let voyage_dials = Arc::new(AtomicUsize::new(0));
    let ep = LinkDownEndpoint { link_up: Arc::clone(&link_up), voyage_dials: Arc::clone(&voyage_dials) };
    let viewed = Arc::new(AtomicBool::new(false));

    let (msg_tx, cmd_rx) = mpsc::channel::<WorkerMsg>();
    let worker_viewed = Arc::clone(&viewed);
    let worker = thread::spawn(move || {
        let mut reconnect = ReconnectState::new();
        let mut held = Held { quit: None, resize: None, discarded: Arc::new(AtomicUsize::new(0)) };
        let mut quit = QuitDispatcher::new();
        let mut outstanding = OutstandingSlot::new();
        let outcome = converge_on_ready::<LinkDownEndpoint>(
            &ep,
            conn,
            FrameReader::new(),
            "sot-capsule-row-r4d",
            &cmd_rx,
            &mut reconnect,
            &mut held,
            &mut quit,
            &mut outstanding,
            None,
            &worker_viewed,
            &|_e| {},
        );
        (matches!(outcome, ReadyOutcome::Shutdown), held.resize)
    });

    thread::sleep(Duration::from_millis(2000));
    assert_eq!(voyage_dials.load(Ordering::Acquire), 1, "link down: one dial, then paused");
    msg_tx.send(WorkerMsg::Resize(101, 41)).unwrap();

    link_up.store(true, Ordering::Release);
    thread::sleep(Duration::from_millis(2000));
    assert_eq!(voyage_dials.load(Ordering::Acquire), 1, "link up but not viewed: still paused");

    viewed.store(true, Ordering::Release);
    let resumed = Instant::now();
    while voyage_dials.load(Ordering::Acquire) < 2 && resumed.elapsed() < Duration::from_millis(300) {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(voyage_dials.load(Ordering::Acquire) >= 2, "link up and viewed: the next dial within 300 ms");

    msg_tx.send(WorkerMsg::Shutdown).unwrap();
    let (shut_down, resize) = worker.join().unwrap();
    assert!(shut_down);
    assert_eq!(resize, Some((101, 41)), "a Resize read while paused is the size of the next attach");
}

/// ADR 0043 decision 28: a failed voyage dial waits the doubling backoff
/// even before the row's first attach (over an ssh lane each dial is a
/// login), so 2 s of `Ready` + `Unreachable` holds at most 4 dials
/// (at 0, 0.25, 0.75 and 1.75 s), not one per 250 ms.
#[test]
fn a_failed_voyage_dial_backs_off_before_the_first_attach() {
    let v = "11111111-1111-1111-1111-111111111111";
    let ready = SupervisorReply::StatusOk { pid: 1, created: 1, voyage: Some(v.to_string()), leg: Some(1), phase: SupervisorPhase::Ready };
    let conn = ScriptedReadyClient::new((0..40).map(|_| ready.clone()).collect());
    let ep = CountingUnreachableEndpoint { voyage_dials: AtomicUsize::new(0) };

    let (msg_tx, cmd_rx) = mpsc::channel::<WorkerMsg>();
    let stopper = thread::spawn(move || {
        thread::sleep(Duration::from_millis(2000));
        let _ = msg_tx.send(WorkerMsg::Shutdown);
    });
    let mut reconnect = ReconnectState::new();
    let mut held = Held { quit: None, resize: None, discarded: Arc::new(AtomicUsize::new(0)) };
    let mut quit = QuitDispatcher::new();
    let mut outstanding = OutstandingSlot::new();
    let outcome = converge_on_ready::<CountingUnreachableEndpoint>(
        &ep,
        conn,
        FrameReader::new(),
        "sot-capsule-row-r4d",
        &cmd_rx,
        &mut reconnect,
        &mut held,
        &mut quit,
        &mut outstanding,
        None,
        &AtomicBool::new(true),
        &|_e| {},
    );
    stopper.join().unwrap();
    assert!(matches!(outcome, ReadyOutcome::Shutdown), "the test ends the loop with Shutdown");
    let dials = ep.voyage_dials.load(Ordering::Acquire);
    assert!(dials <= 4, "at most 4 voyage dials in 2 s, got {dials}");
}

/// Answers the `attach` request with one scripted `attach_refused`
/// frame and nothing else — a second `read` would be the test's own
/// bug, since the refusal ends the transfer.
struct RefusingClient {
    frame: Mutex<Option<Vec<u8>>>,
}
impl Client for RefusingClient {
    fn write_all(&self, _bytes: &[u8]) -> Result<(), crate::lane::transport::TransportError> {
        Ok(())
    }
    fn read(&self, buf: &mut [u8]) -> Result<usize, crate::lane::transport::TransportError> {
        let frame = self.frame.lock().unwrap().take().expect("the refusal frame must end the transfer on its own");
        buf[..frame.len()].copy_from_slice(&frame);
        Ok(frame.len())
    }
    fn cancel(&self) {}
}

/// Only names [`RefusingClient`] as `Endpoint::Client` — this test
/// hands the connection in directly and never dials anything.
struct RefusingEndpoint;
impl Endpoint for RefusingEndpoint {
    type Client = RefusingClient;
    type Process = TestProcess;

    fn connect_voyage_unchallenged(&self, _lane: &str, _voyage_id: &str) -> Result<Self::Client, crate::lane::transport::TransportError> {
        unreachable!("an_attach_refusal_carries_its_reason_to_the_caller never dials")
    }
    fn connect_supervisor_unchallenged(&self, _lane: &str) -> Result<Self::Client, crate::lane::transport::TransportError> {
        unreachable!("an_attach_refusal_carries_its_reason_to_the_caller never dials")
    }
    fn challenge(&self, _conn: &Self::Client, _exchange: &mut dyn crate::identity::exchange::IdentityExchange, _deadline: Instant) -> ChallengeOutcome<Self::Process> {
        unreachable!("an_attach_refusal_carries_its_reason_to_the_caller never challenges")
    }
    fn authenticate_server(&self, _conn: &Self::Client) -> PeerAuthOutcome {
        unreachable!("an_attach_refusal_carries_its_reason_to_the_caller never authenticates")
    }
}

/// The refusal reason must reach the caller, not collapse into one
/// anonymous `Protocol("attach_refused")`: `SubscriberCap` held by
/// orphaned watchers never clears on its own, so the episode that
/// retries it has to be able to SAY so (the row's status line), which
/// it cannot do from an error that dropped the reason on the floor.
#[test]
fn an_attach_refusal_carries_its_reason_to_the_caller() {
    for reason in [wire::AttachRefusedReason::SubscriberCap, wire::AttachRefusedReason::GroundTimeout] {
        let conn = RefusingClient {
            frame: Mutex::new(Some(
                wire::encode_attach_server(&AttachServer::AttachRefused { reason }).expect("fixed-shape body"),
            )),
        };
        let mut reader = FrameReader::new();
        let err = attach_and_collect_checkpoint::<RefusingEndpoint>(&conn, &mut reader, "controller-1")
            .expect_err("a refused attach must never yield a checkpoint");
        match err {
            LaneError::AttachRefused(got) => assert_eq!(got, reason, "the reason the capsule named must be the reason the caller sees"),
            other => panic!("the refusal reason must survive to the caller, not become {other}"),
        }
    }

    // The wording is the whole point of carrying the reason: only the
    // permanent refusal names a recovery, and it names the ACTION and
    // nothing more. It must NOT promise what the restart costs: a
    // supervisor launched after `user_scope_available()` was denied runs
    // in the daemon's own kill domain and dies with it, so an
    // unconditional "sessions survive" would be false exactly for the
    // user worst placed to know it. The reassurance and that exception
    // both belong to the troubleshooting page, which states both.
    let capped = attach_refused_text(wire::AttachRefusedReason::SubscriberCap);
    assert!(
        capped.contains("restart the backend daemon"),
        "the permanent refusal must name its recovery in the pane line, got {capped:?}"
    );
    assert!(
        !capped.contains("survive") && !capped.contains("keep running"),
        "the pane line must not promise what a restart costs — the degraded-scope supervisor breaks that promise, got {capped:?}"
    );
}

/// Drives the actual worker challenge and failed-Status paths, recording their contract calls.
struct AbandonedEndpoint { foreign: bool, drops: AtomicUsize }
struct DeadClient;
impl Client for DeadClient {
    fn write_all(&self, _: &[u8]) -> Result<(), crate::lane::transport::TransportError> {
        Err(crate::lane::transport::TransportError::LinkDown)
    }
    fn read(&self, _: &mut [u8]) -> Result<usize, crate::lane::transport::TransportError> {
        Err(crate::lane::transport::TransportError::LinkDown)
    }
    fn cancel(&self) {}
}
impl Endpoint for AbandonedEndpoint {
    type Client = DeadClient;
    type Process = TestProcess;
    fn connect_supervisor_unchallenged(&self, _: &str) -> Result<DeadClient, crate::lane::transport::TransportError> { Ok(DeadClient) }
    fn connect_voyage_unchallenged(&self, _: &str, _: &str) -> Result<DeadClient, crate::lane::transport::TransportError> { panic!("an abandoned attempt never dials voyage") }
    fn challenge(&self, _: &DeadClient, _: &mut dyn crate::identity::exchange::IdentityExchange, _: Instant) -> ChallengeOutcome<TestProcess> {
        if self.foreign { ChallengeOutcome::Foreign } else { ChallengeOutcome::Undetermined }
    }
    fn authenticate_server(&self, _: &DeadClient) -> PeerAuthOutcome { unreachable!() }
    fn drop_spare(&self) { self.drops.fetch_add(1, Ordering::SeqCst); }
}

#[test]
fn an_unproven_supervisor_hello_drops_the_spare() {
    let mut calls = Vec::new();
    for foreign in [true, false] {
        let ep = AbandonedEndpoint { foreign, drops: AtomicUsize::new(0) };
        assert!(connect_supervisor_lane(&ep, "owned-row").is_err());
        calls.push(ep.drops.load(Ordering::SeqCst));
    }
    assert_eq!(calls, [1, 1], "Foreign and Undetermined supervisor hellos must each abandon their spare");
}

#[test]
fn a_supervisor_link_down_drops_the_spare() {
    let ep = AbandonedEndpoint { foreign: false, drops: AtomicUsize::new(0) };
    let (_tx, rx) = mpsc::channel();
    let mut reconnect = ReconnectState::new();
    let mut held = Held { quit: None, resize: None, discarded: Arc::new(AtomicUsize::new(0)) };
    let outcome = converge_on_ready(&ep, DeadClient, FrameReader::new(), "owned-row", &rx,
        &mut reconnect, &mut held, &mut QuitDispatcher::new(), &mut OutstandingSlot::new(),
        None, &AtomicBool::new(true), &|_| {});
    assert!(matches!(outcome, ReadyOutcome::LaneDown));
    assert_eq!(ep.drops.load(Ordering::SeqCst), 1, "failed supervisor Status must abandon its spare");
}
