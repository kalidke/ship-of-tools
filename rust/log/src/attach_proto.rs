//! The ADR 0041 step-5 attach protocol: a platform-neutral connection/role
//! state machine over the [`crate::wire`] frames. No I/O, no OS types, no
//! clocks read directly (every timing-relevant method is fed a monotonic
//! `now: Instant`) — the `host_handshake.rs`/`wire.rs` precedent this crate
//! already follows for a byte/state machine that must run and be tested on
//! every CI leg, not just Windows. THIS module decides; `capsule_win.rs`'s
//! writer loop (the U3 seam: a real named pipe on Windows) executes the
//! [`Action`]s and feeds the [`AttachProto`] events back.
//!
//! # What this module owns, and what it explicitly does not
//!
//! Owned: the connection registry and its role machine; lockstep (one
//! outstanding client request per connection); the two admission caps and
//! the pre-admission timeout; the ground-gated, single-slot attach/snapshot
//! sequencing; the pen (ephemeral driver capability: demote-on-take,
//! capability-only EOF); the driver keepalive and the generic queue-progress
//! deadline; the per-watcher live-output queue budget.
//!
//! Not owned, deliberately: encoding a checkpoint (`vt100-ctt` is a
//! Windows-only dependency of this crate — this module must build and be
//! tested on every platform), performing the actual OS resize call, reading
//! `pid`/process-creation-time, computing whether a wire input is stale
//! against DURABLE state, or ever writing a WAL frame. Those all require
//! either OS access or the fsync'd voyage this module never touches — they
//! are the loop's job, requested via an [`Action`] and reported back via an
//! event ([`AttachProto::checkpoint_ready`], [`AttachProto::take_committed`],
//! [`AttachProto::resize_outcome`], [`AttachProto::input_outcome`]). "The
//! durable holder/epoch is the CAPSULE's" (ADR 0041): this module tracks
//! only the EPHEMERAL, connection-scoped half of "the pen" (who currently
//! holds the driver capability) — the epoch NUMBER itself is a value fed in
//! from the capsule at [`AttachProto::take_committed`], never invented here.
//!
//! # Connections and roles
//!
//! A connection starts [`Role::Unclassified`] the instant it opens — lane
//! and identity are unknown until its first frame arrives (a single named
//! pipe carries both lanes; [`crate::wire::FrameSplitter`] latches which one
//! from the first frame's magic, so by the time a `frame` event reaches this
//! module every later frame from that connection is guaranteed the same
//! lane). The first decoded frame reclassifies it:
//!
//! | first frame                  | new role                              |
//! |-------------------------------|---------------------------------------|
//! | any `MgmtRequest`              | [`Role::Mgmt`] (no further deadline)  |
//! | `AttachClient::Hello` (accepted) | [`Role::PostHello`] (same deadline)  |
//! | `AttachClient::Hello` (refused)  | closed (`hello_refused` then close)  |
//! | anything else                  | closed — a protocol violation        |
//!
//! `Attach` on a `PostHello` connection promotes it to [`Role::Watcher`]
//! IMMEDIATELY on admission (before its checkpoint has even started) — "a
//! frontend relaunch is precisely a reconnect, and reconnects arrive as
//! watchers" (ADR 0041), and the subscriber cap must count a
//! ground-pending/queued attach the moment it is admitted, not only once its
//! first byte goes out, or a burst of concurrent attaches could blow past
//! the cap before any of them finish. A connection that never completes
//! `hello`+`attach` within the shared 10 s admission window is closed
//! ([`RefusalReason::PreAdmissionTimeout`]) — a judgment call: the ADR names
//! this "pre-hello timeout", but a connection that says `hello` and then
//! never attaches is occupying the exact same slot a pre-hello connection
//! does, so the SAME deadline (started once, at `connection_opened`, never
//! reset by a successful `hello`) governs reaching `Watcher`, not merely
//! completing `hello`.
//!
//! # Lockstep
//!
//! Every connection tracks one `outstanding_request` flag. A second
//! lockstep-classified client frame while it is set is
//! [`RefusalReason::LockstepViolation`] — closed, no reply (mirrors that
//! `feed` can decode several frames from one burst read, so this is checked
//! per decoded frame, not per transport read). `keepalive` is exempt (it is
//! not a client "request" in this sense — see below). The flag is set the
//! instant a lockstep request is accepted, and cleared only when the
//! corresponding reply's bytes are reported PHYSICALLY WRITTEN via
//! [`AttachProto::sent`] with [`SentMarker::Reply`] (or a
//! [`SentMarker::CheckpointLastChunk`]/`ReplyThenClose`/`ShutdownAck`, which
//! also clear it) — never merely at the moment this module *decides* the
//! reply, because a real transport can buffer several already-decoded
//! client frames ahead of any reply physically leaving. For `attach`, that
//! means the flag can stay set through the whole ground-pend + checkpoint
//! transfer; wire.rs's own "the first `checkpoint_chunk` IS the attach
//! success signal" is exactly when it clears, not when the LAST chunk goes
//! out — a client is free to send `take` while its own checkpoint is still
//! streaming (though `take` has an independent admission rule for that
//! case; see below).
//!
//! # Caps
//!
//! Two independent counters, both closing (not merely refusing) on
//! overflow: `non_watcher_count` (`Unclassified` + `Mgmt` + `PostHello`,
//! checked at `connection_opened` — a cap on connections that have not yet
//! finished attaching) and `watcher_count` (`Role::Watcher`, the ADR's "≤4
//! subscribers TOTAL, driver included", checked when `attach` is accepted).
//! A `SubscriberCap` refusal — unlike the non-watcher cap, which just closes
//! outright — sends `attach_refused` and leaves the connection open and
//! retryable: it never became a `Watcher`, so nothing about it needs
//! reverting.
//!
//! # The ground-gated attach and the one-slot checkpoint transfer
//!
//! `attach` reserves a `Watcher` slot immediately, then either takes the one
//! global `checkpoint_slot` (if free) and starts its own 5 s ground-wait
//! deadline, or joins `checkpoint_queue` with NO deadline yet — "a second
//! attach pends for the SLOT", not for ground; its own clock only starts
//! once it becomes the slot holder. [`AttachProto::ground_reached`] (fed by
//! the loop after a group-commit where `parser.is_ground()`) promotes the
//! current slot holder, if it is waiting, from `AwaitingGround` to
//! `Sending` and returns [`Action::BeginCheckpoint`] — the loop encodes
//! (`Screen::checkpoint()`, which this module cannot call) and hands the
//! bytes back via [`AttachProto::checkpoint_ready`], which slices them into
//! `checkpoint_chunk` frames at [`crate::wire::MAX_CHECKPOINT_CHUNK_PAYLOAD`]
//! and marks the first [`SentMarker::Reply`] and the last
//! [`SentMarker::CheckpointLastChunk`] (the SAME chunk carries both markers
//! when there is only one). A [`GROUND_TIMEOUT`] (5 s) with no
//! `ground_reached` DEMOTES the connection back to `PostHello` (freeing both
//! its `Watcher` slot and the checkpoint slot, which then advances the
//! queue) and replies `attach_refused {GroundTimeout}` — explicitly
//! retryable, per the ADR.
//!
//! `take`'s own [`crate::wire::TakeRefusedReason::CheckpointInFlight`] is a
//! DIFFERENT rule from the slot: it fires only when the REQUESTING
//! connection's OWN checkpoint has not yet finished (i.e. it is not yet
//! [`CheckpointProgress::Done`]) — "refused until the taker's final chunk is
//! REPORTED physically written" (ADR 0041) names the taker's own transfer,
//! not some unrelated connection's. `Done` is reached only via
//! [`SentMarker::CheckpointLastChunk`]'s sent-completion, never merely
//! having chunked the bytes.
//!
//! # The pen
//!
//! `self.driver: Option<DriverState>` is the ephemeral capability. `take`
//! from a non-`Watcher` connection is `NotAttached`; from an attached
//! connection whose own checkpoint is not `Done` is `CheckpointInFlight`;
//! otherwise this module asks the loop to [`Action::CommitTake`] (the fsync
//! and the epoch NUMBER are the capsule's) and, once told the committed
//! epoch via [`AttachProto::take_committed`], installs the capability on
//! THIS connection — silently overwriting whatever connection held it
//! before, which is the demotion: the previous holder's `Role::Watcher`
//! entry is untouched, it simply stops being able to pass the driver check.
//! `input`/`resize` from any connection that is not `self.driver`'s current
//! holder is refused (`input`: folded into the same "stale" wire reply the
//! ADR already defines, since a replayed identity from a connection lacking
//! the capability is indistinguishable on the wire from a stale epoch — see
//! [`Action::ForwardInput`]'s doc; `resize`: `NotDriver`). A connection's
//! close ([`AttachProto::connection_closed`], or this module's own
//! `close_with_refusal`) clears `self.driver` ONLY if it was that
//! connection — capability-only EOF, no durable transition, per the ADR's
//! spec-gate deletion of the old local-grant behavior.
//!
//! # Keepalive and the generic progress deadline
//!
//! Driver-only. `tick` starts ONE `keepalive` after 30 s since the driver
//! connection's `last_activity` (any inbound frame, or any `sent`
//! completion) with none currently outstanding — suspended entirely while
//! `checkpoint_slot.is_some()` (a judgment call, documented once here
//! rather than at the call site: the writer loop's attention can be
//! monopolized for real by a multi-MiB transfer belonging to ANY connection,
//! not only the driver's own, and that must not be misread as a hung
//! driver). The reply deadline (30 s) is armed at
//! [`SentMarker::Keepalive`]'s sent-completion, not at enqueue — a ping
//! stuck behind a real backlog must not kill a healthy reader before its
//! bytes even left. A wrong or unexpected `Keepalive` echo (wrong nonce, no
//! nonce outstanding, or from a non-driver connection) is
//! `UnexpectedKeepalive` — closed. Independently, ANY connection with
//! `queued_live_bytes > 0` whose `last_send_progress` is more than 30 s old
//! is `ProgressStall` — closed (this is the queue-liveness bound, distinct
//! from keepalive; it applies to watchers too, since a wedged watcher can
//! sit for a long time below the 4 MiB overflow bound while never draining).
//!
//! # Queue accounting
//!
//! `queued_live_bytes` (LIVE output only — a `Watcher`'s field, absent
//! entirely for a connection still mid-checkpoint) is incremented by
//! [`AttachProto::bytes_queued`] (called internally by
//! [`AttachProto::output_committed`] for every `Done` watcher, and directly
//! by a caller/test that wants to drive the bound explicitly) and
//! decremented by [`AttachProto::sent`]'s [`SentMarker::OutputBytes`] arm.
//! Checkpoint bytes never touch this counter (decision 5: "the checkpoint
//! work item rides OUTSIDE this budget"). Overflow closes with no wire
//! frame — "no `evicted` frame exists on the wire, deliberately" — logged
//! only via [`Action::RecordRefusal`].

