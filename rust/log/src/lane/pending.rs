//! What both lane reapers share: the inbox message and its bounded intake, one claimed connection's join and close
//! bookkeeping, and the nonblocking publish of a lifecycle event. Each transport keeps only what is its own: how a
//! connection is claimed (a stream shut down, or two I/O slots cancelled) and what follows a retirement (nothing, or an
//! instance recycle).

use super::attach_proto::ConnId;
use super::test_progress::Progress;
use super::transport::{ClosedReason, Joined, LaneEvent, JOIN_POLL_INTERVAL};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::OnceLock;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// One of a connection's two workers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Worker {
    Reader,
    Writer,
}

impl Worker {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Worker::Reader => "reader",
            Worker::Writer => "writer",
        }
    }
}

/// `reader`, `writer` or `both` for the workers a record names.
pub(crate) fn worker_label(workers: &[Worker]) -> &'static str {
    match workers {
        [only] => only.name(),
        _ => "both",
    }
}

/// What one [`PendingJoins::poll`] found.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct JoinPoll {
    /// The workers joined by this poll, each with whether it ended without a panic.
    pub(crate) joined: Vec<(Worker, bool)>,
    /// The workers still unfinished when the deadline was first seen expired; reported by one poll only.
    pub(crate) expired: Option<Vec<Worker>>,
    /// Both workers have been joined.
    pub(crate) done: bool,
}

impl JoinPoll {
    /// True for a poll that joined a worker that had panicked.
    pub(crate) fn panicked(&self) -> bool {
        self.joined.iter().any(|(_, ok)| !ok)
    }

    /// Print one record per completed panic. `prefix` is the transport's (`sot-sock`, `sot-pipe`).
    pub(crate) fn report_panics(&self, prefix: &str, conn: ConnId, elapsed: Duration) {
        let ms = elapsed.as_millis();
        for (worker, _) in self.joined.iter().filter(|(_, ok)| !ok) {
            eprintln!(
                "{prefix}: connection teardown failed conn={conn} worker={} elapsed_ms={ms} reason=worker-panicked \
                 outcome=panicked; worker join completed",
                worker.name()
            );
        }
    }
}

/// The record a server prints when its teardown ends failed, after the per-connection records above.
pub(crate) fn report_server_teardown_failed(prefix: &str) {
    eprintln!("{prefix}: server teardown failed; see worker panic and deadline records");
}

/// A connection's two worker handles under teardown, owned until both are joined. A poll joins every handle that has
/// finished (so a panic or a clean end is seen the moment it happens, whatever the other worker is doing) and never
/// joins or drops a handle that has not; expiry is reported once and the unfinished handles stay owned, to be polled
/// again.
pub(crate) struct PendingJoins {
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    deadline: Instant,
    expiry_reported: bool,
}

impl PendingJoins {
    pub(crate) fn new(reader: JoinHandle<()>, writer: JoinHandle<()>, deadline: Instant) -> Self {
        Self {
            reader: Some(reader),
            writer: Some(writer),
            deadline,
            expiry_reported: false,
        }
    }

    /// Bring the absolute deadline forward to `deadline`; it never moves back.
    pub(crate) fn tighten(&mut self, deadline: Instant) {
        self.deadline = self.deadline.min(deadline);
    }

    /// The workers not yet joined.
    pub(crate) fn unfinished(&self) -> Vec<Worker> {
        [
            (Worker::Reader, &self.reader),
            (Worker::Writer, &self.writer),
        ]
        .into_iter()
        .filter(|(_, slot)| slot.is_some())
        .map(|(worker, _)| worker)
        .collect()
    }

    pub(crate) fn poll(&mut self, now: Instant) -> JoinPoll {
        let mut poll = JoinPoll::default();
        for (worker, slot) in [
            (Worker::Reader, &mut self.reader),
            (Worker::Writer, &mut self.writer),
        ] {
            if slot.as_ref().is_some_and(JoinHandle::is_finished) {
                let ended = slot.take().expect("checked present").join();
                poll.joined.push((worker, ended.is_ok()));
            }
        }
        poll.done = self.reader.is_none() && self.writer.is_none();
        if !poll.done && now >= self.deadline && !self.expiry_reported {
            self.expiry_reported = true;
            poll.expired = Some(self.unfinished());
        }
        poll
    }
}

