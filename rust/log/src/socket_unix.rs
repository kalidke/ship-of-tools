//! The L1-unix LU1b Unix-domain-socket transport (ADR 0043): a server for
//! `<runtime_dir>/voyage-<id>.sock` and `<runtime_dir>/supervisor-<h>.sock`.
//! It moves bytes and reports completions; it does not know about
//! mgmt/attach lanes, `hello`, opcodes, or checkpoints — `wire.rs` owns
//! every frame shape. Transport only: no dependency on the capsule or
//! `sot-capsule` bin, and none may be added here.
//!
//! This module is [`pipe_win`](crate::pipe_win)'s mechanical twin BY
//! PROPERTY, not by mechanism (ADR 0043 "Port by property, not by
//! mechanism") — same event vocabulary, same thread roles
//! (`sot-sock-accept`/`sot-sock-reaper`/`sot-sock-r-<id>`/
//! `sot-sock-w-<id>`, vs. `sot-pipe-accept`/`sot-pipe-reaper`/
//! `sot-pipe-r-<id>`/`sot-pipe-w-<id>`), same teardown order and the same
//! shared bounds (`crate::transport`). What Unix DELETES relative to that
//! module (ADR 0043 "What this deletes" / decision 5): the whole
//! completion-proof apparatus (`CompletionUnproven`, `mem::forget`,
//! `process::abort`) — POSIX `read`/`write` never borrow the caller's
//! buffer past the call, so there is nothing to prove or leak — and the
//! instance-recycling `InstanceRegistry` — a Unix listener's backlog
//! already holds pending connections for us; there is no fixed pool of
//! pre-created "instances" whose name-holding requires manual upkeep.
//!
//! # No instance registry — the kernel's own backlog does that job
//!
//! `pipe_win.rs` needs an [`InstanceRegistry`](crate::pipe_win) because
//! `CreateNamedPipeW` allocates a FIXED pool of named-pipe instances and
//! the pipe NAME is only held while at least one instance exists — so an
//! instance must be recycled (never closed) to keep accepting without
//! losing the name. A Unix listening socket has no such pool: the kernel
//! itself queues pending connections in the listen backlog, and the name
//! (the socket's directory entry) is held by the LISTENER FD alone, for
//! as long as that fd stays open — no per-connection "instance" ever
//! needs to be separately created, recycled, or retained-dead to keep the
//! name alive. This is why this module has no equivalent of
//! `AcceptState::recycled`/`retained_dead`/`current`, no squat-detection
//! probe, and a much shorter accept loop.
//!
//! # Cancellation: `shutdown(2)`, no per-op cancel primitive
//!
//! Windows needs one [`IoSlot`](crate::pipe_win) per direction per
//! connection because `CancelIoEx` targets a SPECIFIC pending overlapped
//! op. POSIX has no equivalent of targeting one blocked call from another
//! thread — the primitive that generalizes is `shutdown(2)` on the
//! connection's fd: it unblocks a blocked `read` (returns `0`, ordinary
//! EOF) and a blocked `write` (returns a partial count or `EPIPE`) BOTH AT
//! ONCE, from any thread, without needing to know which direction (if
//! either) is currently mid-call. So [`teardown_if_present`] issues ONE
//! `shutdown(SHUT_RDWR)` regardless of which of the three triggers (an
//! explicit [`SocketServer::close`], the reader's own EOF/error signal, or
//! the writer's own error signal) requested it — the direct analogue of
//! `pipe_win`'s own `teardown_if_present` unconditionally cancelling BOTH
//! its read and write `IoSlot`s no matter which one signalled first.
//!
//! # The accept loop wakes via `poll(2)` over a self-pipe, never a
//! connect-to-self
//!
//! [`SocketServer::disconnect_listener`] must wake a blocked acceptor
//! without ever dialing the socket itself (a connect-to-self is a real,
//! observable client from the outside — exactly what a rival-bind test
//! must never see). The acceptor instead blocks in `libc::poll` over TWO
//! fds: the listener, and the read end of a `libc::pipe(2)` (CLOEXEC and
//! NONBLOCK applied via `fcntl` — portable across Linux and macOS/BSD,
//! unlike Linux's own combined-flag `pipe2(2)`) pair
//! this server owns. `disconnect_listener` writes one byte to the write
//! end; the poll wakes, the accept loop's own `accept_stopping` check (set
//! by the SAME call, under the same store-then-notify ordering `pipe_win`
//! uses for its own `AcceptState::accept_stopping`) fires, and the loop
//! returns without ever accepting the wake byte as a client.
//!
//! # Two distinct "stop" signals — the same split `pipe_win` makes
//!
//! [`ServerShared::dropping`] is set ONLY by
//! [`SocketServer::disconnect_listener`] (via `Drop`, or the writer loop's
//! own explicit call) and exists SOLELY as [`send_lifecycle_event`]'s
//! escape hatch — the one case where continuing to retry a full events
//! channel is pure busywork because nothing could ever drain it again.
//! [`ServerShared::accept_stopping`] is set by disconnect_listener TOO, but
//! ALSO by a persistent accept failure ([`terminalize_accept_loop`]) that
//! has nothing to do with the whole server being dropped — the consumer is
//! very much still alive and needs to actually RECEIVE the `AcceptError`
//! event that failure produces. Conflating the two would let a transient
//! events-channel backlog silently swallow that very event at the moment
//! it matters most; `pipe_win.rs` keeps the identical split between its
//! own `ServerShared::dropping` and `AcceptState::accept_stopping` for the
//! same reason.
//!
//! # Reliable lifecycle delivery, byte-bounded both directions
//!
//! Identical contract to `pipe_win.rs` (see that module's doc for the full
//! argument): `Accepted`/`Sent`/`Closed`/`AcceptError` retry against a full
//! `events()` channel indefinitely (escaping only via `dropping`); `Bytes`
//! is the one event kind allowed to be abandoned, and abandoning it always
//! forces a guaranteed `Closed` through the same reliable path. Outbound:
//! [`crate::transport::OutboundBudget`] reserves bytes per connection,
//! including the in-flight item, released only once the write physically
//! completes.
//!
//! # Security: the runtime dir's ancestors are not trusted
//!
//! A private LEAF directory reached through a world-writable, non-sticky
//! parent, or through a symlinked ancestor, would pass a by-PATH check
//! (`is_private_dir`, `chmod`, `stat`) and still let a same-instant
//! ancestor swap redirect every later by-path operation to an attacker's
//! own directory. So after [`ensure_private_runtime_dir`]'s by-path
//! pre-check (create-if-absent, or a first-pass verify), [`bind_named`]
//! [`open`](libc::open)s the directory itself with `O_NOFOLLOW` and
//! `fstat`s the resulting FD (real directory, owned by this uid,
//! owner-only) — the check that actually counts. EVERY later filesystem
//! step (the stale-unlink, the `bind`, the `chmod`+verify, and
//! [`SocketServer::disconnect_listener`]'s own eventual unlink) is then
//! anchored to THAT VERIFIED FD via `*at()` calls (`unlinkat`/`fchmodat`/
//! `fstatat`), never a fresh by-path lookup that could re-walk a since-
//! swapped ancestor. On Linux, even `bind(2)` itself goes through the
//! anchored `/proc/self/fd/<dirfd>/<name>` path rather than the ordinary
//! path string, for the identical reason; other Unix targets keep an
//! ordinary by-path `bind` (macOS/BSD support here is experimental — see
//! ADR 0043's own "Open for the maintainer" — so this document the
//! narrower guarantee there rather than adding more platform-specific
//! code to close it). What this does NOT defend: an attacker sharing this
//! process's OWN uid (out of scope — the `is_private_dir`/`fstat`
//! ownership check is exactly the boundary this crate draws), and a
//! CLIENT that connects by the real path through an ancestor swapped
//! AFTER `bind` returns — that client reaches whatever the swapped
//! ancestor now resolves to, which is why LU1c's same-user challenge (not
//! this module) is what a connecting client ultimately trusts, never a
//! bare successful `connect()`.
//!
//! # Visibility
//!
//! Every type below is `pub`, not `pub(crate)` — `tests/socket_unix.rs` is
//! a separate integration-test crate and can only ever reach a library's
//! `pub` items, the same reason `pipe_win.rs`'s own types are `pub`.