use crate::wire::{
    self, AttachClient, AttachRefusedReason, AttachServer, DecodedFrame, MgmtReply, MgmtRequest,
    ResizeRefusedReason, Survival, TakeRefusedReason,
};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// A caller-assigned, opaque connection identifier. This module attaches no
/// meaning to the value beyond identity — the loop (real pipe handle, or a
/// test transport's own counter) owns the numbering.
pub type ConnId = u64;

const NON_WATCHER_CAP: usize = 4;
const SUBSCRIBER_CAP: usize = 4;
const PRE_ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);
const GROUND_TIMEOUT: Duration = Duration::from_secs(5);
const KEEPALIVE_IDLE_TRIGGER: Duration = Duration::from_secs(30);
const KEEPALIVE_REPLY_DEADLINE: Duration = Duration::from_secs(30);
const PROGRESS_DEADLINE: Duration = Duration::from_secs(30);
/// ADR 0041 budget table: "per-watcher queue 4 MiB, overflow = eviction" —
/// LIVE output only (decision 5: the checkpoint work item rides outside it).
const WATCHER_LIVE_QUEUE_BUDGET_BYTES: u64 = 4 * 1024 * 1024;

/// The capsule's self-reported mgmt `status` fields (ADR 0041 attach
/// protocol: pid, raw FILETIME creation time, survival) — supplied once at
/// construction (the loop computes these via OS calls this module must
/// never make) and answered synchronously from then on; they never change
/// for the run's lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MgmtStatus {
    pub pid: u32,
    pub created: u64,
    pub survival: Survival,
}

/// One connection's role. `Unclassified`/`PostHello` share the same
/// admission `deadline` semantics — see the module doc.
#[derive(Debug, Clone)]
enum Role {
    Unclassified { deadline: Instant },
    Mgmt,
    PostHello { deadline: Instant },
    Watcher(WatcherState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckpointProgress {
    /// Waiting for the global slot; no deadline yet (see module doc).
    QueuedForSlot,
    /// Holds the slot, waiting for a ground boundary.
    AwaitingGround { deadline: Instant },
    /// `BeginCheckpoint` was requested; waiting for `checkpoint_ready`
    /// and/or for its chunks' sent-completions.
    Sending,
    /// The final chunk was reported physically written.
    Done,
}

#[derive(Debug, Clone)]
struct WatcherState {
    /// Recorded per-connection bookkeeping (ADR 0041 attach protocol); not
    /// read internally today — `take`'s controller_id comes from the wire
    /// frame that names it, not from this connection's own attach identity
    /// — but kept for future diagnostics/attribution (e.g. distinguishing
    /// which reconnected controller a given subscriber slot belongs to).
    #[allow(dead_code)]
    controller_id: String,
    checkpoint: CheckpointProgress,
    /// LIVE output only, budget-checked (see module doc's "Queue
    /// accounting").
    queued_live_bytes: u64,
}

#[derive(Debug, Clone)]
struct Conn {
    role: Role,
    outstanding_request: bool,
    /// Any inbound frame, or any `sent` completion — the keepalive
    /// idle-trigger clock.
    last_activity: Instant,
    /// Only `sent` completions — the generic queue-progress-stall clock,
    /// deliberately NOT reset by inbound frames (a connection that keeps
    /// talking to us while its own queue never drains must still be caught).
    last_send_progress: Instant,
}

#[derive(Debug, Clone)]
struct DriverState {
    conn: ConnId,
    #[allow(dead_code)] // carried for future callers/diagnostics; not read internally today
    controller_id: String,
    #[allow(dead_code)]
    take_epoch: u64,
    keepalive_outstanding: Option<u64>,
    /// Armed only once the ping's sent-completion is reported (ADR 0041:
    /// "not at enqueue").
    keepalive_deadline: Option<Instant>,
}

/// What a physically-completed [`Action::Send`] means beyond "one fewer
/// thing in flight" — see the module doc sections on lockstep, the ground
/// gate, and keepalive for why each variant exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SentMarker {
    /// This send fully satisfies the connection's outstanding lockstep
    /// request; clear the flag.
    Reply,
    /// As `Reply`, and the connection must close once this reply is
    /// physically written (`hello_refused`).
    ReplyThenClose,
    /// The mgmt `shutdown_ok` reply: clears outstanding, tells the loop to
    /// begin EndRun, and closes this connection — "the shutdown ack is
    /// physically written before teardown closes its connection" (ADR
    /// 0041).
    ShutdownAck { reason: String },
    /// The final `checkpoint_chunk` of one connection's own attach
    /// transfer: clears outstanding if not already, marks this
    /// connection's checkpoint `Done` (unlocking `take` eligibility),
    /// frees the global slot, and promotes the next queued attach if any.
    CheckpointLastChunk,
    /// The server-originated keepalive echo request: arms its 30 s reply
    /// deadline NOW.
    Keepalive { nonce: u64 },
    /// A live `output` frame carrying `n` raw payload bytes (the SAME count
    /// [`AttachProto::output_committed`] already passed to
    /// [`AttachProto::bytes_queued`] when it created this send — carried on
    /// the marker itself, not re-derived from the encoded frame's length,
    /// so the two can never drift out of sync): decrements the sending
    /// watcher's queued-byte counter by `n`.
    OutputBytes { n: u64 },
}

/// Why this module closed or refused a connection — diagnostic only; no
/// wire frame exists for most of these (queue overflow explicitly has none,
/// by design; the rest are protocol violations with no defined reply
/// either).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    PreAdmissionTimeout,
    NonWatcherCapExceeded,
    LockstepViolation,
    LaneSequenceViolation,
    QueueOverflow,
    ProgressStall,
    KeepaliveDeath,
    UnexpectedKeepalive,
}

/// The dedupe-chain outcome the loop reports back after executing
/// [`Action::ForwardInput`] — see that variant's doc for the full WAL
/// sequence this drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOutcome {
    Recorded,
    RefusedStale,
    DeliveryUnknown,
}

