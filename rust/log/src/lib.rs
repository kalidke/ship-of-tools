//! Ship's Log voyage store — frame codec + segment format.
//!
//! ADR 0039 is the normative contract; this crate implements it. The bytes
//! this crate writes are read forever, so behavior here favors refusing
//! loudly over guessing: only a *provably* torn tail is ever discarded, and
//! every other defect halts. Nothing is ever deleted (v1 has no GC, no
//! retention deletion, no forks, no packs — those return through the
//! `codec_id` / `required_features` / version seams).

pub mod attach_client;
pub mod capsule;
pub mod claude;
pub mod host;
pub mod identity;
pub mod lane;
pub mod secret;
pub mod store;
pub mod supervisor;

pub use host::lock_writer;
#[cfg(windows)]
pub use host::owner_protected_pipe_descriptor;

pub use store::envelope::*;
pub use store::record::{RecordKind, TailClass, CODEC_JSON, MAGIC, PRELUDE_LEN, RECORD_MAX_BODY};
pub use store::segment::{SegmentIdentity, SegmentReader, SegmentState, SegmentWriter};

/// Errors are split by what the caller may do about them: `TornTail` is the
/// ONLY recoverable corruption (ADR 0039 tail rule); everything else under
/// `Corrupt` requires an operator.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Provably incomplete final record of an unsealed file (tail rule cases
    /// a/b). Recovery may truncate exactly this.
    #[error("torn tail at offset {offset}: {what}")]
    TornTail { offset: u64, what: &'static str },
    /// Any other defect: loud, never auto-repaired.
    #[error("corrupt at offset {offset}: {what}")]
    Corrupt { offset: u64, what: String },
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
    /// U1a (ADR 0041 Lifecycle "Discovery, and the two windows"):
    /// `VoyageStore::open_for_writing_with_lease`'s caller-supplied
    /// parent-death lease reported itself already broken, checked as the
    /// FIRST act after the writer fence is acquired and before any history
    /// traversal. The fence has already been released by the time this
    /// error returns — the caller (a spawned child whose supervisor is
    /// already gone) must exit without binding, never retry or repair.
    #[error("parent-death lease broken; exiting without binding the voyage")]
    LeaseBroken,
    /// The voyage store requires OS durability primitives this platform
    /// lacks (Windows, Linux and macOS have real arms; any other Unix
    /// fails closed).
    #[error("unsupported on this platform: {0}")]
    Unsupported(&'static str),
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
    /// both platforms). Binding the transport (`PipeServer::bind`/
    /// `SocketServer::bind`, inside `PlatformTransport::bind`) is the ONLY
    /// place `platform_transport.rs` converts one of these into this crate's own
    /// `Error`. A LATER, background failure on an already-bound transport
    /// (`LaneEvent::AcceptError`, the accept loop's persistent-failure
    /// signal) never reaches this type at all — the bridge translates it
    /// to `transport::TransportEvent::TransportFatal` instead, delivered
    /// through `Transport::try_recv_event` like any other transport
    /// event, since by then `run` is already past `bind` and mid-loop, not
    /// somewhere a `Result` could propagate to.
    #[error("transport: {0}")]
    Transport(#[from] lane::transport::TransportError),
}

pub type Result<T> = std::result::Result<T, Error>;