/// Extra capacity on a server's bounded reaper inbox beyond its connection
/// ceiling -- a connection's own at-most-once teardown flag already caps live
/// `Torn` messages at one per open connection, so the only other traffic this
/// inbox ever carries is the single phase-one wake and the single shutdown
/// wake, each sent without blocking at most once.
///
/// The invariant this keeps: `close()` sends its `Torn` with a BLOCKING send
/// while holding the `conns` lock, and the reaper's `claim` takes that lock. If
/// the inbox could fill, the reaper would take a message, a worker's own
/// teardown send would take the freed slot, and the reaper would block on
/// `conns` while `close()` held it waiting for a slot: a deadlock. At most one
/// `Torn` per connection plus at most two wakes never fills it. No wake carries
/// state (see [`ReaperMsg::Wake`]), so a wake that cannot be queued loses
/// nothing; the slack is for `close()`, not for the wakes' correctness.
pub(super) const REAPER_INBOX_SLACK: usize = 2;

/// How long a normal (not shutdown) close may leave a connection's workers unfinished before the reaper reports it,
/// once. Twenty seconds is the aggregate shutdown deadline's number, but this is a separate budget: it is a
/// per-connection report threshold, not an absolute deadline. A stalled consumer can hold a writer in its reliable
/// `Sent` delivery that long and the worker still finishes whenever the consumer drains, so expiry is loud but does not
/// fail the run-end teardown; the pair stays reaper-owned. A reaper pass at or after the shutdown deadline that finds
/// the pair still unfinished latches the failed teardown (`Claimed::note_poll`), and the reaper cannot exit while it
/// owns one, so `join_workers` fails there too.
pub(crate) const NORMAL_CLOSE_BUDGET: Duration = Duration::from_secs(20);

/// What `join_workers` found of a server's own threads (the acceptor and the reaper). Both fail the teardown for good:
/// one unfinished at the deadline, and one that panicked, because a panicked reaper left its pending pairs unjoined.
#[derive(Default)]
pub(crate) struct ThreadJoins {
    pub(crate) expired: bool,
    pub(crate) panicked: bool,
}

impl ThreadJoins {
    /// Take one thread's outcome; a panic is reported by name, loudly.
    pub(crate) fn record(&mut self, prefix: &str, thread: &str, joined: Joined) {
        match joined {
            Joined::Ended => {}
            Joined::Unfinished => self.expired = true,
            Joined::Panicked => {
                self.panicked = true;
                eprintln!("{prefix}: server thread panicked thread={thread}; teardown failed");
            }
        }
    }

    /// The `server.join.end` result: the first of expiry, a thread panic, an earlier latched failure, or ok.
    pub(crate) fn result(&self, failed: bool) -> &'static str {
        if self.expired {
            "deadline-expired"
        } else if self.panicked {
            "thread-panicked"
        } else if failed {
            "teardown-failed"
        } else {
            "ok"
        }
    }

    pub(crate) fn failed(&self) -> bool {
        self.expired || self.panicked
    }
}

/// A message to a reaper -- the only thread that ever claims a registered connection or joins its workers.
pub(crate) enum ReaperMsg {
    /// A connection ended (natural EOF/error, or a caller's `close`).
    Torn(ConnId, ClosedReason),
    /// A pure wake, sent without blocking and at most once for phase one and once for shutdown. It carries no state:
    /// every pass re-reads `dropping` (phase one cancelled every registered connection: claim them) and the shutdown
    /// deadline (one absolute deadline for all registered and pending pairs; unfinished pairs remain owned).
    Wake,
}

/// Record the shutdown `deadline` in `shutdown` -- the first call wins, and a repeat never extends it -- and wake the
/// reaper, without blocking. The deadline is shared state the reaper reads each pass, so a wake that cannot be queued
/// loses nothing.
pub(crate) fn signal_shutdown(
    shutdown: &OnceLock<Instant>,
    tx: &SyncSender<ReaperMsg>,
    progress: &Progress,
    deadline: Instant,
) {
    if shutdown.set(deadline).is_ok() {
        let _ = tx.try_send(ReaperMsg::Wake);
        progress.note(None, "reaper.shutdown", "signalled");
    }
}

/// The most messages one reaper pass takes before it polls the pending pairs, so continuous inbox traffic cannot
/// starve a pair that is already being joined.
const INTAKE_BATCH: usize = 32;

