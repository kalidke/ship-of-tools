//! lease.rs — the window lease: which windows hold this computer's
//! sessions, what the last one's departure decides, and `held.json`, the
//! record that carries both across a daemon restart.
//!
//! A lease is a dedicated connection whose first frame is `fe.lease`; the
//! connection is the handle, so a lease's generation never goes on the
//! wire. This module is the pure core the connection's holder calls: it
//! does no IO but the record's read at construction and its write, and it
//! never looks a process up. The
//! peer's identity arrives already read from the OS at accept, and a
//! restart's plan is a function of the record and this boot alone.
//!
//! Every deadline is wall-clock unix milliseconds, because a handover's
//! deadline must survive a restart; one ticker calls [`Leases::tick`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use sot_log::challenge::{PeerAuthOutcome, PeerAuthenticated, ProcessIdentity};
use sot_protocol::ops::{
    lease as bounds, op, FeLeaseReq, FeLeaseRes, FeLeavingReq, FeLeavingRes, FeNoticeSeenReq, FeNoticeSeenRes,
    LeaseOutcome, LeaveIntent,
};
use sot_protocol::{Frame, Kind};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite};
use tokio::sync::{mpsc, watch};

/// `held.json`'s shape; a record with any other `v` plans Cleanup.
const RECORD_V: u32 = 1;

/// How long a relaunch handover, or a restart's recorded holders, keep
/// the rows waiting for a window. `SOT_TEST_HANDOVER_BOUND_MS` overrides it
/// for tests, read once per process; unset in every real deployment.
pub(crate) fn handover_bound() -> std::time::Duration {
    static OVERRIDE_MS: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    let override_ms = *OVERRIDE_MS.get_or_init(|| {
        std::env::var("SOT_TEST_HANDOVER_BOUND_MS")
            .ok()
            .and_then(|s| s.parse().ok())
    });
    override_ms
        .map(std::time::Duration::from_millis)
        .unwrap_or(bounds::HANDOVER_BOUND)
}

/// Wall-clock unix milliseconds: the one clock every lease deadline uses.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn handover_bound_ms() -> u64 {
    u64::try_from(handover_bound().as_millis()).unwrap_or(u64::MAX)
}

/// `held.json`, in the state root. It exists iff some field is non-empty
/// or true, so one window's `Keep` or `Close` never deletes another
/// window's holder entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HeldRecord {
    pub v: u32,
    /// The writing daemon's own `boot_identity()` (`""` on Windows when
    /// the BootId is unreadable). A
    /// record from another boot plans Cleanup at once.
    pub boot: String,
    /// The live leases' identities.
    pub holders: Vec<ProcessIdentity>,
    /// An in-process or pending handover's deadline, or a restart's
    /// pending deadline: written once, so a restart never extends it.
    pub handover_until_ms: Option<u64>,
    /// True from shutdown step 1 to step 5.
    pub closing: bool,
    /// Sessions an earlier close could not end, until a window acks them.
    pub not_ended: u32,
    /// Workspace ids that were ended but whose registration file could not
    /// be removed.
    pub forget: Vec<String>,
}

impl HeldRecord {
    fn is_empty(&self) -> bool {
        self.holders.is_empty()
            && self.handover_until_ms.is_none()
            && !self.closing
            && self.not_ended == 0
            && self.forget.is_empty()
    }
}

/// A start's decision from the record ([`startup_plan`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StartPlan {
    Resume,
    /// End every row without resuming it, write the record, then resume
    /// what remains.
    Cleanup,
    /// Resume, and wait for a window as a pending handover does: any
    /// granted lease clears it; at `until_ms` with no lease held it is a
    /// shutdown.
    Pending { until_ms: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartEvent {
    /// A granted lease cleared the pending start.
    Qualified,
}

/// What a departure decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    None,
    /// The last lease ended with `Close`; the lease mutex already refuses
    /// new leases and `gone` has fired.
    Shutdown,
    /// The last lease ended with `Handover`; its deadline is recorded.
    Handover,
}

/// What a tick decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tick {
    None,
    /// A handover or a pending start expired with no lease held; as
    /// [`Decision::Shutdown`].
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Open,
    /// Shutdown steps 1 to 5: the record says `closing`.
    Closing,
    /// After step 5. Leases are still refused until the process exits.
    Finished,
}

struct State {
    /// `None` when this daemon's `boot_identity()` failed: no grant then.
    own_boot: Option<String>,
    /// `None` with no state root: unfenced, no record.
    path: Option<PathBuf>,
    next_gen: u64,
    /// One entry per granted lease, removed by its generation only.
    held: Vec<(u64, ProcessIdentity)>,
    handover_until_ms: Option<u64>,
    /// A restart's pending start, until a grant or its deadline.
    pending_until_ms: Option<u64>,
    phase: Phase,
    not_ended: u32,
    forget: Vec<String>,
    /// The deciding departure was a `fe.leaving{close}`: its window waits
    /// for the shutdown's count (shutdown step 6).
    closer_waits: bool,
    /// The start's plan is Cleanup, for any cause, from construction until
    /// [`Leases::finish_cleanup`]. Meanwhile the record only moves toward
    /// close: until a shutdown begins nothing writes it (a grant still
    /// answers the window, a departure still decides), so a kill
    /// mid-Cleanup re-runs the Cleanup at the next start.
    startup_cleanup: bool,
}

impl State {
    fn record(&self) -> HeldRecord {
        let mut holders: Vec<ProcessIdentity> = Vec::new();
        for (_, who) in &self.held {
            if !holders.contains(who) {
                holders.push(who.clone());
            }
        }
        HeldRecord {
            v: RECORD_V,
            boot: self.own_boot.clone().unwrap_or_default(),
            holders,
            handover_until_ms: self.handover_until_ms.or(self.pending_until_ms),
            closing: self.phase == Phase::Closing,
            not_ended: self.not_ended,
            forget: self.forget.clone(),
        }
    }

    fn persist(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
        // A startup Cleanup's record only moves toward close.
        if self.startup_cleanup && self.phase == Phase::Open {
            return Ok(());
        }
        write_or_delete(path, &self.record())
    }

    /// For a write that follows an event that already happened (a
    /// departure, an expiry, shutdown step 1): refusing it would be a
    /// silent keep-alive, so a failure is logged and the event stands.
    fn persist_or_log(&self) {
        if let (Err(e), Some(path)) = (self.persist(), &self.path) {
            tracing::error!(path = %path.display(), "held record not written: {e}");
        }
    }

    /// Shutdown step 1: record `closing` and refuse every later lease. A
    /// handover or pending start ends here; the shutdown ends its rows.
    /// True iff this call began the shutdown.
    fn close(&mut self) -> bool {
        if self.phase != Phase::Open {
            return false;
        }
        self.phase = Phase::Closing;
        self.handover_until_ms = None;
        self.pending_until_ms = None;
        self.persist_or_log();
        true
    }
}

/// The daemon's leases, shared by every lease connection, the ticker and
/// the start.
#[derive(Clone)]
pub(crate) struct Leases {
    state: Arc<Mutex<State>>,
    /// `true` once a shutdown has begun; every waiter sees it, however
    /// late it subscribes.
    gone: watch::Sender<bool>,
    starts: mpsc::UnboundedSender<StartEvent>,
    /// Shutdown step 5's `not_ended`, for the waiting closer.
    report: watch::Sender<Option<u32>>,
    /// The waiting closer has been answered (step 6), or gave up.
    answered: watch::Sender<bool>,
}

