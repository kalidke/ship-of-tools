// The window lease (ADR 0050): one dedicated connection per local daemon,
// held for the window's life, so the daemon knows a window is attached and
// the window can say, on leaving, what should happen to the computer's
// sessions. This file is the lease's whole client side; `transport.rs` calls
// `before_data_connection` before each local data connection, and `gpu.rs`
// reads `notice()` and (later) leaves through it.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use interprocess::local_socket::tokio::prelude::*;
use sot_log::challenge::self_identity;
use sot_protocol::ops::{lease, op, FeLeaseReq, FeLeaseRes, FeLeavingReq, FeNoticeSeenReq, LeaveIntent, LeaseOutcome};
use sot_protocol::{codec, Frame};
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio::sync::oneshot::error::TryRecvError;

use crate::dial::HostKey;
use crate::transport::{connect_pipe, Dial, TransportConfig};

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
    /// The state root this label's last grant named (the daemon's issued
    /// identity); kept when a later handshake on the label fails.
    daemon: Option<String>,
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
    /// The runtime the holders run on, for a forced exit's wait
    /// (`deliver_queued`).
    rt: Option<tokio::runtime::Handle>,
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
            .map(|h| (h, Slot { standing: Standing::Pending, holder: None, daemon: None }))
            .collect();
        Arc::new(Self {
            exempt,
            book: Mutex::new(Book { slots, leaving: None, inflight: 0, late: None, rt: None }),
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
        let mut book = self.book.lock().unwrap();
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
        let daemon = match &standing {
            Standing::Granted { state_root: Some(r) } => Some(r.clone()),
            _ => book.slots.get(host).and_then(|s| s.daemon.clone()),
        };
        book.slots.insert(host.clone(), Slot { standing, holder, daemon });
    }

    /// Claim the daemon on a dedicated connection before a data connection is
    /// made. Returns the count of sessions an earlier close could not end.
    /// An `Err` is the transport's cue to back off and retry.
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
        // daemon. Once the request is written the reply is awaited for as long
        // as the connection lasts, since the daemon may still grant it and a
        // dropped granted connection departs as a Close.
        let sent = tokio::time::timeout(self.reply_wait, async {
            let stream = connect_pipe(path).await?;
            let (rx, mut tx) = stream.split();
            let rx = codec::buffered(rx);
            let frame = Frame::req(1, op::FE_LEASE, serde_json::to_value(&req)?);
            codec::write_frame(&mut tx, &frame, None).await?;
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
            let read = codec::read_frame(&mut rx);
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
            Ok((reply, _)) => reply,
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
                self.book.lock().unwrap().rt.get_or_insert_with(tokio::runtime::Handle::current);
                self.set(host, Standing::Granted { state_root: res.state_root }, Some(holder));
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

    /// The key a not-ended count is shown and acked under: the state root
    /// this label's grant named (one daemon holds one state root), else the
    /// label itself, since without an issued identity there is nothing to merge.
    pub fn daemon_key(&self, host: &HostKey) -> HostKey {
        let book = self.book.lock().unwrap();
        book.slots.get(host).and_then(|s| s.daemon.clone()).unwrap_or_else(|| host.clone())
    }

    /// Tell the daemon the not-ended line for `key` (a label or a daemon key)
    /// has been shown.
    pub fn notice_seen(&self, key: &HostKey, n: u32) {
        let book = self.book.lock().unwrap();
        for (host, slot) in book.slots.iter() {
            if host == key || slot.daemon.as_deref() == Some(key.as_str()) {
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
        let rt = self.book.lock().unwrap().rt.clone();
        if let Some(rt) = rt {
            rt.block_on(self.written(bound));
        }
    }

    /// `deliver_queued`'s wait. The hosts still unwritten at the bound are
    /// named in one warn line, and the exit goes on.
    async fn written(&self, bound: Duration) {
        let waits: Vec<(HostKey, oneshot::Receiver<()>)> = {
            let book = self.book.lock().unwrap();
            book.slots
                .iter()
                .filter_map(|(host, slot)| {
                    let (tx, rx) = oneshot::channel();
                    slot.holder.as_ref()?.send(HolderCmd::Written(tx)).ok()?;
                    Some((host.clone(), rx))
                })
                .collect()
        };
        let deadline = tokio::time::Instant::now() + bound;
        let mut unwritten = Vec::new();
        for (host, rx) in waits {
            if !matches!(tokio::time::timeout_at(deadline, rx).await, Ok(Ok(()))) {
                unwritten.push(host);
            }
        }
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
        let mut next_id: u64 = 2;
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

#[cfg(test)]
mod tests {
    use super::*;
    use interprocess::local_socket::{GenericFilePath, ListenerOptions};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn bind(tag: &str) -> (interprocess::local_socket::tokio::Listener, PathBuf) {
        let unique = format!(
            "sot-lease-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        #[cfg(windows)]
        let path = PathBuf::from(format!(r"\\.\pipe\{unique}"));
        #[cfg(not(windows))]
        let path = {
            // macOS's temp_dir() is long enough to overflow sun_path
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let _ = &unique;
            PathBuf::from(format!(
                "/tmp/sl-{tag}-{}-{}.sock",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ))
        };
        let _ = std::fs::remove_file(&path);
        let name = path.to_str().unwrap().to_fs_name::<GenericFilePath>().unwrap();
        let listener = ListenerOptions::new().name(name).create_tokio().expect("bind");
        (listener, path)
    }

    fn pipe_config(path: &Path) -> TransportConfig {
        TransportConfig { dial: Dial::Pipe(path.to_path_buf()), token: None }
    }

    #[test]
    fn harness_never_leases() {
        for bits in 0..8u8 {
            let (e, c, n) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
            assert_eq!(lease_exempt(e, c, n), bits != 0, "row {bits}");
        }
        let ssh = sot_protocol::ssh_bridge::SshRecipe::new("somehost", None).unwrap();
        let conns = vec![
            ("local".to_string(), pipe_config(Path::new("/x"))),
            ("far".to_string(), TransportConfig { dial: Dial::Ssh(ssh), token: None }),
        ];
        assert_eq!(pipe_hosts(&conns), vec!["local".to_string()]);

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (listener, path) = bind("exempt");
            let host = "local".to_string();
            let leases = Leases::new(true, vec![host.clone()]);
            assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
            let accepted = tokio::time::timeout(Duration::from_millis(300), listener.accept()).await;
            assert!(accepted.is_err(), "an exempt window must not connect");
            assert_eq!(leases.notice(), None);
        });
    }

    #[tokio::test]
    async fn lease_retried_each_data_connection() {
        let (listener, path) = bind("retry");
        let count = Arc::new(AtomicUsize::new(0));
        let (drop_tx, drop_rx) = oneshot::channel::<()>();
        let c = count.clone();
        tokio::spawn(async move {
            let mut drop_rx = Some(drop_rx);
            loop {
                let conn = listener.accept().await.unwrap();
                let n = c.fetch_add(1, Ordering::SeqCst) + 1;
                let (rx, mut tx) = conn.split();
                let mut rx = codec::buffered(rx);
                let (req, _) = codec::read_frame(&mut rx).await.unwrap();
                let payload = match n {
                    1 => serde_json::json!({"error": "unknown op: fe.lease"}),
                    2 => serde_json::json!({"outcome": "closing"}),
                    3 => {
                        // No reply, then the connection ends: a failed connect,
                        // reached at the end, not at the wait.
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(400)).await;
                            drop((rx, tx));
                        });
                        continue;
                    }
                    4 => serde_json::json!({"outcome": "foreign"}),
                    5 => serde_json::json!({"outcome": "undetermined"}),
                    _ => serde_json::json!({"outcome": "granted", "state_root": "r"}),
                };
                codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, payload), None)
                    .await
                    .unwrap();
                if n == 6 {
                    let rx_drop = drop_rx.take().unwrap();
                    tokio::spawn(async move {
                        let _ = rx_drop.await;
                        drop((rx, tx));
                    });
                } else if n > 6 {
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        drop((rx, tx));
                    });
                }
            }
        });
        let host = "local".to_string();
        let mut leases = Leases::new(false, vec![host.clone()]);
        Arc::get_mut(&mut leases).unwrap().reply_wait = Duration::from_millis(100);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(leases.standing(&host), Some(Standing::Unsupported));
        // Closing, and a connection that ends unanswered: each a failed connect.
        assert!(leases.before_data_connection(&host, &path, None).await.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert_eq!(leases.standing(&host), Some(Standing::Unreached));
        assert!(leases.before_data_connection(&host, &path, None).await.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 3);
        assert_eq!(leases.standing(&host), Some(Standing::Unreached));
        // Foreign and Undetermined proceed unleased, and the next connection asks again.
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        assert_eq!(count.load(Ordering::SeqCst), 4);
        assert_eq!(leases.standing(&host), Some(Standing::Foreign));
        assert!(!leases.held(&host));
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        assert_eq!(count.load(Ordering::SeqCst), 5);
        assert_eq!(leases.standing(&host), Some(Standing::Undetermined));
        assert!(!leases.held(&host));
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        assert_eq!(count.load(Ordering::SeqCst), 6);
        assert_eq!(leases.standing(&host), Some(Standing::Granted { state_root: Some("r".into()) }));
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        assert_eq!(count.load(Ordering::SeqCst), 6, "a held lease is not retaken");
        drop_tx.send(()).unwrap();
        for _ in 0..40 {
            if !leases.held(&host) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!leases.held(&host), "the dropped stream ends the lease");
        leases.before_data_connection(&host, &path, None).await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 7);
    }

    #[tokio::test]
    async fn notice_seen_timeout_keeps_the_lease() {
        let (listener, path) = bind("slowseen");
        let (eof_tx, eof_rx) = oneshot::channel::<bool>();
        tokio::spawn(async move {
            let conn = listener.accept().await.unwrap();
            let (rx, mut tx) = conn.split();
            let mut rx = codec::buffered(rx);
            let (req, _) = codec::read_frame(&mut rx).await.unwrap();
            let granted = serde_json::json!({"outcome": "granted"});
            codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, granted), None).await.unwrap();
            let (seen, _) = codec::read_frame(&mut rx).await.unwrap();
            assert_eq!(seen.op, op::FE_NOTICE_SEEN);
            // Never answer; report whether the window's end of the stream closes.
            let eof = tokio::time::timeout(Duration::from_millis(500), codec::read_frame(&mut rx)).await.is_ok();
            let _ = eof_tx.send(eof);
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(tx);
        });
        let host = "local".to_string();
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        leases.notice_seen(&host, 2);
        assert!(!eof_rx.await.unwrap(), "the fake daemon read an EOF after a slow notice_seen reply");
        assert!(leases.held(&host), "a slow notice_seen reply must not end the lease");
    }

    #[tokio::test]
    async fn lease_wire_round_trip() {
        let (listener, path) = bind("wire");
        let (seen_tx, seen_rx) = oneshot::channel::<Frame>();
        let (leaving_tx, leaving_rx) = oneshot::channel::<Frame>();
        tokio::spawn(async move {
            let conn = listener.accept().await.unwrap();
            let (rx, mut tx) = conn.split();
            let mut rx = codec::buffered(rx);
            let (req, _) = codec::read_frame(&mut rx).await.unwrap();
            assert_eq!(req.op, op::FE_LEASE);
            assert_eq!(req.id, 1);
            let me = self_identity().unwrap();
            assert_eq!(req.payload["boot"], serde_json::json!(me.boot));
            assert_eq!(req.payload["pid"], serde_json::json!(me.pid));
            assert_eq!(req.payload["created"], serde_json::json!(me.created));
            assert_eq!(req.payload["token"], serde_json::json!("tok"));
            let granted = serde_json::json!({"outcome": "granted", "state_root": "r", "not_ended": 1});
            codec::write_frame(&mut tx, &Frame::res(1, op::FE_LEASE, granted), None).await.unwrap();
            let (seen, _) = codec::read_frame(&mut rx).await.unwrap();
            codec::write_frame(&mut tx, &Frame::res(seen.id, op::FE_NOTICE_SEEN, serde_json::json!({})), None)
                .await
                .unwrap();
            let _ = seen_tx.send(seen);
            let (leaving, _) = codec::read_frame(&mut rx).await.unwrap();
            let _ = leaving_tx.send(leaving.clone());
            codec::write_frame(&mut tx, &Frame::res(leaving.id, op::FE_LEAVING, serde_json::json!({"not_ended": 3})), None)
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let host = "local".to_string();
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, Some("tok")).await.unwrap(), 1);
        assert_eq!(leases.granted_state_roots(), vec!["r".to_string()]);
        leases.notice_seen(&host, 1);
        let seen = tokio::time::timeout(Duration::from_secs(2), seen_rx).await.unwrap().unwrap();
        assert_eq!(seen.op, op::FE_NOTICE_SEEN);
        assert_eq!(seen.payload, serde_json::json!({"not_ended": 1}));
        let mut leaving = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(2), leaving_rx).await.unwrap().unwrap();
        assert_eq!(frame.op, op::FE_LEAVING);
        assert_eq!(frame.payload, serde_json::json!({"intent": "close"}));
        let mut step = leaving.poll(Instant::now());
        for _ in 0..40 {
            if !matches!(step, LeaveStep::Wait(_)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            step = leaving.poll(Instant::now());
        }
        assert_eq!(step, LeaveStep::Show);
        assert_eq!(leaving.line(), not_ended_line(3));
    }

    /// A daemon that grants (once `gate` has signalled the request and been
    /// opened, when given), then logs each frame the window writes, and
    /// `eof` the moment the stream ends, concurrently with its replies. It
    /// answers each leave whose intent is `reply_to`, after `delay`, with the
    /// sentinel `not_ended: 7`, and marks it replied only once that is written.
    fn leave_fake(
        listener: interprocess::local_socket::tokio::Listener,
        gate: Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>,
        reply_to: Option<&'static str>,
        delay: Duration,
    ) -> (Arc<std::sync::Mutex<Vec<String>>>, Arc<std::sync::atomic::AtomicBool>, tokio::task::JoinHandle<()>) {
        let log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let replied = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (l, r) = (log.clone(), replied.clone());
        let task = tokio::spawn(async move {
            let conn = listener.accept().await.unwrap();
            let (rx, mut tx) = conn.split();
            let mut rx = codec::buffered(rx);
            let (req, _) = codec::read_frame(&mut rx).await.unwrap();
            if let Some((asked, open)) = gate {
                asked.send(()).unwrap();
                open.await.unwrap();
            }
            let granted = serde_json::json!({"outcome": "granted"});
            codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, granted), None).await.unwrap();
            let (want_tx, mut want_rx) = mpsc::unbounded_channel::<u64>();
            let reader = async move {
                while let Ok((f, _)) = codec::read_frame(&mut rx).await {
                    l.lock().unwrap().push(format!("{} {}", f.op, f.payload));
                    if reply_to.is_some_and(|i| f.payload["intent"] == i) {
                        want_tx.send(f.id).unwrap();
                    }
                }
                l.lock().unwrap().push("eof".to_string());
            };
            let writer = async move {
                while let Some(id) = want_rx.recv().await {
                    tokio::time::sleep(delay).await;
                    let res = Frame::res(id, op::FE_LEAVING, serde_json::json!({"not_ended": 7}));
                    codec::write_frame(&mut tx, &res, None).await.unwrap();
                    r.store(true, Ordering::SeqCst);
                }
            };
            tokio::join!(reader, writer);
        });
        (log, replied, task)
    }

    /// Poll the way the event loop does until the step is not a wait (3 s at most).
    async fn poll_out(leaving: &mut Leaving) -> LeaveStep {
        for _ in 0..150 {
            let step = leaving.poll(Instant::now());
            if !matches!(step, LeaveStep::Wait(_)) {
                return step;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        leaving.poll(Instant::now())
    }

    /// The fake's log once it has `n` entries (1 s at most).
    async fn logged(log: &Arc<std::sync::Mutex<Vec<String>>>, n: usize) -> Vec<String> {
        for _ in 0..100 {
            if log.lock().unwrap().len() >= n {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        log.lock().unwrap().clone()
    }

    fn is_leave(entry: &str, intent: &str) -> bool {
        entry.starts_with(op::FE_LEAVING) && entry.contains(&format!(r#""intent":"{intent}""#))
    }

    /// Join the fake once the window's side is dropped (a panic in it fails
    /// the test); its whole log, which ends with `eof`.
    async fn finish(fake: tokio::task::JoinHandle<()>, log: &Arc<std::sync::Mutex<Vec<String>>>) -> Vec<String> {
        tokio::time::timeout(Duration::from_secs(5), fake).await.expect("the fake never saw eof").unwrap();
        let seen = log.lock().unwrap().clone();
        assert_eq!(seen.last().map(String::as_str), Some("eof"), "{seen:?}");
        seen
    }

    #[tokio::test]
    async fn keep_reaches_the_daemon_before_eof() {
        let (listener, path) = bind("keepleave");
        let (log, replied, fake) = leave_fake(listener, None, Some("keep"), Duration::from_millis(300));
        let host = "local".to_string();
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        let mut leaving = leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).unwrap();
        assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)));
        let seen = logged(&log, 1).await;
        assert!(seen.len() == 1 && is_leave(&seen[0], "keep"), "the leave line reaches the daemon before any eof: {seen:?}");
        assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "no reply yet");
        assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show);
        assert!(replied.load(Ordering::SeqCst), "done only after the reply");
        assert_eq!(leaving.line(), not_ended_line(7), "the window received the reply's own count");
        drop((leaving, leases));
        assert_eq!(finish(fake, &log).await.len(), 2);
    }

    #[tokio::test]
    async fn close_waits_for_its_reply() {
        let (listener, path) = bind("closeleave");
        let (log, replied, fake) = leave_fake(listener, None, Some("close"), Duration::from_millis(300));
        let host = "local".to_string();
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        let mut leaving = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
        assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)));
        let seen = logged(&log, 1).await;
        assert!(seen.len() == 1 && is_leave(&seen[0], "close"), "the leave line reaches the daemon before any eof: {seen:?}");
        assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "no reply yet");
        assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show);
        assert!(replied.load(Ordering::SeqCst), "done only after the reply");
        assert_eq!(leaving.line(), not_ended_line(7), "the window received the reply's own count");
        drop((leaving, leases));
        assert_eq!(finish(fake, &log).await.len(), 2);

        // A withheld reply ends the wait at the close-ack bound itself.
        let (listener, path) = bind("closenoreply");
        let (log, replied, fake) = leave_fake(listener, None, None, Duration::ZERO);
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        let t0 = Instant::now();
        let mut leaving = leases.leave_all(LeaveIntent::Close, 0, t0).unwrap();
        let seen = logged(&log, 1).await;
        assert!(seen.len() == 1 && is_leave(&seen[0], "close"), "{seen:?}");
        let ms = Duration::from_millis;
        assert!(matches!(leaving.poll(t0 + lease::CLOSE_ACK_WAIT - ms(1)), LeaveStep::Wait(_)), "waits inside the bound");
        assert_eq!(leaving.poll(t0 + lease::CLOSE_ACK_WAIT), LeaveStep::Show, "the bound ends the wait");
        assert_eq!(leaving.line().as_deref(), Some(LEAVE_UNCONFIRMED_CLOSE), "and says the close was not confirmed");
        assert!(!replied.load(Ordering::SeqCst));
        drop((leaving, leases));
        assert_eq!(finish(fake, &log).await.len(), 2);
    }

    #[tokio::test]
    async fn close_after_keep_supersedes() {
        let (listener, path) = bind("supersede");
        // The keep's reply is withheld; the close's comes after 300 ms.
        let (log, _replied, fake) = leave_fake(listener, None, Some("close"), Duration::from_millis(300));
        let host = "local".to_string();
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        let keep = leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).unwrap();
        assert_eq!(logged(&log, 1).await.len(), 1);
        // The X during the keep's ack wait (`exit_intent`'s Supersede).
        let mut leaving = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
        drop(keep);
        assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "the close waits for its reply");
        let seen = logged(&log, 2).await;
        assert!(seen.len() == 2 && is_leave(&seen[0], "keep") && is_leave(&seen[1], "close"), "keep, then close: {seen:?}");
        assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show);
        assert_eq!(leaving.line(), not_ended_line(7), "the close's own reply");
        drop((leaving, leases));
        assert_eq!(finish(fake, &log).await.len(), 3, "keep, close, then eof");
    }

    #[tokio::test]
    async fn late_grant_gets_the_leave() {
        let (la, pa) = bind("latea");
        let (lb, pb) = bind("lateb");
        let (lc, pc) = bind("latec");
        let (log_a, replied_a, fake_a) = leave_fake(la, None, Some("keep"), Duration::ZERO);
        let (asked_tx, asked) = oneshot::channel();
        let (open, open_rx) = oneshot::channel();
        let (log_b, _, fake_b) = leave_fake(lb, Some((asked_tx, open_rx)), Some("keep"), Duration::ZERO);
        let (a, b, c) = ("a".to_string(), "b".to_string(), "c".to_string());
        let mut leases = Leases::new(false, vec![a.clone(), b.clone(), c.clone()]);
        Arc::get_mut(&mut leases).unwrap().reply_wait = Duration::from_secs(2);
        assert_eq!(leases.before_data_connection(&a, &pa, None).await.unwrap(), 0);
        let in_flight = {
            let leases = leases.clone();
            tokio::spawn(async move { leases.before_data_connection(&b, &pb, None).await })
        };
        asked.await.unwrap();
        let mut leaving = leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).unwrap();
        // Polled before the grant, once a's reply is in.
        answered(&replied_a).await;
        assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "the handshake in flight holds the leave");
        open.send(()).unwrap();
        assert_eq!(in_flight.await.unwrap().unwrap(), 0);
        // A handshake started after the leave is refused, and never connects.
        assert!(leases.before_data_connection(&c, &pc, None).await.is_err());
        assert!(tokio::time::timeout(Duration::from_millis(200), lc.accept()).await.is_err(), "a lease after the leave");
        // Both acks are in the one wait: 7 from each, and their sum shows.
        assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show);
        assert_eq!(leaving.line(), not_ended_line(14));
        drop((leaving, leases));
        let seen_b = finish(fake_b, &log_b).await;
        assert!(seen_b.len() == 2 && is_leave(&seen_b[0], "keep"), "the late lease got the keep before eof: {seen_b:?}");
        assert_eq!(finish(fake_a, &log_a).await.len(), 2);
    }

    /// Until a fake has written its reply, then a moment for the holder to
    /// pass it on.
    async fn answered(replied: &std::sync::atomic::AtomicBool) {
        for _ in 0..100 {
            if replied.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    /// A daemon that grants, answers the first leave with `payload` under the
    /// leave's id plus `shift`, then reads until EOF.
    fn odd_reply_fake(
        listener: interprocess::local_socket::tokio::Listener,
        shift: u64,
        payload: serde_json::Value,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let conn = listener.accept().await.unwrap();
            let (rx, mut tx) = conn.split();
            let mut rx = codec::buffered(rx);
            let (req, _) = codec::read_frame(&mut rx).await.unwrap();
            let granted = serde_json::json!({"outcome": "granted"});
            codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, granted), None).await.unwrap();
            let (leave, _) = codec::read_frame(&mut rx).await.unwrap();
            codec::write_frame(&mut tx, &Frame::res(leave.id + shift, op::FE_LEAVING, payload), None).await.unwrap();
            while codec::read_frame(&mut rx).await.is_ok() {}
        })
    }

    #[tokio::test]
    async fn slow_grant_is_held_not_dropped() {
        let (listener, path) = bind("slowgrant");
        let accepts = Arc::new(AtomicUsize::new(0));
        let eof = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (a, e) = (accepts.clone(), eof.clone());
        tokio::spawn(async move {
            loop {
                let conn = listener.accept().await.unwrap();
                a.fetch_add(1, Ordering::SeqCst);
                let e = e.clone();
                tokio::spawn(async move {
                    let (rx, mut tx) = conn.split();
                    let mut rx = codec::buffered(rx);
                    let (req, _) = codec::read_frame(&mut rx).await.unwrap();
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    let granted = serde_json::json!({"outcome": "granted", "state_root": "r", "not_ended": 2});
                    codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, granted), None).await.unwrap();
                    if tokio::time::timeout(Duration::from_millis(500), codec::read_frame(&mut rx)).await.is_ok() {
                        e.store(true, Ordering::SeqCst);
                    }
                });
            }
        });
        let host = "local".to_string();
        let mut leases = Leases::new(false, vec![host.clone()]);
        Arc::get_mut(&mut leases).unwrap().reply_wait = Duration::from_millis(100);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 2);
        assert!(leases.held(&host));
        assert_eq!(accepts.load(Ordering::SeqCst), 1);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(!eof.load(Ordering::SeqCst), "a granted lease's connection was dropped");
    }

    #[tokio::test]
    async fn one_daemon_under_two_labels_is_one_count() {
        // A fake daemon that grants every connection `root` and `n`, logging each later frame.
        fn daemon(
            listener: interprocess::local_socket::tokio::Listener,
            root: &'static str,
            n: u32,
        ) -> (Arc<std::sync::Mutex<Vec<String>>>, Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>) {
            let log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let conns = Arc::new(std::sync::Mutex::new(Vec::new()));
            let (l, c) = (log.clone(), conns.clone());
            tokio::spawn(async move {
                loop {
                    let conn = listener.accept().await.unwrap();
                    let l = l.clone();
                    let task = tokio::spawn(async move {
                        let (rx, mut tx) = conn.split();
                        let mut rx = codec::buffered(rx);
                        let (req, _) = codec::read_frame(&mut rx).await.unwrap();
                        let granted = serde_json::json!({"outcome": "granted", "state_root": root, "not_ended": n});
                        codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, granted), None).await.unwrap();
                        while let Ok((f, _)) = codec::read_frame(&mut rx).await {
                            l.lock().unwrap().push(f.op.to_string());
                        }
                    });
                    c.lock().unwrap().push(task.abort_handle());
                }
            });
            (log, conns)
        }
        let (l1, p1) = bind("twolabels1");
        let (l2, p2) = bind("twolabels2");
        let (log1, conns1) = daemon(l1, "r", 3);
        let (_log2, _conns2) = daemon(l2, "s", 4);
        let (a, b, c) = ("a".to_string(), "b".to_string(), "c".to_string());
        let leases = Leases::new(false, vec![a.clone(), b.clone(), c.clone()]);
        assert_eq!(leases.before_data_connection(&a, &p1, None).await.unwrap(), 3);
        assert_eq!(leases.before_data_connection(&b, &p1, None).await.unwrap(), 3);
        assert_eq!(leases.before_data_connection(&c, &p2, None).await.unwrap(), 4);
        assert_eq!(leases.daemon_key(&a), leases.daemon_key(&b), "two labels of one daemon must share its key");
        assert_ne!(leases.daemon_key(&a), leases.daemon_key(&c));
        // What gpu.rs does with the queued counts: the newest per key, summed.
        let queued = [(leases.daemon_key(&a), 3), (leases.daemon_key(&b), 3), (leases.daemon_key(&c), 4)];
        let mut newest: Vec<(HostKey, u32)> = Vec::new();
        for (k, n) in queued {
            match newest.iter_mut().find(|(h, _)| *h == k) {
                Some(slot) => slot.1 = n,
                None => newest.push((k, n)),
            }
        }
        assert_eq!(newest.iter().map(|(_, n)| n).sum::<u32>(), 7);
        // End a's holder, then fail a re-handshake: the identity stays.
        let key = leases.daemon_key(&a);
        conns1.lock().unwrap()[0].abort();
        for _ in 0..40 {
            if !leases.held(&a) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!leases.held(&a));
        assert!(leases.before_data_connection(&a, Path::new("/nonexistent/sot-lease-dead.sock"), None).await.is_err());
        assert_eq!(leases.daemon_key(&a), key, "a failed re-handshake lost the daemon's identity");
        leases.notice_seen(&leases.daemon_key(&b), 3);
        for _ in 0..40 {
            if log1.lock().unwrap().iter().any(|o| o == op::FE_NOTICE_SEEN) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(log1.lock().unwrap().iter().any(|o| o == op::FE_NOTICE_SEEN), "{:?}", log1.lock().unwrap());
    }

    #[tokio::test]
    async fn inflight_handshake_gets_the_leave() {
        // One host, its handshake still in flight when the window leaves.
        let (listener, path) = bind("inflight");
        let (asked_tx, asked) = oneshot::channel();
        let (open, open_rx) = oneshot::channel();
        let (log, _, fake) = leave_fake(listener, Some((asked_tx, open_rx)), Some("keep"), Duration::ZERO);
        let host = "local".to_string();
        let leases = Leases::new(false, vec![host.clone()]);
        let in_flight = {
            let (leases, host) = (leases.clone(), host.clone());
            tokio::spawn(async move { leases.before_data_connection(&host, &path, None).await })
        };
        asked.await.unwrap();
        let mut leaving =
            leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).expect("a handshake in flight is a lease to leave");
        assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "polled before the grant");
        open.send(()).unwrap();
        assert_eq!(in_flight.await.unwrap().unwrap(), 0);
        assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show);
        assert_eq!(leaving.line(), not_ended_line(7));
        drop((leaving, leases));
        let seen = finish(fake, &log).await;
        assert!(seen.len() == 2 && is_leave(&seen[0], "keep"), "the keep reached the lease before eof: {seen:?}");

        // Two hosts: a granted one acks at once, the other is in flight.
        let (la, pa) = bind("inflighta");
        let (lb, pb) = bind("inflightb");
        let (log_a, replied_a, fake_a) = leave_fake(la, None, Some("close"), Duration::ZERO);
        let (asked_tx, asked) = oneshot::channel();
        let (open, open_rx) = oneshot::channel();
        let (log_b, _, fake_b) = leave_fake(lb, Some((asked_tx, open_rx)), Some("close"), Duration::ZERO);
        let (a, b) = ("a".to_string(), "b".to_string());
        let leases = Leases::new(false, vec![a.clone(), b.clone()]);
        assert_eq!(leases.before_data_connection(&a, &pa, None).await.unwrap(), 0);
        let in_flight = {
            let leases = leases.clone();
            tokio::spawn(async move { leases.before_data_connection(&b, &pb, None).await })
        };
        asked.await.unwrap();
        let mut leaving = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
        answered(&replied_a).await;
        assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "a's reply is in; the leave waits for b");
        open.send(()).unwrap();
        assert_eq!(in_flight.await.unwrap().unwrap(), 0);
        assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show);
        drop((leaving, leases));
        let seen_b = finish(fake_b, &log_b).await;
        assert!(seen_b.len() == 2 && is_leave(&seen_b[0], "close"), "b got the close before eof: {seen_b:?}");
        assert_eq!(finish(fake_a, &log_a).await.len(), 2);
    }

    #[tokio::test]
    async fn failed_leave_send_is_not_confirmed() {
        // a and b reply 7 each; c's handshake ends with a holder already gone,
        // so the leave cannot be sent to it; b, in flight, keeps the leave open.
        let (la, pa) = bind("deadsenda");
        let (lb, pb) = bind("deadsendb");
        let (log_a, _, fake_a) = leave_fake(la, None, Some("keep"), Duration::ZERO);
        let (asked_tx, asked) = oneshot::channel();
        let (open, open_rx) = oneshot::channel();
        let (log_b, _, fake_b) = leave_fake(lb, Some((asked_tx, open_rx)), Some("keep"), Duration::ZERO);
        let (a, b, c) = ("a".to_string(), "b".to_string(), "c".to_string());
        let leases = Leases::new(false, vec![a.clone(), b.clone(), c.clone()]);
        assert_eq!(leases.before_data_connection(&a, &pa, None).await.unwrap(), 0);
        let in_flight = {
            let leases = leases.clone();
            tokio::spawn(async move { leases.before_data_connection(&b, &pb, None).await })
        };
        asked.await.unwrap();
        let mut leaving = leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).unwrap();
        let (dead, _) = mpsc::unbounded_channel::<HolderCmd>();
        leases.set(&c, Standing::Granted { state_root: None }, Some(dead));
        open.send(()).unwrap();
        assert_eq!(in_flight.await.unwrap().unwrap(), 0);
        assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show);
        let line = leaving.line().unwrap();
        assert!(line.contains(LEAVE_UNCONFIRMED_KEEP) && line.contains(&not_ended_line(14).unwrap()), "{line}");
        drop((leaving, leases));
        assert_eq!(finish(fake_a, &log_a).await.len(), 2);
        assert_eq!(finish(fake_b, &log_b).await.len(), 2);
    }

    #[tokio::test]
    async fn inflight_handshake_ended_unanswered_is_not_confirmed() {
        // The daemon never answers the lease, and its connection ends after
        // the window began to close.
        let (listener, path) = bind("inflightnogrant");
        let (asked_tx, asked) = oneshot::channel();
        let (open, open_rx) = oneshot::channel();
        let (_, _, fake) = leave_fake(listener, Some((asked_tx, open_rx)), Some("close"), Duration::ZERO);
        let host = "local".to_string();
        let mut leases = Leases::new(false, vec![host.clone()]);
        Arc::get_mut(&mut leases).unwrap().reply_wait = Duration::from_millis(300);
        let in_flight = {
            let (leases, host) = (leases.clone(), host.clone());
            tokio::spawn(async move { leases.before_data_connection(&host, &path, None).await })
        };
        asked.await.unwrap();
        let mut leaving = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
        fake.abort();
        assert!(in_flight.await.unwrap().is_err(), "the connection ended unanswered");
        assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show, "a handshake in flight yields an outcome");
        assert_eq!(leaving.line().as_deref(), Some(LEAVE_UNCONFIRMED_CLOSE));
        drop(open);
    }

    #[test]
    fn forced_exit_delivers_the_queued_close() {
        // The window's runtime runs only inside its `block_on`s, as if the
        // exit came before the holder's next turn; dropping it is the exit.
        let daemon = tokio::runtime::Runtime::new().unwrap();
        let window = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let (log, _, fake, path) = daemon.block_on(async {
            let (listener, path) = bind("forcedexit");
            let (log, replied, fake) = leave_fake(listener, None, Some("keep"), Duration::ZERO);
            (log, replied, fake, path)
        });
        let host = "local".to_string();
        let leases = Leases::new(false, vec![host.clone()]);
        window.block_on(async {
            assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
            let mut keep = leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).unwrap();
            assert_eq!(poll_out(&mut keep).await, LeaveStep::Show, "the daemon accepted the keep");
        });
        // An X queues a Close; a second X exits at once.
        let _close = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
        window.block_on(leases.written(LEAVE_WRITE_WAIT));
        // The exit closes every handle, not just the runtime: on Windows the
        // pipe's halves share one handle, which stays open while any owner lives.
        drop(window);
        drop(_close);
        drop(leases);
        let seen = daemon.block_on(finish(fake, &log));
        assert!(
            seen.len() == 3 && is_leave(&seen[0], "keep") && is_leave(&seen[1], "close"),
            "the queued close reaches the daemon before eof: {seen:?}"
        );
    }

    #[tokio::test]
    async fn invalid_leave_reply_is_not_confirmed() {
        let host = "local".to_string();
        // Unparsable, an error, no count: each is a failure at once, never a 0.
        let bad = [
            serde_json::json!({"not_ended": "invalid"}),
            serde_json::json!({"error": "unknown op: fe.leaving"}),
            serde_json::json!({}),
        ];
        for (i, payload) in bad.into_iter().enumerate() {
            let (listener, path) = bind(&format!("badreply{i}"));
            let fake = odd_reply_fake(listener, 0, payload.clone());
            let leases = Leases::new(false, vec![host.clone()]);
            assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
            let mut leaving = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
            assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show, "{payload}");
            assert_eq!(leaving.line().as_deref(), Some(LEAVE_UNCONFIRMED_CLOSE), "{payload}");
            drop((leaving, leases));
            tokio::time::timeout(Duration::from_secs(5), fake).await.unwrap().unwrap();
        }
        // A reply under another id answers no leave: not confirmed at the bound.
        let (listener, path) = bind("wrongid");
        let fake = odd_reply_fake(listener, 100, serde_json::json!({"not_ended": 7}));
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        let t0 = Instant::now();
        let mut leaving = leases.leave_all(LeaveIntent::Close, 0, t0).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "a stray reply answers nothing");
        assert_eq!(leaving.poll(t0 + lease::CLOSE_ACK_WAIT), LeaveStep::Show);
        assert_eq!(leaving.line().as_deref(), Some(LEAVE_UNCONFIRMED_CLOSE));
        drop((leaving, leases));
        tokio::time::timeout(Duration::from_secs(5), fake).await.unwrap().unwrap();
    }

    #[test]
    fn mixed_outcomes_show_the_sum() {
        let t0 = Instant::now();
        let (a, b, c) = ("a".to_string(), "b".to_string(), "c".to_string());
        let (ta, ra) = oneshot::channel();
        let (tb, rb) = oneshot::channel::<LeaveOutcome>();
        let (tc, rc) = oneshot::channel();
        let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(a.clone(), ra), (b, rb), (c.clone(), rc)], t0);
        ta.send(LeaveOutcome::Replied(7)).unwrap();
        drop(tb);
        tc.send(LeaveOutcome::Replied(3)).unwrap();
        assert_eq!(l.poll(t0), LeaveStep::Show);
        // Both lines: the sum of the counts, hidden by neither the failure
        // nor the other count, then the failure's own line.
        assert_eq!(l.line(), Some(format!("{}\n{LEAVE_UNCONFIRMED_CLOSE}", not_ended_line(10).unwrap())));
        // Each count is acked once a frame draws the line whole.
        assert_eq!(l.presented(t0), vec![(a.clone(), 7), (c.clone(), 3)]);

        // Two daemons reply 7 and 3: the line shows 10, and both are acked.
        let (ta, ra) = oneshot::channel();
        let (tc, rc) = oneshot::channel();
        let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(a.clone(), ra), (c.clone(), rc)], t0);
        ta.send(LeaveOutcome::Replied(7)).unwrap();
        tc.send(LeaveOutcome::Replied(3)).unwrap();
        assert_eq!(l.poll(t0), LeaveStep::Show);
        assert_eq!(l.line(), not_ended_line(10));
        assert_eq!(l.presented(t0), vec![(a, 7), (c, 3)]);
    }

    #[tokio::test]
    async fn notice_seen_reply_never_swallows_a_handover_reply() {
        let (listener, path) = bind("seenhandover");
        let (log, replied, fake) = leave_fake(listener, None, Some("handover"), Duration::from_millis(200));
        let host = "local".to_string();
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        // A relaunch; then the owed count's ack goes out while the handover
        // awaits its reply (the fake never answers the ack).
        let mut leaving = leases.leave_all(LeaveIntent::Handover, 75, Instant::now()).unwrap();
        leases.notice_seen(&host, 1);
        assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show, "the handover's own reply");
        assert!(replied.load(Ordering::SeqCst));
        assert_eq!(leaving.line(), not_ended_line(7));
        drop((leaving, leases));
        let seen = finish(fake, &log).await;
        assert!(seen.len() == 3 && is_leave(&seen[0], "handover") && seen[1].starts_with(op::FE_NOTICE_SEEN), "{seen:?}");
    }

    #[tokio::test]
    async fn ended_lease_gives_eof_while_window_lives() {
        // The window drops its lease and runs on: the daemon reads EOF
        // within the bound, on Windows too, where a pipe's halves share one
        // handle.
        let (listener, path) = bind("endeof");
        let (log, _, fake) = leave_fake(listener, None, None, Duration::ZERO);
        let host = "local".to_string();
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        drop(leases);
        assert_eq!(finish(fake, &log).await, vec!["eof".to_string()]);
    }

    #[tokio::test]
    async fn spawned_child_does_not_hold_the_lease() {
        // After the grant, a child spawned through the window's std `Command`
        // path (`open_url_in_browser`); then every owner of the lease stream
        // goes, as when the window is killed. The daemon reads EOF while the
        // child still runs: no child inherits the lease handle.
        let (listener, path) = bind("child");
        let (log, _, fake) = leave_fake(listener, None, None, Duration::ZERO);
        let host = "local".to_string();
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        #[cfg(windows)]
        let mut cmd = std::process::Command::new("ping");
        #[cfg(windows)]
        cmd.args(["-n", "60", "127.0.0.1"]);
        #[cfg(not(windows))]
        let mut cmd = std::process::Command::new("sleep");
        #[cfg(not(windows))]
        cmd.arg("60");
        let mut child = cmd.stdout(std::process::Stdio::null()).spawn().unwrap();
        drop(leases);
        let eof = tokio::time::timeout(Duration::from_secs(5), fake).await;
        let alive = child.try_wait().unwrap().is_none();
        let _ = child.kill();
        let _ = child.wait();
        assert!(eof.is_ok(), "a child holds the killed window's lease: the fake never saw eof");
        assert!(alive, "the child must still run when the daemon reads eof");
        assert_eq!(log.lock().unwrap().clone(), vec!["eof".to_string()]);
    }

    // PIN, NOT FAIL-FIRST: interprocess 2.4.2 already creates the socket
    // SOCK_CLOEXEC (os/unix/c_wrappers.rs:177), so this passed on its first
    // run. On Windows the pipe client handle is non-inheritable because
    // CreateFileW is given null security attributes
    // (os/windows/named_pipe/c_wrappers.rs:158-165); there is no Windows half.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn lease_stream_not_inherited() {
        fn sockets() -> std::collections::HashSet<String> {
            std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
                .map(|p| p.to_string_lossy().into_owned())
                .filter(|p| p.starts_with("socket:"))
                .collect()
        }
        let (_listener, path) = bind("cloexec");
        let before = sockets();
        let _stream = connect_pipe(&path).await.unwrap();
        let ours: Vec<String> = sockets().difference(&before).cloned().collect();
        assert!(!ours.is_empty(), "the connect must have opened a socket");
        let out = std::process::Command::new("sh")
            .args(["-c", "for f in /proc/self/fd/*; do readlink $f; done"])
            .output()
            .unwrap();
        let seen = String::from_utf8_lossy(&out.stdout);
        for s in &ours {
            assert!(!seen.contains(s.as_str()), "{s} leaked into a child");
        }
    }

    #[test]
    fn no_lease_status_line() {
        use Standing::*;
        let granted = || Granted { state_root: None };
        assert_eq!(lease_notice(true, &[Foreign]), None);
        assert_eq!(lease_notice(true, &[]), None);
        assert_eq!(lease_notice(false, &[granted()]), None);
        assert_eq!(lease_notice(false, &[granted(), Foreign]), None);
        assert_eq!(lease_notice(false, &[Pending]), None);
        assert_eq!(lease_notice(false, &[Undetermined]), Some(NOTICE_UNDETERMINED));
        assert_eq!(lease_notice(false, &[Unsupported]), Some(NOTICE_UNSUPPORTED));
        assert_eq!(lease_notice(false, &[Foreign]), Some(NOTICE_NO_BACKEND));
        assert_eq!(lease_notice(false, &[Unreached]), Some(NOTICE_NO_BACKEND));
        assert_eq!(lease_notice(false, &[]), Some(NOTICE_NO_BACKEND));
        assert_eq!(lease_notice(false, &[Undetermined, Unsupported]), Some(NOTICE_UNDETERMINED));
        for set in [&[Undetermined][..], &[Unsupported], &[Foreign], &[Unreached], &[]] {
            let notice = lease_notice(false, set).expect("a no-lease set has a notice");
            assert!(notice.starts_with("closing will not end sessions"), "{notice}");
        }
        // A granted slot whose holder has ended reads as unreached.
        let (tx, rx) = mpsc::unbounded_channel::<HolderCmd>();
        drop(rx);
        let leases = Leases::new(false, vec!["h".to_string()]);
        leases.set(&"h".to_string(), granted(), Some(tx));
        assert_eq!(leases.standing(&"h".to_string()), Some(Unreached));
        assert_eq!(leases.notice(), Some(NOTICE_NO_BACKEND));
    }

    #[test]
    fn not_ended_status_line() {
        assert_eq!(not_ended_line(0), None);
        assert_eq!(not_ended_line(1).unwrap(), "1 session could not be ended and is still running");
        assert_eq!(not_ended_line(3).unwrap(), "3 sessions could not be ended and are still running");

        let h = "h".to_string();
        let t0 = Instant::now();
        let s = Duration::from_secs;

        let (tx, rx) = oneshot::channel();
        let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(h.clone(), rx)], t0);
        assert_eq!(l.line().as_deref(), Some("closing…"));
        tx.send(LeaveOutcome::Replied(2)).unwrap();
        assert_eq!(l.poll(t0), LeaveStep::Show);
        assert_eq!(l.line().unwrap(), "2 sessions could not be ended and are still running");
        assert_eq!(l.presented(t0), vec![(h.clone(), 2)]);
        assert_eq!(l.poll(t0 + s(1)), LeaveStep::Wait(t0 + s(3)));
        assert_eq!(l.poll(t0 + s(3)), LeaveStep::Exit);

        let (tx, rx) = oneshot::channel();
        let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(h.clone(), rx)], t0);
        tx.send(LeaveOutcome::Replied(0)).unwrap();
        assert_eq!(l.poll(t0), LeaveStep::Exit);

        let (_tx, rx) = oneshot::channel::<LeaveOutcome>();
        let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(h.clone(), rx)], t0);
        assert_eq!(l.poll(t0 + s(124)), LeaveStep::Wait(t0 + s(124) + LEAVE_POLL));
        assert_eq!(l.poll(t0 + s(125)), LeaveStep::Show);

        let (_tx, rx) = oneshot::channel::<LeaveOutcome>();
        let mut l = Leaving::new(LeaveIntent::Keep, 0, vec![(h.clone(), rx)], t0);
        assert_eq!(l.line().as_deref(), Some("closing…"));
        assert_eq!(l.poll(t0 + s(10)), LeaveStep::Show);
        assert_eq!(l.line().as_deref(), Some(LEAVE_UNCONFIRMED_KEEP));

        let l = Leaving::new(LeaveIntent::Handover, 75, vec![], t0);
        assert_eq!(l.line(), None);
    }

    #[test]
    fn not_ended_holds_from_the_presented_frame() {
        let h = "h".to_string();
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let (tx, rx) = oneshot::channel();
        let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(h.clone(), rx)], t0);
        tx.send(LeaveOutcome::Replied(2)).unwrap();
        // The count arrives at t0; the step names no ack to send.
        assert_eq!(l.poll(t0), LeaveStep::Show);
        assert!(matches!(l.poll(t0 + ms(1000)), LeaveStep::Wait(_)), "no frame yet: still waiting");
        let t1 = t0 + ms(1500);
        let acks = l.presented(t1);
        assert_eq!(l.poll(t1 + NOT_ENDED_EXIT_HOLD - ms(1)), LeaveStep::Wait(t1 + NOT_ENDED_EXIT_HOLD), "held from the presented frame");
        assert_eq!(l.poll(t1 + NOT_ENDED_EXIT_HOLD), LeaveStep::Exit);
        assert_eq!(acks, vec![(h.clone(), 2)], "the ack goes with the presented frame");
        assert_eq!(l.presented(t1 + ms(10)), vec![], "and only once");

        // Nothing presented (a minimized window): exit at the bound, unacked.
        let (tx, rx) = oneshot::channel();
        let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(h.clone(), rx)], t0);
        tx.send(LeaveOutcome::Replied(2)).unwrap();
        assert_eq!(l.poll(t0), LeaveStep::Show);
        assert_eq!(l.poll(t0 + NOT_ENDED_PRESENT_WAIT - ms(1)), LeaveStep::Wait(t0 + NOT_ENDED_PRESENT_WAIT));
        assert_eq!(l.poll(t0 + NOT_ENDED_PRESENT_WAIT), LeaveStep::Exit);
    }

    #[test]
    fn leave_failure_table() {
        use LeaveIntent::*;
        use LeaveOutcome::*;
        let h = "h".to_string();
        let one = |intent, o: LeaveOutcome| leave_report(intent, &[(h.clone(), o)]);
        let failed = |line: Option<&str>, o: &LeaveOutcome| LeaveReport {
            line: line.map(str::to_string),
            acks: vec![],
            warn: vec![(h.clone(), o.clone())],
        };
        // A reply of 0 is the only quiet exit; a reply of n shows (and acks) n.
        assert_eq!(one(Close, Replied(0)), LeaveReport::default());
        assert_eq!(
            one(Close, Replied(3)),
            LeaveReport { line: not_ended_line(3), acks: vec![(h.clone(), 3)], warn: vec![] }
        );
        // A write error, EOF before the reply, and the deadline: a warn and
        // the intent's line; a handover only warns.
        let write = Failed("write: broken pipe".to_string());
        let eof = Failed("the stream ended before the reply".to_string());
        for o in [write, eof, TimedOut] {
            assert_eq!(one(Close, o.clone()), failed(Some(LEAVE_UNCONFIRMED_CLOSE), &o));
            assert_eq!(one(Keep, o.clone()), failed(Some(LEAVE_UNCONFIRMED_KEEP), &o));
            assert_eq!(one(Handover, o.clone()), failed(None, &o));
        }
        // Beside a count both lines show, and the count is acked once drawn.
        let mixed = leave_report(Close, &[("a".to_string(), Replied(2)), ("b".to_string(), TimedOut)]);
        let both = format!("{}\n{LEAVE_UNCONFIRMED_CLOSE}", not_ended_line(2).unwrap());
        assert_eq!((mixed.line, mixed.acks), (Some(both), vec![("a".to_string(), 2)]));
        // Through `poll`: a dropped reply is a failure, never a zero-count ack.
        let t0 = Instant::now();
        let (tx, rx) = oneshot::channel::<LeaveOutcome>();
        let mut l = Leaving::new(Close, 0, vec![(h.clone(), rx)], t0);
        drop(tx);
        assert_eq!(l.poll(t0), LeaveStep::Show);
        assert_eq!(l.line().as_deref(), Some(LEAVE_UNCONFIRMED_CLOSE));
    }
}
