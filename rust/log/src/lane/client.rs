//! The client seam: the traits every concrete client implements, and the delegating `impl`s that prove each does.
//!
//! Ungated, like `transport.rs`: this is the CONTRACT, not an implementation. `Client`/`Endpoint`'s two implementors
//! (`PipeClient`/`PipeEndpoint` on Windows, `SocketClient`/`SocketEndpoint` on Linux and macOS) live in
//! `lane/pipe_win/`/`lane/socket_unix/`, next to the concrete types they delegate to; `challengeable_client!` below
//! writes the `ChallengeableConnection` impl once.

use sot_platform::identity::challenge::{ChallengeOutcome, PeerAuthOutcome};
use sot_platform::identity::exchange::IdentityExchange;
use crate::lane::transport::TransportError;
use sot_platform::identity::peer::PeerIdentity;
use std::time::Instant;

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
pub fn transport_error_to_io(e: TransportError) -> std::io::Error {
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

/// Makes a [`Client`] challengeable (platform's `ChallengeableConnection`), via the ONE mapping above. A trait platform
/// owns cannot be implemented for every `Client` at once, so each concrete client names itself here: `SocketClient`,
/// `PipeClient` and the topology's three.
#[macro_export]
macro_rules! challengeable_client {
    ($client:ty) => {
        impl sot_platform::identity::challenge::ChallengeableConnection for $client {
            fn write_all(&self, bytes: &[u8]) -> std::io::Result<()> {
                <$client as $crate::lane::client::Client>::write_all(self, bytes)
                    .map_err($crate::lane::client::transport_error_to_io)
            }

            fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
                <$client as $crate::lane::client::Client>::read(self, buf)
                    .map_err($crate::lane::client::transport_error_to_io)
            }

            fn cancel(&self) {
                <$client as $crate::lane::client::Client>::cancel(self)
            }
        }
    };
}

/// What a step-6 supervisor needs from one platform's own connect/
/// challenge machinery, seamed so it can eventually be generic over
/// `PipeEndpoint`/`SocketEndpoint` (LU3b/LU3c) — today, both implement
/// this purely by delegation to free functions each module already
/// exposes, and no consumer is generic over it yet.
pub trait Endpoint {
    type Client: Client;
    type Process: PeerIdentity;

    /// Connect to the row's own socket, with NO authentication — every real caller runs [`Self::authenticate_server`] or
    /// [`Self::challenge`] on top; see either concrete module's own doc for why the raw connect and the identity proof
    /// stay two separately observed steps. `row` is the row in this endpoint's own namespace: the state-dir hash for the
    /// platform endpoints, the row's session name for the daemon-lane endpoint, which reaches the socket through the
    /// row's own daemon.
    fn connect_row_unchallenged(&self, row: &str) -> Result<Self::Client, TransportError>;
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
    /// Whether a dial through this endpoint can start at all right now.
    /// An endpoint reached over a link another component watches (an ssh
    /// lane) reports that link here, so a worker pauses instead of dialing
    /// into it. Local endpoints have no such link and are always up.
    fn link_up(&self) -> bool {
        true
    }
}

/// L1-unix LU3b: the endpoint a process speaks on the platform it runs
/// on — the frontend (`attach_client/client.rs`) and `supervisor_client` are
/// generic over [`Endpoint`] and instantiated with this, the ONLY place
/// the platform is chosen for a client. Windows speaks `PipeEndpoint`,
/// Linux and macOS BOTH speak `SocketEndpoint` — one alias arm, not two,
/// because the Unix-socket endpoint is one implementation whose per-OS
/// half is chosen inside `socket_unix` by its own `challenge_os` alias.
/// No other platform has an endpoint yet (a generic build for one still
/// needs a concrete `Endpoint` to monomorphize against, which is exactly
/// what does not exist off these three today — see `attach_client::client`'s own
/// top-of-module `cfg`).
#[cfg(windows)]
pub type PlatformEndpoint = crate::lane::pipe_win::PipeEndpoint;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub type PlatformEndpoint = crate::lane::socket_unix::SocketEndpoint;