impl Leases {
    pub(crate) fn new(
        own_boot: Option<String>,
        record: Option<PathBuf>,
        now_ms: u64,
    ) -> (Self, mpsc::UnboundedReceiver<StartEvent>) {
        let (starts, events) = mpsc::unbounded_channel();
        let read = record.as_deref().map(read_record);
        let startup_cleanup = read
            .as_ref()
            .is_some_and(|read| startup_plan(read, own_boot.as_deref().ok_or(()), now_ms) == StartPlan::Cleanup);
        // The count survives a restart until a window acks it.
        let not_ended = match &read {
            Some(Ok(Some(rec))) => rec.not_ended,
            _ => 0,
        };
        let state = State {
            own_boot,
            path: record,
            next_gen: 1,
            held: Vec::new(),
            handover_until_ms: None,
            pending_until_ms: None,
            phase: Phase::Open,
            not_ended,
            forget: Vec::new(),
            closer_waits: false,
            startup_cleanup,
        };
        let leases = Leases {
            state: Arc::new(Mutex::new(state)),
            gone: watch::Sender::new(false),
            starts,
            report: watch::Sender::new(None),
            answered: watch::Sender::new(false),
        };
        (leases, events)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The grant rule, in order: `Closing` once a shutdown has begun;
    /// `Undetermined` if the peer is, or this daemon's boot is unknown;
    /// `Granted` iff the token passed and the claim equals this boot and
    /// the peer's OS identity; otherwise `Foreign`. A grant carries its
    /// generation, clears an in-process handover and, before its
    /// deadline, a pending start. A grant whose record cannot be written
    /// is undone and refused as `Undetermined`, so a grant never outruns
    /// its record.
    pub(crate) fn grant(
        &self,
        req: &FeLeaseReq,
        peer: &PeerAuthOutcome,
        token_ok: bool,
        now_ms: u64,
    ) -> (LeaseOutcome, Option<u64>) {
        let mut st = self.lock();
        if st.phase != Phase::Open {
            return (LeaseOutcome::Closing, None);
        }
        let who = match claim(st.own_boot.as_deref(), req, peer, token_ok) {
            Ok(who) => who,
            Err((outcome, check)) => {
                tracing::warn!(
                    claimed_boot = %req.boot,
                    own_boot = ?st.own_boot,
                    claimed_pid = req.pid,
                    claimed_created = req.created,
                    ?peer,
                    "fe.lease refused ({outcome:?}): {check}"
                );
                return (outcome, None);
            }
        };
        let gen = st.next_gen;
        st.next_gen += 1;
        let handover = st.handover_until_ms.take();
        let qualified = st.pending_until_ms.is_some_and(|until| now_ms < until);
        let cleared = if qualified { st.pending_until_ms.take() } else { None };
        st.held.push((gen, who));
        if let Err(e) = st.persist() {
            st.held.pop();
            st.handover_until_ms = handover;
            if qualified {
                st.pending_until_ms = cleared;
            }
            let path = st.path.as_deref().unwrap_or(Path::new(""));
            tracing::error!(path = %path.display(), "fe.lease refused: held record not written: {e}");
            return (LeaseOutcome::Undetermined, None);
        }
        drop(st);
        if qualified {
            let _ = self.starts.send(StartEvent::Qualified);
        }
        (LeaseOutcome::Granted, Some(gen))
    }

    /// A lease's end: `intent` is its well-formed `fe.leaving`, `None` for
    /// an EOF or io error (which means `Close`). Only the last lease's end
    /// decides anything; an unknown or already-removed generation is a
    /// no-op.
    pub(crate) fn depart(&self, gen: u64, intent: Option<LeaveIntent>, now_ms: u64) -> Decision {
        let mut st = self.lock();
        let Some(at) = st.held.iter().position(|(g, _)| *g == gen) else {
            return Decision::None;
        };
        st.held.remove(at);
        self.decide(st, intent, now_ms)
    }

    /// A `fe.leaving{close}` after this connection's own `Keep`: the
    /// user's latest intent wins, so it is that window's `Close` departure
    /// at this moment. With no other lease held it is the shutdown.
    pub(crate) fn close_after_keep(&self, now_ms: u64) -> Decision {
        let st = self.lock();
        self.decide(st, Some(LeaveIntent::Close), now_ms)
    }

    /// What a departure decides once its lease is gone: nothing unless it
    /// was the last.
    fn decide(&self, mut st: MutexGuard<'_, State>, intent: Option<LeaveIntent>, now_ms: u64) -> Decision {
        if !st.held.is_empty() || st.phase != Phase::Open {
            st.persist_or_log();
            return Decision::None;
        }
        match intent.unwrap_or(LeaveIntent::Close) {
            LeaveIntent::Close => {
                st.close();
                st.closer_waits = intent.is_some();
                drop(st);
                self.gone.send_replace(true);
                Decision::Shutdown
            }
            LeaveIntent::Keep => {
                st.persist_or_log();
                Decision::None
            }
            LeaveIntent::Handover => {
                st.handover_until_ms = Some(now_ms.saturating_add(handover_bound_ms()));
                st.persist_or_log();
                Decision::Handover
            }
        }
    }

    /// The 1 s ticker's check: a passed pending start with a lease held is
    /// cleared and decides nothing; a passed handover or pending start with
    /// no lease held is a shutdown.
    pub(crate) fn tick(&self, now_ms: u64) -> Tick {
        let mut st = self.lock();
        if st.phase != Phase::Open {
            return Tick::None;
        }
        let passed = |until: Option<u64>| until.is_some_and(|until| until <= now_ms);
        if passed(st.pending_until_ms) && !st.held.is_empty() {
            st.pending_until_ms = None;
            st.persist_or_log();
        }
        if st.held.is_empty() && (passed(st.handover_until_ms) || passed(st.pending_until_ms)) {
            st.close();
            drop(st);
            self.gone.send_replace(true);
            return Tick::Shutdown;
        }
        Tick::None
    }

    /// Shutdown step 1, for a shutdown no departure or tick decided.
    /// Idempotent.
    pub(crate) fn begin_close(&self) {
        if self.lock().close() {
            self.gone.send_replace(true);
        }
    }

    /// Shutdown step 5, the shutdown's report written as the record: it
    /// ends `closing` (leases stay refused until the process exits).
    pub(crate) fn finish_shutdown(&self, not_ended: u32, forget: Vec<String>) -> std::io::Result<()> {
        let mut st = self.lock();
        if st.phase == Phase::Closing {
            st.phase = Phase::Finished;
        }
        st.not_ended = not_ended;
        st.forget = forget;
        let written = st.persist();
        drop(st);
        self.report.send_replace(Some(not_ended));
        written
    }

    /// The end of a startup Cleanup: its report written as the record, its
    /// count added to the one the start loaded. It never touches the
    /// phase, so a shutdown that began during the Cleanup stays `closing`
    /// until its own step 5.
    pub(crate) fn finish_cleanup(&self, not_ended: u32, forget: Vec<String>) -> std::io::Result<()> {
        let mut st = self.lock();
        st.startup_cleanup = false;
        st.not_ended = st.not_ended.saturating_add(not_ended);
        st.forget = forget;
        st.persist()
    }

    /// The count every grant carries until a window acks it.
    pub(crate) fn notice(&self) -> u32 {
        self.lock().not_ended
    }

    /// `fe.notice_seen{n}`: clears the count iff it equals `n`, so an ack
    /// of an older count never hides a newer one.
    pub(crate) fn notice_seen(&self, n: u32) -> std::io::Result<()> {
        let mut st = self.lock();
        if n != 0 && st.not_ended == n {
            st.not_ended = 0;
            return st.persist();
        }
        Ok(())
    }

    pub(crate) fn record(&self) -> HeldRecord {
        self.lock().record()
    }

    /// A start's Pending plan. Its deadline is written to the record as
    /// `handover_until_ms` before it returns, so a restart inside the
    /// window reads the same deadline and never extends it.
    pub(crate) fn install_pending(&self, until_ms: u64) -> std::io::Result<()> {
        let mut st = self.lock();
        st.pending_until_ms = Some(until_ms);
        st.persist()
    }

    /// Resolves once a shutdown has begun.
    pub(crate) async fn gone(&self) {
        let _ = self.gone.subscribe().wait_for(|gone| *gone).await;
    }

    /// The deciding departure's window waits for the shutdown's count.
    pub(crate) fn closer_waits(&self) -> bool {
        self.lock().closer_waits
    }

    /// Resolves with shutdown step 5's `not_ended`.
    async fn report(&self) -> u32 {
        let mut rx = self.report.subscribe();
        let n = match rx.wait_for(Option::is_some).await {
            Ok(n) => n.unwrap_or_default(),
            Err(_) => 0,
        };
        n
    }

    /// Resolves once the waiting closer has been answered or gave up.
    pub(crate) async fn answered(&self) {
        let _ = self.answered.subscribe().wait_for(|done| *done).await;
    }
}

/// The 1 s ticker: every lease deadline is checked here, never by a
/// per-handover timer. It stops once a shutdown has begun.
pub(crate) async fn ticker(leases: Arc<Leases>) {
    let mut every = tokio::time::interval(std::time::Duration::from_secs(1));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = every.tick() => {}
            () = leases.gone() => return,
        }
        leases.tick(now_ms());
    }
}

