//! The ADR 0041 step-5 Windows named-pipe transport (round 2: discharges
//! the Codex adversarial review of round 1 — 9 blockers + 2 should-fixes +
//! a deletion note, all folded in below). Moves bytes over
//! `\\.\pipe\sot-voyage-<id>` and reports completions; it does not know
//! about mgmt/attach lanes, `hello`, opcodes, or checkpoints — `wire.rs`
//! owns every frame shape, and [`wire::FrameSplitter`] is what a consumer
//! of this module's `Bytes` events feeds. Round 2 is still TRANSPORT
//! ONLY: no dependency on the capsule or `sot-capsule` bin, and none may
//! be added here.
//!
//! # The I/O slot: one state machine, every direction, every role
//!
//! Round 1's mistake (findings 1–2): auto-reset events, and `CancelIoEx`
//! called with no lock relating it to submission or reuse — a cancel
//! could land in the gap between "decided not to submit" and "actually
//! submitted," or between one iteration's cancel and the next iteration's
//! fresh submission, and every "bounded" join could still hang. [`IoSlot`]
//! is the fix, used uniformly for the accept loop's `ConnectNamedPipe`,
//! each connection's read and write directions (server AND client): a
//! `Mutex<SlotState>` (`Idle` / `Pending` / `Closing`) guards an
//! address-stable, MANUAL-RESET-event-backed `OVERLAPPED` — manual, not
//! auto, because Microsoft documents overlapped pipe I/O against
//! manual-reset events and warns auto-reset can hang `GetOverlappedResult`.
//! [`IoSlot::submit_and_wait`] resets and issues the OS call and flips the
//! state to `Pending` ALL UNDER ONE LOCK ACQUISITION, then releases the
//! lock before the (possibly long) wait — so [`IoSlot::cancel`], which
//! also takes that lock, can only ever observe `Idle` (nothing to cancel;
//! latch `Closing` so the NEXT submission attempt refuses before touching
//! the OS at all), or `Pending` (call `CancelIoEx`, then latch `Closing`).
//! There is no window in which a cancel can miss a submission that hasn't
//! happened yet, and no window in which a submission can proceed after a
//! cancel has already started. Completion (`GetOverlappedResult`) is
//! always consumed by exactly the one thread that called
//! `submit_and_wait` — a slot is never read by two threads at once, only
//! ever cancelled from a second thread while the first blocks in the wait.
//! `unsafe impl Sync for IoSlot` is sound on exactly this basis: every
//! access to the `OVERLAPPED`'s CONTENTS happens either inside the lock
//! (reset, issue) or is the one thread's own wait outside it; a second
//! thread's `cancel` only ever reads the STABLE ADDRESS to hand to
//! `CancelIoEx`, never the struct's bytes.
//!
//! # Reaping: one thread owns every join
//!
//! Round 1's reader threads tried to remove their own connection from the
//! map and hand their own `JoinHandle` to a growing `retired` list for
//! `Drop` to collect later — unbounded growth under connection churn, and
//! a genuine race where `Drop` could observe an empty `conns` map and an
//! as-yet-unpushed `retired` list and return while that reader was still
//! running (finding 5). Round 2 deletes all of that: a single dedicated
//! REAPER thread (started in [`PipeServer::bind`], alongside the accept
//! thread) is the ONLY code in this module that ever removes an entry
//! from `conns` or joins a connection's reader/writer — for ANY reason.
//! [`PipeServer::close`] and a reader's own natural-EOF signal both just
//! send a [`ReaperMsg`] and return; [`teardown_if_present`] is the one
//! function that does the real work, called exclusively from
//! [`reaper_loop`], which processes messages strictly one at a time. There
//! is no other path that can remove a connection, so there is no race to
//! have.
//!
//! Registration is similarly ordered (finding 4): a connection's reader
//! and writer threads are spawned already blocked on a [`StartGate`] and
//! do not touch the pipe at all until AFTER the `ConnHandle` is in the map
//! and `Accepted` has been published — a client that connects and
//! disconnects instantly can never let a reader reach the reaper before
//! the entry exists to be found.
//!
//! # Continuous name hold (finding 7)
//!
//! An instance is never actually closed while the server lives. Once
//! `DisconnectNamedPipe`'d, a torn-down instance is RECYCLED — pushed onto
//! `AcceptState::recycled` for the accept loop to re-listen on — rather
//! than dropped and later re-created. The total instance count therefore
//! never touches zero after `bind` (which creates instance #1
//! synchronously, the only one ever marked `FILE_FLAG_FIRST_PIPE_INSTANCE`,
//! and fails loudly if that squat check fails), closing the reopened
//! squatting window round 1's test 3 missed. `recycle_instance` is the
//! ONLY way an instance is ever set aside short of the whole server's
//! `Drop`, which is the one place instances are actually closed (via
//! `AcceptState::recycled`'s own `Drop`, when the last `Arc<ServerShared>`
//! goes away).
//!
//! # Byte-bounded both directions (finding 8)
//!
//! Outbound: [`OutboundBudget`] reserves BYTES (not items) per connection,
//! release only after the write physically completes — including the
//! in-flight item, per the ADR's own accounting shape. Inbound: the
//! reader is the read-ahead producer, so it owns the bound on the OTHER
//! side too — `events_tx` is a bounded channel, and a `Bytes` delivery
//! that finds it full retries with a short sleep, aborting (dropping the
//! event) the moment its own slot has been cancelled, so a stalled
//! consumer bounds memory without a worker that teardown cannot wake.
//! `Accepted`/`Sent`/`Closed` are small, rare, best-effort `try_send` —
//! only `Bytes` is the actual memory-bound-critical path.
//!
//! # Deleted (finding 6, finding 12)
//!
//! `drain_and_close` and `ClosedReason::Drained`: a synchronous drain
//! joined an uncancelled writer with no way for a concurrent `close` to
//! reach it (the entry was already gone from the map), and `PipeServer`
//! was never `Sync` besides. U2's shutdown-ack rule is: enqueue the ack
//! with a marker, wait for `Sent(marker)`, then `close` — the ordered
//! writer loop never blocks on a peer. `ClosedReason::ServerShutdown`:
//! unobservable through safe use (its `Closed` would be generated during
//! `Drop`, while the only receiver is a field of the object being
//! dropped), so `Drop`'s own teardown pass emits no event at all instead
//! of constructing a reason nobody could ever see. `cancel_read`: replaced
//! by [`PipeClient::cancel`], which needs `PipeClient` genuinely `Sync`
//! (via the same `IoSlot`) to be safe to call from a second thread at all,
//! and covers writes too — step 6's mgmt challenge needs bounded reads
//! AND bounded writes.
//!
//! # Visibility
//!
//! Every type below is `pub`, not `pub(crate)` — `tests/pipe_win.rs` is a
//! separate integration-test crate (needed for the same structural reason
//! `tests/conpty.rs` is), and an integration test can only ever reach a
//! library's `pub` items.

