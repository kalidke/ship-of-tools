//! The platform-neutral transport contract every Ship's Log capsule
//! drives against: [`TransportEvent`], [`Transport`], and the shared
//! teardown bound + poll-join primitive every implementation's worker
//! threads join against.
//!
//! Deliberately ungated — no `#![cfg(windows)]` here, unlike most of this
//! crate's siblings: this is the CONTRACT, not an implementation.
//! Implementers: Windows — `crate::pipe_transport::PipeTransport`
//! (bridging `crate::pipe_win`'s real named pipe); tests — the
//! in-memory synthetic transports in `tests/capsule_win.rs`; Unix — a
//! domain-socket implementation lands in L1-unix's LU1.
//!
//! [`ConnId`] lives in [`crate::attach_proto`], not here — this module
//! only uses it. [`TEARDOWN_AGGREGATE_DEADLINE`]/`join_within` are the
//! one teardown bound and poll-join both platforms, and `capsule_win`'s
//! own closer/reader thread joins, share.

use crate::attach_proto::ConnId;
use crate::Result;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// One event the transport layer reports to the writer loop. The
/// production transport produces these (a named pipe on Windows today; a
/// Unix domain socket once L1-unix lands); `capsule_win`'s tests drive
/// the identical channel with a synthetic transport (ADR 0041 step 5:
/// "the loop gains a transport-event channel... for THIS unit, a test
/// transport").
#[derive(Debug)]
pub enum TransportEvent {
    /// A new connection accepted (mgmt or attach — lane is unknown
    /// until its first frame; see `attach_proto`'s module doc).
    ConnectionOpened(ConnId),
    /// Raw bytes read from `conn`, in order. May contain zero, one, or
    /// several complete frames, or a partial one carried to the next call.
    Bytes(ConnId, Vec<u8>),
    /// `conn` is gone (ordered EOF or a transport error) — capability-only
    /// EOF per the pen (see `attach_proto`'s module doc); no durable
    /// transition happens here.
    ConnectionClosed(ConnId),
    /// A previously requested [`Transport::send`] (identified by the send
    /// id that call returned) is now reported PHYSICALLY WRITTEN. Arriving
    /// through this SAME event channel keeps completion ordering
    /// centralized in the one loop that already serializes everything
    /// else.
    Sent(ConnId, u64),
    /// The transport itself has permanently, terminally failed — no future
    /// connection can ever be accepted while this capsule holds the
    /// voyage's name (round-2 e2e review, finding 4: `pipe_win`'s
    /// `AcceptError` is this event for the Windows transport).
    /// Unlike every other variant, this is not a per-connection event and
    /// carries no `ConnId` — `run` maps it to an ORDERLY self-end, the
    /// SAME path as an externally requested `EndRun`
    /// (`shutdown_requested`/`shutdown_reason`, reason
    /// "transport-accept-failed"): drain, `producer_dead`, seal
    /// verify-green, exit — so a step-6 supervisor sees an ordinary
    /// "run ended, new leg" and respawns a fresh capsule that binds a
    /// fresh listener, rather than a wedged process silently unreachable
    /// forever. The `String` is a diagnostic detail, logged but not
    /// itself part of the recorded reason (which stays the fixed,
    /// supervisor-matchable string above).
    TransportFatal(String),
}

