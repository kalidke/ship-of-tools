//! The ADR 0041 step-5 attach protocol: a platform-neutral connection/role
//! state machine over the [`crate::lane::wire`] frames. No I/O, no OS types, no
//! clocks read directly (every timing-relevant method is fed a monotonic
//! `now: Instant`) — the `host_handshake.rs`/`lane/wire/` precedent this crate
//! already follows for a byte/state machine that must run and be tested on
//! every CI leg, not just Windows. THIS module decides; `capsule_win.rs`'s
//! writer loop (the U3 seam: a real named pipe on Windows) executes the
//! [`Action`]s and feeds the [`AttachProto`] events back.
//!
//! # What this module owns, and what it explicitly does not
//!
//! Owned: the connection registry and its role machine; lockstep (one
//! outstanding client request per connection, now request-correlated); the
//! two admission caps and the pre-admission timeout; the ground-gated,
//! single-slot, one-chunk-at-a-time attach/snapshot sequencing; the pen
//! (ephemeral driver capability: demote-on-take, capability-only EOF); the
//! driver keepalive and the generic queue-progress deadline; the
//! per-watcher live-output queue budget; teardown's producer-bound
//! admission revocation.
//!
//! Not owned, deliberately: encoding a checkpoint (`vt100-ctt` is a
//! Windows-only dependency of this crate — this module must build and be
//! tested on every platform), performing the actual OS resize call, reading
//! `pid`/process-creation-time, computing whether a wire input is stale
//! against DURABLE state, or ever writing a WAL frame, or deciding WHEN to
//! stop feeding this module events during teardown. Those all require
//! either OS access, the fsync'd voyage this module never touches, or a
//! resource (the ConPTY handle) this module never holds — they are the
//! loop's job, requested via an [`Action`] and reported back via an event
//! ([`AttachProto::checkpoint_ready`], [`AttachProto::take_committed`],
//! [`AttachProto::resize_outcome`], [`AttachProto::input_outcome`]).
//!
//! # Connections and roles
//!
//! A connection starts [`Role::Unclassified`] the instant it opens — lane
//! and identity are unknown until its first frame arrives (a single named
//! pipe carries both lanes; [`crate::lane::wire::FrameSplitter`] latches which one
//! from the first frame's magic, so by the time a `frame` event reaches this
//! module every later frame from that connection is guaranteed the same
//! lane). The first decoded frame reclassifies it:
//!
//! | first frame                  | new role                              |
//! |-------------------------------|---------------------------------------|
//! | any `MgmtRequest`              | [`Role::Mgmt`] (no further deadline)  |
//! | `AttachClient::Hello` (accepted) | `Role::PostHello` (same deadline)   |
//! | `AttachClient::Hello` (refused)  | closed (`hello_refused` then close)  |
//! | anything else                  | closed — a protocol violation        |
//!
//! `Attach` on a `PostHello` connection promotes it to `Role::Watcher`
//! IMMEDIATELY on admission (before its checkpoint has even started) — "a
//! frontend relaunch is precisely a reconnect, and reconnects arrive as
//! watchers" (ADR 0041), and the subscriber cap must count a
//! ground-pending/queued attach the moment it is admitted, not only once its
//! first byte goes out, or a burst of concurrent attaches could blow past
//! the cap before any of them finish. A connection that never completes
//! `hello`+`attach` within the shared 10 s admission window is closed
//! (`RefusalReason::PreAdmissionTimeout`) — a judgment call: the ADR names
//! this "pre-hello timeout", but a connection that says `hello` and then
//! never attaches is occupying the exact same slot a pre-hello connection
//! does, so the SAME deadline (started once, at `connection_opened`, never
//! reset by a successful `hello`) governs reaching `Watcher`, not merely
//! completing `hello`.
//!
//! # Lockstep and request correlation
//!
//! Every connection tracks `outstanding_request: Option<RequestId>`. A
//! second lockstep-classified client frame while it is `Some(_)` is
//! `RefusalReason::LockstepViolation` — closed, no reply (mirrors that
//! `feed` can decode several frames from one burst read, so this is checked
//! per decoded frame, not per transport read). `keepalive` is exempt (it is
//! not a client "request" in this sense — see below). `mark_outstanding`
//! allocates a fresh id and stores it the instant a lockstep request is
//! accepted; it is cleared only when [`AttachProto::sent`] reports the
//! MATCHING reply's marker physically written (`clear_outstanding_if_matches`)
//! — never merely at the moment this module *decides* the reply, because a
//! real transport can buffer several already-decoded client frames ahead of
//! any reply physically leaving, and never by an unrelated marker's
//! completion (finding 4). For `attach`, that means the flag can stay set
//! through the whole ground-pend + checkpoint transfer; lane/wire/'s own "the
//! first `checkpoint_chunk` IS the attach success signal" is exactly when
//! it clears (the FIRST chunk's `clears_request`), not when the LAST chunk
//! goes out — a client is free to send `take` while its own checkpoint is
//! still streaming (though `take` has an independent admission rule for
//! that case; see below).
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
//! reverting. `ground_timeout`'s own demotion back into the non-watcher
//! pool is ALSO cap-checked (finding 12): a timed-out attach demotes back to
//! `PostHello` only if there is room; otherwise it closes instead, after its
//! refusal is sent.
//!
//! # The ground-gated, streamed attach and the one-slot checkpoint transfer
//!
//! `attach` reserves a `Watcher` slot immediately, then either takes the one
//! global `checkpoint_slot` (if free) and starts its own 5 s ground-wait
//! deadline, or joins `checkpoint_queue` with NO deadline yet — "a second
//! attach pends for the SLOT", not for ground; its own clock only starts
//! once it becomes the slot holder. [`AttachProto::ground_reached`] (fed by
//! the loop after a group-commit where `parser.is_ground()`) requests
//! [`Action::BeginCheckpoint`] for the current slot holder if it is waiting;
//! the loop encodes (`Screen::checkpoint()`, which this module cannot call)
//! and hands the bytes back via [`AttachProto::checkpoint_ready`], which
//! wraps them in an `Arc<Vec<u8>>` ONCE (moving the buffer, never copying
//! it a second time -- finding 9) and streams them ONE CHUNK AT A TIME —
//! each chunk's `sent`-completion requests the next
//! (`advance_checkpoint_stream`) — at
//! [`crate::lane::wire::MAX_CHECKPOINT_CHUNK_PAYLOAD`] per chunk, marking the
//! first `clears_request: Some(_)` and the last `is_last: true` (one chunk
//! carries both when there is only one). A [`GROUND_TIMEOUT`] (5 s) with no
//! `ground_reached` DEMOTES the connection back to `PostHello` (subject to
//! the cap check above; freeing both its `Watcher` slot and the checkpoint
//! slot, which then advances the queue) and replies `attach_refused
//! {GroundTimeout}` — explicitly retryable, per the ADR.
//!
//! `take`'s own [`crate::lane::wire::TakeRefusedReason::CheckpointInFlight`] is a
//! DIFFERENT rule from the slot: it fires only when the REQUESTING
//! connection's OWN checkpoint has not yet finished (i.e. it is not yet
//! `CheckpointProgress::Done`) — "refused until the taker's final chunk is
//! REPORTED physically written" (ADR 0041) names the taker's own transfer,
//! not some unrelated connection's. `Done` is reached only via the final
//! chunk's sent-completion, never merely having chunked the bytes.
//!
//! Output committed for a `Watcher` whose checkpoint is not yet `Done`
//! queues in `WatcherState::pending_post_watermark` (finding 3) — accounted
//! against the live-output budget at enqueue time, exactly as if it had
//! been sent immediately — and is flushed, in order, the instant the final
//! chunk's completion marks the connection `Done`. Nothing committed after
//! the watermark is ever silently dropped for a slow-to-transfer
//! subscriber. That queue also accumulates everything committed while the
//! connection was `QueuedForSlot`/`AwaitingGround` — i.e. everything the
//! checkpoint itself is about to encode — so `checkpoint_ready` PURGES it
//! the moment it takes the snapshot (real CI bug, PR #139 discharge round:
//! left unpurged, that backlog is a duplicate of the checkpoint's own
//! grid, redelivered a second time once `Done`). From that purge onward
//! the checkpoint and the queue are non-overlapping halves of the same
//! committed timeline.
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
//! entry is untouched, it simply stops being able to pass the driver check,
//! and its keepalive nonce is simply discarded along with the rest of the
//! old `DriverState` (a late echo for it is ignored, not fatal — see
//! keepalive below). `input`/`resize` from any connection that is not
//! `self.driver`'s current holder is refused (`input`: folded into the same
//! "stale" wire reply the ADR already defines, since a replayed identity
//! from a connection lacking the capability is indistinguishable on the
//! wire from a stale epoch — see [`Action::ForwardInput`]'s doc; `resize`:
//! `NotDriver`). A connection's close ([`AttachProto::connection_closed`],
//! or this module's own `close_with_refusal`) clears `self.driver` ONLY
//! if it was that connection — capability-only EOF, no durable transition,
//! per the ADR's spec-gate deletion of the old local-grant behavior.
//!
//! # Keepalive and the generic progress deadline
//!
//! Driver-only. `tick` starts ONE `keepalive` after 30 s since the driver
//! connection's `last_activity` (any inbound frame, or any `sent`
//! completion) with none currently outstanding. The reply deadline (30 s)
//! is armed at [`SentMarker::Keepalive`]'s sent-completion, not at enqueue
//! — a ping stuck behind a real backlog must not kill a healthy reader
//! before its bytes even left. Nonces retire by CONNECTION
//! (`Conn::last_keepalive_nonce`), not by "is this the current driver"
//! (round-2 review, finding 3): a nonce this connection was NEVER issued
//! is `UnexpectedKeepalive` regardless of role; a nonce it WAS issued,
//! echoed after it stopped being the actionable one (demoted, retaken by
//! the SAME connection, or already answered), is a recognized late echo —
//! ignorable, not fatal. Independently, ANY connection with
//! `outstanding_sends > 0`
//! whose `last_send_progress` is more than 30 s old is `ProgressStall` —
//! closed (finding 5: this is the queue-liveness bound, distinct from
//! keepalive, and it covers EVERY kind of outstanding send — checkpoint
//! chunks, replies, keepalives, shutdown acks, live output — not only live
//! output; the clock resets at the empty→nonempty transition and on every
//! completion, so an idle connection is never penalized for having been
//! idle).
//!
//! # Queue accounting
//!
//! `queued_live_bytes` (LIVE output only — a `Watcher`'s field, tracked
//! whether or not its checkpoint is `Done`, since post-watermark output
//! queues behind an in-flight transfer rather than skipping the budget) is
//! incremented by [`AttachProto::bytes_queued`] (called internally by
//! [`AttachProto::output_committed`] for every watcher, and directly by a
//! caller/test that wants to drive the bound explicitly) and decremented by
//! [`AttachProto::sent`]'s [`SentMarker::OutputBytes`] arm, whenever that
//! batch's frame actually completes (immediately if `Done`, later — once
//! flushed from `pending_post_watermark` — if not). Checkpoint bytes never
//! touch this counter (decision 5: "the checkpoint work item rides OUTSIDE
//! this budget"). Overflow closes with no wire frame — "no `evicted` frame
//! exists on the wire, deliberately" — logged only via
//! [`Action::RecordRefusal`]. This is a MEMORY bound, independent of the
//! `outstanding_sends` TIME bound above.