#![cfg(windows)]

use std::cell::UnsafeCell;
use std::collections::{HashMap, VecDeque};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_OPERATION_ABORTED,
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent};

/// Bound on one outstanding overlapped `ReadFile` (ADR 0041: "the transport
/// just must not read unboundedly ahead" — the consumer's lockstep protocol
/// bounds logical inbound; this is only the raw-byte ceiling per read).
const READ_BUF_LEN: usize = 65_536;

/// Per-connection outbound byte budget (finding 8): the caller's own
/// enqueued-but-not-yet-physically-written bytes, INCLUDING whatever the
/// writer currently has in flight, may never exceed this. Sized to the
/// same order of magnitude as the ADR's own "4 MiB per-watcher queue"
/// figure — not a literal citation of it (that number bounds a DIFFERENT
/// queue, the future capsule's checkpoint/output transfer), just a
/// consistent, documented order of magnitude for this transport's own
/// per-connection ceiling.
const OUTBOUND_BUDGET_BYTES: usize = 4 * 1024 * 1024;

/// The `events()` channel's item capacity (finding 8, inbound side): sized
/// so that even a run of maximum-size `Bytes` deliveries (`READ_BUF_LEN`
/// each) caps total buffered inbound at roughly the same order of
/// magnitude as [`OUTBOUND_BUDGET_BYTES`] — `Accepted`/`Sent`/`Closed`
/// payloads are tiny by comparison and do not change this arithmetic.
const EVENTS_CHANNEL_CAP: usize = OUTBOUND_BUDGET_BYTES / READ_BUF_LEN;

/// How long a stalled `Bytes` delivery sleeps between retries against a
/// full `events` channel before checking whether its own connection has
/// since been cancelled (see [`deliver_bytes`]).
const EVENTS_RETRY_INTERVAL: Duration = Duration::from_millis(20);

/// How long the accept loop waits on its condvar between checks of
/// `shutting_down` / `recycled` / `created` while blocked at the instance
/// cap with nothing recycled yet — a poll rather than a plain `wait`
/// specifically so `PipeServer::drop` (which sets `shutting_down` and
/// notifies, but cannot itself hand back a *recycled instance* out of
/// thin air) is bounded by this interval rather than needing a spurious
/// extra recycle to wake the loop.
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// `\\.\pipe\sot-voyage-<id>`, UTF-16, NUL-terminated. Panics never: `id`
/// is validated by [`validate_voyage_id`] at every call site before this
/// runs, so the interpolation is always onto a canonical, hyphen-and-hex-
/// only string.
fn pipe_name_wide(voyage_id: &str) -> Vec<u16> {
    wide_null(&format!(r"\\.\pipe\sot-voyage-{voyage_id}"))
}

/// NUL-terminated UTF-16 for an arbitrary Rust string. A small, deliberate
/// duplicate of `conpty.rs`'s and `fsutil.rs`'s own private copies of this
/// exact helper (Codex review, finding 12: the only duplication here is
/// this three-line leaf helper, and sharing it would add machinery without
/// value under this crate's existing leaf-helper rule).
fn wide_null(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// The voyage id is validated as a canonical RFC 4122 UUID — lowercase,
/// hyphenated, the exact form [`uuid::Uuid`]'s own `Display` produces —
/// before it is ever interpolated into a pipe name. `Uuid::parse_str`
/// accepts several equivalent shapes (uppercase hex, the 32-hex-digit
/// "simple" form, braced GUIDs, `urn:uuid:...`); re-rendering the parsed
/// value and requiring a BYTE-IDENTICAL match to the input is what pins
/// the wire to exactly one shape rather than silently accepting all of
/// them. Anything that fails to parse at all (path-traversal shapes,
/// wrong length, non-hex bytes) is rejected the same way. (Pipe names are
/// actually case-INsensitive on Windows, so the stricter validation here
/// is a wire-format discipline choice, not a name-collision one.)
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
/// send's `WriteFile` has PHYSICALLY completed. This module never
/// interprets a marker's value.
pub type SendMarker = u64;

/// Why a connection ended, reported once per connection on
/// [`TransportEvent::Closed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosedReason {
    /// The peer disconnected (or this side observed a broken/unconnected
    /// pipe) — detected by the connection's own reader loop, with no
    /// [`PipeServer::close`] call involved.
    Eof,
    /// [`PipeServer::close`] tore this connection down.
    Closed,
    /// An I/O error other than a recognized disconnect ended the
    /// connection's reader loop.
    Error(String),
}

