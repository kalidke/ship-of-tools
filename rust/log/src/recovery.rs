//! Startup reconciliation + tear recovery (ADR 0039 §Segment lifecycle 4).
//!
//! Recovery applies ONLY to provable tears; every loud condition halts. The
//! transaction: `.open` → `.recovering` (quarantined original) → build
//! `.recovering-out` (valid prefix + recovery seal) → publish `.sotseg`
//! (RENAME_NOREPLACE) → verify → unlink the original. Idempotent at every
//! crash point; states are distinguished by filename.

use crate::fsutil;
use crate::segment::{
    RecoveryMeta, SegmentIdentity, SegmentReader, SegmentState, SegmentWriter,
};
use crate::{Error, Result};
use std::path::{Path, PathBuf};

/// What reconciliation did for one segment identity.
#[derive(Debug, PartialEq, Eq)]
pub enum Reconciled {
    /// Published `.sotseg` present; nothing else to do.
    Sealed,
    /// `.open` had a durable header and no defect — still the live segment.
    StillOpen,
    /// A headerless/header-partial `.open` was reinitialized away (removed);
    /// the caller may recreate it (nothing could have been acked).
    ReinitializedOpen,
    /// A tear was recovered: original quarantine removed, `.sotseg` published.
    Recovered,
    /// `.open` carried a valid seal at EOF — published as-is.
    PublishedAsIs,
}

fn state_paths(seg_dir: &Path, id: &SegmentIdentity) -> [(SegmentState, PathBuf); 4] {
    [
        (SegmentState::Open, id.path(seg_dir, SegmentState::Open)),
        (SegmentState::Recovering, id.path(seg_dir, SegmentState::Recovering)),
        (
            SegmentState::RecoveringOut,
            id.path(seg_dir, SegmentState::RecoveringOut),
        ),
        (SegmentState::Sealed, id.path(seg_dir, SegmentState::Sealed)),
    ]
}

/// Reconcile one segment identity. MUST be called under the voyage writer
/// lock. `recovering_epoch` stamps recovery seals (the epoch doing the
/// recovery, not the one that wrote the data).
pub fn reconcile(seg_dir: &Path, id: &SegmentIdentity, recovering_epoch: u64) -> Result<Reconciled> {
    let paths = state_paths(seg_dir, id);
    let exists: Vec<SegmentState> = paths
        .iter()
        .filter(|(_, p)| p.exists())
        .map(|(s, _)| *s)
        .collect();
    use SegmentState::*;

    match exists.as_slice() {
        [Sealed] => Ok(Reconciled::Sealed),

        // R + P: the publish landed; the quarantined original is scratch.
        [Recovering, Sealed] => {
            let p = id.path(seg_dir, Sealed);
            SegmentReader::read(&p, true)?.verify_seal()?;
            std::fs::remove_file(id.path(seg_dir, Recovering))?;
            fsutil::fsync_dir(seg_dir)?;
            Ok(Reconciled::Recovered)
        }

        // R + S: staging may be partial — rebuild it from the original.
        [Recovering, RecoveringOut] => {
            std::fs::remove_file(id.path(seg_dir, RecoveringOut))?;
            fsutil::fsync_dir(seg_dir)?;
            recover_from_quarantine(seg_dir, id, recovering_epoch)
        }

        // R alone: resume forward.
        [Recovering] => recover_from_quarantine(seg_dir, id, recovering_epoch),

        // S alone is unreachable by the transaction order (R is durable
        // before S can exist) — loud.
        [RecoveringOut] => Err(Error::State(
            "orphan .recovering-out with no .recovering original".into(),
        )),

        [Open] => reconcile_open(seg_dir, id, recovering_epoch),

        [] => Err(Error::State(format!(
            "segment {} has no file in any state",
            id.file_stem()
        ))),

        other => Err(Error::State(format!(
            "segment {} in invalid state combination {:?}",
            id.file_stem(),
            other
        ))),
    }
}

