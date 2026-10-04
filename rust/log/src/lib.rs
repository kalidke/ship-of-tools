//! Ship's Log voyage store — frame codec + segment format.
//!
//! ADR 0039 is the normative contract; this crate implements it. The bytes
//! this crate writes are read forever, so behavior here favors refusing
//! loudly over guessing: only a *provably* torn tail is ever discarded, and
//! every other defect halts. Nothing is ever deleted (v1 has no GC, no
//! retention deletion, no forks, no packs — those return through the
//! `codec_id` / `required_features` / version seams).

pub use lane::attach_proto;
pub mod capsule;
pub use capsule::producer;
#[cfg(windows)]
pub use capsule::producer::conpty::producer as producer_conpty;
#[cfg(unix)]
pub use capsule::producer::pty as producer_pty;
mod identity;
pub use identity::challenge;
#[cfg(windows)]
pub use identity::challenge_win;
#[cfg(target_os = "linux")]
pub use identity::challenge_unix;
#[cfg(target_os = "macos")]
pub use identity::challenge_macos;
mod lane;
pub use lane::client;
pub use supervisor::probe::classify;
pub mod claude;
#[cfg(windows)]
pub use capsule::producer::conpty;
pub use lane::transport;
#[cfg(windows)]
pub use lane::pipe_win;
#[cfg(windows)]
pub use lane::pipe_transport;
#[cfg(unix)]
pub use lane::socket_unix;
#[cfg(unix)]
pub use lane::socket_transport;
pub use supervisor::probe;
#[cfg(windows)]
pub use supervisor::probe::win as probe_win;
#[cfg(target_os = "linux")]
pub use supervisor::probe::unix as probe_unix;
#[cfg(target_os = "macos")]
pub use supervisor::probe::macos as probe_macos;
use capsule::producer::host_handshake;
pub use identity::deadline;
mod store;
pub use store::envelope;
pub use supervisor::journal::fence;
pub use identity::exchange;
pub use attach_client::rules as fe_client;
mod attach_client;
pub use attach_client::client as fe_client_io;
pub use attach_client::worker as attach_worker;
pub use supervisor::journal;
#[cfg(windows)]
pub use supervisor::lease_win as lease;
pub use supervisor::journal::pointer;
pub use store::record;
pub use store::recovery;
pub use store::rollout;
pub use store::segment;
mod host;
pub use host::state_dir;
pub mod supervisor;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub use attach_client::supervisor_client;
pub use store::verify;
pub use store::voyage;
pub use lane::wire;
#[cfg(windows)]
pub use host::winhandle;

use host as fsutil;

pub use fsutil::lock_writer;
#[cfg(windows)]
pub use fsutil::owner_protected_pipe_descriptor;

pub use envelope::*;
pub use record::{RecordKind, TailClass, CODEC_JSON, MAGIC, PRELUDE_LEN, RECORD_MAX_BODY};
pub use segment::{SegmentIdentity, SegmentReader, SegmentState, SegmentWriter};

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
    Conpty(#[from] conpty::ConptyError),
    /// A transport OPERATION failed — the former Windows-only `Pipe`
    /// variant (`pipe_win::PipeError`) and Unix-only `Socket` variant
    /// (`socket_unix::SocketError`) merged into one ungated variant (ADR
    /// 0043 decisions 17/19: one `transport::TransportError` now serves
    /// both platforms). Binding the transport (`PipeServer::bind`/
    /// `SocketServer::bind`, inside `PipeTransport::bind`/
    /// `SocketTransport::bind`) is the ONLY place `pipe_transport.rs`/
    /// `socket_transport.rs` convert one of these into this crate's own
    /// `Error`. A LATER, background failure on an already-bound transport
    /// (`LaneEvent::AcceptError`, the accept loop's persistent-failure
    /// signal) never reaches this type at all — the bridge translates it
    /// to `transport::TransportEvent::TransportFatal` instead, delivered
    /// through `Transport::try_recv_event` like any other transport
    /// event, since by then `run` is already past `bind` and mid-loop, not
    /// somewhere a `Result` could propagate to.
    #[error("transport: {0}")]
    Transport(#[from] transport::TransportError),
}

pub type Result<T> = std::result::Result<T, Error>;
