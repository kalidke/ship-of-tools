//! The ADR 0041 step-5 Windows named-pipe transport: a server and client
//! for `\\.\pipe\sot-voyage-<id>`. It moves bytes and reports completions;
//! it does not know about mgmt/attach lanes, `hello`, opcodes, or
//! checkpoints — `wire.rs` owns every frame shape, and
//! [`wire::FrameSplitter`] is what a consumer of this module's `Bytes`
//! events feeds. Transport only: no dependency on the capsule or
//! `sot-capsule` bin, and none may be added here.
//!
//! # The I/O slot: one state machine, every direction, every role
//!
//! A `Mutex<SlotState>` (`Idle` / `Pending` / `Closing`) guards an
//! address-stable, MANUAL-RESET-event-backed `OVERLAPPED` — manual, not
//! auto, because Microsoft documents overlapped pipe I/O against
//! manual-reset events and warns that an auto-reset event can hang
//! `GetOverlappedResult(..., TRUE)` when a completion races the wait
//! call. [`IoSlot::submit_and_wait`] resets and issues the OS call and
//! flips the state to `Pending` ALL UNDER ONE LOCK ACQUISITION, then
//! releases the lock before the (possibly long) wait — so
//! [`IoSlot::cancel`], which also takes that lock, can only ever observe
//! `Idle` (latch `Closing`; the next submission refuses before touching
//! the OS) or `Pending` (call `CancelIoEx`, then latch `Closing`). A
//! cancel can never miss a submission that hasn't happened yet, nor land
//! after one has already started reusing the structure. The same check
//! also rejects a SECOND submission while one is already `Pending`,
//! distinctly from `Closing` — reachable only through `PipeClient`, whose
//! `read`/`write_all` take `&self`: without this, two concurrent
//! same-direction callers could both reset and reissue the one shared
//! `OVERLAPPED`, corrupting whichever completed second. Rejecting it
//! (`PipeError::ConcurrentSubmit` at the `PipeClient` boundary, never
//! touching the OS) is what makes `unsafe impl Sync for IoSlot` sound:
//! completion is always consumed by exactly the one thread that got past
//! this check.
//!
//! [`IoSlot::submit_and_wait`] is the CLIENT-facing primitive: it takes a
//! plain `HANDLE` the caller already owns outright. Every SERVER-side
//! instance handle instead goes through
//! [`IoSlot::submit_and_wait_registered`]/[`IoSlot::cancel_registered`]
//! (ADR 0041 step 6 U1b, Codex round-4), which additionally proves the
//! handle is still registered — via [`InstanceRegistry::live`] — for
//! exactly the moment it is handed to the OS, never merely inferring
//! liveness from some other, staler signal. See [`LiveHandle`]'s own doc
//! for the invariant this establishes and the "One registry, one closer"
//! section below for why it is necessary.
//!
//! # Reaping: one thread owns every join
//!
//! A single dedicated REAPER thread (started in [`PipeServer::bind`],
//! alongside the accept thread) is the only code in this module that ever
//! removes an entry from `conns` or joins a REGISTERED connection's
//! reader/writer — for any reason. [`PipeServer::close`], a reader's own
//! natural-EOF signal, and a writer's own `WriteFile`-error signal all
//! route through [`request_teardown`], which enqueues at most once per
//! connection (see "Bounded reaper inbox" below); [`teardown_if_present`]
//! is the one function that does the real work, called exclusively from
//! [`reaper_loop`], processing messages strictly one at a time. The one
//! correct exception: `handle_new_connection`'s own partial-spawn-failure
//! unwind directly `join`s an aborted reader that was NEVER registered
//! into `conns` — the reaper has nothing to reap because that connection
//! never had a lifecycle for it to know about.
//!
//! Registration is ordered so that a client which connects and
//! disconnects instantly can never let a reader reach the reaper before
//! the entry exists to be found: a connection's reader/writer threads
//! spawn already blocked on a [`StartGate`] and do not touch the pipe
//! until AFTER the `ConnHandle` is in the map AND `Accepted` has been
//! RELIABLY queued (see below).
//!
//! # Reliable lifecycle delivery
//!
//! `Accepted`, `Sent`, `Closed`, and `AcceptError` must be delivered, not
//! merely attempted — a dropped `Accepted` lets `Bytes` arrive for a
//! connection the consumer was never told exists; a dropped `Sent`
//! violates the ADR's physical-write barrier; a dropped `Closed` leaves a
//! stream gap the consumer's `FrameSplitter` can never detect.
//! [`send_lifecycle_event`] retries against a full `events()` channel
//! indefinitely, with exactly ONE escape: [`ServerShared::dropping`], set
//! only by [`PipeServer::drop`] — once true, nothing could ever call
//! `events()` again (its `Receiver` lives inside the `PipeServer` being
//! dropped), so continuing would be pure busywork. `dropping` is set at
//! the very START of `drop`, before anything else, INCLUDING before the
//! accept-thread join: any thread (the accept thread's own `AcceptError`/
//! `Accepted` publishes included) can be inside this retry loop when
//! `Drop` runs, and joining it first would deadlock `Drop` behind the
//! very escape hatch meant to unblock it. Memory-bounded by construction
//! (exactly one event is ever "in hand" being retried per caller, never
//! accumulated); time-bounded otherwise ONLY by the CONSUMER's own
//! contract — it must keep draining `events()`, which the future
//! capsule's one ordered loop does by construction. That contract is this
//! mechanism's other half; this module cannot enforce it, only document
//! it.
//!
//! ONLY `Bytes` may still be abandoned — the transport is the read-ahead
//! producer and must not let a stalled consumer grow memory without
//! bound. [`deliver_bytes`] retries for up to [`BYTES_ABANDON_AFTER`]
//! against a full channel (or until its own slot is independently
//! cancelled); abandoning always forces this ONE connection closed with a
//! GUARANTEED `Closed` (through the same reliable path), never a silent
//! stream gap with nothing to mark it.
//!
//! # Bounded reaper inbox
//!
//! `reaper_tx` is a bounded channel — `max_instances +`
//! [`REAPER_INBOX_SLACK`] — rather than unbounded. What makes that bound
//! actually hold: every connection carries its own
//! `torn_down_requested: Arc<AtomicBool>`, and [`request_teardown`]
//! enqueues a `ReaperMsg` only on the `compare_exchange` that WINS
//! flipping it — an explicit `close`, the reader's own EOF signal, and
//! the writer's own error signal can all race for the same connection,
//! but at most one of them ever reaches the channel. The inbox can
//! therefore never hold more than one live `Torn` message per
//! currently-open connection (≤ `max_instances`) plus `Drop`'s own single
//! `Shutdown`.
//!
//! # Continuous name hold
//!
//! An instance is never actually closed while the server lives AND
//! intends to keep accepting. Once `DisconnectNamedPipe`'d, a torn-down
//! instance is RECYCLED — pushed onto `AcceptState::recycled` — rather
//! than dropped and later re-created. If `DisconnectNamedPipe` itself
//! fails, the instance is in an unknown state and unsafe to hand back
//! for a future `ConnectNamedPipe` — but it is deliberately RETAINED
//! anyway (`AcceptState::retained_dead`), never closed, for the rest of
//! the server's life. This looks wasteful — that instance's capacity is
//! gone for good — but the alternative is worse: creating a replacement
//! here would need to exceed `max_instances` while the failed instance
//! is still open (at `max_instances == 1` this is not merely awkward, it
//! is impossible — `CreateNamedPipeW` fails with `ERROR_PIPE_BUSY` every
//! time, since the OS still counts the open, merely-broken handle
//! against the cap), and closing the failed instance to make room is
//! exactly the name-hold lapse this design exists to prevent. The
//! invariant this module promises is that the NAME stays held, not that
//! every instance stays usable — a held name and a dead handle both
//! satisfy it; a closed handle does not. `recycle_instance` remains the
//! ONLY way an instance is ever set aside short of teardown, and a
//! `DisconnectNamedPipe` failure there also terminalizes the accept loop
//! via [`terminalize_accept_loop`] — see that function's doc for why
//! stopping (rather than merely losing one slot's worth of capacity and
//! continuing) is the safer default.
//!
//! # One registry, one closer, one live-use guard (ADR 0041 step 6 U1b,
//! Codex rounds 3-4)
//!
//! `recycle_instance` never itself calls `CloseHandle` — not on
//! recycle, not on retain-dead, not once teardown is under way. EVERY
//! instance handle this module ever creates is created AND registered
//! ATOMICALLY, in [`InstanceRegistry::create_and_register`] (round-4:
//! creation and registration share ONE lock section with
//! [`InstanceRegistry::close_all`], so a handle can never come into
//! existence — or be recreated — in a window `close_all` has already
//! passed), and stays registered — through any number of recycle/reuse
//! cycles, through becoming a live connection, through sitting in
//! `retained_dead` — until `close_all` (called exactly once, from
//! [`PipeServer::disconnect_listener`]) finds and closes it. Because no
//! OTHER code path ever individually removes an id from the registry,
//! there is no "removed here, but the remover assumed someone else would
//! close it" gap: whichever of this module's several buckets (the accept
//! loop's own pending instance, `recycled`, `retained_dead`, or a live
//! `ConnHandle` in `conns`) an id's instance currently sits in, at the
//! instant `close_all` runs it is found and closed — independent of
//! `conns`' own, unrelated Rust-level bookkeeping timing.
//!
//! That closes the NAME-leak races (round 3). It does NOT, by itself,
//! make USING a handle safe: `close_all` can run at any instant, and
//! Windows can and does reuse a closed handle's numeric value for an
//! unrelated object soon after — so a stale raw `HANDLE` passed to
//! `CancelIoEx`/`DisconnectNamedPipe`/a fresh `ConnectNamedPipe`/
//! `ReadFile`/`WriteFile` SUBMISSION is a genuine use-after-close, not
//! merely a harmless failed call (round 4). The fix: [`InstanceRegistry`]
//! is a `RwLock`, and [`InstanceRegistry::live`] hands back a
//! [`LiveHandle`] that holds the READ side for exactly the span of ONE
//! such call — `close_all` needs the WRITE side, which cannot be granted
//! while any `LiveHandle` (for any id) is outstanding, so a handle is
//! NEVER closed while it is mid-use, and never used once closed. This is
//! deliberately narrow: a `LiveHandle` is NEVER held across
//! [`wait_overlapped`]'s own blocking wait (only reads/writes that are
//! themselves fast, non-blocking Win32 calls run under it), so
//! `disconnect_listener`'s "never blocks" contract survives — see
//! `LiveHandle`'s own doc.
//!
//! Every remaining `ServerShared::dropping` check in
//! `recycle_instance`/`accept_loop` is now a pure CLEANLINESS
//! optimization (skip a pointless `DisconnectNamedPipe`/`ConnectNamedPipe`
//! follow-up and a possible spurious `AcceptError` once teardown is under
//! way) — never a safety decision; a stale read there can at worst waste
//! one OS call, never leak, double-close, or use-after-close a handle.
//!
//! This is also what makes the pipe NAME actually disappear promptly on
//! teardown (ADR 0041 Lifecycle: "the pipe NAME disappears before any
//! blocking join") instead of only when the whole `ServerShared` finally
//! drops — `close_all` never blocks on anything but its own lock (every
//! entry is one `CloseHandle`, not a join), so `disconnect_listener` can
//! call it synchronously and return with every instance already gone.
//!
//! # Pending-I/O completion proof
//!
//! `CancelIoEx` only REQUESTS cancellation, and an external `CloseHandle`
//! only forces a pending op to complete or error — neither WAITS for
//! that to actually happen. Microsoft's own overlapped-I/O rules require
//! the `OVERLAPPED` structure, its event, and any I/O buffer to remain
//! valid until the kernel is DONE with a GENUINELY submitted (i.e.
//! `ERROR_IO_PENDING`) op — an error return from `GetOverlappedResult`
//! alone is not that proof, since an external close can race the call
//! itself. [`wait_overlapped`] additionally waits on the OVERLAPPED's own
//! event in that case (bounded, never Win32 `INFINITE`); a
//! SYNCHRONOUSLY-completed op (the OS call itself returned success) needs
//! no such wait — there is nothing left pending for the kernel to still
//! be doing, so a later `GetOverlappedResult` failure there just means
//! the handle is no longer valid for querying the byte count, not that
//! memory safety is at risk. If a GENUINELY pending op's completion is
//! still not observed within the bound, this module can never safely
//! return normally — [`CompletionUnproven`] is the marker every caller
//! must react to by leaking (never freeing) whatever storage it handed
//! to the OS, or, where that storage is caller-owned and cannot be
//! leaked on the caller's behalf ([`PipeClient::write_all`]/
//! [`PipeClient::read`]), aborting the process. See that marker's own
//! doc.
//!
//! # Byte-bounded both directions
//!
//! Outbound: [`OutboundBudget`] reserves BYTES (not items) per connection,
//! including the in-flight item, released only once the write physically
//! completes. Inbound: `events_tx` is a bounded channel and `Bytes`
//! delivery is bounded as described above.
//!
//! # Visibility
//!
//! Every type below is `pub`, not `pub(crate)` — `tests/pipe_win.rs` is a
//! separate integration-test crate, and an integration test can only ever
//! reach a library's `pub` items.

#![cfg(windows)]

use std::cell::UnsafeCell;
use std::collections::{HashMap, VecDeque};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, RwLock, RwLockReadGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED,
    GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

/// Bound on one outstanding overlapped `ReadFile` (ADR 0041: "the transport
/// just must not read unboundedly ahead").
const READ_BUF_LEN: usize = 65_536;

/// Per-connection outbound byte budget: enqueued-but-not-yet-physically-
/// written bytes, INCLUDING the writer's in-flight item, may never exceed
/// this. Sized to the same order of magnitude as the ADR's own "4 MiB
/// per-watcher queue" figure — not a literal citation of it (that number
/// bounds a different queue, the future capsule's checkpoint transfer),
/// just a consistent order of magnitude for this transport's own ceiling.
const OUTBOUND_BUDGET_BYTES: usize = 4 * 1024 * 1024;

/// The `events()` channel's item capacity: sized so a run of maximum-size
/// `Bytes` deliveries caps buffered inbound at roughly the same order of
/// magnitude as [`OUTBOUND_BUDGET_BYTES`].
const EVENTS_CHANNEL_CAP: usize = OUTBOUND_BUDGET_BYTES / READ_BUF_LEN;

/// How long a stalled delivery (lifecycle retry, or one `Bytes` attempt)
/// sleeps between retries against a full `events` channel.
const EVENTS_RETRY_INTERVAL: Duration = Duration::from_millis(20);

/// How long [`deliver_bytes`] may retry a single `Bytes` chunk against a
/// full `events` channel before abandoning it and force-closing the
/// connection with a guaranteed `Closed`. Generous relative to
/// [`EVENTS_RETRY_INTERVAL`] — this is "the consumer has genuinely
/// stalled," not routine backpressure.
const BYTES_ABANDON_AFTER: Duration = Duration::from_secs(5);

/// Extra capacity on the bounded reaper inbox beyond `max_instances` — a
/// connection's own at-most-once teardown flag already caps live `Torn`
/// messages at one per open connection, so the only other traffic this
/// inbox ever carries is `Drop`'s own single `Shutdown` message.
const REAPER_INBOX_SLACK: usize = 1;