use crate::lane::wire::{
    self, AttachClient, AttachRefusedReason, AttachServer, DecodedFrame, MgmtReply, MgmtRequest,
    ResizeRefusedReason, Survival, TakeRefusedReason,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A caller-assigned, opaque connection identifier. This module attaches no
/// meaning to the value beyond identity — the loop (real pipe handle, or a
/// test transport's own counter) owns the numbering.
pub type ConnId = u64;

/// An id this module allocates per accepted lockstep request
/// (`mark_outstanding`), so the eventual reply's marker can name exactly
/// which request it answers (finding 4: markers must correlate to their
/// request, not clear whatever happens to be outstanding).
pub type RequestId = u64;

const NON_WATCHER_CAP: usize = 4;
const SUBSCRIBER_CAP: usize = 4;
const PRE_ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);
const GROUND_TIMEOUT: Duration = Duration::from_secs(5);
const KEEPALIVE_IDLE_TRIGGER: Duration = Duration::from_secs(30);
const KEEPALIVE_REPLY_DEADLINE: Duration = Duration::from_secs(30);
/// ADR 0041 bounds table ("mgmt idle", role "pool squatting") / Lifecycle
/// "Two capsule-side changes": an ADMITTED mgmt connection (`Role::Mgmt` —
/// classified by its first frame, not the pre-classification
/// `PRE_ADMISSION_TIMEOUT` above) that sends nothing for this long is
/// closed. Before this, four idle mgmt clients could hold a healthy
/// capsule at [`NON_WATCHER_CAP`] forever while every new probe was
/// refused outright — nothing legitimate holds an idle mgmt connection
/// open now that the death signal is the retained challenged-process
/// handle (U1a), not a live pipe a prober can lean on. Measured off the
/// SAME `last_activity` clock every inbound frame and outbound completion
/// already resets (U1a introduces no second clock) — an ACTIVE mgmt
/// client, one that sends a request at least this often, is never
/// touched (out of scope per the ADR: it is the owner, whom the threat
/// model excludes).
const MGMT_IDLE_DEADLINE: Duration = Duration::from_secs(5);
/// The generic write-progress deadline (finding 5): ANY connection with a
/// nonempty outstanding-sends count must see a completion within this
/// window, covering every kind of send — not only live output.
const PROGRESS_DEADLINE: Duration = Duration::from_secs(30);
/// ADR 0041 budget table: "per-watcher queue 4 MiB, overflow = eviction" —
/// LIVE output only (decision 5: the checkpoint work item rides outside it).
/// This is the WATCHER row specifically — the table's DRIVER row is a
/// different number with a different consequence ("committed driver-visible
/// bytes are never dropped while the connection is live... a hung driver
/// cannot wedge the writer loop"), so [`AttachProto::bytes_queued`] never
/// applies this eviction to whichever connection currently holds the
/// driver capability, even though it is still, underneath, a `Watcher`.
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