#![cfg(unix)]

use crate::transport::{
    join_within, OutboundBudget, StartGate, BYTES_ABANDON_AFTER, EVENTS_CHANNEL_CAP,
    EVENTS_RETRY_INTERVAL, READ_BUF_LEN, TEARDOWN_AGGREGATE_DEADLINE,
};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

/// Extra capacity on the bounded reaper inbox beyond `max_connections` —
/// mirrors `pipe_win::REAPER_INBOX_SLACK`: a connection's own
/// at-most-once teardown flag already caps live `Torn` messages at one
/// per open connection, so the only other traffic this inbox ever
/// carries is `Drop`'s own single `Shutdown`.
const REAPER_INBOX_SLACK: usize = 1;

/// `sockaddr_un::sun_path`'s usable byte length — its own array capacity
/// minus the terminating NUL every `bind`/`connect` needs (ADR 0043
/// decision 1: "`sun_path` is 108 bytes on Linux including the NUL", i.e.
/// 107 usable; macOS/BSD's own `sockaddr_un` is smaller, 104 total / 103
/// usable). Computed from the REAL platform struct rather than a
/// hardcoded Linux-shaped literal, so a path this crate accepts as fitting
/// is GUARANTEED to fit the `sun_path` array it is about to be copied
/// into on whichever Unix this actually runs on, never silently truncated
/// by a bound sized for a different platform's layout.
fn max_sun_path_bytes() -> usize {
    // SAFETY: a zeroed `sockaddr_un` is a valid value of that type; this
    // reads its `sun_path` field's own array length only -- the value is
    // never passed to an OS call.
    let addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_path.len() - 1
}

/// Identifies one accepted connection for the lifetime of a
/// [`SocketServer`]. Assigned sequentially; never reused.
pub type ConnId = u64;

/// An opaque, caller-assigned correlation tag for one
/// [`SocketServer::send`] call, echoed back on [`TransportEvent::Sent`]
/// when the OS reports that send's `write` has PHYSICALLY completed.
pub type SendMarker = u64;

/// Why a connection ended, reported once per connection on
/// [`TransportEvent::Closed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosedReason {
    /// The peer disconnected (or this side observed a broken/reset/
    /// unconnected stream) — detected by the connection's own reader
    /// loop, or (rarely) its writer.
    Eof,
    /// [`SocketServer::close`] tore this connection down.
    Closed,
    /// An I/O error other than a recognized disconnect ended a
    /// connection's reader or writer loop, or its `Bytes` delivery was
    /// abandoned — always paired with this guaranteed notification,
    /// never a silent stream gap.
    Error(String),
}

