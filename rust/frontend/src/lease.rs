// The window lease (ADR 0050): one dedicated connection per local daemon,
// held for the window's life, so the daemon knows a window is attached and
// the window can say, on leaving, what should happen to the computer's
// sessions. This file is the lease's whole client side; `net/transport/mod.rs` calls
// `before_data_connection` before each local data connection, and the window
// (ui/chrome/draw.rs, ui/app/frame.rs) reads `notice()` and `owed()`, acks through `notice_seen`, and leaves through it.
// Part of lifecycle; charter: rust/backend/src/lifecycle/CLAUDE.md.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use interprocess::local_socket::tokio::prelude::*;
use sot_log::identity::challenge::self_identity;
use sot_protocol::ops::{lease, op, FeLeaseReq, FeLeaseRes, FeLeavingReq, FeNoticeSeenReq, LeaveIntent, LeaseOutcome};
use sot_protocol::{codec, Frame, HelloReq, HANDOFF_ROLE};
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::{mpsc, oneshot};
use tokio::sync::oneshot::error::TryRecvError;

use crate::net::dial::HostKey;
use crate::net::transport::{connect_pipe, Dial, TransportConfig};

const NOTICE_UNDETERMINED: &str =
    "closing will not end sessions: this computer's backend could not verify this window";
const NOTICE_UNSUPPORTED: &str =
    "closing will not end sessions: this computer's backend is older than this window";
const NOTICE_NO_BACKEND: &str =
    "closing will not end sessions: there is no backend on this computer";
const LEAVE_UNCONFIRMED_CLOSE: &str =
    "closing: this computer's backend did not confirm the sessions ended";
const LEAVE_UNCONFIRMED_KEEP: &str =
    "keep was not confirmed: this computer's backend may have ended the sessions";

/// What the daemon sent back to the hello and the lease request: the lease's own reply, or the hello's refusal.
enum Replies {
    Lease(Frame),
    Refused { code: String, error: String },
}

/// Reads the hello's reply and then the lease's, through the one reader, so the caller's wait rule covers both.
async fn read_replies<R: AsyncBufRead + Unpin>(rx: &mut R) -> Result<Replies> {
    let (hello_reply, _) = codec::read_frame(rx).await?;
    if let Some(error) = hello_reply.payload.get("error").and_then(|e| e.as_str()) {
        let code = hello_reply.payload.get("code").and_then(|c| c.as_str()).unwrap_or("");
        return Ok(Replies::Refused { code: code.to_string(), error: error.to_string() });
    }
    let (reply, _) = codec::read_frame(rx).await?;
    Ok(Replies::Lease(reply))
}

/// A window that never leases: `--ephemeral`, `--capture`, `--no-lease`.
pub fn lease_exempt(ephemeral: bool, capture: bool, no_lease: bool) -> bool {
    ephemeral || capture || no_lease
}

/// The hosts a lease can exist for: those dialled over a local pipe.
pub fn pipe_hosts(connections: &[(HostKey, TransportConfig)]) -> Vec<HostKey> {
    connections
        .iter()
        .filter(|(_, c)| matches!(c.dial, Dial::Pipe(_)))
        .map(|(h, _)| h.clone())
        .collect()
}

/// Where one host's lease stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    Pending,
    Granted { state_root: Option<String> },
    Foreign,
    Undetermined,
    Unsupported,
    Unreached,
}

enum HolderCmd {
    Leave(LeaveIntent, oneshot::Sender<LeaveOutcome>),
    NoticeSeen(u32),
    /// Answered once every command queued before it is written.
    Written(oneshot::Sender<()>),
}

/// How one lease's leave ended. A zero count is only a real reply of 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaveOutcome {
    Replied(u32),
    Failed(String),
    TimedOut,
}

type PendingAck = (HostKey, oneshot::Receiver<LeaveOutcome>);

struct Slot {
    standing: Standing,
    holder: Option<mpsc::UnboundedSender<HolderCmd>>,
}

/// The slots and the leave, under one lock, so a lease that a handshake
/// already in flight grants after `leave_all` took its snapshot still leaves.
struct Book {
    slots: HashMap<HostKey, Slot>,
    /// Set by `leave_all`; from then on no handshake starts.
    leaving: Option<LeaveIntent>,
    /// Lease handshakes in flight (`Handshake`).
    inflight: usize,
    /// Where a handshake in flight puts its one outcome for the `Leaving`
    /// wait (`set`); open while one is in flight, so the leave is not done
    /// until it closes.
    late: Option<mpsc::UnboundedSender<PendingAck>>,
    /// Each daemon's newest nonzero not-ended count no presented frame has acked, keyed at
    /// its grant by the state root it named (else its label): `owed`, `notice_seen`.
    rt: Option<tokio::runtime::Handle>,
    owed: BTreeMap<HostKey, u32>,
}