/// What the writer loop needs from a transport to deliver bytes and sever
/// connections. The Windows transport (`pipe_transport` over `pipe_win`)
/// implements this over a named pipe; `capsule_win`'s tests implement it
/// as an in-memory sink; the Unix transport (L1-unix) implements it over
/// a Unix domain socket.
///
/// # Contract (round-2 review, finding 7)
///
/// The protocol machine's own correctness depends on THREE properties this
/// trait's prior docs implied but never actually required of an
/// implementation:
///
/// 1. **`send` enqueues without blocking.** It queues the write and
///    returns; it never waits for the write to complete before returning
///    control to the loop (a genuinely asynchronous transport issues the
///    write and returns immediately; a synchronous test transport may
///    report completion inline, but still returns before doing so from
///    inside `send` itself — see `send`'s own doc).
/// 2. **Every `(conn, id)` pair `send` ever returns is unique while
///    outstanding.** The loop keys `pending_sends` by exactly that pair
///    (finding 11); a reused id before its predecessor's completion is
///    reported makes the wrong marker resolve for a later `Sent`. The loop
///    asserts this at the `send` call site — an implementation that
///    violates it is a bug in the TRANSPORT, not something the protocol
///    machine can route around.
/// 3. **Per-connection FIFO byte delivery.** Bytes queued for `conn` via
///    `send` must arrive at the peer, and be reported [`TransportEvent::
///    Sent`], in the SAME order they were enqueued — the protocol's own
///    ordering guarantees (checkpoint chunks before the post-watermark
///    output queued behind them, one reply before the next request on the
///    same connection) assume this, not just document it as convenient.
///
/// A [`TransportEvent::Sent`] for an id the loop has no record of is
/// tolerated ONLY for a connection already closed (a late completion
/// racing the close, purged already — finding 11); for a connection still
/// considered active it is a contract violation and the loop fails loudly
/// rather than silently dropping it.
pub trait Transport {
    /// Bind the transport to `voyage_id` — called by `run` EXACTLY ONCE,
    /// immediately after `open_for_writing` has the voyage's writer lock
    /// and before anything else that could fail, so that on EVERY exit
    /// path (including a `bind` failure itself) the listener-lifetime
    /// invariant ("the transport is never live while the writer lock is
    /// free") is enforced by CODE ORDER alone: nothing before this call
    /// can have made the transport live, and [`Transport::shutdown_all`]
    /// — reached via `run`'s `ShutdownGuard` on every path, `bind`'s own
    /// early return included — always runs before `store`'s drop releases
    /// the lock. A transport with nothing to bind (a synthetic test
    /// transport driving the wire lane directly) implements this as a
    /// no-op `Ok(())`.
    fn bind(&mut self, voyage_id: &str) -> Result<()>;
    /// Return the next available [`TransportEvent`] for this transport, or
    /// `None` if nothing is available RIGHT NOW — round-2 e2e review's
    /// deletion pressure: `run` used to take a separate
    /// `mpsc::Receiver<TransportEvent>` parameter, which only existed to
    /// let a transport hand events back on a channel of its own; polling
    /// through the trait itself removes that whole seam (an unbounded
    /// forwarding channel, an actor thread, a join) for an implementation
    /// that already owns a channel — real or synthetic — of its own.
    /// MUST NOT BLOCK: called once per MAIN-LOOP iteration
    /// (`service_transport_events!`/`_teardown!`), in a `while let Some(ev)
    /// = ...` drain, immediately BEFORE this loop's own
    /// `output_rx.recv_timeout(GROUP_COMMIT_WINDOW)` wait — that recv is
    /// this loop's ONE latency budget per iteration; a blocking or
    /// timed-wait implementation here would add a second, uncoordinated
    /// wait ahead of it every iteration, silently doubling worst-case
    /// per-iteration latency regardless of how small the deadline
    /// (see the module doc's own `output_rx.recv_timeout` starvation
    /// history for why an extra wait on this loop's thread is never free).
    fn try_recv_event(&mut self) -> Option<TransportEvent>;
    /// Queue `bytes` for `conn`. Returns an opaque send id; the transport
    /// reports physical completion via [`TransportEvent::Sent`]`(conn, id)`
    /// on the SAME event channel this loop polls — a genuinely
    /// asynchronous transport reports it whenever the write actually
    /// finishes; a synchronous test transport may push it immediately,
    /// since that is an ordinary channel send picked up on this loop's
    /// NEXT poll, not a same-stack callback into it. Must return before
    /// the completion it eventually reports, never after — see the
    /// trait's own contract doc above.
    fn send(&mut self, conn: ConnId, bytes: Vec<u8>) -> u64;
    /// Sever `conn` at the transport level. Idempotent: closing an
    /// already-closed or unknown connection is a no-op.
    fn close(&mut self, conn: ConnId);
    /// Close EVERY connection and stop accepting new ones, so the
    /// transport is guaranteed closed BEFORE the writer lock releases
    /// (finding 7: "the capsule can enforce pipe-closed-before-lock-
    /// release"). The production transport already closes everything on
    /// `Drop`; this method exists so the writer loop can trigger that
    /// deterministically at the right moment rather than relying on
    /// cross-type drop ordering between the transport and the
    /// `VoyageStore` holding the lock.
    ///
    /// MUST BE IDEMPOTENT (U1a): `run` now calls this explicitly once the
    /// ack-grace window resolves (see the `SHUTDOWN_ACK_GRACE` call site),
    /// promptly disposing of the transport rather than leaving it live
    /// through the remaining process-exit wait and seal — and
    /// `ShutdownGuard`'s own `Drop`, reached on EVERY exit path including
    /// that one, calls this again unconditionally afterward. A SECOND
    /// call, on an already shut-down transport, must be a safe no-op: it
    /// is not an error for this to run twice, and an implementation that
    /// panics or double-frees on a repeat call breaks every exit path,
    /// not merely the one that exercises the ack grace.
    ///
    /// Codex round-1 Blocker 3 discharge: `deadline` is the SAME absolute
    /// aggregate deadline `run` shares with its own closer/reader thread
    /// joins (ADR 0041 "the joins share ONE 20 s absolute deadline") — a
    /// real transport must issue cancellation to every worker it owns
    /// FIRST (so the listener name and every connection are gone before
    /// any blocking wait), THEN join everything against `deadline` rather
    /// than a budget it invents itself. Returns `true` iff every one of
    /// ITS OWN joins finished within `deadline`; `false` — LOUD, and the
    /// caller MUST treat this as terminal (no seal, no fence release past
    /// it), since this crate cannot force an OS thread to stop. A
    /// synthetic test transport with nothing to bound returns `true`
    /// unconditionally.
    fn shutdown_all(&mut self, deadline: Instant) -> bool;
}