/// This transport's event surface to its consumer. Delivered over
/// [`SocketServer::events`] in the order this module observed them; the
/// consumer feeds `Bytes` payloads to its own [`crate::wire::FrameSplitter`]
/// per connection. Field-for-field identical to `pipe_win::TransportEvent`
/// so [`crate::socket_transport`]'s own `translate()` is the same five-arm
/// map as `pipe_transport`'s.
#[derive(Debug)]
pub enum TransportEvent {
    /// A new connection accepted; `send`/`close` may now target it.
    Accepted(ConnId),
    /// Raw bytes read from a connection, in the order read. Never empty.
    Bytes(ConnId, Vec<u8>),
    /// The `write` for a marker-tagged [`SocketServer::send`] call has
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
///
/// `Cancelled` and `ConcurrentSubmit` are declared now, unused by anything
/// in this file, for the LU1c client (`SocketClient::read`/`write_all`/
/// `cancel`) that lands on top of this module — so this enum's SHAPE does
/// not change twice. Same reasoning as `PipeError`'s own identically
/// unused-by-the-server variants.
#[derive(Debug, thiserror::Error)]
pub enum SocketError {
    #[error("invalid voyage id {0:?}: must be the canonical lowercase-hyphenated form of an RFC 4122 UUID")]
    InvalidVoyageId(String),
    #[error("max_connections must be between 1 and 255")]
    InvalidMaxConnections,
    #[error("socket path {0:?} exceeds sun_path's {limit}-byte limit", limit = max_sun_path_bytes())]
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
    /// LU1c: a client-side blocking call was cancelled from another
    /// thread.
    #[error("operation cancelled")]
    Cancelled,
    /// LU1c: a second same-direction client call (e.g. two concurrent
    /// `read`s) was rejected before it ever touched the OS.
    #[error("another operation is already pending on this client's same direction")]
    ConcurrentSubmit,
}

// ---------------------------------------------------------------------
// Paths (ADR 0043 decision 1).
// ---------------------------------------------------------------------

/// `<runtime_dir>/voyage-<voyage_id>.sock`, after validating `voyage_id`
/// is the canonical lowercase-hyphenated form of an RFC 4122 UUID — the
/// same check [`crate::pipe_win`]'s own `validate_voyage_id` runs,
/// delegating to the SAME `pointer::canonical_voyage_id` (one
/// implementation, not two that can drift).
pub fn voyage_socket_path(voyage_id: &str) -> Result<PathBuf, SocketError> {
    validate_voyage_id(voyage_id)?;
    socket_path(&format!("voyage-{voyage_id}"))
}

/// `<runtime_dir>/supervisor-<h>.sock` — the supervisor lane's own
/// socket, otherwise identical to [`voyage_socket_path`]. `h` is the
/// caller's own stable hash of the canonicalized state-dir path; this
/// function neither derives nor validates it as a voyage id, unlike
/// [`voyage_socket_path`] — matching `pipe_win::supervisor_pipe_name_wide`.
pub fn supervisor_socket_path(h: &str) -> Result<PathBuf, SocketError> {
    socket_path(&format!("supervisor-{h}"))
}

fn validate_voyage_id(voyage_id: &str) -> Result<(), SocketError> {
    if crate::pointer::canonical_voyage_id(voyage_id).is_some() {
        Ok(())
    } else {
        Err(SocketError::InvalidVoyageId(voyage_id.to_string()))
    }
}

fn socket_path(file_name: &str) -> Result<PathBuf, SocketError> {
    let dir = crate::state_dir::runtime_dir().map_err(SocketError::RuntimeDir)?;
    let path = dir.join(format!("{file_name}.sock"));
    if path.as_os_str().as_bytes().len() > max_sun_path_bytes() {
        return Err(SocketError::PathTooLong(path));
    }
    Ok(path)
}

// ---------------------------------------------------------------------
// The server.
// ---------------------------------------------------------------------

/// One queued outbound send: raw bytes, plus an optional marker to echo
/// back on physical write completion. Identical shape to
/// `pipe_win::WriteCmd`.
struct WriteCmd {
    bytes: Vec<u8>,
    marker: Option<SendMarker>,
}

/// One live connection's threads, handle, and budget — owned by the
/// `conns` map for the connection's whole life; removed and torn down
/// exclusively by [`teardown_if_present`], called exclusively from
/// [`reaper_loop`] (mirrors `pipe_win::ConnHandle`'s own "one registry,
/// one closer" invariant — here there is no separate registry at all,
/// since `Arc<UnixStream>` closes its own fd on its own last drop, one
/// unavoidable owner).
struct ConnHandle {
    stream: Arc<UnixStream>,
    outbound: Arc<OutboundBudget>,
    sender: Sender<WriteCmd>,
    reader_jh: JoinHandle<()>,
    writer_jh: JoinHandle<()>,
    /// At-most-once teardown gate shared with the reader/writer threads
    /// — see [`request_teardown`]. Doubles as the abandon-early signal
    /// [`deliver_bytes`] polls (the direct analogue of `pipe_win`'s own
    /// `IoSlot::is_closing`).
    torn_down_requested: Arc<AtomicBool>,
}

/// A message to [`reaper_loop`] — the only thread that ever removes a
/// registered connection from `conns` or joins its threads.
enum ReaperMsg {
    /// A connection ended (natural EOF/error, or a caller's `close`).
    Torn(ConnId, ClosedReason),
    /// The server is being dropped: drain and tear down every connection
    /// still in `conns` (no `Closed` event for these — nothing could
    /// ever observe it), then stop.
    Shutdown,
}

struct ServerShared {
    conns: Mutex<HashMap<ConnId, ConnHandle>>,
    next_id: AtomicU64,
    reaper_tx: SyncSender<ReaperMsg>,
    events_tx: SyncSender<TransportEvent>,
    max_connections: u32,
    /// TWO jobs, both won via `compare_exchange` (Codex review finding 1):
    /// (a) the ONE escape for [`send_lifecycle_event`]'s otherwise-
    /// indefinite retry loop once true (nothing could ever drain
    /// `events()` again), and (b) the exactly-once latch on
    /// [`SocketServer::disconnect_listener`]'s own unlink — a repeat call
    /// (an explicit one, then `Drop`'s own; or two racing threads) must
    /// unlink at most once, since a second unconditional unlink could
    /// delete a REPLACEMENT server's endpoint bound at the same path
    /// after this one tore down. See the module doc's "Two distinct
    /// 'stop' signals" section for why this stays a SEPARATE flag from
    /// `accept_stopping`.
    dropping: AtomicBool,
    /// The accept loop should stop (and, once observed, HAS stopped)
    /// accepting new connections — set by `disconnect_listener` OR by a
    /// persistent accept failure (`terminalize_accept_loop`). See the
    /// module doc.
    accept_stopping: AtomicBool,
    /// The verified runtime directory's own fd (module doc "Security"):
    /// opened `O_NOFOLLOW`+`O_DIRECTORY`, `fstat`-verified, and kept open
    /// for this server's whole life so every later `*at()` call (the
    /// stale-unlink at bind time, and `disconnect_listener`'s own eventual
    /// unlink) is anchored to THIS inode, never a fresh by-path lookup.
    dir_fd: OwnedFd,
    /// The socket's own file name (`voyage-<id>.sock` /
    /// `supervisor-<h>.sock`) inside `dir_fd` — paired with it for every
    /// `*at()` call. NUL-terminated once, up front, for reuse.
    file_name: CString,
    /// Write end of the self-pipe the acceptor's `poll(2)` also watches;
    /// writing one byte wakes it without ever dialing the socket itself
    /// (no connect-to-self). Closed when `ServerShared` finally drops
    /// (every `Arc` clone gone, so no concurrent access is possible).
    wake_write: OwnedFd,
}

/// The server side of one voyage's (or the supervisor lane's) socket:
/// [`SocketServer::bind`] creates the listener and starts accepting;
/// connections and their bytes/completions/closes surface on
/// [`SocketServer::events`]. Mirrors `pipe_win::PipeServer`'s own
/// lifetime rule verbatim: `bind` is an explicit constructor the CALLER
/// is responsible for invoking only after the endpoint's lifetime lock is
/// held (ADR 0043 decision 2 — the voyage writer lock for
/// `voyage-<id>.sock`, the supervisor fence for `supervisor-<h>.sock`),
/// and for dropping the returned `SocketServer` before releasing that
/// lock.
pub struct SocketServer {
    shared: Arc<ServerShared>,
    events_rx: Receiver<TransportEvent>,
    accept_jh: Option<JoinHandle<()>>,
    reaper_jh: Option<JoinHandle<()>>,
    /// Reader/writer `JoinHandle`s for every connection
    /// [`SocketServer::disconnect_listener`] closed directly — their
    /// `ConnHandle` never reaches the reaper (it drained `shared.conns`
    /// itself), so nothing else would ever join them.
    /// [`SocketServer::join_workers`] joins every entry here under the
    /// SAME shared deadline as the acceptor and reaper.
    detached_workers: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for SocketServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocketServer").finish_non_exhaustive()
    }
}

impl SocketServer {
    /// Bind `<runtime_dir>/voyage-<voyage_id>.sock` and start accepting.
    /// `max_connections` must be in `1..=255`.
    pub fn bind(voyage_id: &str, max_connections: u32) -> Result<Self, SocketError> {
        let path = voyage_socket_path(voyage_id)?;
        Self::bind_named(path, max_connections)
    }

    /// The supervisor lane's own socket,
    /// `<runtime_dir>/supervisor-<h>.sock` — otherwise identical to
    /// [`Self::bind`].
    pub fn bind_supervisor(h: &str, max_connections: u32) -> Result<Self, SocketError> {
        let path = supervisor_socket_path(h)?;
        Self::bind_named(path, max_connections)
    }