/// The connecting process, read from the OS at accept, before the stream
/// is split (1.2). Only Linux checks the uid, because it comes with the
/// pid there; on macOS the socket directory and on Windows the pipe ACL
/// already bind the peer to this user. Any OS-call failure is
/// `Undetermined`.
pub(crate) fn accepted_peer(stream: &interprocess::local_socket::tokio::Stream) -> PeerAuthOutcome {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let interprocess::local_socket::tokio::Stream::UdSocket(s) = stream else {
            return PeerAuthOutcome::Undetermined;
        };
        match sot_log::challenge_macos::peer_pid_created(s.inner().as_raw_fd()) {
            Ok((pid, created)) => PeerAuthOutcome::Authenticated(PeerAuthenticated { pid, created }),
            Err(_) => PeerAuthOutcome::Undetermined,
        }
    }
    #[cfg(any(target_os = "linux", windows))]
    {
        use interprocess::local_socket::traits::StreamCommon as _;
        let Ok(creds) = stream.peer_creds() else {
            return PeerAuthOutcome::Undetermined;
        };
        #[cfg(target_os = "linux")]
        match creds.euid() {
            // SAFETY: geteuid has no preconditions and cannot fail.
            Some(uid) if uid == unsafe { libc::geteuid() } => {}
            Some(_) => return PeerAuthOutcome::Foreign,
            None => return PeerAuthOutcome::Undetermined,
        }
        let Some(pid) = creds.pid().and_then(|pid| u32::try_from(pid).ok()) else {
            return PeerAuthOutcome::Undetermined;
        };
        match sot_log::challenge::process_created(pid) {
            Ok(created) => PeerAuthOutcome::Authenticated(PeerAuthenticated { pid, created }),
            Err(_) => PeerAuthOutcome::Undetermined,
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = stream;
        PeerAuthOutcome::Undetermined
    }
}

/// A lease line's cap; an over-cap line is discarded up to its newline
/// and answered with an error.
const LINE_CAP: usize = 64 * 1024;

/// One line of a lease connection.
enum Line {
    Complete(Vec<u8>),
    OverCap,
    /// EOF, including a partial line followed by EOF.
    End,
}

async fn read_line<R: AsyncBufRead + Unpin>(rx: &mut R) -> std::io::Result<Line> {
    let (mut line, mut over) = (Vec::new(), false);
    loop {
        let buf = rx.fill_buf().await?;
        if buf.is_empty() {
            return Ok(Line::End);
        }
        let (take, done) = match buf.iter().position(|b| *b == b'\n') {
            Some(at) => (at + 1, true),
            None => (buf.len(), false),
        };
        if !over {
            line.extend_from_slice(&buf[..take]);
            if line.len() > LINE_CAP {
                over = true;
                line = Vec::new();
            }
        }
        rx.consume(take);
        if done {
            return Ok(if over { Line::OverCap } else { Line::Complete(line) });
        }
    }
}

/// One lease connection, from its `fe.lease` to its end (1.2, 1.3). The
/// grant's answer carries the state root's hash and the unacked count. A
/// granted lease departs at its well-formed `fe.leaving`, at EOF or at an
/// io error (both `Close`); any other line is answered with an error and
/// the lease continues, and the daemon never closes it. A
/// `fe.leaving{close}` that decided the shutdown is answered after it,
/// with the count it could not end.
pub(crate) async fn hold<R, W>(
    mut rx: R,
    mut tx: W,
    first: Frame,
    peer: PeerAuthOutcome,
    expected_token: Option<&str>,
    leases: &Leases,
    state_root: Option<&Path>,
) -> anyhow::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let Ok(req) = serde_json::from_value::<FeLeaseReq>(first.payload.clone()) else {
        return crate::proxy::reject(&mut tx, first.id, op::FE_LEASE, "bad_request", "malformed fe.lease").await;
    };
    let token_ok = expected_token.is_none_or(|expected| {
        let presented = req.token.clone().unwrap_or_default();
        crate::handlers::constant_time_eq(presented.as_bytes(), expected.as_bytes())
    });
    let (outcome, gen) = leases.grant(&req, &peer, token_ok, now_ms());
    let granted = gen.is_some();
    let res = FeLeaseRes {
        outcome,
        state_root: state_root.filter(|_| granted).map(sot_log::state_dir::state_dir_hash),
        not_ended: if granted { leases.notice() } else { 0 },
    };
    reply(&mut tx, first.id, op::FE_LEASE, &res).await?;
    let Some(gen) = gen else { return Ok(()) };

    let mut held = Some(gen);
    // This lease departed with `Keep`: a later `fe.leaving{close}` on it
    // still applies.
    let mut kept = false;
    loop {
        let bytes = match read_line(&mut rx).await {
            Ok(Line::Complete(bytes)) => bytes,
            Ok(Line::OverCap) => {
                crate::proxy::reject(&mut tx, 0, op::FE_LEASE, "bad_request", "line over the lease cap").await?;
                continue;
            }
            Ok(Line::End) => break,
            Err(e) => {
                tracing::info!(error = %e, "lease connection read failed: it departs as a close");
                break;
            }
        };
        let frame = match serde_json::from_slice::<Frame>(&bytes) {
            Ok(frame) if frame.kind == Kind::Req => frame,
            _ => {
                crate::proxy::reject(&mut tx, 0, op::FE_LEASE, "bad_request", "not a request").await?;
                continue;
            }
        };
        match frame.op.as_str() {
            op::FE_LEAVING => {
                let Ok(leaving) = serde_json::from_value::<FeLeavingReq>(frame.payload.clone()) else {
                    crate::proxy::reject(&mut tx, frame.id, op::FE_LEAVING, "bad_request", "malformed fe.leaving").await?;
                    continue;
                };
                let decision = match held.take() {
                    Some(gen) => {
                        kept = leaving.intent == LeaveIntent::Keep;
                        leases.depart(gen, Some(leaving.intent), now_ms())
                    }
                    None if kept && leaving.intent == LeaveIntent::Close => {
                        kept = false;
                        leases.close_after_keep(now_ms())
                    }
                    None => Decision::None,
                };
                if decision == Decision::Shutdown && leaving.intent == LeaveIntent::Close {
                    answer_close(&mut rx, &mut tx, frame.id, leases).await;
                    return Ok(());
                }
                reply(&mut tx, frame.id, op::FE_LEAVING, &FeLeavingRes { not_ended: 0 }).await?;
            }
            op::FE_NOTICE_SEEN => {
                let Ok(seen) = serde_json::from_value::<FeNoticeSeenReq>(frame.payload.clone()) else {
                    crate::proxy::reject(&mut tx, frame.id, op::FE_NOTICE_SEEN, "bad_request", "malformed fe.notice_seen").await?;
                    continue;
                };
                if let Err(e) = leases.notice_seen(seen.not_ended) {
                    tracing::error!("held record not written after fe.notice_seen: {e}");
                }
                reply(&mut tx, frame.id, op::FE_NOTICE_SEEN, &FeNoticeSeenRes {}).await?;
            }
            other => {
                crate::proxy::reject(&mut tx, frame.id, other, "bad_request", "not a lease request").await?;
            }
        }
    }
    if let Some(gen) = held {
        leases.depart(gen, None, now_ms());
    }
    Ok(())
}