/// One reaper pass's intake: wait for the first message (without bound when `idle`, else one poll interval), then take
/// up to [`INTAKE_BATCH`] in all, handing each to `handle`. False when the inbox is disconnected while idle: the reaper
/// is done.
pub(crate) fn intake(
    rx: &Receiver<ReaperMsg>,
    idle: bool,
    mut handle: impl FnMut(ReaperMsg),
) -> bool {
    let mut message = if idle {
        match rx.recv() {
            Ok(message) => Some(message),
            Err(_) => return false,
        }
    } else {
        rx.recv_timeout(JOIN_POLL_INTERVAL).ok()
    };
    for taken in 1..=INTAKE_BATCH {
        let Some(next) = message.take() else { break };
        handle(next);
        if taken < INTAKE_BATCH {
            message = rx.try_recv().ok();
        }
    }
    true
}

/// The checkpoint of a lifecycle event's enqueue: its connection, step and the marker identity of a `Sent`.
pub(crate) fn enqueue_checkpoint(
    evt: &LaneEvent,
) -> Option<(Option<ConnId>, &'static str, String)> {
    match evt {
        LaneEvent::Accepted(id) => Some((Some(*id), "accepted.enqueue", String::new())),
        LaneEvent::Closed(id, _) => Some((Some(*id), "closed.enqueue", String::new())),
        LaneEvent::Sent(id, marker) => {
            Some((Some(*id), "sent.enqueue", format!(" marker={marker}")))
        }
        LaneEvent::AcceptError(_) => Some((None, "accept_error.enqueue", String::new())),
        LaneEvent::Bytes(..) => None,
    }
}

/// What a reaper pass reads of its server: the transport's record prefix (`sot-sock`, `sot-pipe`), its recorder, its
/// events channel and flags, and the wake to ping after a successful publish.
pub(crate) struct Ctx<'a> {
    pub(crate) prefix: &'static str,
    pub(crate) progress: &'a Progress,
    pub(crate) events_tx: &'a SyncSender<LaneEvent>,
    pub(crate) dropping: &'a AtomicBool,
    pub(crate) teardown_failed: &'a AtomicBool,
    /// The shutdown deadline, once `join_workers` has set it.
    pub(crate) shutdown: Option<Instant>,
    pub(crate) wake: Option<&'a (dyn Fn() + Send + Sync)>,
}

/// One nonblocking attempt to publish `event`: true when it is retired -- sent, or nothing could ever read it (the
/// consumer gone, or the server dropping) -- and false with `event` put back in `slot` when the channel is full.
/// `last` is the last result noted for this event's enqueue (empty before the first attempt).
pub(crate) fn try_publish(
    ctx: &Ctx,
    event: LaneEvent,
    slot: &mut Option<LaneEvent>,
    last: &mut &'static str,
) -> bool {
    let checkpoint = enqueue_checkpoint(&event);
    if let Some((id, step, _)) = &checkpoint {
        if last.is_empty() {
            ctx.progress.note(*id, step, "begin");
        }
    }
    let sent = ctx.events_tx.try_send(event);
    let result = match &sent {
        Ok(()) => "ok",
        Err(TrySendError::Full(_)) => "full",
        Err(TrySendError::Disconnected(_)) => "disconnected",
    };
    if let Some((id, step, _)) = &checkpoint {
        if result != *last {
            ctx.progress.note(*id, step, result);
        }
    }
    *last = result;
    match sent {
        Ok(()) => {
            if let Some(wake) = ctx.wake {
                wake();
            }
            true
        }
        Err(TrySendError::Disconnected(_)) => true,
        Err(TrySendError::Full(event)) => {
            if ctx.dropping.load(Ordering::Acquire) {
                return true;
            }
            *slot = Some(event);
            false
        }
    }
}

/// Where a claimed connection is: workers being joined, `Closed` waiting for channel room, done.
enum Stage {
    Joining,
    Closing,
    Done,
}

/// A claimed connection under teardown: its workers being joined, then its `Closed` waiting for room in the events
/// channel. The transport keeps whatever else the claim moved (a stream, slots, an instance) beside it until
/// [`Claimed::poll`] reports it retired.
pub(crate) struct Claimed {
    id: ConnId,
    /// `None` is a shutdown claim: nothing is published for it.
    reason: Option<ClosedReason>,
    claimed_at: Instant,
    stage: Stage,
    joins: PendingJoins,
    panicked: bool,
    /// The record for a pair found unfinished at the shutdown deadline has been printed (and the teardown latched).
    deadline_reported: bool,
    /// The `Closed` retained after both joins until the channel has room (or the consumer is gone).
    closed: Option<LaneEvent>,
    /// The last result noted for that `Closed`'s enqueue; empty before the first attempt.
    last_enqueue: &'static str,
}

