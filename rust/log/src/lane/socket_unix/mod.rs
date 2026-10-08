//! The L1-unix LU1b Unix-domain-socket transport (ADR 0043): a server for
//! `<runtime_dir>/voyage-<id>.sock` and `<runtime_dir>/supervisor-<h>.sock`.
//! It moves bytes and reports completions; it does not know about
//! mgmt/attach lanes, `hello`, opcodes, or checkpoints — `lane/wire/` owns
//! every frame shape. Transport only: no dependency on the capsule or
//! `sot-capsule` bin, and none may be added here.
//!
//! This module is [`pipe_win`](crate::lane::pipe_win)'s mechanical twin BY
//! PROPERTY, not by mechanism (ADR 0043 "Port by property, not by
//! mechanism") — same event vocabulary, same thread roles
//! (`sot-sock-accept`/`sot-sock-reaper`/`sot-sock-r-<id>`/
//! `sot-sock-w-<id>`, vs. `sot-pipe-accept`/`sot-pipe-reaper`/
//! `sot-pipe-r-<id>`/`sot-pipe-w-<id>`), same teardown order and the same
//! shared bounds (`crate::lane::transport`). What Unix DELETES relative to that
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
//! `lane/pipe_win/` needs an [`InstanceRegistry`](crate::lane::pipe_win) because
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
//! Windows needs one [`IoSlot`](crate::lane::pipe_win) per direction per
//! connection because `CancelIoEx` targets a SPECIFIC pending overlapped
//! op. POSIX has no equivalent of targeting one blocked call from another
//! thread — the primitive that generalizes is `shutdown(2)` on the
//! connection's fd: it unblocks a blocked `read` (returns `0`, ordinary
//! EOF) and a blocked `write` (returns a partial count or `EPIPE`) BOTH AT
//! ONCE, from any thread, without needing to know which direction (if
//! either) is currently mid-call. So the reaper's claim issues ONE
//! `shutdown(SHUT_RDWR)` regardless of which of the three triggers (an
//! explicit [`SocketServer::close`], the reader's own EOF/error signal, or
//! the writer's own error signal) requested it — the direct analogue of
//! `pipe_win`'s own claim unconditionally cancelling BOTH
//! its read and write `IoSlot`s no matter which one signalled first.
//!
//! # The accept loop wakes via `poll(2)` over a self-pipe, never a
//! connect-to-self
//!
//! disconnect_listener wakes the acceptor through its nonblocking self-pipe, never by dialing the listener. Linux creates
//! both wake ends with pipe2(O_CLOEXEC | O_NONBLOCK); macOS immediately owns and checks both ends with fcntl before
//! publication. The macOS creation-to-flagging inheritance window remains.
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
//! it matters most; `lane/pipe_win/` keeps the identical split between its
//! own `ServerShared::dropping` and `AcceptState::accept_stopping` for the
//! same reason.
//!
//! # Reliable lifecycle delivery, byte-bounded both directions
//!
//! Lifecycle events remain reliable until consumer-gone or dropping. Accepted, Sent and acceptor errors use their retry
//! sender; the reaper retains blocked Closed and recycle-error records and tries them nonblockingly while polling every
//! pending pair. Bytes abandonment still forces Closed. Outbound bytes remain reserved until the physical write returns.
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
//! Every type below is `pub`, not `pub(crate)` — `tests/socket_unix/` is
//! a separate integration-test crate and can only ever reach a library's
//! `pub` items, the same reason `lane/pipe_win/`'s own types are `pub`.

#![cfg(unix)]

use crate::lane::client::Client;
// `Endpoint`'s only implementor here (`SocketEndpoint`) exists on the two
// Unix targets that have a peer-identity mechanism this crate trusts -- a
// plain, unconditional `use` would warn "unused import" on any OTHER Unix
// build, the same device this crate already uses for
// `deadline.rs`/`exchange_identity`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::lane::client::Endpoint;
// The platform's own challenge module for THIS transport, chosen ONCE by
// an alias -- the same device `client::PlatformEndpoint` and
// `transport::PlatformLaneServer` already use, and the reason every
// `Endpoint`/constructor body below is written once rather than twice.
// Linux reads `SO_PEERCRED` plus a pidfd pin; macOS reads `getpeereid`
// for the account and one `LOCAL_PEERTOKEN` audit token carrying the peer's
// pid AND the kernel's own reuse generation together (see either module's
// own doc). No other Unix has one, which is what the gate above says.
#[cfg(target_os = "macos")]
use crate::identity::challenge_macos as challenge_os;
#[cfg(target_os = "linux")]
use crate::identity::challenge_unix as challenge_os;
use crate::lane::attach_proto::ConnId;
use crate::lane::test_progress::Role;
use crate::lane::pending::{self, report_server_teardown_failed, Claimed, ReaperMsg, REAPER_INBOX_SLACK};
use crate::lane::transport::{
    join_within, validate_voyage_id, ClosedReason, LaneEvent, LaneServer, OutboundBudget,
    SendMarker, StartGate, TransportError, BYTES_ABANDON_AFTER, CONNECT_BOUND, EVENTS_CHANNEL_CAP,
    EVENTS_RETRY_INTERVAL, READ_BUF_LEN, TEARDOWN_AGGREGATE_DEADLINE,
};
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

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