/// What the loop must do. THIS module decides; the loop executes and, for
/// the four "request" variants, reports the outcome back via the matching
/// event ([`AttachProto::checkpoint_ready`], [`AttachProto::take_committed`],
/// [`AttachProto::resize_outcome`], [`AttachProto::input_outcome`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Write `frame_bytes` (an already-encoded, complete wire frame) to
    /// `conn`. Once physically written, the loop must report it via
    /// [`AttachProto::sent`] with the same `marker` — harmless to always
    /// report every send uniformly, including a `None` marker.
    Send {
        conn: ConnId,
        frame_bytes: Vec<u8>,
        marker: Option<SentMarker>,
    },
    /// Sever `conn` at the transport level. This module has already
    /// forgotten it (or is about to, in the same batch) — the loop does not
    /// need to call `connection_closed` for a `conn` it closes on this
    /// module's own instruction, though doing so is a harmless no-op.
    Close(ConnId),
    /// Encode this connection's checkpoint NOW (`Screen::checkpoint()`,
    /// which only the loop — the Windows-only `vt100-ctt` consumer — can
    /// call) and report the bytes via [`AttachProto::checkpoint_ready`].
    BeginCheckpoint { conn: ConnId },
    /// Commit `take_state {holder: controller_id, epoch: prior + 1}` with
    /// `Commit::Immediate` (fsync), THEN report the committed epoch via
    /// [`AttachProto::take_committed`]. The epoch NUMBER is the capsule's;
    /// this module never invents one.
    CommitTake { conn: ConnId, controller_id: String },
    /// Execute the full ADR 0039 input WAL for one wire `input` frame:
    /// dedupe-check `idem_key` against the store's index (folded once at
    /// open, kept live) → per the lattice, either commit `input` (fsync) as
    /// a new entry or (a `{input}`-only retry) reuse the ORIGINAL input's
    /// identity without writing a second `input` frame → the LAST-MOMENT
    /// recheck of `(controller_id, take_epoch)` against DURABLE state
    /// (`connection_authorized` is this module's connection-scoped half of
    /// that same check — the ADR requires BOTH "the capability AND the
    /// durable holder/epoch"; a connection lacking the capability cannot
    /// possibly also match the durable identity, but the loop's own
    /// durable comparison is the actual source of truth and must run
    /// regardless) → if STALE (by either check): commit
    /// `{input, refused_stale_epoch}` (fsync) directly, `forward_intent`
    /// NEVER committed — this is exactly why the lattice's refused chain is
    /// `{input, refused}`, with no intent in it; if FRESH: commit
    /// `input_fact:forward_intent` (fsync) → forward syscall → commit
    /// `forwarded` (fsync). A `{input,intent}`-only chain (crash-in-flight,
    /// or a duplicate that reached exactly that far) replies
    /// `DeliveryUnknown` and appends nothing further. Report the outcome
    /// via [`AttachProto::input_outcome`].
    ForwardInput {
        conn: ConnId,
        controller_id: String,
        take_epoch: u64,
        idem_key: [u8; 16],
        payload: Vec<u8>,
        connection_authorized: bool,
    },
    /// Run the existing step-4 ordered resize exchange (request commit →
    /// one `ResizePseudoConsole` call, skipped if out of budget → parser +
    /// geometry updated only on success → outcome commit) — unchanged by
    /// this unit. Report the outcome via [`AttachProto::resize_outcome`].
    ApplyResize { conn: ConnId, cols: u16, rows: u16 },
    /// Begin `EndRun`: the mgmt `shutdown_ok` ack has already been reported
    /// physically written. `reason` is the client-supplied string, to be
    /// recorded in `producer_dead`'s detail.
    Shutdown { reason: String },
    /// Diagnostic only — log why `conn` (`None` for a cap refusal with no
    /// connection yet, though today every caller has one) was refused or
    /// closed. Always paired with a `Close` in the same batch.
    RecordRefusal {
        conn: Option<ConnId>,
        reason: RefusalReason,
    },
}

/// The platform-neutral attach-protocol state machine. See the module doc.
#[derive(Debug)]
pub struct AttachProto {
    conns: HashMap<ConnId, Conn>,
    mgmt_status: MgmtStatus,
    non_watcher_count: usize,
    watcher_count: usize,
    checkpoint_slot: Option<ConnId>,
    checkpoint_queue: VecDeque<ConnId>,
    driver: Option<DriverState>,
    nonce_counter: u64,
}

impl AttachProto {
    #[must_use]
    pub fn new(mgmt_status: MgmtStatus) -> Self {
        Self {
            conns: HashMap::new(),
            mgmt_status,
            non_watcher_count: 0,
            watcher_count: 0,
            checkpoint_slot: None,
            checkpoint_queue: VecDeque::new(),
            driver: None,
            nonce_counter: 0,
        }
    }

    // -- lifecycle events ------------------------------------------------

    /// A new connection accepted at the transport level. Refused outright
    /// (closed, no frame) if the combined mgmt/pre-hello cap is already at
    /// [`NON_WATCHER_CAP`] — every connection starts non-watcher, so this is
    /// the only place that cap can ever be exceeded.
    pub fn connection_opened(&mut self, conn: ConnId, now: Instant) -> Vec<Action> {
        if self.non_watcher_count >= NON_WATCHER_CAP {
            return vec![
                Action::RecordRefusal {
                    conn: Some(conn),
                    reason: RefusalReason::NonWatcherCapExceeded,
                },
                Action::Close(conn),
            ];
        }
        self.conns.insert(
            conn,
            Conn {
                role: Role::Unclassified {
                    deadline: now + PRE_ADMISSION_TIMEOUT,
                },
                outstanding_request: false,
                last_activity: now,
                last_send_progress: now,
            },
        );
        self.non_watcher_count += 1;
        vec![]
    }

    /// The transport reports `conn` is gone (ordered EOF or an error). Frees
    /// every reservation it held; if it held the ephemeral driver
    /// capability, that capability simply vanishes — no durable transition
    /// (ADR 0041's spec-gate deletion of the local-grant/EOF-clears-holder
    /// behavior).
    pub fn connection_closed(&mut self, conn: ConnId, now: Instant) -> Vec<Action> {
        self.remove_connection(conn, now);
        vec![]
    }

    /// One decoded frame arrived on `conn`, in order. May return zero, one,
    /// or several actions (a multi-chunk checkpoint kickoff is still only
    /// one `BeginCheckpoint`, but a resize/take/input round trip's ultimate
    /// reply arrives through a LATER event, not this one, for anything that
    /// needs the loop's own state).
    pub fn frame(&mut self, conn: ConnId, decoded: DecodedFrame, now: Instant) -> Vec<Action> {
        if let DecodedFrame::Keepalive { nonce } = decoded {
            return self.handle_keepalive_reply(conn, nonce, now);
        }
        let Some(c) = self.conns.get(&conn) else {
            return vec![];
        };
        if c.outstanding_request {
            return self.close_with_refusal(conn, RefusalReason::LockstepViolation, now);
        }
        if let Some(c) = self.conns.get_mut(&conn) {
            c.last_activity = now;
        }
        match decoded {
            DecodedFrame::MgmtRequest(req) => self.handle_mgmt(conn, req, now),
            DecodedFrame::MgmtReply(_) | DecodedFrame::AttachServer(_) => {
                // A client sending server-shaped bytes: wire.rs decodes by
                // tag alone, so this is reachable from adversarial input,
                // not merely "impossible" — reject it here, not with a
                // panic.
                self.close_with_refusal(conn, RefusalReason::LaneSequenceViolation, now)
            }
            DecodedFrame::AttachClient(ac) => self.handle_attach_client(conn, ac, now),
            DecodedFrame::Keepalive { .. } => unreachable!("handled above"),
        }
    }

    /// One `Action::Send`'s bytes were reported PHYSICALLY WRITTEN. `marker`
    /// is whatever the originating `Send` carried, or `None` for a send
    /// with no bookkeeping consequence. Always resets
    /// `last_activity`/`last_send_progress`.
    pub fn sent(&mut self, conn: ConnId, marker: Option<SentMarker>, now: Instant) -> Vec<Action> {
        if let Some(c) = self.conns.get_mut(&conn) {
            c.last_activity = now;
            c.last_send_progress = now;
        }
        let Some(marker) = marker else { return vec![] };
        match marker {
            SentMarker::Reply => {
                self.clear_outstanding(conn);
                vec![]
            }
            SentMarker::ReplyThenClose => {
                self.clear_outstanding(conn);
                self.remove_connection(conn, now);
                vec![Action::Close(conn)]
            }
            SentMarker::ShutdownAck { reason } => {
                self.clear_outstanding(conn);
                self.remove_connection(conn, now);
                vec![Action::Shutdown { reason }, Action::Close(conn)]
            }
            SentMarker::CheckpointLastChunk => {
                self.clear_outstanding(conn);
                if let Some(Role::Watcher(w)) = self.conns.get_mut(&conn).map(|c| &mut c.role) {
                    w.checkpoint = CheckpointProgress::Done;
                }
                if self.checkpoint_slot == Some(conn) {
                    self.checkpoint_slot = None;
                    self.advance_checkpoint_queue(now);
                }
                vec![]
            }
            SentMarker::Keepalive { nonce } => {
                if let Some(d) = &mut self.driver {
                    if d.conn == conn && d.keepalive_outstanding == Some(nonce) {
                        d.keepalive_deadline = Some(now + KEEPALIVE_REPLY_DEADLINE);
                    }
                }
                vec![]
            }
            SentMarker::OutputBytes { n } => {
                if let Some(Role::Watcher(w)) = self.conns.get_mut(&conn).map(|c| &mut c.role) {
                    w.queued_live_bytes = w
                        .queued_live_bytes
                        .checked_sub(n)
                        .expect("sent(OutputBytes): more bytes reported sent than were ever queued");
                }
                vec![]
            }
        }
    }

