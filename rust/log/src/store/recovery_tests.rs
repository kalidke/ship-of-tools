use super::*;
use crate::store::envelope::Seq;
use crate::store::segment::{Commit, HeaderBody, RetentionClass, SegmentWriter};

fn header(index: u64, epoch: u64) -> HeaderBody {
    HeaderBody {
        version: 1,
        required_features: vec![],
        voyage_id: "voy".into(),
        segment_index: index,
        epoch,
        prev_seal_digest: None,
        created_wall_ms: 0,
        retention_class: (index == 0).then_some(RetentionClass::Discard),
    }
}

fn id(index: u64, epoch: u64) -> SegmentIdentity {
    SegmentIdentity {
        voyage_id: "voy".into(),
        segment_index: index,
        epoch,
    }
}

fn write_open_with_frames(dir: &Path, n_frames: u64) -> SegmentWriter {
    let mut w = SegmentWriter::create(dir, header(0, 1)).unwrap();
    for n in 1..=n_frames {
        w.append(&crate::store::segment::tests::test_env(1, n), Commit::Immediate)
            .unwrap();
    }
    w
}

#[test]
fn clean_open_stays_open() {
    let dir = tempfile::tempdir().unwrap();
    let w = write_open_with_frames(dir.path(), 2);
    drop(w); // file remains .open with durable records
    assert_eq!(
        reconcile(dir.path(), &id(0, 1), 2).unwrap(),
        Reconciled::StillOpen
    );
}

#[test]
fn torn_tail_recovers_and_keeps_valid_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let w = write_open_with_frames(dir.path(), 3);
    drop(w);
    let p = dir.path().join("00000000-00000000000001.open");
    let bytes = std::fs::read(&p).unwrap();
    // Tear: chop the last 5 bytes of the final record.
    std::fs::write(&p, &bytes[..bytes.len() - 5]).unwrap();
    assert_eq!(
        reconcile(dir.path(), &id(0, 1), 7).unwrap(),
        Reconciled::Recovered
    );
    let sealed = dir.path().join("00000000-00000000000001.sotseg");
    let r = SegmentReader::read(&sealed, true).unwrap();
    r.verify_seal().unwrap();
    assert_eq!(r.frames.len(), 2); // third frame was the torn one
    let seal = r.seal.as_ref().unwrap();
    assert_eq!(seal.recovered, Some(true));
    assert_eq!(seal.recovered_by_epoch, Some(7));
    assert!(seal.truncated_bytes.unwrap() > 0);
    assert_eq!(seal.last_seq, Some(Seq { epoch: 1, n: 2 }));
    // Quarantine retired; only the published file remains.
    assert!(!dir.path().join("00000000-00000000000001.recovering").exists());
    assert!(!dir
        .path()
        .join("00000000-00000000000001.recovering-out")
        .exists());
}

#[test]
fn headerless_open_reinitializes() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("00000000-00000000000001.open");
    std::fs::write(&p, b"").unwrap();
    assert_eq!(
        reconcile(dir.path(), &id(0, 1), 2).unwrap(),
        Reconciled::ReinitializedOpen
    );
    assert!(!p.exists());
}

#[test]
fn seal_in_open_publishes_as_is() {
    let dir = tempfile::tempdir().unwrap();
    // Produce a sealed file, then rename it BACK to .open to simulate a
    // crash between seal-fsync and rename.
    let w = write_open_with_frames(dir.path(), 1);
    w.seal(None).unwrap();
    let sealed = dir.path().join("00000000-00000000000001.sotseg");
    let open = dir.path().join("00000000-00000000000001.open");
    std::fs::rename(&sealed, &open).unwrap();
    assert_eq!(
        reconcile(dir.path(), &id(0, 1), 2).unwrap(),
        Reconciled::PublishedAsIs
    );
    SegmentReader::read(&sealed, true).unwrap().verify_seal().unwrap();
}