/// ADR 0041 Lifecycle "the pipe NAME disappears before any blocking
/// join" / the bounds table's "teardown aggregate": 20 s TOTAL after the
/// listener is gone, one absolute deadline shared by every join
/// (acceptor, reaper, and — inside the reaper's own drain — every
/// connection worker), loud on expiry. Shared by every platform's
/// transport and by `capsule_win`'s own closer/reader joins — one
/// constant, one mechanism.
pub const TEARDOWN_AGGREGATE_DEADLINE: Duration = Duration::from_secs(20);

/// L1-unix LU1b (ADR 0043 "Bounds are the same numbers on both
/// platforms"): the total connect retry budget, hoisted here from
/// `pipe_win.rs`'s own `PIPE_CONNECT_BOUND` — the one bound LU1a left
/// behind because only one platform's client existed yet. Both
/// `pipe_win::connect_named_pipe_unchallenged` (Windows, retrying
/// `ERROR_PIPE_BUSY`/`ERROR_FILE_NOT_FOUND`) and the Unix client's own
/// connect retry (LU1c, retrying `ECONNREFUSED`/`ENOENT`/`EAGAIN`) share
/// this SAME constant — ADR 0043 decision 4's "a deliberate, documented
/// difference from `PIPE_BUSY`" is in which errno family each retries,
/// never in how long either budget runs.
pub const CONNECT_BOUND: Duration = Duration::from_secs(2);