/// One `Watcher`'s checkpoint-transfer progress. `Sending` streams ONE
/// chunk at a time (finding 10): `bytes` is the full encoded checkpoint,
/// wrapped in `Arc` exactly once so advancing the cursor never re-clones
/// it; `offset` is where the NEXT chunk to be emitted starts.
#[derive(Debug, Clone)]
enum CheckpointProgress {
    /// Waiting for the global slot; no deadline yet (see module doc).
    QueuedForSlot,
    /// Holds the slot, waiting for a ground boundary.
    AwaitingGround { deadline: Instant },
    /// Streaming; `offset` names the start of the chunk currently in
    /// flight (sent, awaiting its own completion).
    /// `Arc<Vec<u8>>`, not `Arc<[u8]>` (round-2 review, finding 9):
    /// converting an owned `Vec<u8>` into an `Arc<[u8]>` always
    /// allocates a fresh, differently-shaped buffer and copies into it
    /// (an unsized `Arc<[T]>`'s allocation has to combine the refcount
    /// header with the slice data in one block, which a `Vec`'s own
    /// allocation was never laid out for) -- confirmed by the
    /// reviewer's own pointer probe. `Arc<Vec<u8>>` wraps the `Vec`
    /// struct itself (ptr/len/cap) in a new, SEPARATE, small Arc
    /// allocation without ever touching the multi-MiB buffer it
    /// points at, so the checkpoint's bytes are moved into the Arc
    /// exactly once, never briefly duplicated.
    Sending { bytes: Arc<Vec<u8>>, offset: usize },
    /// The final chunk was reported physically written.
    Done,
}