/// `\\.\pipe\sot-voyage-<id>`, UTF-16, NUL-terminated. Panics never: `id`
/// is validated by [`validate_voyage_id`] at every call site before this
/// runs.
fn pipe_name_wide(voyage_id: &str) -> Vec<u16> {
    wide_null(&format!(r"\\.\pipe\sot-voyage-{voyage_id}"))
}

/// `\\.\pipe\sot-supervisor-<h>`, UTF-16, NUL-terminated (ADR 0041
/// Lifecycle "Name and identity") — the supervisor lane's OWN pipe, a
/// second, independently-named instance of this same server/client
/// machinery, never a voyage pipe under another name. `h` is the caller's
/// own stable hash of the canonicalized state-dir path; unlike
/// [`pipe_name_wide`]'s `voyage_id`, this function neither derives nor
/// validates it — the supervisor lane has no UUID-shape requirement to
/// enforce.
fn supervisor_pipe_name_wide(h: &str) -> Vec<u16> {
    wide_null(&format!(r"\\.\pipe\sot-supervisor-{h}"))
}

/// NUL-terminated UTF-16 for an arbitrary Rust string. A small, deliberate
/// duplicate of `conpty.rs`'s and `fsutil.rs`'s own private copies of this
/// exact helper — sharing a three-line leaf helper would add machinery
/// without value under this crate's existing rule.
fn wide_null(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// The voyage id is validated as a canonical RFC 4122 UUID — lowercase,
/// hyphenated, the exact form [`uuid::Uuid`]'s own `Display` produces —
/// before it is ever interpolated into a pipe name. Delegates to
/// `pointer::canonical_voyage_id` (ADR 0041 U0 round-1 minor finding 9):
/// one canonical-UUID check for this crate, not two that can drift, as
/// this one already had from `drawer.voyage`'s own (stricter) validation.
/// Anything that fails to parse at all (path-traversal shapes, wrong
/// length, non-hex bytes) is rejected the same way.
fn validate_voyage_id(voyage_id: &str) -> Result<(), PipeError> {
    if crate::pointer::canonical_voyage_id(voyage_id).is_some() {
        Ok(())
    } else {
        Err(PipeError::InvalidVoyageId(voyage_id.to_string()))
    }
}

/// Identifies one accepted connection for the lifetime of a [`PipeServer`].
/// Assigned sequentially; never reused.
pub type ConnId = u64;

/// An opaque, caller-assigned correlation tag for one [`PipeServer::send`]
/// call, echoed back on [`TransportEvent::Sent`] when the OS reports that
/// send's `WriteFile` has PHYSICALLY completed.
pub type SendMarker = u64;

/// Why a connection ended, reported once per connection on
/// [`TransportEvent::Closed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosedReason {
    /// The peer disconnected (or this side observed a broken/unconnected
    /// pipe) — detected by the connection's own reader loop.
    Eof,
    /// [`PipeServer::close`] tore this connection down.
    Closed,
    /// An I/O error other than a recognized disconnect ended a
    /// connection's reader or writer loop, or its `Bytes` delivery was
    /// abandoned — always paired with this guaranteed notification,
    /// never a silent stream gap.
    Error(String),
}

/// This transport's event surface to its consumer. Delivered over
/// [`PipeServer::events`] in the order this module observed them; the
/// consumer feeds `Bytes` payloads to its own [`crate::wire::FrameSplitter`]
/// per connection.
#[derive(Debug)]
pub enum TransportEvent {
    /// A new connection accepted; `send`/`close` may now target it.
    Accepted(ConnId),
    /// Raw bytes read from a connection, in the order read. Never empty.
    Bytes(ConnId, Vec<u8>),
    /// The `WriteFile` for a marker-tagged [`PipeServer::send`] call has
    /// physically completed.
    Sent(ConnId, SendMarker),
    /// The connection ended; no further events for this `ConnId` follow.
    Closed(ConnId, ClosedReason),
    /// The accept loop hit a persistent, unrecoverable resource failure
    /// and has stopped accepting new connections FOR GOOD — existing
    /// connections are unaffected.
    AcceptError(String),
}

/// Errors this transport's own API surface can report synchronously, at
/// the call site. Background-thread failures surface as
/// [`TransportEvent::Closed`]/[`TransportEvent::AcceptError`] instead.
#[derive(Debug, thiserror::Error)]
pub enum PipeError {
    #[error("invalid voyage id {0:?}: must be the canonical lowercase-hyphenated form of an RFC 4122 UUID")]
    InvalidVoyageId(String),
    #[error("max_instances must be between 1 and 255 (CreateNamedPipeW's own documented range)")]
    InvalidMaxInstances,
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
    #[error("payload of {0} bytes exceeds what a single Win32 write/read call can represent")]
    PayloadTooLarge(usize),
    #[error("operation cancelled")]
    Cancelled,
    /// A second same-direction `PipeClient` call (e.g. two concurrent
    /// `read`s) was rejected before it ever touched the OS or the shared
    /// `OVERLAPPED` — misuse, not a race this module resolves for the
    /// caller.
    #[error("another operation is already pending on this client's same direction")]
    ConcurrentSubmit,
    /// U1a: `connect_voyage_pipe`'s own SID authentication (ADR 0041
    /// Lifecycle "The challenge", steps 1-3 via
    /// `challenge::authenticate_server` — NOT the full five-step
    /// `challenge()`, see that function's own doc) answered with a
    /// WELL-FORMED WRONG proof — a different token-user SID behind the
    /// pipe. A loud, typed failure: never retried as if the peer might
    /// still turn out legitimate.
    #[error("connect_voyage_pipe: the peer failed SID authentication (a different account's process is behind this pipe)")]
    Foreign,
    /// U1a: SID authentication could not be completed at all — an OS-call
    /// failure (`GetNamedPipeServerProcessId`, `OpenProcess`,
    /// `OpenProcessToken`, `GetTokenInformation`, `GetProcessTimes`).
    /// Never silently treated as either authenticated or foreign (ADR
    /// 0041: "a failure... is PENDING, never READY and never ADOPTED").
    #[error("connect_voyage_pipe: SID authentication could not be completed (peer identity undetermined)")]
    Undetermined,
}

/// A raw Windows `HANDLE`, asserted `Send` AND `Sync`. `Send`: exactly one
/// owner ever calls `CloseHandle` on it, only after every thread using a
/// copy has stopped. `Sync`: the wrapped value is never dereferenced as a
/// pointer — it is an opaque OS handle, passed only to `windows-sys`
/// calls — so reading a `&SendableHandle` from multiple threads at once
/// is just reading a plain integer.
#[derive(Clone, Copy)]
struct SendableHandle(HANDLE);
unsafe impl Send for SendableHandle {}
unsafe impl Sync for SendableHandle {}

/// One I/O slot's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Idle,
    Pending,
    /// Terminal: latched by [`IoSlot::cancel`], never leaves this state.
    Closing,
}

/// A synchronized, reusable overlapped-I/O slot: one `Mutex<SlotState>`
/// plus one address-stable, manual-reset-event-backed `OVERLAPPED`. Used
/// for the accept loop's `ConnectNamedPipe`, every connection's read and
/// write directions (server and client alike). See the module doc's "I/O
/// slot" section for the full soundness argument.
struct IoSlot {
    state: Mutex<SlotState>,
    ov: UnsafeCell<OVERLAPPED>,
    /// `true` iff the CURRENT (or most recently settled) submission on
    /// this slot genuinely went `ERROR_IO_PENDING` at the OS level —
    /// DISTINCT from `SlotState::Pending`, which is ALSO set for a
    /// synchronously-completed op still awaiting `GetOverlappedResult`
    /// collection (Codex round-5 finding: the two are not the same
    /// observable state — a test polling `SlotState::Pending` alone can
    /// pass during that synchronous-completion window without ever
    /// proving a genuine kernel-level pending op existed). Reset to
    /// `false` at the START of every submission (before its outcome is
    /// known) and set `true` only in the actual `ERROR_IO_PENDING`
    /// branch, so it always reflects the CURRENT attempt, never a stale
    /// one.
    genuinely_async: AtomicBool,
}
// SAFETY: `ov`'s contents are only ever touched (reset, issued, or read
// via `GetOverlappedResult`) by the ONE thread that got past
// `submit_and_wait`'s `Pending` check for this slot — a second thread
// racing that check is REJECTED before touching `ov` at all. A cancelling
// thread only ever reads the slot's STABLE ADDRESS to hand to
// `CancelIoEx`, an OS-level operation on that address, never a Rust-level
// read of the struct's bytes. `state`'s `Mutex` is what serializes
// submission against both cancellation and a second submission attempt.
unsafe impl Send for IoSlot {}
unsafe impl Sync for IoSlot {}

fn aborted_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(ERROR_OPERATION_ABORTED as i32)
}

/// Marker error: `submit_and_wait` rejected a call because another
/// submission is already `Pending` on this exact direction. Server-
/// internal code never triggers this — each slot has exactly one driving
/// thread by construction — it exists so `PipeClient`'s genuinely `Sync`,
/// `&self`-based `read`/`write_all` reject concurrent same-direction
/// misuse with a distinct `Result` instead of corrupting one shared
/// `OVERLAPPED`.
#[derive(Debug)]
struct ConcurrentSubmitMarker;
impl std::fmt::Display for ConcurrentSubmitMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another submission is already pending on this IoSlot direction")
    }
}
impl std::error::Error for ConcurrentSubmitMarker {}

fn is_concurrent_submit(e: &std::io::Error) -> bool {
    e.get_ref()
        .is_some_and(|inner| inner.is::<ConcurrentSubmitMarker>())
}

/// Marker error (ADR 0041 step 6 U1b, Codex round-4 "pending-I/O
/// completion proof"): a GENUINELY PENDING overlapped op's completion
/// could not be affirmatively observed within
/// [`OVERLAPPED_COMPLETION_PROOF_TIMEOUT`]. `CancelIoEx` only REQUESTS
/// cancellation; Microsoft's synchronous/asynchronous I/O rules require
/// the `OVERLAPPED`, its event, and any I/O buffer to remain valid until
/// the kernel is DONE with them — an error return alone is not that
/// proof. This module cannot safely return normally in that case:
///
/// - Every SERVER-side caller ([`accept_loop`], [`reader_loop`],
///   [`writer_loop`]) owns the buffer/slot it handed to the OS outright
///   and MUST permanently leak it — `std::mem::forget` an extra
///   `Arc<IoSlot>` clone (so the underlying allocation, including the
///   `OVERLAPPED` and its event, is never freed) and `std::mem::forget`
///   the I/O buffer — then treat the connection (or the accept loop
///   itself) as unrecoverably gone, reported loudly (`eprintln!`), never
///   silently. This matches `join_within`'s own abandonment philosophy:
///   never silently unsafe, but also never a process-wide abort for a
///   condition scoped to one connection.
/// - [`PipeClient::write_all`]/[`PipeClient::read`] receive a BORROWED
///   buffer from their own caller — this module cannot leak memory it
///   does not own, and the caller may free or reuse that memory the
///   instant this call returns. The only safe response left is to abort
///   the whole process (`std::process::abort()`) rather than risk the
///   kernel writing into (or reading stale bytes from) memory that has
///   already been freed or reused.
#[derive(Debug)]
struct CompletionUnproven;
impl std::fmt::Display for CompletionUnproven {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "a genuinely pending overlapped op's completion could not be affirmatively \
             observed; its storage must be leaked, never reused",
        )
    }
}
impl std::error::Error for CompletionUnproven {}

fn is_completion_unproven(e: &std::io::Error) -> bool {
    e.get_ref()
        .is_some_and(|inner| inner.is::<CompletionUnproven>())
}

/// The Win32 error codes that mean "the pipe is disconnected, broken, or
/// being closed" — the family both a live connection's read/write AND an
/// in-flight `ConnectNamedPipe` can report when a peer vanishes.
/// [`classify_terminal_error`] treats these (plus this module's own
/// cancellation code) as an ordinary, expected `Eof` for a LIVE
/// connection's reader/writer; the accept loop's own connect-result match
/// treats them as "a client connected and vanished before the completion
/// was fully processed" and registers that connection anyway rather than
/// silently discarding it — the SAME family, one call earlier.
/// `ERROR_BROKEN_PIPE` and `ERROR_PIPE_NOT_CONNECTED` are Microsoft's
/// documented disconnect-family codes for named-pipe I/O; `ERROR_NO_DATA`
/// ("The pipe is being closed") is the code Windows documents pipe I/O
/// (including a `ConnectNamedPipe` racing a local close) returning when
/// the local end is torn down mid-operation — exactly the instant-close
/// race this module must not misclassify as a fatal accept failure. Any
/// OTHER connect error is a genuine anomaly and is NOT in this list —
/// see the accept loop's own match for why that distinction matters.
fn is_disconnect_family(code: i32) -> bool {
    code == ERROR_NO_DATA as i32
        || code == ERROR_BROKEN_PIPE as i32
        || code == ERROR_PIPE_NOT_CONNECTED as i32
}

impl IoSlot {
    fn new() -> std::io::Result<Self> {
        // Manual-reset (bManualReset = TRUE): auto-reset events can hang
        // `GetOverlappedResult(..., TRUE)` per Microsoft's overlapped-I/O
        // documentation; every reuse below explicitly `ResetEvent`s.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = event;
        Ok(Self {
            state: Mutex::new(SlotState::Idle),
            ov: UnsafeCell::new(ov),
            genuinely_async: AtomicBool::new(false),
        })
    }

    fn ptr(&self) -> *mut OVERLAPPED {
        self.ov.get()
    }

    /// Zero every field except `hEvent`, then explicitly `ResetEvent` it —
    /// zeroing our copy of the struct does not touch the EVENT OBJECT's
    /// own kernel-side signaled state.
    fn reset(&self) {
        unsafe {
            let event = (*self.ov.get()).hEvent;
            ResetEvent(event);
            *self.ov.get() = std::mem::zeroed();
            (*self.ov.get()).hEvent = event;
        }
    }

    /// Attempt to submit one overlapped op (`issue`, returning the raw
    /// `BOOL` of `ReadFile`/`WriteFile`) against a handle THIS CALLER
    /// already owns outright, and block for its definitive result.
    /// CLIENT-facing only — see the module doc's "I/O slot" section for
    /// why server-side instances go through
    /// [`submit_and_wait_registered`](Self::submit_and_wait_registered)
    /// instead. Refuses WITHOUT ever calling `issue` if this slot is
    /// `Closing` (`Err(aborted)`) or already `Pending`
    /// (`Err(ConcurrentSubmitMarker)`) — both checked under the same lock
    /// `cancel` uses, so neither race is possible. `synchronous_ok` names
    /// a synchronous-failure `GetLastError` code that actually means
    /// success; pass `|_| false` for plain reads/writes.
    fn submit_and_wait(
        &self,
        handle: HANDLE,
        issue: impl FnOnce(*mut OVERLAPPED) -> i32,
        synchronous_ok: impl Fn(i32) -> bool,
    ) -> std::io::Result<u32> {
        let mut genuinely_pending = false;
        {
            let mut st = self.state.lock().unwrap();
            if *st == SlotState::Closing {
                return Err(aborted_error());
            }
            if *st == SlotState::Pending {
                return Err(std::io::Error::other(ConcurrentSubmitMarker));
            }
            self.genuinely_async.store(false, Ordering::Release);
            // Reset AND issue while holding the lock: a concurrent
            // `cancel` cannot observe a half-reset `OVERLAPPED`, and
            // cannot call `CancelIoEx` in the gap between this reset and
            // the `issue` call below.
            self.reset();
            let ok = issue(self.ptr());
            let sync_err = (ok == 0).then(std::io::Error::last_os_error);
            if ok == 0 {
                let err = sync_err.unwrap();
                let code = err.raw_os_error().unwrap_or(0);
                if code == ERROR_IO_PENDING as i32 {
                    *st = SlotState::Pending;
                    genuinely_pending = true;
                    self.genuinely_async.store(true, Ordering::Release);
                } else if synchronous_ok(code) {
                    return Ok(0);
                } else {
                    return Err(err);
                }
            } else {
                *st = SlotState::Pending;
            }
        } // lock released BEFORE the (possibly long) wait below.
        let result = wait_overlapped(handle, self.ptr(), genuinely_pending);
        if let Err(e) = &result {
            if is_completion_unproven(e) {
                // Never touch this slot's state again -- see
                // `CompletionUnproven`'s own doc. The caller MUST leak
                // whatever storage it owns.
                return result;
            }
        }
        self.genuinely_async.store(false, Ordering::Release);
        let mut st = self.state.lock().unwrap();
        if *st != SlotState::Closing {
            *st = SlotState::Idle;
        }
        result
    }