impl Claimed {
    pub(crate) fn new(
        id: ConnId,
        reason: Option<ClosedReason>,
        reader: JoinHandle<()>,
        writer: JoinHandle<()>,
        deadline: Instant,
    ) -> Self {
        Self {
            id,
            reason,
            claimed_at: Instant::now(),
            stage: Stage::Joining,
            joins: PendingJoins::new(reader, writer, deadline),
            panicked: false,
            deadline_reported: false,
            closed: None,
            last_enqueue: "",
        }
    }

    pub(crate) fn id(&self) -> ConnId {
        self.id
    }

    /// Bring the absolute deadline forward to `deadline`; it never moves back.
    pub(crate) fn tighten(&mut self, deadline: Instant) {
        self.joins.tighten(deadline);
    }

    /// One pass: join the workers that finished and report what the pass found (see `note_poll`), and once both are
    /// joined try to publish its `Closed` without blocking. True when it is retired.
    pub(crate) fn poll(&mut self, ctx: &Ctx, now: Instant) -> bool {
        if let Stage::Joining = self.stage {
            let poll = self.joins.poll(now);
            self.note_poll(ctx, &poll, now);
            if !poll.done {
                return false;
            }
            ctx.progress.note(Some(self.id()), "pending.done", "joined");
            self.stage = Stage::Closing;
            if let Some(reason) = self.reason.take() {
                let reason = if self.panicked {
                    ClosedReason::Error("connection worker panicked".into())
                } else {
                    reason
                };
                self.closed = Some(LaneEvent::Closed(self.id, reason));
            }
        }
        if let Stage::Closing = self.stage {
            if let Some(event) = self.closed.take() {
                let mut slot = None;
                let retired = try_publish(ctx, event, &mut slot, &mut self.last_enqueue);
                self.closed = slot;
                if !retired {
                    return false;
                }
            }
            self.stage = Stage::Done;
        }
        true
    }

    /// Note and report one poll. A completed worker panic latches the failed teardown. A close that outlives its normal
    /// budget before shutdown is reported once and fails nothing: the workers stay owned. A pass at or after the
    /// shutdown deadline that finds a worker still unfinished latches the failed teardown and reports it once, whether
    /// or not the budget expiry was reported earlier; a worker that finishes after that pass does not undo it.
    fn note_poll(&mut self, ctx: &Ctx, poll: &JoinPoll, now: Instant) {
        let id = self.id;
        let elapsed = self.claimed_at.elapsed();
        for (worker, ok) in &poll.joined {
            let step = match worker {
                Worker::Reader => "reader.join.end",
                Worker::Writer => "writer.join.end",
            };
            ctx.progress
                .note(Some(id), step, if *ok { "ok" } else { "panic" });
            if !ok {
                self.panicked = true;
                ctx.progress.note(
                    Some(id),
                    "pending.panicked",
                    format_args!("worker={}", worker.name()),
                );
            }
        }
        if poll.panicked() {
            ctx.teardown_failed.store(true, Ordering::Release);
            poll.report_panics(ctx.prefix, id, elapsed);
        }
        let at_shutdown = ctx.shutdown.is_some_and(|deadline| now >= deadline);
        if let Some(unfinished) = &poll.expired {
            let which = worker_label(unfinished);
            ctx.progress
                .note(Some(id), "pending.expired", format_args!("worker={which}"));
            if !at_shutdown {
                eprintln!(
                    "{}: connection close outlived its budget conn={id} worker={which} elapsed_ms={} \
                     reason=close-budget-expired; unfinished workers remain owned",
                    ctx.prefix,
                    elapsed.as_millis()
                );
            }
        }
        if at_shutdown && !poll.done && !self.deadline_reported {
            self.deadline_reported = true;
            ctx.teardown_failed.store(true, Ordering::Release);
            let which = worker_label(&self.joins.unfinished());
            ctx.progress
                .note(Some(id), "pending.deadline", format_args!("worker={which}"));
            eprintln!(
                "{}: connection teardown failed conn={id} worker={which} elapsed_ms={} \
                 reason=deadline-expired; unfinished workers remain owned",
                ctx.prefix,
                elapsed.as_millis()
            );
        }
    }
}