    fn bind_named(path: PathBuf, max_connections: u32) -> Result<Self, SocketError> {
        if !(1..=255).contains(&max_connections) {
            return Err(SocketError::InvalidMaxConnections);
        }
        let dir = path
            .parent()
            .expect("a socket path built by socket_path() always has a parent (the runtime dir)");
        ensure_private_runtime_dir(dir)?;
        // Module doc "Security": the by-path pre-check above is not the
        // authoritative one. Open the directory itself `O_NOFOLLOW` and
        // `fstat` the resulting fd -- keeping it open for this server's
        // whole life so every later `*at()` call is anchored to THIS
        // verified inode, never a fresh path lookup that could re-walk a
        // since-swapped ancestor.
        let dir_fd = open_verified_dir_fd(dir)?;
        let file_name = path
            .file_name()
            .expect("a socket path built by socket_path() always has a file name");
        let file_name = CString::new(file_name.as_bytes()).map_err(|_| SocketError::Io {
            op: "CString::new(socket file name)",
            source: io::Error::new(io::ErrorKind::InvalidInput, "socket file name contains a NUL byte"),
        })?;

        let listener = create_and_bind_listener(dir_fd.as_raw_fd(), &file_name, &path, max_connections)?;

        // The wake self-pipe (module doc: "the accept loop wakes via
        // poll(2) over a self-pipe"). O_NONBLOCK on both ends: the
        // accept loop's own drain read must never block, and a write
        // from `disconnect_listener` must never block either (its own
        // "never blocks" contract) -- one byte always fits in a fresh
        // pipe's buffer, but non-blocking costs nothing and removes any
        // doubt.
        //
        // `pipe(2)` + `fcntl` (not Linux's own combined-flag `pipe2(2)`):
        // macOS/BSD has no `pipe2` at all, so this crate sets CLOEXEC and
        // NONBLOCK as two ordinary, portable `fcntl` calls per fd instead
        // -- identical end state, one extra syscall pair, and it now
        // compiles (and behaves identically) on every Unix target this
        // workspace's CI checks, not only Linux.
        let mut fds: [RawFd; 2] = [-1, -1];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if rc != 0 {
            let err = SocketError::Io {
                op: "pipe(wake)",
                source: io::Error::last_os_error(),
            };
            unsafe { libc::unlinkat(dir_fd.as_raw_fd(), file_name.as_ptr(), 0) };
            return Err(err);
        }
        // SAFETY: `pipe` just returned these two fds; each is valid,
        // open, and not owned by anything else yet.
        let wake_read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let wake_write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        for fd in [wake_read.as_raw_fd(), wake_write.as_raw_fd()] {
            if let Err(e) = set_cloexec(fd).and_then(|()| set_nonblocking(fd)) {
                let err = SocketError::Io {
                    op: "fcntl(wake pipe)",
                    source: e,
                };
                unsafe { libc::unlinkat(dir_fd.as_raw_fd(), file_name.as_ptr(), 0) };
                return Err(err);
            }
        }

        let (events_tx, events_rx) = mpsc::sync_channel(EVENTS_CHANNEL_CAP);
        let (reaper_tx, reaper_rx) =
            mpsc::sync_channel(max_connections as usize + REAPER_INBOX_SLACK);

        let shared = Arc::new(ServerShared {
            conns: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            reaper_tx,
            events_tx,
            max_connections,
            dropping: AtomicBool::new(false),
            accept_stopping: AtomicBool::new(false),
            dir_fd,
            file_name,
            wake_write,
        });

        // Spawn the reaper FIRST -- if the accept thread then fails to
        // spawn, unwind the reaper (nothing queued yet, so its own
        // `Shutdown` drain is instant) rather than leave it running
        // forever with no accept thread able to feed it. Mirrors
        // `pipe_win::PipeServer::bind_named`'s own ordering.
        let reaper_jh = match thread::Builder::new()
            .name("sot-sock-reaper".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || reaper_loop(shared, reaper_rx)
            }) {
            Ok(jh) => jh,
            Err(e) => {
                unsafe { libc::unlinkat(shared.dir_fd.as_raw_fd(), shared.file_name.as_ptr(), 0) };
                return Err(SocketError::Io {
                    op: "spawn reaper thread",
                    source: e,
                });
            }
        };

        let accept_jh = thread::Builder::new()
            .name("sot-sock-accept".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || accept_loop(shared, listener, wake_read)
            });
        let accept_jh = match accept_jh {
            Ok(jh) => jh,
            Err(e) => {
                let _ = shared.reaper_tx.send(ReaperMsg::Shutdown);
                reaper_jh.join().ok();
                unsafe { libc::unlinkat(shared.dir_fd.as_raw_fd(), shared.file_name.as_ptr(), 0) };
                return Err(SocketError::Io {
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
    /// convention (a `Receiver` is not `Sync`).
    pub fn events(&self) -> &Receiver<TransportEvent> {
        &self.events_rx
    }

    /// Queue `bytes` for `conn_id`, tagged with `marker` if the caller
    /// wants a [`TransportEvent::Sent`] once the OS write physically
    /// completes. `bytes` must be non-empty. Non-blocking: a full
    /// outbound budget or an unknown/already-closed connection both
    /// return `Err` immediately — backpressure POLICY belongs to
    /// whoever calls this.
    pub fn send(
        &self,
        conn_id: ConnId,
        bytes: Vec<u8>,
        marker: Option<SendMarker>,
    ) -> Result<(), SocketError> {
        if bytes.is_empty() {
            return Err(SocketError::EmptyPayload);
        }
        // Parity with `pipe_win::PipeServer::send`'s own near-unreachable
        // representable-size check -- POSIX `write(2)` has no fixed
        // per-call ceiling this transport itself needs to enforce (the
        // writer loop below loops over partial writes), but a single
        // absurdly large payload is still rejected loudly rather than
        // silently accepted only to blow the (far smaller)
        // `OUTBOUND_BUDGET_BYTES` check that follows.
        if bytes.len() > isize::MAX as usize {
            return Err(SocketError::PayloadTooLarge(bytes.len()));
        }
        let len = bytes.len();
        let map = self.shared.conns.lock().unwrap();
        let conn = map
            .get(&conn_id)
            .ok_or(SocketError::UnknownConnection(conn_id))?;
        if !conn.outbound.try_reserve(len) {
            return Err(SocketError::QueueFull(conn_id));
        }
        if conn.sender.send(WriteCmd { bytes, marker }).is_err() {
            conn.outbound.release(len);
            return Err(SocketError::UnknownConnection(conn_id));
        }
        Ok(())
    }

    /// Request that `conn_id` be torn down: cancelled (`shutdown(2)`),
    /// both threads joined. Fire-and-forget — this enqueues the request
    /// at most once for the reaper thread; completion is observed as
    /// [`TransportEvent::Closed`]. A no-op if `conn_id` is already gone or
    /// already has a teardown in flight.
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

    /// Phase one of teardown: make the socket NAME disappear AND issue
    /// cancellation to every worker — synchronous, no blocking join.
    /// [`ServerShared::dropping`]'s `compare_exchange` is the unlink's
    /// EXACTLY-ONCE latch (Codex review finding 1, property 5): a second
    /// call to this method — or `Drop`'s own call, always made after an
    /// explicit one — must NOT unlink again, because by then a caller
    /// could legitimately have bound a REPLACEMENT `SocketServer` at the
    /// identical path (the same voyage's next leg, say), and a second
    /// unconditional unlink would delete THAT server's endpoint instead
    /// of this (already torn-down) one's. Unlinking is therefore anchored
    /// via `unlinkat` to `dir_fd`/`file_name` (module doc "Security"),
    /// never a fresh by-path lookup, and gated on actually WINNING the
    /// `dropping` transition — every other step below stays unconditional
    /// and idempotent, matching before: wake the accept loop out of
    /// `poll(2)`, then `shutdown(SHUT_RDWR)` every currently-live
    /// connection's stream (property 12) and move its reader/writer
    /// threads into `detached_workers` for [`Self::join_workers`] to join
    /// later.
    pub fn disconnect_listener(&mut self) {
        let unlink_once = self
            .shared
            .dropping
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        self.shared.accept_stopping.store(true, Ordering::Release);
        if unlink_once {
            // SAFETY: `dir_fd` was opened, `O_NOFOLLOW`+`fstat`-verified,
            // and kept open for this server's whole life; `unlinkat`
            // removes only the entry named `file_name` inside THAT
            // directory, never re-resolving any path.
            unsafe {
                libc::unlinkat(self.shared.dir_fd.as_raw_fd(), self.shared.file_name.as_ptr(), 0);
            }
        }
        let wake_byte = [0u8; 1];
        // A failed/short write here just means the acceptor will notice
        // `accept_stopping` on its NEXT ordinary wakeup instead of this
        // one -- never a correctness issue, only latency, and one this
        // transport does not otherwise promise a bound on beyond
        // `TEARDOWN_AGGREGATE_DEADLINE`'s own join.
        let _ = unsafe {
            libc::write(
                self.shared.wake_write.as_raw_fd(),
                wake_byte.as_ptr().cast(),
                1,
            )
        };
        let drained: Vec<ConnHandle> = {
            let mut map = self.shared.conns.lock().unwrap();
            map.drain().map(|(_, conn)| conn).collect()
        };
        for conn in drained {
            // Fast-exits a reader stuck retrying `deliver_bytes` against
            // a saturated events channel -- the same role `pipe_win`'s
            // own `IoSlot::is_closing` plays there once its own
            // `cancel_registered` latches `Closing`.
            conn.torn_down_requested.store(true, Ordering::Release);
            unsafe { libc::shutdown(conn.stream.as_raw_fd(), libc::SHUT_RDWR) };
            drop(conn.sender); // unblocks a writer idle-waiting on `recv`
            self.detached_workers.push(conn.reader_jh);
            self.detached_workers.push(conn.writer_jh);
        }
    }

    /// Phase two: tell the reaper to drain (a no-op for any connection
    /// `disconnect_listener` already claimed), then wait for the accept
    /// thread, the reaper thread, AND every detached connection worker
    /// `disconnect_listener` stashed — ALL sharing ONE absolute
    /// `deadline`. `true` iff every one finished within budget; `false`
    /// (LOUD — the caller MUST treat this as terminal) on expiry. Call
    /// [`disconnect_listener`](Self::disconnect_listener) first — this
    /// method does not call it, so the two phases stay independently
    /// observable (and independently testable).
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

impl Drop for SocketServer {
    /// The two teardown phases in order, with a FRESH pinned 20 s
    /// budget computed here — mirrors `pipe_win::PipeServer`'s own
    /// `Drop`. This is the SAFETY-NET path; the designed path computes
    /// ONE deadline in the capsule's own run loop and calls both methods
    /// explicitly with it.
    fn drop(&mut self) {
        self.disconnect_listener();
        let deadline = Instant::now() + TEARDOWN_AGGREGATE_DEADLINE;
        if !self.join_workers(deadline) {
            eprintln!(
                "sot-sock: teardown did not complete within its {TEARDOWN_AGGREGATE_DEADLINE:?} \
                 aggregate deadline; a worker thread may still be running"
            );
        }
    }
}

/// Create/verify the socket's parent directory (ADR 0043 decision 3): a
/// missing directory is created EXCLUSIVELY at mode `0700` (plain,
/// non-`_all` `create` + `DirBuilderExt::mode` maps to one `mkdir(2)` —
/// atomic, no window for a racing attacker to land a symlink in); a
/// present one gets a by-PATH PRE-check via
/// [`crate::state_dir::is_private_dir`] (lstat-based: real directory,
/// owned by this uid, owner-only) — NOT the authoritative check (module
/// doc "Security"): [`open_verified_dir_fd`], called immediately
/// afterward, re-verifies the SAME properties against the actually-opened
/// fd, which is what every later filesystem step is anchored to. Mirrors
/// `rust/backend/src/paths.rs::secure_private_dir`'s own contract exactly
/// (that function lives in a crate `sot-log` cannot depend on, so this is
/// a small, deliberate, self-contained duplicate rather than a new
/// dependency edge).
fn ensure_private_runtime_dir(dir: &Path) -> Result<(), SocketError> {
    use std::os::unix::fs::DirBuilderExt;
    match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => std::fs::DirBuilder::new()
            .mode(0o700)
            .create(dir)
            .map_err(|e| SocketError::Io {
                op: "mkdir(runtime dir)",
                source: e,
            }),
        Err(e) => Err(SocketError::Io {
            op: "stat(runtime dir)",
            source: e,
        }),
        Ok(_) if crate::state_dir::is_private_dir(dir) => Ok(()),
        Ok(_) => Err(SocketError::Io {
            op: "verify runtime dir",
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} is not a private, owner-only directory (not a symlink, owned by this \
                     uid, mode 0700)",
                    dir.display()
                ),
            ),
        }),
    }
}