    /// Same contract as [`submit_and_wait`](Self::submit_and_wait), for a
    /// REGISTERED server-side instance (ADR 0041 step 6 U1b, Codex
    /// round-4): `issue` receives the raw handle only once a
    /// [`LiveHandle`] proves `id` is still registered in `registry` —
    /// held for exactly the duration of the `issue` call itself, never
    /// across the subsequent blocking wait (see [`LiveHandle`]'s own
    /// doc). If `id` is already gone (closed by
    /// [`InstanceRegistry::close_all`]), this behaves exactly like this
    /// module's own cancellation: `Err(aborted_error())`, without ever
    /// calling `issue` — a torn-down instance is, from every caller's
    /// perspective, indistinguishable from one THIS module cancelled.
    fn submit_and_wait_registered(
        &self,
        registry: &InstanceRegistry,
        id: u64,
        issue: impl FnOnce(HANDLE, *mut OVERLAPPED) -> i32,
        synchronous_ok: impl Fn(i32) -> bool,
    ) -> std::io::Result<u32> {
        let mut genuinely_pending = false;
        let handle = {
            let mut st = self.state.lock().unwrap();
            if *st == SlotState::Closing {
                return Err(aborted_error());
            }
            if *st == SlotState::Pending {
                return Err(std::io::Error::other(ConcurrentSubmitMarker));
            }
            let Some(live) = registry.live(id) else {
                return Err(aborted_error());
            };
            let handle = live.get();
            self.genuinely_async.store(false, Ordering::Release);
            self.reset();
            let ok = issue(handle, self.ptr());
            // Codex round-5 finding 1: capture `GetLastError` IMMEDIATELY
            // after `issue` returns, BEFORE `drop(live)` -- dropping the
            // `LiveHandle` releases the registry's `RwLock` read side,
            // and this crate never assumes an intervening call (however
            // unlikely to actually touch it) leaves the thread's last-
            // error value alone. A real `ERROR_IO_PENDING` misread as an
            // ordinary failure here would free an `OVERLAPPED`/buffer
            // the kernel still owns.
            let sync_err = (ok == 0).then(std::io::Error::last_os_error);
            drop(live);
            if ok == 0 {
                let err = sync_err.unwrap();
                let code = err.raw_os_error().unwrap_or(0);
                if code == ERROR_IO_PENDING as i32 {
                    *st = SlotState::Pending;
                    genuinely_pending = true;
                    self.genuinely_async.store(true, Ordering::Release);
                } else if synchronous_ok(code) {
                    return Ok(0);
                } else {
                    return Err(err);
                }
            } else {
                *st = SlotState::Pending;
            }
            handle
        };
        let result = wait_overlapped(handle, self.ptr(), genuinely_pending);
        if let Err(e) = &result {
            if is_completion_unproven(e) {
                // Never touch this slot's state again -- see
                // `CompletionUnproven`'s own doc. The caller MUST leak
                // whatever storage it owns.
                return result;
            }
        }
        self.genuinely_async.store(false, Ordering::Release);
        let mut st = self.state.lock().unwrap();
        if *st != SlotState::Closing {
            *st = SlotState::Idle;
        }
        result
    }

    /// Cancel this slot: if an operation is genuinely `Pending`, call
    /// `CancelIoEx`; either way, latch `Closing` so every FUTURE
    /// `submit_and_wait` call refuses before ever touching the OS again.
    /// Idempotent — safe to call more than once, from any thread.
    /// CLIENT-facing only — see
    /// [`cancel_registered`](Self::cancel_registered) for server-side
    /// instances.
    fn cancel(&self, handle: HANDLE) {
        let mut st = self.state.lock().unwrap();
        if *st == SlotState::Pending {
            unsafe { CancelIoEx(handle, self.ptr()) };
        }
        *st = SlotState::Closing;
    }

    /// Same contract as [`cancel`](Self::cancel), for a REGISTERED
    /// server-side instance: `CancelIoEx` is issued only while a
    /// [`LiveHandle`] proves `id` is still registered. If `id` is
    /// already gone, there is nothing to cancel against — its
    /// `CloseHandle` already forced the pending op to complete/error —
    /// so this just latches `Closing`, exactly like the plain `cancel`
    /// always does.
    ///
    /// Returns whether the op THIS CALL cancelled (if any) was
    /// GENUINELY asynchronously pending, decided under the SAME lock
    /// acquisition that performs the cancellation (Codex round-5 fix
    /// 2b/2c) — this is the TOCTOU-free proof a caller needing to KNOW
    /// (not merely poll-and-hope) must use: a separate prior check of
    /// [`is_genuinely_pending`](Self::is_genuinely_pending) can always
    /// go stale between the check and this call; this return value
    /// cannot, because both the read and the cancellation happen inside
    /// one critical section.
    fn cancel_registered(&self, registry: &InstanceRegistry, id: u64) -> bool {
        let mut st = self.state.lock().unwrap();
        let was_genuinely_pending =
            *st == SlotState::Pending && self.genuinely_async.load(Ordering::Acquire);
        if *st == SlotState::Pending {
            if let Some(live) = registry.live(id) {
                unsafe { CancelIoEx(live.get(), self.ptr()) };
            }
        }
        *st = SlotState::Closing;
        was_genuinely_pending
    }

    fn is_closing(&self) -> bool {
        *self.state.lock().unwrap() == SlotState::Closing
    }

    /// `true` iff the CURRENT submission genuinely went `ERROR_IO_PENDING`
    /// at the OS level right now — see [`IoSlot::genuinely_async`]'s own
    /// doc for why this is NOT the same thing as `SlotState::Pending`.
    /// A caller that needs a race-free ANSWER (not merely a heuristic
    /// "is it probably time to act") must use
    /// [`cancel_registered`](Self::cancel_registered)'s own return value
    /// instead, which decides this under the same lock that performs
    /// the cancellation. This accessor is a best-effort PRE-check only
    /// — e.g. "has the writer plausibly reached a pending write yet, so
    /// it is worth proceeding to teardown" — never itself the proof.
    fn is_genuinely_pending(&self) -> bool {
        self.genuinely_async.load(Ordering::Acquire)
    }
}

impl Drop for IoSlot {
    fn drop(&mut self) {
        unsafe { CloseHandle((*self.ov.get()).hEvent) };
    }
}

/// Bound on the affirmative wait for a GENUINELY PENDING overlapped op's
/// OWN completion signal (Codex round-4, "pending-I/O completion
/// proof"): Microsoft documents `CancelIoEx` as REQUESTING cancellation,
/// never waiting for it, and `GetOverlappedResult`'s error return alone
/// is not itself proof the kernel is done with this `OVERLAPPED` — if
/// `handle` was closed by another thread (e.g. `InstanceRegistry::close_all`
/// racing this exact call) in the gap between `IoSlot::submit_and_wait`
/// releasing its lock and `wait_overlapped` ever calling
/// `GetOverlappedResult`, that call can fail IMMEDIATELY against the
/// now-invalid handle without ever having waited on anything. The
/// completion EVENT's own lifetime is independent of `handle` (this
/// `IoSlot` owns the event; see `IoSlot::new`/`Drop`), so on error
/// [`wait_overlapped`] additionally waits on the event directly — but
/// ONLY when the op was genuinely submitted asynchronously
/// (`ERROR_IO_PENDING`); a synchronously-completed op has nothing left
/// pending and may never signal the event at all (Microsoft's named-pipe
/// overlapped example notes exactly this), so waiting on it
/// unconditionally would manufacture a FALSE timeout. Bounded, never
/// Win32 `INFINITE` — this crate's own rule (see
/// `fsutil::duration_to_wait_ms`'s doc); in the ordinary case (the error
/// came from a properly-waited cancellation) the event is ALREADY
/// signalled, so this returns effectively instantly.
const OVERLAPPED_COMPLETION_PROOF_TIMEOUT: Duration = Duration::from_secs(5);

/// Block for the definitive result of an overlapped op already submitted
/// on `handle`/`ov`. `genuinely_pending` MUST be `true` iff `issue`
/// itself returned `ERROR_IO_PENDING` (an actual asynchronous
/// submission) rather than a synchronous result — see
/// [`OVERLAPPED_COMPLETION_PROOF_TIMEOUT`]'s own doc for why that
/// distinction is load-bearing. On a GENUINELY pending op whose
/// completion cannot be affirmatively observed within the bound, this
/// returns [`CompletionUnproven`] rather than an ordinary error — every
/// caller MUST react to that marker per its own doc, never treat it as a
/// normal I/O failure.
fn wait_overlapped(
    handle: HANDLE,
    ov: *const OVERLAPPED,
    genuinely_pending: bool,
) -> std::io::Result<u32> {
    let mut transferred: u32 = 0;
    let ok = unsafe { GetOverlappedResult(handle, ov, &mut transferred, 1) };
    if ok == 0 {
        let err = std::io::Error::last_os_error();
        if !genuinely_pending {
            // This op completed SYNCHRONOUSLY (`issue` itself returned
            // success) -- there is nothing left for the kernel to still
            // be doing with this OVERLAPPED. A failure here means only
            // that `handle` is no longer valid for QUERYING the result
            // (e.g. an external close raced this exact call), never that
            // a pending kernel operation might still touch this memory.
            // Nothing to prove; return the plain error.
            return Err(err);
        }
        let event = unsafe { (*ov).hEvent };
        let ms = crate::fsutil::duration_to_wait_ms(OVERLAPPED_COMPLETION_PROOF_TIMEOUT);
        return match unsafe { WaitForSingleObject(event, ms) } {
            WAIT_OBJECT_0 => Err(err),
            WAIT_TIMEOUT => {
                eprintln!(
                    "sot-pipe: a genuinely pending overlapped op's completion was not \
                     affirmatively observed within {OVERLAPPED_COMPLETION_PROOF_TIMEOUT:?}; its \
                     OVERLAPPED/event/buffer must be leaked, never reused (see \
                     CompletionUnproven's own doc)"
                );
                Err(std::io::Error::other(CompletionUnproven))
            }
            WAIT_FAILED => {
                eprintln!(
                    "sot-pipe: WaitForSingleObject on the overlapped completion event failed \
                     ({:?}) while establishing the completion proof; leaking rather than risking \
                     a use-after-free",
                    std::io::Error::last_os_error()
                );
                Err(std::io::Error::other(CompletionUnproven))
            }
            other => {
                eprintln!(
                    "sot-pipe: WaitForSingleObject on the overlapped completion event returned \
                     an unexpected result ({other:#x}) while establishing the completion proof; \
                     leaking rather than risking a use-after-free"
                );
                Err(std::io::Error::other(CompletionUnproven))
            }
        };
    }
    Ok(transferred)
}

/// Create one instance of the voyage's named pipe. `first` must be `true`
/// for EXACTLY the very first instance ever created for this pipe name
/// (see [`PipeServer::bind`]'s squat check); every later instance is
/// created with `first = false`, or — per the continuous-name-hold design
/// — RECYCLED rather than freshly created at all. A fresh owner-only
/// descriptor is built per call: `CreateNamedPipeW` copies what it needs
/// from it at creation time.
fn create_pipe_instance(
    name: &[u16],
    first: bool,
    max_instances: u32,
) -> std::io::Result<OwnedHandle> {
    let descriptor = crate::fsutil::owner_protected_pipe_descriptor()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let sa = windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_ptr(),
        bInheritHandle: 0,
    };
    let h = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS | PIPE_WAIT,
            max_instances,
            READ_BUF_LEN as u32,
            READ_BUF_LEN as u32,
            0,
            &sa,
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(h as RawHandle) })
}

/// Per-connection outbound byte accounting: reserved eagerly by
/// [`PipeServer::send`] before an item is even queued, released only once
/// the writer's `submit_and_wait` for that item RETURNS (success or
/// failure) — the in-flight item stays counted the whole time. The cap is
/// always [`OUTBOUND_BUDGET_BYTES`] — not configurable, so no field for
/// it.
struct OutboundBudget {
    used: Mutex<usize>,
}

impl OutboundBudget {
    fn new() -> Self {
        Self {
            used: Mutex::new(0),
        }
    }

    fn try_reserve(&self, n: usize) -> bool {
        let mut used = self.used.lock().unwrap();
        if *used + n > OUTBOUND_BUDGET_BYTES {
            return false;
        }
        *used += n;
        true
    }

    fn release(&self, n: usize) {
        let mut used = self.used.lock().unwrap();
        *used = used.saturating_sub(n);
    }
}

/// One queued outbound send: raw bytes, plus an optional marker to echo
/// back on physical write completion.
struct WriteCmd {
    bytes: Vec<u8>,
    marker: Option<SendMarker>,
}

/// A connection's reader/writer threads block here until released — the
/// gate is opened only after the `ConnHandle` is in the map and `Accepted`
/// has been RELIABLY queued, or `abort`ed if the connection could not be
/// fully set up, in which case the gated thread returns immediately,
/// having never touched the pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateSignal {
    Wait,
    Start,
    Abort,
}

struct StartGate {
    state: Mutex<GateSignal>,
    cv: Condvar,
}

impl StartGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(GateSignal::Wait),
            cv: Condvar::new(),
        })
    }

    /// Blocks until `open` or `abort`; returns `true` to proceed, `false`
    /// to return immediately without ever touching the pipe.
    fn wait_for_start(&self) -> bool {
        let mut st = self.state.lock().unwrap();
        while *st == GateSignal::Wait {
            st = self.cv.wait(st).unwrap();
        }
        *st == GateSignal::Start
    }

    fn open(&self) {
        *self.state.lock().unwrap() = GateSignal::Start;
        self.cv.notify_all();
    }

    fn abort(&self) {
        *self.state.lock().unwrap() = GateSignal::Abort;
        self.cv.notify_all();
    }
}