/// [`join_within`]'s poll granularity — small enough that a fast, healthy
/// teardown (the ordinary case) never visibly waits for it, and small
/// enough that a test proving "loud on expiry" against a short injected
/// budget stays fast too.
// On non-Windows nothing calls these two until LU1's Unix transport lands —
// the same device `host_handshake`/`deadline` use in lib.rs, so the Linux and
// macOS builds stay warning-free without gating the contract module itself.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const JOIN_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Wait for `jh` to finish without ever calling the BLOCKING
/// `JoinHandle::join` — std gives that call no timeout, which is exactly
/// what "each wait taking the remaining budget" (ADR 0041) rules out.
/// Polls [`JoinHandle::is_finished`] (never blocks) until either it
/// reports true (join it for real — `is_finished() == true` guarantees
/// that call cannot then block — and return `true`) or `deadline` passes
/// (`false`: LOUD, since this crate cannot force an OS thread to stop —
/// the handle is simply dropped here, which detaches rather than kills
/// it; the thread keeps running in the background). `pub(crate)`:
/// `capsule_win.rs` reuses this SAME mechanism (Codex round-1 Blocker 3)
/// to join its own closer/reader threads under the identical aggregate
/// deadline, rather than a second bespoke poll loop.
///
/// **The exact boundary semantic (Codex round-2b, ruling on finding 2,
/// a reasoned deviation from literal strictness):** expiry is terminal
/// IFF a thread REMAINS UNFINISHED at the instant the budget is
/// exhausted — `is_finished` is checked FIRST on every poll, including
/// the one that observes the expired deadline, so a thread that
/// genuinely finished at or before that exact instant is accepted
/// (`true`) even though the deadline has ALSO passed by the time this
/// function notices. This is deliberate, not a race: the invariant this
/// bound exists to serve is "never proceed past an UNFINISHED worker",
/// and a thread that has ALREADY finished leaves nothing dangling — it
/// satisfies that invariant regardless of which check this function
/// happened to run first. Flagging it as failed would be a false alarm,
/// not a safety property. There is no "acceptance after the decision"
/// hazard either way: once this function returns, its `bool` is the
/// decision, made exactly once, from the caller's own single call site —
/// nothing later re-evaluates or overturns it, whether the answer was
/// `true` or `false`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn join_within(jh: JoinHandle<()>, deadline: Instant) -> bool {
    loop {
        if jh.is_finished() {
            let _ = jh.join();
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(JOIN_POLL_INTERVAL);
    }
}

// ---------------------------------------------------------------------
// Shared implementation helpers — both platforms' transports use these
// (L1-unix LU1a, hoisted out of `pipe_win.rs`). Not part of the
// `Transport` CONTRACT above; a Unix transport implementation is free to
// reuse them exactly as `pipe_win.rs` does, or not, at its own
// discretion.
// ---------------------------------------------------------------------

/// Bound on one outstanding overlapped `ReadFile` (ADR 0041: "the transport
/// just must not read unboundedly ahead").
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const READ_BUF_LEN: usize = 65_536;

/// Per-connection outbound byte budget: enqueued-but-not-yet-physically-
/// written bytes, INCLUDING the writer's in-flight item, may never exceed
/// this. Sized to the same order of magnitude as the ADR's own "4 MiB
/// per-watcher queue" figure — not a literal citation of it (that number
/// bounds a different queue, the future capsule's checkpoint transfer),
/// just a consistent order of magnitude for this transport's own ceiling.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const OUTBOUND_BUDGET_BYTES: usize = 4 * 1024 * 1024;

/// The `events()` channel's item capacity: sized so a run of maximum-size
/// `Bytes` deliveries caps buffered inbound at roughly the same order of
/// magnitude as [`OUTBOUND_BUDGET_BYTES`].
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const EVENTS_CHANNEL_CAP: usize = OUTBOUND_BUDGET_BYTES / READ_BUF_LEN;

/// How long a stalled delivery (lifecycle retry, or one `Bytes` attempt)
/// sleeps between retries against a full `events` channel.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const EVENTS_RETRY_INTERVAL: Duration = Duration::from_millis(20);

/// How long a stalled `Bytes` delivery may retry a single chunk against a
/// full `events` channel before abandoning it and force-closing the
/// connection with a guaranteed `Closed`. Generous relative to
/// [`EVENTS_RETRY_INTERVAL`] — this is "the consumer has genuinely
/// stalled," not routine backpressure.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const BYTES_ABANDON_AFTER: Duration = Duration::from_secs(5);

