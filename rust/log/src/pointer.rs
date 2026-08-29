//! `<state-dir>/drawer.voyage`: the WRITE-ONCE pointer naming a drawer's
//! current voyage (ADR 0041 Lifecycle "Discovery, and the two windows a
//! spawn passes through"). Published through this crate's OWN pinned
//! crash-durable order — temp file → write → flush → NO-REPLACE rename →
//! renamed-file flush → parent-directory flush — by calling
//! [`fsutil::publish_noreplace`] rather than reimplementing that sequence
//! a second time.
//!
//! Validation is a typed result that keeps four outcomes distinct on
//! purpose: `Valid`, `Corrupt` (exists but empty or non-canonical),
//! `NotFound`, and `OtherIo`. ADR 0039 pins absence of data as corruption,
//! ALWAYS, and "missing" can equally be an access denial, a transient I/O
//! failure, or an interrupted move — collapsing any of these into a
//! single "not there, mint a fresh one" case is exactly the licence-to-
//! re-mint bug this module exists to refuse. Every caller of `validate`
//! gets a LOUD STOP naming `reset` for anything but `Valid`; this module
//! itself makes no such decision — it only tells the caller which case it
//! is.

use crate::{fsutil, Result};
use std::path::{Path, PathBuf};

/// The pointer's fixed file name. Every function here takes the STATE DIR
/// and joins this itself, so a caller can never validate or publish under
/// a sibling name by accident.
const POINTER_FILE_NAME: &str = "drawer.voyage";

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
    /// Exists but is empty or not the canonical form — a loud stop naming
    /// `reset`, never a silent re-mint.
    Corrupt,
    /// The pointer path itself does not exist. This is NOT, by itself, a
    /// licence to mint a fresh voyage — the caller (acting under the
    /// `supervisor.lock` fence) decides what "no drawer yet" means.
    NotFound,
    /// Any I/O failure reading the pointer OTHER than not-found
    /// (permission denial, a transient error, the residue of an
    /// interrupted move) — as loud as `Corrupt`, never folded into
    /// `NotFound`.
    OtherIo(std::io::Error),
}

/// Read and validate `<state_dir>/drawer.voyage`.
pub fn validate(state_dir: &Path) -> PointerState {
    match std::fs::read_to_string(pointer_path(state_dir)) {
        Ok(text) => match canonical_voyage_id(text.trim()) {
            Some(id) => PointerState::Valid(id),
            None => PointerState::Corrupt,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => PointerState::NotFound,
        Err(e) => PointerState::OtherIo(e),
    }
}

/// `Some(canonical)` iff `text` is ALREADY the canonical lowercase-
/// hyphenated form [`uuid::Uuid`]'s own `Display` produces.
/// `Uuid::parse_str` alone accepts strictly more shapes (uppercase hex,
/// the 32-hex-digit "simple" form, braced GUIDs, `urn:uuid:...`) than this
/// pointer is ever published in; round-tripping through `to_string` and
/// requiring a byte-identical match pins the file to exactly one shape.
/// (Mirrors `pipe_win::validate_voyage_id`'s reasoning for the same value
/// on the wire, duplicated here rather than shared: that function lives
/// in a `#![cfg(windows)]` module and this one must build on every
/// platform.)
fn canonical_voyage_id(text: &str) -> Option<String> {
    let id = uuid::Uuid::parse_str(text).ok()?;
    let canonical = id.to_string();
    (canonical == text).then_some(canonical)
}

/// Publish `voyage_id` (must already be the canonical form; this function
/// validates it and never re-derives or reformats it) as `state_dir`'s
/// pointer. WRITE-ONCE: a second publication against an existing pointer
/// fails through the identical `AlreadyExists` path a racing writer would
/// hit — [`fsutil::publish_noreplace`]'s no-clobber rename IS the
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
    let result = fsutil::publish_noreplace(&tmp, &target);
    if result.is_err() {
        // A lost race or any other publish failure: don't leave this
        // attempt's temp file behind as residue.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_voyage_id() -> String {
        uuid::Uuid::now_v7().to_string()
    }

    #[test]
    fn publish_then_validate_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let id = fresh_voyage_id();
        publish(dir.path(), &id).unwrap();
        match validate(dir.path()) {
            PointerState::Valid(got) => assert_eq!(got, id),
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[test]
    fn a_second_publication_fails_write_once() {
        let dir = tempfile::tempdir().unwrap();
        let first = fresh_voyage_id();
        let second = fresh_voyage_id();
        publish(dir.path(), &first).unwrap();
        let err = match publish(dir.path(), &second) {
            Err(e) => e,
            Ok(()) => panic!("expected the second publication to fail"),
        };
        assert!(
            matches!(&err, crate::Error::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists),
            "{err}"
        );
        // The FIRST voyage id must still be the one on disk.
        match validate(dir.path()) {
            PointerState::Valid(got) => assert_eq!(got, first),
            other => panic!("expected Valid({first}), got {other:?}"),
        }
    }

    #[test]
    fn validate_reports_not_found_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(validate(dir.path()), PointerState::NotFound));
    }

    #[test]
    fn validate_reports_corrupt_for_empty_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(pointer_path(dir.path()), b"").unwrap();
        assert!(matches!(validate(dir.path()), PointerState::Corrupt));
    }

    #[test]
    fn validate_reports_corrupt_for_non_canonical_uuid_text() {
        let dir = tempfile::tempdir().unwrap();
        // Uppercase hex is a UUID `Uuid::parse_str` accepts but the
        // canonical form this pointer is pinned to rejects.
        std::fs::write(pointer_path(dir.path()), b"NOT-A-UUID").unwrap();
        assert!(matches!(validate(dir.path()), PointerState::Corrupt));
        std::fs::write(
            pointer_path(dir.path()),
            uuid::Uuid::now_v7().to_string().to_uppercase(),
        )
        .unwrap();
        assert!(matches!(validate(dir.path()), PointerState::Corrupt));
    }

    #[test]
    fn a_crash_between_create_and_publish_leaves_no_usable_permanent_pointer() {
        // Simulate the exact crash shape `publish` can leave: the temp
        // file was created and written, but the publish rename never ran
        // (process died first). Nothing named `drawer.voyage` should
        // exist — a leftover `.tmp-*` file is invisible to `validate`,
        // and a later `publish` call picks a FRESH random nonce so it
        // never collides with this residue.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(format!("{POINTER_FILE_NAME}.tmp-dead")), b"leftover").unwrap();
        assert!(matches!(validate(dir.path()), PointerState::NotFound));
        // A subsequent real publish still succeeds cleanly.
        let id = fresh_voyage_id();
        publish(dir.path(), &id).unwrap();
        match validate(dir.path()) {
            PointerState::Valid(got) => assert_eq!(got, id),
            other => panic!("expected Valid, got {other:?}"),
        }
    }

    #[test]
    fn publish_refuses_a_non_canonical_voyage_id() {
        let dir = tempfile::tempdir().unwrap();
        let err = publish(dir.path(), "not-a-uuid").unwrap_err();
        assert!(matches!(err, crate::Error::State(_)), "{err}");
        assert!(matches!(validate(dir.path()), PointerState::NotFound));
    }
}