/// A registry-verified, temporarily-live view of one instance's raw
/// `HANDLE` — the ONLY way any code in this module may pass a raw
/// SERVER-side instance handle to a Win32 call that submits new work
/// against it (`ConnectNamedPipe`/`ReadFile`/`WriteFile`) or cancels/
/// disconnects it (`CancelIoEx`/`DisconnectNamedPipe`).
///
/// INVARIANT: a handle is DEREFERENCED (passed to any such Win32 call)
/// only while THIS guard proves the registry still considers it live.
/// The guard holds [`InstanceRegistry`]'s `RwLock` READ side for its
/// whole lifetime, and [`InstanceRegistry::close_all`] cannot acquire
/// the WRITE side — and therefore cannot run `CloseHandle` on ANY
/// instance — while even ONE `LiveHandle` (for any id) is outstanding
/// anywhere. Take it, use its `HANDLE` for exactly ONE Win32 call, then
/// drop it immediately: NEVER hold it across [`wait_overlapped`]'s own
/// blocking wait — the kernel already has an established I/O context
/// tied to the handle value used at submission time, so the wait itself
/// needs no further guarding, and holding a guard across it would let a
/// single stalled connection's read/write block `close_all` (and
/// therefore `disconnect_listener`) indefinitely, exactly the "never
/// blocks" contract this module promises elsewhere.
struct LiveHandle<'a> {
    _read: RwLockReadGuard<'a, RegistryState>,
    handle: SendableHandle,
}

impl LiveHandle<'_> {
    fn get(&self) -> HANDLE {
        self.handle.0
    }
}

/// [`InstanceRegistry`]'s internal state: either open (mapping ids to
/// their currently-live handle) or permanently closed.
enum RegistryState {
    Open(HashMap<u64, SendableHandle>),
    /// [`InstanceRegistry::close_all`] has run — permanently; never
    /// reopened.
    Closed,
}

/// The outcome of [`InstanceRegistry::create_and_register`].
enum CreateOutcome {
    Created(u64, SendableHandle),
    CreateFailed(std::io::Error),
    /// The registry was already `Closed` — `make` was never called.
    ShuttingDown,
}

/// Every pipe-instance HANDLE this server ever creates is created AND
/// registered here ATOMICALLY (see [`create_and_register`]
/// (Self::create_and_register)), and stays registered — through any
/// number of recycle/reuse cycles, through becoming a live connection,
/// through sitting in `AcceptState::retained_dead` — until
/// [`close_all`](Self::close_all) finds and closes it.
///
/// INVARIANT: every instance handle has exactly one closer, AND a
/// handle is only ever passed to an OS call while a [`LiveHandle`]
/// proves it live (see that type's own doc). In this module's own
/// normal operation (see the module doc's "Continuous name hold"
/// section) the closer is NEVER invoked — an instance is recycled or
/// retained-dead forever, never actually closed, while the server
/// intends to keep accepting. The ONE exception is `close_all`, called
/// EXACTLY once, from [`PipeServer::disconnect_listener`]: it atomically
/// drains every currently-registered id and closes each one directly,
/// then permanently refuses (`create_and_register` reports
/// `ShuttingDown` instead of ever calling its `make` closure) any later
/// creation. Because no OTHER code path ever individually removes an id,
/// there is no "removed here, but the remover assumed someone else would
/// close it" gap: whichever bucket (the accept loop's own pending
/// instance, `recycled`, `retained_dead`, or a live `ConnHandle` in
/// `conns`) an id's instance currently sits in, at the instant
/// `close_all` runs it is found and closed — independent of `conns`'
/// own, unrelated bookkeeping timing.
struct InstanceRegistry {
    next_id: AtomicU64,
    state: RwLock<RegistryState>,
}

impl InstanceRegistry {
    fn new() -> Self {
        Self {
            next_id: AtomicU64::new(0),
            state: RwLock::new(RegistryState::Open(HashMap::new())),
        }
    }

    /// Create a NEW instance and register it, ATOMICALLY with respect to
    /// [`close_all`](Self::close_all) (Codex round-4 finding 1): both this
    /// method and `close_all` take the SAME lock's WRITE side, so either
    /// `make` runs to completion and its handle is inserted BEFORE
    /// `close_all` can ever observe this registry as fully drained, or
    /// the registry is ALREADY `Closed` and `make` never runs at all —
    /// there is no window where a handle exists, holds (or is about to
    /// recreate) the pipe NAME, and is not yet in the map for
    /// `close_all` to find. `make` is `CreateNamedPipeW` wrapped by the
    /// caller — a local syscall, not expected to block meaningfully —
    /// held under this write lock as a deliberate, rare-and-bounded
    /// exception to this module's usual "never block disconnect_listener"
    /// rule, exactly because instance CREATION and teardown's
    /// `close_all` must never interleave.
    fn create_and_register(
        &self,
        make: impl FnOnce() -> std::io::Result<OwnedHandle>,
    ) -> CreateOutcome {
        let mut state = self.state.write().unwrap();
        match &mut *state {
            RegistryState::Closed => CreateOutcome::ShuttingDown,
            RegistryState::Open(map) => match make() {
                Ok(owned) => {
                    let raw = SendableHandle(owned.as_raw_handle() as HANDLE);
                    // From this point, THIS registry is the sole future
                    // closer -- never Rust's own `Drop`.
                    std::mem::forget(owned);
                    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                    map.insert(id, raw);
                    CreateOutcome::Created(id, raw)
                }
                Err(e) => CreateOutcome::CreateFailed(e),
            },
        }
    }

    /// Take a temporarily-live view of `id`'s handle for exactly ONE
    /// Win32 call — see [`LiveHandle`]'s own doc for the invariant this
    /// establishes. `None` if `id` is not currently registered (already
    /// closed by `close_all`).
    fn live(&self, id: u64) -> Option<LiveHandle<'_>> {
        let read = self.state.read().unwrap();
        match &*read {
            RegistryState::Open(map) => {
                let handle = *map.get(&id)?;
                Some(LiveHandle { _read: read, handle })
            }
            RegistryState::Closed => None,
        }
    }

    /// The one atomic drain-and-shutdown: close EVERY instance currently
    /// registered — regardless of which of this module's several
    /// buckets currently references it — then permanently transition to
    /// `Closed` (future `create_and_register`/`live` calls report
    /// `ShuttingDown`/`None`). Takes the WRITE side of the SAME lock
    /// `live`/`create_and_register` use, so this cannot run concurrently
    /// with (or interleave into the middle of) either — see
    /// [`LiveHandle`]'s and `create_and_register`'s own docs. Never
    /// blocks on anything but that lock: every entry is one
    /// `CloseHandle`, not a join.
    fn close_all(&self) {
        let mut state = self.state.write().unwrap();
        if let RegistryState::Open(map) = &mut *state {
            for (_, handle) in map.drain() {
                unsafe { CloseHandle(handle.0) };
            }
        }
        *state = RegistryState::Closed;
    }
}

/// One live connection's threads, handles, and slots — owned by the
/// `conns` map for the connection's whole life; removed and torn down
/// exclusively by [`teardown_if_present`], called exclusively from
/// [`reaper_loop`]. The underlying instance HANDLE's own closing is
/// entirely [`InstanceRegistry`]'s job (`registry_id` is this
/// connection's persistent id there, registered once at creation and
/// never released individually) — this struct's own `raw` is a
/// non-owning reference, usable only through a [`LiveHandle`].
struct ConnHandle {
    raw: SendableHandle,
    registry_id: u64,
    read_slot: Arc<IoSlot>,
    write_slot: Arc<IoSlot>,
    outbound: Arc<OutboundBudget>,
    sender: Sender<WriteCmd>,
    reader_jh: JoinHandle<()>,
    writer_jh: JoinHandle<()>,
    /// At-most-once teardown gate shared with the reader/writer threads —
    /// see [`request_teardown`].
    torn_down_requested: Arc<AtomicBool>,
}

/// Accept-loop state shared with [`PipeServer`]'s public methods and
/// `Drop`.
struct AcceptState {
    /// The accept loop should stop (and, once observed, HAS stopped)
    /// accepting new connections — set either by `PipeServer::drop` or by
    /// a persistent resource failure the accept loop reported via
    /// `AcceptError` (see `ServerShared::dropping` for the distinct
    /// "the whole server is being dropped" flag). EXISTING connections
    /// are unaffected either way.
    accept_stopping: bool,
    /// Instances successfully created and currently held (recycled or
    /// live) for this pipe name (<= `max_instances`): incremented when a
    /// creation attempt is about to run, decremented if that attempt
    /// fails, so it always reflects instances this server actually holds
    /// rather than a permanently-climbing attempt counter.
    created: u32,
    /// Disconnected, ready-to-relisten instances, each paired with its
    /// PERSISTENT `InstanceRegistry` id (registered once, at creation;
    /// recycling reuses the SAME id, never re-registers). Popped by the
    /// accept loop in preference to creating a fresh instance.
    recycled: VecDeque<(u64, SendableHandle)>,
    /// Instances a failed `DisconnectNamedPipe` left in an unknown state
    /// — retained (never closed here, never reused) for the rest of the
    /// server's life so the pipe name's continuous hold survives the
    /// failure; `InstanceRegistry::close_all` closes them like every
    /// other still-registered instance once the server actually tears
    /// down. See the module doc's "Continuous name hold" section.
    retained_dead: Vec<(u64, SendableHandle)>,
    /// The accept loop's currently in-flight `ConnectNamedPipe` attempt,
    /// if any — consulted so [`stop_accept_loop`] can
    /// [`IoSlot::cancel_registered`] exactly the operation that's
    /// actually pending, from whichever thread discovers a reason to
    /// stop (the caller dropping the server, or the reaper thread
    /// finding a `DisconnectNamedPipe` failure while tearing down an
    /// unrelated connection).
    current: Option<(u64, SendableHandle, Arc<IoSlot>)>,
}

/// A message to [`reaper_loop`] — the only thread that ever removes a
/// registered connection from `conns` or joins its threads.
enum ReaperMsg {
    /// A connection ended (natural EOF/error, or a caller's `close`).
    Torn(ConnId, ClosedReason),
    /// The server is being dropped: drain and tear down every connection
    /// still in `conns` (no `Closed` event for these — nothing could ever
    /// observe it), then stop.
    Shutdown,
}

struct ServerShared {
    conns: Mutex<HashMap<ConnId, ConnHandle>>,
    next_id: AtomicU64,
    accept: Mutex<AcceptState>,
    accept_cv: Condvar,
    reaper_tx: SyncSender<ReaperMsg>,
    events_tx: SyncSender<TransportEvent>,
    max_instances: u32,
    name: Vec<u16>,
    /// Set exactly once, by `PipeServer::disconnect_listener` (which
    /// `Drop::drop` always calls first), at the very START of that
    /// call — before anything else, including the accept-thread join —
    /// because it is the one escape for [`send_lifecycle_event`]'s
    /// otherwise-indefinite retry loop, and that loop can be running on
    /// the very thread `drop` is about to join. See the module doc's
    /// "Reliable lifecycle delivery" section. `recycle_instance`/
    /// `accept_loop` also read it, purely as a CLEANLINESS optimization
    /// (skip a pointless OS call once teardown is under way) — never a
    /// safety decision; see [`InstanceRegistry`]'s own doc for why actual
    /// instance closing/use no longer depends on this flag at all.
    dropping: AtomicBool,
    /// Every pipe-instance handle this server has ever created, and the
    /// SOLE mechanism that ever closes one or proves one live — see
    /// [`InstanceRegistry`]'s own doc for the ownership invariant.
    instances: InstanceRegistry,
    /// TEST-OBSERVABLE (Codex round-5 fix 2b/2c), written unconditionally
    /// by production code: `stop_accept_loop` sets this to whatever
    /// [`IoSlot::cancel_registered`] returned for the accept loop's own
    /// pending `ConnectNamedPipe`, if any — the TOCTOU-free proof that a
    /// genuinely async op existed at the EXACT instant `disconnect_listener`
    /// cancelled it, as opposed to a separate pre-check that could go
    /// stale before teardown actually runs. Left `false` if there was
    /// nothing pending to cancel.
    accept_cancel_observed_genuine_pending: AtomicBool,
    /// TEST-OBSERVABLE (Codex round-5 fix 2b/2c), written unconditionally
    /// by `disconnect_listener`: for every connection still live at
    /// teardown, whether its WRITE slot's cancellation observed a
    /// genuinely async pending `WriteFile` — same TOCTOU-free reasoning
    /// as `accept_cancel_observed_genuine_pending`, scoped per
    /// connection since several can be torn down at once.
    write_cancel_observed_genuine_pending: Mutex<HashMap<ConnId, bool>>,
}

/// The server side of one voyage's pipe: `bind` creates the pipe (with the
/// squat-detecting first instance) and starts accepting; connections and
/// their bytes/completions/closes surface on [`PipeServer::events`].
///
/// # Lifetime rule (ADR 0041: "the pipe is never live while the writer
/// lock is free")
///
/// `bind` is an explicit constructor with no implicit background
/// construction — the CALLER (the capsule, in the follow-up unit) is
/// responsible for calling it only after `open_for_writing` holds the
/// voyage's writer lock, and for dropping the returned `PipeServer` before
/// releasing that lock. This module only guarantees: while a `PipeServer`
/// is alive, the pipe exists; the instant it is dropped, every instance is
/// closed.
///
/// `max_instances` is the RAW total simultaneous pipe-instance ceiling
/// this transport enforces (`CreateNamedPipeW`'s own `nMaxInstances`) —
/// ADR 0041 requires this to already be the CALLER's combined figure
/// (subscribers plus separately bounded pre-hello/mgmt connections); that
/// combination is the follow-up capsule unit's job, not this transport's.
pub struct PipeServer {
    shared: Arc<ServerShared>,
    events_rx: Receiver<TransportEvent>,
    accept_jh: Option<JoinHandle<()>>,
    reaper_jh: Option<JoinHandle<()>>,
    /// Reader/writer `JoinHandle`s for every connection
    /// [`PipeServer::disconnect_listener`] closed directly (Codex round-1
    /// Blocker 2/3 discharge) — their `ConnHandle` never reaches the
    /// reaper (it drained `shared.conns` itself), so nothing else would
    /// ever join them. [`PipeServer::join_workers`] joins every entry here
    /// under the SAME shared deadline as the acceptor and reaper.
    detached_workers: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for PipeServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeServer").finish_non_exhaustive()
    }
}