/// This transport's event surface to its consumer — one connection's
/// worth of raw bytes and lifecycle, uninterpreted, plus the accept
/// loop's own terminal failure. Delivered over [`PipeServer::events`] in
/// the order this module observed them; the consumer feeds `Bytes`
/// payloads to its own [`crate::wire::FrameSplitter`] per connection.
#[derive(Debug)]
pub enum TransportEvent {
    /// A new connection accepted; `send`/`close` may now target it.
    Accepted(ConnId),
    /// Raw bytes read from a connection, in the order read. At most one
    /// `ReadFile` is ever outstanding per connection (see the module doc).
    /// Never empty (finding 10: an empty transport send is rejected at the
    /// API, and a successful zero-byte read is not surfaced as data).
    Bytes(ConnId, Vec<u8>),
    /// The `WriteFile` for a marker-tagged [`PipeServer::send`] call has
    /// physically completed — the spec-gate's write-completion signal,
    /// uninterpreted.
    Sent(ConnId, SendMarker),
    /// The connection ended; no further events for this `ConnId` follow.
    Closed(ConnId, ClosedReason),
    /// The accept loop hit a persistent, unrecoverable resource failure
    /// (finding 9) and has stopped accepting new connections FOR GOOD —
    /// existing connections are unaffected, but nothing new will ever be
    /// `Accepted` again on this server.
    AcceptError(String),
}

/// Errors this transport's own API surface can report. I/O failures deep
/// inside a background thread that end one connection are reported as
/// [`TransportEvent::Closed`], not through this type — this is only for
/// calls that fail SYNCHRONOUSLY, at the call site.
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
}

/// A raw Windows `HANDLE`, asserted `Send` AND `Sync`. `Send`: exactly one
/// owner ([`ConnHandle::owned`] / `PipeClient::handle`) ever calls
/// `CloseHandle` on it, only after every thread using a copy has stopped;
/// a named pipe's two directions are legitimately driven by two different
/// threads at once (the standard full-duplex pattern). `Sync`: the wrapped
/// value is never dereferenced as a pointer — it is an opaque OS handle,
/// passed only to `windows-sys` calls — so reading a `&SendableHandle`
/// from multiple threads at once is just reading a plain integer.
#[derive(Clone, Copy)]
struct SendableHandle(HANDLE);
unsafe impl Send for SendableHandle {}
unsafe impl Sync for SendableHandle {}

/// One I/O slot's state — see the module doc's "I/O slot" section for the
/// invariant this enum, and the lock guarding it, together enforce.
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
/// slot" section for the full soundness argument; in short, `submit_and_wait`
/// and `cancel` share one lock, so a cancel can never be "too late" for a
/// submission that hasn't happened yet, nor miss one that's already
/// pending.
struct IoSlot {
    state: Mutex<SlotState>,
    ov: UnsafeCell<OVERLAPPED>,
}
// SAFETY: `ov`'s contents are only ever touched (reset, or read via
// `GetOverlappedResult`) by the ONE thread currently inside
// `submit_and_wait` for this slot; a second thread's `cancel` only reads
// the slot's STABLE ADDRESS (via `ptr()`) to pass to `CancelIoEx`, which
// is an OS-level operation on that address, not a Rust-level read of the
// struct's bytes. `state`'s `Mutex` is what actually serializes the two
// roles against each other.
unsafe impl Send for IoSlot {}
unsafe impl Sync for IoSlot {}

fn aborted_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(ERROR_OPERATION_ABORTED as i32)
}