/// A lease handshake in flight. Its drop, after its `set`, counts it down; the last one closes a leave's late-grant channel.
struct Handshake<'a>(&'a Leases);

impl Drop for Handshake<'_> {
    fn drop(&mut self) {
        let mut book = self.0.book.lock().unwrap();
        book.inflight -= 1;
        if book.inflight == 0 {
            book.late = None;
        }
    }
}

/// Every lease this window holds or wants, one slot per local host.
pub struct Leases {
    exempt: bool,
    book: Mutex<Book>,
    /// `lease::LEASE_REPLY_WAIT`: how long the request may take to send, and
    /// when an unanswered one is warned about; tests shorten it.
    reply_wait: Duration,
}

impl Leases {
    pub fn new(exempt: bool, pipe_hosts: Vec<HostKey>) -> Arc<Self> {
        let slots = pipe_hosts
            .into_iter()
            .map(|h| (h, Slot { standing: Standing::Pending, holder: None }))
            .collect();
        Arc::new(Self {
            exempt,
            book: Mutex::new(Book { slots, leaving: None, inflight: 0, late: None, rt: None, owed: BTreeMap::new() }),
            reply_wait: lease::LEASE_REPLY_WAIT,
        })
    }

    /// Granted, and the holder task that owns the stream is still running.
    pub(crate) fn held(&self, host: &HostKey) -> bool {
        let book = self.book.lock().unwrap();
        book.slots.get(host).is_some_and(|s| {
            matches!(s.standing, Standing::Granted { .. })
                && s.holder.as_ref().is_some_and(|h| !h.is_closed())
        })
    }

    fn resolved(slot: &Slot) -> Standing {
        match (&slot.standing, &slot.holder) {
            (Standing::Granted { .. }, Some(h)) if h.is_closed() => Standing::Unreached,
            (s, _) => s.clone(),
        }
    }

    #[cfg(test)]
    fn standing(&self, host: &HostKey) -> Option<Standing> {
        self.book.lock().unwrap().slots.get(host).map(Self::resolved)
    }

    fn set(&self, host: &HostKey, standing: Standing, holder: Option<mpsc::UnboundedSender<HolderCmd>>) {
        Self::install(&mut self.book.lock().unwrap(), host, standing, holder);
    }

    fn install(book: &mut Book, host: &HostKey, standing: Standing, holder: Option<mpsc::UnboundedSender<HolderCmd>>) {
        // A handshake in flight when the window began to leave gives the
        // leave exactly one outcome: a grant leaves at once too, and its ack
        // joins the same wait (a failed send drops `tx`, which the wait
        // records as a failure); any other end is not confirmed.
        if let (Some(intent), Some(late)) = (book.leaving, &book.late) {
            let (tx, rx) = oneshot::channel();
            match &holder {
                Some(h) => {
                    let _ = h.send(HolderCmd::Leave(intent, tx));
                }
                None => {
                    let _ = tx.send(LeaveOutcome::Failed(format!("the lease was not granted: {standing:?}")));
                }
            }
            let _ = late.send((host.clone(), rx));
        }
        book.slots.insert(host.clone(), Slot { standing, holder });
    }

