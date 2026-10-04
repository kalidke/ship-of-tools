//! The attach protocol's output vocabulary: what a send means (`SentMarker`), why a request is refused
//! (`RefusalReason`), the input result and the `Action`s the leg executes.

use super::*;

/// What a physically-completed [`Action::Send`] means beyond "one fewer
/// thing in flight" — see the module doc sections on lockstep, the ground
/// gate, and keepalive for why each variant exists. `Reply`/
/// `ReplyThenClose`/`ShutdownAck`/`CheckpointChunk`'s `clears_request`/
/// (`request_id`) all carry the [`RequestId`] they answer (finding 4):
/// [`AttachProto::sent`] only clears lockstep if it still matches the
/// connection's CURRENT outstanding request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SentMarker {
    /// This send fully satisfies request `request_id`'s outstanding
    /// lockstep obligation, IF it is still outstanding.
    Reply { request_id: RequestId },
    /// As `Reply`, and the connection must close once this reply is
    /// physically written (`hello_refused`, or a ground-timeout refusal
    /// that lost the non-watcher-cap race).
    ReplyThenClose { request_id: RequestId },
    /// The mgmt `shutdown_ok` reply: clears `request_id` (if still
    /// outstanding), tells the loop to begin EndRun, and closes this
    /// connection — "the shutdown ack is physically written before
    /// teardown closes its connection" (ADR 0041).
    ShutdownAck { request_id: RequestId, reason: String },
    /// One `checkpoint_chunk` in a streamed transfer (finding 10: never
    /// all of them at once). `clears_request` is `Some(_)` only on the
    /// FIRST chunk (the attach success signal); `is_last` is true only on
    /// the actual final chunk — the same chunk carries both when there is
    /// only one. A non-final chunk's completion requests the next one; the
    /// final chunk's completion marks the watcher `Done`, frees the global
    /// slot, and flushes anything queued behind it (finding 3).
    CheckpointChunk {
        clears_request: Option<RequestId>,
        is_last: bool,
    },
    /// The server-originated keepalive echo request: arms its 30 s reply
    /// deadline NOW.
    Keepalive { nonce: u64 },
    /// A live `output` frame carrying `n` raw payload bytes (the SAME
    /// count [`AttachProto::bytes_queued`] was already given when this
    /// batch was enqueued — carried on the marker itself, not re-derived
    /// from the encoded frame's length, so the two can never drift out of
    /// sync): decrements the sending watcher's queued-byte counter by `n`.
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
    /// Same mechanism as `QueueOverflow` (the connection is closed; no
    /// wire frame exists for either, by design), distinct label only so
    /// step 6's adoption UX and any operator-facing log can tell "a
    /// passive watcher never drained" apart from "the driver itself
    /// could not keep up with its own producer" (round-2 review, finding
    /// 2 — see `bytes_queued`'s doc for the ADR reading this restores).
    DriverQueueOverflow,
    ProgressStall,
    KeepaliveDeath,
    UnexpectedKeepalive,
    /// U1a: an admitted mgmt connection went silent past
    /// [`MGMT_IDLE_DEADLINE`] (ADR 0041 bounds table, "mgmt idle" — "pool
    /// squatting").
    MgmtIdleTimeout,
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
/// [`AttachProto::resize_outcome`], [`AttachProto::input_outcome`]) —
/// carrying `request_id` through so the reply can be correlated.
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
    CommitTake {
        conn: ConnId,
        controller_id: String,
        request_id: RequestId,
    },
    /// Execute the full ADR 0039 input WAL for one wire `input` frame:
    /// dedupe-check `idem_key` against the store's index (folded once at
    /// open, kept live) → per the lattice, either append `input` as
    /// a new entry or (a `{input}`-only retry) reuse the ORIGINAL input's
    /// identity without writing a second `input` frame → the LAST-MOMENT
    /// recheck of `(controller_id, take_epoch)` against DURABLE state
    /// (`connection_authorized` is this module's connection-scoped half of
    /// that same check — the ADR requires BOTH "the capability AND the
    /// durable holder/epoch"; a connection lacking the capability cannot
    /// possibly also match the durable identity, but the loop's own
    /// durable comparison is the actual source of truth and must run
    /// regardless) → if STALE (by either check): commit
    /// `{input, refused_stale_epoch}` under one fsync directly, `forward_intent`
    /// NEVER committed — this is exactly why the lattice's refused chain is
    /// `{input, refused}`, with no intent in it; if FRESH: commit
    /// `input_fact:forward_intent` (one fsync covers it and `input`) → forward
    /// syscall → append `forwarded`, which the loop's next commit covers
    /// (ADR 0039 Durability invariants). A `{input,intent}`-only chain (crash-in-flight,
    /// or a duplicate that reached exactly that far) replies
    /// `DeliveryUnknown` and appends nothing further. Report the outcome
    /// via [`AttachProto::input_outcome`]. Never emitted once
    /// [`AttachProto::begin_teardown`] has run (finding 7).
    ForwardInput {
        conn: ConnId,
        controller_id: String,
        take_epoch: u64,
        idem_key: [u8; 16],
        payload: Vec<u8>,
        connection_authorized: bool,
        request_id: RequestId,
    },
    /// Run the existing step-4 ordered resize exchange (request commit →
    /// one `ResizePseudoConsole` call, skipped if out of budget → parser +
    /// geometry updated only on success → outcome commit) — unchanged by
    /// this unit. Report the outcome via [`AttachProto::resize_outcome`].
    /// Never emitted once [`AttachProto::begin_teardown`] has run (finding
    /// 7) — the ConPTY handle needed to perform it may already be gone by
    /// then regardless.
    ApplyResize {
        conn: ConnId,
        cols: u16,
        rows: u16,
        request_id: RequestId,
    },
    /// ADR 0041 EndRun steps 1-2 (the durable marker + its irrevocable
    /// latch) — emitted alongside (BEFORE, in the same returned `Vec`) the
    /// mgmt `shutdown_ok` ack's `Send`, from the SAME `shutdown` request
    /// that produced it, so the writer loop appends and fsyncs one
    /// `run_end_requested {reason}` lifecycle frame and IRREVOCABLY
    /// latches EndRun before that ack is ever queued for the transport. A
    /// concurrent second `shutdown` (this epoch already latched) writes
    /// no second marker — the loop is the one place that knows whether it
    /// already has (step 4: first commit wins, every later request is
    /// acked regardless). `reason` is the client-supplied string,
    /// verbatim — also recorded in `producer_dead`'s detail.
    RunEndRequested { reason: String },
    /// Begin THE TEARDOWN ITSELF: the mgmt `shutdown_ok` ack has already
    /// been reported physically written. Distinct from
    /// `RunEndRequested` — the durable marker is committed at REQUEST
    /// time (above); this fires only once its ack has physically
    /// shipped, and is what the loop's own `shutdown_requested` flag
    /// (teardown start) reacts to. `reason` is the client-supplied
    /// string, to be recorded in `producer_dead`'s detail.
    Shutdown { reason: String },
    /// Diagnostic only — log why `conn` (`None` for a cap refusal with no
    /// connection yet, though today every caller has one) was refused or
    /// closed. Always paired with a `Close` in the same batch.
    RecordRefusal {
        conn: Option<ConnId>,
        reason: RefusalReason,
    },
}