impl IoSlot {
    fn new() -> std::io::Result<Self> {
        // Manual-reset (bManualReset = TRUE): finding 1. Auto-reset events
        // can hang `GetOverlappedResult(..., TRUE)` per Microsoft's own
        // overlapped-I/O documentation; every reuse below explicitly
        // `ResetEvent`s instead of relying on auto-clear.
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
    /// own kernel-side signaled state, so the explicit reset is required
    /// regardless of how the struct fields are cleared.
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
    /// its definitive result. Refuses with `Err(aborted)` WITHOUT ever
    /// calling `issue` if this slot has already been [`cancel`](Self::cancel)led
    /// — see the module doc for why this check and the submission it
    /// guards must share one lock with `cancel`. `synchronous_ok` names a
    /// synchronous-failure `GetLastError` code that actually means success
    /// (`ConnectNamedPipe`'s `ERROR_PIPE_CONNECTED`); pass `|_| false` for
    /// plain reads/writes, which have no such code.
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
            // Reset AND issue while holding the lock: a concurrent
            // `cancel` cannot observe a half-reset `OVERLAPPED`, and
            // cannot call `CancelIoEx` in the gap between this reset and
            // the `issue` call below — both run inside one critical
            // section, so `cancel` either sees `Idle` (nothing pending
            // yet; latches `Closing` and this call will refuse above on
            // its NEXT attempt) or sees `Pending` (a real op to cancel).
            self.reset();
            let ok = issue(self.ptr());
            if ok == 0 {
                let err = std::io::Error::last_os_error();
                let code = err.raw_os_error().unwrap_or(0);
                if code == ERROR_IO_PENDING as i32 {
                    *st = SlotState::Pending;
                } else if synchronous_ok(code) {
                    // Completed synchronously as a recognized success
                    // shape (e.g. `ConnectNamedPipe`'s `ERROR_PIPE_CONNECTED`)
                    // — nothing was ever queued, so there is nothing to
                    // wait for.
                    return Ok(0);
                } else {
                    // Synchronous, non-pending, non-recognized failure:
                    // nothing queued; stay Idle rather than Pending.
                    return Err(err);
                }
            } else {
                // Completed synchronously as an ordinary success. Still
                // routed through `GetOverlappedResult` below for the
                // accurate transferred-byte count (Microsoft's own
                // documented idiom), so this is `Pending` too.
                *st = SlotState::Pending;
            }
        } // lock released BEFORE the (possibly long) wait below — this is
          // exactly what lets `cancel` run concurrently.
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
/// on `handle`/`ov`. `ERROR_OPERATION_ABORTED` (this op's `CancelIoEx`
/// fired) and every other I/O error surface as `Err`, undistinguished
/// here — callers classify by `raw_os_error()` where the distinction
/// matters.
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
/// either freshly created with `first = false` or — per the module doc's
/// continuous-name-hold section — RECYCLED rather than freshly created at
/// all. A fresh owner-only descriptor is built per call: `CreateNamedPipeW`
/// copies what it needs from it at creation time, and a fresh SID lookup
/// on each new-instance creation (an infrequent event, bounded by
/// `max_instances`) is cheap enough not to justify holding the descriptor
/// alive across a thread boundary.
fn create_pipe_instance(name: &[u16], first: bool, max_instances: u32) -> std::io::Result<OwnedHandle> {
    let descriptor =
        crate::fsutil::owner_protected_pipe_descriptor().map_err(|e| std::io::Error::other(e.to_string()))?;
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
    // `descriptor` drops here: `CreateNamedPipeW` already copied what it
    // needed into the new instance's own security descriptor.
}

/// Per-connection outbound byte accounting (finding 8): reserved eagerly
/// by [`PipeServer::send`] before an item is even queued, released only
/// once the writer's `submit_and_wait` for that item RETURNS (success or
/// failure) — so the budget always reflects everything not yet physically
/// finished writing, the in-flight item included.
struct OutboundBudget {
    used: Mutex<usize>,
    cap: usize,
}

impl OutboundBudget {
    fn new(cap: usize) -> Self {
        Self { used: Mutex::new(0), cap }
    }