// `ClosedReason`, `LaneEvent`, and `TransportError` used to be defined
// here (`SocketError`/this module's own event enums) — L1-unix LU3a (ADR
// 0043 decisions 17/19) hoisted all three into `crate::lane::transport`, since
// `lane/pipe_win/`'s own copies were byte-for-byte identical in shape and
// both platforms' servers now produce the SAME event type. Imported
// below; nothing in this module defines them anymore.

// ---------------------------------------------------------------------
// Paths (ADR 0043 decision 1).
// ---------------------------------------------------------------------

/// `<runtime_dir>/voyage-<voyage_id>.sock`, after validating `voyage_id`
/// is the canonical lowercase-hyphenated form of an RFC 4122 UUID — the
/// shared [`validate_voyage_id`] check, which delegates to
/// `pointer::canonical_voyage_id` (one implementation, not two that can
/// drift).
pub fn voyage_socket_path(voyage_id: &str) -> Result<PathBuf, TransportError> {
    validate_voyage_id(voyage_id)?;
    socket_path(&format!("voyage-{voyage_id}"))
}

/// `<runtime_dir>/supervisor-<h>.sock` — the supervisor lane's own
/// socket, otherwise identical to [`voyage_socket_path`]. `h` is the
/// caller's own stable hash of the canonicalized state-dir path; this
/// function neither derives nor validates it as a voyage id, unlike
/// [`voyage_socket_path`] — matching `pipe_win::supervisor_pipe_name_wide`.
pub fn supervisor_socket_path(h: &str) -> Result<PathBuf, TransportError> {
    socket_path(&format!("supervisor-{h}"))
}

