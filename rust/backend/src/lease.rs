//! lease.rs — the window lease: which windows hold this computer's
//! sessions, what the last one's departure decides, and `held.json`, the
//! record that carries both across a daemon restart.
//!
//! A lease is a dedicated connection whose first frame is `fe.lease`; the
//! connection is the handle, so a lease's generation never goes on the
//! wire. This module is the pure core the connection's holder calls: it
//! does no IO but the record write, and it never looks a process up. The
//! peer's identity arrives already read from the OS at accept, and a
//! restart's plan is a function of the record and this boot alone.
//!
//! Every deadline is wall-clock unix milliseconds, because a handover's
//! deadline must survive a restart; one ticker calls [`Leases::tick`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use sot_log::challenge::{PeerAuthOutcome, ProcessIdentity};
use sot_protocol::ops::{lease as bounds, FeLeaseReq, LeaseOutcome, LeaveIntent};
use tokio::sync::{mpsc, Notify};

/// `held.json`'s shape; a record with any other `v` plans Cleanup.
const RECORD_V: u32 = 1;

/// How long a relaunch handover, or a restart's recorded holders, keep
/// the rows waiting for a window.
pub(crate) fn handover_bound() -> std::time::Duration {
    bounds::HANDOVER_BOUND
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
    /// The live leases' identities, plus a pending start's recorded
    /// holders until it resolves.
    pub holders: Vec<ProcessIdentity>,
    /// An in-process or pending handover's deadline.
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

/// What completes a pending start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Qualify {
    /// Any granted lease (the record carried a handover).
    Any,
    /// Only a lease whose `(boot, pid, created)` is one of these.
    Holders(Vec<ProcessIdentity>),
}

/// A start's decision from the record ([`startup_plan`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StartPlan {
    Resume,
    /// End every row without resuming it, write the record, then resume
    /// what remains.
    Cleanup,
    /// Hold back every row registered at start until a qualifying lease,
    /// or Cleanup at `until_ms`.
    Pending { until_ms: u64, qualify: Qualify },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartEvent {
    /// A lease completed the pending start: release the held-back rows
    /// and resume them.
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
    /// A handover expired with no lease held; as [`Decision::Shutdown`].
    Shutdown,
    /// A pending start expired. Its holders stay in the record until the
    /// Cleanup's [`Leases::finish`], so a kill mid-Cleanup cannot resume.
    Cleanup,
}

/// A restart's pending start.
struct Pending {
    until_ms: u64,
    qualify: Qualify,
    /// Its Cleanup has been handed out; it no longer qualifies.
    expired: bool,
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
    pending: Option<Pending>,
    phase: Phase,
    not_ended: u32,
    forget: Vec<String>,
}

impl State {
    fn record(&self) -> HeldRecord {
        let recorded = match &self.pending {
            Some(Pending { qualify: Qualify::Holders(h), .. }) => h.as_slice(),
            _ => &[],
        };
        let mut holders: Vec<ProcessIdentity> = Vec::new();
        for who in self.held.iter().map(|(_, who)| who).chain(recorded) {
            if !holders.contains(who) {
                holders.push(who.clone());
            }
        }
        let pending_until = match &self.pending {
            Some(Pending { until_ms, qualify: Qualify::Any, .. }) => Some(*until_ms),
            _ => None,
        };
        HeldRecord {
            v: RECORD_V,
            boot: self.own_boot.clone().unwrap_or_default(),
            holders,
            handover_until_ms: self.handover_until_ms.or(pending_until),
            closing: self.phase == Phase::Closing,
            not_ended: self.not_ended,
            forget: self.forget.clone(),
        }
    }

    fn persist(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else { return Ok(()) };
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
        self.pending = None;
        self.persist_or_log();
        true
    }
}

/// The daemon's leases, shared by every lease connection, the ticker and
/// the start.
#[derive(Clone)]
pub(crate) struct Leases {
    state: Arc<Mutex<State>>,
    /// Fired once, when a shutdown begins.
    gone: Arc<Notify>,
    starts: mpsc::UnboundedSender<StartEvent>,
}