#[test]
fn mid_segment_corruption_is_loud_not_recovered() {
    let dir = tempfile::tempdir().unwrap();
    let w = write_open_with_frames(dir.path(), 3);
    drop(w);
    let p = dir.path().join("00000000-00000000000001.open");
    let mut bytes = std::fs::read(&p).unwrap();
    // Corrupt a byte in the MIDDLE (inside the first frame), not the tail.
    let hdr = crate::store::record::decode_at(&bytes, 0).unwrap().unwrap();
    bytes[hdr.wire_len + 20] ^= 0xFF;
    std::fs::write(&p, &bytes).unwrap();
    assert!(reconcile(dir.path(), &id(0, 1), 2).is_err());
    assert!(p.exists(), "loud path must not consume the file");
}

#[test]
fn orphan_staging_is_loud() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("00000000-00000000000001.recovering-out"),
        b"x",
    )
    .unwrap();
    assert!(reconcile(dir.path(), &id(0, 1), 2).is_err());
}

/// The review-finding regression: recovery must copy the valid prefix
/// BYTE-VERBATIM. A frame with an unknown envelope member and
/// deliberately non-sorted payload keys (hand-crafted wire bytes — the
/// struct serializer would normalize both) must survive recovery with
/// identical bytes: no key re-sorting, no unknown-member stripping.
#[test]
fn recovery_preserves_wire_bytes_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let w = SegmentWriter::create(dir.path(), header(0, 1)).unwrap();
    drop(w); // durable header only
    let p = dir.path().join("00000000-00000000000001.open");

    // Hand-crafted frame body: valid per the open-schema rules, but with
    // z-before-a key order and a member no Envelope field captures.
    let body = br#"{"seq":{"epoch":1,"n":1},"class":"lifecycle","source":{"emitter":"capsule","actor":{"kind":"unknown"},"derivation":"synthetic"},"t_wall_ms":0,"t_mono_us":0,"refs":[],"payload":{"kind":"producer_ready","zeta":1,"alpha":2},"zz_future_member":{"keep":"me"}}"#;
    let frame_wire = crate::store::record::encode(crate::store::record::RecordKind::Frame, body, None).unwrap();
    let mut bytes = std::fs::read(&p).unwrap();
    bytes.extend_from_slice(&frame_wire);
    let prefix_len = bytes.len();
    bytes.extend_from_slice(&frame_wire[..7]); // torn tail
    std::fs::write(&p, &bytes).unwrap();

    assert_eq!(
        reconcile(dir.path(), &id(0, 1), 9).unwrap(),
        Reconciled::Recovered
    );
    let sealed = std::fs::read(dir.path().join("00000000-00000000000001.sotseg")).unwrap();
    assert_eq!(
        &sealed[..prefix_len],
        &bytes[..prefix_len],
        "retained prefix must be byte-identical"
    );
    let text = String::from_utf8_lossy(&sealed[..prefix_len]);
    assert!(text.contains(r#""zeta":1,"alpha":2"#), "key order must survive");
    assert!(text.contains("zz_future_member"), "unknown member must survive");
    SegmentReader::read(&dir.path().join("00000000-00000000000001.sotseg"), true)
        .unwrap()
        .verify_seal()
        .unwrap();
}

/// Clean-survivor honesty (review finding 2): a quarantined tip with NO
/// tear gets a PLAIN seal — no recovery metadata, no false "torn tail".
#[test]
fn clean_quarantine_gets_plain_seal() {
    let dir = tempfile::tempdir().unwrap();
    let w = write_open_with_frames(dir.path(), 2);
    drop(w);
    let open = dir.path().join("00000000-00000000000001.open");
    std::fs::rename(&open, dir.path().join("00000000-00000000000001.recovering")).unwrap();
    assert_eq!(
        reconcile(dir.path(), &id(0, 1), 5).unwrap(),
        Reconciled::Recovered
    );
    let r = SegmentReader::read(&dir.path().join("00000000-00000000000001.sotseg"), true).unwrap();
    r.verify_seal().unwrap();
    let seal = r.seal.as_ref().unwrap();
    assert_eq!(seal.recovered, None, "clean content: no recovery metadata");
    assert_eq!(seal.truncation_reason, None);
    assert_eq!(seal.frame_count, 2);
}

#[test]
fn recovery_is_idempotent_from_quarantine() {
    let dir = tempfile::tempdir().unwrap();
    let w = write_open_with_frames(dir.path(), 2);
    drop(w);
    let open = dir.path().join("00000000-00000000000001.open");
    let bytes = std::fs::read(&open).unwrap();
    std::fs::write(&open, &bytes[..bytes.len() - 3]).unwrap();
    // Crash simulation: quarantine happened, then nothing.
    std::fs::rename(&open, dir.path().join("00000000-00000000000001.recovering")).unwrap();
    assert_eq!(
        reconcile(dir.path(), &id(0, 1), 5).unwrap(),
        Reconciled::Recovered
    );
    // Running reconciliation AGAIN on the final state builds nothing
    // new — still reports Sealed — but still restates the barrier.
    assert_eq!(reconcile(dir.path(), &id(0, 1), 5).unwrap(), Reconciled::Sealed);
}

/// Part 3 finding: the `[Sealed]` row must restate the publication
/// barrier on EVERY call, not just skip straight to `Ok`. Proved by
/// making `seg_dir` execute-only AFTER a first, ordinary call already
/// succeeded: traversable (the initial per-state `.exists()` stats still
/// resolve by name) but not openable, so `fsync_dir(seg_dir)` — reached
/// only if this call actually attempts the flush — fails loudly. The
/// OLD `[Sealed] => Ok(Reconciled::Sealed)` row never touched `seg_dir`
/// at all and would pass this unchanged.
#[test]
#[cfg(unix)]
fn sealed_reconcile_reflushes_on_every_call() {
    use std::os::unix::fs::PermissionsExt;

    struct RestorePerms(std::path::PathBuf, std::fs::Permissions);
    impl Drop for RestorePerms {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, self.1.clone());
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let w = write_open_with_frames(dir.path(), 1);
    w.seal(None).unwrap();

    assert_eq!(reconcile(dir.path(), &id(0, 1), 2).unwrap(), Reconciled::Sealed);

    let original = std::fs::metadata(dir.path()).unwrap().permissions();
    // Declared AFTER `dir`, so it drops (and restores permissions)
    // BEFORE `dir`'s own cleanup tries to `remove_dir_all` it.
    let _restore = RestorePerms(dir.path().to_path_buf(), original);
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o111)).unwrap();

    let e = reconcile(dir.path(), &id(0, 1), 2).unwrap_err();
    assert!(matches!(e, Error::Io(_)), "{e}");
}