/// Module doc "Security", the AUTHORITATIVE check: open `dir` with
/// `O_NOFOLLOW`+`O_DIRECTORY` (a symlink leaf is rejected outright by the
/// OS itself, `ELOOP`, before this function's own check ever runs) and
/// `fstat` the resulting fd — real directory, owned by this uid,
/// owner-only. `ensure_private_runtime_dir`'s own by-path check only
/// APPROXIMATES this (a TOCTOU window exists between any by-path stat and
/// a later by-path operation); this one is what every later `*at()` call
/// in [`create_and_bind_listener`] and [`SocketServer::disconnect_listener`]
/// is anchored to, so the fd is kept open for the whole server's life
/// rather than closed once this check passes.
fn open_verified_dir_fd(dir: &Path) -> Result<OwnedFd, SocketError> {
    let c_dir = CString::new(dir.as_os_str().as_bytes()).map_err(|_| SocketError::Io {
        op: "open(runtime dir)",
        source: io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime dir path contains a NUL byte",
        ),
    })?;
    let raw = unsafe {
        libc::open(
            c_dir.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(SocketError::Io {
            op: "open(runtime dir)",
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: `raw` is a freshly opened, valid, not-otherwise-owned fd.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstat(fd.as_raw_fd(), &mut st) };
    if rc != 0 {
        return Err(SocketError::Io {
            op: "fstat(runtime dir fd)",
            source: io::Error::last_os_error(),
        });
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFDIR
        || st.st_uid != crate::state_dir::current_uid()
        || st.st_mode & 0o077 != 0
    {
        return Err(SocketError::Io {
            op: "verify runtime dir fd",
            source: io::Error::other(
                "the opened runtime dir fd is not a real, owner-only directory owned by this uid",
            ),
        });
    }
    Ok(fd)
}

/// ADR 0043 decision 2: the caller HOLDS the endpoint's lifetime lock, so
/// a pre-existing socket file named `file_name` inside `dir_fd` is stale
/// by construction — unlinked via `unlinkat`, never probed. Module doc
/// "Security": every filesystem step below is anchored to `dir_fd` — the
/// ALREADY-VERIFIED directory fd [`open_verified_dir_fd`] returned, kept
/// open for the server's whole life — via `unlinkat`/`fchmodat`/`fstatat`,
/// never a fresh by-path lookup that could land in a since-swapped
/// ancestor. On Linux, even `bind` itself goes through the anchored
/// `/proc/self/fd/<dir_fd>/<file_name>` path (a magic symlink that always
/// resolves against the FD's own identity, never re-walking `path`'s own
/// ancestors) for the identical reason; other Unix targets bind by the
/// ordinary `path` (a narrower, documented guarantee there — macOS/BSD
/// support is experimental, ADR 0043's own "Open for the maintainer").
/// `libc::socket`/`bind`/`fchmodat`+verify/`listen` in that exact order
/// (ADR 0043 decision 3): `UnixListener::bind` alone would `bind` AND
/// `listen` together, leaving a window where the socket exists (and,
/// once `listen`ed, is connectable) before this transport has verified
/// its own permissions — so the listener is built by hand from raw
/// `libc` calls instead, and wrapped in a `UnixListener` only once
/// `listen` has already run.
fn create_and_bind_listener(
    dir_fd: RawFd,
    file_name: &CStr,
    path: &Path,
    max_connections: u32,
) -> Result<UnixListener, SocketError> {
    let rc = unsafe { libc::unlinkat(dir_fd, file_name.as_ptr(), 0) };
    if rc != 0 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::NotFound {
            return Err(SocketError::Io {
                op: "unlinkat(stale socket)",
                source: err,
            });
        }
    }

    // Plain `SOCK_STREAM`, not Linux's own `SOCK_STREAM | SOCK_CLOEXEC`:
    // `SOCK_CLOEXEC` as a `socket(2)` type flag is a Linux (and some BSD)
    // extension macOS lacks entirely -- `set_cloexec` below is the
    // portable two-call equivalent, same end state on every target.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(SocketError::Io {
            op: "socket(AF_UNIX)",
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: `raw` is a freshly created, valid, not-otherwise-owned fd.
    // Wrapped immediately so every early return below closes it.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    set_cloexec(fd.as_raw_fd()).map_err(|e| SocketError::Io {
        op: "fcntl(FD_CLOEXEC socket)",
        source: e,
    })?;

    #[cfg(target_os = "linux")]
    assert_domain_is_unix(fd.as_raw_fd());

    // Whichever bind mechanism is used below, every future CLIENT's own
    // `connect()` must still address this socket by the REAL path -- so
    // ITS length is what must fit `sun_path`, independent of the (usually
    // much shorter) `/proc/self/fd` bind trick's own length.
    // `socket_path()` already enforces this for the production call
    // sites; reasserted here since this function is reachable directly (a
    // future in-crate caller, or a test).
    if path.as_os_str().as_bytes().len() > max_sun_path_bytes() {
        return Err(SocketError::PathTooLong(path.to_path_buf()));
    }

    #[cfg(target_os = "linux")]
    let addr_bytes_owned = {
        let mut v = format!("/proc/self/fd/{dir_fd}/").into_bytes();
        v.extend_from_slice(file_name.to_bytes());
        v
    };
    #[cfg(not(target_os = "linux"))]
    let addr_bytes_owned = path.as_os_str().as_bytes().to_vec();
    let addr_bytes = &addr_bytes_owned[..];
    // Belt and braces: the mechanism-specific bytes actually handed to
    // `bind` get their OWN reassertion too (on Linux this is a much
    // shorter string than `path`'s own and will essentially never trip;
    // on other Unix it's the identical check as above).
    if addr_bytes.len() > max_sun_path_bytes() {
        return Err(SocketError::PathTooLong(path.to_path_buf()));
    }
    // SAFETY: a zeroed `sockaddr_un` is a valid value of that type
    // (all-zero bytes for every field, including a NUL-filled
    // `sun_path`).
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (dst, &b) in addr.sun_path.iter_mut().zip(addr_bytes) {
        *dst = b as libc::c_char;
    }
    let addr_len = (std::mem::size_of::<libc::sa_family_t>() + addr_bytes.len() + 1)
        as libc::socklen_t; // +1: the NUL terminator `sockaddr_un` expects, already zeroed in.

    let rc = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            std::ptr::addr_of!(addr).cast(),
            addr_len,
        )
    };
    if rc != 0 {
        // ADR 0043 decision 2: a real error, never a retry -- the caller
        // holds the endpoint's lifetime lock, so nothing legitimate
        // should ever be racing this bind.
        return Err(SocketError::Io {
            op: "bind(AF_UNIX)",
            source: io::Error::last_os_error(),
        });
    }

    // fchmodat 0600, then VERIFY via fstatat, BEFORE `listen` (ADR 0043
    // decision 3): no connection can exist before `listen` runs, so this
    // closes the window completely rather than merely narrowing it. Both
    // anchored to `dir_fd`/`file_name` (module doc "Security"), never a
    // fresh by-path lookup -- `fchmod` on the SOCKET FD ITSELF was tried
    // first and observed (this module's own test suite, real Linux) to
    // be a silent no-op against a just-bound `AF_UNIX` socket, which is
    // why this goes through the `*at()` pair instead. Neither call passes
    // `AT_SYMLINK_NOFOLLOW`: Linux's `fchmodat` does not implement that
    // flag at all (`ENOTSUP`), and it is not load-bearing here regardless
    // — the entry named `file_name` was JUST created by the `bind` call
    // immediately above, inside a directory this module already verified
    // is owner-only 0700, so nothing outside this same uid (out of scope,
    // module doc "Security") could have replaced it with a symlink in the
    // instant since.
    let rc = unsafe { libc::fchmodat(dir_fd, file_name.as_ptr(), 0o600, 0) };
    if rc != 0 {
        return Err(SocketError::Io {
            op: "fchmodat(socket)",
            source: io::Error::last_os_error(),
        });
    }
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(dir_fd, file_name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW)
    };
    if rc != 0 {
        return Err(SocketError::Io {
            op: "fstatat(socket)",
            source: io::Error::last_os_error(),
        });
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFSOCK
        || st.st_uid != crate::state_dir::current_uid()
        || st.st_mode & 0o777 != 0o600
    {
        return Err(SocketError::Io {
            op: "verify socket permissions",
            source: io::Error::other(
                "socket file is not an owner-only (0600) socket special file after fchmodat",
            ),
        });
    }

    let rc = unsafe { libc::listen(fd.as_raw_fd(), max_connections as libc::c_int) };
    if rc != 0 {
        return Err(SocketError::Io {
            op: "listen(AF_UNIX)",
            source: io::Error::last_os_error(),
        });
    }

    // SAFETY: `fd` was just bound and listened on as an `AF_UNIX`/
    // `SOCK_STREAM` socket; `UnixListener` takes ownership of exactly
    // that fd.
    Ok(unsafe { UnixListener::from_raw_fd(fd.into_raw_fd()) })
}