    /// `n` LIVE output bytes are about to be enqueued for `conn` (a
    /// `Watcher`; any other role is a no-op — checkpoint bytes never call
    /// this). Closes on overflow past [`WATCHER_LIVE_QUEUE_BUDGET_BYTES`],
    /// per decision 5's "the checkpoint work item rides OUTSIDE this
    /// budget" — this is the LIVE-only counter.
    pub fn bytes_queued(&mut self, conn: ConnId, n: u64, now: Instant) -> Vec<Action> {
        let overflowed = match self.conns.get_mut(&conn).map(|c| &mut c.role) {
            Some(Role::Watcher(w)) => {
                w.queued_live_bytes += n;
                w.queued_live_bytes > WATCHER_LIVE_QUEUE_BUDGET_BYTES
            }
            _ => false,
        };
        if overflowed {
            self.close_with_refusal(conn, RefusalReason::QueueOverflow, now)
        } else {
            vec![]
        }
    }

    /// Time-driven checks with no inbound frame to trigger them: the two
    /// admission timeouts, the ground-wait deadline, the generic
    /// queue-progress stall, and the driver keepalive state machine.
    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();

        let mut pre_admission_timed_out = Vec::new();
        for (id, c) in &self.conns {
            let deadline = match &c.role {
                Role::Unclassified { deadline } | Role::PostHello { deadline } => Some(*deadline),
                _ => None,
            };
            if deadline.is_some_and(|d| now >= d) {
                pre_admission_timed_out.push(*id);
            }
        }
        for id in pre_admission_timed_out {
            actions.extend(self.close_with_refusal(id, RefusalReason::PreAdmissionTimeout, now));
        }

        let mut ground_timed_out = Vec::new();
        for (id, c) in &self.conns {
            if let Role::Watcher(w) = &c.role {
                if let CheckpointProgress::AwaitingGround { deadline } = w.checkpoint {
                    if now >= deadline {
                        ground_timed_out.push(*id);
                    }
                }
            }
        }
        for id in ground_timed_out {
            actions.extend(self.ground_timeout(id, now));
        }

        let mut stalled = Vec::new();
        for (id, c) in &self.conns {
            let queued = matches!(&c.role, Role::Watcher(w) if w.queued_live_bytes > 0);
            if queued && now.saturating_duration_since(c.last_send_progress) >= PROGRESS_DEADLINE {
                stalled.push(*id);
            }
        }
        for id in stalled {
            actions.extend(self.close_with_refusal(id, RefusalReason::ProgressStall, now));
        }