    /// Claim the daemon on a dedicated connection before a data connection is
    /// made. Returns the count of sessions an earlier close could not end.
    /// An `Err` is the transport's cue to back off and retry.
    #[allow(clippy::too_many_lines, reason = "claims the daemon on a dedicated connection before the data connection; predates the 100-line limit")]
    pub async fn before_data_connection(
        &self,
        host: &HostKey,
        path: &Path,
        token: Option<&str>,
    ) -> Result<u32> {
        if self.exempt || self.held(host) {
            return Ok(0);
        }
        let _handshake = {
            let mut book = self.book.lock().unwrap();
            // The window is leaving: no new lease, and the failed-connect path.
            if book.leaving.is_some() {
                return Err(anyhow!("this window is closing").context("window lease"));
            }
            book.inflight += 1;
            Handshake(self)
        };
        let ident = match self_identity() {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(%host, error = %e, "window lease: cannot identify this process");
                self.set(host, Standing::Undetermined, None);
                return Ok(0);
            }
        };
        let req = FeLeaseReq { boot: ident.boot, pid: ident.pid, created: ident.created, token: token.map(str::to_string) };
        // Connect and write are bounded: failing here changes nothing at the
        // daemon. The hello (a handoff, ADR 0049 `## User isolation`) and the
        // lease go out in one write. Once they are written the replies are
        // awaited for as long as the connection lasts, since the daemon may
        // still grant the lease and a dropped granted connection departs as a
        // Close.
        let sent = tokio::time::timeout(self.reply_wait, async {
            let stream = connect_pipe(path).await?;
            let (rx, mut tx) = stream.split();
            let rx = codec::buffered(rx);
            let hello = HelloReq::this_process(
                "sot-fe-lease",
                HANDOFF_ROLE,
                Some(crate::net::identity::frontend_identity().host.clone()),
            )?;
            let mut both = Vec::new();
            codec::write_frame(&mut both, &Frame::req(1, op::HELLO, serde_json::to_value(&hello)?), None).await?;
            codec::write_frame(&mut both, &Frame::req(2, op::FE_LEASE, serde_json::to_value(&req)?), None).await?;
            tx.write_all(&both).await?;
            tx.flush().await?;
            Ok::<_, anyhow::Error>((rx, tx))
        })
        .await;
        let (mut rx, tx) = match sent {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                self.set(host, Standing::Unreached, None);
                tracing::warn!(%host, error = %format_args!("{e:#}"), "window lease: not taken");
                return Err(e.context("window lease"));
            }
            Err(_) => {
                self.set(host, Standing::Unreached, None);
                tracing::warn!(%host, "window lease: the request was not sent in time");
                return Err(anyhow!("the window lease request was not sent in time").context("window lease"));
            }
        };
        let replied = {
            let read = read_replies(&mut rx);
            tokio::pin!(read);
            match tokio::time::timeout(self.reply_wait, &mut read).await {
                Ok(r) => r,
                Err(_) => {
                    tracing::warn!(%host, waited = ?self.reply_wait, "window lease: no reply yet; still waiting, since dropping the request would read as a close");
                    read.await
                }
            }
        };
        let reply = match replied {
            Ok(Replies::Lease(reply)) => reply,
            // Not a failed connect: the data connection's own hello is refused the same way and shows the
            // daemon's message on the blocking screen, which an `Err` here would never let it reach.
            Ok(Replies::Refused { code, error }) => {
                self.set(host, Standing::Undetermined, None);
                tracing::warn!(%host, %code, %error, "window lease: the backend refused this window's hello");
                return Ok(0);
            }
            Err(e) => {
                self.set(host, Standing::Unreached, None);
                tracing::warn!(%host, error = %format_args!("{e:#}"), "window lease: not taken");
                return Err(anyhow::Error::from(e).context("window lease"));
            }
        };
        let res: FeLeaseRes = match serde_json::from_value(reply.payload) {
            Ok(r) => r,
            Err(_) => {
                self.set(host, Standing::Unsupported, None);
                tracing::warn!(%host, "window lease: this backend does not know it");
                return Ok(0);
            }
        };
        match res.outcome {
            LeaseOutcome::Granted => {
                let holder = spawn_holder(rx, tx);
                let key = res.state_root.clone().unwrap_or_else(|| host.clone());
                {
                    // The count and the holder its ack goes through are published under one lock: a count drawn
                    // and acked before its holder was installed would be acked to nobody and stay owed forever.
                    let mut book = self.book.lock().unwrap();
                    book.rt.get_or_insert_with(tokio::runtime::Handle::current);
                    if res.not_ended > 0 {
                        book.owed.insert(key, res.not_ended);
                    }
                    Self::install(&mut book, host, Standing::Granted { state_root: res.state_root }, Some(holder));
                }
                tracing::info!(%host, not_ended = res.not_ended, "window lease: granted");
                Ok(res.not_ended)
            }
            LeaseOutcome::Foreign => {
                self.set(host, Standing::Foreign, None);
                tracing::warn!(%host, "window lease: another user's backend");
                Ok(0)
            }
            LeaseOutcome::Undetermined => {
                self.set(host, Standing::Undetermined, None);
                tracing::warn!(%host, "window lease: the backend could not verify this window");
                Ok(0)
            }
            LeaseOutcome::Closing => {
                self.set(host, Standing::Unreached, None);
                tracing::warn!(%host, "window lease: the backend is shutting down");
                Err(anyhow!("this computer's backend is shutting down"))
            }
        }
    }

    /// The status-line notice for the current standings, if any.
    pub fn notice(&self) -> Option<&'static str> {
        let standings: Vec<Standing> =
            self.book.lock().unwrap().slots.values().map(Self::resolved).collect();
        lease_notice(self.exempt, &standings)
    }

    /// The state roots of every granted lease.
    pub fn granted_state_roots(&self) -> Vec<String> {
        self.book
            .lock()
            .unwrap()
            .slots
            .values()
            .filter_map(|s| match Self::resolved(s) {
                Standing::Granted { state_root } => state_root,
                _ => None,
            })
            .collect()
    }

    /// The not-ended counts owed, one per daemon, keyed as each grant named it.
    pub fn owed(&self) -> Vec<(HostKey, u32)> {
        self.book.lock().unwrap().owed.iter().map(|(k, n)| (k.clone(), *n)).collect()
    }

    /// Tell the daemon behind `key` (a label, or the state root a grant named) that its
    /// count `n` was shown. It stops being owed only if it is still `n`; a newer one shows in its turn.
    pub fn notice_seen(&self, key: &HostKey, n: u32) {
        let mut book = self.book.lock().unwrap();
        if book.owed.get(key) == Some(&n) {
            book.owed.remove(key);
        }
        for (host, slot) in book.slots.iter() {
            let root = match &slot.standing {
                Standing::Granted { state_root } => state_root.as_ref(),
                _ => None,
            };
            if host == key || root == Some(key) {
                if let Some(h) = &slot.holder {
                    let _ = h.send(HolderCmd::NoticeSeen(n));
                }
            }
        }
    }

    /// Ask every held lease's daemon to end (or keep) its sessions. None when
    /// no lease is held and no handshake is in flight: the window can exit at
    /// once. From here no new lease is taken, and one that a handshake in
    /// flight grants leaves too (`set`); the leave waits for each such
    /// handshake. A second call (an X during a Keep) supersedes the first:
    /// each holder writes the new line after the old one, and the old acks
    /// are dropped.
    pub fn leave_all(&self, intent: LeaveIntent, exit_code: i32, now: Instant) -> Option<Leaving> {
        let mut book = self.book.lock().unwrap();
        let (late_tx, late) = mpsc::unbounded_channel();
        book.leaving = Some(intent);
        book.late = (book.inflight > 0).then_some(late_tx);
        let mut pending = Vec::new();
        for (host, slot) in book.slots.iter() {
            if !matches!(Self::resolved(slot), Standing::Granted { .. }) {
                continue;
            }
            if let Some(h) = &slot.holder {
                // A failed send drops `tx`: the wait records the failure.
                let (tx, rx) = oneshot::channel();
                let _ = h.send(HolderCmd::Leave(intent, tx));
                pending.push((host.clone(), rx));
            }
        }
        if pending.is_empty() && book.late.is_none() {
            return None;
        }
        let mut leaving = Leaving::new(intent, exit_code, pending, now);
        leaving.late = Some(late);
        Some(leaving)
    }

    /// A forced exit's last step: block until every holder has written each
    /// leave already queued to it (the write, not the reply), at most `bound`
    /// in all, so the stream's EOF never overtakes a queued Close.
    pub fn deliver_queued(&self, bound: Duration) {
        let deadline = Instant::now() + bound;
        let (waits, rt) = loop {
            match self.book.try_lock() {
                Ok(book) => break (book.slots.iter().filter_map(|(host, slot)| {
                    let holder = slot.holder.as_ref()?;
                    let (tx, rx) = oneshot::channel();
                    // A failed send leaves a closed receipt, which is unconfirmed.
                    let _ = holder.send(HolderCmd::Written(tx));
                    Some((host.clone(), rx))
                }).collect::<Vec<_>>(), book.rt.clone()),
                Err(std::sync::TryLockError::WouldBlock) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        tracing::warn!("window lease: lease book stayed locked through forced delivery deadline");
                        return;
                    }
                    std::thread::sleep(left.min(Duration::from_millis(2)));
                }
                Err(std::sync::TryLockError::Poisoned(_)) => panic!("window lease book poisoned"),
            }
        };
        let Some(rt) = rt else { return; };
        let unwritten = rt.block_on(async {
            let deadline = tokio::time::Instant::now() + bound;
            let mut unwritten = Vec::new();
            for (host, rx) in waits {
                if !matches!(tokio::time::timeout_at(deadline, rx).await, Ok(Ok(()))) { unwritten.push(host); }
            }
            unwritten
        });
        if !unwritten.is_empty() {
            tracing::warn!(hosts = ?unwritten, "window lease: exiting before every queued leave was written");
        }
    }

}

