//! L1-unix LU3a (ADR 0043 decision 19): the three seam traits landed
//! BEFORE any consumer uses them — every existing call site still names
//! `PipeClient`/`SocketClient`/`PipeServer`/`SocketServer`/
//! `challenge_win::ChallengedProcess`/`challenge_unix::ChallengedProcess`
//! directly; this module only adds the trait vocabulary and the
//! delegating `impl`s that prove each concrete type already satisfies
//! it. Generic consumers (`fe_client_io`, `supervisor`) are LU3b/LU3c's
//! job, not this lane's.
//!
//! Ungated, like `challenge.rs`/`transport.rs`: this is the CONTRACT, not
//! an implementation. `Client`/`Endpoint`'s two implementors
//! (`PipeClient`/`PipeEndpoint` on Windows, `SocketClient`/
//! `SocketEndpoint` on Linux) live in `pipe_win.rs`/`socket_unix.rs`
//! themselves, next to the concrete types they delegate to — the same
//! placement `impl crate::challenge::ChallengeableConnection for
//! PipeClient` used to have, before the blanket impl below replaced it.

use crate::challenge::{ChallengeOutcome, ChallengeableConnection, PeerAuthOutcome};
use crate::exchange::IdentityExchange;
use crate::transport::TransportError;
use std::time::{Duration, Instant};

/// The blocking read/write/cancel shape every concrete pipe/socket
/// client already exposed as an inherent API — named here so a caller
/// (and, eventually, a generic one, LU3b) can hold either kind behind one
/// type. `Sync`: mirrors [`ChallengeableConnection`]'s own bound — a
/// watchdog thread calls `cancel()` while the caller's own thread blocks
/// in `read`/`write_all`.
pub trait Client: Send + Sync {
    /// Blocking write of the whole buffer, cancellable from another
    /// thread via [`cancel`](Self::cancel).
    fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError>;
    /// Blocking read into `buf`. `Ok(0)` is ordered EOF — never a
    /// spurious zero-byte completion.
    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError>;
    /// Abort whatever is in flight, from any thread. One-shot, like
    /// [`ChallengeableConnection::cancel`].
    fn cancel(&self);
}

/// The ONE `TransportError -> io::Error` mapping every concrete client's
/// former hand-written `ChallengeableConnection` façade duplicated
/// (`pipe_win::pipe_error_to_io`, `socket_unix::socket_error_to_io` —
/// proven identical in effect before this blanket impl replaced both):
/// `Io` unwraps to its underlying `std::io::Error` (preserving
/// `ErrorKind` — e.g. a disconnect code); everything else — `Cancelled`
/// included — wraps opaquely. `Cancelled` wrapping opaquely rather than
/// as a distinguished `io::ErrorKind` is exactly what
/// `challenge::exchange_identity`'s own blanket `map_err(|_|
/// StatusFailure::Undetermined)` already treats as "failed, don't ask
/// which way" — so a cancelled challenge behaves identically to any
/// other read/write failure mid-exchange, matching both platforms'
/// pre-existing behavior.
pub(crate) fn transport_error_to_io(e: TransportError) -> std::io::Error {
    match e {
        // Both variants that CARRY an `io::Error` hand it back unwrapped, so
        // its `ErrorKind` survives for callers that classify on it (the
        // attach client's access-denied check): `RuntimeDir` is the Linux
        // shape of "the endpoint's directory refused us" (an invalid or
        // foreign-owned `SOT_RUNTIME_DIR` -> `PermissionDenied`); wrapping it
        // as `Other` sent the FE down the 120 s unresponsive path instead
        // (LU3b review round). Windows never produces `RuntimeDir`.
        TransportError::Io { source, .. } | TransportError::RuntimeDir(source) => source,
        other => std::io::Error::other(other),
    }
}

