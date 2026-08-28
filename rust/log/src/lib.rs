//! Ship's Log voyage store — frame codec + segment format.
//!
//! ADR 0039 is the normative contract; this crate implements it. The bytes
//! this crate writes are read forever, so behavior here favors refusing
//! loudly over guessing: only a *provably* torn tail is ever discarded, and
//! every other defect halts. Nothing is ever deleted (v1 has no GC, no
//! retention deletion, no forks, no packs — those return through the
//! `codec_id` / `required_features` / version seams).

pub mod capsule;
pub mod capsule_win;
pub mod claude;
pub mod conpty;
// ADR 0041 step 5, unit U3: the Windows named-pipe transport (server +
// client). `pub`, matching `conpty`/`capsule_win`/`wire`: its tests live in
// `tests/pipe_win.rs`, a separate integration-test crate that can only ever
// reach `pub` items — the same reason those sibling modules are `pub`
// rather than `pub(crate)`.
pub mod pipe_win;
// Crate-private (Codex review finding, capsule_win.rs round): ADR 0041's
// "one private machine" ruling means this module's items are not part of
// the crate's public API — `capsule_win.rs` is the only real caller and
// reaches it via `crate::host_handshake::...`, which needs no `pub` beyond
// the crate boundary. Not `#[cfg(windows)]`: its own tests are pure bytes
// and run on every platform (see the module doc) — which is exactly why a
// plain (non-test) build on a non-Windows target now has NO caller at all
// for these now-private items (the only real caller, capsule_win.rs, is
// windows-only): `cfg_attr` suppresses the resulting dead_code warning
// there specifically, rather than losing it crate-wide or windows-only.
#[cfg_attr(not(windows), allow(dead_code))]
mod host_handshake;
pub mod envelope;
pub mod record;
pub mod recovery;
pub mod segment;
pub mod verify;
pub mod voyage;
pub mod wire;

mod fsutil;

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
    /// The voyage store requires OS durability primitives this platform
    /// lacks (Linux and Windows have real arms; others fail closed).
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
}

pub type Result<T> = std::result::Result<T, Error>;
