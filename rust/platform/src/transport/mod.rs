//! The local connection: a bounded, cancellable client of a socket or pipe on this box, and the one rule that reaches
//! it only when this OS account serves it (`CLAUDE.md`, guarantee 13). The clients are `socket_unix::SocketClient`
//! (Unix) and `pipe_win::PipeClient` (Windows); [`connect_own`] is the way in by name; [`accept_policy`] is what an
//! accept loop does with an error. Every server is its owner's (the row socket, the daemon's listener).
//!
//! Deliberately ungated: [`Client`] and [`TransportError`] are the contract, not an implementation.

use crate::identity::challenge::ChallengeableConnection;
use std::path::PathBuf;
use std::time::Duration;

pub mod accept_policy;
pub mod connect_own;
#[cfg(windows)]
pub mod pipe_win;
#[cfg(unix)]
pub mod socket_unix;

/// The total connect retry budget, shared by both platforms' clients: an endpoint that is BUSY (a pipe instance already
/// claimed, `ERROR_PIPE_BUSY`, or a Unix listener whose backlog is momentarily full or whose accept was merely
/// interrupted, `EAGAIN`/`EINTR`) is retried within it, because a listener holds its instances continuously and
/// recycles them once bound, so "unavailable" past that point can only mean busy. An ABSENT endpoint (no pipe instance
/// exists yet, `ERROR_FILE_NOT_FOUND`, or no listener at all, `ECONNREFUSED`/`ENOENT`) returns on the very first
/// attempt: this bound is never charged waiting for a server that has not started yet. Whoever knows a server is
/// coming (the daemon's post-spawn poll, a viewer's own episode loop, a test harness) polls for that at its own
/// interval instead.
pub const CONNECT_BOUND: Duration = Duration::from_secs(2);

/// Why a connection call failed. The variants name their producers: the first five are platform's own clients';
/// `PathTooLong` and `RuntimeDir` are an endpoint that resolves a path's (the capsule's); the last four are for an
/// endpoint reached through a daemon (topology's lane client), whose words are that endpoint's. Platform builds none
/// of the last six and knows no op.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The OS said no at `op`, its error kept so a caller can classify it by kind or code. Platform's clients.
    #[error("{op}: {source}")]
    Io {
        op: &'static str,
        source: std::io::Error,
    },
    /// A write of zero bytes was asked for. Platform's clients.
    #[error("empty payload: a write never carries a zero-length send")]
    EmptyPayload,
    /// A payload larger than one call can represent. Platform's clients.
    #[error("payload of {0} bytes exceeds what a single write/read call can represent")]
    PayloadTooLarge(usize),
    /// The call was ended by [`Client::cancel`] (or by the client's own latch after a terminal failure). Platform's
    /// clients.
    #[error("operation cancelled")]
    Cancelled,
    /// A second same-direction client call (e.g. two concurrent `read`s) was rejected before it ever touched the OS or
    /// a shared I/O slot: misuse, not a race this crate resolves for the caller. Platform's clients.
    #[error("another operation is already pending on this client's same direction")]
    ConcurrentSubmit,
    /// The endpoint's path exceeds the platform's `sun_path` limit; the message names the path without a number this
    /// type cannot portably compute. An endpoint that resolves a path (the capsule's); never produced on Windows.
    #[error("endpoint path {0:?} exceeds this platform's path-length limit")]
    PathTooLong(PathBuf),
    /// The runtime folder could not be resolved or refused this account. An endpoint that resolves a path (the
    /// capsule's); Windows never produces it.
    #[error("resolving the runtime dir: {0}")]
    RuntimeDir(std::io::Error),
    /// The daemon behind a dial could not tell whose process is on the other end: the check's own message. An endpoint
    /// reached through a daemon (topology's lane client); never treated as authenticated or foreign.
    #[error("peer identity undetermined: {detail}")]
    Undetermined { detail: String },
    /// The daemon answered the dial with a refusal; `code` is its word, verbatim. Terminal: never retried as if the
    /// daemon might change its mind. An endpoint reached through a daemon (topology's lane client).
    #[error("the daemon refused the lane ({code}): {detail}")]
    Refused { code: String, detail: String },
    /// The dial or its handshake itself failed (timed out, refused, or no answer within the bound). Distinct from
    /// [`Self::Io`], which [`Self::is_endpoint_absent`] can read as a genuine absence: transport uncertainty is never
    /// charged to that absence window. An endpoint reached through a daemon (topology's lane client).
    #[error("the daemon's lane bridge is unreachable: {0}")]
    Unreachable(std::io::Error),
    /// The host's link is down, so no ssh was started. An endpoint reached through a daemon (topology's lane client).
    #[error("the host's link is down; no ssh was started")]
    LinkDown,
}

impl TransportError {
    /// True iff this error means "the endpoint is not there right now": a pipe name that does not exist (Windows:
    /// `NotFound`) or a Unix domain socket path with no listener (`ConnectionRefused`: the kernel refuses a connect
    /// against a bound but unlistened, or unlinked, path; a path never created is `NotFound` there too). One predicate
    /// for both platforms; a named pipe connect never yields `ConnectionRefused`, so it widens nothing on Windows.
    pub fn is_endpoint_absent(&self) -> bool {
        matches!(
            self,
            TransportError::Io { source, .. }
                if matches!(source.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)
        )
    }
}

/// The blocking read/write/cancel shape every concrete client exposes, so a caller can hold either kind behind one
/// type. `Sync`: a watchdog thread calls `cancel()` while the caller's own thread blocks in `read`/`write_all`. Every
/// `Client` is a [`ChallengeableConnection`] by the blanket impl below.
pub trait Client: Send + Sync {
    /// Blocking write of the whole buffer, cancellable from another thread via [`cancel`](Self::cancel).
    fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError>;
    /// Blocking read into `buf`. `Ok(0)` is ordered EOF, never a spurious zero-byte completion.
    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError>;
    /// Abort whatever is in flight, from any thread. One-shot, like [`ChallengeableConnection::cancel`].
    fn cancel(&self);
}

/// The one `TransportError -> io::Error` mapping a challengeable client uses: `Io` unwraps to its underlying
/// `std::io::Error` (preserving `ErrorKind`, e.g. a disconnect code); everything else, `Cancelled` included, wraps
/// opaquely. `Cancelled` wrapping opaquely rather than as a distinguished `io::ErrorKind` is what
/// `challenge::exchange_identity`'s own `map_err(|_| StatusFailure::Undetermined)` already treats as "failed, don't
/// ask which way", so a cancelled challenge behaves like any other failure mid-exchange.
pub fn transport_error_to_io(e: TransportError) -> std::io::Error {
    match e {
        // Both variants that carry an `io::Error` hand it back unwrapped, so its `ErrorKind` survives for callers that
        // classify on it (the attach client's access-denied check): `RuntimeDir` is the Linux shape of "the endpoint's
        // directory refused us" (an invalid or foreign-owned `SOT_RUNTIME_DIR` -> `PermissionDenied`). Windows never
        // produces it.
        TransportError::Io { source, .. } | TransportError::RuntimeDir(source) => source,
        other => std::io::Error::other(other),
    }
}

impl<T: Client> ChallengeableConnection for T {
    fn write_all(&self, bytes: &[u8]) -> std::io::Result<()> {
        Client::write_all(self, bytes).map_err(transport_error_to_io)
    }

    fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        Client::read(self, buf).map_err(transport_error_to_io)
    }

    fn cancel(&self) {
        Client::cancel(self)
    }
}