/// How long the leaving line holds, from the presented frame that drew it,
/// before the window exits.
pub const NOT_ENDED_EXIT_HOLD: Duration = Duration::from_secs(3);
/// How long a line waits for a frame to present it (a minimized or occluded
/// window may never); then the window exits without its ack.
pub const NOT_ENDED_PRESENT_WAIT: Duration = Duration::from_secs(2);
/// How often the window polls the acks while leaving.
pub const LEAVE_POLL: Duration = Duration::from_millis(250);
/// How long a forced exit waits for the queued leaves to be written.
pub const LEAVE_WRITE_WAIT: Duration = Duration::from_secs(1);

/// A window on its way out: waiting for the daemons' acks, then for a frame
/// to show what they left to say.
pub struct Leaving {
    pub intent: LeaveIntent,
    pub exit_code: i32,
    pending: Vec<PendingAck>,
    /// The outcomes of handshakes in flight when the leave began
    /// (`Leases::set`); None once closed, when no handshake is in flight.
    late: Option<mpsc::UnboundedReceiver<PendingAck>>,
    outcomes: Vec<(HostKey, LeaveOutcome)>,
    deadline: Instant,
    /// The line the outcomes left to show, and when the poll first had it.
    shown: Option<(String, Instant)>,
    /// The counts that line acks once a presented frame has drawn it.
    acks: Vec<(HostKey, u32)>,
    hold_until: Option<Instant>,
}

