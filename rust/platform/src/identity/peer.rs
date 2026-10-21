//! What a proven peer process offers its caller, whichever platform proved it (implemented by each platform's
//! `ChallengedProcess`).

use std::time::Duration;

/// The methods both platforms' `ChallengedProcess` share — everything the
/// full five-step [`super::challenge::ChallengeOutcome::Proven`] proof
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