        actions.extend(self.tick_keepalive(now));
        actions
    }

    /// Fed by the loop right after a group-commit where `parser.is_ground()`
    /// held. Promotes the current checkpoint-slot holder from
    /// `AwaitingGround` to `Sending`, if any — a no-op otherwise (ground
    /// recurs constantly; most calls have nothing to promote).
    pub fn ground_reached(&mut self, now: Instant) -> Vec<Action> {
        let _ = now;
        let Some(conn) = self.checkpoint_slot else {
            return vec![];
        };
        let awaiting = matches!(
            self.conns.get(&conn).and_then(watcher_checkpoint),
            Some(CheckpointProgress::AwaitingGround { .. })
        );
        if !awaiting {
            return vec![];
        }
        if let Some(Role::Watcher(w)) = self.conns.get_mut(&conn).map(|c| &mut c.role) {
            w.checkpoint = CheckpointProgress::Sending;
        }
        vec![Action::BeginCheckpoint { conn }]
    }

    /// The loop encoded `conn`'s checkpoint (only it can — see the module
    /// doc) and hands back the bytes. Slices them into `checkpoint_chunk`
    /// frames within [`crate::wire::MAX_CHECKPOINT_CHUNK_PAYLOAD`], marking
    /// the first [`SentMarker::Reply`] and the last
    /// [`SentMarker::CheckpointLastChunk`] (one chunk carries both).
    /// Ignored (defensive no-op) if `conn` is not this run's current slot
    /// holder mid-`Sending` — should not happen given the loop only ever
    /// calls this in response to `BeginCheckpoint`.
    pub fn checkpoint_ready(&mut self, conn: ConnId, bytes: Vec<u8>, _now: Instant) -> Vec<Action> {
        let sending = matches!(
            self.conns.get(&conn).and_then(watcher_checkpoint),
            Some(CheckpointProgress::Sending)
        );
        if !sending || self.checkpoint_slot != Some(conn) {
            return vec![];
        }
        chunk_checkpoint(conn, bytes)
    }

    /// The loop fsynced `take_state {holder: controller_id, epoch:
    /// new_take_epoch}`. Installs the ephemeral capability on `conn`,
    /// silently overwriting whoever held it before (the demotion — that
    /// connection's own `Watcher` entry is untouched).
    pub fn take_committed(&mut self, conn: ConnId, controller_id: String, new_take_epoch: u64, now: Instant) -> Vec<Action> {
        self.driver = Some(DriverState {
            conn,
            controller_id,
            take_epoch: new_take_epoch,
            keepalive_outstanding: None,
            keepalive_deadline: None,
        });
        if let Some(c) = self.conns.get_mut(&conn) {
            c.last_activity = now;
        }
        let bytes = wire::encode_attach_server(&AttachServer::TakeOk { take_epoch: new_take_epoch })
            .expect("TakeOk is a fixed-shape body, always within MAX_BODY_LEN");
        vec![Action::Send {
            conn,
            frame_bytes: bytes,
            marker: Some(SentMarker::Reply),
        }]
    }

    /// The loop ran the input WAL for `conn`'s `input` frame and reports the
    /// outcome.
    pub fn input_outcome(&mut self, conn: ConnId, outcome: InputOutcome, now: Instant) -> Vec<Action> {
        if let Some(c) = self.conns.get_mut(&conn) {
            c.last_activity = now;
        }
        let frame = match outcome {
            InputOutcome::Recorded => AttachServer::InputRecorded,
            InputOutcome::RefusedStale => AttachServer::InputRefusedStale,
            InputOutcome::DeliveryUnknown => AttachServer::InputDeliveryUnknown,
        };
        let bytes =
            wire::encode_attach_server(&frame).expect("input reply is a fixed-shape body, always within MAX_BODY_LEN");
        vec![Action::Send {
            conn,
            frame_bytes: bytes,
            marker: Some(SentMarker::Reply),
        }]
    }

    /// The loop ran the ordered resize exchange for `conn` and reports
    /// whether the geometry was in budget.
    pub fn resize_outcome(&mut self, conn: ConnId, ok: bool, now: Instant) -> Vec<Action> {
        if let Some(c) = self.conns.get_mut(&conn) {
            c.last_activity = now;
        }
        let frame = if ok {
            AttachServer::ResizeOk
        } else {
            AttachServer::ResizeRefused {
                reason: ResizeRefusedReason::OutOfBudget,
            }
        };
        let bytes =
            wire::encode_attach_server(&frame).expect("resize reply is a fixed-shape body, always within MAX_BODY_LEN");
        vec![Action::Send {
            conn,
            frame_bytes: bytes,
            marker: Some(SentMarker::Reply),
        }]
    }

    /// Live producer output just committed (the watermark). Enqueues an
    /// `output` frame for every `Watcher` whose own checkpoint is `Done` —
    /// a connection still mid-attach gets nothing here (checkpoint chunks
    /// then only post-watermark output, never a batch that predates its own
    /// watermark).
    pub fn output_committed(&mut self, bytes: &[u8], now: Instant) -> Vec<Action> {
        let targets: Vec<ConnId> = self
            .conns
            .iter()
            .filter_map(|(id, c)| match &c.role {
                Role::Watcher(w) if w.checkpoint == CheckpointProgress::Done => Some(*id),
                _ => None,
            })
            .collect();
        let mut actions = Vec::new();
        for conn in targets {
            actions.extend(self.bytes_queued(conn, bytes.len() as u64, now));
            let still_live = matches!(
                self.conns.get(&conn).and_then(watcher_checkpoint),
                Some(CheckpointProgress::Done)
            );
            if still_live {
                let encoded = wire::encode_attach_server(&AttachServer::Output { bytes: bytes.to_vec() })
                    .expect("output frame within the outer 1 MiB cap is the loop's own responsibility");
                actions.push(Action::Send {
                    conn,
                    frame_bytes: encoded,
                    marker: Some(SentMarker::OutputBytes { n: bytes.len() as u64 }),
                });
            }
        }
        actions
    }

    // -- internal dispatch -------------------------------------------------

    fn handle_mgmt(&mut self, conn: ConnId, req: MgmtRequest, now: Instant) -> Vec<Action> {
        match self.conns.get(&conn).map(|c| &c.role) {
            Some(Role::Unclassified { .. }) => {
                if let Some(c) = self.conns.get_mut(&conn) {
                    c.role = Role::Mgmt;
                }
            }
            Some(Role::Mgmt) => {}
            // Structurally unreachable given wire.rs's own lane latching
            // (an mgmt-tagged body cannot arrive on a connection already
            // latched to the attach lane) — refuse rather than panic.
            _ => return self.close_with_refusal(conn, RefusalReason::LaneSequenceViolation, now),
        }
        self.mark_outstanding(conn);
        match req {
            MgmtRequest::Probe => {
                let bytes = wire::encode_mgmt_reply(&MgmtReply::ProbeOk).expect("fixed-shape body");
                vec![Action::Send {
                    conn,
                    frame_bytes: bytes,
                    marker: Some(SentMarker::Reply),
                }]
            }
            MgmtRequest::Status => {
                let s = self.mgmt_status;
                let bytes = wire::encode_mgmt_reply(&MgmtReply::StatusOk {
                    pid: s.pid,
                    created: s.created,
                    survival: s.survival,
                })
                .expect("fixed-shape body");
                vec![Action::Send {
                    conn,
                    frame_bytes: bytes,
                    marker: Some(SentMarker::Reply),
                }]
            }
            MgmtRequest::Shutdown { reason } => {
                let bytes = wire::encode_mgmt_reply(&MgmtReply::ShutdownOk).expect("fixed-shape body");
                vec![Action::Send {
                    conn,
                    frame_bytes: bytes,
                    marker: Some(SentMarker::ShutdownAck { reason }),
                }]
            }
        }
    }

    fn handle_attach_client(&mut self, conn: ConnId, frame: AttachClient, now: Instant) -> Vec<Action> {
        let role_is_unclassified = matches!(self.conns.get(&conn).map(|c| &c.role), Some(Role::Unclassified { .. }));
        let role_is_mgmt = matches!(self.conns.get(&conn).map(|c| &c.role), Some(Role::Mgmt));
        if role_is_mgmt {
            // Structurally unreachable given wire.rs's own lane latching.
            return self.close_with_refusal(conn, RefusalReason::LaneSequenceViolation, now);
        }
        if role_is_unclassified && !matches!(frame, AttachClient::Hello { .. }) {
            return self.close_with_refusal(conn, RefusalReason::LaneSequenceViolation, now);
        }
        if !role_is_unclassified && matches!(frame, AttachClient::Hello { .. }) {
            return self.close_with_refusal(conn, RefusalReason::LaneSequenceViolation, now);
        }
        let is_watcher = matches!(self.conns.get(&conn).map(|c| &c.role), Some(Role::Watcher(_)));
        if matches!(frame, AttachClient::Attach { .. }) && is_watcher {
            return self.close_with_refusal(conn, RefusalReason::LaneSequenceViolation, now);
        }

        self.mark_outstanding(conn);
        match frame {
            AttachClient::Hello { proto } => self.handle_hello(conn, proto),
            AttachClient::Attach { controller_id } => self.handle_attach(conn, controller_id, now),
            AttachClient::Take { controller_id } => self.handle_take(conn, controller_id),
            AttachClient::Input {
                controller_id,
                take_epoch,
                idem_key,
                payload,
            } => self.handle_input(conn, controller_id, take_epoch, idem_key, payload),
            AttachClient::Resize { cols, rows } => self.handle_resize(conn, cols, rows),
        }
    }

    fn handle_hello(&mut self, conn: ConnId, proto: u32) -> Vec<Action> {
        match wire::negotiate(proto) {
            wire::Negotiated::Accepted(v) => {
                if let Some(c) = self.conns.get_mut(&conn) {
                    if let Role::Unclassified { deadline } = c.role {
                        c.role = Role::PostHello { deadline };
                    }
                }
                let bytes = wire::encode_attach_server(&AttachServer::HelloOk { proto: v }).expect("fixed-shape body");
                vec![Action::Send {
                    conn,
                    frame_bytes: bytes,
                    marker: Some(SentMarker::Reply),
                }]
            }
            wire::Negotiated::Refused { supported } => {
                let bytes =
                    wire::encode_attach_server(&AttachServer::HelloRefused { supported }).expect("fixed-shape body");
                vec![Action::Send {
                    conn,
                    frame_bytes: bytes,
                    marker: Some(SentMarker::ReplyThenClose),
                }]
            }
        }
    }

    fn handle_attach(&mut self, conn: ConnId, controller_id: String, now: Instant) -> Vec<Action> {
        if self.watcher_count >= SUBSCRIBER_CAP {
            self.clear_outstanding(conn);
            let bytes = wire::encode_attach_server(&AttachServer::AttachRefused {
                reason: AttachRefusedReason::SubscriberCap,
            })
            .expect("fixed-shape body");
            return vec![Action::Send {
                conn,
                frame_bytes: bytes,
                marker: Some(SentMarker::Reply),
            }];
        }
        self.non_watcher_count = self.non_watcher_count.saturating_sub(1);
        self.watcher_count += 1;
        let checkpoint = if self.checkpoint_slot.is_none() {
            self.checkpoint_slot = Some(conn);
            CheckpointProgress::AwaitingGround {
                deadline: now + GROUND_TIMEOUT,
            }
        } else {
            self.checkpoint_queue.push_back(conn);
            CheckpointProgress::QueuedForSlot
        };
        if let Some(c) = self.conns.get_mut(&conn) {
            c.role = Role::Watcher(WatcherState {
                controller_id,
                checkpoint,
                queued_live_bytes: 0,
            });
        }
        vec![]
    }

    fn handle_take(&mut self, conn: ConnId, controller_id: String) -> Vec<Action> {
        let checkpoint = self.conns.get(&conn).and_then(watcher_checkpoint);
        let reason = match checkpoint {
            None => Some(TakeRefusedReason::NotAttached),
            Some(CheckpointProgress::Done) => None,
            Some(_) => Some(TakeRefusedReason::CheckpointInFlight),
        };
        if let Some(reason) = reason {
            self.clear_outstanding(conn);
            let bytes = wire::encode_attach_server(&AttachServer::TakeRefused { reason }).expect("fixed-shape body");
            return vec![Action::Send {
                conn,
                frame_bytes: bytes,
                marker: Some(SentMarker::Reply),
            }];
        }
        vec![Action::CommitTake { conn, controller_id }]
    }

    fn handle_input(
        &mut self,
        conn: ConnId,
        controller_id: String,
        take_epoch: u64,
        idem_key: [u8; 16],
        payload: Vec<u8>,
    ) -> Vec<Action> {
        let connection_authorized = self.driver.as_ref().map(|d| d.conn) == Some(conn);
        vec![Action::ForwardInput {
            conn,
            controller_id,
            take_epoch,
            idem_key,
            payload,
            connection_authorized,
        }]
    }

    fn handle_resize(&mut self, conn: ConnId, cols: u16, rows: u16) -> Vec<Action> {
        if self.driver.as_ref().map(|d| d.conn) != Some(conn) {
            self.clear_outstanding(conn);
            let bytes = wire::encode_attach_server(&AttachServer::ResizeRefused {
                reason: ResizeRefusedReason::NotDriver,
            })
            .expect("fixed-shape body");
            return vec![Action::Send {
                conn,
                frame_bytes: bytes,
                marker: Some(SentMarker::Reply),
            }];
        }
        vec![Action::ApplyResize { conn, cols, rows }]
    }

    fn handle_keepalive_reply(&mut self, conn: ConnId, nonce: u64, now: Instant) -> Vec<Action> {
        let matches = self
            .driver
            .as_ref()
            .is_some_and(|d| d.conn == conn && d.keepalive_outstanding == Some(nonce));
        if !matches {
            return self.close_with_refusal(conn, RefusalReason::UnexpectedKeepalive, now);
        }
        if let Some(d) = &mut self.driver {
            d.keepalive_outstanding = None;
            d.keepalive_deadline = None;
        }
        if let Some(c) = self.conns.get_mut(&conn) {
            c.last_activity = now;
        }
        vec![]
    }

    fn tick_keepalive(&mut self, now: Instant) -> Vec<Action> {
        let Some(conn) = self.driver.as_ref().map(|d| d.conn) else {
            return vec![];
        };
        // Suspended while ANY checkpoint transfer occupies the writer
        // loop's attention — see the module doc.
        if self.checkpoint_slot.is_some() {
            return vec![];
        }
        let (outstanding, deadline) = {
            let d = self.driver.as_ref().expect("checked above");
            (d.keepalive_outstanding, d.keepalive_deadline)
        };
        if let Some(deadline) = deadline {
            if now >= deadline {
                return self.close_with_refusal(conn, RefusalReason::KeepaliveDeath, now);
            }
            return vec![];
        }
        if outstanding.is_some() {
            // Sent, sent-completion not yet reported: deadline not armed.
            return vec![];
        }
        let idle = self
            .conns
            .get(&conn)
            .is_some_and(|c| now.saturating_duration_since(c.last_activity) >= KEEPALIVE_IDLE_TRIGGER);
        if !idle {
            return vec![];
        }
        self.nonce_counter += 1;
        let nonce = self.nonce_counter;
        if let Some(d) = &mut self.driver {
            d.keepalive_outstanding = Some(nonce);
        }
        vec![Action::Send {
            conn,
            frame_bytes: wire::encode_keepalive(nonce),
            marker: Some(SentMarker::Keepalive { nonce }),
        }]
    }

    // -- shared helpers ------------------------------------------------

    fn mark_outstanding(&mut self, conn: ConnId) {
        if let Some(c) = self.conns.get_mut(&conn) {
            c.outstanding_request = true;
        }
    }

    fn clear_outstanding(&mut self, conn: ConnId) {
        if let Some(c) = self.conns.get_mut(&conn) {
            c.outstanding_request = false;
        }
    }

    fn ground_timeout(&mut self, conn: ConnId, now: Instant) -> Vec<Action> {
        if let Some(c) = self.conns.get_mut(&conn) {
            c.role = Role::PostHello {
                deadline: now + PRE_ADMISSION_TIMEOUT,
            };
            c.outstanding_request = false;
        }
        self.watcher_count = self.watcher_count.saturating_sub(1);
        self.non_watcher_count += 1;
        if self.checkpoint_slot == Some(conn) {
            self.checkpoint_slot = None;
            self.advance_checkpoint_queue(now);
        } else {
            self.checkpoint_queue.retain(|&id| id != conn);
        }
        let bytes = wire::encode_attach_server(&AttachServer::AttachRefused {
            reason: AttachRefusedReason::GroundTimeout,
        })
        .expect("fixed-shape body");
        vec![Action::Send {
            conn,
            frame_bytes: bytes,
            marker: Some(SentMarker::Reply),
        }]
    }

    fn advance_checkpoint_queue(&mut self, now: Instant) {
        if self.checkpoint_slot.is_some() {
            return;
        }
        let Some(next) = self.checkpoint_queue.pop_front() else {
            return;
        };
        self.checkpoint_slot = Some(next);
        if let Some(Role::Watcher(w)) = self.conns.get_mut(&next).map(|c| &mut c.role) {
            w.checkpoint = CheckpointProgress::AwaitingGround {
                deadline: now + GROUND_TIMEOUT,
            };
        }
    }

    fn remove_connection(&mut self, conn: ConnId, now: Instant) {
        let Some(c) = self.conns.remove(&conn) else {
            return;
        };
        match c.role {
            Role::Unclassified { .. } | Role::Mgmt | Role::PostHello { .. } => {
                self.non_watcher_count = self.non_watcher_count.saturating_sub(1);
            }
            Role::Watcher(_) => {
                self.watcher_count = self.watcher_count.saturating_sub(1);
            }
        }
        self.checkpoint_queue.retain(|&id| id != conn);
        if self.checkpoint_slot == Some(conn) {
            self.checkpoint_slot = None;
            self.advance_checkpoint_queue(now);
        }
        if self.driver.as_ref().map(|d| d.conn) == Some(conn) {
            // Capability-only EOF -- no durable transition (ADR 0041).
            self.driver = None;
        }
    }

    fn close_with_refusal(&mut self, conn: ConnId, reason: RefusalReason, now: Instant) -> Vec<Action> {
        self.remove_connection(conn, now);
        vec![
            Action::RecordRefusal {
                conn: Some(conn),
                reason,
            },
            Action::Close(conn),
        ]
    }
}