/// One event queued behind an in-flight checkpoint transfer for a
/// watcher not yet `Done` — `output_committed`'s own mechanism (finding
/// 3), extended by ADR 0046 decision 3 (lane B3b1) to carry
/// `PenChanged`/`Geometry` alongside `Output`, in the exact order
/// emitted, so a watcher still mid-transfer never applies a pen/geometry
/// change before its own checkpoint's `restore_screen` (which would
/// otherwise silently overwrite `Geometry` with the checkpoint's own
/// baked-in dimensions — the bug this queue extension exists to close).
/// Drained, in order, immediately after that watcher's own final
/// checkpoint chunk (see `AttachProto::sent`'s `CheckpointChunk` arm).
#[derive(Debug, Clone)]
enum QueuedPostWatermark {
    Output(Vec<u8>),
    PenChanged { holder: Option<String>, take_epoch: u64 },
    Geometry { cols: u16, rows: u16 },
}

impl QueuedPostWatermark {
    fn into_attach_server(self) -> AttachServer {
        match self {
            Self::Output(bytes) => AttachServer::Output { bytes },
            Self::PenChanged { holder, take_epoch } => AttachServer::PenChanged { holder, take_epoch },
            Self::Geometry { cols, rows } => AttachServer::Geometry { cols, rows },
        }
    }
}

