//! The ADR 0041 step-5 attach protocol: a platform-neutral connection/role
//! state machine over the [`crate::wire`] frames. No I/O, no OS types, no
//! clocks read directly (every timing-relevant method is fed a monotonic
//! `now: Instant`) — the `host_handshake.rs`/`wire.rs` precedent this crate
//! already follows for a byte/state machine that must run and be tested on
//! every CI leg, not just Windows. THIS module decides; `capsule_win.rs`'s
//! writer loop (the U3 seam: a real named pipe on Windows) executes the
//! [`Action`]s and feeds the [`AttachProto`] events back.
//!
//! ## Rework round (Codex adversarial review, U2)
//!
//! The first version of this file shipped six protocol blockers; this is
//! the corrected version. What changed, and why — cited by finding number
//! so the review thread and this code stay traceable to each other:
//!
//! - **Finding 4 (lockstep cleared early, by unrelated markers).** Every
//!   accepted client request now allocates a [`RequestId`] ([`AttachProto::mark_outstanding`]
//!   returns it); `Conn::outstanding_request` is `Option<RequestId>`, not a
//!   bare bool. A reply's marker CARRIES the request id it answers
//!   (`Reply`/`ReplyThenClose`/`ShutdownAck`'s `request_id`, a
//!   `CheckpointChunk`'s `clears_request`), and [`AttachProto::sent`] only
//!   clears the flag if it still names the SAME request
//!   (`clear_outstanding_if_matches`) — never unconditionally, and never
//!   before the reply's bytes are reported physically written. The
//!   previous version cleared refusals immediately at decision time and let
//!   a checkpoint's LAST chunk clear whatever request happened to be
//!   outstanding on that connection at that moment, including one from an
//!   unrelated LATER request (e.g. a `take` sent after the attach's own
//!   reply had already cleared) — request correlation closes both holes.
//! - **Finding 3 + 10 (checkpoint transfer loses output; doubles the
//!   transient).** A `Watcher`'s checkpoint now streams ONE chunk at a
//!   time ([`CheckpointProgress::Sending`] holds a shared `Arc<Vec<u8>>`
//!   (round-2 review, finding 9: `Arc<[u8]>` always copies on conversion
//!   from an owned `Vec`; `Arc<Vec<u8>>` moves it) plus
//!   a cursor; [`AttachProto::sent`]'s `CheckpointChunk` arm requests the
//!   NEXT chunk on every non-final completion) instead of materializing
//!   every chunk up front — the peak transient is the one shared buffer
//!   plus at most one in-flight chunk copy, not the buffer plus a second
//!   complete copy of it. Live output committed while a watcher is not yet
//!   `Done` now queues in `WatcherState::pending_post_watermark` (still
//!   budget-accounted at enqueue time via `bytes_queued`) instead of being
//!   silently dropped for that subscriber; the final chunk's completion
//!   flushes it as ordinary `Output` sends.
//! - **Finding 5 (progress deadline saw only live output).** Every
//!   connection now tracks `outstanding_sends: u64` and
//!   `last_send_progress: Instant`, updated by [`AttachProto::sent`] for
//!   EVERY marker (chunks, replies, keepalives, shutdown acks — everything
//!   a `make_send` ever emits), with the clock explicitly reset at the
//!   empty→nonempty transition (not merely at each completion) so a queue
//!   that stays empty for a long time is never penalized the instant one
//!   item finally arrives. `tick`'s stall check is now `outstanding_sends >
//!   0`, replacing the old watcher-only, live-byte-only check — a stalled
//!   checkpoint, a non-reading mgmt client, a lost keepalive, and an
//!   unconfirmed shutdown ack are now ALL bounded by the same 30 s rule,
//!   which is what actually frees the checkpoint slot / admission-cap slot
//!   a stalled connection was holding.
//! - **Finding 6 (keepalive suspension: wrong scope) / round-2 finding 3
//!   (nonce retirement lost across a same-connection retake).**
//!   `tick_keepalive`'s original fix scoped suspension to the DRIVER
//!   CONNECTION'S OWN in-flight transfer only — never to an unrelated
//!   connection merely occupying the global slot. Round-2 review deleted
//!   that whole suspend/freeze branch outright (with `DriverState.
//!   last_tick`, which existed only to compute it): `take` already refuses
//!   `CheckpointInFlight` until a connection's OWN transfer is `Done`, and
//!   nothing ever moves a watcher's checkpoint backward out of `Done`, so
//!   the branch was unreachable in every real path, not merely rare — kept
//!   "for fidelity" is not a reason once deletion pressure is applied to
//!   it. Separately, [`AttachProto::handle_keepalive_reply`] now retires a
//!   nonce by CONNECTION, via `Conn::last_keepalive_nonce`, not by "is this
//!   still the current `DriverState`": a same-connection retake used to
//!   discard the outstanding nonce along with the rest of `DriverState`,
//!   so that connection's own later late echo of it looked identical to a
//!   fabricated one and was closed as `UnexpectedKeepalive` — a real
//!   round-2 finding, reproduced by the reviewer's own state-machine probe.
//!   Now: a nonce this connection was NEVER issued is a protocol violation
//!   regardless of role (a watcher that was never driver echoing anything
//!   is closed, not silently waved through); a nonce it WAS issued, echoed
//!   after it stopped being the actionable one (demoted, retaken, or
//!   already answered), is a recognized but no longer actionable late echo
//!   — ignorable, not fatal.
//! - **Finding 12 (ground-timeout demotion could exceed the non-watcher
//!   cap).** `ground_timeout` now checks `non_watcher_count` against
//!   [`NON_WATCHER_CAP`] before demoting a timed-out attach back to
//!   `PostHello`; over cap, it closes the connection instead (still after
//!   its `AttachRefused` reply is physically sent).
//! - **Finding 7 (teardown revocation scope).** `self.teardown` is a new
//!   flag ([`AttachProto::begin_teardown`]) covering ONLY producer-bound
//!   admission: once set, `take`/`input`/`resize` are silently ignored
//!   (the lockstep slot they briefly held is released immediately, since
//!   no reply will ever come). `hello`/`attach`/mgmt `probe`/`status`/
//!   `shutdown` are UNCHANGED by this flag — this module has no opinion on
//!   when the caller stops feeding it events; see `capsule_win.rs`'s module
//!   doc for the loop-side half of this (a reduced action-execution set
//!   during teardown, since the ConPTY handle needed for `ApplyResize` is
//!   gone by then regardless).
//! - **Finding 15 (dead fields).** `WatcherState.controller_id` and
//!   `DriverState.controller_id`/`take_epoch` are deleted — they had
//!   producers but no consumers; the durable holder/epoch lives
//!   capsule-side (`FrameCtx.holder`/`take_epoch` in `capsule_win.rs`), and
//!   nothing here ever needed a second copy.
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
//! pipe carries both lanes; [`crate::wire::FrameSplitter`] latches which one
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
//! through the whole ground-pend + checkpoint transfer; wire.rs's own "the
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
//! [`crate::wire::MAX_CHECKPOINT_CHUNK_PAYLOAD`] per chunk, marking the
//! first `clears_request: Some(_)` and the last `is_last: true` (one chunk
//! carries both when there is only one). A [`GROUND_TIMEOUT`] (5 s) with no
//! `ground_reached` DEMOTES the connection back to `PostHello` (subject to
//! the cap check above; freeing both its `Watcher` slot and the checkpoint
//! slot, which then advances the queue) and replies `attach_refused
//! {GroundTimeout}` — explicitly retryable, per the ADR.
//!
//! `take`'s own [`crate::wire::TakeRefusedReason::CheckpointInFlight`] is a
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