impl PipeServer {
    /// Create `\\.\pipe\sot-voyage-<voyage_id>` (squat-detected via
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` on this, its first instance,
    /// created AND REGISTERED synchronously — Codex round-4 finding 1 —
    /// so a squat is a loud, immediate `bind` failure and no unregistered
    /// handle can ever outlive this constructor) and start the reaper
    /// and accept threads. `max_instances` must be in Win32's own
    /// documented `1..=255` range.
    pub fn bind(voyage_id: &str, max_instances: u32) -> Result<Self, PipeError> {
        validate_voyage_id(voyage_id)?;
        Self::bind_named(pipe_name_wide(voyage_id), max_instances)
    }

    /// ADR 0041 step 6 U2: the supervisor lane's own pipe,
    /// `\\.\pipe\sot-supervisor-<h>` — otherwise identical to [`Self::bind`]
    /// (same security posture via [`create_pipe_instance`], same
    /// accept/reaper machinery, same squat detection). `h` is the caller's
    /// own stable hash of the canonicalized state-dir path (ADR 0041
    /// Lifecycle "Name and identity") — this constructor does not derive
    /// or validate it as a voyage id, unlike [`Self::bind`].
    pub fn bind_supervisor(h: &str, max_instances: u32) -> Result<Self, PipeError> {
        Self::bind_named(supervisor_pipe_name_wide(h), max_instances)
    }

    /// Shared construction (round-4 finding 1's squat-detection ordering
    /// applies identically to both pipe families): given an
    /// already-resolved wide pipe name, create AND REGISTER the
    /// squat-detecting first instance synchronously, then start the
    /// reaper and accept threads. `max_instances` must be in Win32's own
    /// documented `1..=255` range.
    fn bind_named(name: Vec<u16>, max_instances: u32) -> Result<Self, PipeError> {
        if !(1..=255).contains(&max_instances) {
            return Err(PipeError::InvalidMaxInstances);
        }

        let (events_tx, events_rx) = mpsc::sync_channel(EVENTS_CHANNEL_CAP);
        let (reaper_tx, reaper_rx) =
            mpsc::sync_channel(max_instances as usize + REAPER_INBOX_SLACK);
        let shared = Arc::new(ServerShared {
            conns: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            accept: Mutex::new(AcceptState {
                accept_stopping: false,
                created: 0,
                recycled: VecDeque::new(),
                retained_dead: Vec::new(),
                current: None,
            }),
            accept_cv: Condvar::new(),
            reaper_tx,
            events_tx,
            max_instances,
            name,
            dropping: AtomicBool::new(false),
            instances: InstanceRegistry::new(),
            accept_cancel_observed_genuine_pending: AtomicBool::new(false),
            write_cancel_observed_genuine_pending: Mutex::new(HashMap::new()),
        });

        // Create AND register the squat-detecting first instance
        // synchronously, before this constructor ever returns (Codex
        // round-4 finding 1): `shared` is a brand-new `Arc` no other
        // code has a reference to yet, so `disconnect_listener` cannot
        // possibly have run against it -- `ShuttingDown` here would mean
        // this module's own invariant is broken, not a real runtime
        // condition.
        let (first_id, first_raw) = match shared
            .instances
            .create_and_register(|| create_pipe_instance(&shared.name, true, max_instances))
        {
            CreateOutcome::Created(id, raw) => {
                shared.accept.lock().unwrap().created = 1;
                (id, raw)
            }
            CreateOutcome::CreateFailed(e) => {
                return Err(PipeError::Io {
                    op: "CreateNamedPipeW(first instance)",
                    source: e,
                })
            }
            CreateOutcome::ShuttingDown => {
                unreachable!("a brand-new PipeServer's registry cannot already be torn down")
            }
        };

        // Spawn the reaper FIRST. If the accept thread then fails to
        // spawn, unwind the reaper (it has nothing queued yet, so its
        // own `Shutdown` drain is instant) rather than leave it running
        // forever with no accept thread able to feed it. No `JoinHandle`
        // is ever dropped while its thread could still run.
        let reaper_jh = thread::Builder::new()
            .name("sot-pipe-reaper".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || reaper_loop(shared, reaper_rx)
            });
        let reaper_jh = match reaper_jh {
            Ok(jh) => jh,
            Err(e) => {
                // The first instance was already created AND registered
                // above (Codex round-4: `mem::forget`'d into
                // `shared.instances`, no longer under Rust's own Drop) --
                // with no `PipeServer` ever coming into existence to call
                // `disconnect_listener`, nothing else will ever close it.
                // `close_all` here is this failure path's ONLY chance.
                shared.instances.close_all();
                return Err(PipeError::Io {
                    op: "spawn reaper thread",
                    source: e,
                });
            }
        };

        let accept_jh = thread::Builder::new()
            .name("sot-pipe-accept".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || accept_loop(shared, first_id, first_raw)
            });
        let accept_jh = match accept_jh {
            Ok(jh) => jh,
            Err(e) => {
                // Same reasoning as the reaper-spawn-failure arm above.
                shared.instances.close_all();
                let _ = shared.reaper_tx.send(ReaperMsg::Shutdown);
                reaper_jh.join().ok();
                return Err(PipeError::Io {
                    op: "spawn accept thread",
                    source: e,
                });
            }
        };

        Ok(Self {
            shared,
            events_rx,
            accept_jh: Some(accept_jh),
            reaper_jh: Some(reaper_jh),
            detached_workers: Vec::new(),
        })
    }

    /// The event stream: `Accepted`/`Bytes`/`Sent`/`Closed`/`AcceptError`,
    /// in the order this transport observed them. Single-consumer by
    /// convention (a `Receiver` is not `Sync`). The CONSUMER's half of the
    /// reliable-lifecycle-delivery contract (see the module doc) is to
    /// keep draining this — a stalled consumer backs everything up but
    /// never silently loses a lifecycle event.
    pub fn events(&self) -> &Receiver<TransportEvent> {
        &self.events_rx
    }

    /// Queue `bytes` for `conn_id`, tagged with `marker` if the caller
    /// wants a [`TransportEvent::Sent`] once the OS write physically
    /// completes. `bytes` must be non-empty and no larger than a single
    /// Win32 write can represent. Non-blocking: a full outbound budget or
    /// an unknown/already-closed connection both return `Err` immediately
    /// — backpressure POLICY belongs to whoever calls this.
    pub fn send(
        &self,
        conn_id: ConnId,
        bytes: Vec<u8>,
        marker: Option<SendMarker>,
    ) -> Result<(), PipeError> {
        if bytes.is_empty() {
            return Err(PipeError::EmptyPayload);
        }
        if bytes.len() > u32::MAX as usize {
            return Err(PipeError::PayloadTooLarge(bytes.len()));
        }
        let len = bytes.len();
        let map = self.shared.conns.lock().unwrap();
        let conn = map
            .get(&conn_id)
            .ok_or(PipeError::UnknownConnection(conn_id))?;
        if !conn.outbound.try_reserve(len) {
            return Err(PipeError::QueueFull(conn_id));
        }
        if conn.sender.send(WriteCmd { bytes, marker }).is_err() {
            conn.outbound.release(len);
            return Err(PipeError::UnknownConnection(conn_id));
        }
        Ok(())
    }

    /// Request that `conn_id` be torn down: both directions cancelled,
    /// both threads joined, the instance recycled. Fire-and-forget — this
    /// enqueues the request at most once for the reaper thread;
    /// completion is observed as [`TransportEvent::Closed`]. A no-op if
    /// `conn_id` is already gone or already has a teardown in flight.
    pub fn close(&self, conn_id: ConnId) {
        let map = self.shared.conns.lock().unwrap();
        if let Some(conn) = map.get(&conn_id) {
            request_teardown(
                &self.shared,
                conn_id,
                &conn.torn_down_requested,
                ClosedReason::Closed,
            );
        }
    }
}

/// TEST-ONLY (ADR 0041 step 6 U1b, Codex round-3/4/5 test premise-gap
/// fixes). Not compiled into a production build — `feature =
/// "test-support"` only (see `Cargo.toml`'s own doc on that feature).
#[cfg(any(test, feature = "test-support"))]
impl PipeServer {
    /// Poll until the accept loop's CURRENT `ConnectNamedPipe` has
    /// GENUINELY gone `ERROR_IO_PENDING` at the OS level
    /// (`IoSlot::is_genuinely_pending`, set only once `issue` has
    /// actually returned that code — NOT `AcceptState::current.is_some()`,
    /// populated BEFORE `ConnectNamedPipe` is ever called, and NOT plain
    /// `SlotState::Pending`, which is ALSO set for a synchronously-
    /// completed op still awaiting result collection — Codex round-4
    /// finding 3 / round-5 finding 2) or `timeout` elapses. This poll is
    /// a best-effort PRE-check only, deciding WHEN it is worth calling
    /// `disconnect_listener` — the actual PROOF returned is the TOCTOU-
    /// free latch `stop_accept_loop`'s own synchronized cancellation
    /// records in `ServerShared::accept_cancel_observed_genuine_pending`
    /// (Codex round-5 fix 2b/2c: being in one function does not itself
    /// eliminate a TOCTOU between a pre-check and a later act — the
    /// PROOF must come from the SAME critical section that performs the
    /// cancellation, which this method's call to `disconnect_listener`
    /// triggers). Returns that latch's value; `false` on timeout
    /// (`disconnect_listener` NOT called at all).
    pub fn assert_accept_parked_then_disconnect_listener_for_test(
        &mut self,
        timeout: Duration,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let slot = self
                .shared
                .accept
                .lock()
                .unwrap()
                .current
                .as_ref()
                .map(|(_, _, slot)| Arc::clone(slot));
            if let Some(slot) = slot {
                if slot.is_genuinely_pending() {
                    self.disconnect_listener();
                    return self
                        .shared
                        .accept_cancel_observed_genuine_pending
                        .load(Ordering::Acquire);
                }
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(JOIN_POLL_INTERVAL);
        }
    }

    /// Poll until `conn_id`'s writer has genuinely gone `ERROR_IO_PENDING`
    /// at the OS level (`IoSlot::is_genuinely_pending`) or `timeout`
    /// elapses. `PipeError::QueueFull` alone only proves the outbound
    /// BYTE budget is reserved, and plain `SlotState::Pending` is ALSO
    /// set for a synchronously-completed write still awaiting result
    /// collection (Codex round-4 finding 3 / round-5 finding 2) — neither
    /// proves the writer thread has actually reached a GENUINE pending
    /// `WriteFile` against a peer that never drains. This is a
    /// best-effort PRE-check only, deciding WHEN it is worth proceeding
    /// to teardown — see
    /// `conn_write_was_genuinely_pending_at_teardown_for_test` for the
    /// actual, TOCTOU-free proof, which must be read AFTER
    /// `disconnect_listener` has run.
    pub fn conn_write_pending_for_test(&self, conn_id: ConnId, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let pending = self
                .shared
                .conns
                .lock()
                .unwrap()
                .get(&conn_id)
                .map(|c| c.write_slot.is_genuinely_pending())
                .unwrap_or(false);
            if pending {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(JOIN_POLL_INTERVAL);
        }
    }

    /// The TOCTOU-free proof (Codex round-5 fix 2b/2c) that `conn_id`'s
    /// WRITE slot was GENUINELY asynchronously pending at the exact
    /// synchronized instant `disconnect_listener`'s own cancellation
    /// pass touched it — decided under the SAME lock acquisition that
    /// performed the cancellation, so (unlike a separate pre-check) it
    /// cannot go stale between being observed and being acted on. Call
    /// AFTER `disconnect_listener`, never before. `None` if `conn_id`
    /// was never live at any `disconnect_listener` call.
    pub fn conn_write_was_genuinely_pending_at_teardown_for_test(
        &self,
        conn_id: ConnId,
    ) -> Option<bool> {
        self.shared
            .write_cancel_observed_genuine_pending
            .lock()
            .unwrap()
            .get(&conn_id)
            .copied()
    }
}

/// ADR 0041 Lifecycle "the pipe NAME disappears before any blocking
/// join" / the bounds table's "teardown aggregate": 20 s TOTAL after the
/// listener is gone, one absolute deadline shared by every join
/// (acceptor, reaper, and — inside the reaper's own drain — every
/// connection worker), loud on expiry.
pub const TEARDOWN_AGGREGATE_DEADLINE: Duration = Duration::from_secs(20);

/// [`join_within`]'s poll granularity — small enough that a fast, healthy
/// teardown (the ordinary case) never visibly waits for it, and small
/// enough that a test proving "loud on expiry" against a short injected
/// budget stays fast too.
const JOIN_POLL_INTERVAL: Duration = Duration::from_millis(5);

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

impl PipeServer {
    /// Phase one of teardown: make the pipe NAME disappear AND issue
    /// cancellation to every worker — synchronous, no blocking join.
    /// Latches [`ServerShared::dropping`] FIRST (see its doc for why),
    /// cancels a pending accept (if any -- [`stop_accept_loop`]), THEN
    /// requests cancellation on every currently-live connection's read
    /// AND write I/O.
    ///
    /// **Real-Windows correction (this round's own CI diagnosis):** an
    /// earlier revision of this method relied on
    /// [`InstanceRegistry::close_all`]'s `CloseHandle` ALONE to unstick
    /// a connection's I/O, reasoning that closing the handle "forces any
    /// outstanding ReadFile/WriteFile to complete or error" and made an
    /// explicit per-connection `CancelIoEx` redundant. Real Windows CI
    /// proved that reasoning wrong for a WRITE genuinely stalled on
    /// full-buffer backpressure (a stalled reader has no pending write
    /// to unstick; the write test case does): `CancelIoEx` TARGETS and
    /// cancels the exact pending I/O request at the driver level and is
    /// the documented, prompt way to unstick it; `CloseHandle` alone was
    /// observed NOT to complete that unstick within this suite's 5s
    /// teardown budget. The explicit cancellation pass is restored here,
    /// BEFORE `close_all`, and is not merely a nicety.
    ///
    /// Each connection's cancellation ALSO latches (Codex round-5 fix
    /// 2b/2c) whether its WRITE slot was observed GENUINELY
    /// asynchronously pending at that exact synchronized instant, into
    /// [`ServerShared::write_cancel_observed_genuine_pending`] — the
    /// TOCTOU-free proof a test needs, decided under the SAME lock
    /// acquisition that performs the cancellation rather than a separate
    /// pre-check that could go stale before this method actually runs.
    ///
    /// THEN — Codex round-4 finding 1's second bullet: BEFORE touching
    /// `conns` for detached-worker bookkeeping, not after — calls
    /// `close_all`: the ONE atomic sweep that closes EVERY instance
    /// handle still registered ANYWHERE (the accept-pending one, every
    /// idle `recycled`/`retained_dead` one, and every live connection's),
    /// independent of whether `shared.conns` still holds that
    /// connection's `ConnHandle` at this exact instant. A
    /// `FIRST_PIPE_INSTANCE` probe can win the INSTANT this method
    /// returns, live connections or not — see [`InstanceRegistry`]'s own
    /// doc for why no instance can ever be closed twice or missed, no
    /// matter which of the accept loop / reaper / this method wins
    /// whatever race, and [`LiveHandle`]'s own doc for why no OTHER
    /// thread can be mid-use of a handle when `close_all` closes it. The
    /// reader/writer threads themselves are NOT joined here —
    /// `disconnect_listener` never blocks — they are stashed in
    /// `detached_workers` for [`PipeServer::join_workers`] to join under
    /// the shared deadline.
    ///
    /// Idempotent — safe to call more than once (a test proving this
    /// phase in isolation, then the eventual real `Drop`; a second call
    /// finds `conns`/`recycled`/`retained_dead` already empty and
    /// `close_all` already run).
    pub fn disconnect_listener(&mut self) {
        self.shared.dropping.store(true, Ordering::Release);
        stop_accept_loop(&self.shared);
        self.shared.accept_cv.notify_all();
        {
            let map = self.shared.conns.lock().unwrap();
            let mut write_latches = self.shared.write_cancel_observed_genuine_pending.lock().unwrap();
            for (&conn_id, conn) in map.iter() {
                conn.read_slot.cancel_registered(&self.shared.instances, conn.registry_id);
                let write_was_pending =
                    conn.write_slot.cancel_registered(&self.shared.instances, conn.registry_id);
                write_latches.insert(conn_id, write_was_pending);
            }
        }
        // THE atomic close -- see this method's own doc and
        // `InstanceRegistry`'s. Runs BEFORE the `conns` drain below.
        self.shared.instances.close_all();
        {
            let mut st = self.shared.accept.lock().unwrap();
            st.recycled.clear();
            st.retained_dead.clear();
        }
        let drained: Vec<ConnHandle> = {
            let mut map = self.shared.conns.lock().unwrap();
            map.drain().map(|(_, conn)| conn).collect()
        };
        for conn in drained {
            drop(conn.sender);
            self.detached_workers.push(conn.reader_jh);
            self.detached_workers.push(conn.writer_jh);
        }
    }