#[derive(Debug, Clone)]
struct WatcherState {
    /// The `attach` request this connection is still owed a reply for —
    /// consumed (as `clears_request`) by the FIRST checkpoint chunk this
    /// watcher ever streams, or by an `AttachRefused` if it never gets
    /// that far (ground timeout, subscriber cap).
    attach_request_id: RequestId,
    checkpoint: CheckpointProgress,
    /// Budget-checked (see module doc's "Queue accounting") — tracked
    /// regardless of checkpoint state. Originally live output only;
    /// lane B3b1's `PenChanged`/`Geometry` additions are charged here
    /// too (Codex review round, should-fix 5), by their own encoded
    /// size, via the exact same `AttachProto::bytes_queued` call and
    /// overflow path ordinary output already uses — "rare and small" is
    /// not itself a bound, so they share the ONE budget rather than
    /// riding an unbounded list of their own.
    queued_live_bytes: u64,
    /// Events committed while `checkpoint` is not yet `Done` (finding 3,
    /// extended by lane B3b1 — see [`QueuedPostWatermark`]'s own doc):
    /// queued behind the transfer, never dropped, flushed in order once
    /// the final chunk's completion marks this watcher `Done`.
    pending_post_watermark: VecDeque<QueuedPostWatermark>,
}

#[derive(Debug, Clone)]
struct Conn {
    role: Role,
    outstanding_request: Option<RequestId>,
    /// Set the instant `make_send` queues the marker that will eventually
    /// clear `outstanding_request` (`Reply`/`ReplyThenClose`/`ShutdownAck`/
    /// `CheckpointChunk`'s first-chunk `clears_request`) — round-2 e2e
    /// review finding 1: distinguishes "the reply already exists, so a
    /// next frame is a benign transport race" (HOLD it, see `held_frame`)
    /// from "no reply exists yet at all" (a genuine lockstep violation) —
    /// something `outstanding_sends != 0` alone cannot do, since a
    /// connection can have UNRELATED sends (live output, an earlier
    /// keepalive) in flight at the same time. Cleared alongside
    /// `outstanding_request` in `clear_outstanding_if_matches`.
    reply_queued: bool,
    /// At most one frame, held when it arrives for a connection whose
    /// `reply_queued` is true (see `frame`'s own doc) — replayed through
    /// `frame` itself the moment the matching completion clears
    /// `outstanding_request` (`clear_outstanding_and_replay`). A SECOND
    /// frame arriving while one is already held is a genuine lockstep
    /// violation, not a race: a real client waits for exactly one reply
    /// before sending its next request.
    held_frame: Option<DecodedFrame>,
    /// Any inbound frame, or any `sent` completion — the keepalive
    /// idle-trigger clock.
    last_activity: Instant,
    /// How many `Send`s this connection has outstanding right now — EVERY
    /// kind (finding 5), not just live output. Zero means nothing to make
    /// progress on; `tick`'s stall check only ever looks at connections
    /// where this is nonzero.
    outstanding_sends: u64,
    /// Reset on every `sent` completion AND at the empty→nonempty
    /// transition when a new `Send` is issued (never left stale from a
    /// long-idle period, which is exactly finding 5's "queue first
    /// becoming nonempty after 30 idle seconds closes immediately" bug).
    last_send_progress: Instant,
    /// The last keepalive nonce ever issued to THIS connection while it
    /// was the driver, whether or not it still is (round-2 review, finding
    /// 3) — survives a same-connection retake (`take_committed` preserves
    /// it rather than discarding it with a fresh `DriverState`), so a late
    /// echo of it is recognizable and ignorable regardless of whether the
    /// nonce is still the CURRENTLY outstanding one. A connection this is
    /// `None` for was never issued anything: any keepalive from it is a
    /// genuine protocol violation, not a routine "some other connection's
    /// late echo" to wave through.
    last_keepalive_nonce: Option<u64>,
    /// The attach-lane protocol version this connection negotiated at
    /// `hello` (Codex round on #194, finding 1 — "attach proto v2 bound
    /// to checkpoint v2"). Defaults to [`wire::ATTACH_PROTO_V1`], the
    /// same value a connection that has not said hello yet would be
    /// refused down to if it tried anything else — never read before a
    /// successful hello sets it for real, since `BeginCheckpoint`
    /// structurally cannot fire before then (a connection reaches
    /// `Watcher` only via a hello-accepted `PostHello`).
    attach_proto_version: u32,
}

