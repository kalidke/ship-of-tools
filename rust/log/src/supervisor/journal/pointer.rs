//! `<state-dir>/drawer.voyage`: the WRITE-ONCE pointer naming a drawer's
//! current voyage (ADR 0041 Lifecycle "Discovery, and the two windows a
//! spawn passes through"). Published through this crate's OWN pinned
//! crash-durable order — temp file → write → flush → NO-REPLACE rename →
//! renamed-file flush → parent-directory flush — by calling
//! [`host::publish_noreplace`] rather than reimplementing that sequence
//! a second time.
//!
//! Validation is a typed result that keeps four outcomes distinct on
//! purpose: `Valid`, `Corrupt` (exists but not exactly 36 bytes of the
//! canonical UUID text), `NotFound`, and `OtherIo`. ADR 0039 pins absence
//! of data as corruption, ALWAYS, and "missing" can equally be an access
//! denial, a transient I/O failure, or an interrupted move — collapsing
//! any of these into a single "not there, mint a fresh one" case is
//! exactly the licence-to-re-mint bug this module exists to refuse.
//! Every caller of `validate` gets a LOUD STOP naming `reset` for
//! anything but `Valid`; this module itself makes no such decision — it
//! only tells the caller which case it is.
//!
//! Validation is BYTE-EXACT: the read is
//! bounded to one byte past the canonical length (so an oversized file is
//! rejected without ever loading it into memory), no whitespace is
//! trimmed (a file wrapping a valid UUID in whitespace is exactly as
//! corrupt as one that doesn't contain a UUID at all), and invalid UTF-8
//! is `Corrupt` — malformed CONTENT — never `OtherIo`, which this module
//! reserves for actual operational failures (permission denial, a
//! transient read error).

use crate::Result;
use sot_platform::host;
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// The pointer's fixed file name. Every function here takes the STATE DIR
/// and joins this itself, so a caller can never validate or publish under
/// a sibling name by accident.
const POINTER_FILE_NAME: &str = "drawer.voyage";

/// The exact byte length of the canonical lowercase-hyphenated form of an
/// RFC 4122 UUID: 8-4-4-4-12 hex digits (32) plus 4 hyphens.
const CANONICAL_UUID_LEN: usize = 36;

/// `<state_dir>/drawer.voyage`.
pub fn pointer_path(state_dir: &Path) -> PathBuf {
    state_dir.join(POINTER_FILE_NAME)
}

/// What reading the pointer under `state_dir` found.
#[derive(Debug)]
pub enum PointerState {
    /// Exists and is the canonical lowercase-hyphenated form of an RFC
    /// 4122 UUID — ready to use as a voyage id.
    Valid(String),
    /// Exists but is not EXACTLY the 36-byte canonical UUID text — empty,
    /// too short, too long, wrapped in whitespace, non-canonical, or not
    /// valid UTF-8 at all. A loud stop naming `reset`, never a silent
    /// re-mint.
    Corrupt,
    /// The pointer path itself does not exist. This is NOT, by itself, a
    /// licence to mint a fresh voyage — the caller (acting under the
    /// `supervisor.lock` fence) decides what "no drawer yet" means.
    NotFound,
    /// Any I/O failure reading the pointer OTHER than not-found
    /// (permission denial, a transient error, the residue of an
    /// interrupted move) — as loud as `Corrupt`, never folded into
    /// `NotFound`, and never confused with malformed CONTENT (which is
    /// `Corrupt`, not this).
    OtherIo(std::io::Error),
}

/// Read and validate `<state_dir>/drawer.voyage`. Bounded: reads at most
/// [`CANONICAL_UUID_LEN`] + 1 bytes, so a file of unbounded size is never
/// loaded in full just to discover it is too long.
pub fn validate(state_dir: &Path) -> PointerState {
    let path = pointer_path(state_dir);
    let mut file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return PointerState::NotFound,
        Err(e) => return PointerState::OtherIo(e),
    };
    let mut buf = [0u8; CANONICAL_UUID_LEN + 1];
    let mut filled = 0usize;
    loop {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                if filled == buf.len() {
                    break; // definitely over length -- no need to read further
                }
            }
            Err(e) => return PointerState::OtherIo(e),
        }
    }
    let bytes = &buf[..filled];
    if bytes.len() != CANONICAL_UUID_LEN {
        return PointerState::Corrupt; // too short, too long, or empty
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return PointerState::Corrupt; // invalid UTF-8 is malformed CONTENT, not OtherIo
    };
    match canonical_voyage_id(text) {
        Some(id) => PointerState::Valid(id),
        None => PointerState::Corrupt,
    }
}

/// `Some(canonical)` iff `text` is ALREADY the canonical lowercase-
/// hyphenated form [`uuid::Uuid`]'s own `Display` produces — the ONE
/// canonical-UUID check this crate uses everywhere a voyage id's shape
/// must be pinned to exactly one spelling (this pointer's own content,
/// and `lane::transport::validate_voyage_id`'s endpoint-name guard, which delegates
/// here — one implementation, not
/// two that can drift). `Uuid::parse_str` alone accepts strictly more
/// shapes (uppercase hex, the 32-hex-digit "simple" form, braced GUIDs,
/// `urn:uuid:...`) than either caller ever accepts; round-tripping
/// through `to_string` and requiring a byte-identical match pins both to
/// exactly one shape. `pub(crate)`: portable, so the Windows-only
/// `pipe_win` module can reach it too.
pub(crate) fn canonical_voyage_id(text: &str) -> Option<String> {
    let id = uuid::Uuid::parse_str(text).ok()?;
    let canonical = id.to_string();
    (canonical == text).then_some(canonical)
}

/// Publish `voyage_id` (must already be the canonical form; this function
/// validates it and never re-derives or reformats it) as `state_dir`'s
/// pointer. WRITE-ONCE: a second publication against an existing pointer
/// fails through the identical `AlreadyExists` path a racing writer would
/// hit — [`host::publish_noreplace`]'s no-clobber rename IS the
/// write-once enforcement, so this function adds no second check for it.
pub fn publish(state_dir: &Path, voyage_id: &str) -> Result<()> {
    let canonical = canonical_voyage_id(voyage_id).ok_or_else(|| {
        crate::Error::State(format!("{voyage_id:?} is not a canonical voyage id"))
    })?;
    let target = pointer_path(state_dir);

    // Attempt-owned random suffix — the same shape `voyage.rs`'s staging
    // dir and blob temp names use: two concurrent publish attempts must
    // never race one shared temp path.
    let mut nonce_bytes = [0u8; 8];
    getrandom::fill(&mut nonce_bytes).map_err(std::io::Error::from)?;
    let nonce = u64::from_le_bytes(nonce_bytes);
    let tmp = state_dir.join(format!("{POINTER_FILE_NAME}.tmp-{nonce:016x}"));

    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(canonical.as_bytes())?;
        f.sync_all()?;
    }
    let result = host::publish_noreplace(&tmp, &target);
    if result.is_err() {
        // A lost race or any other publish failure: don't leave this
        // attempt's temp file behind as residue.
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(crate::Error::from)
}

#[cfg(test)]
#[path = "pointer_tests.rs"]
mod tests;