    /// Phase two: tell the reaper to drain (a no-op for any connection
    /// `disconnect_listener` already claimed; harmless for the rare one
    /// it lost the race for, see that method's doc), then wait for the
    /// accept thread, the reaper thread, AND every detached connection
    /// worker `disconnect_listener` stashed — ALL sharing ONE absolute
    /// `deadline` (ADR 0041 "the joins share ONE 20 s absolute deadline,
    /// each wait taking the remaining budget"; Codex round-1 Blocker 3:
    /// an externally supplied absolute `Instant`, not a budget this
    /// method computes itself, so `capsule_win::run` can fold its OWN
    /// closer/reader threads into the identical deadline). `true` iff
    /// every one finished within budget; `false` (LOUD — the caller MUST
    /// treat this as terminal, never seal-and-succeed past it) on
    /// expiry. Call [`disconnect_listener`] first — this method does not
    /// call it, so the two phases stay independently observable (and
    /// independently testable).
    ///
    /// [`disconnect_listener`]: Self::disconnect_listener
    pub fn join_workers(&mut self, deadline: Instant) -> bool {
        let mut ok = true;
        if let Some(jh) = self.accept_jh.take() {
            ok = join_within(jh, deadline) && ok;
        }
        let _ = self.shared.reaper_tx.send(ReaperMsg::Shutdown);
        if let Some(jh) = self.reaper_jh.take() {
            ok = join_within(jh, deadline) && ok;
        }
        for jh in self.detached_workers.drain(..) {
            ok = join_within(jh, deadline) && ok;
        }
        ok
    }
}

impl Drop for PipeServer {
    /// The two teardown phases in order, with a FRESH pinned 20 s budget
    /// computed here — see [`PipeServer::disconnect_listener`] and
    /// [`PipeServer::join_workers`]. This is the SAFETY-NET path (a bare
    /// `drop(server)`, or an early-return `?` before the explicit
    /// `Transport::shutdown_all` call ever runs) — the designed path
    /// computes ONE deadline in `capsule_win::run` and calls both methods
    /// explicitly with it, folding the capsule's own closer/reader
    /// threads into the SAME budget; this `Drop` still works standalone
    /// for every caller that never does that. Every thread this module
    /// ever spawned is joined by the time this returns, UNLESS the
    /// deadline expired, in which case it is loudly reported (stderr —
    /// `Drop` cannot return a `Result`) and simply abandoned: a
    /// still-running worker thread outlives this `PipeServer` value, but
    /// not the process, which is exiting through this same teardown
    /// regardless.
    fn drop(&mut self) {
        self.disconnect_listener();
        let deadline = Instant::now() + TEARDOWN_AGGREGATE_DEADLINE;
        if !self.join_workers(deadline) {
            eprintln!(
                "sot-pipe: teardown did not complete within its {TEARDOWN_AGGREGATE_DEADLINE:?} \
                 aggregate deadline; a worker thread may still be running"
            );
        }
    }
}

/// Mark the accept loop stopped and request cancellation on its
/// currently in-flight `ConnectNamedPipe`, if any. Cancellation-request
/// ONLY -- closing whatever handle that in-flight attempt is using is
/// [`InstanceRegistry::close_all`]'s job (called by
/// `disconnect_listener`), not this function's: the registry, not
/// `AcceptState::current`, is this module's one source of truth for
/// "has this instance been closed" (see that type's own doc), so this
/// function no longer needs to hand anything back to its caller. Shared
/// by `PipeServer::disconnect_listener` (planned shutdown) and
/// [`terminalize_accept_loop`] (a persistent resource failure) — both
/// need the SAME cross-thread-safe cancellation, since either can be
/// triggered by a thread other than the accept thread itself (`Drop`
/// runs on the caller's thread; a resource failure can be discovered on
/// the reaper thread while tearing down an unrelated connection's
/// instance). Without this, a failure discovered off the accept thread
/// could leave a pending accept to linger — or even admit one more
/// client — after the consumer was already told no more connections are
/// coming.
fn stop_accept_loop(shared: &Arc<ServerShared>) {
    let mut st = shared.accept.lock().unwrap();
    st.accept_stopping = true;
    if let Some((id, _raw, slot)) = st.current.take() {
        // Codex round-5 fix 2b/2c: latch whether THIS cancellation
        // observed a genuinely async pending `ConnectNamedPipe`, decided
        // under the same lock acquisition `cancel_registered` uses to
        // perform the cancellation -- see
        // `ServerShared::accept_cancel_observed_genuine_pending`'s own
        // doc for why this is the TOCTOU-free proof a test needs.
        let was_pending = slot.cancel_registered(&shared.instances, id);
        shared
            .accept_cancel_observed_genuine_pending
            .store(was_pending, Ordering::Release);
    }
}

/// Stop the accept loop for good and report why — the ONE place every
/// persistent-resource-failure path routes through, regardless of which
/// thread discovers the failure. Cancels only -- never closes the
/// pending instance's handle, unlike a whole-server teardown: a resource
/// failure is not that, and the accept thread itself still owns and will
/// dispose of that handle normally (`recycle_instance`, which is a
/// cleanliness no-op once the registry is already torn down -- see its
/// own doc).
fn terminalize_accept_loop(shared: &Arc<ServerShared>, message: String) {
    stop_accept_loop(shared);
    shared.accept_cv.notify_all();
    send_lifecycle_event(shared, TransportEvent::AcceptError(message));
}

/// Set `id`/`raw` aside for reuse rather than closing it.
/// `DisconnectNamedPipe` resets it to the listening state; on success
/// it's pushed onto `AcceptState::recycled` (SAME `id`, never
/// re-registered). On FAILURE, it's retained (still registered, never
/// closed by this function) and the accept loop is terminalized — see
/// the module doc's "Continuous name hold" section for why retaining
/// the dead handle, rather than replacing or closing it, is the correct
/// response.
///
/// This function itself NEVER calls `CloseHandle`, and NEVER touches the
/// raw handle except through a [`LiveHandle`] (Codex round-4 finding 3):
/// `id` stays registered either way, and [`InstanceRegistry::close_all`]
/// is the only thing that ever actually closes it. If `id` is already
/// gone (`InstanceRegistry::live` returns `None` — `close_all` already
/// closed it, or is closing it this instant), this returns immediately,
/// touching neither `DisconnectNamedPipe` nor either queue: not a
/// cleanliness nicety here but the load-bearing check that prevents a
/// stale-handle `DisconnectNamedPipe` call racing `close_all`.
fn recycle_instance(shared: &Arc<ServerShared>, id: u64, raw: SendableHandle) {
    let disconnected = match shared.instances.live(id) {
        Some(live) => (unsafe { DisconnectNamedPipe(live.get()) }) != 0,
        None => return,
    };
    if disconnected {
        shared.accept.lock().unwrap().recycled.push_back((id, raw));
        shared.accept_cv.notify_all();
        return;
    }
    shared.accept.lock().unwrap().retained_dead.push((id, raw));
    terminalize_accept_loop(
        shared,
        "DisconnectNamedPipe failed on a torn-down instance; it is retained (never closed) to keep the pipe \
         name held, permanently costing one instance's worth of capacity, and no further connections will be \
         accepted"
            .to_string(),
    );
}

/// Deliver one lifecycle event (`Accepted`/`Sent`/`Closed`/`AcceptError`)
/// RELIABLY — see the module doc's "Reliable lifecycle delivery" section
/// for the full contract this implements.
fn send_lifecycle_event(shared: &Arc<ServerShared>, evt: TransportEvent) {
    let mut item = evt;
    loop {
        match shared.events_tx.try_send(item) {
            Ok(()) => return,
            Err(TrySendError::Disconnected(_)) => return,
            Err(TrySendError::Full(v)) => {
                item = v;
                if shared.dropping.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(EVENTS_RETRY_INTERVAL);
            }
        }
    }
}

/// Request teardown for `conn_id`, at most once: every caller (an
/// explicit `close`, the reader's own EOF signal, a writer's own
/// `WriteFile`-error signal) races the SAME connection's `flag` via
/// `compare_exchange`; only the winner enqueues a [`ReaperMsg`], so
/// repeated or concurrent requests can never grow the reaper's bounded
/// inbox past one entry per connection.
fn request_teardown(
    shared: &Arc<ServerShared>,
    conn_id: ConnId,
    flag: &AtomicBool,
    reason: ClosedReason,
) {
    if flag
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let _ = shared.reaper_tx.send(ReaperMsg::Torn(conn_id, reason));
    }
}

/// Notify the consumer that a just-connected instance could not be fully
/// registered (write-slot creation, or a worker's `thread::Builder::spawn`,
/// failed) — `Accepted` then an immediate `Closed(Error(..))`, both via
/// the reliable path, so the consumer's own per-connection bookkeeping is
/// created and discarded cleanly rather than never learning this
/// connection existed. Not terminal to the accept loop — the next
/// connection attempt is unaffected.
fn report_registration_failure(shared: &Arc<ServerShared>, what: &str, e: impl std::fmt::Display) {
    let conn_id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    send_lifecycle_event(shared, TransportEvent::Accepted(conn_id));
    send_lifecycle_event(
        shared,
        TransportEvent::Closed(conn_id, ClosedReason::Error(format!("{what}: {e}"))),
    );
}

/// Tear down `conn_id` if it is still present — called EXCLUSIVELY from
/// [`reaper_loop`], which processes messages strictly one at a time, so
/// this never runs concurrently with itself and the `conns.remove` below
/// is the single, uncontested point of truth for "who claims this
/// connection." `reason: None` is `Drop`'s shutdown pass — no event is
/// emitted (nothing could ever observe it).
fn teardown_if_present(shared: &Arc<ServerShared>, conn_id: ConnId, reason: Option<ClosedReason>) {
    let conn = shared.conns.lock().unwrap().remove(&conn_id);
    let Some(conn) = conn else { return };
    conn.read_slot.cancel_registered(&shared.instances, conn.registry_id);
    conn.write_slot.cancel_registered(&shared.instances, conn.registry_id);
    drop(conn.sender); // unblocks a writer idle-waiting on `recv` with nothing queued
    conn.reader_jh.join().ok();
    conn.writer_jh.join().ok();
    // Codex round-3/4 discharge: this connection's instance HANDLE is
    // closed entirely through `InstanceRegistry` (`conn.registry_id`
    // stayed registered this whole connection's life, independent of
    // `conns` map membership) -- `disconnect_listener`'s own
    // `close_all` finds and closes it correctly regardless of whether
    // that runs before, during, or after THIS function, and regardless
    // of whether the reaper (here) or `disconnect_listener`'s own drain
    // is what removed the `ConnHandle` from `conns`. `recycle_instance`
    // itself now checks liveness before touching the handle (round-4),
    // so calling it unconditionally is safe -- it is a no-op if
    // `close_all` already claimed this id.
    recycle_instance(shared, conn.registry_id, conn.raw);
    if let Some(reason) = reason {
        send_lifecycle_event(shared, TransportEvent::Closed(conn_id, reason));
    }
}

/// The reaper: the only thread that ever removes a registered connection
/// from `conns` or joins its reader/writer (see the module doc's
/// "Reaping" section — `handle_new_connection`'s local join of an
/// ABORTED, never-registered reader is the one correct exception).
/// Processes [`ReaperMsg`]s strictly one at a time.
fn reaper_loop(shared: Arc<ServerShared>, rx: Receiver<ReaperMsg>) {
    for msg in rx.iter() {
        match msg {
            ReaperMsg::Torn(id, reason) => teardown_if_present(&shared, id, Some(reason)),
            ReaperMsg::Shutdown => {
                let ids: Vec<ConnId> = shared.conns.lock().unwrap().keys().copied().collect();
                for id in ids {
                    teardown_if_present(&shared, id, None);
                }
                return;
            }
        }
    }
}

/// Obtain the next instance to listen on: a recycled one (SAME
/// registered id it was given at its own original creation — see
/// [`InstanceRegistry`]'s own doc for why recycling never re-registers)
/// in preference to creating a fresh one (via
/// [`InstanceRegistry::create_and_register`], Codex round-4 finding 1),
/// blocking at the instance cap with nothing recycled yet. Waits on the
/// plain condvar — every state change that could satisfy this predicate
/// (`Drop`, a recycle) already `notify_all`s, so a polling wait would
/// buy nothing. `None` means: stop accepting — shutdown, a persistent
/// creation failure already reported via `TransportEvent::AcceptError`,
/// or (rarely) a creation attempt that found the registry already torn
/// down (`create_and_register` never even called `CreateNamedPipeW` in
/// that case).
fn obtain_instance(shared: &Arc<ServerShared>) -> Option<(u64, SendableHandle)> {
    loop {
        let mut st = shared.accept.lock().unwrap();
        if st.accept_stopping {
            return None;
        }
        if let Some(entry) = st.recycled.pop_front() {
            return Some(entry);
        }
        if st.created < shared.max_instances {
            st.created += 1;
            drop(st);
            return match shared
                .instances
                .create_and_register(|| create_pipe_instance(&shared.name, false, shared.max_instances))
            {
                CreateOutcome::Created(id, raw) => Some((id, raw)),
                CreateOutcome::CreateFailed(e) => {
                    let mut st = shared.accept.lock().unwrap();
                    st.created -= 1;
                    drop(st);
                    terminalize_accept_loop(shared, e.to_string());
                    None
                }
                CreateOutcome::ShuttingDown => None,
            };
        }
        st = shared.accept_cv.wait(st).unwrap();
        drop(st);
    }
}

/// The accept loop, one dedicated thread for the server's whole life.
/// `(first_id, first_raw)` is the already-created-and-registered (with
/// `FILE_FLAG_FIRST_PIPE_INSTANCE`) instance from `bind`; every later
/// instance comes from [`obtain_instance`] (recycled or freshly created,
/// never carrying that flag).
fn accept_loop(shared: Arc<ServerShared>, first_id: u64, first_raw: SendableHandle) {
    let mut pending_first = Some((first_id, first_raw));
    loop {
        let (id, raw) = match pending_first.take() {
            Some(entry) => entry,
            None => match obtain_instance(&shared) {
                Some(entry) => entry,
                None => return,
            },
        };

        let slot = match IoSlot::new() {
            Ok(s) => Arc::new(s),
            Err(e) => {
                // A slot-creation failure here is a resource failure
                // exactly like `create_pipe_instance`'s own.
                recycle_instance(&shared, id, raw);
                terminalize_accept_loop(&shared, format!("IoSlot::new (accept): {e}"));
                return;
            }
        };

        {
            let mut st = shared.accept.lock().unwrap();
            if st.accept_stopping {
                drop(st);
                recycle_instance(&shared, id, raw);
                return;
            }
            st.current = Some((id, raw, Arc::clone(&slot)));
        }
        let connect_result = slot.submit_and_wait_registered(
            &shared.instances,
            id,
            |h, ov| unsafe { ConnectNamedPipe(h, ov) },
            |code| code == ERROR_PIPE_CONNECTED as i32,
        );
        shared.accept.lock().unwrap().current = None;

        if let Err(e) = &connect_result {
            if is_completion_unproven(e) {
                // Codex round-4 finding 2: never reuse or drop this
                // slot's OVERLAPPED/event again -- leak an extra
                // reference to it forever (see `CompletionUnproven`'s
                // own doc) and stop accepting entirely. The instance's
                // OWN handle stays registered and will be closed
                // normally, safely, whenever the server actually tears
                // down (`CloseHandle` on a handle with genuinely pending
                // I/O is well documented as safe; it is only THIS
                // module's own OVERLAPPED/event/buffer memory that must
                // never be freed early).
                std::mem::forget(Arc::clone(&slot));
                terminalize_accept_loop(
                    &shared,
                    "a pending ConnectNamedPipe's completion could not be affirmatively observed; \
                     the accept loop stopped rather than risk a use-after-free"
                        .to_string(),
                );
                return;
            }
        }

        // Codex round-3/4: `id`/`raw` stay correctly owned by
        // `InstanceRegistry` regardless of this check's own timing (see
        // that type's doc, and `recycle_instance`'s) -- this is a pure
        // CLEANLINESS optimization, not a safety decision. Once
        // `disconnect_listener` has started, avoid a pointless
        // recycle/connection attempt and a possible spurious
        // `AcceptError` on what may already be a closed handle.
        if shared.dropping.load(Ordering::Acquire) {
            return;
        }

        match connect_result {
            Ok(_) => handle_new_connection(&shared, id, raw, slot),
            Err(e) if e.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) => {
                // This module's own cancellation (a `Drop`-triggered
                // stop) — never a real client.
                recycle_instance(&shared, id, raw);
                if shared.accept.lock().unwrap().accept_stopping {
                    return;
                }
                // Not actually stopping -- nothing else ever cancels this
                // slot, but stay defensive and just try again.
            }
            Err(e) if e.raw_os_error().is_some_and(is_disconnect_family) => {
                // A client connected and vanished before/while the
                // completion was processed -- register it anyway so the
                // reader's own first `ReadFile` discovers and classifies
                // whatever actually happened, giving it a real
                // `Accepted` -> `Closed` lifecycle instead of discarding
                // a real client attempt.
                handle_new_connection(&shared, id, raw, slot);
            }
            Err(e) => {
                // Any other connect failure is a genuine anomaly, not a
                // disconnect race -- recycle the instance and terminalize
                // rather than misreport it as a client that connected.
                recycle_instance(&shared, id, raw);
                terminalize_accept_loop(&shared, format!("ConnectNamedPipe: {e}"));
                return;
            }
        }
    }
}