/// Per-connection outbound byte accounting: reserved eagerly before an
/// item is even queued, released only once the writer's submission for
/// that item RETURNS (success or failure) — the in-flight item stays
/// counted the whole time. The cap is always [`OUTBOUND_BUDGET_BYTES`] —
/// not configurable, so no field for it.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct OutboundBudget {
    used: Mutex<usize>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl OutboundBudget {
    pub(crate) fn new() -> Self {
        Self {
            used: Mutex::new(0),
        }
    }

    pub(crate) fn try_reserve(&self, n: usize) -> bool {
        let mut used = self.used.lock().unwrap();
        if *used + n > OUTBOUND_BUDGET_BYTES {
            return false;
        }
        *used += n;
        true
    }

    pub(crate) fn release(&self, n: usize) {
        let mut used = self.used.lock().unwrap();
        *used = used.saturating_sub(n);
    }
}

/// A connection's reader/writer threads block here until released — the
/// gate is opened only after the connection is fully registered and its
/// initial lifecycle event has been RELIABLY queued, or `abort`ed if the
/// connection could not be fully set up, in which case the gated thread
/// returns immediately, having never touched the transport.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateSignal {
    Wait,
    Start,
    Abort,
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct StartGate {
    state: Mutex<GateSignal>,
    cv: Condvar,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl StartGate {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(GateSignal::Wait),
            cv: Condvar::new(),
        })
    }

    /// Blocks until `open` or `abort`; returns `true` to proceed, `false`
    /// to return immediately without ever touching the pipe.
    pub(crate) fn wait_for_start(&self) -> bool {
        let mut st = self.state.lock().unwrap();
        while *st == GateSignal::Wait {
            st = self.cv.wait(st).unwrap();
        }
        *st == GateSignal::Start
    }

    pub(crate) fn open(&self) {
        *self.state.lock().unwrap() = GateSignal::Start;
        self.cv.notify_all();
    }

    pub(crate) fn abort(&self) {
        *self.state.lock().unwrap() = GateSignal::Abort;
        self.cv.notify_all();
    }
}

// ---------------------------------------------------------------------
// L1-unix LU3a (ADR 0043 decisions 17/19): the ONE error type and event
// vocabulary every concrete `pipe_win`/`socket_unix` transport now
// produces, plus the `LaneServer` seam trait a step-6 supervisor will
// eventually be generic over (LU3c). This section is a pure hoist —
// `pipe_win::PipeError`/`socket_unix::SocketError` and each module's own
// `TransportEvent`/`ClosedReason` were already identical in shape (Codex
// review of ADR 0043's own sizing note); nothing here changes behavior,
// and no consumer goes generic yet — every existing call site still
// names the concrete `PipeServer`/`SocketServer`/`PipeClient`/
// `SocketClient` type directly.
// ---------------------------------------------------------------------