/// Set `FD_CLOEXEC` on `fd` — the portable (Linux AND macOS/BSD)
/// two-call equivalent of Linux's own combined `SOCK_CLOEXEC`/`O_CLOEXEC`
/// creation flags (not available uniformly across this crate's Unix
/// targets — see the call sites' own doc).
fn set_cloexec(fd: RawFd) -> io::Result<()> {
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Set `O_NONBLOCK` on `fd` via a read-modify-write `fcntl` pair — the
/// portable equivalent of Linux's own `pipe2(O_NONBLOCK)` (see the wake
/// pipe's own construction).
fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// ADR 0043 decision 3, "`AF_UNIX` is asserted (property 2)": a pure
/// internal sanity check, not a caller-facing error path — this can only
/// fail if `create_and_bind_listener` stops actually requesting
/// `AF_UNIX`, which is a bug in THIS module, never a legitimate runtime
/// condition. `getsockopt(SO_DOMAIN)` itself is only documented on Linux
/// (hence `#[cfg(target_os = "linux")]` at the one call site); a
/// `getsockopt` failure (an older kernel lacking `SO_DOMAIN`) is silently
/// skipped rather than treated as a hard error, since it proves nothing
/// either way.
#[cfg(target_os = "linux")]
fn assert_domain_is_unix(fd: RawFd) {
    let mut domain: libc::c_int = -1;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_DOMAIN,
            std::ptr::addr_of_mut!(domain).cast(),
            &mut len,
        )
    };
    if rc == 0 {
        assert_eq!(domain, libc::AF_UNIX, "socket() did not create an AF_UNIX socket");
    }
}

/// Marker error: a partial-progress write loop observed
/// [`ConnHandle::torn_down_requested`] flip mid-write and stopped rather
/// than continue submitting more of the payload — the write(s) that DID
/// land are real (never rolled back), but nothing further is attempted.
/// [`classify_terminal_error`] maps this to [`ClosedReason::Closed`].
#[derive(Debug)]
struct TeardownRequestedMarker;
impl std::fmt::Display for TeardownRequestedMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("teardown requested for this connection")
    }
}
impl std::error::Error for TeardownRequestedMarker {}

fn is_teardown_requested(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<TeardownRequestedMarker>())
}