/// Hand off a just-connected instance: spawn its reader/writer threads
/// (gated — see [`StartGate`]), register it, THEN reliably publish
/// `Accepted` and open the gate. `thread::Builder::spawn` makes a spawn
/// failure recoverable: if the writer fails to spawn, the already-spawned
/// reader (still gated, having never touched the pipe) is `abort`ed and
/// joined directly here — bounded, since an aborted gate wait returns
/// immediately — before the instance is recycled, so a handle is never
/// closed (or recycled) while any thread might still be using it. Any
/// registration failure is reported via [`report_registration_failure`]
/// rather than silently dropping the client.
fn handle_new_connection(
    shared: &Arc<ServerShared>,
    id: u64,
    raw: SendableHandle,
    read_slot: Arc<IoSlot>,
) {
    let write_slot = match IoSlot::new() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            recycle_instance(shared, id, raw);
            report_registration_failure(shared, "write-slot setup failed", e);
            return;
        }
    };
    let conn_id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<WriteCmd>();
    let outbound = Arc::new(OutboundBudget::new());
    let gate = StartGate::new();
    let torn_down_requested = Arc::new(AtomicBool::new(false));

    let reader_jh = {
        let shared2 = Arc::clone(shared);
        let read_slot = Arc::clone(&read_slot);
        let gate = Arc::clone(&gate);
        let torn = Arc::clone(&torn_down_requested);
        thread::Builder::new()
            .name(format!("sot-pipe-r-{conn_id}"))
            .spawn(move || {
                if !gate.wait_for_start() {
                    return;
                }
                reader_loop(read_slot, conn_id, shared2, id, torn)
            })
    };
    let reader_jh = match reader_jh {
        Ok(jh) => jh,
        Err(e) => {
            recycle_instance(shared, id, raw);
            report_registration_failure(shared, "reader thread spawn failed", e);
            return;
        }
    };

    let writer_jh = {
        let shared2 = Arc::clone(shared);
        let write_slot = Arc::clone(&write_slot);
        let outbound = Arc::clone(&outbound);
        let gate = Arc::clone(&gate);
        let torn = Arc::clone(&torn_down_requested);
        thread::Builder::new()
            .name(format!("sot-pipe-w-{conn_id}"))
            .spawn(move || {
                if !gate.wait_for_start() {
                    return;
                }
                writer_loop(write_slot, conn_id, rx, shared2, id, outbound, torn)
            })
    };
    let writer_jh = match writer_jh {
        Ok(jh) => jh,
        Err(e) => {
            // The reader is spawned but still gated -- abort makes its
            // `wait_for_start` return `false` immediately, so joining it
            // here (NOT through the reaper: it was never registered) is
            // bounded and it never touches `raw`.
            gate.abort();
            reader_jh.join().ok();
            recycle_instance(shared, id, raw);
            report_registration_failure(shared, "writer thread spawn failed", e);
            return;
        }
    };

    let conn = ConnHandle {
        raw,
        registry_id: id,
        read_slot,
        write_slot,
        outbound,
        sender: tx,
        reader_jh,
        writer_jh,
        torn_down_requested,
    };
    shared.conns.lock().unwrap().insert(conn_id, conn);
    // RELIABLE, not best-effort: retries until the consumer actually has
    // room, so the gate below can never open onto a connection the
    // consumer was never told exists.
    send_lifecycle_event(shared, TransportEvent::Accepted(conn_id));
    gate.open(); // ONLY now may the reader/writer threads touch the pipe.
}

/// Attempt to deliver one `Bytes` event, retrying against a full `events`
/// channel for up to [`BYTES_ABANDON_AFTER`] — abandoning delivery
/// (returning `false`) once that bound elapses OR the moment `slot` has
/// been independently cancelled (`Bytes` is the one event kind allowed to
/// be abandoned, but abandoning it always forces this connection closed
/// with a guaranteed `Closed` — see [`reader_loop`]). Returns `false`
/// also if the consumer is gone entirely (channel disconnected).
fn deliver_bytes(
    shared: &Arc<ServerShared>,
    conn_id: ConnId,
    bytes: Vec<u8>,
    slot: &IoSlot,
) -> bool {
    let mut item = TransportEvent::Bytes(conn_id, bytes);
    let deadline = Instant::now() + BYTES_ABANDON_AFTER;
    loop {
        match shared.events_tx.try_send(item) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(v)) => {
                item = v;
                if slot.is_closing() || Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(EVENTS_RETRY_INTERVAL);
            }
        }
    }
}

/// Classify a terminal `ReadFile`/`WriteFile`/`GetOverlappedResult`
/// error: the disconnect family (see [`is_disconnect_family`]) plus this
/// side's own cancellation is `Eof`; anything else is `Error`. A
/// SUCCESSFUL zero-byte read is handled separately, in [`reader_loop`]
/// itself — it never reaches this function.
fn classify_terminal_error(e: std::io::Error) -> ClosedReason {
    match e.raw_os_error() {
        Some(c) if c == ERROR_OPERATION_ABORTED as i32 || is_disconnect_family(c) => {
            ClosedReason::Eof
        }
        _ => ClosedReason::Error(e.to_string()),
    }
}

/// One connection's read side: at most one outstanding `ReadFile` at a
/// time. On any terminal condition this thread does NOT touch `conns` or
/// join anything itself — it only [`request_teardown`]s and returns.
fn reader_loop(
    slot: Arc<IoSlot>,
    conn_id: ConnId,
    shared: Arc<ServerShared>,
    registry_id: u64,
    torn_down_requested: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; READ_BUF_LEN];
    let reason = loop {
        let result = slot.submit_and_wait_registered(
            &shared.instances,
            registry_id,
            |h, ov| unsafe {
                ReadFile(h, buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut(), ov)
            },
            |_| false,
        );
        match result {
            // A SUCCESSFUL zero-byte completion is not EOF — Microsoft
            // documents it as a legitimate outcome of the peer issuing
            // its own zero-byte write. Just read again.
            Ok(0) => continue,
            Ok(n) => {
                if !deliver_bytes(&shared, conn_id, buf[..n as usize].to_vec(), &slot) {
                    break ClosedReason::Error(format!(
                        "events channel saturated for longer than {BYTES_ABANDON_AFTER:?}; Bytes delivery abandoned"
                    ));
                }
            }
            Err(e) if is_completion_unproven(&e) => {
                // Codex round-4 finding 2: leak this slot and the
                // in-flight read buffer forever rather than let either
                // be freed/reused while the kernel might still write
                // into them -- see `CompletionUnproven`'s own doc.
                std::mem::forget(Arc::clone(&slot));
                std::mem::forget(buf);
                break ClosedReason::Error(
                    "a pending read's completion could not be affirmatively observed; its \
                     buffer was leaked rather than risk a use-after-free"
                        .to_string(),
                );
            }
            Err(e) => break classify_terminal_error(e),
        }
    };
    request_teardown(&shared, conn_id, &torn_down_requested, reason);
}

/// One connection's write side: drains queued sends in order, one
/// outstanding `WriteFile` at a time, reliably emitting `Sent` for
/// marker-tagged sends once the OS reports the write physically complete,
/// and releasing its outbound-budget reservation once that write RETURNS
/// either way. Exits when its channel disconnects (the reaper dropped the
/// sender) or its current write is cancelled/fails — a write failure
/// [`request_teardown`]s directly rather than merely exiting, so a
/// write-side failure the reader never independently notices still gets
/// the connection torn down. Never touches `shared.conns` — teardown is
/// always the reaper's.
fn writer_loop(
    slot: Arc<IoSlot>,
    conn_id: ConnId,
    rx: Receiver<WriteCmd>,
    shared: Arc<ServerShared>,
    registry_id: u64,
    outbound: Arc<OutboundBudget>,
    torn_down_requested: Arc<AtomicBool>,
) {
    while let Ok(cmd) = rx.recv() {
        let len = cmd.bytes.len();
        let result = slot.submit_and_wait_registered(
            &shared.instances,
            registry_id,
            |h, ov| unsafe {
                WriteFile(h, cmd.bytes.as_ptr(), cmd.bytes.len() as u32, std::ptr::null_mut(), ov)
            },
            |_| false,
        );
        outbound.release(len);
        match result {
            Ok(_) => {
                if let Some(marker) = cmd.marker {
                    send_lifecycle_event(&shared, TransportEvent::Sent(conn_id, marker));
                }
            }
            Err(e) if is_completion_unproven(&e) => {
                // Codex round-4 finding 2: leak this slot and the
                // in-flight write buffer forever -- see
                // `CompletionUnproven`'s own doc.
                std::mem::forget(Arc::clone(&slot));
                std::mem::forget(cmd.bytes);
                request_teardown(
                    &shared,
                    conn_id,
                    &torn_down_requested,
                    ClosedReason::Error(
                        "a pending write's completion could not be affirmatively observed; its \
                         buffer was leaked rather than risk a use-after-free"
                            .to_string(),
                    ),
                );
                break;
            }
            Err(e) => {
                request_teardown(
                    &shared,
                    conn_id,
                    &torn_down_requested,
                    classify_terminal_error(e),
                );
                break;
            }
        }
    }
}

/// The client side of one voyage's pipe: `read`/`write_all` are blocking
/// from the calling thread's own perspective, but `PipeClient` is `Sync`
/// (via the same [`IoSlot`] the server uses, rejecting a concurrent
/// same-direction submission rather than corrupting one) — a second
/// thread may call [`PipeClient::cancel`] at any time to unblock whichever
/// of the two is currently in flight.
pub struct PipeClient {
    #[allow(dead_code)] // held for its Drop (closes the pipe handle)
    handle: OwnedHandle,
    raw: SendableHandle,
    read_slot: IoSlot,
    write_slot: IoSlot,
}

impl std::fmt::Debug for PipeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeClient").finish_non_exhaustive()
    }
}

/// Maps [`crate::challenge::SidAuthOutcome`] to this module's own
/// `Result` — the exact logic [`connect_voyage_pipe`] runs, pulled out so
/// it is directly unit-testable (U1a Codex round-1, minor cluster: "a
/// constructor-level failure-mapping test") without needing an OS-level
/// SID mismatch or OS-call failure through a live pipe, neither of which
/// is constructible in CI (a genuine Foreign result needs a second real
/// account; the ADR itself scopes that proof to step 7's real-machine
/// suite).
fn map_sid_auth_outcome(outcome: crate::challenge::SidAuthOutcome) -> Result<(), PipeError> {
    match outcome {
        crate::challenge::SidAuthOutcome::Authenticated(_) => Ok(()),
        crate::challenge::SidAuthOutcome::Foreign => Err(PipeError::Foreign),
        crate::challenge::SidAuthOutcome::Undetermined => Err(PipeError::Undetermined),
    }
}

/// Connect to `\\.\pipe\sot-voyage-<voyage_id>` AND authenticate the
/// server behind it (ADR 0041 Lifecycle "The challenge", steps 1-3 —
/// U1a) before handing the connection back — the shared,
/// step-5-client-facing constructor every ordinary caller (tests, the
/// e2e harness, and any future mgmt/attach client) uses. A pipe's DACL is
/// directional (governs who may CONNECT, not who MADE the object), so a
/// raw successful `CreateFileW` here proves nothing about who is on the
/// other end; this function runs
/// [`crate::challenge::authenticate_server`] (identify the peer process,
/// compare its token-user SID to this account's — NOT the full five-step
/// `challenge()`, which additionally binds a reply's own pid/creation to
/// this connection and needs a lane-specific request to get one) before
/// returning `Ok(_)` — the MINIMAL SAFE CALL for a connection whose lane
/// is not yet known here (the caller's own first frame —
/// `status`/`probe`/`shutdown` for mgmt, `hello` for attach — decides
/// that, and this function must not consume either by sending a
/// lane-specific request of its own; see `authenticate_server`'s own doc
/// for why the full proof does not apply at this layer). A failed
/// authentication is a loud, typed [`PipeError::Foreign`] or
/// [`PipeError::Undetermined`] — never a silent retry. A caller that
/// needs the FULL proof (mgmt lane; the probe classifier) runs
/// `challenge::challenge` itself on top of this — see
/// `probe::RealProbeOps` for exactly that composition.
///
/// Retries `CreateFileW` (bounded, 2s total) on `ERROR_PIPE_BUSY` (all
/// instances currently connected — waits on `WaitNamedPipeW` between
/// attempts) and `ERROR_FILE_NOT_FOUND` (the server has not called `bind`
/// yet) — both are ordinary races in a healthy multi-client server, not
/// failures.
pub fn connect_voyage_pipe(voyage_id: &str) -> Result<PipeClient, PipeError> {
    let client = connect_voyage_pipe_unchallenged(voyage_id)?;
    map_sid_auth_outcome(crate::challenge::authenticate_server(&client))?;
    Ok(client)
}

/// The raw connect, with NO authentication — every step-5 client must go
/// through [`connect_voyage_pipe`] instead. `pub(crate)`, and MUST STAY
/// `pub(crate)` (U1a Codex round-1, Blocker 2): the only in-crate consumer
/// is `probe::RealProbeOps::connect`, which is itself `pub(crate)` for
/// exactly this reason — an unchallenged `PipeClient` reachable through a
/// PUBLIC type would be a public path to raw pipe I/O on an unauthenticated
/// connection, defeating this whole module's own enforcement. See
/// `probe.rs`'s module doc for why making `RealProbeOps` crate-private
/// costs nothing today (no production code instantiates it yet) and stays
/// architecturally sound once U2's classifier lands (a public function
/// in THIS crate, reachable from `sot-capsule`'s separate bin target,
/// wrapping this crate-private plumbing).
///
/// This exists ONLY for the probe classifier's own `ProbeOps::connect` (a
/// later unit, ADR 0041 "The probe"), which deliberately keeps "connect"
/// and "challenge" as two separately-observed steps — Stage B's own
/// transition table (B1-B6) is defined in terms of a raw connect outcome
/// followed by a SEPARATELY timed challenge (a bespoke deadline clamped to
/// the probe episode's remaining wall time), so folding authentication
/// into the connect itself here would collapse rows the classifier needs
/// to tell apart.
pub(crate) fn connect_voyage_pipe_unchallenged(voyage_id: &str) -> Result<PipeClient, PipeError> {
    validate_voyage_id(voyage_id)?;
    connect_named_pipe_unchallenged(pipe_name_wide(voyage_id))
}