/// Errors a concrete transport's own synchronous API surface can report
/// at the call site — `crate::pipe_win`'s former `PipeError` and
/// [`crate::socket_unix`]'s former `SocketError` merged into one type
/// (ADR 0043 decision 17): the union of both platforms' variants,
/// `Io { op, source }` kept verbatim (the shape production code already
/// matches on), `InvalidMaxInstances` folded into `InvalidMaxConnections`
/// (one name for one concept), and the Unix-only
/// `PathTooLong`/`RuntimeDir`/`Unsupported` carried unconditionally — an
/// enum variant costs nothing on a platform that never produces it.
/// Background-thread failures still surface as [`LaneEvent::Closed`]/
/// [`LaneEvent::AcceptError`] instead — this type is only ever returned
/// from a synchronous call, never delivered through the event channel.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("invalid voyage id {0:?}: must be the canonical lowercase-hyphenated form of an RFC 4122 UUID")]
    InvalidVoyageId(String),
    #[error("max_connections must be between 1 and 255")]
    InvalidMaxConnections,
    // Unix only (never produced on Windows, where there is no `sun_path`
    // limit to exceed): the exact byte ceiling is platform-specific
    // (`crate::socket_unix::max_sun_path_bytes`, unreachable from this
    // ungated module on a non-Unix target), so the message names the
    // path without embedding a number this type cannot portably compute.
    #[error("endpoint path {0:?} exceeds this platform's path-length limit")]
    PathTooLong(PathBuf),
    #[error("resolving the runtime dir: {0}")]
    RuntimeDir(std::io::Error),
    #[error("{op}: {source}")]
    Io {
        op: &'static str,
        source: std::io::Error,
    },
    #[error("unknown or already-closed connection {0}")]
    UnknownConnection(ConnId),
    #[error("outbound budget exhausted for connection {0}")]
    QueueFull(ConnId),
    #[error("empty payload: this wire never carries a zero-length send")]
    EmptyPayload,
    #[error("payload of {0} bytes exceeds what a single write/read call can represent")]
    PayloadTooLarge(usize),
    #[error("operation cancelled")]
    Cancelled,
    /// A second same-direction client call (e.g. two concurrent `read`s)
    /// was rejected before it ever touched the OS or a shared I/O slot —
    /// misuse, not a race this crate resolves for the caller.
    #[error("another operation is already pending on this client's same direction")]
    ConcurrentSubmit,
    /// A connect-time peer-identity check (ADR 0041 Lifecycle "The
    /// challenge", steps 1-3 — Windows: token-user SID comparison via
    /// `challenge_win::authenticate_server`; Linux: `SO_PEERCRED` same-
    /// user comparison via `challenge_unix::authenticate_server` — NOT
    /// the full five-step `challenge()`, see either function's own doc)
    /// answered with a WELL-FORMED WRONG proof — a different account's
    /// process is behind this endpoint. A loud, typed failure: never
    /// retried as if the peer might still turn out legitimate.
    #[error("the peer failed identity authentication (a different account's process is behind this endpoint)")]
    Foreign,
    /// Peer identity authentication could not be completed at all — an
    /// OS-call failure anywhere in the platform's own steps 1-3. Never
    /// silently treated as either authenticated or foreign (ADR 0041: "a
    /// failure... is PENDING, never READY and never ADOPTED").
    #[error("peer identity authentication could not be completed (peer identity undetermined)")]
    Undetermined,
    /// This Unix target has no kernel-provided peer-pid mechanism this
    /// crate trusts (`SO_PEERCRED`'s pid field and `pidfd_open` are
    /// Linux-specific) — the connect constructor fails closed here
    /// rather than skip authentication silently. Never produced on
    /// Windows.
    #[error("{0}")]
    Unsupported(&'static str),
}

impl TransportError {
    /// L1-unix LU3c (ADR 0043 decision 21): true iff this error means
    /// "the endpoint is not there right now" — a pipe name that does not
    /// exist (Windows: `NotFound`) or a Unix domain socket path with no
    /// listener (`ConnectionRefused` — the kernel refuses a connect
    /// against a bound but unlistened, or unlinked, path; a path that was
    /// never created at all is `NotFound` there too). The supervisor's
    /// own `end_run_over_mgmt_lane`/`probe_writer_liveness` use this ONE
    /// predicate on both platforms instead of the former inline
    /// `NotFound`-only guard: Windows behavior is unchanged (a named
    /// pipe connect never yields `ConnectionRefused`), so this widens
    /// nothing there.
    pub fn is_endpoint_absent(&self) -> bool {
        matches!(
            self,
            TransportError::Io { source, .. }
                if matches!(source.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)
        )
    }
}

/// Why a connection ended, reported once per connection on
/// [`LaneEvent::Closed`] — hoisted (ADR 0043 decision 19) from
/// `pipe_win`/`socket_unix`'s own byte-for-byte identical copies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosedReason {
    /// The peer disconnected (or this side observed a broken/reset/
    /// unconnected endpoint) — detected by the connection's own reader
    /// loop, or (rarely) its writer.
    Eof,
    /// The server's own `close` call tore this connection down.
    Closed,
    /// An I/O error other than a recognized disconnect ended a
    /// connection's reader or writer loop, or its `Bytes` delivery was
    /// abandoned — always paired with this guaranteed notification,
    /// never a silent stream gap.
    Error(String),
}

