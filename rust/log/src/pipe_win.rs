//! The ADR 0041 step-5 Windows named-pipe transport (round 1: TRANSPORT
//! ONLY). Moves bytes over `\\.\pipe\sot-voyage-<id>` and reports
//! completions; it does not know about mgmt/attach lanes, `hello`,
//! opcodes, or checkpoints — `wire.rs` owns every frame shape, and
//! [`wire::FrameSplitter`] is what a consumer of this module's `Bytes`
//! events feeds. Round 2 (out of scope here) wires this into the capsule's
//! ordered writer loop and the `sot-capsule` bin; this module has no
//! dependency on either and must not gain one.
//!
//! # Cancellation, pinned (ADR 0041 "Step 5 as specified")
//!
//! "Overlapped I/O + `CancelIoEx`... dedicated threads alone do not make
//! blocked I/O cancellable" is the load-bearing ruling this whole module is
//! built around. Every blocking OS call here (`ConnectNamedPipe`,
//! `ReadFile`, `WriteFile`) is issued overlapped, through a *per-direction,
//! address-stable* [`OverlappedBuf`] reused across every iteration of that
//! direction's loop — so a canceller on another thread can always reach the
//! CURRENTLY (or most recently) pending operation by an address it learned
//! once, at connection-accept time. `CancelIoEx` targeting a SPECIFIC
//! `OVERLAPPED` cancels only that direction; passed `NULL` implicitly (by
//! cancelling both directions' addresses) it tears down a whole connection.
//! Every thread this module spawns is joined somewhere in this file before
//! its connection (or the server) is considered torn down — never detached
//! — and `CancelIoEx` is what makes each join bounded rather than a hang.
//!
//! # Squat detection
//!
//! `FILE_FLAG_FIRST_PIPE_INSTANCE` is passed on the very first
//! `CreateNamedPipeW` call for a voyage's pipe name ONLY (see
//! [`PipeServer::bind`]); every later instance — created by the accept loop
//! after each hand-off — omits it. Windows fails a `CreateNamedPipeW` call
//! carrying that flag with `ERROR_ACCESS_DENIED` if any instance of the
//! name already exists, regardless of whether that existing instance was
//! itself the "first" one — so a rival's create-with-the-flag failing IS
//! the squat check, and it holds for as long as this server keeps ANY
//! instance open (pending-accept or connected), which is the server's
//! entire lifetime by construction.
//!
//! # Visibility
//!
//! Every type below is `pub`, not `pub(crate)` — a deliberate exception to
//! this unit's default (see the brief's constraints): `tests/pipe_win.rs`
//! is a separate integration-test crate (needed for the same structural
//! reason `tests/conpty.rs` is: a plain echo consumer thread with no
//! capsule, run against a real OS pipe), and an integration test can only
//! ever reach a library's `pub` items. `conpty.rs` and `capsule_win.rs` are
//! `pub` for the identical reason.
//!
//! # What this module does NOT do (round 1 scope, ADR 0041 step 5)
//!
//! No lane typing, no `hello`/opcodes, no checkpoint watermarking, no
//! keepalive deadlines, no dedupe, no lockstep enforcement, no subscriber
//! cap beyond the raw `max_instances` the caller passes — all of that is
//! the capsule's ordered writer loop, wired in the follow-up unit that
//! also touches `capsule_win.rs` and the `sot-capsule` bin (out of THIS
//! unit's scope).

#![cfg(windows)]

use std::cell::UnsafeCell;
use std::collections::HashMap;
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
    ConnectNamedPipe, CreateNamedPipeW, WaitNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::CreateEventW;

/// Bound on one outstanding overlapped `ReadFile` (ADR 0041: "the transport
/// just must not read unboundedly ahead" — the consumer's lockstep protocol
/// bounds logical inbound; this is only the raw-byte ceiling per read).
const READ_BUF_LEN: usize = 65_536;

/// The outbound-send queue's capacity per connection. Fixed, not a
/// constructor parameter: the invariant it serves is "a slow/hostile
/// consumer cannot make this transport buffer unboundedly," not a tunable
/// performance knob — the ADR's own bounded-queue backpressure POLICY lives
/// in whoever calls [`PipeServer::send`], not here.
const WRITE_QUEUE_CAP: usize = 8;

/// `\\.\pipe\sot-voyage-<id>`, UTF-16, NUL-terminated. Panics never: `id`
/// is validated by [`validate_voyage_id`] at every call site before this
/// runs, so the interpolation is always onto a canonical, hyphen-and-hex-
/// only string.
fn pipe_name_wide(voyage_id: &str) -> Vec<u16> {
    wide_null(&format!(r"\\.\pipe\sot-voyage-{voyage_id}"))
}

