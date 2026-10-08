//! lease.rs — the window lease: which windows hold this computer's
//! sessions, what the last one's departure decides, and `held.json`, the
//! record that carries both across a daemon restart.
//!
//! A lease is a dedicated connection whose hello says `handoff` and whose next frame is `fe.lease`; the
//! connection is the handle, so a lease's generation never goes on the
//! wire. This module is the pure core the connection's holder calls: it
//! does no IO but the record's write, and it never looks a process up. The
//! peer's identity arrives already read from the OS at accept, and a
//! restart's plan is a function of the record and this boot alone.
//!
//! Every deadline is wall-clock unix milliseconds, because a handover's
//! deadline must survive a restart; one ticker calls [`Leases::tick`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use sot_log::identity::challenge::{PeerAuthenticated, ProcessIdentity};
use sot_protocol::ops::{
    lease as bounds, op, FeLeaseReq, FeLeaseRes, FeLeavingReq, FeLeavingRes, FeNoticeSeenReq, FeNoticeSeenRes,
    LeaseOutcome, LeaveIntent,
};
use sot_protocol::{Frame, Kind};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite};
use tokio::sync::watch;

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
    /// End every row without resuming it, then write the record.
    Cleanup,
    /// Resume, and wait for a window as a pending handover does: any
    /// granted lease clears it; at `until_ms` with no lease held it is a
    /// shutdown.
    Pending { until_ms: u64 },
}

/// What a departure decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    None,
    /// The last lease ended with `Close`; the lease mutex already refuses
    /// new leases and `gone` has fired.
    Shutdown,
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
    /// An update restart is committed: no close begins and no lease is granted; the process exits 75.
    Updating,
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

    /// An end's `forget`, added to the ids already kept: an id leaves
    /// only when a start removes its registration.
    fn forget_also(&mut self, ids: Vec<String>) {
        for id in ids {
            if !self.forget.contains(&id) {
                self.forget.push(id);
            }
        }
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
    /// The record's count after shutdown step 5, for the waiting closer.
    report: watch::Sender<Option<u32>>,
    /// The waiting closer has been answered (step 6), or gave up.
    answered: watch::Sender<bool>,
}