/// ADR 0046 decision 3 (lane B3b1): `controller_id`/`take_epoch`
/// reintroduced here — finding 15
/// deleted them as dead fields with "producers but no consumers," since
/// the durable holder/epoch lived only capsule-side (`FrameCtx::holder`/
/// `take_epoch`). This lane is their first consumer: a v3 watcher's
/// `PenSnapshot` (sent on ITS OWN attach, after its final checkpoint
/// chunk) must report who is driving RIGHT NOW, which is exactly this
/// ephemeral, in-memory state — deliberately NOT the same thing as the
/// durable historical holder a resumed voyage's journal may still
/// remember from a driver that has since disconnected (`remove_connection`
/// clears this on EOF with no durable transition, same as before this
/// lane; a fresh `AttachProto` after a capsule restart starts with
/// `driver: None` regardless of what the journal says). A caller wanting
/// the durable value still reads `FrameCtx::holder` — this module never
/// duplicates that; it only tracks the ephemeral capability, one step
/// further than before, now that something (`PenSnapshot`/`PenChanged`)
/// actually reads it back.
#[derive(Debug, Clone)]
struct DriverState {
    conn: ConnId,
    controller_id: String,
    take_epoch: u64,
    keepalive_outstanding: Option<u64>,
    /// Armed only once the ping's sent-completion is reported (ADR 0041:
    /// "not at enqueue").
    keepalive_deadline: Option<Instant>,
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
    next_request_id: u64,
    /// Set once by [`AttachProto::begin_teardown`] (finding 7): from then
    /// on, `take`/`input`/`resize` are silently ignored rather than
    /// admitted — producer-bound admission revocation. `hello`/`attach`/
    /// mgmt are unaffected; this module has no opinion on whether the
    /// caller keeps feeding it events past this point.
    teardown: bool,
    /// True while `output_committed`/`broadcast_pen_changed`/
    /// `broadcast_geometry` is actively iterating its own connection
    /// snapshot — see [`AttachProto::begin_broadcast`]'s own doc for why
    /// a broadcast must never be re-entered while one is already
    /// running.
    broadcasting: bool,
    /// A driver eviction [`AttachProto::remove_connection`] discovered
    /// WHILE `broadcasting` (Codex round-2 review, blocker): an overflow
    /// closing the NEW driver's own connection mid-broadcast used to
    /// recurse into `broadcast_pen_changed(None, ..)` immediately, and
    /// the OUTER broadcast then resumed publishing its own now-stale
    /// `Some(holder)` to watchers it had not reached yet — a surviving
    /// watcher could see `None` then `Some(the connection that was just
    /// evicted)`, wrong indefinitely. Deferred here instead;
    /// [`AttachProto::end_broadcast`] drains and publishes it as a
    /// FRESH, non-nested broadcast once the outer one has fully settled,
    /// so every watcher sees at most one transition, in the correct
    /// final order. At most one entry is ever pending: only one
    /// connection can be `self.driver` at a time, and evicting it clears
    /// `self.driver` immediately (this field defers only the
    /// ANNOUNCEMENT, never the state change itself), so no second
    /// driver eviction can occur before this one is drained.
    deferred_driver_eviction: Option<u64>,
}

fn watcher_checkpoint(c: &Conn) -> Option<CheckpointProgress> {
    match &c.role {
        Role::Watcher(w) => Some(w.checkpoint.clone()),
        _ => None,
    }
}

mod actions;
mod bookkeeping;
mod dispatch;
mod events;

pub use actions::*;

#[cfg(test)]
mod support_tests;
#[cfg(test)]
mod lockstep_tests;
#[cfg(test)]
mod attach_tests;
#[cfg(test)]
mod liveness_tests;
#[cfg(test)]
mod v3_tests;