/// NUL-terminated UTF-16 for an arbitrary Rust string. A small, deliberate
/// duplicate of `conpty.rs`'s and `fsutil.rs`'s own private copies of this
/// exact helper — `capsule_win.rs`'s doc already states this crate's rule
/// for cross-module Windows primitives: duplicate a three-line leaf helper
/// rather than invent a shared home for it.
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
/// them — an uppercase or brace-wrapped id would otherwise name a
/// DIFFERENT pipe than its canonical form under naive string use elsewhere
/// (the codec's own atom is the canonical lowercase-hyphenated string).
/// Anything that fails to parse at all (path-traversal shapes, wrong
/// length, non-hex bytes) is rejected the same way.
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
/// interprets a marker's value — a future capsule's spec-gate write-
/// completion signal is exactly this event, uninterpreted.
pub type SendMarker = u64;

/// Why a connection ended, reported once per connection on
/// [`TransportEvent::Closed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosedReason {
    /// The peer disconnected (or this side observed a broken/unconnected
    /// pipe) — detected by the connection's own reader loop, with no
    /// [`PipeServer::close`]/[`PipeServer::drain_and_close`] call involved.
    Eof,
    /// [`PipeServer::close`] tore this connection down.
    Closed,
    /// [`PipeServer::drain_and_close`] finished draining this connection's
    /// outbound queue, then tore it down.
    Drained,
    /// The whole [`PipeServer`] was dropped while this connection was
    /// still open.
    ServerShutdown,
    /// An I/O error other than a recognized disconnect ended the
    /// connection's reader loop.
    Error(String),
}

/// This transport's event surface to its consumer — one connection's
/// worth of raw bytes and lifecycle, uninterpreted. Delivered over
/// [`PipeServer::events`] in the order this module observed them; the
/// consumer feeds `Bytes` payloads to its own [`crate::wire::FrameSplitter`]
/// per connection.
#[derive(Debug)]
pub enum TransportEvent {
    /// A new connection accepted; `send`/`close`/`drain_and_close` may now
    /// target it.
    Accepted(ConnId),
    /// Raw bytes read from a connection, in the order read. At most one
    /// `ReadFile` is ever outstanding per connection (see the module doc).
    Bytes(ConnId, Vec<u8>),
    /// The `WriteFile` for a marker-tagged [`PipeServer::send`] call has
    /// physically completed — the spec-gate's write-completion signal,
    /// uninterpreted.
    Sent(ConnId, SendMarker),
    /// The connection ended; no further events for this `ConnId` follow.
    Closed(ConnId, ClosedReason),
}

/// Errors this transport's own API surface can report. I/O failures deep
/// inside a background thread that end one connection are reported as
/// [`TransportEvent::Closed`], not through this type — this is only for
/// calls that fail SYNCHRONOUSLY, at the call site.
#[derive(Debug, thiserror::Error)]
pub enum PipeError {
    #[error("invalid voyage id {0:?}: must be the canonical lowercase-hyphenated form of an RFC 4122 UUID")]
    InvalidVoyageId(String),
    #[error("max_instances must be at least 1")]
    InvalidMaxInstances,
    #[error("{op}: {source}")]
    Io {
        op: &'static str,
        source: std::io::Error,
    },
    #[error("unknown or already-closed connection {0}")]
    UnknownConnection(ConnId),
    #[error("send queue full for connection {0}")]
    QueueFull(ConnId),
}

/// A raw Windows `HANDLE`, asserted `Send` across exactly the discipline
/// this module relies on: a named pipe's two directions are legitimately
/// driven by two DIFFERENT threads at once (the standard full-duplex-pipe
/// pattern), and exactly one owner ([`ConnHandle::owned`] /
/// [`PipeClient::handle`]) ever calls `CloseHandle` on it, only after every
/// thread using this copy has already stopped (joined). The wrapped value
/// is never dereferenced as a pointer — it is an opaque OS handle, passed
/// only to `windows-sys` calls.
#[derive(Clone, Copy)]
struct SendableHandle(HANDLE);
unsafe impl Send for SendableHandle {}