impl Leases {
    /// From the start's own read of the record: `loaded` is what it read,
    /// and `cleanup` is true iff its plan is Cleanup.
    pub(crate) fn new(
        own_boot: Option<String>,
        record: Option<PathBuf>,
        loaded: Option<&HeldRecord>,
        cleanup: bool,
    ) -> Self {
        // The count survives a restart until a window acks it, and the
        // forget list until the start removes each id's registration.
        let (not_ended, forget) = loaded.map_or((0, Vec::new()), |rec| (rec.not_ended, rec.forget.clone()));
        let state = State {
            own_boot,
            path: record,
            next_gen: 1,
            held: Vec::new(),
            handover_until_ms: None,
            pending_until_ms: None,
            phase: Phase::Open,
            not_ended,
            forget,
            closer_waits: false,
            startup_cleanup: cleanup,
        };
        Leases {
            state: Arc::new(Mutex::new(state)),
            gone: watch::Sender::new(false),
            report: watch::Sender::new(None),
            answered: watch::Sender::new(false),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The grant rule, in order: `Closing` once a shutdown has begun;
    /// `Undetermined` if this daemon's boot is unknown;
    /// `Granted` iff the claim equals this boot and
    /// the peer's OS identity; otherwise `Foreign`. A grant carries its
    /// generation and clears an in-process handover and a pending start:
    /// one still here is one the ticker has not acted on, whatever the
    /// clock says. A grant whose record cannot be written
    /// is undone and refused as `Undetermined`, so a grant never outruns
    /// its record.
    pub(crate) fn grant(
        &self,
        req: &FeLeaseReq,
        peer: &PeerAuthenticated,
    ) -> (LeaseOutcome, Option<u64>) {
        let mut st = self.lock();
        if st.phase != Phase::Open {
            return (LeaseOutcome::Closing, None);
        }
        let who = match claim(st.own_boot.as_deref(), req, peer) {
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
        let pending = st.pending_until_ms.take();
        st.held.push((gen, who));
        if let Err(e) = st.persist() {
            st.held.pop();
            st.handover_until_ms = handover;
            st.pending_until_ms = pending;
            let path = st.path.as_deref().unwrap_or(Path::new(""));
            tracing::error!(path = %path.display(), "fe.lease refused: held record not written: {e}");
            return (LeaseOutcome::Undetermined, None);
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

    /// A later `fe.leaving` on a lease that already departed: the user's
    /// latest intent replaces the earlier one, so it is that window's
    /// departure at this moment. With no other lease held it decides.
    pub(crate) fn leave_again(&self, intent: LeaveIntent, now_ms: u64) -> Decision {
        let st = self.lock();
        self.decide(st, Some(intent), now_ms)
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
                // A Keep after a Handover is open-ended.
                st.handover_until_ms = None;
                st.persist_or_log();
                Decision::None
            }
            LeaveIntent::Handover => {
                st.handover_until_ms = Some(now_ms.saturating_add(handover_bound_ms()));
                st.persist_or_log();
                Decision::None
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

    /// Commit an update restart iff no shutdown has begun: the phase becomes `Updating` under the lease lock, so no close
    /// begins afterwards (`close` refuses it) and no lease is granted. The caller exits 75 after this returns, outside the
    /// lock. False once a shutdown has begun, or an update was already committed: the shutdown's own exit stands.
    pub(crate) fn commit_update(&self) -> bool {
        let mut st = self.lock();
        if st.phase != Phase::Open {
            return false;
        }
        st.phase = Phase::Updating;
        true
    }

    /// Shutdown step 5, the shutdown's report written as the record, its
    /// count added to one no window has acked: it ends `closing` (leases
    /// stay refused until the process exits). The waiting closer is told
    /// the sum, so its ack clears it.
    pub(crate) fn finish_shutdown(&self, not_ended: u32, forget: Vec<String>) -> std::io::Result<()> {
        let mut st = self.lock();
        if st.phase == Phase::Closing {
            st.phase = Phase::Finished;
        }
        st.not_ended = st.not_ended.saturating_add(not_ended);
        st.forget_also(forget);
        let written = st.persist();
        let total = st.not_ended;
        drop(st);
        self.report.send_replace(Some(total));
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
        st.forget_also(forget);
        st.persist()
    }

    /// The start's forget pass: of the record's `forget`, only the ids
    /// whose registration would not go stay, so every later record write
    /// keeps them for the next start.
    pub(crate) fn keep_unremoved(&self, unremoved: &[String]) {
        self.lock().forget.retain(|id| unremoved.contains(id));
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

    /// Resolves with the record's count after shutdown step 5.
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
/// with the count not ended. From the grant on there is one exit, so a
/// failed write departs too.
pub(crate) async fn hold<R, W>(
    mut rx: R,
    mut tx: W,
    first: Frame,
    peer: PeerAuthenticated,
    leases: &Leases,
    state_root: Option<&Path>,
) -> anyhow::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let Ok(req) = serde_json::from_value::<FeLeaseReq>(first.payload.clone()) else {
        return crate::server::pipe::reject(&mut tx, first.id, op::FE_LEASE, "bad_request", "malformed fe.lease").await;
    };
    let (outcome, gen) = leases.grant(&req, &peer);
    let granted = gen.is_some();
    let res = FeLeaseRes {
        outcome,
        state_root: state_root.filter(|_| granted).map(sot_log::host::state_dir::state_dir_hash),
        not_ended: if granted { leases.notice() } else { 0 },
    };
    let Some(gen) = gen else {
        return reply(&mut tx, first.id, op::FE_LEASE, &res).await;
    };

    let mut held = Some(gen);
    let end: anyhow::Result<()> = async {
        reply(&mut tx, first.id, op::FE_LEASE, &res).await?;
        loop {
            let bytes = match read_line(&mut rx).await {
                Ok(Line::Complete(bytes)) => bytes,
                Ok(Line::OverCap) => {
                    crate::server::pipe::reject(&mut tx, 0, op::FE_LEASE, "bad_request", "line over the lease cap").await?;
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
                    crate::server::pipe::reject(&mut tx, 0, op::FE_LEASE, "bad_request", "not a request").await?;
                    continue;
                }
            };
            match frame.op.as_str() {
                op::FE_LEAVING => {
                    let Ok(leaving) = serde_json::from_value::<FeLeavingReq>(frame.payload.clone()) else {
                        crate::server::pipe::reject(&mut tx, frame.id, op::FE_LEAVING, "bad_request", "malformed fe.leaving").await?;
                        continue;
                    };
                    // The first departs the lease; each later one replaces
                    // its intent (the latest intent wins).
                    let decision = match held.take() {
                        Some(gen) => leases.depart(gen, Some(leaving.intent), now_ms()),
                        None => leases.leave_again(leaving.intent, now_ms()),
                    };
                    if decision == Decision::Shutdown && leaving.intent == LeaveIntent::Close {
                        answer_close(&mut rx, &mut tx, frame.id, leases).await;
                        return Ok(());
                    }
                    reply(&mut tx, frame.id, op::FE_LEAVING, &FeLeavingRes { not_ended: 0 }).await?;
                }
                op::FE_NOTICE_SEEN => {
                    let Ok(seen) = serde_json::from_value::<FeNoticeSeenReq>(frame.payload.clone()) else {
                        crate::server::pipe::reject(&mut tx, frame.id, op::FE_NOTICE_SEEN, "bad_request", "malformed fe.notice_seen").await?;
                        continue;
                    };
                    if let Err(e) = leases.notice_seen(seen.not_ended) {
                        tracing::error!("held record not written after fe.notice_seen: {e}");
                    }
                    reply(&mut tx, frame.id, op::FE_NOTICE_SEEN, &FeNoticeSeenRes {}).await?;
                }
                other => {
                    crate::server::pipe::reject(&mut tx, frame.id, other, "bad_request", "not a lease request").await?;
                }
            }
        }
        Ok(())
    }
    .await;
    // Every `fe.leaving` departs on receipt, so a lease still held here
    // received none: it departs as a close.
    if let Some(gen) = held {
        leases.depart(gen, None, now_ms());
    }
    end
}

/// Shutdown step 6 for the window whose `fe.leaving{close}` decided it:
/// the record's count after the shutdown, then up to `NOTICE_ACK_WAIT` for its
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
    crate::server::reply::write_frame_within(tx, &frame, None, bounds::LEASE_REPLY_WAIT).await
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
/// the refusal and the check that failed.
fn claim(
    own_boot: Option<&str>,
    req: &FeLeaseReq,
    peer: &PeerAuthenticated,
) -> Result<ProcessIdentity, (LeaseOutcome, &'static str)> {
    let Some(own_boot) = own_boot else {
        return Err((LeaseOutcome::Undetermined, "own boot unknown"));
    };
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
#[path = "lease_tests.rs"]
mod tests;
