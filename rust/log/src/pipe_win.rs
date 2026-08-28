//! The ADR 0041 step-5 Windows named-pipe transport (round 3: discharges
//! the second Codex adversarial round — 5 blockers + 4 should-fixes,
//! several against round 2's own new machinery). Moves bytes over
//! `\\.\pipe\sot-voyage-<id>` and reports completions; it does not know
//! about mgmt/attach lanes, `hello`, opcodes, or checkpoints — `wire.rs`
//! owns every frame shape, and [`wire::FrameSplitter`] is what a consumer
//! of this module's `Bytes` events feeds. Still TRANSPORT ONLY: no
//! dependency on the capsule or `sot-capsule` bin, and none may be added
//! here.
//!
//! # The I/O slot: one state machine, every direction, every role
//!
//! A `Mutex<SlotState>` (`Idle` / `Pending` / `Closing`) guards an
//! address-stable, MANUAL-RESET-event-backed `OVERLAPPED` — manual, not
//! auto, because Microsoft documents overlapped pipe I/O against
//! manual-reset events. [`IoSlot::submit_and_wait`] resets and issues the
//! OS call and flips the state to `Pending` ALL UNDER ONE LOCK
//! ACQUISITION, then releases the lock before the (possibly long) wait —
//! so [`IoSlot::cancel`], which also takes that lock, can only ever
//! observe `Idle` (latch `Closing`; the next submission refuses before
//! touching the OS) or `Pending` (call `CancelIoEx`, then latch
//! `Closing`). A cancel can never miss a submission that hasn't happened
//! yet, nor land after one has already started reusing the structure.
//!
//! Round 2 left one gap here (round-3 finding 2): the same check only
//! rejected `Closing`, not `Pending` — a SECOND concurrent same-direction
//! caller (only reachable through `PipeClient`, whose `read`/`write_all`
//! take `&self`) could see `Pending`, proceed anyway, and reset/reissue
//! the SAME `OVERLAPPED` an earlier caller was still waiting on,
//! violating Microsoft's unique-OVERLAPPED/no-reuse-until-completion
//! rule. `submit_and_wait` now rejects `Pending` too, with a distinct
//! marker error (`PipeError::ConcurrentSubmit` at the `PipeClient`
//! boundary) — never touching the OS or the `OVERLAPPED` for the rejected
//! call. This is what makes `unsafe impl Sync for IoSlot` actually sound:
//! completion is now provably consumed by exactly the one thread that
//! got past this check, never a second one racing it.
//!
//! # Reaping: one thread owns every join
//!
//! A single dedicated REAPER thread (started in [`PipeServer::bind`],
//! alongside the accept thread) is the only code in this module that ever
//! removes an entry from `conns` or joins a REGISTERED connection's
//! reader/writer — for any reason. [`PipeServer::close`], a reader's own
//! natural-EOF signal, and a writer's own `WriteFile`-error signal (round
//! 3, finding 4: previously the writer just exited silently) all route
//! through [`request_teardown`], which enqueues at most ONCE per
//! connection (round-3 finding 5 — see "Bounded reaper inbox" below);
//! [`teardown_if_present`] is the one function that does the real work,
//! called exclusively from [`reaper_loop`], processing messages strictly
//! one at a time. The one correct exception: `handle_new_connection`'s
//! own partial-spawn-failure unwind directly `join`s an aborted reader
//! that was NEVER registered into `conns` at all — the reaper has nothing
//! to reap because that connection never had a lifecycle for it to know
//! about.
//!
//! Registration is ordered (a client that connects and disconnects
//! instantly can never let a reader reach the reaper before the entry
//! exists to be found): a connection's reader/writer threads spawn
//! already blocked on a [`StartGate`] and do not touch the pipe until
//! AFTER the `ConnHandle` is in the map AND `Accepted` has been RELIABLY
//! queued (see below) — round 2's version opened the gate even if the
//! best-effort `Accepted` publish had silently failed (round-3 finding 1).
//!
//! # Reliable lifecycle delivery (round-3 finding 1)
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
//! dropped), so continuing would be pure busywork. Memory-bounded by
//! construction (exactly one event is ever "in hand" being retried per
//! caller, never accumulated); time-bounded otherwise ONLY by the
//! CONSUMER's own contract — it must keep draining `events()`, which the
//! future capsule's one ordered loop does by construction. That contract
//! is this mechanism's other half; this module cannot enforce it, only
//! document it.
//!
//! ONLY `Bytes` may still be abandoned — the transport is the read-ahead
//! producer and must not let a stalled consumer grow memory without
//! bound. [`deliver_bytes`] retries for up to [`BYTES_ABANDON_AFTER`]
//! against a full channel (or until its own slot is independently
//! cancelled); abandoning always forces this ONE connection closed with a
//! GUARANTEED `Closed` (through the same reliable path), never a silent
//! stream gap with nothing to mark it.
//!
//! # Bounded reaper inbox (round-3 finding 5)
//!
//! `reaper_tx` is a bounded channel — `max_instances + `[`REAPER_INBOX_SLACK`]
//! — rather than unbounded. What makes that bound actually hold: every
//! connection carries its own `torn_down_requested: Arc<AtomicBool>`, and
//! [`request_teardown`] enqueues a `ReaperMsg` only on the `compare_exchange`
//! that WINS flipping it — an explicit `close`, the reader's own EOF
//! signal, and the writer's own error signal can all race for the same
//! connection, but at most one of them ever reaches the channel. The
//! inbox can therefore never hold more than one live `Torn` message per
//! currently-open connection (≤ `max_instances`) plus `Drop`'s own
//! `Shutdown`.
//!
//! # Continuous name hold
//!
//! An instance is never actually closed while the server lives. Once
//! `DisconnectNamedPipe`'d, a torn-down instance is RECYCLED — pushed onto
//! `AcceptState::recycled` — rather than dropped and later re-created.
//! Round 3 (finding 4) closes a gap here too: if `DisconnectNamedPipe`
//! itself fails, that instance is in an unknown state and is NOT safe to
//! hand back for a future `ConnectNamedPipe` — [`recycle_instance`] now
//! creates a FRESH replacement FIRST and only closes the failed instance
//! once the replacement is already recycled, so the name is never
//! instance-less even transiently. If no replacement can be created
//! either, that is a genuine terminal resource failure, reported the same
//! way any other one is (`AcceptError`, accept loop stopped).
//! `recycle_instance` remains the ONLY way an instance is ever set aside
//! short of the whole server's `Drop`.
//!
//! # Byte-bounded both directions
//!
//! Outbound: [`OutboundBudget`] reserves BYTES (not items) per connection,
//! including the in-flight item, released only once the write physically
//! completes. Inbound: `events_tx` is a bounded channel and `Bytes`
//! delivery is bounded as described above.
//!
//! # Deleted
//!
//! `drain_and_close` / `ClosedReason::Drained` / `ClosedReason::ServerShutdown`
//! / `cancel_read`: see round 2's history for why (unobservable through
//! safe use, or superseded by `PipeClient::cancel`). `ACCEPT_POLL_INTERVAL`
//! (round-3 finding 10): the accept loop's wait predicate is checked under
//! the SAME mutex the condvar uses, and every state change that could
//! satisfy it (`Drop`, a recycle) already `notify_all`s — a plain,
//! un-timed `wait` is correct and simpler; polling bought nothing.
//! `OutboundBudget::cap` (round-3 finding 10): it was always
//! [`OUTBOUND_BUDGET_BYTES`], never configurable, so the field is gone and
//! the constant is used directly.
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
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED,
    GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent};
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
/// connection with a guaranteed `Closed` (round-3 finding 1). Generous
/// relative to [`EVENTS_RETRY_INTERVAL`] — this is "the consumer has
/// genuinely stalled," not routine backpressure.
const BYTES_ABANDON_AFTER: Duration = Duration::from_secs(5);