/// A boxed `OVERLAPPED` plus its own dedicated auto-reset event,
/// address-stable for as long as `self` lives — reused across every
/// iteration of one connection's one I/O direction (never reallocated), so
/// an external `CancelIoEx` can always target the CURRENT pending
/// operation via an address it read once. Auto-reset (`bManualReset =
/// FALSE`) means `GetOverlappedResult`'s own wait clears the event on
/// success, so reuse needs no manual `ResetEvent` between calls.
///
/// `UnsafeCell`-backed with `&self` accessors (rather than `&mut self`) so
/// [`PipeClient`] can offer a `read` and a `cancel_read` that are legally
/// callable from two different threads at once — the exact shape a test
/// needs to unblock a blocked read from another thread. This does not
/// weaken any safety property versus a plain `&mut` design: the ACTUAL
/// synchronization of the pointed-at memory is the Windows overlapped-I/O
/// contract, enforced by the kernel, not by Rust's aliasing rules — the
/// only thing ever read through `&self` from a second thread is the
/// STABLE ADDRESS, passed opaquely to `CancelIoEx`, never the struct's
/// content.
struct OverlappedBuf {
    cell: UnsafeCell<OVERLAPPED>,
}
unsafe impl Send for OverlappedBuf {}
unsafe impl Sync for OverlappedBuf {}

impl OverlappedBuf {
    fn new() -> std::io::Result<Self> {
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = event;
        Ok(Self { cell: UnsafeCell::new(ov) })
    }

    fn as_mut_ptr(&self) -> *mut OVERLAPPED {
        self.cell.get()
    }

    fn as_ptr(&self) -> *const OVERLAPPED {
        self.cell.get() as *const OVERLAPPED
    }

    /// Reset for reuse before the next `ReadFile`/`WriteFile`/
    /// `ConnectNamedPipe` call on this slot: the event handle is kept
    /// (closing and recreating one per iteration would be wasteful and
    /// pointless — auto-reset already clears its signaled state), every
    /// other field is zeroed exactly as at construction.
    fn reset(&self) {
        unsafe {
            let event = (*self.cell.get()).hEvent;
            *self.cell.get() = std::mem::zeroed();
            (*self.cell.get()).hEvent = event;
        }
    }
}

impl Drop for OverlappedBuf {
    fn drop(&mut self) {
        unsafe { CloseHandle((*self.cell.get()).hEvent) };
    }
}

/// Block for the definitive result of an overlapped op already submitted
/// on `handle`/`ov` (`ConnectNamedPipe`/`ReadFile`/`WriteFile`, called
/// exactly when that call itself returned nonzero OR failed with
/// `ERROR_IO_PENDING` — never on a synchronous non-pending failure, which
/// has nothing queued to wait for). Returns the transferred byte count on
/// success; `ERROR_OPERATION_ABORTED` (this op's `CancelIoEx` fired) and
/// every other I/O error surface as `Err`, undistinguished here — callers
/// classify by `raw_os_error()` where the distinction matters.
fn wait_overlapped(handle: HANDLE, ov: *const OVERLAPPED) -> std::io::Result<u32> {
    let mut transferred: u32 = 0;
    let ok = unsafe { GetOverlappedResult(handle, ov, &mut transferred, 1) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(transferred)
}

/// Issue one overlapped `ReadFile`/`WriteFile`-shaped call and drive it to
/// completion via [`wait_overlapped`] — the "call, then unconditionally
/// wait unless the call itself failed synchronously" pattern both the
/// reader and writer loops (and the client's blocking read/write) share.
/// `issue` performs the raw `ReadFile`/`WriteFile` call and returns its
/// bare `BOOL` result (nonzero: succeeded or queued; zero: check
/// `GetLastError`).
fn run_overlapped(handle: HANDLE, ov: &OverlappedBuf, issue: impl FnOnce() -> i32) -> std::io::Result<u32> {
    ov.reset();
    let ok = issue();
    if ok == 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
            return Err(err);
        }
    }
    wait_overlapped(handle, ov.as_ptr())
}

/// Create one instance of the voyage's named pipe. `first` must be `true`
/// for EXACTLY the very first instance ever created for this pipe name
/// (see [`PipeServer::bind`] and the module doc's squat-detection note); a
/// fresh owner-only descriptor is built per call — `CreateNamedPipeW`
/// copies what it needs from it at creation time (same contract
/// `fsutil::create_dir_protected` documents for `CreateDirectoryW`), and a
/// fresh SID lookup on each new-client-triggered instance creation is
/// cheap enough not to justify holding the descriptor alive across a
/// thread boundary for the accept loop's whole life.
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