/// What the event loop does next while leaving.
#[derive(Debug, PartialEq, Eq)]
pub enum LeaveStep {
    Wait(Instant),
    Show,
    Exit,
}

impl Leaving {
    pub fn new(
        intent: LeaveIntent,
        exit_code: i32,
        pending: Vec<PendingAck>,
        now: Instant,
    ) -> Self {
        let wait = match intent {
            LeaveIntent::Close => lease::CLOSE_ACK_WAIT,
            _ => lease::KEEP_ACK_WAIT,
        };
        Self {
            intent,
            exit_code,
            pending,
            late: None,
            outcomes: Vec::new(),
            deadline: now + wait,
            shown: None,
            acks: Vec::new(),
            hold_until: None,
        }
    }

    pub fn poll(&mut self, now: Instant) -> LeaveStep {
        if let Some(t) = self.hold_until {
            return if now >= t { LeaveStep::Exit } else { LeaveStep::Wait(t) };
        }
        if let Some((_, since)) = &self.shown {
            // No frame has presented the line (a minimized or occluded window
            // may never). Past the bound the window exits unacked, so the
            // daemon keeps the count and the next window shows it.
            let bound = *since + NOT_ENDED_PRESENT_WAIT;
            if now < bound {
                return LeaveStep::Wait(bound);
            }
            tracing::warn!(intent = ?self.intent, "window lease: no frame showed the leaving line; exiting without its ack");
            return LeaveStep::Exit;
        }
        let mut closed = false;
        if let Some(late) = self.late.as_mut() {
            loop {
                match late.try_recv() {
                    Ok(p) => self.pending.push(p),
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        closed = true;
                        break;
                    }
                }
            }
        }
        if closed {
            self.late = None;
        }
        let mut still = Vec::new();
        for (host, mut rx) in std::mem::take(&mut self.pending) {
            match rx.try_recv() {
                Ok(o) => self.outcomes.push((host, o)),
                Err(TryRecvError::Closed) => {
                    self.outcomes.push((host, LeaveOutcome::Failed("the lease ended before its reply".to_string())))
                }
                Err(TryRecvError::Empty) => still.push((host, rx)),
            }
        }
        self.pending = still;
        if (!self.pending.is_empty() || self.late.is_some()) && now < self.deadline {
            return LeaveStep::Wait(self.deadline.min(now + LEAVE_POLL));
        }
        let timed_out = std::mem::take(&mut self.pending).into_iter().map(|(h, _)| (h, LeaveOutcome::TimedOut));
        self.outcomes.extend(timed_out);
        // A handshake still in flight at the deadline is not confirmed either.
        if self.late.take().is_some() {
            self.outcomes.push(("a lease handshake in flight".to_string(), LeaveOutcome::TimedOut));
        }
        let report = leave_report(self.intent, &self.outcomes);
        for (host, outcome) in &report.warn {
            tracing::warn!(%host, intent = ?self.intent, ?outcome, "window lease: the leave was not confirmed");
        }
        let Some(line) = report.line else { return LeaveStep::Exit };
        self.shown = Some((line, now));
        self.acks = report.acks;
        LeaveStep::Show
    }

    /// A presented frame drew the line whole: the hold starts from it, and
    /// the counts the line showed are acked now (returned once).
    pub fn presented(&mut self, now: Instant) -> Vec<(HostKey, u32)> {
        if self.shown.is_none() || self.hold_until.is_some() {
            return Vec::new();
        }
        self.hold_until = Some(now + NOT_ENDED_EXIT_HOLD);
        std::mem::take(&mut self.acks)
    }

    /// A line is owed a frame that has not presented it yet.
    pub fn owes_frame(&self) -> bool {
        self.shown.is_some() && self.hold_until.is_none()
    }

    /// The status line while leaving: what the outcomes left to show (one
    /// line or two), once they are in, otherwise `closing…` (a handover has
    /// none).
    pub fn line(&self) -> Option<String> {
        match (&self.shown, self.intent) {
            (Some((line, _)), _) => Some(line.clone()),
            (None, LeaveIntent::Handover) => None,
            (None, _) => Some("closing…".to_string()),
        }
    }
}