fn watcher_checkpoint(c: &Conn) -> Option<CheckpointProgress> {
    match &c.role {
        Role::Watcher(w) => Some(w.checkpoint),
        _ => None,
    }
}

/// Slices an encoded checkpoint into `checkpoint_chunk` frames at
/// [`crate::wire::MAX_CHECKPOINT_CHUNK_PAYLOAD`]. The first chunk carries
/// [`SentMarker::Reply`] (the attach success signal); the last carries
/// [`SentMarker::CheckpointLastChunk`] — the SAME chunk when there is only
/// one. A zero-byte checkpoint (never produced in practice — even an empty
/// screen's fixed header is nonzero) still emits exactly one empty `last`
/// chunk, matching the wire's "a non-final `bytes` may legally be empty"
/// rule read at its edge case.
fn chunk_checkpoint(conn: ConnId, bytes: Vec<u8>) -> Vec<Action> {
    let max = wire::MAX_CHECKPOINT_CHUNK_PAYLOAD;
    let total = bytes.len();
    let mut actions = Vec::new();
    let mut offset = 0usize;
    loop {
        let end = (offset + max).min(total);
        let is_last = end == total;
        let chunk = AttachServer::CheckpointChunk {
            last: is_last,
            bytes: bytes[offset..end].to_vec(),
        };
        let encoded =
            wire::encode_attach_server(&chunk).expect("each chunk is capped at MAX_CHECKPOINT_CHUNK_PAYLOAD by construction");
        let marker = if is_last {
            Some(SentMarker::CheckpointLastChunk)
        } else if offset == 0 {
            Some(SentMarker::Reply)
        } else {
            None
        };
        actions.push(Action::Send {
            conn,
            frame_bytes: encoded,
            marker,
        });
        offset = end;
        if is_last {
            break;
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{encode_attach_client, encode_keepalive, encode_mgmt_request, FrameSplitter};

    fn t0() -> Instant {
        Instant::now()
    }

    fn proto() -> AttachProto {
        AttachProto::new(MgmtStatus {
            pid: 4242,
            created: 0x0123_4567_89ab_cdef,
            survival: Survival::Normal,
        })
    }

    fn decode_one(bytes: &[u8]) -> DecodedFrame {
        let mut s = FrameSplitter::new();
        let (frames, err) = s.feed(bytes);
        assert_eq!(err, None);
        assert_eq!(frames.len(), 1);
        frames.into_iter().next().unwrap()
    }

    fn hello_frame() -> DecodedFrame {
        decode_one(&encode_attach_client(&AttachClient::Hello { proto: wire::ATTACH_PROTO_V1 }).unwrap())
    }

    fn attach_frame(controller_id: &str) -> DecodedFrame {
        decode_one(
            &encode_attach_client(&AttachClient::Attach {
                controller_id: controller_id.to_string(),
            })
            .unwrap(),
        )
    }

    fn take_frame(controller_id: &str) -> DecodedFrame {
        decode_one(
            &encode_attach_client(&AttachClient::Take {
                controller_id: controller_id.to_string(),
            })
            .unwrap(),
        )
    }

    /// Drives one connection all the way to a `Done` watcher (the common
    /// setup every take/input/resize/keepalive/budget test needs):
    /// connection_opened -> hello -> attach -> ground_reached ->
    /// checkpoint_ready(1 byte, a single chunk) -> its sent-completion.
    fn attach_to_done(p: &mut AttachProto, conn: ConnId, now: Instant) {
        assert_eq!(p.connection_opened(conn, now), vec![]);
        let a = p.frame(conn, hello_frame(), now);
        assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::Reply), .. }]));
        p.sent(conn, a[0].send_marker(), now);
        let a = p.frame(conn, attach_frame("ctrl"), now);
        assert!(a.is_empty(), "attach should pend, not reply immediately: {a:?}");
        let a = p.ground_reached(now);
        assert!(matches!(a.as_slice(), [Action::BeginCheckpoint { conn: c }] if *c == conn));
        let a = p.checkpoint_ready(conn, vec![0xAB], now);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].send_marker(), Some(SentMarker::CheckpointLastChunk));
        p.sent(conn, a[0].send_marker(), now);
    }

    impl Action {
        fn send_marker(&self) -> Option<SentMarker> {
            match self {
                Action::Send { marker, .. } => marker.clone(),
                _ => panic!("not a Send: {self:?}"),
            }
        }
    }

    // -- lockstep ---------------------------------------------------------

    #[test]
    fn lockstep_violation_closes() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let probe = decode_one(&encode_mgmt_request(&MgmtRequest::Probe).unwrap());
        let a1 = p.frame(1, probe.clone(), now);
        assert!(matches!(a1.as_slice(), [Action::Send { .. }]), "{a1:?}");
        // A second request before the first's reply is reported sent.
        let a2 = p.frame(1, probe, now);
        assert!(
            a2.iter().any(|a| matches!(a, Action::Close(c) if *c == 1)),
            "expected a close on lockstep violation: {a2:?}"
        );
        assert!(a2
            .iter()
            .any(|a| matches!(a, Action::RecordRefusal { reason: RefusalReason::LockstepViolation, .. })));
    }

    #[test]
    fn a_reply_confirmed_sent_clears_the_lockstep_flag_for_the_next_request() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let probe = decode_one(&encode_mgmt_request(&MgmtRequest::Probe).unwrap());
        let a1 = p.frame(1, probe.clone(), now);
        p.sent(1, a1[0].send_marker(), now);
        let a2 = p.frame(1, probe, now);
        assert!(matches!(a2.as_slice(), [Action::Send { .. }]), "{a2:?}");
    }

    // -- caps ---------------------------------------------------------

    #[test]
    fn non_watcher_cap_enforced() {
        let mut p = proto();
        let now = t0();
        for id in 0..4 {
            assert_eq!(p.connection_opened(id, now), vec![]);
        }
        let a = p.connection_opened(4, now);
        assert!(a.iter().any(|x| matches!(x, Action::Close(c) if *c == 4)));
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::NonWatcherCapExceeded, .. })));
    }

    #[test]
    fn subscriber_cap_enforced_and_retryable() {
        let mut p = proto();
        let now = t0();
        for id in 0..4u64 {
            attach_to_done(&mut p, id, now);
        }
        // A 5th connection completes hello, then its attach is refused --
        // but the connection stays open (no Close in the response) and is
        // retryable.
        p.connection_opened(9, now);
        let a = p.frame(9, hello_frame(), now);
        p.sent(9, a[0].send_marker(), now);
        let a = p.frame(9, attach_frame("late"), now);
        assert!(!a.iter().any(|x| matches!(x, Action::Close(_))), "must stay open: {a:?}");
        match a.as_slice() {
            [Action::Send { frame_bytes, marker: Some(SentMarker::Reply), .. }] => {
                let decoded = decode_one(frame_bytes);
                assert_eq!(
                    decoded,
                    DecodedFrame::AttachServer(AttachServer::AttachRefused {
                        reason: AttachRefusedReason::SubscriberCap
                    })
                );
            }
            other => panic!("expected AttachRefused{{SubscriberCap}}: {other:?}"),
        }
        p.sent(9, a[0].send_marker(), now);
        // Retryable: freeing one existing watcher lets the same connection
        // attach successfully afterward.
        p.connection_closed(0, now);
        let a = p.frame(9, attach_frame("late"), now);
        assert!(a.is_empty(), "should now pend for ground, not refuse: {a:?}");
    }

    // -- hello --------------------------------------------------------

    #[test]
    fn hello_refusal_closes_after_the_reply_is_sent() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let bad_hello = decode_one(&encode_attach_client(&AttachClient::Hello { proto: 999 }).unwrap());
        let a = p.frame(1, bad_hello, now);
        assert!(matches!(
            a.as_slice(),
            [Action::Send { marker: Some(SentMarker::ReplyThenClose), .. }]
        ));
        let decoded = decode_one(a[0].send_bytes());
        assert_eq!(
            decoded,
            DecodedFrame::AttachServer(AttachServer::HelloRefused {
                supported: wire::ATTACH_PROTO_V1
            })
        );
        // The connection only actually closes once this reply's
        // sent-completion is reported.
        let after = p.sent(1, a[0].send_marker(), now);
        assert_eq!(after, vec![Action::Close(1)]);
    }

    impl Action {
        fn send_bytes(&self) -> &[u8] {
            match self {
                Action::Send { frame_bytes, .. } => frame_bytes,
                _ => panic!("not a Send: {self:?}"),
            }
        }
    }

    // -- the pen --------------------------------------------------------

    #[test]
    fn take_demotes_the_previous_driver_which_stays_a_watcher() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        attach_to_done(&mut p, 2, now);

        let a = p.frame(1, take_frame("alice"), now);
        assert_eq!(a, vec![Action::CommitTake { conn: 1, controller_id: "alice".into() }]);
        let a = p.take_committed(1, "alice".into(), 1, now);
        p.sent(1, a[0].send_marker(), now);

        // conn 2 takes next -- conn 1 is demoted but its Watcher role
        // survives (still eligible to receive output; just no driver).
        let a = p.frame(2, take_frame("bob"), now);
        assert_eq!(a, vec![Action::CommitTake { conn: 2, controller_id: "bob".into() }]);
        let a = p.take_committed(2, "bob".into(), 2, now);
        p.sent(2, a[0].send_marker(), now);

        // conn 1 can no longer resize (not the driver any more).
        let resize = decode_one(&encode_attach_client(&AttachClient::Resize { cols: 100, rows: 40 }).unwrap());
        let a = p.frame(1, resize, now);
        let decoded = decode_one(a[0].send_bytes());
        assert_eq!(
            decoded,
            DecodedFrame::AttachServer(AttachServer::ResizeRefused {
                reason: ResizeRefusedReason::NotDriver
            })
        );
        // conn 1 is still a subscriber: output still reaches it.
        p.sent(1, a[0].send_marker(), now);
        let out = p.output_committed(b"hello", now);
        assert!(out.iter().any(|a| matches!(a, Action::Send { conn: 1, .. })));
    }

    #[test]
    fn driver_eof_clears_the_capability_only() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        let a = p.frame(1, take_frame("alice"), now);
        let a = p.take_committed(1, "alice".into(), 1, now).into_iter().chain(a).collect::<Vec<_>>();
        // (order doesn't matter here -- just confirm the driver was set)
        let _ = a;

        p.connection_closed(1, now);
        // No durable action is emitted for the EOF -- capability-only.
        // A NEW connection taking (as the very first ever driver) succeeds
        // without any special-casing, proving no stale state lingered.
        attach_to_done(&mut p, 2, now);
        let a = p.frame(2, take_frame("carol"), now);
        assert_eq!(a, vec![Action::CommitTake { conn: 2, controller_id: "carol".into() }]);
    }

    #[test]
    fn stale_input_from_a_non_driver_connection_is_flagged_unauthorized() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        attach_to_done(&mut p, 2, now);
        let a = p.frame(1, take_frame("alice"), now);
        p.take_committed(1, "alice".into(), 1, now);
        let _ = a;

        // conn 2 was never granted the capability; even claiming the SAME
        // (controller_id, take_epoch) as the current driver must not be
        // trusted purely from the wire fields -- the connection itself must
        // hold the capability.
        let input = decode_one(
            &encode_attach_client(&AttachClient::Input {
                controller_id: "alice".into(),
                take_epoch: 1,
                idem_key: [7u8; 16],
                payload: b"ls\n".to_vec(),
            })
            .unwrap(),
        );
        let a = p.frame(2, input, now);
        assert_eq!(
            a,
            vec![Action::ForwardInput {
                conn: 2,
                controller_id: "alice".into(),
                take_epoch: 1,
                idem_key: [7u8; 16],
                payload: b"ls\n".to_vec(),
                connection_authorized: false,
            }]
        );
    }

    #[test]
    fn take_before_attach_is_not_attached() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let a = p.frame(1, hello_frame(), now);
        p.sent(1, a[0].send_marker(), now);
        let a = p.frame(1, take_frame("alice"), now);
        let decoded = decode_one(a[0].send_bytes());
        assert_eq!(
            decoded,
            DecodedFrame::AttachServer(AttachServer::TakeRefused {
                reason: TakeRefusedReason::NotAttached
            })
        );
    }

    /// `take` sent before ground is even reached: lockstep itself blocks
    /// it (the connection's `attach` is still outstanding), which the
    /// lockstep tests already cover. This test proves the DISTINCT
    /// `CheckpointInFlight` rule: a multi-chunk checkpoint's FIRST chunk
    /// already clears lockstep ("the first checkpoint_chunk IS the attach
    /// success signal" — ADR 0041), so a client is free to send `take`
    /// immediately after, while chunks are still streaming — and THAT is
    /// refused, distinctly, until the connection's own final chunk is
    /// reported physically written.
    #[test]
    fn take_while_own_checkpoint_in_flight_is_refused() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let a = p.frame(1, hello_frame(), now);
        p.sent(1, a[0].send_marker(), now);
        p.frame(1, attach_frame("alice"), now);
        p.ground_reached(now);
        let big = vec![0u8; wire::MAX_CHECKPOINT_CHUNK_PAYLOAD + 1];
        let chunks = p.checkpoint_ready(1, big, now);
        assert_eq!(chunks.len(), 2, "expected two chunks for a payload just over the per-chunk cap");
        assert_eq!(chunks[0].send_marker(), Some(SentMarker::Reply));
        assert_eq!(chunks[1].send_marker(), Some(SentMarker::CheckpointLastChunk));
        // Lockstep clears on the FIRST chunk's sent-completion -- a `take`
        // is now legal to send, but the checkpoint transfer isn't Done yet.
        p.sent(1, chunks[0].send_marker(), now);
        let a = p.frame(1, take_frame("alice"), now);
        let decoded = decode_one(a[0].send_bytes());
        assert_eq!(
            decoded,
            DecodedFrame::AttachServer(AttachServer::TakeRefused {
                reason: TakeRefusedReason::CheckpointInFlight
            })
        );
        // Once the final chunk is reported sent, take succeeds.
        p.sent(1, chunks[1].send_marker(), now);
        let a = p.frame(1, take_frame("alice"), now);
        assert_eq!(a, vec![Action::CommitTake { conn: 1, controller_id: "alice".into() }]);
    }

    // -- attach: ground gate + snapshot slot -----------------------------

    #[test]
    fn attach_pends_for_ground_then_begins_checkpoint() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let a = p.frame(1, hello_frame(), now);
        p.sent(1, a[0].send_marker(), now);
        let a = p.frame(1, attach_frame("alice"), now);
        assert!(a.is_empty(), "must pend, not reply, before ground: {a:?}");
        let a = p.ground_reached(now);
        assert_eq!(a, vec![Action::BeginCheckpoint { conn: 1 }]);
    }

    #[test]
    fn attach_pends_for_the_snapshot_slot_behind_another_attach() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let a = p.frame(1, hello_frame(), now);
        p.sent(1, a[0].send_marker(), now);
        p.frame(1, attach_frame("alice"), now); // takes the slot

        p.connection_opened(2, now);
        let a = p.frame(2, hello_frame(), now);
        p.sent(2, a[0].send_marker(), now);
        let a = p.frame(2, attach_frame("bob"), now);
        assert!(a.is_empty(), "queued, no reply yet: {a:?}");
        // Ground now reached: only conn 1 (the slot holder) may proceed.
        let a = p.ground_reached(now);
        assert_eq!(a, vec![Action::BeginCheckpoint { conn: 1 }]);
        let a = p.checkpoint_ready(1, vec![0xAB], now);
        p.sent(1, a[0].send_marker(), now);
        // conn 1's slot is freed and conn 2 is promoted -- but still needs
        // its OWN ground_reached call to actually begin.
        let a = p.ground_reached(now);
        assert_eq!(a, vec![Action::BeginCheckpoint { conn: 2 }]);
    }

    #[test]
    fn attach_ground_timeout_is_retryable() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let a = p.frame(1, hello_frame(), now);
        p.sent(1, a[0].send_marker(), now);
        p.frame(1, attach_frame("alice"), now);

        let later = now + Duration::from_secs(6);
        let a = p.tick(later);
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::Send { marker: Some(SentMarker::Reply), .. })));
        let send = a.iter().find_map(|x| match x {
            Action::Send { frame_bytes, marker, .. } => Some((frame_bytes.clone(), marker.clone())),
            _ => None,
        });
        let (bytes, marker) = send.unwrap();
        let decoded = decode_one(&bytes);
        assert_eq!(
            decoded,
            DecodedFrame::AttachServer(AttachServer::AttachRefused {
                reason: AttachRefusedReason::GroundTimeout
            })
        );
        p.sent(1, marker, later);

        // Retry succeeds: the connection reverted, not lost.
        let a = p.frame(1, attach_frame("alice"), later);
        assert!(a.is_empty());
        let a = p.ground_reached(later);
        assert_eq!(a, vec![Action::BeginCheckpoint { conn: 1 }]);
    }

    // -- keepalive --------------------------------------------------------

    #[test]
    fn keepalive_deadline_starts_at_sent_completion_not_enqueue() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        p.take_committed(1, "alice".into(), 1, now);

        let idle = now + KEEPALIVE_IDLE_TRIGGER;
        let a = p.tick(idle);
        assert_eq!(a.len(), 1);
        let nonce = match &a[0] {
            Action::Send {
                marker: Some(SentMarker::Keepalive { nonce }),
                ..
            } => *nonce,
            other => panic!("expected a Keepalive send: {other:?}"),
        };

        // Even well past 30s WITHOUT the sent-completion ever being
        // reported, the reply deadline must not have started -- no close,
        // no second keepalive.
        let much_later = idle + Duration::from_secs(120);
        let a = p.tick(much_later);
        assert!(a.is_empty(), "deadline must not run before sent-completion: {a:?}");

        // NOW report it sent -- the 30s reply deadline starts here.
        p.sent(1, Some(SentMarker::Keepalive { nonce }), much_later);
        let a = p.tick(much_later + Duration::from_secs(29));
        assert!(a.is_empty(), "must not fire before its own 30s window: {a:?}");
        let a = p.tick(much_later + Duration::from_secs(31));
        assert!(a.iter().any(|x| matches!(x, Action::Close(1))), "expected keepalive death: {a:?}");
    }

    #[test]
    fn keepalive_suspended_during_a_checkpoint_transfer() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        p.take_committed(1, "alice".into(), 1, now);

        // A second connection's attach occupies the global slot.
        p.connection_opened(2, now);
        let a = p.frame(2, hello_frame(), now);
        p.sent(2, a[0].send_marker(), now);
        p.frame(2, attach_frame("bob"), now);
        p.ground_reached(now); // conn 2 begins Sending -- slot occupied

        let idle = now + KEEPALIVE_IDLE_TRIGGER + Duration::from_secs(60);
        let a = p.tick(idle);
        assert!(a.is_empty(), "keepalive must be suspended while a checkpoint is in flight: {a:?}");
    }

    #[test]
    fn keepalive_reply_wrong_nonce_is_unexpected_and_closes() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        p.take_committed(1, "alice".into(), 1, now);
        let idle = now + KEEPALIVE_IDLE_TRIGGER;
        let a = p.tick(idle);
        let real_nonce = match &a[0] {
            Action::Send { marker: Some(SentMarker::Keepalive { nonce }), .. } => *nonce,
            other => panic!("{other:?}"),
        };
        let bogus = decode_one(&encode_keepalive(real_nonce.wrapping_add(1)));
        let a = p.frame(1, bogus, idle);
        assert!(a.iter().any(|x| matches!(x, Action::Close(1))));
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::UnexpectedKeepalive, .. })));
    }

    // -- queue accounting -------------------------------------------------

    #[test]
    fn progress_deadline_fires_on_a_stalled_nonempty_queue() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        let a = p.bytes_queued(1, 1024, now);
        assert!(a.is_empty());
        let a = p.tick(now + Duration::from_secs(29));
        assert!(a.is_empty(), "must not fire early: {a:?}");
        let a = p.tick(now + Duration::from_secs(31));
        assert!(a.iter().any(|x| matches!(x, Action::Close(1))), "{a:?}");
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::ProgressStall, .. })));
    }

    #[test]
    fn queue_overflow_closes_with_no_wire_frame() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        let a = p.bytes_queued(1, WATCHER_LIVE_QUEUE_BUDGET_BYTES + 1, now);
        assert!(a.iter().any(|x| matches!(x, Action::Close(1))));
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::QueueOverflow, .. })));
        assert!(
            !a.iter().any(|x| matches!(x, Action::Send { .. })),
            "no wire frame exists for eviction, by design: {a:?}"
        );
    }

    #[test]
    fn output_committed_reaches_a_done_watcher_and_is_gated_by_the_budget() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        let a = p.output_committed(b"hi", now);
        assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::OutputBytes { n: 2 }), .. }]));
        p.sent(1, a[0].send_marker(), now);
    }
}