    fn try_reserve(&self, n: usize) -> bool {
        let mut used = self.used.lock().unwrap();
        if *used + n > self.cap {
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
/// has been published (finding 4), or `abort`ed if the connection could
/// not be fully set up (finding 9's partial-spawn-failure path), in which
/// case the gated thread returns immediately, having never touched the
/// pipe.
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
}

/// Accept-loop state shared with [`PipeServer`]'s public methods and
/// `Drop`.
struct AcceptState {
    shutting_down: bool,
    /// Instances ever created for this pipe name (<= `max_instances`),
    /// MONOTONIC — never decremented. A torn-down instance is recycled
    /// (see `recycled`), never destroyed, so this count is never the
    /// thing that would need to drop back toward zero.
    created: u32,
    /// Disconnected, ready-to-relisten instances (finding 7: continuous
    /// name hold). Popped by the accept loop in preference to creating a
    /// fresh instance.
    recycled: VecDeque<OwnedHandle>,
    /// The accept loop's currently in-flight `ConnectNamedPipe` attempt,
    /// if any — consulted by `Drop` so it can [`IoSlot::cancel`] exactly
    /// the operation that's actually pending, rather than guessing.
    current: Option<(SendableHandle, Arc<IoSlot>)>,
}

/// A message to [`reaper_loop`] — the only thread that ever removes a
/// connection from `conns` or joins its threads.
enum ReaperMsg {
    /// A connection ended (natural EOF/error, or a caller's `close`).
    Torn(ConnId, ClosedReason),
    /// The server is being dropped: drain and tear down every connection
    /// still in `conns` (no `Closed` event for these — see the module
    /// doc's deletion note on `ClosedReason::ServerShutdown`), then stop.
    Shutdown,
}

struct ServerShared {
    conns: Mutex<HashMap<ConnId, ConnHandle>>,
    next_id: AtomicU64,
    accept: Mutex<AcceptState>,
    accept_cv: Condvar,
    reaper_tx: Sender<ReaperMsg>,
    events_tx: SyncSender<TransportEvent>,
    max_instances: u32,
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
/// releasing that lock. This module enforces neither half of that
/// ordering itself — it only guarantees: while a `PipeServer` is alive,
/// the pipe exists; the instant it is dropped, every instance is closed.
///
/// `max_instances` is the RAW total simultaneous pipe-instance ceiling
/// this transport enforces (`CreateNamedPipeW`'s own `nMaxInstances`) —
/// ADR 0041 requires this to already be the CALLER's combined figure
/// (subscribers plus separately bounded pre-hello/mgmt connections), not
/// merely the subscriber count; computing that combination is the
/// follow-up capsule unit's job, not this transport's.
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
    /// failure) and start the accept and reaper threads. `max_instances`
    /// must be in Win32's own documented `1..=255` range.
    pub fn bind(voyage_id: &str, max_instances: u32) -> Result<Self, PipeError> {
        validate_voyage_id(voyage_id)?;
        if !(1..=255).contains(&max_instances) {
            return Err(PipeError::InvalidMaxInstances);
        }
        let name = pipe_name_wide(voyage_id);
        let first = create_pipe_instance(&name, true, max_instances).map_err(|e| PipeError::Io {
            op: "CreateNamedPipeW(first instance)",
            source: e,
        })?;

        let (events_tx, events_rx) = mpsc::sync_channel(EVENTS_CHANNEL_CAP);
        let (reaper_tx, reaper_rx) = mpsc::channel();
        let shared = Arc::new(ServerShared {
            conns: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            accept: Mutex::new(AcceptState {
                shutting_down: false,
                created: 1,
                recycled: VecDeque::new(),
                current: None,
            }),
            accept_cv: Condvar::new(),
            reaper_tx,
            events_tx,
            max_instances,
        });

        let accept_jh = thread::Builder::new()
            .name("sot-pipe-accept".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || accept_loop(shared, name, first)
            })
            .map_err(|e| PipeError::Io { op: "spawn accept thread", source: e })?;
        let reaper_jh = thread::Builder::new()
            .name("sot-pipe-reaper".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || reaper_loop(shared, reaper_rx)
            })
            .map_err(|e| PipeError::Io { op: "spawn reaper thread", source: e })?;

        Ok(Self {
            shared,
            events_rx,
            accept_jh: Some(accept_jh),
            reaper_jh: Some(reaper_jh),
        })
    }

    /// The event stream: `Accepted`/`Bytes`/`Sent`/`Closed`/`AcceptError`,
    /// in the order this transport observed them. Single-consumer by
    /// convention (a `Receiver` is not `Sync`) — exactly the shape a
    /// capsule's one ordered writer loop wants.
    pub fn events(&self) -> &Receiver<TransportEvent> {
        &self.events_rx
    }

    /// Queue `bytes` for `conn_id`, tagged with `marker` if the caller
    /// wants a [`TransportEvent::Sent`] once the OS write physically
    /// completes. `bytes` must be non-empty (finding 10: this wire never
    /// carries a zero-length send) and no larger than a single Win32
    /// write can represent. Non-blocking: a full outbound budget or an
    /// unknown/already-closed connection both return `Err` immediately —
    /// backpressure POLICY belongs to whoever calls this.
    pub fn send(&self, conn_id: ConnId, bytes: Vec<u8>, marker: Option<SendMarker>) -> Result<(), PipeError> {
        if bytes.is_empty() {
            return Err(PipeError::EmptyPayload);
        }
        if bytes.len() > u32::MAX as usize {
            return Err(PipeError::PayloadTooLarge(bytes.len()));
        }
        let len = bytes.len();
        let map = self.shared.conns.lock().unwrap();
        let conn = map.get(&conn_id).ok_or(PipeError::UnknownConnection(conn_id))?;
        if !conn.outbound.try_reserve(len) {
            return Err(PipeError::QueueFull(conn_id));
        }
        if conn.sender.send(WriteCmd { bytes, marker }).is_err() {
            // Should not normally happen (the writer outlives the entry's
            // presence in this map), but stay defensive rather than leak
            // the reservation.
            conn.outbound.release(len);
            return Err(PipeError::UnknownConnection(conn_id));
        }
        Ok(())
    }

