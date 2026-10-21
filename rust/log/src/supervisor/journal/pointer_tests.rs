use super::*;

fn fresh_voyage_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

// macOS (non-Linux unix) fails closed in host::rename_noreplace_raw
// (host.rs: "renamex_np when a macOS FE exists to dogfood it" -- ADR
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

/// Concurrent FIRST publication: several
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

/// Whitespace surrounding an otherwise-valid UUID
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

/// Invalid UTF-8 is malformed CONTENT (`Corrupt`),
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

/// A file over the canonical length is rejected
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