/// The text for the standings, or None when closing will end the sessions
/// (or when it is too early to know).
pub fn lease_notice(exempt: bool, standings: &[Standing]) -> Option<&'static str> {
    if exempt
        || standings.iter().any(|s| matches!(s, Standing::Granted { .. } | Standing::Pending))
    {
        return None;
    }
    if standings.contains(&Standing::Undetermined) {
        Some(NOTICE_UNDETERMINED)
    } else if standings.contains(&Standing::Unsupported) {
        Some(NOTICE_UNSUPPORTED)
    } else {
        Some(NOTICE_NO_BACKEND)
    }
}

/// What a finished leave shows, acks and warns.
#[derive(Debug, Default, PartialEq, Eq)]
struct LeaveReport {
    line: Option<String>,
    acks: Vec<(HostKey, u32)>,
    warn: Vec<(HostKey, LeaveOutcome)>,
}

/// The sum of the counts the replies carried shows (each daemon reports its
/// own in one reply), and each nonzero count is acked once a frame has drawn
/// the line whole. A failure, an invalid reply
/// or no reply is warned and, but for a handover (the relaunched window's own
/// lease shows its standing), adds its intent's line below the count.
fn leave_report(intent: LeaveIntent, outcomes: &[(HostKey, LeaveOutcome)]) -> LeaveReport {
    let mut report = LeaveReport::default();
    for (host, outcome) in outcomes {
        match outcome {
            LeaveOutcome::Replied(0) => {}
            LeaveOutcome::Replied(n) => report.acks.push((host.clone(), *n)),
            _ => report.warn.push((host.clone(), outcome.clone())),
        }
    }
    let unconfirmed = match intent {
        _ if report.warn.is_empty() => None,
        LeaveIntent::Close => Some(LEAVE_UNCONFIRMED_CLOSE),
        LeaveIntent::Keep => Some(LEAVE_UNCONFIRMED_KEEP),
        LeaveIntent::Handover => None,
    };
    let count = not_ended_line(report.acks.iter().map(|(_, n)| *n).sum());
    let lines: Vec<String> = count.into_iter().chain(unconfirmed.map(str::to_string)).collect();
    report.line = (!lines.is_empty()).then(|| lines.join("\n"));
    report
}

/// A leave's reply: its count, or a failure when the payload carries none
/// (an error, or a count missing or unparsable), never a zero.
fn leave_reply(payload: &serde_json::Value) -> LeaveOutcome {
    match payload.get("not_ended").and_then(serde_json::Value::as_u64).and_then(|n| u32::try_from(n).ok()) {
        Some(n) => LeaveOutcome::Replied(n),
        None => LeaveOutcome::Failed(format!("invalid reply: {payload}")),
    }
}