impl Leases {
    pub(crate) fn new(
        own_boot: Option<String>,
        record: Option<PathBuf>,
    ) -> (Self, mpsc::UnboundedReceiver<StartEvent>) {
        let (starts, events) = mpsc::unbounded_channel();
        let state = State {
            own_boot,
            path: record,
            next_gen: 1,
            held: Vec::new(),
            handover_until_ms: None,
            pending: None,
            phase: Phase::Open,
            not_ended: 0,
            forget: Vec::new(),
        };
        let leases = Leases { state: Arc::new(Mutex::new(state)), gone: Arc::new(Notify::new()), starts };
        (leases, events)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The grant rule, in order: `Closing` once a shutdown has begun;
    /// `Undetermined` if the peer is, or this daemon's boot is unknown;
    /// `Granted` iff the token passed and the claim equals this boot and
    /// the peer's OS identity; otherwise `Foreign`. A grant carries its
    /// generation, clears an in-process handover, and may complete a
    /// pending start. A grant whose record cannot be written is undone and
    /// refused as `Undetermined`, so a grant never outruns its record.
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
        let Some(own_boot) = st.own_boot.as_deref() else {
            return (LeaseOutcome::Undetermined, None);
        };
        let peer = match peer {
            PeerAuthOutcome::Undetermined => return (LeaseOutcome::Undetermined, None),
            PeerAuthOutcome::Foreign => return (LeaseOutcome::Foreign, None),
            PeerAuthOutcome::Authenticated(peer) => peer,
        };
        if !token_ok {
            tracing::warn!(pid = req.pid, "fe.lease refused: bad or missing token");
            return (LeaseOutcome::Foreign, None);
        }
        if !boots_match(&req.boot, own_boot, cfg!(windows)) || req.pid != peer.pid || req.created != peer.created {
            return (LeaseOutcome::Foreign, None);
        }
        let who = ProcessIdentity { boot: req.boot.clone(), pid: req.pid, created: req.created };
        let gen = st.next_gen;
        st.next_gen += 1;
        let handover_until_ms = st.handover_until_ms.take();
        let qualified = st.pending.as_ref().is_some_and(|p| {
            !p.expired
                && now_ms < p.until_ms
                && match &p.qualify {
                    Qualify::Any => true,
                    Qualify::Holders(holders) => holders.contains(&who),
                }
        });
        let cleared = if qualified { st.pending.take() } else { None };
        st.held.push((gen, who));
        if let Err(e) = st.persist() {
            st.held.pop();
            st.handover_until_ms = handover_until_ms;
            if qualified {
                st.pending = cleared;
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
        if !st.held.is_empty() || st.phase != Phase::Open {
            st.persist_or_log();
            return Decision::None;
        }
        match intent.unwrap_or(LeaveIntent::Close) {
            LeaveIntent::Close => {
                st.close();
                drop(st);
                self.gone.notify_one();
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

    /// The 1 s ticker's check: an expired handover with no lease held is a
    /// shutdown; an expired pending start is a Cleanup, handed out once.
    pub(crate) fn tick(&self, now_ms: u64) -> Tick {
        let mut st = self.lock();
        if st.phase != Phase::Open {
            return Tick::None;
        }
        if st.held.is_empty() && st.handover_until_ms.is_some_and(|until| until <= now_ms) {
            st.close();
            drop(st);
            self.gone.notify_one();
            return Tick::Shutdown;
        }
        if let Some(p) = st.pending.as_mut().filter(|p| !p.expired && p.until_ms <= now_ms) {
            p.expired = true;
            return Tick::Cleanup;
        }
        Tick::None
    }

    /// Shutdown step 1, for a shutdown no departure or tick decided.
    /// Idempotent.
    pub(crate) fn begin_close(&self) {
        if self.lock().close() {
            self.gone.notify_one();
        }
    }

    /// An end's report, written as the record: shutdown step 5, or a
    /// Cleanup's write. It ends a shutdown's `closing` (leases stay
    /// refused) and a pending start whose Cleanup this was.
    pub(crate) fn finish(&self, not_ended: u32, forget: Vec<String>) -> std::io::Result<()> {
        let mut st = self.lock();
        if st.phase == Phase::Closing {
            st.phase = Phase::Finished;
        }
        if st.pending.as_ref().is_some_and(|p| p.expired) {
            st.pending = None;
        }
        st.not_ended = not_ended;
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

    /// A start's Pending plan: the record keeps its holders, or its
    /// handover deadline, until a qualifying lease or the Cleanup.
    pub(crate) fn install_pending(&self, until_ms: u64, qualify: Qualify) -> std::io::Result<()> {
        let mut st = self.lock();
        st.pending = Some(Pending { until_ms, qualify, expired: false });
        st.persist()
    }

    /// Resolves once a shutdown has begun.
    pub(crate) async fn gone(&self) {
        let notified = self.gone.notified();
        let open = self.lock().phase == Phase::Open;
        if open {
            notified.await;
        }
    }
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
        match std::fs::remove_file(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            r => r?,
        }
        return sync_dir(path);
    }
    let bytes = serde_json::to_vec(rec).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    sync_dir(path)
}

/// A rename or a delete is durable only once its directory is synced.
/// Windows has no directory sync: std cannot open a directory handle there.
fn sync_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
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
        return StartPlan::Pending { until_ms, qualify: Qualify::Any };
    }
    if !rec.holders.is_empty() {
        let until_ms = now_ms.saturating_add(handover_bound_ms());
        return StartPlan::Pending { until_ms, qualify: Qualify::Holders(rec.holders.clone()) };
    }
    StartPlan::Resume
}

#[cfg(test)]
mod tests {
    use super::*;
    use sot_log::challenge::PeerAuthenticated;
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
        HeldRecord { v: 1, boot: BOOT.into(), holders: vec![], handover_until_ms: None, closing: false, not_ended: 0, forget: vec![] }
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
        let (leases, starts) = Leases::new(own_boot.map(String::from), Some(path.clone()));
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
    }

    #[test]
    fn lease_claim_table() {
        use LeaseOutcome::*;
        let me = id(4242);
        let claim = |edit: fn(&mut FeLeaseReq)| {
            let mut r = req(&me);
            edit(&mut r);
            r
        };
        let cases: Vec<(&str, FeLeaseReq, PeerAuthOutcome, bool, LeaseOutcome)> = vec![
            ("equal identity", req(&me), peer(&me), true, Granted),
            ("different pid", claim(|r| r.pid += 1), peer(&me), true, Foreign),
            ("different created", claim(|r| r.created += 1), peer(&me), true, Foreign),
            ("different boot, same pid and created", claim(|r| r.boot = "boot-b".into()), peer(&me), true, Foreign),
            ("foreign peer", req(&me), PeerAuthOutcome::Foreign, true, Foreign),
            ("undetermined peer", req(&me), PeerAuthOutcome::Undetermined, true, Undetermined),
            ("undetermined peer, bad token", req(&me), PeerAuthOutcome::Undetermined, false, Undetermined),
            ("bad token", req(&me), peer(&me), false, Foreign),
        ];
        let f = fixture(Some(BOOT));
        for (name, r, p, token_ok, want) in &cases {
            let (got, gen) = f.leases.grant(r, p, *token_ok, T0);
            assert_eq!(got, *want, "{name}");
            assert_eq!(gen.is_some(), *want == Granted, "{name}: a generation iff granted");
        }
        let a = f.grant(&me);
        let b = f.grant(&me);
        assert_ne!(a, b, "two leases from one identity are two entries");
        assert_eq!(f.leases.record().holders, vec![me.clone()]);

        let f = fixture(None);
        for p in [peer(&me), PeerAuthOutcome::Foreign] {
            assert_eq!(f.leases.grant(&req(&me), &p, true, T0).0, Undetermined, "missing daemon boot, {p:?}");
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
        f.leases.install_pending(T0 + handover_bound_ms(), Qualify::Holders(vec![id(1)])).unwrap();
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
        f.leases.install_pending(until, Qualify::Any).unwrap();
        assert_eq!(f.on_disk(), Some(HeldRecord { handover_until_ms: Some(until), ..empty() }));
        f.grant(&id(9));
        assert_eq!(f.starts.try_recv(), Ok(StartEvent::Qualified), "any grant completes a handover start");
        assert_eq!(f.on_disk(), Some(HeldRecord { holders: vec![id(9)], ..empty() }));
        assert_eq!(f.leases.tick(until), Tick::None);

        let mut f = fixture(Some(BOOT));
        f.leases.install_pending(until, Qualify::Holders(vec![id(1)])).unwrap();
        assert_eq!(f.on_disk(), Some(HeldRecord { holders: vec![id(1)], ..empty() }));
        f.grant(&id(2));
        assert!(f.starts.try_recv().is_err(), "an unrecorded window does not complete it");
        assert_eq!(f.leases.record().holders, vec![id(2), id(1)]);
        f.grant(&id(1));
        assert_eq!(f.starts.try_recv(), Ok(StartEvent::Qualified), "a recorded holder completes it");
        assert_eq!(f.leases.tick(until), Tick::None);

        for qualify in [Qualify::Any, Qualify::Holders(vec![id(1)])] {
            let mut f = fixture(Some(BOOT));
            f.leases.install_pending(until, qualify.clone()).unwrap();
            assert_eq!(f.leases.tick(until - 1), Tick::None);
            assert_eq!(f.leases.tick(until), Tick::Cleanup, "{qualify:?}");
            assert_eq!(f.leases.tick(until + 1), Tick::None, "handed out once");
            let read = Ok(f.on_disk());
            assert_ne!(startup_plan(&read, Ok(BOOT), until + 1), StartPlan::Resume, "a kill mid-Cleanup never resumes, {qualify:?}");
            f.leases.grant(&req(&id(1)), &peer(&id(1)), true, until);
            assert!(f.starts.try_recv().is_err(), "a lease after expiry does not complete it");
            f.leases.finish(0, vec![]).unwrap();
            assert_eq!(f.on_disk(), Some(HeldRecord { holders: vec![id(1)], ..empty() }), "{qualify:?}");
        }
    }

    #[test]
    fn notice_clears_only_on_matching_ack() {
        let f = fixture(Some(BOOT));
        f.leases.finish(2, vec![]).unwrap();
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
            ("handover in the future", rec(|r| r.handover_until_ms = Some(T0 + 5)), Pending { until_ms: future, qualify: Qualify::Any }),
            (
                "holders",
                rec(|r| r.holders = vec![id(1)]),
                Pending { until_ms: T0 + handover_bound_ms(), qualify: Qualify::Holders(vec![id(1)]) },
            ),
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