/// Shared by every Windows-only test below: open `path` read-only,
/// sharing exactly `share` with any other handle. Each test uses this to
/// force `finish_publication`'s write-reopen (inside `flush_renamed`) to
/// fail with a sharing violation — but the hold must NOT also block
/// anything the OLD code path does to the same file, or old code fails
/// for an unrelated reason and the test "passes" against it for free,
/// worthless as a regression proof. `FILE_SHARE_READ` alone covers the
/// common case (old code only reads this file); `recovering_alone_...`
/// below additionally needs `FILE_SHARE_DELETE`, because old code's last
/// step deletes this exact file — see that test for why.
#[cfg(windows)]
fn hold_with_share(path: &std::path::Path, share: u32) -> std::fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(share)
        .open(path)
        .unwrap()
}

/// Windows variant of the `[Sealed]` restatement above: hold the
/// `.sotseg` file write-denied, so `finish_publication`'s renamed-target
/// flush fails with a sharing violation. Old code never reopens the
/// target at all, so this hold cannot affect it — the OLD `[Sealed] =>
/// Ok(Reconciled::Sealed)` row would pass this unchanged.
#[test]
#[cfg(windows)]
fn sealed_reconcile_reflushes_target_on_windows() {
    let dir = tempfile::tempdir().unwrap();
    let w = write_open_with_frames(dir.path(), 1);
    w.seal(None).unwrap();
    let sealed = dir.path().join("00000000-00000000000001.sotseg");

    let _held = hold_with_share(&sealed, windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ);

    let e = reconcile(dir.path(), &id(0, 1), 2).unwrap_err();
    assert!(matches!(e, Error::Io(_)), "{e}");
}