/// What the status line says about sessions an earlier close could not end.
pub fn not_ended_line(n: u32) -> Option<String> {
    match n {
        0 => None,
        1 => Some("1 session could not be ended and is still running".to_string()),
        n => Some(format!("{n} sessions could not be ended and are still running")),
    }
}

/// Own the stream for the process's life. A reader task forwards frames into
/// a channel so the holder's `select!` never cancels a read mid-line. The
/// holder's end aborts the reader, so the whole stream closes: the halves of
/// a Windows pipe share one handle, which the write half's drop alone leaves
/// open.
fn spawn_holder<R, W>(mut rx: R, mut tx: W) -> mpsc::UnboundedSender<HolderCmd>
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<HolderCmd>();
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Frame>();
    let reader = tokio::spawn(async move {
        while let Ok((f, _)) = codec::read_frame(&mut rx).await {
            if frame_tx.send(f).is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        // The hello took id 1 and the lease id 2.
        let mut next_id: u64 = 3;
        // The one table of awaited replies, by request id. A second leave (an
        // X during a Keep) is written at once, while the first still awaits
        // its reply. Nothing awaits `fe.notice_seen`'s reply, which matches no
        // entry and is dropped like any stray frame.
        let mut leaves: HashMap<u64, oneshot::Sender<LeaveOutcome>> = HashMap::new();
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    let id = next_id;
                    next_id += 1;
                    match cmd {
                        HolderCmd::Leave(intent, ack) => {
                            let frame = Frame::req(id, op::FE_LEAVING, serde_json::json!(FeLeavingReq { intent }));
                            if let Err(e) = codec::write_frame(&mut tx, &frame, None).await {
                                let _ = ack.send(LeaveOutcome::Failed(format!("write: {e:#}")));
                                break;
                            }
                            leaves.insert(id, ack);
                        }
                        HolderCmd::NoticeSeen(n) => {
                            let frame = Frame::req(id, op::FE_NOTICE_SEEN, serde_json::json!(FeNoticeSeenReq { not_ended: n }));
                            if codec::write_frame(&mut tx, &frame, None).await.is_err() {
                                break;
                            }
                        }
                        HolderCmd::Written(done) => {
                            let _ = done.send(());
                        }
                    }
                }
                f = frame_rx.recv() => {
                    let Some(f) = f else { break };
                    if let Some(ack) = leaves.remove(&f.id) {
                        let _ = ack.send(leave_reply(&f.payload));
                    }
                }
            }
        }
        reader.abort();
        for (_, ack) in leaves {
            let _ = ack.send(LeaveOutcome::Failed("the stream ended before the reply".to_string()));
        }
    });
    cmd_tx
}

/// Why the window is being asked to exit.
#[derive(Clone, Copy)]
pub(crate) enum ExitReason {
    WindowClose,
    QuitKey,
    Relaunch(i32),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ExitStep {
    Ask,
    Leave { intent: LeaveIntent, code: i32 },
    Supersede,
    Now { code: i32 },
    Ignore,
}

/// What an exit request does. A window already leaving exits at once, with
/// 0, on a second close, except that an X or OS close during a Keep
/// supersedes it with a Close: the user's latest intent wins, so a second
/// close during a Handover never relaunches. A relaunch then defers to the
/// leave in progress.
pub(crate) fn exit_intent(reason: ExitReason, leaving: Option<LeaveIntent>) -> ExitStep {
    match (reason, leaving) {
        (ExitReason::WindowClose, None) => ExitStep::Leave { intent: LeaveIntent::Close, code: 0 },
        (ExitReason::QuitKey, None) => ExitStep::Ask,
        (ExitReason::Relaunch(code), None) => ExitStep::Leave { intent: LeaveIntent::Handover, code },
        (ExitReason::WindowClose, Some(LeaveIntent::Keep)) => ExitStep::Supersede,
        (ExitReason::WindowClose | ExitReason::QuitKey, Some(_)) => ExitStep::Now { code: 0 },
        (ExitReason::Relaunch(_), Some(_)) => ExitStep::Ignore,
    }
}

/// A second close exits at once with `code` (0), and the leave in progress
/// takes that code: winit still runs `about_to_wait` while it shuts down, and
/// its poll exits with the leave's own code, so no restart code outlives a
/// close.
pub(crate) fn close_now(leaving: Option<&mut crate::lease::Leaving>, code: i32) -> i32 {
    if let Some(l) = leaving {
        l.exit_code = code;
    }
    code
}

#[cfg(test)]
#[path = "lease_grant_tests.rs"]
pub(crate) mod grant_tests;

#[cfg(test)]
#[path = "lease_leave_tests.rs"]
pub(crate) mod leave_tests;

#[cfg(test)]
mod delivery_tests {
    use super::*;
    use sot_log::test_isolated::run_isolated;