    /// Request that `conn_id` be torn down: both directions cancelled,
    /// both threads joined, the instance recycled. Fire-and-forget — this
    /// only enqueues the request for the reaper thread (finding 6: the
    /// ordered writer loop this transport serves must never block waiting
    /// on a peer or on this call); completion is observed as
    /// [`TransportEvent::Closed`]. A no-op if `conn_id` is already gone.
    pub fn close(&self, conn_id: ConnId) {
        let _ = self.shared.reaper_tx.send(ReaperMsg::Torn(conn_id, ClosedReason::Closed));
    }
}

impl Drop for PipeServer {
    /// Cancel a pending accept (if any) and join the accept thread; tell
    /// the reaper to drain and tear down every remaining connection, then
    /// join it. Every thread this module ever spawned is joined by the
    /// time this returns — bounded, per the module doc, entirely by
    /// `IoSlot::cancel`'s `CancelIoEx`.
    fn drop(&mut self) {
        {
            let mut st = self.shared.accept.lock().unwrap();
            st.shutting_down = true;
            if let Some((h, slot)) = st.current.take() {
                slot.cancel(h.0);
            }
        }
        self.shared.accept_cv.notify_all();
        if let Some(jh) = self.accept_jh.take() {
            jh.join().ok();
        }

        let _ = self.shared.reaper_tx.send(ReaperMsg::Shutdown);
        if let Some(jh) = self.reaper_jh.take() {
            jh.join().ok();
        }
    }
}

/// Set `inst` aside for reuse rather than closing it (finding 7):
/// `DisconnectNamedPipe` resets it to the listening state, then it is
/// pushed onto `AcceptState::recycled` for the accept loop to pick up in
/// preference to creating a fresh instance. The ONLY way an instance is
/// ever actually closed is `AcceptState::recycled`'s own `Drop`, reached
/// when the last `Arc<ServerShared>` goes away — i.e. full server
/// shutdown, never a single connection's teardown.
fn recycle_instance(shared: &Arc<ServerShared>, inst: OwnedHandle) {
    unsafe { DisconnectNamedPipe(inst.as_raw_handle() as HANDLE) };
    shared.accept.lock().unwrap().recycled.push_back(inst);
    shared.accept_cv.notify_all();
}

/// Tear down `conn_id` if it is still present — called EXCLUSIVELY from
/// [`reaper_loop`], which processes messages strictly one at a time, so
/// this never runs concurrently with itself and the `conns.remove` below
/// is the single, uncontested point of truth for "who claims this
/// connection." `reason: None` is `Drop`'s shutdown pass — no event is
/// emitted (nothing could ever observe it; see the module doc's deletion
/// note).
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
        let _ = shared.events_tx.try_send(TransportEvent::Closed(conn_id, reason));
    }
}

/// The reaper: the only thread that ever removes an entry from `conns` or
/// joins a connection's reader/writer (see the module doc's "Reaping"
/// section). Processes [`ReaperMsg`]s strictly one at a time.
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
/// creating a fresh one, blocking (polling `ACCEPT_POLL_INTERVAL`) at the
/// instance cap with nothing recycled yet. `None` means: stop accepting —
/// either shutdown, or a persistent creation failure already reported via
/// `TransportEvent::AcceptError` (finding 9: loud and terminal, not a
/// silent retry).
fn obtain_instance(shared: &Arc<ServerShared>, name: &[u16]) -> Option<OwnedHandle> {
    loop {
        let mut st = shared.accept.lock().unwrap();
        if st.shutting_down {
            return None;
        }
        if let Some(h) = st.recycled.pop_front() {
            return Some(h);
        }
        if st.created < shared.max_instances {
            st.created += 1;
            drop(st);
            match create_pipe_instance(name, false, shared.max_instances) {
                Ok(h) => return Some(h),
                Err(e) => {
                    let mut st = shared.accept.lock().unwrap();
                    st.created -= 1;
                    st.shutting_down = true;
                    drop(st);
                    let _ = shared.events_tx.try_send(TransportEvent::AcceptError(e.to_string()));
                    shared.accept_cv.notify_all();
                    return None;
                }
            }
        }
        let (guard, _timeout) = shared.accept_cv.wait_timeout(st, ACCEPT_POLL_INTERVAL).unwrap();
        drop(guard);
    }
}

/// The accept loop, one dedicated thread for the server's whole life.
/// `first_instance` is the already-created (with
/// `FILE_FLAG_FIRST_PIPE_INSTANCE`) instance from `bind` — this loop posts
/// its `ConnectNamedPipe` as its very first act; every later instance
/// comes from [`obtain_instance`] (recycled or freshly created, never
/// carrying that flag).
fn accept_loop(shared: Arc<ServerShared>, name: Vec<u16>, first_instance: OwnedHandle) {
    let mut pending_instance = Some(first_instance);
    loop {
        let inst = match pending_instance.take() {
            Some(h) => h,
            None => match obtain_instance(&shared, &name) {
                Some(h) => h,
                None => return,
            },
        };

        let raw = SendableHandle(inst.as_raw_handle() as HANDLE);
        let slot = match IoSlot::new() {
            Ok(s) => Arc::new(s),
            Err(_e) => {
                recycle_instance(&shared, inst);
                continue;
            }
        };

        {
            let mut st = shared.accept.lock().unwrap();
            if st.shutting_down {
                drop(st);
                recycle_instance(&shared, inst);
                return;
            }
            st.current = Some((raw, Arc::clone(&slot)));
        }
        let connect_result =
            slot.submit_and_wait(raw.0, |ov| unsafe { ConnectNamedPipe(raw.0, ov) }, |code| {
                code == ERROR_PIPE_CONNECTED as i32
            });
        shared.accept.lock().unwrap().current = None;

        match connect_result {
            Ok(_) => handle_new_connection(&shared, inst, raw, slot),
            Err(e) => {
                recycle_instance(&shared, inst);
                let aborted = e.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32);
                if aborted && shared.accept.lock().unwrap().shutting_down {
                    return;
                }
                // A real connect error on this one instance — try a fresh
                // instance rather than ending the whole accept loop.
            }
        }
    }
}

