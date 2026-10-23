//! The capsule: a row's terminal outlives the window and the daemon (decision D2). One supervisor per state dir is
//! the authority over its runs, one leg runs the agent on a real terminal and keeps its screen, and every viewer
//! attaches through one client over typed lanes. No append-only record of the terminal is kept.

pub mod attach_client;
pub mod capsule;
pub mod lane;
pub mod row;
pub mod secret;
pub mod storage;
pub mod supervisor;
#[cfg(all(unix, feature = "native-barrier"))]
pub mod test_barrier;
#[cfg(feature = "test-support")]
pub mod test_log;
#[cfg(feature = "test-support")]
pub mod test_scan;
#[cfg(feature = "test-support")]
pub mod test_copy;
#[cfg(all(unix, feature = "test-support"))]
pub mod test_foreign;

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
    /// The supervisor lane answered but refused this client
    /// (`SupervisorReply::Refused { reason: VersionSkew }`, ADR 0030 §8
    /// decision 31c) — typed so a caller (`sot-backend`'s `phase_of`) can
    /// match on it directly instead of substring-matching the generic
    /// `Foreign`-challenge `State` text, which also covers a malformed
    /// reply, trailing bytes, or a wrong pid/creation and does NOT mean
    /// this. `supervisor_client::connect_and_challenge` is the only place
    /// this is constructed. Display text is deliberately non-committal
    /// about the cause (Codex review, 2026-09-11): ADR 0045 decision 7
    /// retired the build-boundary gate this used to mean exclusively, so
    /// during the migration window the SAME refusal can come from either
    /// a genuinely different lane protocol or an OLD, pre-ADR-0045
    /// supervisor still refusing on build — the wire alone can't say
    /// which, so this never claims to.
    #[error("supervisor lane refused: another lane protocol, or a supervisor from before the protocol-only gate")]
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
    /// A transport OPERATION failed — the former Windows-only `Pipe`
    /// variant (`pipe_win::PipeError`) and Unix-only `Socket` variant
    /// (`socket_unix::SocketError`) merged into one ungated variant (ADR
    /// 0043 decisions 17/19: one `transport::TransportError` now serves
    /// both platforms). A LATER, background failure on an already-bound server
    /// (`LaneEvent::AcceptError`, the accept loop's persistent-failure
    /// signal) never reaches this type: it arrives as an event.
    #[error("transport: {0}")]
    Transport(#[from] lane::transport::TransportError),
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