/// Extra capacity on the bounded reaper inbox beyond `max_instances`
/// (round-3 finding 5) — headroom for `Drop`'s own `Shutdown` message to
/// still fit even when every live connection already has a `Torn`
/// message in flight.
const REAPER_INBOX_SLACK: usize = 4;

/// `\\.\pipe\sot-voyage-<id>`, UTF-16, NUL-terminated. Panics never: `id`
/// is validated by [`validate_voyage_id`] at every call site before this
/// runs.
fn pipe_name_wide(voyage_id: &str) -> Vec<u16> {
    wide_null(&format!(r"\\.\pipe\sot-voyage-{voyage_id}"))
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
/// before it is ever interpolated into a pipe name. `Uuid::parse_str`
/// accepts several equivalent shapes (uppercase hex, the 32-hex-digit
/// "simple" form, braced GUIDs, `urn:uuid:...`); re-rendering the parsed
/// value and requiring a BYTE-IDENTICAL match to the input pins the wire
/// to exactly one shape. Anything that fails to parse at all (path-
/// traversal shapes, wrong length, non-hex bytes) is rejected the same
/// way.
fn validate_voyage_id(voyage_id: &str) -> Result<(), PipeError> {
    match uuid::Uuid::parse_str(voyage_id) {
        Ok(u) if u.to_string() == voyage_id => Ok(()),
        _ => Err(PipeError::InvalidVoyageId(voyage_id.to_string())),
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
    /// abandoned (round-3 finding 1: always paired with this guaranteed
    /// notification, never a silent stream gap).
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
    /// Round-3 finding 2: a second same-direction `PipeClient` call (e.g.
    /// two concurrent `read`s) was rejected before it ever touched the OS
    /// or the shared `OVERLAPPED` — misuse, not a race this module
    /// resolves for the caller.
    #[error("another operation is already pending on this client's same direction")]
    ConcurrentSubmit,
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
}
// SAFETY: `ov`'s contents are only ever touched (reset, issued, or read
// via `GetOverlappedResult`) by the ONE thread that got past
// `submit_and_wait`'s `Pending` check for this slot — a second thread
// racing that check is REJECTED before touching `ov` at all (round-3
// finding 2). A cancelling thread only ever reads the slot's STABLE
// ADDRESS to hand to `CancelIoEx`, an OS-level operation on that address,
// never a Rust-level read of the struct's bytes. `state`'s `Mutex` is
// what serializes submission against both cancellation and a second
// submission attempt.
unsafe impl Send for IoSlot {}
unsafe impl Sync for IoSlot {}

fn aborted_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(ERROR_OPERATION_ABORTED as i32)
}

/// Marker error: `submit_and_wait` rejected a call because another
/// submission is already `Pending` on this exact direction (round-3
/// finding 2). Server-internal code never triggers this — each slot has
/// exactly one driving thread by construction — it exists so
/// `PipeClient`'s genuinely `Sync`, `&self`-based `read`/`write_all`
/// reject concurrent same-direction misuse with a distinct `Result`
/// instead of corrupting one shared `OVERLAPPED`.
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
    /// `BOOL` of `ConnectNamedPipe`/`ReadFile`/`WriteFile`) and block for
    /// its definitive result. Refuses WITHOUT ever calling `issue` if this
    /// slot is `Closing` (`Err(aborted)`) or already `Pending`
    /// (`Err(ConcurrentSubmitMarker)`, round-3 finding 2) — both checked
    /// under the same lock `cancel` uses, so neither race is possible.
    /// `synchronous_ok` names a synchronous-failure `GetLastError` code
    /// that actually means success (`ConnectNamedPipe`'s
    /// `ERROR_PIPE_CONNECTED`); pass `|_| false` for plain reads/writes.
    fn submit_and_wait(
        &self,
        handle: HANDLE,
        issue: impl FnOnce(*mut OVERLAPPED) -> i32,
        synchronous_ok: impl Fn(i32) -> bool,
    ) -> std::io::Result<u32> {
        {
            let mut st = self.state.lock().unwrap();
            if *st == SlotState::Closing {
                return Err(aborted_error());
            }
            if *st == SlotState::Pending {
                return Err(std::io::Error::other(ConcurrentSubmitMarker));
            }
            // Reset AND issue while holding the lock: a concurrent
            // `cancel` cannot observe a half-reset `OVERLAPPED`, and
            // cannot call `CancelIoEx` in the gap between this reset and
            // the `issue` call below.
            self.reset();
            let ok = issue(self.ptr());
            if ok == 0 {
                let err = std::io::Error::last_os_error();
                let code = err.raw_os_error().unwrap_or(0);
                if code == ERROR_IO_PENDING as i32 {
                    *st = SlotState::Pending;
                } else if synchronous_ok(code) {
                    return Ok(0);
                } else {
                    return Err(err);
                }
            } else {
                *st = SlotState::Pending;
            }
        } // lock released BEFORE the (possibly long) wait below.
        let result = wait_overlapped(handle, self.ptr());
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
    fn cancel(&self, handle: HANDLE) {
        let mut st = self.state.lock().unwrap();
        if *st == SlotState::Pending {
            unsafe { CancelIoEx(handle, self.ptr()) };
        }
        *st = SlotState::Closing;
    }

    fn is_closing(&self) -> bool {
        *self.state.lock().unwrap() == SlotState::Closing
    }
}

impl Drop for IoSlot {
    fn drop(&mut self) {
        unsafe { CloseHandle((*self.ov.get()).hEvent) };
    }
}

/// Block for the definitive result of an overlapped op already submitted
/// on `handle`/`ov`.
fn wait_overlapped(handle: HANDLE, ov: *const OVERLAPPED) -> std::io::Result<u32> {
    let mut transferred: u32 = 0;
    let ok = unsafe { GetOverlappedResult(handle, ov, &mut transferred, 1) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
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
/// always [`OUTBOUND_BUDGET_BYTES`] (round-3 finding 10: not configurable,
/// so no field for it).
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

/// One live connection's threads, handles, and slots — owned by the
/// `conns` map for the connection's whole life; removed and torn down
/// exclusively by [`teardown_if_present`], called exclusively from
/// [`reaper_loop`].
struct ConnHandle {
    owned: OwnedHandle,
    raw: SendableHandle,
    read_slot: Arc<IoSlot>,
    write_slot: Arc<IoSlot>,
    outbound: Arc<OutboundBudget>,
    sender: Sender<WriteCmd>,
    reader_jh: JoinHandle<()>,
    writer_jh: JoinHandle<()>,
    /// At-most-once teardown gate shared with the reader/writer threads
    /// (round-3 finding 5) — see [`request_teardown`].
    torn_down_requested: Arc<AtomicBool>,
}

/// Accept-loop state shared with [`PipeServer`]'s public methods and
/// `Drop`.
struct AcceptState {
    /// The accept loop should stop (and, once observed, HAS stopped)
    /// accepting new connections — set either by `PipeServer::drop` or by
    /// a persistent resource failure the accept loop reported via
    /// `AcceptError` (round-3 finding 10: this name no longer conflates
    /// "the server is being dropped" with "the accept loop died on its
    /// own" — see `ServerShared::dropping` for the former). EXISTING
    /// connections are unaffected either way.
    accept_stopping: bool,
    /// Instances successfully created and currently held (recycled or
    /// live) for this pipe name (<= `max_instances`): incremented when a
    /// creation attempt is about to run, decremented if that attempt
    /// fails, so it always reflects instances this server actually holds
    /// rather than a permanently-climbing attempt counter (round-3
    /// finding 10: the prior doc claimed pure monotonicity, which the
    /// failure-path decrement already contradicted).
    created: u32,
    /// Disconnected, ready-to-relisten instances. Popped by the accept
    /// loop in preference to creating a fresh instance.
    recycled: VecDeque<OwnedHandle>,
    /// The accept loop's currently in-flight `ConnectNamedPipe` attempt,
    /// if any — consulted by `Drop` so it can [`IoSlot::cancel`] exactly
    /// the operation that's actually pending.
    current: Option<(SendableHandle, Arc<IoSlot>)>,
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
    /// Set exactly once, by `PipeServer::drop`, BEFORE it sends
    /// `ReaperMsg::Shutdown` — the one escape for
    /// [`send_lifecycle_event`]'s otherwise-indefinite retry loop. See the
    /// module doc's "Reliable lifecycle delivery" section.
    dropping: AtomicBool,
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
}

impl std::fmt::Debug for PipeServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeServer").finish_non_exhaustive()
    }
}

impl PipeServer {
    /// Create `\\.\pipe\sot-voyage-<voyage_id>` (squat-detected via
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` on this, its first instance,
    /// created synchronously so a squat is a loud, immediate `bind`
    /// failure) and start the reaper and accept threads. `max_instances`
    /// must be in Win32's own documented `1..=255` range.
    pub fn bind(voyage_id: &str, max_instances: u32) -> Result<Self, PipeError> {
        validate_voyage_id(voyage_id)?;
        if !(1..=255).contains(&max_instances) {
            return Err(PipeError::InvalidMaxInstances);
        }
        let name = pipe_name_wide(voyage_id);
        let first =
            create_pipe_instance(&name, true, max_instances).map_err(|e| PipeError::Io {
                op: "CreateNamedPipeW(first instance)",
                source: e,
            })?;

        let (events_tx, events_rx) = mpsc::sync_channel(EVENTS_CHANNEL_CAP);
        let (reaper_tx, reaper_rx) =
            mpsc::sync_channel(max_instances as usize + REAPER_INBOX_SLACK);
        let shared = Arc::new(ServerShared {
            conns: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            accept: Mutex::new(AcceptState {
                accept_stopping: false,
                created: 1,
                recycled: VecDeque::new(),
                current: None,
            }),
            accept_cv: Condvar::new(),
            reaper_tx,
            events_tx,
            max_instances,
            name,
            dropping: AtomicBool::new(false),
        });

        // Round-3 finding 3: spawn the reaper FIRST. If the accept thread
        // then fails to spawn, unwind the reaper (it has nothing queued
        // yet, so its own `Shutdown` drain is instant) rather than leave
        // it running forever with no accept thread able to feed it. No
        // `JoinHandle` is ever dropped while its thread could still run.
        let reaper_jh = thread::Builder::new()
            .name("sot-pipe-reaper".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || reaper_loop(shared, reaper_rx)
            });
        let reaper_jh = match reaper_jh {
            Ok(jh) => jh,
            Err(e) => {
                return Err(PipeError::Io {
                    op: "spawn reaper thread",
                    source: e,
                })
            }
        };

        let accept_jh = thread::Builder::new()
            .name("sot-pipe-accept".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || accept_loop(shared, first)
            });
        let accept_jh = match accept_jh {
            Ok(jh) => jh,
            Err(e) => {
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
    /// enqueues the request at most once (round-3 finding 5) for the
    /// reaper thread; completion is observed as [`TransportEvent::Closed`].
    /// A no-op if `conn_id` is already gone or already has a teardown in
    /// flight.
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

impl Drop for PipeServer {
    /// Cancel a pending accept (if any) and join the accept thread; latch
    /// [`ServerShared::dropping`] (the lifecycle-delivery escape hatch);
    /// tell the reaper to drain and tear down every remaining connection,
    /// then join it. Every thread this module ever spawned is joined by
    /// the time this returns.
    fn drop(&mut self) {
        {
            let mut st = self.shared.accept.lock().unwrap();
            st.accept_stopping = true;
            if let Some((h, slot)) = st.current.take() {
                slot.cancel(h.0);
            }
        }
        self.shared.accept_cv.notify_all();
        if let Some(jh) = self.accept_jh.take() {
            jh.join().ok();
        }

        self.shared.dropping.store(true, Ordering::Release);
        let _ = self.shared.reaper_tx.send(ReaperMsg::Shutdown);
        if let Some(jh) = self.reaper_jh.take() {
            jh.join().ok();
        }
    }
}

/// Set `inst` aside for reuse rather than closing it. `DisconnectNamedPipe`
/// resets it to the listening state; on success it is pushed onto
/// `AcceptState::recycled`. On FAILURE (round-3 finding 4), `inst` is in
/// an unknown state and unsafe to reuse — a fresh replacement instance is
/// created and recycled FIRST, and only then is the failed instance
/// closed, so the name is never instance-less even transiently. If no
/// replacement can be created either, that is a genuine terminal resource
/// failure (`AcceptError`, accept loop stopped) — there is nothing else
/// this function can safely do if it cannot keep the name held.
fn recycle_instance(shared: &Arc<ServerShared>, inst: OwnedHandle) {
    let disconnected = unsafe { DisconnectNamedPipe(inst.as_raw_handle() as HANDLE) } != 0;
    if disconnected {
        shared.accept.lock().unwrap().recycled.push_back(inst);
        shared.accept_cv.notify_all();
        return;
    }
    match create_pipe_instance(&shared.name, false, shared.max_instances) {
        Ok(replacement) => {
            shared
                .accept
                .lock()
                .unwrap()
                .recycled
                .push_back(replacement);
            shared.accept_cv.notify_all();
            drop(inst); // NOW safe: the replacement already holds the name.
        }
        Err(e) => {
            shared.accept.lock().unwrap().accept_stopping = true;
            send_lifecycle_event(
                shared,
                TransportEvent::AcceptError(format!(
                    "DisconnectNamedPipe failed on a torn-down instance and no replacement could be created: {e}"
                )),
            );
            shared.accept_cv.notify_all();
            drop(inst);
        }
    }
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

/// Request teardown for `conn_id`, at most once (round-3 finding 5):
/// every caller (`PipeServer::close`, a reader's own EOF signal, a
/// writer's own `WriteFile`-error signal) races the SAME connection's
/// `flag` via `compare_exchange`; only the winner enqueues a
/// [`ReaperMsg`], so repeated or concurrent requests can never grow the
/// reaper's bounded inbox past one entry per connection.
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
/// failed) — round-3 finding 4: `Accepted` then an immediate
/// `Closed(Error(..))`, both via the reliable path, so the consumer's own
/// per-connection bookkeeping is created and discarded cleanly rather
/// than never learning this connection existed. Not terminal to the
/// accept loop — the next connection attempt is unaffected.
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
    conn.read_slot.cancel(conn.raw.0);
    conn.write_slot.cancel(conn.raw.0);
    drop(conn.sender); // unblocks a writer idle-waiting on `recv` with nothing queued
    conn.reader_jh.join().ok();
    conn.writer_jh.join().ok();
    recycle_instance(shared, conn.owned);
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

/// Obtain the next instance to listen on: a recycled one in preference to
/// creating a fresh one, blocking at the instance cap with nothing
/// recycled yet. Waits on the plain condvar (round-3 finding 10: every
/// state change that could satisfy this predicate — `Drop`, a recycle —
/// already `notify_all`s, so polling bought nothing). `None` means: stop
/// accepting — either shutdown, or a persistent creation failure already
/// reported via `TransportEvent::AcceptError`.
fn obtain_instance(shared: &Arc<ServerShared>) -> Option<OwnedHandle> {
    loop {
        let mut st = shared.accept.lock().unwrap();
        if st.accept_stopping {
            return None;
        }
        if let Some(h) = st.recycled.pop_front() {
            return Some(h);
        }
        if st.created < shared.max_instances {
            st.created += 1;
            drop(st);
            match create_pipe_instance(&shared.name, false, shared.max_instances) {
                Ok(h) => return Some(h),
                Err(e) => {
                    let mut st = shared.accept.lock().unwrap();
                    st.created -= 1;
                    st.accept_stopping = true;
                    drop(st);
                    send_lifecycle_event(shared, TransportEvent::AcceptError(e.to_string()));
                    shared.accept_cv.notify_all();
                    return None;
                }
            }
        }
        st = shared.accept_cv.wait(st).unwrap();
        drop(st);
    }
}

/// The accept loop, one dedicated thread for the server's whole life.
/// `first_instance` is the already-created (with
/// `FILE_FLAG_FIRST_PIPE_INSTANCE`) instance from `bind`; every later
/// instance comes from [`obtain_instance`] (recycled or freshly created,
/// never carrying that flag).
fn accept_loop(shared: Arc<ServerShared>, first_instance: OwnedHandle) {
    let mut pending_instance = Some(first_instance);
    loop {
        let inst = match pending_instance.take() {
            Some(h) => h,
            None => match obtain_instance(&shared) {
                Some(h) => h,
                None => return,
            },
        };

        let raw = SendableHandle(inst.as_raw_handle() as HANDLE);
        let slot = match IoSlot::new() {
            Ok(s) => Arc::new(s),
            Err(e) => {
                // Round-3 finding 4: this used to recycle and silently
                // `continue` forever. A slot-creation failure here is a
                // resource failure exactly like `create_pipe_instance`'s
                // own — terminalize the same way rather than spin.
                recycle_instance(&shared, inst);
                shared.accept.lock().unwrap().accept_stopping = true;
                send_lifecycle_event(
                    &shared,
                    TransportEvent::AcceptError(format!("IoSlot::new (accept): {e}")),
                );
                shared.accept_cv.notify_all();
                return;
            }
        };

        {
            let mut st = shared.accept.lock().unwrap();
            if st.accept_stopping {
                drop(st);
                recycle_instance(&shared, inst);
                return;
            }
            st.current = Some((raw, Arc::clone(&slot)));
        }
        let connect_result = slot.submit_and_wait(
            raw.0,
            |ov| unsafe { ConnectNamedPipe(raw.0, ov) },
            |code| code == ERROR_PIPE_CONNECTED as i32,
        );
        shared.accept.lock().unwrap().current = None;

        match connect_result {
            // A client that connects and vanishes fast enough can make
            // `ConnectNamedPipe` report the completion as an ERROR rather
            // than success or `ERROR_PIPE_CONNECTED`. The only outcome
            // that is EVER this module's own doing is
            // `ERROR_OPERATION_ABORTED`, raised exclusively by
            // `IoSlot::cancel` — every other error still means a real
            // client showed up, so it is registered anyway and the
            // reader's own first `ReadFile` discovers/classifies whatever
            // actually happened.
            Ok(_) => handle_new_connection(&shared, inst, raw, slot),
            Err(e) if e.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) => {
                recycle_instance(&shared, inst);
                if shared.accept.lock().unwrap().accept_stopping {
                    return;
                }
                // Not actually shutting down -- nothing else ever cancels
                // this slot, but stay defensive and just try again.
            }
            Err(_e) => handle_new_connection(&shared, inst, raw, slot),
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
/// rather than silently dropping the client (round-3 finding 4).
fn handle_new_connection(
    shared: &Arc<ServerShared>,
    inst: OwnedHandle,
    raw: SendableHandle,
    read_slot: Arc<IoSlot>,
) {
    let write_slot = match IoSlot::new() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            recycle_instance(shared, inst);
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
                reader_loop(raw, read_slot, conn_id, shared2, torn)
            })
    };
    let reader_jh = match reader_jh {
        Ok(jh) => jh,
        Err(e) => {
            recycle_instance(shared, inst);
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
                writer_loop(raw, write_slot, conn_id, rx, shared2, outbound, torn)
            })
    };
    let writer_jh = match writer_jh {
        Ok(jh) => jh,
        Err(e) => {
            // The reader is spawned but still gated -- abort makes its
            // `wait_for_start` return `false` immediately, so joining it
            // here (NOT through the reaper: it was never registered) is
            // bounded and it never touches `raw`/`inst`.
            gate.abort();
            reader_jh.join().ok();
            recycle_instance(shared, inst);
            report_registration_failure(shared, "writer thread spawn failed", e);
            return;
        }
    };

    let conn = ConnHandle {
        owned: inst,
        raw,
        read_slot,
        write_slot,
        outbound,
        sender: tx,
        reader_jh,
        writer_jh,
        torn_down_requested,
    };
    shared.conns.lock().unwrap().insert(conn_id, conn);
    // RELIABLE, not best-effort (round-3 finding 1): retries until the
    // consumer actually has room, so the gate below can never open onto a
    // connection the consumer was never told exists.
    send_lifecycle_event(shared, TransportEvent::Accepted(conn_id));
    gate.open(); // ONLY now may the reader/writer threads touch the pipe.
}

/// Attempt to deliver one `Bytes` event, retrying against a full `events`
/// channel for up to [`BYTES_ABANDON_AFTER`] — abandoning delivery
/// (returning `false`) once that bound elapses OR the moment `slot` has
/// been independently cancelled (round-3 finding 1: `Bytes` is the one
/// event kind allowed to be abandoned, but abandoning it always forces
/// this connection closed with a guaranteed `Closed` — see
/// [`reader_loop`]). Returns `false` also if the consumer is gone
/// entirely (channel disconnected).
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
/// error: the recognized disconnect family (broken/unconnected pipe, the
/// pipe being closed, or this side's own cancellation) is `Eof`; anything
/// else is `Error`. A SUCCESSFUL zero-byte read is handled separately, in
/// [`reader_loop`] itself — it never reaches this function.
fn classify_terminal_error(e: std::io::Error) -> ClosedReason {
    match e.raw_os_error() {
        Some(c)
            if c == ERROR_BROKEN_PIPE as i32
                || c == ERROR_PIPE_NOT_CONNECTED as i32
                || c == ERROR_NO_DATA as i32
                || c == ERROR_OPERATION_ABORTED as i32 =>
        {
            ClosedReason::Eof
        }
        _ => ClosedReason::Error(e.to_string()),
    }
}

/// One connection's read side: at most one outstanding `ReadFile` at a
/// time. On any terminal condition this thread does NOT touch `conns` or
/// join anything itself — it only [`request_teardown`]s and returns.
fn reader_loop(
    handle: SendableHandle,
    slot: Arc<IoSlot>,
    conn_id: ConnId,
    shared: Arc<ServerShared>,
    torn_down_requested: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; READ_BUF_LEN];
    let reason = loop {
        let result = slot.submit_and_wait(
            handle.0,
            |ov| unsafe {
                ReadFile(
                    handle.0,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    std::ptr::null_mut(),
                    ov,
                )
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
    handle: SendableHandle,
    slot: Arc<IoSlot>,
    conn_id: ConnId,
    rx: Receiver<WriteCmd>,
    shared: Arc<ServerShared>,
    outbound: Arc<OutboundBudget>,
    torn_down_requested: Arc<AtomicBool>,
) {
    while let Ok(cmd) = rx.recv() {
        let len = cmd.bytes.len();
        let result = slot.submit_and_wait(
            handle.0,
            |ov| unsafe {
                WriteFile(
                    handle.0,
                    cmd.bytes.as_ptr(),
                    cmd.bytes.len() as u32,
                    std::ptr::null_mut(),
                    ov,
                )
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
/// (via the same [`IoSlot`] the server uses, now rejecting a concurrent
/// same-direction submission rather than corrupting one — round-3 finding
/// 2) — a second thread may call [`PipeClient::cancel`] at any time to
/// unblock whichever of the two is currently in flight.
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

/// Connect to `\\.\pipe\sot-voyage-<voyage_id>`. Retries `CreateFileW`
/// (bounded, 2s total) on `ERROR_PIPE_BUSY` (all instances currently
/// connected — waits on `WaitNamedPipeW` between attempts) and
/// `ERROR_FILE_NOT_FOUND` (the server has not called `bind` yet) — both
/// are ordinary races in a healthy multi-client server, not failures.
pub fn connect_voyage_pipe(voyage_id: &str) -> Result<PipeClient, PipeError> {
    validate_voyage_id(voyage_id)?;
    let name = pipe_name_wide(voyage_id);
    let deadline = Instant::now() + Duration::from_secs(2);
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
    /// shared `OVERLAPPED` (round-3 finding 2). Named pipes complete a
    /// `WriteFile` as one atomic operation (byte-mode, no partial writes
    /// to retry-loop over).
    pub fn write_all(&self, bytes: &[u8]) -> Result<(), PipeError> {
        if bytes.is_empty() {
            return Err(PipeError::EmptyPayload);
        }
        if bytes.len() > u32::MAX as usize {
            return Err(PipeError::PayloadTooLarge(bytes.len()));
        }
        self.write_slot
            .submit_and_wait(
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
            )
            .map(|_| ())
            .map_err(map_client_io_error("WriteFile"))
    }

    /// Blocking read into `buf`, cancellable from another thread via
    /// [`PipeClient::cancel`]. `buf` must be non-empty and no larger than
    /// a single Win32 read can represent (round-3 finding 6: an empty
    /// buffer previously looped forever re-issuing zero-byte reads, and a
    /// buffer whose length silently narrows to zero at the `u32` Win32
    /// boundary — exactly 4 GiB — had the identical failure). A
    /// concurrent SECOND `read` call from another thread returns
    /// `Err(PipeError::ConcurrentSubmit)`. `Ok(0)` means the server closed
    /// its end (ordered EOF) — NEVER a successful zero-byte completion,
    /// which this method silently retries past (this transport's own
    /// `send`/`write_all` never produce one).
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
                Err(e) if matches!(e.raw_os_error(), Some(c) if c == ERROR_BROKEN_PIPE as i32 || c == ERROR_PIPE_NOT_CONNECTED as i32 || c == ERROR_NO_DATA as i32) =>
                {
                    return Ok(0);
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
