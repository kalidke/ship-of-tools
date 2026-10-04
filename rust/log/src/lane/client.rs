//! L1-unix LU3a (ADR 0043 decision 19): the three seam traits landed
//! BEFORE any consumer uses them — every existing call site still names
//! `PipeClient`/`SocketClient`/`PipeServer`/`SocketServer`/
//! `challenge_win::ChallengedProcess`/`challenge_unix::ChallengedProcess`
//! directly; this module only adds the trait vocabulary and the
//! delegating `impl`s that prove each concrete type already satisfies
//! it. Generic consumers (`attach_client::client`, `supervisor`) are LU3b/LU3c's
//! job, not this lane's.
//!
//! Ungated, like `challenge.rs`/`transport.rs`: this is the CONTRACT, not
//! an implementation. `Client`/`Endpoint`'s two implementors
//! (`PipeClient`/`PipeEndpoint` on Windows, `SocketClient`/
//! `SocketEndpoint` on Linux) live in `lane/pipe_win/`/`lane/socket_unix/`
//! themselves, next to the concrete types they delegate to — the same
//! placement `impl crate::identity::challenge::ChallengeableConnection for
//! PipeClient` used to have, before the blanket impl below replaced it.

use crate::identity::challenge::{ChallengeOutcome, ChallengeableConnection, PeerAuthOutcome};
use crate::identity::exchange::IdentityExchange;
use crate::lane::transport::TransportError;
use std::time::{Duration, Instant};

/// Maps [`PeerAuthOutcome`] to a `Result` — the exact logic both platforms'
/// voyage connects run, pulled out so it is directly unit-testable (U1a
/// Codex round-1, minor cluster: "a constructor-level failure-mapping
/// test") without needing an OS-level SID mismatch or OS-call failure
/// through a live endpoint, neither of which is constructible in CI (a
/// genuine Foreign result needs a second real account; the ADR itself
/// scopes that proof to step 7's real-machine suite).
pub(super) fn map_peer_auth_outcome(outcome: PeerAuthOutcome) -> Result<(), TransportError> {
    match outcome {
        PeerAuthOutcome::Authenticated(_) => Ok(()),
        PeerAuthOutcome::Foreign => Err(TransportError::Foreign),
        PeerAuthOutcome::Undetermined => Err(TransportError::Undetermined {
            via: "direct",
            detail: "peer identity authentication could not be completed".to_string(),
        }),
    }
}

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
/// full five-step [`crate::identity::challenge::ChallengeOutcome::Proven`] proof
/// earns a caller. ADR 0043 decision 33: the per-platform
/// challenged-process exit-status accessor (`GetExitCodeProcess` on
/// Windows, `PIDFD_GET_INFO` on Linux) is deleted outright — readerless
/// once the daemon's watchdog exists only for a `Child` it spawned
/// itself (`wait_and_classify` reads a real `tokio::process::Child`'s
/// own `ExitStatus`, never an adopted `ChallengedProcess`'s).
pub trait PeerIdentity: Send {
    /// The pid this process was proven to be, at proof time.
    fn pid(&self) -> u32;
    /// The creation/start time this process was proven against, in
    /// whatever units the platform's own wire `status_ok.created` field
    /// carries (compared for equality only, never interpreted as a
    /// calendar time).
    fn created(&self) -> u64;
}

/// The rest of what a step-6 supervisor (and, now, `attach_client::client`'s health
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

/// U1a Codex round-1, minor cluster: a constructor-level failure-mapping
/// test for `connect_voyage_pipe`'s own `map_peer_auth_outcome`, proving
/// the mapping code the constructor actually runs -- not `challenge`/
/// `authenticate_server` directly, and not through a live pipe (a genuine
/// OS-level Foreign/Undetermined through a real connection needs either a
/// second real account or an unreliable timing race, neither
/// constructible deterministically in CI; see `authenticate_server_is_
/// undetermined_when_step_one_itself_fails` in the integration test for
/// the OS-call-failure case proven against a real, deliberately invalid
/// handle instead). Lives here (not in `tests/pipe_win/`) because
/// `map_peer_auth_outcome` is a private implementation detail with no
/// reason to be `pub` merely for testability, and a pure mapping over
/// already-constructed `PeerAuthOutcome` values needs no real pipe --
/// exactly the kind of test this crate's OTHER pure-logic modules
/// (`attach_proto`, `wire`, `exchange`) already keep inline.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::challenge::{PeerAuthOutcome, PeerAuthenticated};

    #[test]
    fn map_peer_auth_outcome_authenticated_is_ok() {
        let outcome = PeerAuthOutcome::Authenticated(PeerAuthenticated { pid: 4242, created: 7 });
        assert!(map_peer_auth_outcome(outcome).is_ok());
    }

    #[test]
    fn map_peer_auth_outcome_foreign_is_the_typed_transport_error() {
        assert!(matches!(map_peer_auth_outcome(PeerAuthOutcome::Foreign), Err(TransportError::Foreign)));
    }

    #[test]
    fn map_peer_auth_outcome_undetermined_is_the_typed_transport_error() {
        assert!(matches!(
            map_peer_auth_outcome(PeerAuthOutcome::Undetermined),
            Err(TransportError::Undetermined { via: "direct", .. })
        ));
    }
}