use crate::wire::{
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

#[derive(Debug, Clone)]
struct WatcherState {
    /// The `attach` request this connection is still owed a reply for —
    /// consumed (as `clears_request`) by the FIRST checkpoint chunk this
    /// watcher ever streams, or by an `AttachRefused` if it never gets
    /// that far (ground timeout, subscriber cap).
    attach_request_id: RequestId,
    checkpoint: CheckpointProgress,
    /// LIVE output only, budget-checked (see module doc's "Queue
    /// accounting") — tracked regardless of checkpoint state.
    queued_live_bytes: u64,
    /// Output committed while `checkpoint` is not yet `Done` (finding 3):
    /// queued behind the transfer, never dropped, flushed in order once
    /// the final chunk's completion marks this watcher `Done`.
    pending_post_watermark: VecDeque<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct Conn {
    role: Role,
    outstanding_request: Option<RequestId>,
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
}

#[derive(Debug, Clone)]
struct DriverState {
    conn: ConnId,
    keepalive_outstanding: Option<u64>,
    /// Armed only once the ping's sent-completion is reported (ADR 0041:
    /// "not at enqueue").
    keepalive_deadline: Option<Instant>,
}

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
    next_request_id: u64,
    /// Set once by [`AttachProto::begin_teardown`] (finding 7): from then
    /// on, `take`/`input`/`resize` are silently ignored rather than
    /// admitted — producer-bound admission revocation. `hello`/`attach`/
    /// mgmt are unaffected; this module has no opinion on whether the
    /// caller keeps feeding it events past this point.
    teardown: bool,
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
            next_request_id: 0,
            teardown: false,
        }
    }

    /// Producer-bound admission (`take`/`input`/`resize`) is revoked from
    /// this point on (finding 7) — mgmt and the attach lane's `hello`/
    /// `attach` are unaffected. Idempotent.
    pub fn begin_teardown(&mut self) {
        self.teardown = true;
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
                outstanding_request: None,
                last_activity: now,
                outstanding_sends: 0,
                last_send_progress: now,
                last_keepalive_nonce: None,
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
    /// or several actions.
    pub fn frame(&mut self, conn: ConnId, decoded: DecodedFrame, now: Instant) -> Vec<Action> {
        if let DecodedFrame::Keepalive { nonce } = decoded {
            return self.handle_keepalive_reply(conn, nonce, now);
        }
        let Some(c) = self.conns.get(&conn) else {
            return vec![];
        };
        if c.outstanding_request.is_some() {
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
    /// with no bookkeeping consequence. Always resets `last_activity`/
    /// `last_send_progress` and decrements `outstanding_sends` for `conn`
    /// (finding 5) — a checked decrement: more completions than sends ever
    /// issued is this module's own bookkeeping bug.
    pub fn sent(&mut self, conn: ConnId, marker: Option<SentMarker>, now: Instant) -> Vec<Action> {
        match self.conns.get_mut(&conn) {
            Some(c) => {
                c.last_activity = now;
                c.outstanding_sends = c
                    .outstanding_sends
                    .checked_sub(1)
                    .expect("sent(): more completions than sends were ever issued for this connection");
                c.last_send_progress = now;
            }
            None => return vec![], // already closed; nothing to do (finding 11)
        }
        let Some(marker) = marker else { return vec![] };
        match marker {
            SentMarker::Reply { request_id } => {
                self.clear_outstanding_if_matches(conn, request_id);
                vec![]
            }
            SentMarker::ReplyThenClose { request_id } => {
                self.clear_outstanding_if_matches(conn, request_id);
                self.remove_connection(conn, now);
                vec![Action::Close(conn)]
            }
            SentMarker::ShutdownAck { request_id, reason } => {
                self.clear_outstanding_if_matches(conn, request_id);
                self.remove_connection(conn, now);
                vec![Action::Shutdown { reason }, Action::Close(conn)]
            }
            SentMarker::CheckpointChunk { clears_request, is_last } => {
                if let Some(rid) = clears_request {
                    self.clear_outstanding_if_matches(conn, rid);
                }
                if !is_last {
                    return self.advance_checkpoint_stream(conn, now);
                }
                let pending = match self.conns.get_mut(&conn).map(|c| &mut c.role) {
                    Some(Role::Watcher(w)) => {
                        w.checkpoint = CheckpointProgress::Done;
                        std::mem::take(&mut w.pending_post_watermark)
                    }
                    _ => VecDeque::new(),
                };
                if self.checkpoint_slot == Some(conn) {
                    self.checkpoint_slot = None;
                    self.advance_checkpoint_queue(now);
                }
                let mut actions = Vec::new();
                for bytes in pending {
                    let n = bytes.len() as u64;
                    let encoded = wire::encode_attach_server(&AttachServer::Output { bytes })
                        .expect("output frame within the outer 1 MiB cap is the loop's own responsibility");
                    actions.push(self.make_send(conn, encoded, Some(SentMarker::OutputBytes { n }), now));
                }
                actions
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
    /// budget" — this is the LIVE-only counter, tracked regardless of
    /// whether the watcher is `Done` yet (finding 3: pre-`Done` output
    /// still counts against the budget even though it is not sent yet).
    ///
    /// Round-2 review, finding 2 (an EARLIER exemption here was
    /// INCOMPLETE): the ADR's driver row is TWO clauses, not one —
    /// "driver queue 4 MiB; committed driver-visible bytes are never
    /// dropped while the connection is live, but transport liveness is
    /// bounded... a hung driver cannot wedge the writer loop". The 4 MiB
    /// BOUND STAYS for the driver exactly as for a watcher; what the ADR
    /// actually promises is HOW overflow resolves — by CLOSING the
    /// connection (never by silently dropping bytes while it stays live).
    /// The bytes themselves are never lost either way: they are already
    /// durable in the voyage before this call ever runs (the watermark
    /// barrier commits before it publishes), so a reconnect after this
    /// close replays them via a fresh `attach`'s checkpoint. This is the
    /// SAME mechanism as an ordinary watcher's eviction, just labeled
    /// `DriverQueueOverflow` instead of `QueueOverflow` — distinct enough
    /// for step 6's adoption UX to tell "a passive subscriber never
    /// drained" apart from "the driver itself could not keep up with its
    /// own producer" — because unlike a watcher, losing the driver ALSO
    /// clears the pen (`take`'s null-holder state), which a client is
    /// meant to notice and re-`attach`/`take` for.
    pub fn bytes_queued(&mut self, conn: ConnId, n: u64, now: Instant) -> Vec<Action> {
        let is_driver = self.driver.as_ref().is_some_and(|d| d.conn == conn);
        let overflowed = match self.conns.get_mut(&conn).map(|c| &mut c.role) {
            Some(Role::Watcher(w)) => {
                w.queued_live_bytes += n;
                w.queued_live_bytes > WATCHER_LIVE_QUEUE_BUDGET_BYTES
            }
            _ => false,
        };
        if overflowed {
            let reason = if is_driver { RefusalReason::DriverQueueOverflow } else { RefusalReason::QueueOverflow };
            self.close_with_refusal(conn, reason, now)
        } else {
            vec![]
        }
    }

    /// Time-driven checks with no inbound frame to trigger them: the two
    /// admission timeouts, the ground-wait deadline, the generic
    /// queue-progress stall (finding 5), and the driver keepalive state
    /// machine (finding 6).
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
                        ground_timed_out.push((*id, w.attach_request_id));
                    }
                }
            }
        }
        for (id, rid) in ground_timed_out {
            actions.extend(self.ground_timeout(id, rid, now));
        }

        let mut stalled = Vec::new();
        for (id, c) in &self.conns {
            if c.outstanding_sends > 0 && now.saturating_duration_since(c.last_send_progress) >= PROGRESS_DEADLINE {
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
    /// held. Requests a checkpoint for the current checkpoint-slot holder,
    /// if it is waiting — a no-op otherwise (ground recurs constantly; most
    /// calls have nothing to promote).
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
        vec![Action::BeginCheckpoint { conn }]
    }

    /// The loop encoded `conn`'s checkpoint (only it can — see the module
    /// doc) and hands back the bytes. Wraps them in `Arc` ONCE and emits
    /// only the FIRST chunk (finding 10: streamed, not materialized all at
    /// once) — later chunks are requested by [`AttachProto::sent`]'s
    /// `CheckpointChunk` handling as each one completes. Ignored (defensive
    /// no-op) if `conn` is not this run's current slot holder still
    /// `AwaitingGround` — should not happen given the loop only ever calls
    /// this in response to `BeginCheckpoint`.
    ///
    /// Real CI failure (windows-2022, PR #139 discharge round): every
    /// `output_committed` call made while `conn` was `QueuedForSlot` or
    /// `AwaitingGround` — i.e. every group-commit round from `attach` until
    /// THIS one — has already queued its bytes into
    /// `WatcherState::pending_post_watermark` (`output_committed`'s `Some(_)
    /// => queue` arm treats every non-`Done` state alike). `bytes` (the live
    /// parser's checkpoint, taken via `capsule_win.rs`'s
    /// `flush_output!`/`ground_reached` watermark barrier: fsync -> publish
    /// -> checkpoint, in that order, one loop step) reflects EXACTLY that
    /// same committed history — the barrier's own ordering is correct; the
    /// bug was never syncing the queue to it. Left alone, that backlog is a
    /// duplicate: the SAME bytes are already baked into the grid `bytes`
    /// encodes, and clearing it later at `Done` would deliver them a SECOND
    /// time on top of the checkpoint. Purging it HERE — the one moment this
    /// checkpoint's cut point and the queue's own contents are both in
    /// scope — is what makes the checkpoint and
    /// `WatcherState::pending_post_watermark` two genuinely
    /// non-overlapping halves of the same committed timeline, the
    /// invariant a fidelity check across the two can only hold if it's true
    /// (`tests/capsule_win.rs`'s `attach_mid_stream_checkpoint_reproduces_
    /// reference_screen`).
    pub fn checkpoint_ready(&mut self, conn: ConnId, bytes: Vec<u8>, now: Instant) -> Vec<Action> {
        let awaiting = matches!(
            self.conns.get(&conn).and_then(watcher_checkpoint),
            Some(CheckpointProgress::AwaitingGround { .. })
        );
        if !awaiting || self.checkpoint_slot != Some(conn) {
            return vec![];
        }
        let shared: Arc<Vec<u8>> = Arc::new(bytes);
        if let Some(Role::Watcher(w)) = self.conns.get_mut(&conn).map(|c| &mut c.role) {
            w.checkpoint = CheckpointProgress::Sending { bytes: shared.clone(), offset: 0 };
            // Round-2 review, finding 1: clearing the backlog without also
            // releasing its OWN queued_live_bytes charge left that charge
            // permanently stuck (only an `OutputBytes` `Sent` ever
            // decrements it, and these cleared vectors will never produce
            // one) — a scratch probe reproduced a FALSE eviction one byte
            // after capture, from a charge belonging to bytes that no
            // longer exist anywhere but the checkpoint. Release it
            // atomically with the same clear that retires the bytes.
            let cleared_bytes: u64 = w.pending_post_watermark.iter().map(|b| b.len() as u64).sum();
            w.pending_post_watermark.clear();
            w.queued_live_bytes = w.queued_live_bytes.checked_sub(cleared_bytes).expect(
                "pending_post_watermark's own contribution cannot exceed the connection's total queued_live_bytes",
            );
        }
        self.emit_next_checkpoint_chunk(conn, shared, 0, now)
    }

    /// The loop fsynced `take_state {holder: controller_id, epoch:
    /// new_take_epoch}`. Installs the ephemeral capability on `conn`,
    /// silently overwriting whoever held it before (the demotion — that
    /// connection's own `Watcher` entry is untouched; its keepalive nonce,
    /// if any, is simply discarded — see the module doc's keepalive
    /// section for why a late echo for it is then ignored rather than
    /// fatal).
    pub fn take_committed(&mut self, conn: ConnId, new_take_epoch: u64, request_id: RequestId, now: Instant) -> Vec<Action> {
        self.driver = Some(DriverState { conn, keepalive_outstanding: None, keepalive_deadline: None });
        if let Some(c) = self.conns.get_mut(&conn) {
            c.last_activity = now;
        }
        let bytes = wire::encode_attach_server(&AttachServer::TakeOk { take_epoch: new_take_epoch })
            .expect("TakeOk is a fixed-shape body, always within MAX_BODY_LEN");
        vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)]
    }

    /// The loop ran the input WAL for `conn`'s `input` frame and reports the
    /// outcome.
    pub fn input_outcome(&mut self, conn: ConnId, outcome: InputOutcome, request_id: RequestId, now: Instant) -> Vec<Action> {
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
        vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)]
    }

    /// The loop ran the ordered resize exchange for `conn` and reports
    /// whether the geometry was in budget.
    pub fn resize_outcome(&mut self, conn: ConnId, ok: bool, request_id: RequestId, now: Instant) -> Vec<Action> {
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
        vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)]
    }

    /// Live producer output just committed (the watermark). For every
    /// `Watcher`: budget-check via `bytes_queued`; if `Done`, enqueue an
    /// `output` frame now; otherwise queue it behind the in-flight
    /// checkpoint transfer (finding 3) — flushed once that watcher reaches
    /// `Done`.
    pub fn output_committed(&mut self, bytes: &[u8], now: Instant) -> Vec<Action> {
        let targets: Vec<ConnId> = self
            .conns
            .iter()
            .filter_map(|(id, c)| match &c.role {
                Role::Watcher(_) => Some(*id),
                _ => None,
            })
            .collect();
        let mut actions = Vec::new();
        for conn in targets {
            actions.extend(self.bytes_queued(conn, bytes.len() as u64, now));
            match self.conns.get(&conn).and_then(watcher_checkpoint) {
                Some(CheckpointProgress::Done) => {
                    let encoded = wire::encode_attach_server(&AttachServer::Output { bytes: bytes.to_vec() })
                        .expect("output frame within the outer 1 MiB cap is the loop's own responsibility");
                    actions.push(self.make_send(
                        conn,
                        encoded,
                        Some(SentMarker::OutputBytes { n: bytes.len() as u64 }),
                        now,
                    ));
                }
                Some(_) => {
                    if let Some(Role::Watcher(w)) = self.conns.get_mut(&conn).map(|c| &mut c.role) {
                        w.pending_post_watermark.push_back(bytes.to_vec());
                    }
                }
                None => {} // closed by bytes_queued's own overflow handling, or not a watcher
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
        let rid = self.mark_outstanding(conn);
        match req {
            MgmtRequest::Probe => {
                let bytes = wire::encode_mgmt_reply(&MgmtReply::ProbeOk).expect("fixed-shape body");
                vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id: rid }), now)]
            }
            MgmtRequest::Status => {
                let s = self.mgmt_status;
                let bytes = wire::encode_mgmt_reply(&MgmtReply::StatusOk {
                    pid: s.pid,
                    created: s.created,
                    survival: s.survival,
                })
                .expect("fixed-shape body");
                vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id: rid }), now)]
            }
            MgmtRequest::Shutdown { reason } => {
                let bytes = wire::encode_mgmt_reply(&MgmtReply::ShutdownOk).expect("fixed-shape body");
                vec![self.make_send(conn, bytes, Some(SentMarker::ShutdownAck { request_id: rid, reason }), now)]
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

        let rid = self.mark_outstanding(conn);
        match frame {
            AttachClient::Hello { proto } => self.handle_hello(conn, proto, rid, now),
            AttachClient::Attach { controller_id: _ } => self.handle_attach(conn, rid, now),
            AttachClient::Take { controller_id } => self.handle_take(conn, controller_id, rid, now),
            AttachClient::Input {
                controller_id,
                take_epoch,
                idem_key,
                payload,
            } => self.handle_input(conn, controller_id, take_epoch, idem_key, payload, rid),
            AttachClient::Resize { cols, rows } => self.handle_resize(conn, cols, rows, rid, now),
        }
    }

    fn handle_hello(&mut self, conn: ConnId, proto: u32, request_id: RequestId, now: Instant) -> Vec<Action> {
        match wire::negotiate(proto) {
            wire::Negotiated::Accepted(v) => {
                if let Some(c) = self.conns.get_mut(&conn) {
                    if let Role::Unclassified { deadline } = c.role {
                        c.role = Role::PostHello { deadline };
                    }
                }
                let bytes = wire::encode_attach_server(&AttachServer::HelloOk { proto: v }).expect("fixed-shape body");
                vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)]
            }
            wire::Negotiated::Refused { supported } => {
                let bytes =
                    wire::encode_attach_server(&AttachServer::HelloRefused { supported }).expect("fixed-shape body");
                vec![self.make_send(conn, bytes, Some(SentMarker::ReplyThenClose { request_id }), now)]
            }
        }
    }

    fn handle_attach(&mut self, conn: ConnId, request_id: RequestId, now: Instant) -> Vec<Action> {
        if self.watcher_count >= SUBSCRIBER_CAP {
            let bytes = wire::encode_attach_server(&AttachServer::AttachRefused {
                reason: AttachRefusedReason::SubscriberCap,
            })
            .expect("fixed-shape body");
            return vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)];
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
                attach_request_id: request_id,
                checkpoint,
                queued_live_bytes: 0,
                pending_post_watermark: VecDeque::new(),
            });
        }
        vec![]
    }

    fn handle_take(&mut self, conn: ConnId, controller_id: String, request_id: RequestId, now: Instant) -> Vec<Action> {
        if self.teardown {
            self.clear_outstanding_if_matches(conn, request_id);
            return vec![];
        }
        let checkpoint = self.conns.get(&conn).and_then(watcher_checkpoint);
        let reason = match checkpoint {
            None => Some(TakeRefusedReason::NotAttached),
            Some(CheckpointProgress::Done) => None,
            Some(_) => Some(TakeRefusedReason::CheckpointInFlight),
        };
        if let Some(reason) = reason {
            let bytes = wire::encode_attach_server(&AttachServer::TakeRefused { reason }).expect("fixed-shape body");
            return vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)];
        }
        vec![Action::CommitTake { conn, controller_id, request_id }]
    }

    fn handle_input(
        &mut self,
        conn: ConnId,
        controller_id: String,
        take_epoch: u64,
        idem_key: [u8; 16],
        payload: Vec<u8>,
        request_id: RequestId,
    ) -> Vec<Action> {
        if self.teardown {
            self.clear_outstanding_if_matches(conn, request_id);
            return vec![];
        }
        let connection_authorized = self.driver.as_ref().map(|d| d.conn) == Some(conn);
        vec![Action::ForwardInput {
            conn,
            controller_id,
            take_epoch,
            idem_key,
            payload,
            connection_authorized,
            request_id,
        }]
    }

    fn handle_resize(&mut self, conn: ConnId, cols: u16, rows: u16, request_id: RequestId, now: Instant) -> Vec<Action> {
        if self.teardown {
            self.clear_outstanding_if_matches(conn, request_id);
            return vec![];
        }
        if self.driver.as_ref().map(|d| d.conn) != Some(conn) {
            let bytes = wire::encode_attach_server(&AttachServer::ResizeRefused {
                reason: ResizeRefusedReason::NotDriver,
            })
            .expect("fixed-shape body");
            return vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)];
        }
        vec![Action::ApplyResize { conn, cols, rows, request_id }]
    }

    /// Round-2 review, finding 3: nonces now retire by CONNECTION, not by
    /// "is this the current `DriverState`" — a same-connection retake
    /// (`take_committed`, same `conn`) preserves the prior nonce instead of
    /// discarding it, so this method no longer needs to special-case that
    /// path itself. Two questions, asked in this order:
    ///
    /// 1. Was `nonce` EVER issued to `conn`, at all? If not — a watcher
    ///    that was never driver, or any other fabricated value — this is a
    ///    genuine protocol violation, never silently accepted.
    /// 2. Is `conn` the driver RIGHT NOW, with THIS nonce still the one
    ///    outstanding? If so, the reply is answered for real (clear the
    ///    gate, refresh activity). Otherwise it is a recognized but no
    ///    longer actionable echo — a demoted former driver's late reply, or
    ///    a duplicate of one already cleared — ignorable, not fatal.
    fn handle_keepalive_reply(&mut self, conn: ConnId, nonce: u64, now: Instant) -> Vec<Action> {
        let ever_issued = self.conns.get(&conn).and_then(|c| c.last_keepalive_nonce);
        if ever_issued != Some(nonce) {
            return self.close_with_refusal(conn, RefusalReason::UnexpectedKeepalive, now);
        }
        let is_current_and_outstanding = self
            .driver
            .as_ref()
            .is_some_and(|d| d.conn == conn && d.keepalive_outstanding == Some(nonce));
        if !is_current_and_outstanding {
            return vec![];
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

    /// Round-2 review deletion residue: this used to also FREEZE the
    /// reply deadline while the driver's OWN checkpoint transfer was still
    /// in flight (finding 6, original discharge round) -- but `take`
    /// itself refuses admission (`CheckpointInFlight`) until a
    /// connection's own checkpoint is already `Done`, and nothing ever
    /// moves a watcher's checkpoint backward out of `Done` once reached.
    /// A connection can therefore never actually BECOME the driver while
    /// its own transfer is in flight, which made that whole branch (and
    /// `DriverState.last_tick`, which existed only to compute it) dead in
    /// every real path -- deleted rather than kept "in case", per this
    /// round's own deletion pressure. If a future design lets `take`
    /// admit a connection before its checkpoint finishes, this suspension
    /// needs reintroducing deliberately, not resurrecting from here.
    fn tick_keepalive(&mut self, now: Instant) -> Vec<Action> {
        let Some(conn) = self.driver.as_ref().map(|d| d.conn) else {
            return vec![];
        };

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
        if let Some(c) = self.conns.get_mut(&conn) {
            c.last_keepalive_nonce = Some(nonce);
        }
        let bytes = wire::encode_keepalive(nonce);
        vec![self.make_send(conn, bytes, Some(SentMarker::Keepalive { nonce }), now)]
    }

    // -- checkpoint streaming (finding 10) -------------------------------

    fn advance_checkpoint_stream(&mut self, conn: ConnId, now: Instant) -> Vec<Action> {
        let next = match self.conns.get(&conn).and_then(watcher_checkpoint) {
            Some(CheckpointProgress::Sending { bytes, offset }) => Some((bytes, offset)),
            _ => None,
        };
        let Some((bytes, offset)) = next else { return vec![] };
        self.emit_next_checkpoint_chunk(conn, bytes, offset, now)
    }

    fn emit_next_checkpoint_chunk(&mut self, conn: ConnId, bytes: Arc<Vec<u8>>, offset: usize, now: Instant) -> Vec<Action> {
        let max = wire::MAX_CHECKPOINT_CHUNK_PAYLOAD;
        let end = (offset + max).min(bytes.len());
        let is_last = end == bytes.len();
        let chunk = AttachServer::CheckpointChunk {
            last: is_last,
            bytes: bytes[offset..end].to_vec(),
        };
        let encoded = wire::encode_attach_server(&chunk)
            .expect("each chunk is capped at MAX_CHECKPOINT_CHUNK_PAYLOAD by construction");
        let attach_request_id = match self.conns.get(&conn).map(|c| &c.role) {
            Some(Role::Watcher(w)) => Some(w.attach_request_id),
            _ => None,
        };
        let clears_request = if offset == 0 { attach_request_id } else { None };
        if let Some(Role::Watcher(w)) = self.conns.get_mut(&conn).map(|c| &mut c.role) {
            w.checkpoint = CheckpointProgress::Sending { bytes: bytes.clone(), offset: end };
        }
        vec![self.make_send(
            conn,
            encoded,
            Some(SentMarker::CheckpointChunk { clears_request, is_last }),
            now,
        )]
    }

    // -- shared helpers ------------------------------------------------

    /// Allocates a fresh [`RequestId`], records it as `conn`'s outstanding
    /// request, and returns it so the caller can embed it in whatever
    /// eventual reply resolves this request (finding 4).
    fn mark_outstanding(&mut self, conn: ConnId) -> RequestId {
        self.next_request_id += 1;
        let rid = self.next_request_id;
        if let Some(c) = self.conns.get_mut(&conn) {
            c.outstanding_request = Some(rid);
        }
        rid
    }

    /// Clears `conn`'s outstanding request ONLY if it is still `rid`
    /// (finding 4) — an unrelated or late marker must never clear a
    /// DIFFERENT, still-pending request.
    fn clear_outstanding_if_matches(&mut self, conn: ConnId, rid: RequestId) {
        if let Some(c) = self.conns.get_mut(&conn) {
            if c.outstanding_request == Some(rid) {
                c.outstanding_request = None;
            }
        }
    }

    /// Constructs a `Send` action AND records the generic write-progress
    /// bookkeeping (finding 5): increments `outstanding_sends`, and resets
    /// `last_send_progress` on the empty→nonempty transition. Every `Send`
    /// this module ever emits goes through here — there is no other
    /// construction site.
    fn make_send(&mut self, conn: ConnId, frame_bytes: Vec<u8>, marker: Option<SentMarker>, now: Instant) -> Action {
        if let Some(c) = self.conns.get_mut(&conn) {
            if c.outstanding_sends == 0 {
                c.last_send_progress = now;
            }
            c.outstanding_sends += 1;
        }
        Action::Send { conn, frame_bytes, marker }
    }

    fn ground_timeout(&mut self, conn: ConnId, request_id: RequestId, now: Instant) -> Vec<Action> {
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

        if self.non_watcher_count >= NON_WATCHER_CAP {
            // Demoting would exceed the shared non-watcher cap (finding
            // 12) -- close instead. The role stays `Watcher` until this
            // reply's `Sent` fires `remove_connection`, which does the
            // `watcher_count` bookkeeping; touching it here would
            // double-count.
            return vec![self.make_send(conn, bytes, Some(SentMarker::ReplyThenClose { request_id }), now)];
        }

        // Demote: this connection stops being a Watcher right now, so its
        // OWN bookkeeping must happen here -- no later `remove_connection`
        // call will ever see it as a Watcher again.
        self.watcher_count = self.watcher_count.saturating_sub(1);
        self.non_watcher_count += 1;
        if let Some(c) = self.conns.get_mut(&conn) {
            c.role = Role::PostHello {
                deadline: now + PRE_ADMISSION_TIMEOUT,
            };
        }
        vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)]
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
        Role::Watcher(w) => Some(w.checkpoint.clone()),
        _ => None,
    }
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

    impl Action {
        fn send_marker(&self) -> Option<SentMarker> {
            match self {
                Action::Send { marker, .. } => marker.clone(),
                _ => panic!("not a Send: {self:?}"),
            }
        }
        fn send_bytes(&self) -> &[u8] {
            match self {
                Action::Send { frame_bytes, .. } => frame_bytes,
                _ => panic!("not a Send: {self:?}"),
            }
        }
    }

    /// Drives one checkpoint transfer to completion, one chunk at a time
    /// (finding 10: `checkpoint_ready`/`sent` must never emit more than one
    /// `Send` per step) — the common setup every take/input/resize/
    /// keepalive/budget test needs once a watcher must be fully `Done`.
    fn drive_checkpoint_to_done(p: &mut AttachProto, conn: ConnId, checkpoint_bytes: Vec<u8>, now: Instant) {
        let mut actions = p.checkpoint_ready(conn, checkpoint_bytes, now);
        loop {
            assert_eq!(actions.len(), 1, "expected exactly one Send per checkpoint step: {actions:?}");
            let marker = actions[0].send_marker();
            let is_last = matches!(marker, Some(SentMarker::CheckpointChunk { is_last: true, .. }));
            actions = p.sent(conn, marker, now);
            if is_last {
                break;
            }
        }
    }

    /// Drives one connection all the way to a `Done` watcher: connection_opened
    /// -> hello -> attach -> ground_reached -> a streamed one-chunk checkpoint.
    fn attach_to_done(p: &mut AttachProto, conn: ConnId, now: Instant) {
        assert_eq!(p.connection_opened(conn, now), vec![]);
        let a = p.frame(conn, hello_frame(), now);
        assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::Reply { .. }), .. }]));
        p.sent(conn, a[0].send_marker(), now);
        let a = p.frame(conn, attach_frame("ctrl"), now);
        assert!(a.is_empty(), "attach should pend, not reply immediately: {a:?}");
        let a = p.ground_reached(now);
        assert!(matches!(a.as_slice(), [Action::BeginCheckpoint { conn: c }] if *c == conn));
        drive_checkpoint_to_done(p, conn, vec![0xAB], now);
    }

    /// Drives `take` to completion: frame -> CommitTake -> take_committed
    /// (the loop's own fsync is not modeled here; this module never needs
    /// it) -> the TakeOk reply's own sent-completion.
    fn drive_take(p: &mut AttachProto, conn: ConnId, controller_id: &str, epoch: u64, now: Instant) {
        let a = p.frame(conn, take_frame(controller_id), now);
        let request_id = match a.as_slice() {
            [Action::CommitTake { request_id, .. }] => *request_id,
            other => panic!("expected CommitTake: {other:?}"),
        };
        let a = p.take_committed(conn, epoch, request_id, now);
        assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::Reply { .. }), .. }]));
        p.sent(conn, a[0].send_marker(), now);
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

    /// Finding 4's own regression scenario: an UNRELATED checkpoint chunk's
    /// completion (whose `clears_request` is `None` -- it isn't the first
    /// chunk) must never clear a DIFFERENT request's lockstep. Before the
    /// fix, `CheckpointChunk`'s handler cleared `outstanding_request`
    /// unconditionally, so `take`'s own still-unsent reply would have been
    /// wrongly freed by this unrelated completion.
    #[test]
    fn an_unrelated_checkpoint_chunks_completion_does_not_clear_a_different_outstanding_requests_lockstep() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let a = p.frame(1, hello_frame(), now);
        p.sent(1, a[0].send_marker(), now);
        p.frame(1, attach_frame("alice"), now);
        p.ground_reached(now);
        let big = vec![0u8; wire::MAX_CHECKPOINT_CHUNK_PAYLOAD + 1]; // 2 chunks
        let chunk1 = p.checkpoint_ready(1, big, now);
        let chunk2 = p.sent(1, chunk1[0].send_marker(), now); // clears ATTACH's own lockstep
        assert!(matches!(
            chunk2[0].send_marker(),
            Some(SentMarker::CheckpointChunk { clears_request: None, is_last: true })
        ));

        // `take` is now legal (attach's lockstep already cleared) --
        // refused CheckpointInFlight (own transfer not yet Done); ITS OWN
        // reply is queued but not yet reported sent.
        let a = p.frame(1, take_frame("alice"), now);
        assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::Reply { .. }), .. }]));

        // Report the UNRELATED chunk2 completion now.
        let _ = p.sent(1, chunk2[0].send_marker(), now);

        // take's own reply STILL hasn't been reported sent -- a second
        // take must still be a lockstep violation.
        let a2 = p.frame(1, take_frame("alice"), now);
        assert!(
            a2.iter().any(|x| matches!(x, Action::Close(1))),
            "an unrelated checkpoint completion must not clear take's own lockstep: {a2:?}"
        );
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
            [Action::Send { frame_bytes, marker: Some(SentMarker::Reply { .. }), .. }] => {
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

    /// Finding 12: a timed-out attach demotes back to `PostHello` only if
    /// there is room; over the shared non-watcher cap, it closes instead
    /// (after its refusal is sent).
    #[test]
    fn ground_timeout_over_cap_closes_instead_of_demoting() {
        let mut p = proto();
        let now = t0();
        // conn 1: hello -> attach (now a Watcher, no longer counted as
        // non-watcher) -- pending ground, never resolved.
        p.connection_opened(1, now);
        let a = p.frame(1, hello_frame(), now);
        p.sent(1, a[0].send_marker(), now);
        p.frame(1, attach_frame("alice"), now);

        // Fill the non-watcher cap with 4 OTHER connections.
        for id in 100..104u64 {
            assert_eq!(p.connection_opened(id, now), vec![]);
        }

        // conn 1's ground-wait times out: demoting it back to PostHello
        // would make a 5th non-watcher -- must close instead.
        let later = now + Duration::from_secs(6);
        let a = p.tick(later);
        let send = a.iter().find_map(|x| match x {
            Action::Send { conn, frame_bytes, marker } if *conn == 1 => Some((frame_bytes.clone(), marker.clone())),
            _ => None,
        });
        let (bytes, marker) = send.expect("expected conn 1's ground-timeout reply");
        assert!(
            matches!(marker, Some(SentMarker::ReplyThenClose { .. })),
            "over cap must close, not demote: {marker:?}"
        );
        let decoded = decode_one(&bytes);
        assert_eq!(
            decoded,
            DecodedFrame::AttachServer(AttachServer::AttachRefused {
                reason: AttachRefusedReason::GroundTimeout
            })
        );
        let after = p.sent(1, marker, later);
        assert_eq!(after, vec![Action::Close(1)]);
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
            [Action::Send { marker: Some(SentMarker::ReplyThenClose { .. }), .. }]
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

    // -- the pen --------------------------------------------------------

    #[test]
    fn take_demotes_the_previous_driver_which_stays_a_watcher() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        attach_to_done(&mut p, 2, now);

        drive_take(&mut p, 1, "alice", 1, now);
        drive_take(&mut p, 2, "bob", 2, now);

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
        drive_take(&mut p, 1, "alice", 1, now);

        p.connection_closed(1, now);
        // No durable action is emitted for the EOF -- capability-only.
        // A NEW connection taking (as the very first ever driver) succeeds
        // without any special-casing, proving no stale state lingered.
        attach_to_done(&mut p, 2, now);
        let a = p.frame(2, take_frame("carol"), now);
        assert!(matches!(a.as_slice(), [Action::CommitTake { conn: 2, controller_id, .. }] if controller_id == "carol"));
    }

    #[test]
    fn stale_input_from_a_non_driver_connection_is_flagged_unauthorized() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        attach_to_done(&mut p, 2, now);
        drive_take(&mut p, 1, "alice", 1, now);

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
        match a.as_slice() {
            [Action::ForwardInput {
                conn,
                controller_id,
                take_epoch,
                idem_key,
                payload,
                connection_authorized,
                request_id: _,
            }] => {
                assert_eq!(*conn, 2);
                assert_eq!(controller_id, "alice");
                assert_eq!(*take_epoch, 1);
                assert_eq!(*idem_key, [7u8; 16]);
                assert_eq!(payload, b"ls\n");
                assert!(!connection_authorized);
            }
            other => panic!("expected ForwardInput: {other:?}"),
        }
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

    #[test]
    fn take_while_own_checkpoint_in_flight_is_refused() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let a = p.frame(1, hello_frame(), now);
        p.sent(1, a[0].send_marker(), now);
        p.frame(1, attach_frame("alice"), now);
        p.ground_reached(now);
        let big = vec![0u8; wire::MAX_CHECKPOINT_CHUNK_PAYLOAD + 1]; // 2 chunks
        let chunk1 = p.checkpoint_ready(1, big, now);
        assert_eq!(chunk1.len(), 1);
        assert!(matches!(
            chunk1[0].send_marker(),
            Some(SentMarker::CheckpointChunk { clears_request: Some(_), is_last: false })
        ));
        // The FIRST chunk's completion clears lockstep (the attach success
        // signal) and requests the SECOND (last) chunk.
        let chunk2 = p.sent(1, chunk1[0].send_marker(), now);
        assert_eq!(chunk2.len(), 1);
        assert!(matches!(
            chunk2[0].send_marker(),
            Some(SentMarker::CheckpointChunk { clears_request: None, is_last: true })
        ));

        // `take` is legal to send now (lockstep already cleared), but the
        // transfer is not Done (chunk2 not yet confirmed sent).
        let a = p.frame(1, take_frame("alice"), now);
        let decoded = decode_one(a[0].send_bytes());
        assert_eq!(
            decoded,
            DecodedFrame::AttachServer(AttachServer::TakeRefused {
                reason: TakeRefusedReason::CheckpointInFlight
            })
        );
        // Report THIS refusal's own reply sent (clearing its lockstep)
        // before trying again.
        p.sent(1, a[0].send_marker(), now);
        // Once the final checkpoint chunk is ALSO reported sent, take
        // succeeds.
        p.sent(1, chunk2[0].send_marker(), now);
        let a = p.frame(1, take_frame("alice"), now);
        assert!(matches!(a.as_slice(), [Action::CommitTake { conn: 1, controller_id, .. }] if controller_id == "alice"));
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
        drive_checkpoint_to_done(&mut p, 1, vec![0xAB], now);
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
            .any(|x| matches!(x, Action::Send { marker: Some(SentMarker::Reply { .. }), .. })));
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

    // -- checkpoint streaming + output queue-behind (findings 3, 10) -----

    #[test]
    fn checkpoint_streams_one_chunk_at_a_time_not_all_up_front() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let a = p.frame(1, hello_frame(), now);
        p.sent(1, a[0].send_marker(), now);
        p.frame(1, attach_frame("alice"), now);
        p.ground_reached(now);
        let big = vec![0u8; wire::MAX_CHECKPOINT_CHUNK_PAYLOAD * 3]; // 3 chunks
        let chunk1 = p.checkpoint_ready(1, big, now);
        assert_eq!(chunk1.len(), 1, "must emit exactly one chunk per step, not all up front");
        assert!(matches!(
            chunk1[0].send_marker(),
            Some(SentMarker::CheckpointChunk { is_last: false, .. })
        ));
        let chunk2 = p.sent(1, chunk1[0].send_marker(), now);
        assert_eq!(chunk2.len(), 1);
        assert!(matches!(
            chunk2[0].send_marker(),
            Some(SentMarker::CheckpointChunk { is_last: false, .. })
        ));
        let chunk3 = p.sent(1, chunk2[0].send_marker(), now);
        assert_eq!(chunk3.len(), 1);
        assert!(matches!(
            chunk3[0].send_marker(),
            Some(SentMarker::CheckpointChunk { is_last: true, .. })
        ));
    }

    #[test]
    fn output_queues_behind_an_in_flight_checkpoint_and_flushes_once_done() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let a = p.frame(1, hello_frame(), now);
        p.sent(1, a[0].send_marker(), now);
        p.frame(1, attach_frame("alice"), now);
        p.ground_reached(now);
        let big = vec![0u8; wire::MAX_CHECKPOINT_CHUNK_PAYLOAD + 1]; // 2 chunks
        let chunk1 = p.checkpoint_ready(1, big, now);

        // Output committed WHILE still mid-transfer must not be dropped.
        let a = p.output_committed(b"queued-behind", now);
        assert!(a.is_empty(), "no Output send yet -- not Done: {a:?}");

        let chunk2 = p.sent(1, chunk1[0].send_marker(), now);
        assert!(matches!(
            chunk2[0].send_marker(),
            Some(SentMarker::CheckpointChunk { is_last: true, .. })
        ));
        let flushed = p.sent(1, chunk2[0].send_marker(), now);
        assert_eq!(flushed.len(), 1, "the queued output must flush exactly once Done: {flushed:?}");
        let decoded = decode_one(flushed[0].send_bytes());
        assert_eq!(
            decoded,
            DecodedFrame::AttachServer(AttachServer::Output { bytes: b"queued-behind".to_vec() })
        );
    }

    /// Real CI failure (windows-2022, PR #139 discharge round): the
    /// rebuilt fidelity test found the wire checkpoint diverging from an
    /// independently computed reference of the same prefix. Root cause —
    /// bytes committed WHILE a watcher is still `QueuedForSlot`/
    /// `AwaitingGround` (before its turn) queue into
    /// `pending_post_watermark` the same as any other pre-`Done` output
    /// (finding 3's own `Some(_) => queue` arm doesn't distinguish the
    /// two) — but the live parser that produces the checkpoint at
    /// `checkpoint_ready` time has, by construction, ALREADY consumed
    /// everything ever published to this connection up to and including
    /// that exact commit round (`capsule_win.rs`'s watermark barrier:
    /// fsync -> publish -> checkpoint, one loop step, in that order — the
    /// barrier's own ordering was never the bug). Left in the queue, that
    /// backlog is a duplicate of what the checkpoint already encodes, and
    /// got redelivered a SECOND time once `Done`. Fixed by purging
    /// `pending_post_watermark` inside `checkpoint_ready` itself, the one
    /// moment the cut point and the queue are both in scope.
    #[test]
    fn output_committed_before_the_checkpoint_is_taken_is_not_redelivered_after_it() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let a = p.frame(1, hello_frame(), now);
        p.sent(1, a[0].send_marker(), now);
        p.frame(1, attach_frame("alice"), now);

        // Committed WHILE still AwaitingGround -- queues, exactly like any
        // other pre-Done output (finding 3).
        let a = p.output_committed(b"already-in-the-checkpoint", now);
        assert!(a.is_empty(), "no Output send yet -- not Done: {a:?}");

        // Ground reached: the real loop would encode the checkpoint from a
        // live parser that has ALREADY processed the bytes above -- the
        // checkpoint bytes below stand in for a snapshot that already
        // covers "already-in-the-checkpoint".
        let a = p.ground_reached(now);
        assert!(matches!(a.as_slice(), [Action::BeginCheckpoint { conn: c }] if *c == 1));
        let chunk = p.checkpoint_ready(1, b"checkpoint-bytes".to_vec(), now);
        assert!(matches!(
            chunk.as_slice(),
            [Action::Send { marker: Some(SentMarker::CheckpointChunk { is_last: true, .. }), .. }]
        ));

        // Genuinely NEW output, committed AFTER the checkpoint was taken,
        // still queues behind the (still in-flight, one-chunk) transfer
        // normally.
        let a = p.output_committed(b"after-the-checkpoint", now);
        assert!(a.is_empty());

        let flushed = p.sent(1, chunk[0].send_marker(), now);
        assert_eq!(
            flushed.len(),
            1,
            "exactly ONE queued output frame must flush once Done -- the pre-checkpoint backlog must have been purged: {flushed:?}"
        );
        let decoded = decode_one(flushed[0].send_bytes());
        assert_eq!(
            decoded,
            DecodedFrame::AttachServer(AttachServer::Output { bytes: b"after-the-checkpoint".to_vec() }),
            "only output committed AFTER the checkpoint was taken may be redelivered"
        );
    }

    /// Round-2 review, finding 1 (reproduced by the reviewer's own scratch
    /// probe): clearing `pending_post_watermark` at capture retired the
    /// BYTES but not the `queued_live_bytes` CHARGE those same bytes had
    /// already added -- only an `OutputBytes` `Sent` completion ever
    /// decremented it, and the cleared vectors will never produce one.
    /// Queue right up to the 4 MiB ceiling pre-capture, capture (which
    /// must release that exact charge), then commit one more byte: it
    /// must NOT overflow, because nothing is actually outstanding anymore.
    #[test]
    fn checkpoint_capture_releases_the_cleared_backlogs_own_queue_charge() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let a = p.frame(1, hello_frame(), now);
        p.sent(1, a[0].send_marker(), now);
        p.frame(1, attach_frame("alice"), now);

        // Queue EXACTLY the budget ceiling while still AwaitingGround.
        let chunk = vec![0xAAu8; WATCHER_LIVE_QUEUE_BUDGET_BYTES as usize];
        let a = p.output_committed(&chunk, now);
        assert!(a.is_empty(), "at the ceiling, not over it: {a:?}");

        let a = p.ground_reached(now);
        assert!(matches!(a.as_slice(), [Action::BeginCheckpoint { conn: c }] if *c == 1));
        let a = p.checkpoint_ready(1, b"checkpoint-bytes".to_vec(), now);
        assert!(matches!(
            a.as_slice(),
            [Action::Send { marker: Some(SentMarker::CheckpointChunk { is_last: true, .. }), .. }]
        ));

        // One more byte, post-capture: must not overflow -- the pre-
        // capture backlog's charge was released atomically with the
        // capture that cleared its bytes.
        let a = p.output_committed(b"x", now);
        assert!(
            !a.iter().any(|x| matches!(x, Action::Close(_) | Action::RecordRefusal { .. })),
            "a false eviction one byte after capture: {a:?}"
        );
    }

    // -- keepalive --------------------------------------------------------

    /// The keepalive's OWN send is also subject to the generic
    /// progress-stall bound (finding 5: it is just another outstanding
    /// send) -- so an UNCONFIRMED keepalive eventually closes the
    /// connection regardless, via that generic mechanism, and this test
    /// must not (and does not) contradict that. What it proves instead is
    /// the DISTINCT property the ADR pins for the reply deadline
    /// specifically: once the keepalive IS confirmed sent, its OWN 30s
    /// reply window starts fresh from THAT moment, not from whenever it
    /// was originally enqueued -- confirming it late (here, 25s after
    /// enqueue, comfortably inside both bounds) and then checking a point
    /// that would already be expired under a wrongly enqueue-anchored
    /// deadline, but is not under a correctly sent-completion-anchored
    /// one, is what distinguishes the two.
    #[test]
    fn keepalive_deadline_starts_at_sent_completion_not_enqueue() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        drive_take(&mut p, 1, "alice", 1, now);

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

        // Confirmed late -- 25s after being sent, still inside every
        // bound.
        let confirmed_at = idle + Duration::from_secs(25);
        assert!(p.tick(confirmed_at).is_empty());
        p.sent(1, Some(SentMarker::Keepalive { nonce }), confirmed_at);

        // 29s after CONFIRMATION (54s after the original enqueue): past a
        // WRONGLY enqueue-anchored deadline (idle+30s), but still inside a
        // correctly confirmation-anchored one (confirmed_at+30s).
        let a = p.tick(confirmed_at + Duration::from_secs(29));
        assert!(a.is_empty(), "the reply deadline must run from sent-completion, not enqueue: {a:?}");
        let a = p.tick(confirmed_at + Duration::from_secs(31));
        assert!(a.iter().any(|x| matches!(x, Action::Close(1))), "expected keepalive death: {a:?}");
    }

    #[test]
    fn keepalive_reply_wrong_nonce_is_unexpected_and_closes() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        drive_take(&mut p, 1, "alice", 1, now);
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

    /// Finding 6 / round-2 finding 3: a demoted former driver's late
    /// keepalive echo, for a nonce `Conn::last_keepalive_nonce` still
    /// remembers issuing to it, is ignorable, not fatal -- it must not
    /// close the connection, which is still a legitimate subscriber.
    #[test]
    fn demoted_drivers_late_keepalive_echo_is_ignored_not_fatal() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        attach_to_done(&mut p, 2, now);
        drive_take(&mut p, 1, "alice", 1, now);

        let idle = now + KEEPALIVE_IDLE_TRIGGER;
        let a = p.tick(idle);
        let nonce = match &a[0] {
            Action::Send { marker: Some(SentMarker::Keepalive { nonce }), .. } => *nonce,
            other => panic!("{other:?}"),
        };

        // conn 2 takes next, demoting conn 1 -- conn 1 is no longer the
        // driver, but its OWN Conn record still remembers issuing this nonce.
        drive_take(&mut p, 2, "bob", 2, now);

        let echo = decode_one(&encode_keepalive(nonce));
        let a = p.frame(1, echo, now);
        assert!(a.is_empty(), "a demoted driver's late echo must be ignored, not closed: {a:?}");
        let out = p.output_committed(b"hi", now);
        assert!(out.iter().any(|a| matches!(a, Action::Send { conn: 1, .. })));
    }

    /// Round-2 review, finding 3 (reproduced by the reviewer's own
    /// state-machine probe): a SAME-connection retake used to discard the
    /// outstanding nonce along with the rest of `DriverState`, so that
    /// connection's own later late echo of it looked identical to a
    /// fabricated one and was wrongly closed as `UnexpectedKeepalive`.
    #[test]
    fn same_connection_retake_still_treats_its_old_nonce_as_a_legitimate_late_echo() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        drive_take(&mut p, 1, "alice", 1, now);

        let idle = now + KEEPALIVE_IDLE_TRIGGER;
        let a = p.tick(idle);
        let nonce = match &a[0] {
            Action::Send { marker: Some(SentMarker::Keepalive { nonce }), .. } => *nonce,
            other => panic!("{other:?}"),
        };
        p.sent(1, Some(SentMarker::Keepalive { nonce }), idle); // reply deadline armed

        // Conn 1 retakes -- SAME connection, a fresh `DriverState`.
        drive_take(&mut p, 1, "alice", 2, idle);

        // The old ping, echoed after the retake: must be ignored, not
        // closed as `UnexpectedKeepalive`.
        let echo = decode_one(&encode_keepalive(nonce));
        let a = p.frame(1, echo, idle);
        assert!(a.is_empty(), "a same-connection retake's own prior nonce must still echo as legitimate: {a:?}");
    }

    /// The other side of finding 3: a connection that was NEVER issued a
    /// keepalive nonce at all (a watcher, never driver) is a genuine
    /// protocol violation if it echoes one anyway -- not silently waved
    /// through the way a legitimate former driver's late echo is.
    #[test]
    fn a_never_issued_keepalive_nonce_from_any_connection_is_a_protocol_violation() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        attach_to_done(&mut p, 2, now);
        drive_take(&mut p, 1, "alice", 1, now);

        // Conn 2 was never driver and was never issued anything.
        let echo = decode_one(&encode_keepalive(42));
        let a = p.frame(2, echo, now);
        assert!(a.iter().any(|x| matches!(x, Action::Close(2))));
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::UnexpectedKeepalive, .. })));
    }

    // -- progress deadline (finding 5) ------------------------------------

    /// The generic deadline covers EVERY kind of outstanding send (here: an
    /// unconfirmed mgmt reply, not live output) and resets at the
    /// empty→nonempty transition: a connection that sat idle for a LONG
    /// time before anything was ever queued must not be penalized for that
    /// idle stretch the instant something finally is.
    #[test]
    fn progress_deadline_covers_every_kind_of_outstanding_send_and_resets_at_the_empty_to_nonempty_transition() {
        let mut p = proto();
        let now = t0();
        p.connection_opened(1, now);
        let much_later = now + Duration::from_secs(1000);
        let probe = decode_one(&encode_mgmt_request(&MgmtRequest::Probe).unwrap());
        let a = p.frame(1, probe, much_later);
        assert!(matches!(a.as_slice(), [Action::Send { .. }]));

        let a = p.tick(much_later + Duration::from_secs(29));
        assert!(
            a.is_empty(),
            "must not fire early, and must not be penalized for the earlier idle stretch: {a:?}"
        );
        let a = p.tick(much_later + Duration::from_secs(31));
        assert!(a.iter().any(|x| matches!(x, Action::Close(1))), "{a:?}");
        assert!(a
            .iter()
            .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::ProgressStall, .. })));
    }

    #[test]
    fn progress_deadline_still_covers_live_output_specifically() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        let a = p.output_committed(b"hi", now);
        assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::OutputBytes { n: 2 }), .. }]));
        let a = p.tick(now + Duration::from_secs(31));
        assert!(a.iter().any(|x| matches!(x, Action::Close(1))));
    }

    // -- queue accounting -------------------------------------------------

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

    /// Round-2 review, finding 2: an EARLIER exemption here (a real CI
    /// fix at the time, for a real bug -- the driver getting caught by the
    /// SAME eviction as an actually-slow watcher during
    /// `slow_watcher_overflow_closes_while_driver_stays_live`) removed the
    /// ADR's 4 MiB memory bound for the driver entirely, which is NOT what
    /// the budget table says: "driver queue 4 MiB; committed driver-
    /// visible bytes are never dropped while the connection is live, but
    /// transport liveness is bounded... a hung driver cannot wedge the
    /// writer loop" is TWO clauses together -- the bound stays, and
    /// overflow resolves by closing the connection (never by silently
    /// dropping bytes while it stays live). The bytes are never actually
    /// lost either way (durable in the voyage before this call ever runs;
    /// a reconnect replays them via a fresh checkpoint) -- what changes is
    /// the LABEL, `DriverQueueOverflow` instead of `QueueOverflow`, so
    /// step 6's UX can tell the two situations apart.
    #[test]
    fn the_driver_is_closed_on_overflow_same_as_a_watcher_but_labeled_distinctly() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        drive_take(&mut p, 1, "alice", 1, now);
        let a = p.bytes_queued(1, WATCHER_LIVE_QUEUE_BUDGET_BYTES + 1, now);
        assert!(a.iter().any(|x| matches!(x, Action::Close(1))), "the driver must still be closed on overflow: {a:?}");
        assert!(
            a.iter()
                .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::DriverQueueOverflow, .. })),
            "a driver's overflow must be labeled distinctly from an ordinary watcher's: {a:?}"
        );
    }

    /// Reconstructs, event-for-event, what
    /// `tests/capsule_win.rs::slow_watcher_overflow_closes_while_driver_stays_live`
    /// drives through the wire (PR #139 discharge round CI failure: "timed
    /// out waiting for an expected frame on conn 1"): conn 1 (DRIVER)
    /// attaches and takes; conn 2 (WATCHER) attaches to a `Done` checkpoint
    /// -- exactly like the integration test's `collect_checkpoint`
    /// completing BEFORE `set_hold_for` -- then is held forever (its
    /// `Output` sends are never confirmed here, matching a client that
    /// stopped reading its pipe); a 6 MiB flood, in the capsule's own
    /// `GROUP_COMMIT_BYTES`-sized (256 KiB) increments, is published to
    /// both, with the driver's own sends confirmed immediately every round
    /// (an unheld synthetic transport). Once the watcher is evicted, the
    /// driver must still answer a `resize`.
    ///
    /// `now` never advances past `t0()`: this isolates the state machine's
    /// own LOGIC from real wall-clock throughput. If this passes, the bug
    /// is not in `AttachProto` and must be sought in `capsule_win.rs`'s
    /// wiring or the integration test's own synthetic transport.
    #[test]
    fn replay_slow_watcher_flood_the_driver_still_answers_a_resize() {
        let mut p = proto();
        let now = t0();
        const DRIVER: ConnId = 1;
        const WATCHER: ConnId = 2;

        attach_to_done(&mut p, DRIVER, now);
        drive_take(&mut p, DRIVER, "driver", 1, now);
        attach_to_done(&mut p, WATCHER, now);

        const CHUNK: usize = 256 * 1024;
        const TOTAL: usize = 6 * 1024 * 1024;
        let chunk = vec![0xAAu8; CHUNK];
        let mut sent_so_far = 0usize;
        let mut watcher_closed = false;
        while sent_so_far < TOTAL {
            let actions = p.output_committed(&chunk, now);
            for a in &actions {
                match a {
                    Action::Send { conn, .. } if *conn == DRIVER => {
                        p.sent(DRIVER, a.send_marker(), now);
                    }
                    Action::Send { conn, .. } if *conn == WATCHER => {
                        // Held forever -- never confirmed, exactly like
                        // `set_hold_for(WATCHER, true)`.
                    }
                    Action::Close(c) if *c == WATCHER => watcher_closed = true,
                    Action::RecordRefusal { conn: Some(c), reason: RefusalReason::QueueOverflow } if *c == WATCHER => {}
                    other => panic!("unexpected action mid-flood: {other:?}"),
                }
            }
            sent_so_far += CHUNK;
            if watcher_closed {
                break;
            }
        }
        assert!(
            watcher_closed,
            "the watcher must be evicted well before 6 MiB, exactly like the integration test"
        );

        let resize = decode_one(&encode_attach_client(&AttachClient::Resize { cols: 100, rows: 40 }).unwrap());
        let a = p.frame(DRIVER, resize, now);
        let request_id = match a.as_slice() {
            [Action::ApplyResize { request_id, conn, .. }] if *conn == DRIVER => *request_id,
            other => panic!("expected ApplyResize for the driver, got: {other:?}"),
        };
        let a = p.resize_outcome(DRIVER, true, request_id, now);
        assert!(
            matches!(
                a.as_slice(),
                [Action::Send { conn, marker: Some(SentMarker::Reply { .. }), .. }] if *conn == DRIVER
            ),
            "expected a ResizeOk reply Send for the driver: {a:?}"
        );
    }

    /// Directly answers the coordinator's "double-check the timeout
    /// arithmetic" ask: does the resize this scenario needs answered
    /// legitimately require a keepalive/progress interval to elapse first
    /// (making the integration test's 10s wait too short BY DESIGN), or is
    /// the reply available immediately regardless? Advances `now` well
    /// past `KEEPALIVE_IDLE_TRIGGER` (30s) since the take -- long enough
    /// for `tick` to actually arm a keepalive on the driver, exactly what
    /// a slow-draining flood (during which the driver sends no wire
    /// traffic of its own) would do -- then leaves it UNANSWERED (the
    /// integration test's harness implements no keepalive-reply logic at
    /// all) and confirms the SAME resize still gets an immediate
    /// `ResizeOk`, at that SAME `now`. No deadline in this module gates a
    /// resize behind a keepalive; the ten-second wait is not too short by
    /// protocol design.
    #[test]
    fn resize_answers_immediately_even_with_a_keepalive_outstanding_on_the_driver() {
        let mut p = proto();
        let now = t0();
        const DRIVER: ConnId = 1;
        attach_to_done(&mut p, DRIVER, now);
        drive_take(&mut p, DRIVER, "driver", 1, now);

        let later = now + KEEPALIVE_IDLE_TRIGGER + Duration::from_secs(1);
        let a = p.tick(later);
        assert!(
            matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::Keepalive { .. }), .. }]),
            "expected tick to arm a keepalive after the idle trigger: {a:?}"
        );

        let resize = decode_one(&encode_attach_client(&AttachClient::Resize { cols: 100, rows: 40 }).unwrap());
        let a = p.frame(DRIVER, resize, later);
        let request_id = match a.as_slice() {
            [Action::ApplyResize { request_id, conn, .. }] if *conn == DRIVER => *request_id,
            other => panic!("expected ApplyResize for the driver: {other:?}"),
        };
        let a = p.resize_outcome(DRIVER, true, request_id, later);
        assert!(
            matches!(
                a.as_slice(),
                [Action::Send { conn, marker: Some(SentMarker::Reply { .. }), .. }] if *conn == DRIVER
            ),
            "an outstanding, unanswered keepalive must not delay or block a resize reply: {a:?}"
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

    // -- teardown (finding 7) ---------------------------------------------

    #[test]
    fn teardown_ignores_producer_bound_requests_but_not_mgmt_or_attach() {
        let mut p = proto();
        let now = t0();
        attach_to_done(&mut p, 1, now);
        drive_take(&mut p, 1, "alice", 1, now);
        p.begin_teardown();

        let input = decode_one(
            &encode_attach_client(&AttachClient::Input {
                controller_id: "alice".into(),
                take_epoch: 1,
                idem_key: [1u8; 16],
                payload: b"x".to_vec(),
            })
            .unwrap(),
        );
        assert_eq!(p.frame(1, input, now), vec![]);
        let resize = decode_one(&encode_attach_client(&AttachClient::Resize { cols: 100, rows: 40 }).unwrap());
        assert_eq!(p.frame(1, resize, now), vec![]);

        // No lockstep leak from an ignored request: a follow-up (still
        // ignored) request on the same connection produces no violation
        // either.
        let resize2 = decode_one(&encode_attach_client(&AttachClient::Resize { cols: 90, rows: 30 }).unwrap());
        let a = p.frame(1, resize2, now);
        assert!(a.is_empty());
        assert!(!a.iter().any(|x| matches!(x, Action::Close(_))));

        // mgmt still works, on a separate connection.
        p.connection_opened(2, now);
        let a = p.frame(2, decode_one(&encode_mgmt_request(&MgmtRequest::Probe).unwrap()), now);
        assert!(matches!(a.as_slice(), [Action::Send { .. }]), "mgmt must still be serviced during teardown: {a:?}");
    }
}