/// This transport's event surface to its consumer, hoisted (ADR 0043
/// decision 19) from `pipe_win`/`socket_unix`'s own byte-for-byte
/// identical copies — both platforms' servers produce the SAME event
/// type now, so `pipe_transport`'s and `socket_transport`'s own
/// `translate()` functions read this one type instead of two separately
/// defined ones. Delivered over a `LaneServer::events()` receiver in the
/// order the transport observed them; the consumer feeds `Bytes`
/// payloads to its own [`crate::wire::FrameSplitter`] per connection.
///
/// Not to be confused with this module's own [`TransportEvent`] (the
/// OLDER, ADR-0041-era capsule-facing contract `Transport::try_recv_event`
/// returns) — that is a DIFFERENT vocabulary, one level up the stack
/// (`ConnectionOpened`/`ConnectionClosed`/`TransportFatal`, no
/// `ClosedReason`); `pipe_transport`/`socket_transport` translate FROM
/// this type TO that one.
#[derive(Debug)]
pub enum LaneEvent {
    /// A new connection accepted; `send`/`close` may now target it.
    Accepted(ConnId),
    /// Raw bytes read from a connection, in the order read. Never empty.
    Bytes(ConnId, Vec<u8>),
    /// A marker-tagged `send` call has physically completed.
    Sent(ConnId, u64),
    /// The connection ended; no further events for this `ConnId` follow.
    Closed(ConnId, ClosedReason),
    /// The accept loop hit a persistent, unrecoverable resource failure
    /// and has stopped accepting new connections FOR GOOD — existing
    /// connections are unaffected.
    AcceptError(String),
}

/// L1-unix LU3a (ADR 0043 decision 19): what a step-6 supervisor needs
/// from a lane's own server-side transport, seamed so it can eventually
/// be generic over `PipeServer`/`SocketServer` (LU3c) — today, both
/// implement this purely by delegation, and every existing consumer
/// keeps calling the concrete type directly (no consumer goes generic in
/// this lane). Only `bind_supervisor` is part of the seam: the plain
/// voyage `bind` stays a concrete-type-only constructor, since nothing
/// in this crate needs a generic voyage bind yet.
pub trait LaneServer: Sized {
    /// Bind the supervisor lane's own endpoint and start accepting.
    /// `max_connections` must be in `1..=255`.
    fn bind_supervisor(h: &str, max_connections: u32) -> std::result::Result<Self, TransportError>;
    /// The event stream: single-consumer by convention (a `Receiver` is
    /// not `Sync`).
    fn events(&self) -> &std::sync::mpsc::Receiver<LaneEvent>;
    /// Queue `bytes` for `conn`, tagged with `marker` if the caller wants
    /// a [`LaneEvent::Sent`] once the OS write physically completes.
    /// Non-blocking.
    fn send(&self, conn: ConnId, bytes: Vec<u8>, marker: Option<u64>) -> std::result::Result<(), TransportError>;
    /// Request that `conn` be torn down. Fire-and-forget; a no-op if
    /// already gone or already tearing down.
    fn close(&self, conn: ConnId);
    /// Phase one of teardown: make the endpoint's NAME disappear and
    /// issue cancellation to every worker — synchronous, no blocking
    /// join.
    fn disconnect_listener(&mut self);
    /// Phase two: wait for every thread this transport owns, against one
    /// shared absolute `deadline`. `true` iff every one finished within
    /// budget; `false` — LOUD, terminal — on expiry.
    fn join_workers(&mut self, deadline: Instant) -> bool;
}

/// L1-unix LU3c: the server twin of `client::PlatformEndpoint` — the
/// lane a step-6 supervisor binds on the platform it runs on, chosen
/// ONCE by this alias (never by threading a type parameter through
/// `supervisor.rs`'s own state machine). Windows speaks `PipeServer`;
/// Linux speaks `SocketServer`.
#[cfg(windows)]
pub type PlatformLaneServer = crate::pipe_win::PipeServer;
#[cfg(target_os = "linux")]
pub type PlatformLaneServer = crate::socket_unix::SocketServer;