/// The POSIX disconnect family for a stream socket's read/write errors —
/// the direct analogue of `pipe_win::is_disconnect_family`: an ordinary,
/// expected `Eof` for a live connection whose peer vanished or whose own
/// end was `shutdown(2)`'d, never treated as an anomaly.
fn is_disconnect_family(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
    )
}

/// Classify a terminal read/write error. A SUCCESSFUL zero-byte read is
/// handled separately, in [`reader_loop`] itself — it never reaches this
/// function (mirrors `pipe_win::classify_terminal_error`).
fn classify_terminal_error(e: io::Error) -> ClosedReason {
    if is_teardown_requested(&e) {
        return ClosedReason::Closed;
    }
    if is_disconnect_family(e.kind()) {
        return ClosedReason::Eof;
    }
    ClosedReason::Error(e.to_string())
}

/// Deliver one lifecycle event (`Accepted`/`Sent`/`Closed`/`AcceptError`)
/// RELIABLY: retries against a full `events` channel indefinitely, with
/// exactly one escape — [`ServerShared::dropping`] — once true, nothing
/// could ever call `events()` again. Identical contract to
/// `pipe_win::send_lifecycle_event`.
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
                thread::sleep(EVENTS_RETRY_INTERVAL);
            }
        }
    }
}

/// Attempt to deliver one `Bytes` event, retrying against a full `events`
/// channel for up to [`BYTES_ABANDON_AFTER`] — abandoning delivery
/// (returning `false`) once that bound elapses OR the moment
/// `torn_down_requested` is observed set (this connection is being torn
/// down by another path already; further retrying is pure busywork).
/// Identical contract to `pipe_win::deliver_bytes`.
fn deliver_bytes(
    shared: &Arc<ServerShared>,
    conn_id: ConnId,
    bytes: Vec<u8>,
    torn_down_requested: &AtomicBool,
) -> bool {
    let mut item = TransportEvent::Bytes(conn_id, bytes);
    let deadline = Instant::now() + BYTES_ABANDON_AFTER;
    loop {
        match shared.events_tx.try_send(item) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(v)) => {
                item = v;
                if torn_down_requested.load(Ordering::Acquire) || Instant::now() >= deadline {
                    return false;
                }
                thread::sleep(EVENTS_RETRY_INTERVAL);
            }
        }
    }
}

/// Request teardown for `conn_id`, at most once: every caller (an
/// explicit `close`, the reader's own EOF/error signal, the writer's own
/// error signal) races the SAME connection's `flag` via
/// `compare_exchange`; only the winner enqueues a [`ReaperMsg`].
/// Identical contract to `pipe_win::request_teardown`.
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

/// Notify the consumer that a just-accepted stream could not be fully
/// registered (a worker's `thread::Builder::spawn` failed) — `Accepted`
/// then an immediate `Closed(Error(..))`, both via the reliable path.
/// Identical contract to `pipe_win::report_registration_failure`.
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
/// the `conns.remove` below is the single, uncontested point of truth
/// for "who claims this connection". Issues ONE `shutdown(SHUT_RDWR)`
/// regardless of which of the three triggers requested teardown — see
/// the module doc's "Cancellation" section for why this is the direct
/// analogue of `pipe_win::teardown_if_present` cancelling both of its
/// `IoSlot`s unconditionally. `reason: None` is `Drop`'s shutdown pass —
/// no event is emitted (nothing could ever observe it).
fn teardown_if_present(shared: &Arc<ServerShared>, conn_id: ConnId, reason: Option<ClosedReason>) {
    let conn = shared.conns.lock().unwrap().remove(&conn_id);
    let Some(conn) = conn else { return };
    unsafe { libc::shutdown(conn.stream.as_raw_fd(), libc::SHUT_RDWR) };
    drop(conn.sender); // unblocks a writer idle-waiting on `recv` with nothing queued
    conn.reader_jh.join().ok();
    conn.writer_jh.join().ok();
    if let Some(reason) = reason {
        send_lifecycle_event(shared, TransportEvent::Closed(conn_id, reason));
    }
}

/// The reaper: the only thread that ever removes a registered connection
/// from `conns` or joins its reader/writer. Processes [`ReaperMsg`]s
/// strictly one at a time. Identical contract to `pipe_win::reaper_loop`.
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

