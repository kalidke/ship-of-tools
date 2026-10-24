//! The endpoint seam: what a viewer or the daemon needs from one platform's connect and challenge machinery to reach a
//! row's socket by the row's name, seamed so the attach client and `supervisor_client` are generic over it. The clients
//! it hands back are platform's (`sot_platform::transport`); what is the capsule's here is the row's own socket path
//! (`crate::row::row_socket_path`) and its failures.
//!
//! `PipeEndpoint` on Windows, `SocketEndpoint` on Linux and macOS; [`PlatformEndpoint`] names the one a process speaks.

use sot_platform::identity::challenge::{ChallengeOutcome, PeerAuthOutcome};
use sot_platform::identity::exchange::IdentityExchange;
use sot_platform::identity::peer::PeerIdentity;
use sot_platform::transport::{Client, TransportError};
// The platform's own challenge module for the Unix transport, chosen ONCE by an alias. Linux reads `SO_PEERCRED` plus a
// pidfd pin; macOS reads `getpeereid` for the account and one `LOCAL_PEERTOKEN` audit token carrying the peer's pid AND
// the kernel's own reuse generation together. No other Unix has one.
#[cfg(target_os = "macos")]
use sot_platform::identity::challenge_macos as challenge_os;
#[cfg(target_os = "linux")]
use sot_platform::identity::challenge_unix as challenge_os;
use std::time::Instant;

/// What a caller needs from one platform's connect and challenge machinery to reach a row.
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

/// The endpoint a process speaks on the platform it runs on: the window (`attach_client/client.rs`) and
/// `supervisor_client` are generic over [`Endpoint`] and instantiated with this, the ONLY place the platform is chosen
/// for a client. Windows speaks `PipeEndpoint`; Linux and macOS both speak `SocketEndpoint`, one implementation whose
/// per-OS half is chosen by the `challenge_os` alias above. No other platform has an endpoint.
#[cfg(windows)]
pub type PlatformEndpoint = PipeEndpoint;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub type PlatformEndpoint = SocketEndpoint;

/// A row socket path that could not be made, as a transport error.
fn row_error_to_transport(e: crate::row::RowError) -> TransportError {
    match e {
        crate::row::RowError::PathTooLong(path) => TransportError::PathTooLong(path),
        crate::row::RowError::Io(source) => TransportError::RuntimeDir(source),
        other => TransportError::RuntimeDir(std::io::Error::other(other)),
    }
}

/// The Unix-socket `Endpoint`: a unit struct delegating to platform's connector and challenge functions. One
/// implementation for both targets that have a trusted peer-identity mechanism.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Copy, Default)]
pub struct SocketEndpoint;

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Endpoint for SocketEndpoint {
    type Client = sot_platform::transport::socket_unix::SocketClient;
    type Process = challenge_os::ChallengedProcess;

    fn connect_row_unchallenged(&self, row: &str) -> Result<Self::Client, TransportError> {
        let path = crate::row::row_socket_path(row).map_err(row_error_to_transport)?;
        sot_platform::transport::connect_own::connect_own(&path)
    }

    fn challenge(
        &self,
        conn: &Self::Client,
        exchange: &mut dyn IdentityExchange,
        reply_deadline: Instant,
    ) -> ChallengeOutcome<Self::Process> {
        challenge_os::challenge(conn, exchange, reply_deadline)
    }

    fn authenticate_server(&self, conn: &Self::Client) -> PeerAuthOutcome {
        challenge_os::authenticate_server(conn)
    }
}

/// The Windows `Endpoint`: a unit struct delegating to platform's connector and challenge functions.
#[cfg(windows)]
#[derive(Clone, Copy, Default)]
pub struct PipeEndpoint;

#[cfg(windows)]
impl Endpoint for PipeEndpoint {
    type Client = sot_platform::transport::pipe_win::PipeClient;
    type Process = sot_platform::identity::challenge_win::ChallengedProcess;

    fn connect_row_unchallenged(&self, row: &str) -> Result<Self::Client, TransportError> {
        let path = crate::row::row_socket_path(row).map_err(row_error_to_transport)?;
        sot_platform::transport::connect_own::connect_own(&path)
    }

    fn challenge(
        &self,
        conn: &Self::Client,
        exchange: &mut dyn IdentityExchange,
        reply_deadline: Instant,
    ) -> ChallengeOutcome<Self::Process> {
        sot_platform::identity::challenge_win::challenge(conn, exchange, reply_deadline)
    }

    fn authenticate_server(&self, conn: &Self::Client) -> PeerAuthOutcome {
        sot_platform::identity::challenge_win::authenticate_server(conn)
    }
}