/// `ConnectNamedPipe`, overlapped, treating `ERROR_PIPE_CONNECTED` as
/// success (ADR 0041, pinned: a client can race in between
/// `CreateNamedPipeW` and this call).
fn connect_named_pipe_overlapped(handle: HANDLE, ov: &OverlappedBuf) -> std::io::Result<()> {
    ov.reset();
    let ok = unsafe { ConnectNamedPipe(handle, ov.as_mut_ptr()) };
    if ok != 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(e) if e == ERROR_PIPE_CONNECTED as i32 => Ok(()),
        Some(e) if e == ERROR_IO_PENDING as i32 => wait_overlapped(handle, ov.as_ptr()).map(|_| ()),
        _ => Err(err),
    }
}

/// One queued outbound send: raw bytes, plus an optional marker to echo
/// back on physical write completion.
struct WriteCmd {
    bytes: Vec<u8>,
    marker: Option<SendMarker>,
}

/// One live connection's threads, handles, and cancellation addresses —
/// owned by whichever code path is currently responsible for tearing it
/// down (the shared `conns` map while the connection is live; a local
/// variable in [`teardown_owned`]/[`teardown_from_reader`] while doing so).
struct ConnHandle {
    id: ConnId,
    /// Authoritative close: dropped only after both threads below are
    /// confirmed stopped.
    owned: OwnedHandle,
    /// A non-owning copy of the same raw handle, for the reader/writer
    /// threads' own I/O calls and for this struct's `CancelIoEx` calls.
    raw: SendableHandle,
    read_ov: Arc<OverlappedBuf>,
    write_ov: Arc<OverlappedBuf>,
    sender: SyncSender<WriteCmd>,
    reader_jh: JoinHandle<()>,
    writer_jh: JoinHandle<()>,
}

/// Accept-loop state shared with [`PipeServer`]'s public methods and
/// `Drop`: the shutdown flag, the currently-pending accept (if any, for
/// cancellation), and the live-instance count the `max_instances` cap and
/// its wait/notify are built on.
struct AcceptState {
    shutting_down: bool,
    /// The instance currently blocked in `ConnectNamedPipe`, if any —
    /// `Some` only for the brief window between posting the connect and it
    /// resolving. Consulted by `Drop` to cancel a genuinely blocked accept.
    pending: Option<(SendableHandle, Arc<OverlappedBuf>)>,
    /// Instances currently open (pending-accept OR connected) — never
    /// exceeds `max_instances`; the accept loop blocks on `accept_cv` while
    /// at the cap rather than exceeding it.
    live_instances: u32,
}