/// The accept loop, one dedicated thread for the server's whole life:
/// blocks in `libc::poll` over `{listener, wake_read}` (module doc: "the
/// accept loop wakes via poll(2) over a self-pipe"); at capacity (ADR
/// 0043 decision 4) the newly accepted stream is closed immediately
/// rather than refused at the kernel level (Unix cannot refuse at
/// connect time — the kernel completes the handshake from the listen
/// backlog).
fn accept_loop(shared: Arc<ServerShared>, listener: UnixListener, wake_read: OwnedFd) {
    let listener_fd = listener.as_raw_fd();
    let wake_fd = wake_read.as_raw_fd();
    loop {
        if shared.accept_stopping.load(Ordering::Acquire) {
            return;
        }
        let mut fds = [
            libc::pollfd {
                fd: listener_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            terminalize_accept_loop(&shared, format!("poll: {err}"));
            return;
        }
        if fds[1].revents & libc::POLLIN != 0 {
            // Woken -- drain whatever is queued (defensive: only ever
            // one byte is written per `disconnect_listener` call, but a
            // repeat call is tolerated) and loop back to the top's
            // `accept_stopping` check.
            let mut discard = [0u8; 64];
            loop {
                let n = unsafe {
                    libc::read(wake_fd, discard.as_mut_ptr().cast(), discard.len())
                };
                if n <= 0 {
                    break;
                }
            }
            continue;
        }
        if fds[0].revents & (libc::POLLERR | libc::POLLNVAL | libc::POLLHUP) != 0 {
            // The listener itself is bad. `poll` would report this again
            // immediately, so `continue` here would spin the acceptor hot
            // forever: treat it as the permanent accept failure it is
            // (property 32) — one `AcceptError`, then stop accepting.
            terminalize_accept_loop(
                &shared,
                format!("poll: listener reported revents {:#x}", fds[0].revents),
            );
            return;
        }
        if fds[0].revents & libc::POLLIN == 0 {
            continue; // nothing to accept yet
        }
        match listener.accept() {
            Ok((stream, _addr)) => {
                if shared.conns.lock().unwrap().len() >= shared.max_connections as usize {
                    // ADR 0043 decision 4: accept-then-close at capacity.
                    drop(stream);
                    continue;
                }
                handle_new_connection(&shared, stream);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::WouldBlock
                ) =>
            {
                continue; // transient: ADR 0043 decision 4's retried family
            }
            Err(e) => {
                terminalize_accept_loop(&shared, format!("accept: {e}"));
                return;
            }
        }
    }
}

/// Stop the accept loop for good and report why — the ONE place every
/// persistent-resource-failure path routes through. Sets ONLY
/// `accept_stopping` (never `dropping` — see the module doc's "Two
/// distinct 'stop' signals" section for why conflating them would risk
/// silently losing the very `AcceptError` this function emits).
fn terminalize_accept_loop(shared: &Arc<ServerShared>, message: String) {
    shared.accept_stopping.store(true, Ordering::Release);
    send_lifecycle_event(shared, TransportEvent::AcceptError(message));
}

/// Hand off a just-accepted stream: spawn its reader/writer threads
/// (gated — see [`StartGate`]), register it, THEN reliably publish
/// `Accepted` and open the gate. Mirrors
/// `pipe_win::handle_new_connection`'s own ordering and its recoverable-
/// spawn-failure handling — with no instance to recycle on failure (the
/// `UnixStream` simply drops, closing its fd, once this function
/// returns).
fn handle_new_connection(shared: &Arc<ServerShared>, stream: UnixStream) {
    let stream = Arc::new(stream);
    let conn_id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<WriteCmd>();
    let outbound = Arc::new(OutboundBudget::new());
    let gate = StartGate::new();
    let torn_down_requested = Arc::new(AtomicBool::new(false));

    let reader_jh = {
        let shared2 = Arc::clone(shared);
        let stream2 = Arc::clone(&stream);
        let gate2 = Arc::clone(&gate);
        let torn = Arc::clone(&torn_down_requested);
        thread::Builder::new()
            .name(format!("sot-sock-r-{conn_id}"))
            .spawn(move || {
                if !gate2.wait_for_start() {
                    return;
                }
                reader_loop(stream2, conn_id, shared2, torn)
            })
    };
    let reader_jh = match reader_jh {
        Ok(jh) => jh,
        Err(e) => {
            report_registration_failure(shared, "reader thread spawn failed", e);
            return;
        }
    };

    let writer_jh = {
        let shared2 = Arc::clone(shared);
        let stream2 = Arc::clone(&stream);
        let outbound2 = Arc::clone(&outbound);
        let gate2 = Arc::clone(&gate);
        let torn = Arc::clone(&torn_down_requested);
        thread::Builder::new()
            .name(format!("sot-sock-w-{conn_id}"))
            .spawn(move || {
                if !gate2.wait_for_start() {
                    return;
                }
                writer_loop(stream2, conn_id, rx, shared2, outbound2, torn)
            })
    };
    let writer_jh = match writer_jh {
        Ok(jh) => jh,
        Err(e) => {
            // The reader is spawned but still gated -- abort makes its
            // `wait_for_start` return `false` immediately, so joining it
            // here (NOT through the reaper: it was never registered) is
            // bounded.
            gate.abort();
            reader_jh.join().ok();
            report_registration_failure(shared, "writer thread spawn failed", e);
            return;
        }
    };

    // Codex review finding 2 (P2): registration must not be able to
    // escape phase one of `disconnect_listener`. That method's own drain
    // and this insert take the SAME `conns` lock, so whichever of the two
    // threads acquires it first establishes a real happens-before order
    // for `dropping` that a bare atomic load, on its own, cannot promise:
    // if `disconnect_listener` locked first, its own `dropping` write is
    // now certainly visible here (the lock's release-then-acquire is what
    // provides that, not `dropping`'s own ordering in isolation) — refuse
    // to register at all. If this insert locks FIRST instead,
    // `disconnect_listener` — however soon after it next acquires the
    // SAME lock — will find this connection already in `conns` and hand
    // it a normal, correct teardown through `detached_workers`. There is
    // no window where a connection is registered AFTER
    // `disconnect_listener`'s own drain has already run and will never
    // see it again — which is exactly the leak this check closes (an
    // orphaned reader/writer pair neither joined by the reaper, since it
    // was never registered, NOR by `join_workers`, since it never reached
    // `detached_workers` either).
    let mut conns = shared.conns.lock().unwrap();
    if shared.dropping.load(Ordering::Acquire) {
        drop(conns);
        // Never touched the stream (still gated) -- `abort` makes both
        // threads' own `wait_for_start` return `false` immediately, so
        // joining them here (NOT through the reaper: neither was ever
        // registered) is bounded and legal, the same as the writer-spawn-
        // failure path above. `shutdown` first anyway, defensively, in
        // case either thread is somehow already past the gate (it is
        // not, by construction) -- costs nothing, removes any doubt.
        unsafe { libc::shutdown(stream.as_raw_fd(), libc::SHUT_RDWR) };
        gate.abort();
        reader_jh.join().ok();
        writer_jh.join().ok();
        return; // no event: this connection was never told to exist.
    }
    conns.insert(
        conn_id,
        ConnHandle {
            stream,
            outbound,
            sender: tx,
            reader_jh,
            writer_jh,
            torn_down_requested,
        },
    );
    drop(conns); // never hold this lock while sending on the events channel
    // RELIABLE, not best-effort: retries until the consumer actually has
    // room, so the gate below can never open onto a connection the
    // consumer was never told exists.
    send_lifecycle_event(shared, TransportEvent::Accepted(conn_id));
    gate.open(); // ONLY now may the reader/writer threads touch the stream.
}

/// One connection's read side: at most one outstanding `read` at a time.
/// On any terminal condition this thread does NOT touch `conns` or join
/// anything itself — it only [`request_teardown`]s and returns. Mirrors
/// `pipe_win::reader_loop`.
fn reader_loop(
    stream: Arc<UnixStream>,
    conn_id: ConnId,
    shared: Arc<ServerShared>,
    torn_down_requested: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; READ_BUF_LEN];
    let reason = loop {
        match (&*stream).read(&mut buf) {
            Ok(0) => break ClosedReason::Eof, // ordered EOF (property 13), never reaches classify_terminal_error
            Ok(n) => {
                if !deliver_bytes(&shared, conn_id, buf[..n].to_vec(), &torn_down_requested) {
                    break ClosedReason::Error(format!(
                        "events channel saturated for longer than {BYTES_ABANDON_AFTER:?}; \
                         Bytes delivery abandoned"
                    ));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => break classify_terminal_error(e),
        }
    };
    request_teardown(&shared, conn_id, &torn_down_requested, reason);
}

/// `write(2)` in a partial-progress loop, checking `torn_down_requested`
/// on EVERY iteration (ADR 0043 decision 5, "checked on every iteration
/// of a partial-progress loop, not only at entry") — belt and braces
/// alongside `shutdown(2)`'s own prompt effect on the underlying fd.
fn write_all_checking_teardown(
    stream: &UnixStream,
    mut bytes: &[u8],
    torn_down_requested: &AtomicBool,
) -> io::Result<()> {
    while !bytes.is_empty() {
        if torn_down_requested.load(Ordering::Acquire) {
            return Err(io::Error::other(TeardownRequestedMarker));
        }
        match (&*stream).write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "write returned 0 with bytes still to send",
                ))
            }
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// One connection's write side: drains queued sends in order, one
/// outstanding `write` sequence at a time, reliably emitting `Sent` for
/// marker-tagged sends once the OS reports the write physically
/// complete, and releasing its outbound-budget reservation once that
/// write RETURNS either way (property 11: the in-flight item stays
/// counted the whole time). Exits when its channel disconnects (the
/// reaper dropped the sender) or its current write is cancelled/fails —
/// a write failure [`request_teardown`]s directly. Never touches
/// `shared.conns` — teardown is always the reaper's. Mirrors
/// `pipe_win::writer_loop`.
fn writer_loop(
    stream: Arc<UnixStream>,
    conn_id: ConnId,
    rx: Receiver<WriteCmd>,
    shared: Arc<ServerShared>,
    outbound: Arc<OutboundBudget>,
    torn_down_requested: Arc<AtomicBool>,
) {
    while let Ok(cmd) = rx.recv() {
        let len = cmd.bytes.len();
        let result = write_all_checking_teardown(&stream, &cmd.bytes, &torn_down_requested);
        outbound.release(len);
        match result {
            Ok(()) => {
                if let Some(marker) = cmd.marker {
                    send_lifecycle_event(&shared, TransportEvent::Sent(conn_id, marker));
                }
            }
            Err(e) => {
                request_teardown(&shared, conn_id, &torn_down_requested, classify_terminal_error(e));
                break;
            }
        }
    }
}