/// Hand off a just-connected instance: spawn its reader/writer threads
/// (gated — see [`StartGate`]), register it, THEN publish `Accepted` and
/// open the gate (finding 4). `thread::Builder::spawn` (finding 9) makes a
/// spawn failure recoverable: if the writer fails to spawn, the
/// already-spawned reader (still gated, having never touched the pipe) is
/// `abort`ed and joined — bounded, since an aborted gate wait returns
/// immediately — before the instance is recycled, so a handle is never
/// closed (or recycled) while any thread might still be using it.
fn handle_new_connection(shared: &Arc<ServerShared>, inst: OwnedHandle, raw: SendableHandle, read_slot: Arc<IoSlot>) {
    let write_slot = match IoSlot::new() {
        Ok(s) => Arc::new(s),
        Err(_e) => {
            recycle_instance(shared, inst);
            return;
        }
    };
    let conn_id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<WriteCmd>();
    let outbound = Arc::new(OutboundBudget::new(OUTBOUND_BUDGET_BYTES));
    let gate = StartGate::new();

    let reader_jh = {
        let shared = Arc::clone(shared);
        let events_tx = shared.events_tx.clone();
        let read_slot = Arc::clone(&read_slot);
        let gate = Arc::clone(&gate);
        thread::Builder::new().name(format!("sot-pipe-r-{conn_id}")).spawn(move || {
            if !gate.wait_for_start() {
                return;
            }
            reader_loop(raw, read_slot, conn_id, shared, events_tx)
        })
    };
    let reader_jh = match reader_jh {
        Ok(jh) => jh,
        Err(_e) => {
            recycle_instance(shared, inst);
            return;
        }
    };

    let writer_jh = {
        let events_tx = shared.events_tx.clone();
        let write_slot = Arc::clone(&write_slot);
        let outbound = Arc::clone(&outbound);
        let gate = Arc::clone(&gate);
        thread::Builder::new().name(format!("sot-pipe-w-{conn_id}")).spawn(move || {
            if !gate.wait_for_start() {
                return;
            }
            writer_loop(raw, write_slot, conn_id, rx, events_tx, outbound)
        })
    };
    let writer_jh = match writer_jh {
        Ok(jh) => jh,
        Err(_e) => {
            // Finding 9: the reader is spawned but still gated — abort
            // makes its `wait_for_start` return `false` immediately, so
            // joining it is bounded and it never touches `raw`/`inst`.
            gate.abort();
            reader_jh.join().ok();
            recycle_instance(shared, inst);
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
    };
    shared.conns.lock().unwrap().insert(conn_id, conn);
    let _ = shared.events_tx.try_send(TransportEvent::Accepted(conn_id));
    gate.open(); // ONLY now may the reader/writer threads touch the pipe.
}

/// Attempt to deliver one `Bytes` event, retrying against a full `events`
/// channel (finding 8: bounded inbound without an unwakeable worker) —
/// abandoning delivery (returning `false`) the moment `slot` has been
/// cancelled, since a connection already being torn down has nothing left
/// to gain from an undelivered chunk. Returns `false` also if the
/// consumer is gone entirely (channel disconnected).
fn deliver_bytes(events_tx: &SyncSender<TransportEvent>, conn_id: ConnId, bytes: Vec<u8>, slot: &IoSlot) -> bool {
    let mut item = TransportEvent::Bytes(conn_id, bytes);
    loop {
        match events_tx.try_send(item) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(v)) => {
                item = v;
                if slot.is_closing() {
                    return false;
                }
                std::thread::sleep(EVENTS_RETRY_INTERVAL);
            }
        }
    }
}

/// Classify a terminal `ReadFile`/`GetOverlappedResult` error: the
/// recognized disconnect family (broken/unconnected pipe, or this side's
/// own cancellation) is `Eof`; anything else is `Error`. Finding 10: a
/// SUCCESSFUL zero-byte read is handled separately, in [`reader_loop`]
/// itself — it never reaches this function, and is never classified as
/// EOF.
fn classify_terminal_error(e: std::io::Error) -> ClosedReason {
    match e.raw_os_error() {
        Some(c)
            if c == ERROR_BROKEN_PIPE as i32
                || c == ERROR_PIPE_NOT_CONNECTED as i32
                || c == ERROR_OPERATION_ABORTED as i32 =>
        {
            ClosedReason::Eof
        }
        _ => ClosedReason::Error(e.to_string()),
    }
}

/// One connection's read side: at most one outstanding `ReadFile` at a
/// time. On any terminal condition this thread does NOT touch `conns` or
/// join anything itself — it only signals the reaper (see the module
/// doc's "Reaping" section) and returns.
fn reader_loop(
    handle: SendableHandle,
    slot: Arc<IoSlot>,
    conn_id: ConnId,
    shared: Arc<ServerShared>,
    events_tx: SyncSender<TransportEvent>,
) {
    let mut buf = vec![0u8; READ_BUF_LEN];
    let reason = loop {
        let result = slot.submit_and_wait(
            handle.0,
            |ov| unsafe { ReadFile(handle.0, buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut(), ov) },
            |_| false,
        );
        match result {
            // Finding 10: a SUCCESSFUL zero-byte completion is not EOF —
            // Microsoft documents it as a legitimate outcome of the peer
            // issuing its own zero-byte write. Just read again.
            Ok(0) => continue,
            Ok(n) => {
                if !deliver_bytes(&events_tx, conn_id, buf[..n as usize].to_vec(), &slot) {
                    break ClosedReason::Eof;
                }
            }
            Err(e) => break classify_terminal_error(e),
        }
    };
    let _ = shared.reaper_tx.send(ReaperMsg::Torn(conn_id, reason));
}