    #[test]
    fn held_book_does_not_extend_forced_delivery() {
        if !run_isolated("lease::delivery_tests::held_book_does_not_extend_forced_delivery") { return; }
        let leases = Leases::new(true, vec![]);
        let held = leases.clone();
        let (ready, entered) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let owner = std::thread::spawn(move || {
            let _book = held.book.lock().unwrap();
            ready.send(()).unwrap();
            let _ = gate.recv_timeout(Duration::from_millis(2400));
        });
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let log = sot_log::test_log::capture();
        let start = Instant::now();
        leases.deliver_queued(LEAVE_WRITE_WAIT);
        let elapsed = start.elapsed();
        let _ = release.send(());
        owner.join().unwrap();
        assert!(elapsed < Duration::from_millis(1500), "Book acquisition exceeded total deadline: {elapsed:?}");
        assert!(log.text().contains("lease book stayed locked"), "missing lock-contention warning");
        println!("forced delivery: held Book returned within 1.5 seconds");
    }

    #[test]
    fn frozen_worker_and_multiple_holders_share_one_deadline() {
        if !run_isolated("lease::delivery_tests::frozen_worker_and_multiple_holders_share_one_deadline") { return; }
        println!("T1 body entered: lease::delivery_tests::frozen_worker_and_multiple_holders_share_one_deadline");
        use super::{grant_tests::bind, leave_tests::{leave_fake, logged, is_leave, finish}};
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let leases = Leases::new(false, vec![]);
        let mut peers = Vec::new();
        runtime.block_on(async {
            for host in ["pending-a", "pending-b"] {
                let (listener, path) = bind("forcedclock");
                let (log, _, peer) = leave_fake(listener, None, None, Duration::ZERO);
                tokio::time::timeout(Duration::from_secs(5), leases.before_data_connection(&host.to_string(), &path, None)).await.unwrap().unwrap();
                peers.push((log, peer));
            }
        });
        let fast = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let fast_peer = fast.block_on(async {
            let (listener, path) = bind("forcedready");
            let peer = leave_fake(listener, None, None, Duration::ZERO);
            tokio::time::timeout(Duration::from_secs(5), leases.before_data_connection(&"ready".to_string(), &path, None)).await.unwrap().unwrap();
            peer
        });
        let (closed, rx) = mpsc::unbounded_channel();
        drop(rx);
        leases.set(&"closed".to_string(), Standing::Granted { state_root: None }, Some(closed));
        let (ready, entered) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel();
        runtime.spawn(async move {
            ready.send(()).unwrap();
            let _ = gate.recv_timeout(Duration::from_secs(3));
        });
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        println!("T1 fixture observed: frozen worker and responsive mixed holders");
        let keep = leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).unwrap();
        let close = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
        let log = sot_log::test_log::capture();
        let start = Instant::now();
        leases.deliver_queued(LEAVE_WRITE_WAIT);
        let elapsed = start.elapsed();
        let _ = release.send(());
        runtime.block_on(async {
            for (log, _) in &peers {
                let seen = logged(log, 2).await;
                assert!(is_leave(&seen[0], "keep") && is_leave(&seen[1], "close"));
            }
        });
        drop((keep, close, leases));
        runtime.block_on(async { for (log, peer) in peers { finish(peer, &log).await; } });
        fast.block_on(async { finish(fast_peer.2, &fast_peer.0).await; });
        runtime.shutdown_timeout(LEAVE_WRITE_WAIT);
        fast.shutdown_timeout(LEAVE_WRITE_WAIT);
        assert!(elapsed < Duration::from_millis(1500), "transport worker extended forced delivery: {elapsed:?}");
        let text = log.text();
        assert!(text.contains("pending-a") && text.contains("pending-b") && text.contains("closed"), "unconfirmed holders missing: {text}");
        assert!(!text.contains("ready"), "written holder was reported unconfirmed: {text}");
        assert_eq!(text.matches("exiting before every queued leave was written").count(), 1);
        println!("forced delivery: frozen worker and mixed holders returned within 1.5 seconds");
        println!("T1 assertion passed: transport worker extended forced delivery:");
    }
}