/// Part 3 finding: `[Recovering, Sealed]` must complete the barrier on
/// `.sotseg` BEFORE retiring `.recovering` — `.recovering` is the only
/// fallback if a prior incarnation's publish never finished flushing, so
/// it must survive a failure injected at that flush. Old code's ONLY
/// touch of `.sotseg` here is a read (`verify_seal`), so `FILE_SHARE_READ`
/// alone can't affect it — old code deletes `.recovering`, not `.sotseg`.
#[test]
#[cfg(windows)]
fn recovering_sealed_survives_barrier_failure_on_windows() {
    let dir = tempfile::tempdir().unwrap();
    let w = write_open_with_frames(dir.path(), 1);
    w.seal(None).unwrap();
    let sealed = dir.path().join("00000000-00000000000001.sotseg");
    let recovering = dir.path().join("00000000-00000000000001.recovering");
    // Presence is all this row needs from `.recovering` — it is never
    // read, only deleted after the barrier completes.
    std::fs::write(&recovering, b"quarantine residue").unwrap();

    let _held = hold_with_share(&sealed, windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ);

    let e = reconcile(dir.path(), &id(0, 1), 2).unwrap_err();
    assert!(matches!(e, Error::Io(_)), "{e}");
    assert!(recovering.exists(), ".recovering must survive a barrier failure");
}

/// Part 3 finding: `recover_from_quarantine` must restate `.recovering`'s
/// OWN publication before building `.recovering-out` from it —
/// `.recovering` can itself be crash residue.
///
/// DELETION-DENIAL TRAP (fooled two reviewers before this comment
/// existed): a hold with `FILE_SHARE_READ` alone ALSO denies delete —
/// and old code's LAST step in this path is `remove_file(&r_path)`,
/// deleting this exact file, AFTER it has already built and published
/// `.recovering-out` -> `.sotseg`. With read-only sharing, old code
/// reads fine, publishes fine, and only fails at that final delete —
/// by which point `.recovering-out` no longer exists either (it was
/// already renamed away). Both of this test's assertions would then
/// hold against OLD code too, making it worthless as a regression proof.
/// Adding `FILE_SHARE_DELETE` lets old code's delete succeed (so old
/// code returns `Ok`, and the test correctly fails against it), while
/// `finish_publication`'s write-reopen is still denied under NEW code,
/// failing before `.recovering-out` is ever created.
#[test]
#[cfg(windows)]
fn recovering_alone_reflushes_before_building_output_on_windows() {
    let dir = tempfile::tempdir().unwrap();
    let w = write_open_with_frames(dir.path(), 2);
    drop(w);
    let open = dir.path().join("00000000-00000000000001.open");
    let recovering = dir.path().join("00000000-00000000000001.recovering");
    std::fs::rename(&open, &recovering).unwrap();

    let _held = hold_with_share(
        &recovering,
        windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
            | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE,
    );

    let e = reconcile(dir.path(), &id(0, 1), 5).unwrap_err();
    assert!(matches!(e, Error::Io(_)), "{e}");
    assert!(
        !dir.path().join("00000000-00000000000001.recovering-out").exists(),
        "must restate .recovering's own barrier before building .recovering-out"
    );
}