/// ADR 0041 step 6 U2: connect to the supervisor lane's own pipe with NO
/// authentication — every real caller must run the SAME five-step
/// [`crate::challenge::challenge`] the mgmt lane's own client does (the
/// supervisor lane's security is "MUTUAL", not the weaker SID-only proof
/// [`connect_voyage_pipe`] settles for), so unlike that function this one
/// intentionally has no `_unchallenged`-free sibling here — the caller
/// composes the full challenge itself, exactly as `probe::RealProbeOps`
/// does for the mgmt lane's own unchallenged connect.
pub(crate) fn connect_supervisor_pipe_unchallenged(h: &str) -> Result<PipeClient, PipeError> {
    connect_named_pipe_unchallenged(supervisor_pipe_name_wide(h))
}

/// [`connect_named_pipe_unchallenged`]'s own total `CreateFileW` retry
/// budget — `pub`, matching [`TEARDOWN_AGGREGATE_DEADLINE`]'s own
/// convention, so a caller composing a worst-case bound over a whole
/// connect-then-challenge sequence (`supervisor.rs`'s own recovery/
/// ending watchdogs) cites the real constant instead of re-deriving the
/// same "2s" as an independent, driftable literal.
pub const PIPE_CONNECT_BOUND: Duration = Duration::from_secs(2);

/// Shared raw connect, given an already-resolved wide pipe name: retries
/// `CreateFileW` (bounded, [`PIPE_CONNECT_BOUND`] total) on
/// `ERROR_PIPE_BUSY`/`ERROR_FILE_NOT_FOUND`, exactly as
/// [`connect_voyage_pipe_unchallenged`]'s own doc describes. NO
/// authentication of any kind — every caller of either wrapper above is
/// responsible for running the OS-level identity check (and, where the
/// lane needs it, the full challenge) on top.
fn connect_named_pipe_unchallenged(name: Vec<u16>) -> Result<PipeClient, PipeError> {
    let deadline = Instant::now() + PIPE_CONNECT_BOUND;
    loop {
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if h != INVALID_HANDLE_VALUE {
            let handle = unsafe { OwnedHandle::from_raw_handle(h as RawHandle) };
            let raw = SendableHandle(h);
            let read_slot = IoSlot::new().map_err(|e| PipeError::Io {
                op: "CreateEventW(client read)",
                source: e,
            })?;
            let write_slot = IoSlot::new().map_err(|e| PipeError::Io {
                op: "CreateEventW(client write)",
                source: e,
            })?;
            return Ok(PipeClient {
                handle,
                raw,
                read_slot,
                write_slot,
            });
        }
        let err = std::io::Error::last_os_error();
        let code = err.raw_os_error();
        let retryable =
            code == Some(ERROR_PIPE_BUSY as i32) || code == Some(ERROR_FILE_NOT_FOUND as i32);
        if !retryable || Instant::now() >= deadline {
            return Err(PipeError::Io {
                op: "CreateFileW",
                source: err,
            });
        }
        if code == Some(ERROR_PIPE_BUSY as i32) {
            unsafe { WaitNamedPipeW(name.as_ptr(), 200) };
        } else {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl PipeClient {
    /// Blocking write of the whole buffer, cancellable from another
    /// thread via [`PipeClient::cancel`]. `bytes` must be non-empty and no
    /// larger than a single Win32 write can represent. A concurrent
    /// SECOND `write_all` call from another thread returns
    /// `Err(PipeError::ConcurrentSubmit)` rather than corrupting the
    /// shared `OVERLAPPED`. Named pipes complete a `WriteFile` as one
    /// atomic operation (byte-mode, no partial writes to retry-loop over).
    ///
    /// `bytes` is BORROWED from the caller — if a genuinely pending
    /// write's completion cannot be affirmatively observed
    /// ([`CompletionUnproven`]), this module cannot safely leak it on
    /// the caller's behalf (the caller may free/reuse it the instant
    /// this call returns), so it aborts the process instead — see
    /// `CompletionUnproven`'s own doc.
    pub fn write_all(&self, bytes: &[u8]) -> Result<(), PipeError> {
        if bytes.is_empty() {
            return Err(PipeError::EmptyPayload);
        }
        if bytes.len() > u32::MAX as usize {
            return Err(PipeError::PayloadTooLarge(bytes.len()));
        }
        let result = self.write_slot.submit_and_wait(
            self.raw.0,
            |ov| unsafe {
                WriteFile(
                    self.raw.0,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    std::ptr::null_mut(),
                    ov,
                )
            },
            |_| false,
        );
        if let Err(e) = &result {
            if is_completion_unproven(e) {
                eprintln!(
                    "sot-pipe: a pending client WriteFile's completion could not be affirmatively \
                     observed and its buffer is caller-owned and cannot be safely leaked; \
                     aborting the process rather than risk a use-after-free"
                );
                std::process::abort();
            }
        }
        result.map(|_| ()).map_err(map_client_io_error("WriteFile"))
    }

    /// Blocking read into `buf`, cancellable from another thread via
    /// [`PipeClient::cancel`]. `buf` must be non-empty and no larger than
    /// a single Win32 read can represent — an empty buffer would loop
    /// forever re-issuing zero-byte reads, and a buffer whose length
    /// silently narrows to zero at the `u32` Win32 boundary (exactly
    /// 4 GiB) would have the identical failure. A concurrent SECOND
    /// `read` call from another thread returns
    /// `Err(PipeError::ConcurrentSubmit)`. `Ok(0)` means the server closed
    /// its end (ordered EOF) — NEVER a successful zero-byte completion,
    /// which this method silently retries past (this transport's own
    /// `send`/`write_all` never produce one).
    ///
    /// `buf` is BORROWED from the caller — see `write_all`'s own doc for
    /// why a [`CompletionUnproven`] result here aborts the process
    /// instead of returning.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, PipeError> {
        if buf.is_empty() {
            return Err(PipeError::EmptyPayload);
        }
        if buf.len() > u32::MAX as usize {
            return Err(PipeError::PayloadTooLarge(buf.len()));
        }
        loop {
            let result = self.read_slot.submit_and_wait(
                self.raw.0,
                |ov| unsafe {
                    ReadFile(
                        self.raw.0,
                        buf.as_mut_ptr(),
                        buf.len() as u32,
                        std::ptr::null_mut(),
                        ov,
                    )
                },
                |_| false,
            );
            match result {
                Ok(0) => continue,
                Ok(n) => return Ok(n as usize),
                Err(e) if is_completion_unproven(&e) => {
                    eprintln!(
                        "sot-pipe: a pending client ReadFile's completion could not be \
                         affirmatively observed and its buffer is caller-owned and cannot be \
                         safely leaked; aborting the process rather than risk a use-after-free"
                    );
                    std::process::abort();
                }
                Err(e) if matches!(e.raw_os_error(), Some(c) if is_disconnect_family(c)) => {
                    return Ok(0)
                }
                Err(e) => return Err(map_client_io_error("ReadFile")(e)),
            }
        }
    }

    /// Cancel whatever is currently in flight on EITHER direction, from
    /// any thread — safe to call concurrently with `read`/`write_all` on
    /// another thread. A cancelled call returns `Err(PipeError::Cancelled)`,
    /// distinct from an ordered EOF, an ordinary I/O error, or
    /// `ConcurrentSubmit`.
    pub fn cancel(&self) {
        self.read_slot.cancel(self.raw.0);
        self.write_slot.cancel(self.raw.0);
    }
}

/// ADR 0041 step 6, unit U0: the voyage pipe is one of the (currently
/// one, eventually two) pipe families the same-connection challenge must
/// serve — see `challenge.rs`'s own doc for why that module depends on
/// this trait rather than on `PipeClient` by name.
impl crate::challenge::ChallengeableConnection for PipeClient {
    fn raw_handle(&self) -> HANDLE {
        self.raw.0
    }

    fn write_all(&self, bytes: &[u8]) -> std::io::Result<()> {
        PipeClient::write_all(self, bytes).map_err(pipe_error_to_io)
    }

    fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        PipeClient::read(self, buf).map_err(pipe_error_to_io)
    }

    fn cancel(&self) {
        PipeClient::cancel(self)
    }
}

/// Map a [`PipeError`] to a plain `io::Error` for the `challenge`
/// module's trait boundary, which depends on neither pipe family's own
/// error type by name. `Io` unwraps to its underlying `std::io::Error`
/// (preserving `ErrorKind` — e.g. a disconnect code); everything else
/// (`Cancelled`, the misuse variants) wraps opaquely — none of them
/// distinguishes further at the challenge's own three-way
/// Proven/Foreign/Undetermined split, which only ever asks "did this
/// fail," never which failure.
fn pipe_error_to_io(e: PipeError) -> std::io::Error {
    match e {
        PipeError::Io { source, .. } => source,
        other => std::io::Error::other(other),
    }
}

/// Shared client-side error mapping: `ERROR_OPERATION_ABORTED` becomes
/// [`PipeError::Cancelled`]; the [`ConcurrentSubmitMarker`] becomes
/// [`PipeError::ConcurrentSubmit`]; everything else is an ordinary I/O
/// failure.
fn map_client_io_error(op: &'static str) -> impl Fn(std::io::Error) -> PipeError {
    move |e| {
        if is_concurrent_submit(&e) {
            PipeError::ConcurrentSubmit
        } else if e.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) {
            PipeError::Cancelled
        } else {
            PipeError::Io { op, source: e }
        }
    }
}

/// U1a Codex round-1, minor cluster: a constructor-level failure-mapping
/// test for `connect_voyage_pipe`'s own `map_sid_auth_outcome`, proving
/// the mapping code the constructor actually runs -- not `challenge`/
/// `authenticate_server` directly, and not through a live pipe (a genuine
/// OS-level Foreign/Undetermined through a real connection needs either a
/// second real account or an unreliable timing race, neither
/// constructible deterministically in CI; see `authenticate_server_is_
/// undetermined_when_step_one_itself_fails` in the integration test for
/// the OS-call-failure case proven against a real, deliberately invalid
/// handle instead). Lives here (not in `tests/pipe_win.rs`) because
/// `map_sid_auth_outcome` is a private implementation detail with no
/// reason to be `pub` merely for testability, and a pure mapping over
/// already-constructed `SidAuthOutcome` values needs no real pipe --
/// exactly the kind of test this crate's OTHER pure-logic modules
/// (`attach_proto`, `wire`, `exchange`) already keep inline.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::challenge::{SidAuthOutcome, SidAuthenticated};

    #[test]
    fn map_sid_auth_outcome_authenticated_is_ok() {
        let outcome = SidAuthOutcome::Authenticated(SidAuthenticated { pid: 4242, created: 7 });
        assert!(map_sid_auth_outcome(outcome).is_ok());
    }

    #[test]
    fn map_sid_auth_outcome_foreign_is_the_typed_pipe_error() {
        assert!(matches!(map_sid_auth_outcome(SidAuthOutcome::Foreign), Err(PipeError::Foreign)));
    }

    #[test]
    fn map_sid_auth_outcome_undetermined_is_the_typed_pipe_error() {
        assert!(matches!(
            map_sid_auth_outcome(SidAuthOutcome::Undetermined),
            Err(PipeError::Undetermined)
        ));
    }

    // -- join_within: the ADR 0041 step 6 U1b teardown-deadline mechanism,
    // proven directly against plain `std::thread::spawn` closures this
    // module fully controls -- no real pipe needed for the shared-deadline
    // / loud-on-expiry properties themselves (`tests/pipe_win.rs` proves
    // the same mechanism composed with real workers).

    #[test]
    fn join_within_true_when_the_thread_already_finished() {
        let jh = thread::spawn(|| {});
        // Real time for the thread to actually finish before polling
        // starts -- this asserts the HAPPY path, not a race against it.
        thread::sleep(Duration::from_millis(50));
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(join_within(jh, deadline));
    }

    #[test]
    fn join_within_false_on_expiry_and_never_blocks_past_the_deadline() {
        let (tx, rx) = mpsc::channel::<()>();
        let jh = thread::spawn(move || {
            let _ = rx.recv(); // never sent: blocks until this process exits
        });
        let budget = Duration::from_millis(50);
        let deadline = Instant::now() + budget;
        let started = Instant::now();
        let ok = join_within(jh, deadline);
        let elapsed = started.elapsed();
        assert!(!ok);
        // Bounded, not merely eventually false -- the whole point of never
        // calling the blocking `JoinHandle::join`.
        assert!(
            elapsed < budget + Duration::from_secs(2),
            "took {elapsed:?} against a {budget:?} budget"
        );
        drop(tx);
    }

    #[test]
    fn one_shared_deadline_not_a_fresh_budget_per_join() {
        // Two joins against the SAME deadline: the first consumes most of
        // the budget by construction (a thread that finishes only after
        // most of it has elapsed), so the second must see a near-zero
        // REMAINING budget -- proving the deadline is shared, not reset
        // per call (ADR 0041: "each wait taking the remaining budget").
        let budget = Duration::from_millis(150);
        let deadline = Instant::now() + budget;

        let jh1 = thread::spawn(move || thread::sleep(Duration::from_millis(100)));
        assert!(join_within(jh1, deadline), "the first join should still fit its share");

        let (tx, rx) = mpsc::channel::<()>();
        let jh2 = thread::spawn(move || {
            let _ = rx.recv();
        });
        assert!(
            !join_within(jh2, deadline),
            "the second join must not get a fresh budget after the first consumed most of it"
        );
        drop(tx);
    }

    /// Codex round-2b, ruling on finding 2: the boundary test proving
    /// expiry with a GENUINELY UNFINISHED thread is terminal, even
    /// though that same thread finishes moments later -- "no acceptance
    /// after the decision". Deterministic, not a race: BOTH preconditions
    /// (the thread is still unfinished, AND the deadline has already
    /// passed) are independently confirmed BEFORE `join_within` is ever
    /// called, so this is not racing the exact expiry instant -- it
    /// proves the DECISION itself (`false`), captured once, is never
    /// revisited by the thread's later completion.
    #[test]
    fn expiry_with_a_genuinely_unfinished_thread_is_terminal_even_though_it_finishes_moments_later() {
        let (tx, rx) = mpsc::channel::<()>();
        let jh = thread::spawn(move || {
            let _ = rx.recv(); // blocks until released below, AFTER the decision is made
        });
        let budget = Duration::from_millis(30);
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !jh.is_finished(),
            "the thread must be genuinely unfinished at the deadline for this test to mean              anything -- confirmed BEFORE join_within is ever called"
        );
        let decision = join_within(jh, deadline);
        assert!(!decision, "an unfinished thread at expiry must be terminal (false)");
        // Release the thread now, strictly AFTER the decision was made --
        // it finishing here must not (and structurally cannot: `decision`
        // is a plain bool already captured) retroactively flip anything.
        drop(tx);
    }
}