/// One connection's write side: drains queued sends in order, one
/// outstanding `WriteFile` at a time, emitting `Sent` for marker-tagged
/// sends once the OS reports the write physically complete, and releasing
/// its outbound-budget reservation once that write RETURNS (success or
/// failure) either way. Exits when its channel disconnects (the reaper
/// dropped the sender) or its current write is cancelled. Never touches
/// `shared.conns` — teardown is always the reaper's.
fn writer_loop(
    handle: SendableHandle,
    slot: Arc<IoSlot>,
    conn_id: ConnId,
    rx: Receiver<WriteCmd>,
    events_tx: SyncSender<TransportEvent>,
    outbound: Arc<OutboundBudget>,
) {
    while let Ok(cmd) = rx.recv() {
        let len = cmd.bytes.len();
        let result = slot.submit_and_wait(
            handle.0,
            |ov| unsafe { WriteFile(handle.0, cmd.bytes.as_ptr(), cmd.bytes.len() as u32, std::ptr::null_mut(), ov) },
            |_| false,
        );
        outbound.release(len);
        match result {
            Ok(_) => {
                if let Some(marker) = cmd.marker {
                    let _ = events_tx.try_send(TransportEvent::Sent(conn_id, marker));
                }
            }
            Err(_e) => break, // cancelled, or the pipe is broken — nothing more to write
        }
    }
}

/// The client side of one voyage's pipe: `read`/`write_all` are blocking
/// from the calling thread's own perspective, but `PipeClient` is `Sync`
/// (via the same [`IoSlot`] the server uses) — a second thread may call
/// [`PipeClient::cancel`] at any time to unblock whichever of the two is
/// currently in flight (ADR 0041 step 6's mgmt challenge needs bounded
/// reads AND bounded writes).
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
/// connected — waits on `WaitNamedPipeW` between attempts, the documented
/// idiom) and `ERROR_FILE_NOT_FOUND` (the server has not called `bind` yet)
/// — both are ordinary races in a healthy multi-client server, not
/// failures.
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
            let read_slot = IoSlot::new().map_err(|e| PipeError::Io { op: "CreateEventW(client read)", source: e })?;
            let write_slot =
                IoSlot::new().map_err(|e| PipeError::Io { op: "CreateEventW(client write)", source: e })?;
            return Ok(PipeClient { handle, raw, read_slot, write_slot });
        }
        let err = std::io::Error::last_os_error();
        let code = err.raw_os_error();
        let retryable = code == Some(ERROR_PIPE_BUSY as i32) || code == Some(ERROR_FILE_NOT_FOUND as i32);
        if !retryable || Instant::now() >= deadline {
            return Err(PipeError::Io { op: "CreateFileW", source: err });
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
    /// thread via [`PipeClient::cancel`]. `bytes` must be non-empty
    /// (finding 10) and no larger than a single Win32 write can represent.
    /// Named pipes complete a `WriteFile` as one atomic operation
    /// (byte-mode, no partial writes to retry-loop over).
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
                |ov| unsafe { WriteFile(self.raw.0, bytes.as_ptr(), bytes.len() as u32, std::ptr::null_mut(), ov) },
                |_| false,
            )
            .map(|_| ())
            .map_err(map_client_io_error("WriteFile"))
    }

    /// Blocking read into `buf`, cancellable from another thread via
    /// [`PipeClient::cancel`]. `Ok(0)` means the server closed its end
    /// (ordered EOF, ADR 0041: "there is no `detach` op — ordered pipe EOF
    /// is detach") — NEVER a successful zero-byte completion (finding 10:
    /// that is a legitimate, non-EOF outcome this method silently retries
    /// past, since this transport's own `send`/`write_all` never produce
    /// one anyway).
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, PipeError> {
        loop {
            let result = self.read_slot.submit_and_wait(
                self.raw.0,
                |ov| unsafe { ReadFile(self.raw.0, buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut(), ov) },
                |_| false,
            );
            match result {
                Ok(0) => continue,
                Ok(n) => return Ok(n as usize),
                Err(e)
                    if matches!(e.raw_os_error(), Some(c) if c == ERROR_BROKEN_PIPE as i32 || c == ERROR_PIPE_NOT_CONNECTED as i32) =>
                {
                    return Ok(0);
                }
                Err(e) => return Err(map_client_io_error("ReadFile")(e)),
            }
        }
    }

    /// Cancel whatever is currently in flight on EITHER direction, from
    /// any thread — safe to call concurrently with `read`/`write_all` on
    /// another thread; see [`IoSlot`]'s doc for why. A cancelled call
    /// returns `Err(PipeError::Cancelled)`, distinct from an ordered EOF
    /// or an ordinary I/O error.
    pub fn cancel(&self) {
        self.read_slot.cancel(self.raw.0);
        self.write_slot.cancel(self.raw.0);
    }
}

/// Shared client-side error mapping: `ERROR_OPERATION_ABORTED` (this
/// client's own [`PipeClient::cancel`] fired) becomes [`PipeError::Cancelled`],
/// distinct from an ordinary I/O failure.
fn map_client_io_error(op: &'static str) -> impl Fn(std::io::Error) -> PipeError {
    move |e| {
        if e.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) {
            PipeError::Cancelled
        } else {
            PipeError::Io { op, source: e }
        }
    }
}