struct ServerShared {
    conns: Mutex<HashMap<ConnId, ConnHandle>>,
    next_id: AtomicU64,
    accept: Mutex<AcceptState>,
    accept_cv: Condvar,
    /// Reader threads that self-tore-down on natural EOF push their OWN
    /// `JoinHandle` here (a thread cannot join itself) for `Drop` to join —
    /// see the module doc's cancellation section and [`teardown_from_reader`].
    retired: Mutex<Vec<JoinHandle<()>>>,
    events_tx: Sender<TransportEvent>,
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
/// voyage's writer lock, and for dropping the returned `PipeServer` (which
/// closes every instance, freeing the pipe name) before releasing that
/// lock. This module enforces neither half of that ordering itself — it
/// has no lock to check — it only guarantees the other side of the
/// contract: while a `PipeServer` is alive, the pipe exists; the instant
/// it is dropped, every instance is closed.
pub struct PipeServer {
    shared: Arc<ServerShared>,
    events_rx: Receiver<TransportEvent>,
    accept_jh: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for PipeServer {
    /// Minimal by necessity: several fields (the accept thread's
    /// `JoinHandle`, the connection map's threads) carry nothing
    /// meaningful to print, so this exists only to satisfy trait bounds
    /// like `Result::unwrap_err`'s in this module's own tests.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeServer").finish_non_exhaustive()
    }
}

impl PipeServer {
    /// Create `\\.\pipe\sot-voyage-<voyage_id>` (squat-detected via
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` on this, its first instance) and
    /// start the accept loop. `max_instances` is the total simultaneous
    /// pipe-instance cap (pending-accept + connected) — the caller (later
    /// the capsule) passes the spec-gate's total subscriber count; it must
    /// be at least 1.
    pub fn bind(voyage_id: &str, max_instances: u32) -> Result<Self, PipeError> {
        validate_voyage_id(voyage_id)?;
        if max_instances == 0 {
            return Err(PipeError::InvalidMaxInstances);
        }
        let name = pipe_name_wide(voyage_id);
        let first = create_pipe_instance(&name, true, max_instances).map_err(|e| PipeError::Io {
            op: "CreateNamedPipeW(first instance)",
            source: e,
        })?;

        let (events_tx, events_rx) = mpsc::channel();
        let shared = Arc::new(ServerShared {
            conns: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            accept: Mutex::new(AcceptState {
                shutting_down: false,
                pending: None,
                live_instances: 1,
            }),
            accept_cv: Condvar::new(),
            retired: Mutex::new(Vec::new()),
            events_tx,
            max_instances,
        });

        let thread_shared = Arc::clone(&shared);
        let accept_jh = thread::spawn(move || accept_loop(thread_shared, name, first));

        Ok(Self {
            shared,
            events_rx,
            accept_jh: Some(accept_jh),
        })
    }

    /// The event stream: `Accepted`/`Bytes`/`Sent`/`Closed`, in the order
    /// this transport observed them. Single-consumer by convention (a
    /// `Receiver` is not `Sync`) — exactly the shape a capsule's one
    /// ordered writer loop wants.
    pub fn events(&self) -> &Receiver<TransportEvent> {
        &self.events_rx
    }

    /// Queue `bytes` for `conn_id`, tagged with `marker` if the caller
    /// wants a [`TransportEvent::Sent`] once the OS write physically
    /// completes. Non-blocking: a full queue or an unknown/already-closed
    /// connection both return `Err` immediately rather than blocking the
    /// caller — backpressure POLICY belongs to whoever calls this.
    pub fn send(
        &self,
        conn_id: ConnId,
        bytes: Vec<u8>,
        marker: Option<SendMarker>,
    ) -> Result<(), PipeError> {
        let map = self.shared.conns.lock().unwrap();
        let conn = map.get(&conn_id).ok_or(PipeError::UnknownConnection(conn_id))?;
        conn.sender.try_send(WriteCmd { bytes, marker }).map_err(|e| match e {
            TrySendError::Full(_) => PipeError::QueueFull(conn_id),
            TrySendError::Disconnected(_) => PipeError::UnknownConnection(conn_id),
        })
    }

    /// Cancel BOTH directions immediately (read and any in-flight write —
    /// queued-but-not-yet-started sends are simply dropped with the
    /// channel), join both threads, close the handle. Bounded by
    /// `CancelIoEx`: this returns promptly even if the peer never reads
    /// and a write is stuck in the kernel. A no-op (`Ok`) if `conn_id` is
    /// already gone — idempotent, not an error, since a concurrent natural
    /// EOF racing this call is expected and harmless (see
    /// [`teardown_from_reader`]).
    pub fn close(&self, conn_id: ConnId) -> Result<(), PipeError> {
        let conn = self.shared.conns.lock().unwrap().remove(&conn_id);
        if let Some(conn) = conn {
            teardown_owned(&self.shared, conn, ClosedReason::Closed, true);
        }
        Ok(())
    }

    /// Stop accepting new sends for `conn_id`, then BLOCK the caller until
    /// its already-queued outbound backlog has physically finished writing
    /// (the read direction is cancelled immediately — it is not what this
    /// call is draining), then close. Bounded by "the caller's own
    /// progress policy" (ADR 0041): if the peer never reads and a write
    /// never completes, this call never returns — a caller that wants a
    /// deadline runs it on its own thread/timeout and falls back to
    /// [`PipeServer::close`] to force it, which the "flooded, no reader"
    /// case (see `tests/pipe_win.rs`) exercises directly.
    pub fn drain_and_close(&self, conn_id: ConnId) -> Result<(), PipeError> {
        let conn = self.shared.conns.lock().unwrap().remove(&conn_id);
        if let Some(conn) = conn {
            teardown_owned(&self.shared, conn, ClosedReason::Drained, false);
        }
        Ok(())
    }
}

impl Drop for PipeServer {
    /// Cancel a pending accept (if any) and join the accept thread; tear
    /// down every still-open connection exactly as `close` would; join
    /// every retired reader thread a natural EOF already finished on its
    /// own. Every thread this module ever spawned is joined by the time
    /// this returns — bounded, per the module doc, entirely by
    /// `CancelIoEx`.
    fn drop(&mut self) {
        {
            let mut st = self.shared.accept.lock().unwrap();
            st.shutting_down = true;
            if let Some((h, ov)) = st.pending.take() {
                unsafe { CancelIoEx(h.0, ov.as_ptr()) };
            }
        }
        self.shared.accept_cv.notify_all();
        if let Some(jh) = self.accept_jh.take() {
            jh.join().ok();
        }

        let leftover: Vec<ConnHandle> = {
            let mut map = self.shared.conns.lock().unwrap();
            map.drain().map(|(_, c)| c).collect()
        };
        for conn in leftover {
            teardown_owned(&self.shared, conn, ClosedReason::ServerShutdown, true);
        }

        let retired: Vec<JoinHandle<()>> = std::mem::take(&mut *self.shared.retired.lock().unwrap());
        for jh in retired {
            jh.join().ok();
        }
    }
}

/// Tear down a connection the CALLER already removed from `shared.conns`
/// — always invoked from a thread that is NEITHER this connection's reader
/// nor its writer (an explicit `close`/`drain_and_close` call, or
/// `PipeServer::drop`), so joining both its threads directly is always
/// sound. Dropping `conn.sender` first is what lets a NOT-cancelled
/// writer (the `drain_and_close` case) observe "no more work, and none
/// will ever arrive" once its backlog empties, rather than blocking on
/// `recv` forever. The read direction is unconditionally cancelled: it is
/// never what either `close` or `drain_and_close` is trying to preserve.
fn teardown_owned(shared: &Arc<ServerShared>, conn: ConnHandle, reason: ClosedReason, cancel_write: bool) {
    drop(conn.sender);
    unsafe { CancelIoEx(conn.raw.0, conn.read_ov.as_ptr()) };
    if cancel_write {
        unsafe { CancelIoEx(conn.raw.0, conn.write_ov.as_ptr()) };
    }
    conn.reader_jh.join().ok();
    conn.writer_jh.join().ok();
    drop(conn.owned);
    release_instance_slot(shared);
    let _ = shared.events_tx.send(TransportEvent::Closed(conn.id, reason));
}

/// The natural-EOF teardown a connection's OWN reader thread runs on
/// itself: cancels only the write (the peer is already gone; there is
/// nothing left to drain toward), joins the writer (a DIFFERENT thread —
/// always sound), drops the sender and the handle, releases the instance
/// slot, emits `Closed`, and returns the READER's OWN `JoinHandle` — which
/// a thread cannot join on itself — for the caller (the reader loop) to
/// push into `shared.retired` for `PipeServer::drop` to join later.
fn teardown_from_reader(shared: &Arc<ServerShared>, conn: ConnHandle, reason: ClosedReason) -> JoinHandle<()> {
    drop(conn.sender);
    unsafe { CancelIoEx(conn.raw.0, conn.write_ov.as_ptr()) };
    conn.writer_jh.join().ok();
    drop(conn.owned);
    release_instance_slot(shared);
    let _ = shared.events_tx.send(TransportEvent::Closed(conn.id, reason));
    conn.reader_jh
}

fn release_instance_slot(shared: &Arc<ServerShared>) {
    {
        let mut st = shared.accept.lock().unwrap();
        st.live_instances -= 1;
    }
    shared.accept_cv.notify_all();
}

/// The accept loop, one dedicated thread for the server's whole life
/// (ADR 0041, pinned). `first_instance` is the already-created (with
/// `FILE_FLAG_FIRST_PIPE_INSTANCE`) instance from `bind` — this loop posts
/// its `ConnectNamedPipe` as its very first act rather than creating a
/// redundant instance, then creates every LATER instance itself (never
/// carrying that flag).
fn accept_loop(shared: Arc<ServerShared>, name: Vec<u16>, first_instance: OwnedHandle) {
    let mut pending_instance = Some(first_instance);
    loop {
        let inst = match pending_instance.take() {
            Some(h) => h,
            None => {
                let mut st = shared.accept.lock().unwrap();
                loop {
                    if st.shutting_down {
                        return;
                    }
                    if st.live_instances < shared.max_instances {
                        break;
                    }
                    st = shared.accept_cv.wait(st).unwrap();
                }
                st.live_instances += 1;
                drop(st);
                match create_pipe_instance(&name, false, shared.max_instances) {
                    Ok(h) => h,
                    Err(_e) => {
                        release_instance_slot(&shared);
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                }
            }
        };

        let raw = SendableHandle(inst.as_raw_handle() as HANDLE);
        let ov = match OverlappedBuf::new() {
            Ok(o) => Arc::new(o),
            Err(_e) => {
                drop(inst);
                release_instance_slot(&shared);
                continue;
            }
        };

        {
            let mut st = shared.accept.lock().unwrap();
            if st.shutting_down {
                drop(inst);
                st.live_instances -= 1;
                return;
            }
            st.pending = Some((raw, Arc::clone(&ov)));
        }
        let connect_result = connect_named_pipe_overlapped(raw.0, &ov);
        shared.accept.lock().unwrap().pending = None;

        match connect_result {
            Ok(()) => handle_new_connection(&shared, inst, raw, ov),
            Err(e) => {
                drop(inst);
                release_instance_slot(&shared);
                let aborted = e.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32);
                if aborted && shared.accept.lock().unwrap().shutting_down {
                    return;
                }
                // A real connect error on this one instance — try a fresh
                // instance rather than ending the whole accept loop over
                // it.
            }
        }
    }
}

/// Hand off a just-connected instance: spawn its reader/writer threads,
/// register it, and emit `Accepted`.
fn handle_new_connection(shared: &Arc<ServerShared>, inst: OwnedHandle, raw: SendableHandle, read_ov: Arc<OverlappedBuf>) {
    let write_ov = match OverlappedBuf::new() {
        Ok(o) => Arc::new(o),
        Err(_e) => {
            drop(inst);
            release_instance_slot(shared);
            return;
        }
    };
    let conn_id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::sync_channel::<WriteCmd>(WRITE_QUEUE_CAP);

    let reader_jh = {
        let shared = Arc::clone(shared);
        let events_tx = shared.events_tx.clone();
        let read_ov = Arc::clone(&read_ov);
        thread::spawn(move || reader_loop(raw, read_ov, conn_id, shared, events_tx))
    };
    let writer_jh = {
        let events_tx = shared.events_tx.clone();
        let write_ov = Arc::clone(&write_ov);
        thread::spawn(move || writer_loop(raw, write_ov, conn_id, rx, events_tx))
    };

    let conn = ConnHandle {
        id: conn_id,
        owned: inst,
        raw,
        read_ov,
        write_ov,
        sender: tx,
        reader_jh,
        writer_jh,
    };
    shared.conns.lock().unwrap().insert(conn_id, conn);
    let _ = shared.events_tx.send(TransportEvent::Accepted(conn_id));
}

/// One connection's read side: at most one outstanding `ReadFile` at a
/// time, each iteration emitting `Bytes` on success. On any terminal
/// condition (peer disconnect, cancellation, or an unexpected error) this
/// thread attempts to claim teardown by removing ITS OWN connection from
/// `shared.conns` — succeeding exactly when no concurrent
/// `close`/`drain_and_close`/`Drop` already claimed it first (see
/// [`teardown_from_reader`]'s doc); losing that race means someone else is
/// already handling teardown and this thread has nothing further to do.
fn reader_loop(handle: SendableHandle, ov: Arc<OverlappedBuf>, conn_id: ConnId, shared: Arc<ServerShared>, events_tx: Sender<TransportEvent>) {
    let mut buf = vec![0u8; READ_BUF_LEN];
    let reason = loop {
        let result = run_overlapped(handle.0, &ov, || unsafe {
            ReadFile(handle.0, buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut(), ov.as_mut_ptr())
        });
        match result {
            Ok(n) if n > 0 => {
                if events_tx.send(TransportEvent::Bytes(conn_id, buf[..n as usize].to_vec())).is_err() {
                    break ClosedReason::Eof; // consumer gone; nothing left to serve
                }
            }
            Ok(_) => break ClosedReason::Eof, // zero-byte "success": treat as EOF defensively
            Err(e) => {
                break match e.raw_os_error() {
                    Some(c)
                        if c == ERROR_BROKEN_PIPE as i32
                            || c == ERROR_PIPE_NOT_CONNECTED as i32
                            || c == ERROR_OPERATION_ABORTED as i32 =>
                    {
                        ClosedReason::Eof
                    }
                    _ => ClosedReason::Error(e.to_string()),
                };
            }
        }
    };

    let mut map = shared.conns.lock().unwrap();
    if let Some(conn) = map.remove(&conn_id) {
        drop(map);
        let my_jh = teardown_from_reader(&shared, conn, reason);
        shared.retired.lock().unwrap().push(my_jh);
    }
    // else: an explicit close/drain_and_close/Drop already claimed and is
    // handling (or has handled) teardown for this connection.
}

/// One connection's write side: drains queued sends in order, one
/// outstanding `WriteFile` at a time, emitting `Sent` for marker-tagged
/// sends once the OS reports the write physically complete. Exits when
/// its queue is empty AND disconnected (graceful — `drain_and_close`'s
/// path) or when its current write is cancelled (`close`'s path). Never
/// touches `shared.conns` — teardown is always driven by the reader or an
/// explicit caller, never by this thread.
fn writer_loop(handle: SendableHandle, ov: Arc<OverlappedBuf>, conn_id: ConnId, rx: Receiver<WriteCmd>, events_tx: Sender<TransportEvent>) {
    while let Ok(cmd) = rx.recv() {
        let result = run_overlapped(handle.0, &ov, || unsafe {
            WriteFile(handle.0, cmd.bytes.as_ptr(), cmd.bytes.len() as u32, std::ptr::null_mut(), ov.as_mut_ptr())
        });
        match result {
            Ok(_) => {
                if let Some(marker) = cmd.marker {
                    let _ = events_tx.send(TransportEvent::Sent(conn_id, marker));
                }
            }
            Err(_e) => break, // cancelled, or the pipe is broken — nothing more to write
        }
    }
}

/// The client side of one voyage's pipe: a simple, blocking-style
/// connector and read/write pair (ADR 0041 step 5 round 1: step 6 drives
/// this synchronously, and so do this module's own tests).
pub struct PipeClient {
    #[allow(dead_code)] // held for its Drop (closes the pipe handle)
    handle: OwnedHandle,
    raw: SendableHandle,
    read_ov: OverlappedBuf,
}

impl std::fmt::Debug for PipeClient {
    /// Minimal for the same reason as `PipeServer`'s: this exists only to
    /// satisfy trait bounds like `Result::unwrap_err`'s.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeClient").finish_non_exhaustive()
    }
}

/// Connect to `\\.\pipe\sot-voyage-<voyage_id>`. Retries `CreateFileW`
/// (bounded, 2s total) on `ERROR_PIPE_BUSY` (all instances currently
/// connected — waits on `WaitNamedPipeW` between attempts, the documented
/// idiom) and `ERROR_FILE_NOT_FOUND` (the server has not called `bind` yet,
/// or every instance is between hand-off and the accept loop posting the
/// next one) — both are ordinary races in a healthy multi-client server,
/// not failures.
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
            let read_ov = OverlappedBuf::new().map_err(|e| PipeError::Io {
                op: "CreateEventW(client read)",
                source: e,
            })?;
            return Ok(PipeClient { handle, raw, read_ov });
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
    /// Blocking write of the whole buffer. Named pipes complete a
    /// `WriteFile` as one atomic operation (byte-mode, no partial writes
    /// to retry-loop over) — see the module doc's cancellation section for
    /// why this is still issued overlapped even though this call blocks
    /// the caller until it resolves.
    pub fn write_all(&self, bytes: &[u8]) -> Result<(), PipeError> {
        let ov = OverlappedBuf::new().map_err(|e| PipeError::Io {
            op: "CreateEventW(client write)",
            source: e,
        })?;
        run_overlapped(self.raw.0, &ov, || unsafe {
            WriteFile(self.raw.0, bytes.as_ptr(), bytes.len() as u32, std::ptr::null_mut(), ov.as_mut_ptr())
        })
        .map(|_| ())
        .map_err(|e| PipeError::Io { op: "WriteFile", source: e })
    }

    /// Blocking read into `buf`; `Ok(0)` means the server closed its end
    /// (ordered EOF, ADR 0041: "there is no `detach` op — ordered pipe EOF
    /// is detach"). Takes `&self`, not `&mut self` — see
    /// [`OverlappedBuf`]'s doc — so a second thread can call
    /// [`PipeClient::cancel_read`] to unblock a call already in flight.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, PipeError> {
        let result = run_overlapped(self.raw.0, &self.read_ov, || unsafe {
            ReadFile(self.raw.0, buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut(), self.read_ov.as_mut_ptr())
        });
        match result {
            Ok(n) => Ok(n as usize),
            Err(e)
                if matches!(e.raw_os_error(), Some(c) if c == ERROR_BROKEN_PIPE as i32 || c == ERROR_PIPE_NOT_CONNECTED as i32) =>
            {
                Ok(0)
            }
            Err(e) => Err(PipeError::Io { op: "ReadFile", source: e }),
        }
    }

    /// Unblock a [`PipeClient::read`] currently in flight on ANOTHER
    /// thread — a test harness's own clean-shutdown path, mirroring the
    /// server's `CancelIoEx` discipline so a client-driving thread can
    /// always be joined rather than left blocked forever on a peer that
    /// never sends and never closes.
    pub fn cancel_read(&self) {
        unsafe { CancelIoEx(self.raw.0, self.read_ov.as_ptr()) };
    }
}

// No explicit `Drop` impl: `handle: OwnedHandle` already closes the pipe
// on its own field drop, and there are no threads of this client's own to
// join (it is deliberately the "simple blocking-style" side — see the
// module doc).
