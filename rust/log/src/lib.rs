//! The capsule: a row's terminal outlives the window and the daemon (decision D2). One supervisor per state dir is
//! the authority over its runs, runs the agent on a real terminal and keeps its screen, and every viewer attaches
//! through one client over the row socket. No append-only record of the terminal is kept.

pub mod attach_client;
pub mod capsule;
pub mod endpoint;
pub mod row;
pub mod storage;
pub mod supervisor;

pub use storage::storage_exhaustion;

/// This build's own identity, a diagnostic stamp: the row's hello carries it as `build`, and `sot-capsule build-id` and
/// the log and pane lines that name a peer's build read it. It is never compared: the row number is the only gate. A
/// bare crate version would not serve even that purpose, since two different commits can share one `Cargo.toml` version
/// between releases, so `build.rs` stamps `SOT_LOG_BUILD_SHA` from the FULL `git rev-parse HEAD`, `-dirty`-suffixed over
/// an uncommitted tree (the same git-sha capture `rust/protocol/build.rs` uses for `app_version`; the stamp stays its
/// own). `build.rs` FAILS the build rather than emit an empty or ambiguous value when no git repository is found, so
/// this constant trusts the env var is always a real, nonempty identity.
pub const BUILD_ID: &str = env!("SOT_LOG_BUILD_SHA");

/// A supervisor's phase, total over its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorPhase {
    Starting,
    Ready,
    Ending,
    EndedNoRespawn,
    Terminal,
}

/// Errors are split by what the caller may do about them.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("schema: {0}")]
    Schema(String),
    #[error("state: {0}")]
    State(String),
    /// The row's hello declared another row number than this client speaks (`RowError::RowMismatch`, seen by
    /// `supervisor_client`'s dial). Typed so a caller (`sot-backend`'s `phase_of`) can match on it directly instead
    /// of substring-matching the generic `Foreign` text, which also covers a malformed reply, trailing bytes, or a
    /// wrong pid or creation and does NOT mean this.
    #[error("the row speaks another row protocol number")]
    VersionSkew,
    /// A ConPTY/job OPERATION failed (Windows-only: `conpty` module) — a
    /// spawn stage, or a later runtime call (`terminate`, `active_processes`,
    /// `resize`, ...). Carries WHICH operation and the underlying Win32
    /// error, so a caller can commit `producer_dead {spawn_failed}` for a
    /// creation-time failure, or a resize/teardown outcome for a later one,
    /// with a real diagnostic instead of a bare string. This variant is
    /// deliberately not named/worded "spawn" — an earlier version was, and a
    /// `resize()` failure read as a spawn failure it never was (review
    /// finding on the conpty unit).
    #[cfg(windows)]
    #[error("conpty: {0}")]
    Conpty(#[from] capsule::producer::conpty::ConptyError),
    /// A transport operation failed: platform's one `TransportError`, which serves both platforms' clients. A later,
    /// background failure on an already-bound server never reaches this type.
    #[error("transport: {0}")]
    Transport(#[from] sot_platform::transport::TransportError),
}

impl From<sot_platform::Error> for Error {
    fn from(e: sot_platform::Error) -> Self {
        match e {
            sot_platform::Error::Io(e) => Error::Io(e),
            sot_platform::Error::State(s) => Error::State(s),
            sot_platform::Error::Path { what, source } => Error::Io(std::io::Error::new(source.kind(), format!("{what}: {source}"))),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