fn socket_path(file_name: &str) -> Result<PathBuf, TransportError> {
    let dir = crate::host::state_dir::runtime_dir().map_err(TransportError::RuntimeDir)?;
    let path = dir.join(format!("{file_name}.sock"));
    if path.as_os_str().as_bytes().len() > max_sun_path_bytes() {
        return Err(TransportError::PathTooLong(path));
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

/// A registered connection is owned by the live map until the reaper claims it, then by a charged pending record
/// through worker joins and close-event retirement. The owned Unix stream remains alive through both joins.
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

/// TEST-SUPPORT ONLY counters proving the events channel actually
/// saturated and the `Bytes` abandon bound actually fired — Codex review
/// round 2's own critique of the FIRST fix pass's client-side stall
/// heuristic (a 500 ms client `WouldBlock` does not PROVE the events
/// channel is full: the reader thread may simply be unscheduled for that
/// long, then wake, drain the kernel's own backlog in one go, and never
/// once observe `TrySendError::Full` — kernel socket buffer sizes also
/// differ across Unix targets). Gated `#[cfg(any(test, feature =
/// "test-support"))]` — the SAME combined gate `lane/pipe_win/`'s own
/// equivalent test-only methods use (see `Cargo.toml`'s doc on that
/// feature) — so a normal build carries a ZERO-SIZE unit struct whose
/// `note_*` methods are empty `#[inline]` fns: no atomic, no counter, no
/// cost outside a test build. `note_*` methods exist in BOTH builds
/// (called unconditionally from production code paths in
/// [`deliver_bytes`]/[`send_lifecycle_event`]); the getters exist ONLY
/// under the test-support cfg, since nothing outside a test ever needs
/// to read them.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
struct Probes {
    events_full_bytes: AtomicUsize,
    events_full_lifecycle: AtomicUsize,
    bytes_abandoned: AtomicUsize,
}
#[cfg(not(any(test, feature = "test-support")))]
#[derive(Default)]
struct Probes;

impl Probes {
    #[cfg(any(test, feature = "test-support"))]
    fn note_events_full_bytes(&self) {
        self.events_full_bytes.fetch_add(1, Ordering::Relaxed);
    }
    #[cfg(not(any(test, feature = "test-support")))]
    #[inline]
    fn note_events_full_bytes(&self) {}

    #[cfg(any(test, feature = "test-support"))]
    fn note_events_full_lifecycle(&self) {
        self.events_full_lifecycle.fetch_add(1, Ordering::Relaxed);
    }
    #[cfg(not(any(test, feature = "test-support")))]
    #[inline]
    fn note_events_full_lifecycle(&self) {}

    #[cfg(any(test, feature = "test-support"))]
    fn note_bytes_abandoned(&self) {
        self.bytes_abandoned.fetch_add(1, Ordering::Relaxed);
    }
    #[cfg(not(any(test, feature = "test-support")))]
    #[inline]
    fn note_bytes_abandoned(&self) {}

    #[cfg(any(test, feature = "test-support"))]
    fn events_full_bytes(&self) -> usize {
        self.events_full_bytes.load(Ordering::Relaxed)
    }
    #[cfg(any(test, feature = "test-support"))]
    fn events_full_lifecycle(&self) -> usize {
        self.events_full_lifecycle.load(Ordering::Relaxed)
    }
    #[cfg(any(test, feature = "test-support"))]
    fn bytes_abandoned(&self) -> usize {
        self.bytes_abandoned.load(Ordering::Relaxed)
    }
}

struct ServerShared {
    conns: Mutex<HashMap<ConnId, ConnHandle>>,
    /// The next connection id: assigned sequentially; never reused.
    next_id: AtomicU64,
    reaper_tx: SyncSender<ReaperMsg>,
    events_tx: SyncSender<LaneEvent>,
    /// Switch-latency Phase 1 (c): the bridging `Transport` impl's own
    /// wake callback (`PlatformTransport::bind`, via [`SocketServer::
    /// set_wake`]) — invoked, if set, every time [`send_lifecycle_event`]/
    /// [`deliver_bytes`] successfully push a fresh event, AFTER the push
    /// (so a caller woken by it is guaranteed the event is already
    /// sitting in `events_tx` for `events()`'s own `try_recv` to find).
    /// `OnceLock`, not a `Mutex`: set at most once, by the ONE caller
    /// that ever calls `set_wake` (immediately after `bind`/
    /// `bind_supervisor` returns) — every read after that is wait-free.
    /// Never set at all for the supervisor lane (`bind_supervisor`'s own
    /// caller, `supervisor/`, wakes its main loop by blocking on
    /// `events()` directly — see that module's own `MAIN_LOOP_POLL`
    /// comment — so it has no need of this). NOT to be confused with
    /// `wake_write`/`wake_read` below — this server's OWN internal
    /// accept-loop self-pipe, an unrelated mechanism (module doc: "the
    /// accept loop wakes via poll(2) over a self-pipe").
    activity_wake: OnceLock<Arc<dyn Fn() + Send + Sync>>,
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
    /// TEST-SUPPORT ONLY (see [`Probes`]'s own doc) — zero-size, zero-cost
    /// outside a test build.
    probes: Probes,
    progress: crate::lane::test_progress::Progress,
    /// Claimed connections still charged against `max_connections`: workers unjoined or their close unretired.
    /// Raised under the `conns` lock at the claim, so live plus pending never exceeds the bound.
    pending: AtomicUsize,
    /// Latched by a completed worker panic or an unfinished worker at its deadline, and never cleared.
    teardown_failed: AtomicBool,
    /// The phase-one `Sweep` nudge and the one `Shutdown` have each been sent (or refused) once.
    sweep_nudged: AtomicBool,
    shutdown_sent: AtomicBool,
    /// Scoped regression controls (worker-exit holds and panics, barriers, a short teardown deadline): zero-sized
    /// outside a test build.
    controls: crate::lane::test_progress::Controls,
}

mod accept;
mod client;
mod conn;
mod connect;
mod listener;
mod server;

pub(crate) use client::connect_unix_socket_unchallenged;
#[cfg(unix)]
pub use client::connect_voyage_socket;
pub(crate) use client::connect_voyage_socket_unchallenged;
pub use client::SocketClient;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use client::SocketEndpoint;
pub use server::SocketServer;