fn reconcile_open(seg_dir: &Path, id: &SegmentIdentity, recovering_epoch: u64) -> Result<Reconciled> {
    let open_path = id.path(seg_dir, SegmentState::Open);

    // Headerless / header-partial .open takes precedence over everything
    // (r4-8): nothing could have been acked before the header barrier, so
    // reinitializing is safe.
    let bytes = std::fs::read(&open_path)?;
    let headerless = match crate::record::decode_at(&bytes, 0) {
        Ok(None) => true,                          // empty file
        Err(Error::TornTail { .. }) => true,       // partial header record
        Ok(Some(rec)) => rec.kind != crate::record::RecordKind::Header,
        Err(_) => false, // complete-but-corrupt first record: falls through to loud below
    };
    if headerless {
        std::fs::remove_file(&open_path)?;
        fsutil::fsync_dir(seg_dir)?;
        return Ok(Reconciled::ReinitializedOpen);
    }

    // Full structural read; unsealed expectations (tears permitted at tail).
    let reader = SegmentReader::read(&open_path, false)?; // loud conditions propagate here
    if reader.seal_at_eof() {
        // Crash between seal-fsync and rename: publish as-is.
        reader.verify_seal()?;
        let to = id.path(seg_dir, SegmentState::Sealed);
        fsutil::rename_noreplace(&open_path, &to)?;
        fsutil::fsync_dir(seg_dir)?;
        return Ok(Reconciled::PublishedAsIs);
    }
    if reader.tail_tear.is_none() {
        return Ok(Reconciled::StillOpen);
    }

    // Provable tear: quarantine, then rebuild.
    fsutil::rename_noreplace(&open_path, &id.path(seg_dir, SegmentState::Recovering))?;
    fsutil::fsync_dir(seg_dir)?;
    recover_from_quarantine(seg_dir, id, recovering_epoch)
}

/// Build `.recovering-out` from the quarantined original's valid prefix,
/// seal it with recovery metadata, publish, verify, retire the original.
fn recover_from_quarantine(
    seg_dir: &Path,
    id: &SegmentIdentity,
    recovering_epoch: u64,
) -> Result<Reconciled> {
    let r_path = id.path(seg_dir, SegmentState::Recovering);
    let reader = SegmentReader::read(&r_path, false)?;
    if reader.seal.is_some() {
        // A sealed quarantine means the tear was after the seal — but the
        // reader already made post-seal records loud, so the only way here
        // is a seal at EOF: publish it.
        reader.verify_seal()?;
        fsutil::rename_noreplace(&r_path, &id.path(seg_dir, SegmentState::Sealed))?;
        fsutil::fsync_dir(seg_dir)?;
        return Ok(Reconciled::PublishedAsIs);
    }
    let truncated = reader
        .tail_tear
        .map(|(_, dropped)| dropped)
        .unwrap_or(0);

    // Rebuild: rewrite header + valid frames through a fresh writer into the
    // staging name, then seal with recovery metadata. (Byte-identical prefix
    // copy + seal-append would also satisfy the format; the writer path
    // reuses one implementation and revalidates every frame on the way.)
    let staging = id.path(seg_dir, SegmentState::RecoveringOut);
    if staging.exists() {
        std::fs::remove_file(&staging)?;
        fsutil::fsync_dir(seg_dir)?;
    }
    let mut w = SegmentWriter::create_at_state(
        seg_dir,
        reader.header.clone(),
        SegmentState::RecoveringOut,
    )?;
    for env in &reader.frames {
        w.append(env, crate::segment::Commit::Buffered)?;
    }
    w.commit()?;
    w.seal_from_state(
        SegmentState::RecoveringOut,
        Some(RecoveryMeta {
            truncated_bytes: truncated,
            reason: "torn tail".into(),
            by_epoch: recovering_epoch,
        }),
    )?;
    // Publish happened inside seal (rename staging -> .sotseg). Verify, then
    // retire the original (transaction scratch — every retained record lives
    // in the published file, the recovery seal is the audit).
    let published = id.path(seg_dir, SegmentState::Sealed);
    SegmentReader::read(&published, true)?.verify_seal()?;
    std::fs::remove_file(&r_path)?;
    fsutil::fsync_dir(seg_dir)?;
    Ok(Reconciled::Recovered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Seq;
    use crate::segment::{Commit, HeaderBody, RetentionClass, SegmentWriter};

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
            w.append(&crate::segment::tests::test_env(1, n), Commit::Immediate)
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
        let hdr = crate::record::decode_at(&bytes, 0).unwrap().unwrap();
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
        // Running reconciliation AGAIN on the final state is a no-op Sealed.
        assert_eq!(reconcile(dir.path(), &id(0, 1), 5).unwrap(), Reconciled::Sealed);
    }
}