/// Every [`Client`] is challengeable, via the ONE mapping above — this
/// blanket impl is what makes `PipeClient`/`SocketClient` satisfy
/// [`ChallengeableConnection`] now; neither type writes its own façade
/// anymore.
impl<C: Client> ChallengeableConnection for C {
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

/// The methods both platforms' `ChallengedProcess` share — everything the
/// full five-step [`crate::challenge::ChallengeOutcome::Proven`] proof
/// earns a caller. Deliberately NOT the exit-status accessor (ADR 0043
/// decisions 8/19): Windows reports it as a `u32`
/// (`GetExitCodeProcess`), Linux as an `Option<i32>`
/// (`PIDFD_GET_INFO`, "exited, status unknown" is a real, distinct
/// outcome there) — the platforms disagree on the TYPE, not merely the
/// mechanism, so it stays an inherent method on each concrete
/// `ChallengedProcess` rather than forcing one shape on both. The one
/// consumer that reads it is the daemon's `wait_and_classify` (LU4),
/// which is where the "exited, status unknown" tolerance belongs — not
/// here.
pub trait PeerIdentity: Send {
    /// The pid this process was proven to be, at proof time.
    fn pid(&self) -> u32;
    /// The creation/start time this process was proven against, in
    /// whatever units the platform's own wire `status_ok.created` field
    /// carries (compared for equality only, never interpreted as a
    /// calendar time).
    fn created(&self) -> u64;
}

/// The rest of what a step-6 supervisor (and, now, `fe_client_io`'s health
/// path) needs beyond bare identity — split from [`PeerIdentity`] so a
/// consumer that only reads pid/created (the bridge's `BridgedPeer`, B4a)
/// is not forced to implement re-verification or termination it has no
/// authority to perform.
pub trait PeerProcess: PeerIdentity {
    /// Re-read this process's own identity and compare it against what it
    /// was proven with — the ADR's "pre-terminate re-verification".
    fn reverify(&self) -> std::io::Result<bool>;
    /// The death signal a supervisor waits on rather than sampling
    /// process absence. Bounded, never infinite.
    fn wait(&self, timeout: Duration) -> std::io::Result<bool>;
    /// The KILL half of the probe's own KILL+WAIT row, and the
    /// invalid-mgmt fallback's hard stop.
    fn terminate(&self) -> std::io::Result<()>;
}

/// What a step-6 supervisor needs from one platform's own connect/
/// challenge machinery, seamed so it can eventually be generic over
/// `PipeEndpoint`/`SocketEndpoint` (LU3b/LU3c) — today, both implement
/// this purely by delegation to free functions each module already
/// exposes, and no consumer is generic over it yet.
pub trait Endpoint {
    type Client: Client;
    type Process: PeerIdentity;

    /// Connect to the voyage lane's own endpoint, with NO authentication
    /// — every real caller runs [`Self::authenticate_server`] or
    /// [`Self::challenge`] on top; see either concrete module's own doc
    /// for why the raw connect and the identity proof stay two separately
    /// observed steps. `lane` is the row that owns the voyage, in this
    /// endpoint's own namespace — the platform endpoints ignore it (their
    /// voyage socket is named by id); the daemon-lane endpoint reads it.
    fn connect_voyage_unchallenged(
        &self,
        lane: &str,
        voyage_id: &str,
    ) -> Result<Self::Client, TransportError>;
    /// Connect to the supervisor lane's own endpoint, with NO
    /// authentication — the supervisor lane's security is MUTUAL, so
    /// unlike the voyage lane this has no `_unchallenged`-free sibling:
    /// the caller composes the full [`Self::challenge`] itself. `lane` is
    /// the supervisor lane's name in this endpoint's own namespace — the
    /// state-dir hash for the platform endpoints.
    fn connect_supervisor_unchallenged(&self, lane: &str) -> Result<Self::Client, TransportError>;
    /// The full five-step same-connection challenge (ADR 0041 Lifecycle
    /// "The challenge").
    fn challenge(
        &self,
        conn: &Self::Client,
        exchange: &mut dyn IdentityExchange,
        reply_deadline: Instant,
    ) -> ChallengeOutcome<Self::Process>;
    /// Steps 1-3 ONLY: identify the peer process and authenticate its
    /// identity — no wire I/O, deliberately weaker than [`Self::challenge`]
    /// (see either concrete module's own `authenticate_server` doc).
    fn authenticate_server(&self, conn: &Self::Client) -> PeerAuthOutcome;
}

/// L1-unix LU3b: the endpoint a process speaks on the platform it runs
/// on — the frontend (`fe_client_io.rs`) and `supervisor_client` are
/// generic over [`Endpoint`] and instantiated with this, the ONLY place
/// the platform is chosen for a client. Windows speaks `PipeEndpoint`,
/// Linux speaks `SocketEndpoint`; no other platform has one yet (a
/// generic build for one still needs a concrete `Endpoint` to
/// monomorphize against, which is exactly what does not exist off these
/// two today — see `fe_client_io`'s own top-of-module `cfg`).
#[cfg(windows)]
pub type PlatformEndpoint = crate::pipe_win::PipeEndpoint;
#[cfg(target_os = "linux")]
pub type PlatformEndpoint = crate::socket_unix::SocketEndpoint;