/// Shutdown step 6 for the window whose `fe.leaving{close}` decided it:
/// the count after the shutdown, then up to `NOTICE_ACK_WAIT` for its
/// `fe.notice_seen` when the count is above 0. Answered whatever happens,
/// so the shutdown never waits on a broken window.
async fn answer_close<R, W>(rx: &mut R, tx: &mut W, id: u64, leases: &Leases)
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let not_ended = leases.report().await;
    if reply(tx, id, op::FE_LEAVING, &FeLeavingRes { not_ended }).await.is_ok() && not_ended > 0 {
        let seen = async {
            while let Ok(Line::Complete(bytes)) = read_line(rx).await {
                let Ok(frame) = serde_json::from_slice::<Frame>(&bytes) else { continue };
                if frame.op != op::FE_NOTICE_SEEN {
                    continue;
                }
                if let Ok(seen) = serde_json::from_value::<FeNoticeSeenReq>(frame.payload) {
                    if let Err(e) = leases.notice_seen(seen.not_ended) {
                        tracing::error!("held record not written after fe.notice_seen: {e}");
                    }
                    let _ = reply(tx, frame.id, op::FE_NOTICE_SEEN, &FeNoticeSeenRes {}).await;
                    return;
                }
            }
        };
        let _ = tokio::time::timeout(bounds::NOTICE_ACK_WAIT, seen).await;
    }
    leases.answered.send_replace(true);
}

async fn reply<W: AsyncWrite + Unpin>(tx: &mut W, id: u64, op: &str, res: &impl Serialize) -> anyhow::Result<()> {
    let frame = Frame::res(id, op, serde_json::to_value(res)?);
    crate::server::write_frame_within(tx, &frame, None, bounds::LEASE_REPLY_WAIT).await
}

/// `Ok(None)` when there is no record; `Err` when it cannot be read,
/// does not parse, or has another `v`.
pub(crate) fn read_record(path: &Path) -> Result<Option<HeldRecord>, String> {
    let err = |e: String| format!("{}: {e}", path.display());
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(err(e.to_string())),
    };
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| err(e.to_string()))?;
    match value.get("v").and_then(serde_json::Value::as_u64) {
        Some(v) if v == u64::from(RECORD_V) => {}
        v => return Err(err(format!("unknown record version {v:?}"))),
    }
    serde_json::from_value(value).map(Some).map_err(|e| err(e.to_string()))
}

/// Writes `rec` (tmp file, fsync, rename, directory sync), or deletes the
/// file when the record is empty.
pub(crate) fn write_or_delete(path: &Path, rec: &HeldRecord) -> std::io::Result<()> {
    if rec.is_empty() {
        return crate::durable::remove(path);
    }
    let bytes = serde_json::to_vec(rec).map_err(std::io::Error::other)?;
    crate::durable::write(path, &bytes)
}

/// The grant rule's claim checks, in order: the claimant's identity, or
/// the refusal and the check that failed. A bad token is `Foreign`: the
/// peer is not proven ours, and only a broken install hits it.
fn claim(
    own_boot: Option<&str>,
    req: &FeLeaseReq,
    peer: &PeerAuthOutcome,
    token_ok: bool,
) -> Result<ProcessIdentity, (LeaseOutcome, &'static str)> {
    let Some(own_boot) = own_boot else {
        return Err((LeaseOutcome::Undetermined, "own boot unknown"));
    };
    let peer = match peer {
        PeerAuthOutcome::Undetermined => return Err((LeaseOutcome::Undetermined, "peer undetermined")),
        PeerAuthOutcome::Foreign => return Err((LeaseOutcome::Foreign, "peer foreign")),
        PeerAuthOutcome::Authenticated(peer) => peer,
    };
    if !token_ok {
        return Err((LeaseOutcome::Foreign, "bad token"));
    }
    if !boots_match(&req.boot, own_boot, cfg!(windows)) {
        return Err((LeaseOutcome::Foreign, "boot mismatch"));
    }
    if req.pid != peer.pid {
        return Err((LeaseOutcome::Foreign, "pid mismatch"));
    }
    if req.created != peer.created {
        return Err((LeaseOutcome::Foreign, "created mismatch"));
    }
    Ok(ProcessIdentity { boot: req.boot.clone(), pid: req.pid, created: req.created })
}

/// Whether a claimed boot matches this daemon's. Strict is plain equality.
/// Lenient (Windows) also accepts an empty side: the peer proof there is
/// the pipe client's pid plus its absolute creation time, which a
/// forwarder cannot match, so a failed BootId read must not leave the
/// window unleased.
fn boots_match(req_boot: &str, own_boot: &str, lenient: bool) -> bool {
    req_boot == own_boot || (lenient && (req_boot.is_empty() || own_boot.is_empty()))
}

