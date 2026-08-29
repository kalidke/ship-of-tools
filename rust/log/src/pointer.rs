//! `<state-dir>/drawer.voyage`: the WRITE-ONCE pointer naming a drawer's
//! current voyage (ADR 0041 Lifecycle "Discovery, and the two windows a
//! spawn passes through"). Published through this crate's OWN pinned
//! crash-durable order — temp file → write → flush → NO-REPLACE rename →
//! renamed-file flush → parent-directory flush — by calling
//! [`fsutil::publish_noreplace`] rather than reimplementing that sequence
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
//! Validation is BYTE-EXACT (ADR 0041 U0 round-1 finding 7): the read is
//! bounded to one byte past the canonical length (so an oversized file is
//! rejected without ever loading it into memory), no whitespace is
//! trimmed (a file wrapping a valid UUID in whitespace is exactly as
//! corrupt as one that doesn't contain a UUID at all), and invalid UTF-8
//! is `Corrupt` — malformed CONTENT — never `OtherIo`, which this module
//! reserves for actual operational failures (permission denial, a
//! transient read error).

use crate::{fsutil, Result};
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
/// and `pipe_win::validate_voyage_id`'s pipe-name guard, which delegates
/// here — ADR 0041 U0 round-1 minor finding 9: one implementation, not
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

    // macOS (non-Linux unix) fails closed in fsutil::rename_noreplace_raw
    // (fsutil.rs: "renamex_np when a macOS FE exists to dogfood it" -- ADR
    // 0041 scope note) -- publish() can never succeed there today, so this
    // publish-exercising test is gated the same way the store's own
    // voyage/segment/recovery test suites already gate theirs.
    #[cfg(any(target_os = "linux", windows))]
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

    #[cfg(any(target_os = "linux", windows))]
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

    /// Concurrent FIRST publication (round-1 required test): several
    /// threads racing `publish` over the SAME absent pointer must produce
    /// exactly one winner — the no-clobber rename is what arbitrates, so
    /// this proves that arbitration under REAL contention, not merely by
    /// inspecting the code.
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn concurrent_first_publications_have_exactly_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let ids: Vec<String> = (0..8).map(|_| fresh_voyage_id()).collect();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(ids.len()));
        let results: Vec<_> = ids
            .iter()
            .map(|id| {
                let dir_path = dir.path().to_path_buf();
                let id = id.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    publish(&dir_path, &id)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();

        let winners = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(winners, 1, "exactly one publish must win: {results:?}");
        match validate(dir.path()) {
            PointerState::Valid(got) => assert!(ids.contains(&got)),
            other => panic!("expected Valid, got {other:?}"),
        }
        // No leftover temp-file residue from any loser or the winner's
        // own attempt.
        let residue: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&format!("{POINTER_FILE_NAME}.tmp-")))
            .collect();
        assert!(residue.is_empty(), "temp-file residue: {residue:?}");
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
        std::fs::write(pointer_path(dir.path()), b"NOT-A-UUID-BUT-EXACTLY-36-BYTES!!!!!").unwrap();
        assert!(matches!(validate(dir.path()), PointerState::Corrupt));
        std::fs::write(
            pointer_path(dir.path()),
            uuid::Uuid::now_v7().to_string().to_uppercase(),
        )
        .unwrap();
        assert!(matches!(validate(dir.path()), PointerState::Corrupt));
    }

    /// Round-1 finding 7: whitespace surrounding an otherwise-valid UUID
    /// must be rejected, not silently trimmed — the byte-exact rule and
    /// the ADR's own "empty or not canonical is CORRUPT" both demand it.
    #[test]
    fn validate_reports_corrupt_for_whitespace_padded_uuid() {
        let dir = tempfile::tempdir().unwrap();
        let id = fresh_voyage_id();
        std::fs::write(pointer_path(dir.path()), format!("{id}\n")).unwrap();
        assert!(matches!(validate(dir.path()), PointerState::Corrupt));
        std::fs::write(pointer_path(dir.path()), format!(" {}", &id[..id.len() - 1])).unwrap();
        assert!(matches!(validate(dir.path()), PointerState::Corrupt));
    }

    /// Round-1 finding 7: invalid UTF-8 is malformed CONTENT (`Corrupt`),
    /// never `OtherIo` — a corrupt byte sequence is not an operational
    /// I/O failure.
    #[test]
    fn validate_reports_corrupt_for_invalid_utf8() {
        let dir = tempfile::tempdir().unwrap();
        // Exactly 36 bytes, but with an invalid UTF-8 continuation byte
        // in the middle -- the LENGTH check alone must not short-circuit
        // past the UTF-8 check.
        let mut bytes = vec![b'0'; CANONICAL_UUID_LEN];
        bytes[10] = 0xff;
        std::fs::write(pointer_path(dir.path()), &bytes).unwrap();
        assert!(matches!(validate(dir.path()), PointerState::Corrupt));
    }

    /// Round-1 finding 7: a file over the canonical length is rejected
    /// WITHOUT reading it in full — this test's own file is small enough
    /// to load either way, but the assertion is on the OUTCOME
    /// (`Corrupt`, never treated as a truncatable/valid prefix).
    #[test]
    fn validate_reports_corrupt_for_excessive_length() {
        let dir = tempfile::tempdir().unwrap();
        let mut oversized = fresh_voyage_id();
        oversized.push_str("-trailing-garbage-that-makes-this-far-too-long");
        std::fs::write(pointer_path(dir.path()), &oversized).unwrap();
        assert!(matches!(validate(dir.path()), PointerState::Corrupt));
    }

    /// `OtherIo`, distinct from `Corrupt`: an operational failure reading
    /// an otherwise-well-formed pointer (unix permission denial — the
    /// only portable way to force this deterministically). Hand-writes
    /// the pointer directly rather than calling `publish` (which is
    /// gated to Linux/Windows -- see the tests above): this test only
    /// needs A FILE with valid canonical content to exist, never the
    /// crash-durable publish sequence itself, so it stays portable and
    /// keeps running on macOS.
    #[test]
    #[cfg(unix)]
    fn validate_reports_other_io_for_a_permission_denied_pointer() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let id = fresh_voyage_id();
        std::fs::write(pointer_path(dir.path()), &id).unwrap();
        let path = pointer_path(dir.path());
        let original_perms = std::fs::metadata(&path).unwrap().permissions();
        struct RestorePerms(PathBuf, std::fs::Permissions);
        impl Drop for RestorePerms {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, self.1.clone());
            }
        }
        let _restore = RestorePerms(path.clone(), original_perms);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root (and some CI containers) can read a 0o000 file anyway --
        // skip rather than false-fail where that's true.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        assert!(matches!(validate(dir.path()), PointerState::OtherIo(_)));
    }

    #[cfg(any(target_os = "linux", windows))]
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