/// A start's plan, from the record and this boot alone: it takes no
/// process liveness, so a reused pid can never resume or hold a row.
pub(crate) fn startup_plan(
    read: &Result<Option<HeldRecord>, String>,
    own_boot: Result<&str, ()>,
    now_ms: u64,
) -> StartPlan {
    let rec = match read {
        Ok(None) => return StartPlan::Resume,
        Err(_) => return StartPlan::Cleanup,
        Ok(Some(rec)) => rec,
    };
    // An empty boot on either side is unknown (Windows with an unreadable
    // BootId): only two non-empty, different boots plan Cleanup.
    let other_boot = match own_boot {
        Err(()) => true,
        Ok(own) => !own.is_empty() && !rec.boot.is_empty() && own != rec.boot,
    };
    if rec.closing || other_boot {
        return StartPlan::Cleanup;
    }
    if let Some(until_ms) = rec.handover_until_ms {
        if until_ms <= now_ms {
            return StartPlan::Cleanup;
        }
        return StartPlan::Pending { until_ms };
    }
    if !rec.holders.is_empty() {
        return StartPlan::Pending { until_ms: now_ms.saturating_add(handover_bound_ms()) };
    }
    StartPlan::Resume
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const BOOT: &str = "boot-a";
    const T0: u64 = 1_000_000;

    fn id(pid: u32) -> ProcessIdentity {
        ProcessIdentity { boot: BOOT.into(), pid, created: 7000 + u64::from(pid) }
    }

    fn req(who: &ProcessIdentity) -> FeLeaseReq {
        FeLeaseReq { boot: who.boot.clone(), pid: who.pid, created: who.created, token: None }
    }

    fn peer(who: &ProcessIdentity) -> PeerAuthOutcome {
        PeerAuthOutcome::Authenticated(PeerAuthenticated { pid: who.pid, created: who.created })
    }

    fn empty() -> HeldRecord {
        HeldRecord {
            v: 1,
            boot: BOOT.into(),
            holders: vec![],
            handover_until_ms: None,
            closing: false,
            not_ended: 0,
            forget: vec![],
        }
    }

    struct Fixture {
        leases: Leases,
        starts: mpsc::UnboundedReceiver<StartEvent>,
        path: PathBuf,
        _dir: tempfile::TempDir,
    }

    fn fixture(own_boot: Option<&str>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(bounds::HELD_RECORD_FILE);
        let (leases, starts) = Leases::new(own_boot.map(String::from), Some(path.clone()), T0);
        Fixture { leases, starts, path, _dir: dir }
    }

    impl Fixture {
        fn grant(&self, who: &ProcessIdentity) -> u64 {
            let (outcome, gen) = self.leases.grant(&req(who), &peer(who), true, T0);
            assert_eq!(outcome, LeaseOutcome::Granted, "{who:?}");
            gen.expect("a grant carries its generation")
        }

        fn on_disk(&self) -> Option<HeldRecord> {
            read_record(&self.path).expect("the record parses")
        }

        /// The start's plan at `now`, which must be Pending, installed;
        /// its deadline.
        fn install_planned(&self, now: u64) -> u64 {
            let StartPlan::Pending { until_ms } = startup_plan(&read_record(&self.path), Ok(BOOT), now) else {
                panic!("the record plans Pending");
            };
            self.leases.install_pending(until_ms).unwrap();
            until_ms
        }
    }

    /// A start at `T0` over `text`, the bytes the last daemon left: the
    /// record on disk first, then this daemon built on it.
    fn restart_raw(text: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(bounds::HELD_RECORD_FILE);
        std::fs::write(&path, text).unwrap();
        let (leases, starts) = Leases::new(Some(BOOT.into()), Some(path.clone()), T0);
        Fixture { leases, starts, path, _dir: dir }
    }

    fn restart(rec: &HeldRecord) -> Fixture {
        restart_raw(&serde_json::to_string(rec).unwrap())
    }

    #[test]
    fn not_ended_survives_restart_until_acked() {
        let f = restart(&HeldRecord { not_ended: 3, ..empty() });
        f.grant(&id(1));
        assert_eq!(f.leases.notice(), 3, "the first grant after a restart lost the record's not_ended");
        assert_eq!(f.on_disk().map(|r| r.not_ended), Some(3), "the first grant's record lost the not_ended count");
        f.leases.notice_seen(3).unwrap();
        assert_eq!(f.leases.notice(), 0, "an ack of the loaded count did not clear it");
        assert_eq!(f.on_disk().map(|r| r.not_ended), Some(0));

        let f = restart(&HeldRecord { closing: true, not_ended: 3, ..empty() });
        f.leases.finish_cleanup(2, Vec::new()).unwrap();
        assert_eq!(f.leases.notice(), 5, "a startup Cleanup's count replaced the loaded one instead of adding to it");
        assert_eq!(f.on_disk().map(|r| r.not_ended), Some(5));
    }

    #[test]
    fn lease_during_startup_cleanup_keeps_the_record() {
        let closing = HeldRecord { closing: true, ..empty() };
        let other_boot = HeldRecord { boot: "boot-b".into(), holders: vec![id(9)], ..empty() };
        for (what, rec) in [("closing", closing), ("another boot", other_boot)] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(bounds::HELD_RECORD_FILE);
            write_or_delete(&path, &rec).unwrap();
            let (leases, _starts) = Leases::new(Some(BOOT.into()), Some(path.clone()), T0);
            let who = id(1);
            let (outcome, _) = leases.grant(&req(&who), &peer(&who), true, T0);
            assert_eq!(outcome, LeaseOutcome::Granted, "{what}: a grant still answers the window");
            assert_eq!(
                read_record(&path).unwrap(),
                Some(rec.clone()),
                "{what}: a grant during the startup Cleanup rewrote the record"
            );
            leases.finish_cleanup(0, Vec::new()).unwrap();
            let after = read_record(&path).unwrap().expect("the holder is recorded once the Cleanup finishes");
            assert_eq!(after.holders, vec![who], "{what}");
            assert!(!after.closing, "{what}");
        }
    }

    #[tokio::test]
    async fn pending_expiry_shuts_down() {
        let f = fixture(Some(BOOT));
        f.leases.install_pending(T0 + 10).unwrap();
        assert_eq!(f.leases.tick(T0 + 20), Tick::Shutdown);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), f.leases.gone()).await.is_ok(),
            "a pending start that expired with no qualifying lease did not shut down"
        );
        assert!(f.on_disk().expect("the record").closing, "the shutdown did not record closing");
    }

    #[test]
    fn lease_claim_table() {
        use LeaseOutcome::*;
        let me = id(4242);
        let edited = |edit: fn(&mut FeLeaseReq)| {
            let mut r = req(&me);
            edit(&mut r);
            r
        };
        let cases: Vec<(&str, FeLeaseReq, PeerAuthOutcome, bool, LeaseOutcome, Option<&str>)> = vec![
            ("equal identity", req(&me), peer(&me), true, Granted, None),
            ("different pid", edited(|r| r.pid += 1), peer(&me), true, Foreign, Some("pid mismatch")),
            ("different created", edited(|r| r.created += 1), peer(&me), true, Foreign, Some("created mismatch")),
            ("different boot, same pid and created", edited(|r| r.boot = "boot-b".into()), peer(&me), true, Foreign, Some("boot mismatch")),
            ("foreign peer", req(&me), PeerAuthOutcome::Foreign, true, Foreign, Some("peer foreign")),
            ("undetermined peer", req(&me), PeerAuthOutcome::Undetermined, true, Undetermined, Some("peer undetermined")),
            ("undetermined peer, bad token", req(&me), PeerAuthOutcome::Undetermined, false, Undetermined, Some("peer undetermined")),
            ("bad token", req(&me), peer(&me), false, Foreign, Some("bad token")),
        ];
        let f = fixture(Some(BOOT));
        for (name, r, p, token_ok, want, why) in &cases {
            let (got, gen) = f.leases.grant(r, p, *token_ok, T0);
            assert_eq!(got, *want, "{name}");
            assert_eq!(gen.is_some(), *want == Granted, "{name}: a generation iff granted");
            assert_eq!(claim(Some(BOOT), r, p, *token_ok).err(), why.map(|why| (*want, why)), "{name}: the check named");
        }
        let a = f.grant(&me);
        let b = f.grant(&me);
        assert_ne!(a, b, "two leases from one identity are two entries");
        assert_eq!(f.leases.record().holders, vec![me.clone()]);

        let f = fixture(None);
        for p in [peer(&me), PeerAuthOutcome::Foreign] {
            assert_eq!(f.leases.grant(&req(&me), &p, true, T0).0, Undetermined, "missing daemon boot, {p:?}");
            assert_eq!(claim(None, &req(&me), &p, true).err(), Some((Undetermined, "own boot unknown")), "{p:?}");
        }
        assert_eq!(f.on_disk(), None, "a refusal records nothing");

        let f = fixture(Some(BOOT));
        f.leases.begin_close();
        for p in [peer(&me), PeerAuthOutcome::Undetermined] {
            assert_eq!(f.leases.grant(&req(&me), &p, true, T0), (Closing, None), "closing, {p:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn grant_refused_when_record_cannot_be_written() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |f: &Fixture, m: u32| {
            std::fs::set_permissions(f.path.parent().unwrap(), std::fs::Permissions::from_mode(m)).unwrap()
        };
        let f = fixture(Some(BOOT));
        let a = f.grant(&id(1));
        mode(&f, 0o500);
        assert_eq!(f.leases.grant(&req(&id(2)), &peer(&id(2)), true, T0), (LeaseOutcome::Undetermined, None), "refused, never Granted");
        assert_eq!(f.leases.record().holders, vec![id(1)], "the refused grant's entry is undone");
        mode(&f, 0o700);
        assert_ne!(f.grant(&id(2)), a, "a later grant has a new generation");

        let mut f = fixture(Some(BOOT));
        f.leases.install_pending(T0 + handover_bound_ms()).unwrap();
        let before = f.leases.record();
        mode(&f, 0o500);
        assert_eq!(f.leases.grant(&req(&id(1)), &peer(&id(1)), true, T0), (LeaseOutcome::Undetermined, None));
        assert!(f.starts.try_recv().is_err(), "a refused grant completes nothing");
        assert_eq!(f.leases.record(), before, "every in-memory change is undone");
        mode(&f, 0o700);
        f.grant(&id(1));
        assert_eq!(f.starts.try_recv(), Ok(StartEvent::Qualified), "the pending start survived the refusal");
    }

    #[tokio::test]
    async fn departure_table() {
        use LeaveIntent::*;
        let intents = [None, Some(Close), Some(Keep), Some(Handover)];
        for intent in intents {
            let f = fixture(Some(BOOT));
            let a = f.grant(&id(1));
            f.grant(&id(2));
            assert_eq!(f.leases.depart(a, intent, T0), Decision::None, "not the last, {intent:?}");
            assert_eq!(f.on_disk().unwrap().holders, vec![id(2)], "{intent:?}");
            assert_eq!(f.leases.tick(T0 + 10 * handover_bound_ms()), Tick::None, "{intent:?}");
        }
        let wants = [Decision::Shutdown, Decision::Shutdown, Decision::None, Decision::Handover];
        for (intent, want) in intents.into_iter().zip(wants) {
            let f = fixture(Some(BOOT));
            let a = f.grant(&id(1));
            assert_eq!(f.leases.depart(a, intent, T0), want, "the last, {intent:?}");
            let gone = tokio::time::timeout(Duration::from_millis(50), f.leases.gone()).await.is_ok();
            assert_eq!(gone, want == Decision::Shutdown, "gone fires iff shutdown, {intent:?}");
            let rec = f.on_disk();
            match want {
                Decision::Shutdown => {
                    assert_eq!(rec, Some(HeldRecord { closing: true, ..empty() }), "{intent:?}");
                    assert_eq!(f.leases.grant(&req(&id(3)), &peer(&id(3)), true, T0).0, LeaseOutcome::Closing);
                }
                Decision::None => assert_eq!(rec, Option::None, "Keep leaves nothing to record"),
                Decision::Handover => {
                    assert_eq!(rec, Some(HeldRecord { handover_until_ms: Some(T0 + handover_bound_ms()), ..empty() }))
                }
            }
        }
    }

    #[test]
    fn lease_generation_fences_removal() {
        let f = fixture(Some(BOOT));
        let w = id(1);
        let first = f.grant(&w);
        let second = f.grant(&w);
        assert_eq!(f.leases.depart(first, None, T0), Decision::None, "the replaced lease is not the last");
        assert_eq!(f.on_disk().unwrap().holders, vec![w.clone()]);
        assert_eq!(f.leases.depart(first, Some(LeaveIntent::Close), T0), Decision::None, "a second end is a no-op");
        assert_eq!(f.leases.depart(9999, None, T0), Decision::None, "an unknown generation is a no-op");
        assert_eq!(f.leases.record().holders, vec![w.clone()]);
        assert_eq!(f.leases.depart(second, Some(LeaveIntent::Keep), T0), Decision::None);
        assert_eq!(f.on_disk(), None);
        assert_eq!(f.leases.depart(second, None, T0), Decision::None, "a late end after the last never shuts down");
        assert_eq!(f.leases.depart(first, None, T0), Decision::None);
        assert!(!f.leases.record().closing);
        f.grant(&w);
    }

    #[test]
    fn record_survives_other_windows_keep() {
        for intent in [LeaveIntent::Keep, LeaveIntent::Close, LeaveIntent::Handover] {
            let f = fixture(Some(BOOT));
            let a = f.grant(&id(1));
            f.grant(&id(2));
            f.leases.depart(a, Some(intent), T0);
            let rec = f.on_disk().expect("the other window's holder entry stays");
            assert_eq!(rec, HeldRecord { holders: vec![id(2)], ..empty() }, "{intent:?}");
            assert_eq!(rec, f.leases.record());
        }
    }

    #[test]
    fn handover_table() {
        let until = T0 + handover_bound_ms();
        let f = fixture(Some(BOOT));
        let a = f.grant(&id(1));
        assert_eq!(f.leases.depart(a, Some(LeaveIntent::Handover), T0), Decision::Handover);
        assert_eq!(f.on_disk().unwrap().handover_until_ms, Some(until), "persisted");
        assert_eq!(f.leases.tick(until - 1), Tick::None);
        assert_eq!(f.leases.tick(until), Tick::Shutdown, "expiry with no lease held");
        assert_eq!(f.on_disk(), Some(HeldRecord { closing: true, ..empty() }));
        assert_eq!(f.leases.tick(until + 1), Tick::None, "decided once");

        let f = fixture(Some(BOOT));
        let a = f.grant(&id(1));
        f.leases.depart(a, Some(LeaveIntent::Handover), T0);
        let b = f.grant(&id(2));
        assert_eq!(f.on_disk().unwrap().handover_until_ms, None, "any grant clears it");
        assert_eq!(f.leases.tick(until + 1), Tick::None);
        assert_eq!(f.leases.depart(b, Some(LeaveIntent::Keep), T0 + 1), Decision::None);
        assert_eq!(f.on_disk(), None);
        assert_eq!(f.leases.tick(u64::MAX), Tick::None, "Keep never expires");
        f.grant(&id(3));
    }

    #[test]
    fn pending_start_table() {
        let until = T0 + handover_bound_ms();
        let mut f = fixture(Some(BOOT));
        f.leases.install_pending(until).unwrap();
        assert_eq!(f.on_disk(), Some(HeldRecord { handover_until_ms: Some(until), ..empty() }));
        f.grant(&id(9));
        assert_eq!(f.starts.try_recv(), Ok(StartEvent::Qualified), "any grant completes a pending start");
        assert_eq!(f.on_disk(), Some(HeldRecord { holders: vec![id(9)], ..empty() }));
        assert_eq!(f.leases.tick(until), Tick::None);

        let mut f = fixture(Some(BOOT));
        f.leases.install_pending(until).unwrap();
        assert_eq!(f.leases.tick(until - 1), Tick::None);
        assert_eq!(f.leases.tick(until), Tick::Shutdown, "expiry with no lease held");
        assert_eq!(f.leases.tick(until + 1), Tick::None, "decided once");
        assert_eq!(f.on_disk(), Some(HeldRecord { closing: true, ..empty() }));
        assert_eq!(f.leases.grant(&req(&id(1)), &peer(&id(1)), true, until).0, LeaseOutcome::Closing);
        assert!(f.starts.try_recv().is_err(), "a lease after expiry does not complete it");
    }

    #[test]
    fn cleanup_finish_never_ends_a_shutdown() {
        let f = restart(&HeldRecord { holders: vec![id(1)], handover_until_ms: Some(T0), ..empty() });
        f.leases.begin_close();
        f.leases.finish_cleanup(0, vec![]).unwrap();
        assert_eq!(f.on_disk(), Some(HeldRecord { closing: true, ..empty() }), "closing until the shutdown's own step 5");
        assert_eq!(f.leases.grant(&req(&id(2)), &peer(&id(2)), true, T0).0, LeaseOutcome::Closing);
    }

    #[tokio::test]
    async fn gone_wakes_every_waiter() {
        let f = fixture(Some(BOOT));
        let waiters: Vec<_> = (0..2)
            .map(|_| {
                let leases = f.leases.clone();
                tokio::spawn(async move { leases.gone().await })
            })
            .collect();
        tokio::task::yield_now().await;
        f.leases.begin_close();
        for w in waiters {
            tokio::time::timeout(Duration::from_secs(1), w).await.expect("every waiter sees the shutdown").unwrap();
        }
        tokio::time::timeout(Duration::from_secs(1), f.leases.gone()).await.expect("a late waiter too");
    }

    #[test]
    fn notice_clears_only_on_matching_ack() {
        let f = fixture(Some(BOOT));
        f.leases.finish_cleanup(2, vec![]).unwrap();
        assert_eq!(f.leases.notice(), 2);
        assert_eq!(f.on_disk(), Some(HeldRecord { not_ended: 2, ..empty() }));
        f.grant(&id(1));
        assert_eq!(f.leases.notice(), 2, "a grant does not ack it");
        for stale in [0, 1, 3] {
            f.leases.notice_seen(stale).unwrap();
            assert_eq!(f.leases.notice(), 2, "an ack of {stale}");
        }
        f.leases.notice_seen(2).unwrap();
        assert_eq!(f.leases.notice(), 0);
        assert_eq!(f.on_disk(), Some(HeldRecord { holders: vec![id(1)], ..empty() }));
    }

    #[test]
    fn boots_match_table() {
        for (req, own, strict, lenient) in [
            ("80", "80", true, true),
            ("80", "81", false, false),
            ("80", "", false, true),
            ("", "80", false, true),
            ("", "81", false, true),
            ("", "", true, true),
        ] {
            assert_eq!(boots_match(req, own, false), strict, "strict {req:?} {own:?}");
            assert_eq!(boots_match(req, own, true), lenient, "lenient {req:?} {own:?}");
        }
    }

    #[test]
    fn startup_plan_boot_row_compares_only_two_nonempty_boots() {
        use StartPlan::*;
        let with = |boot: &str| Ok(Some(HeldRecord { boot: boot.into(), ..empty() }));
        for (name, rec_boot, own, want) in [
            ("80 vs 81", "80", Ok("81"), Cleanup),
            ("80 vs unknown", "80", Ok(""), Resume),
            ("unknown vs 81", "", Ok("81"), Resume),
            ("unknown vs unknown", "", Ok(""), Resume),
            ("own Err", "80", Err(()), Cleanup),
        ] {
            assert_eq!(startup_plan(&with(rec_boot), own, T0), want, "{name}");
        }
    }

    #[test]
    fn startup_plan_table() {
        use StartPlan::*;
        let _takes_no_liveness: fn(&Result<Option<HeldRecord>, String>, Result<&str, ()>, u64) -> StartPlan = startup_plan;
        let rec = |edit: fn(&mut HeldRecord)| {
            let mut r = empty();
            edit(&mut r);
            Ok(Some(r))
        };
        let future = T0 + 5;
        let cases: Vec<(&str, Result<Option<HeldRecord>, String>, StartPlan)> = vec![
            ("absent", Ok(Option::None), Resume),
            ("unreadable", Err("unreadable".into()), Cleanup),
            ("closing", rec(|r| r.closing = true), Cleanup),
            ("closing, with holders", rec(|r| (r.closing, r.holders) = (true, vec![id(1)])), Cleanup),
            ("other boot", rec(|r| r.boot = "boot-b".into()), Cleanup),
            ("other boot, future handover", rec(|r| (r.boot, r.handover_until_ms) = ("boot-b".into(), Some(T0 + 5))), Cleanup),
            ("other boot, holders", rec(|r| (r.boot, r.holders) = ("boot-b".into(), vec![id(1)])), Cleanup),
            ("handover at its deadline", rec(|r| r.handover_until_ms = Some(T0)), Cleanup),
            ("handover passed", rec(|r| r.handover_until_ms = Some(T0 - 1)), Cleanup),
            ("handover in the future", rec(|r| r.handover_until_ms = Some(T0 + 5)), Pending { until_ms: future }),
            ("holders", rec(|r| r.holders = vec![id(1)]), Pending { until_ms: T0 + handover_bound_ms() }),
            ("holders, handover passed", rec(|r| (r.holders, r.handover_until_ms) = (vec![id(1)], Some(T0 - 1))), Cleanup),
            ("holders, handover in the future", rec(|r| (r.holders, r.handover_until_ms) = (vec![id(1)], Some(T0 + 5))), Pending { until_ms: future }),
            ("only not_ended and forget", rec(|r| (r.not_ended, r.forget) = (2, vec!["w".into()])), Resume),
        ];
        for (name, read, want) in &cases {
            assert_eq!(startup_plan(read, Ok(BOOT), T0), *want, "{name}");
            let unknown = if matches!(read, Ok(Option::None)) { Resume } else { Cleanup };
            assert_eq!(startup_plan(read, Err(()), T0), unknown, "{name}, own boot unknown");
            if !matches!(read, Ok(Some(r)) if r.boot != BOOT) {
                let windows = read.clone().map(|r| r.map(|r| HeldRecord { boot: String::new(), ..r }));
                assert_eq!(startup_plan(&windows, Ok(""), T0), *want, "{name}, \"\" = \"\"");
            }
        }
    }

    #[test]
    fn shutdown_forgets_the_restart_pending() {
        let f = restart(&HeldRecord { holders: vec![id(1), id(2)], ..empty() });
        f.install_planned(T0);
        f.leases.begin_close();
        f.leases.finish_shutdown(0, vec![]).unwrap();
        let rec = f.on_disk();
        assert!(rec.as_ref().map_or(true, |r| r.holders.is_empty() && r.handover_until_ms.is_none()), "{rec:?}");
        assert_eq!(startup_plan(&read_record(&f.path), Ok(BOOT), T0 + 1), StartPlan::Resume);
    }

    #[test]
    fn restart_with_holders_any_lease_clears_the_pending() {
        let (a, b) = (id(1), id(2));
        let mut f = restart(&HeldRecord { holders: vec![a], ..empty() });
        let until = f.install_planned(T0);
        f.grant(&b);
        assert_eq!(f.leases.tick(until), Tick::None, "any granted lease clears the restart's pending: its deadline decides nothing");
        assert_eq!(f.starts.try_recv(), Ok(StartEvent::Qualified));
    }

    #[test]
    fn lease_granted_before_install_pending_still_counts() {
        let a = id(1);
        let f = restart(&HeldRecord { holders: vec![a.clone()], ..empty() });
        f.grant(&a);
        let until = f.install_planned(T0);
        assert_eq!(f.leases.tick(until), Tick::None, "a lease held when the pending was installed counts: its deadline decides nothing");
        assert_eq!(f.on_disk(), Some(HeldRecord { holders: vec![a], ..empty() }), "the pending is cleared");
    }

    #[test]
    fn close_after_pending_clears_is_immediate() {
        let (a, b) = (id(1), id(2));
        let f = restart(&HeldRecord { holders: vec![a.clone(), b], ..empty() });
        f.install_planned(T0);
        let gen = f.grant(&a);
        assert_eq!(
            f.leases.depart(gen, Some(LeaveIntent::Close), T0 + 1),
            Decision::Shutdown,
            "the last lease's Close after the pending cleared is a shutdown at once, never deferred for a recorded holder"
        );
        assert!(f.on_disk().expect("the record").closing);
    }

    #[test]
    fn pending_deadline_is_persisted_once() {
        let f = restart(&HeldRecord { holders: vec![id(1)], ..empty() });
        let until = f.install_planned(T0);
        assert_eq!(
            f.on_disk().map(|r| r.handover_until_ms),
            Some(Some(until)),
            "install_pending writes its deadline as handover_until_ms"
        );
        for now in [T0 + 10_000, until - 1] {
            let plan = startup_plan(&read_record(&f.path), Ok(BOOT), now);
            assert!(matches!(plan, StartPlan::Pending { until_ms, .. } if until_ms == until), "a restart at {now} keeps the deadline: {plan:?}");
        }
        assert_eq!(startup_plan(&read_record(&f.path), Ok(BOOT), until), StartPlan::Cleanup, "a restart at the deadline");
    }

    #[test]
    fn unreadable_record_cleanup_keeps_the_record_on_grant() {
        let f = restart_raw("{not json");
        f.grant(&id(1));
        assert_eq!(
            std::fs::read_to_string(&f.path).unwrap(),
            "{not json",
            "a grant during an unreadable record's Cleanup rewrote the record"
        );
        f.leases.finish_cleanup(0, vec![]).unwrap();
        assert_eq!(f.on_disk(), Some(HeldRecord { holders: vec![id(1)], ..empty() }), "recorded once the Cleanup finishes");
    }

    #[test]
    fn keep_during_startup_cleanup_keeps_the_record() {
        let passed = HeldRecord { holders: vec![id(9)], handover_until_ms: Some(T0 - 1), ..empty() };
        for intent in [LeaveIntent::Keep, LeaveIntent::Handover] {
            let f = restart(&passed);
            let gen = f.grant(&id(1));
            f.leases.depart(gen, Some(intent), T0);
            assert_eq!(f.on_disk(), Some(passed.clone()), "a {intent:?} during the startup Cleanup deleted or rewrote the record");
        }
    }

    #[test]
    fn close_during_startup_cleanup_writes_closing() {
        let f = restart_raw("{not json");
        let gen = f.grant(&id(1));
        assert_eq!(f.leases.depart(gen, Some(LeaveIntent::Close), T0), Decision::Shutdown);
        assert_eq!(f.on_disk(), Some(HeldRecord { closing: true, ..empty() }), "a last Close during the startup Cleanup");
        let f = restart_raw("{not json");
        f.leases.begin_close();
        assert_eq!(f.on_disk(), Some(HeldRecord { closing: true, ..empty() }), "begin_close during the startup Cleanup");
    }

    #[test]
    fn record_roundtrip_and_empty_file_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(bounds::HELD_RECORD_FILE);
        assert_eq!(read_record(&path), Ok(Option::None));
        let full = HeldRecord {
            holders: vec![id(1)],
            handover_until_ms: Some(T0),
            closing: true,
            not_ended: 3,
            forget: vec!["w1".into()],
            ..empty()
        };
        write_or_delete(&path, &full).unwrap();
        assert_eq!(read_record(&path), Ok(Some(full.clone())));
        write_or_delete(&path, &HeldRecord { handover_until_ms: None, ..full }).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"v":1,"boot":"boot-a","holders":[{"boot":"boot-a","pid":1,"created":7001}],"handover_until_ms":null,"closing":true,"not_ended":3,"forget":["w1"]}"#
        );
        let keeps: [(&str, fn(&mut HeldRecord)); 5] = [
            ("holders", |r| r.holders = vec![id(1)]),
            ("handover", |r| r.handover_until_ms = Some(T0)),
            ("closing", |r| r.closing = true),
            ("not_ended", |r| r.not_ended = 1),
            ("forget", |r| r.forget = vec!["w".into()]),
        ];
        for (name, edit) in keeps {
            let mut r = empty();
            edit(&mut r);
            write_or_delete(&path, &r).unwrap();
            assert_eq!(read_record(&path), Ok(Some(r)), "{name} alone keeps the file");
        }
        write_or_delete(&path, &empty()).unwrap();
        assert!(!path.exists(), "an empty record is deleted");
        write_or_delete(&path, &empty()).unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "no temporary file is left");
        for bad in ["{not json", r#"{"v":2,"boot":"","holders":[],"closing":false,"not_ended":0,"forget":[]}"#, r#"{"v":1}"#] {
            std::fs::write(&path, bad).unwrap();
            assert!(read_record(&path).is_err(), "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn record_write_syncs_its_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(bounds::HELD_RECORD_FILE);
        let mode = |m: u32| std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(m)).unwrap();
        let full = HeldRecord { holders: vec![id(1)], ..empty() };
        // Write and search but no read: the rename and the delete succeed,
        // and only the directory's open for its sync fails.
        mode(0o300);
        assert!(write_or_delete(&path, &full).is_err(), "a write whose directory cannot be synced");
        mode(0o700);
        write_or_delete(&path, &full).unwrap();
        mode(0o300);
        assert!(write_or_delete(&path, &empty()).is_err(), "a delete whose directory cannot be synced");
        mode(0o700);
    }
}
