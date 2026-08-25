//! Startup reconciliation + tear recovery (ADR 0039 §Segment lifecycle 4).
//!
//! Recovery applies ONLY to provable tears; every loud condition halts. The
//! transaction: `.open` → `.recovering` (quarantined original) → build
//! `.recovering-out` (valid prefix + recovery seal) → publish `.sotseg`
//! (RENAME_NOREPLACE) → verify → unlink the original. Idempotent at every
//! crash point; states are distinguished by filename.

use crate::fsutil;
use crate::segment::{RecoveryMeta, SegmentIdentity, SegmentReader, SegmentState};
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

/// Build `.recovering-out` from the quarantined original's valid prefix —
/// **byte-verbatim** — seal it, publish, verify, retire the original.
///
/// The prefix is copied as WIRE BYTES, never decoded-and-re-encoded:
/// re-serialization would re-sort JSON object keys (changing committed
/// frames' bytes), violate the ADR's digests-over-wire-bytes rule, and —
/// sharpest — silently strip unknown ignorable members, which are the
/// format's forward-compat mechanism. Recovery must never alter retained
/// content (review finding on the first cut, which rebuilt via re-append).
///
/// Recovery metadata appears in the seal IFF bytes were actually discarded;
/// a clean quarantined tip (successor sealing a predecessor's survivor) gets
/// a plain seal — its content is exactly what the original writer wrote, and
/// stamping "torn tail" on it would be a permanent falsehood.
fn recover_from_quarantine(
    seg_dir: &Path,
    id: &SegmentIdentity,
    recovering_epoch: u64,
) -> Result<Reconciled> {
    use sha2::Digest as _;
    let r_path = id.path(seg_dir, SegmentState::Recovering);
    let reader = SegmentReader::read(&r_path, false)?;
    if reader.seal.is_some() {
        // Post-seal records are loud in the reader, so a sealed quarantine
        // can only be a seal at EOF: publish it as-is.
        reader.verify_seal()?;
        fsutil::rename_noreplace(&r_path, &id.path(seg_dir, SegmentState::Sealed))?;
        fsutil::fsync_dir(seg_dir)?;
        return Ok(Reconciled::PublishedAsIs);
    }
    let recovery = reader.tail_tear.map(|(_, dropped)| RecoveryMeta {
        truncated_bytes: dropped,
        reason: "torn tail".into(),
        by_epoch: recovering_epoch,
    });
    let prefix = &reader.raw_bytes()[..reader.valid_prefix_len() as usize];

    let staging = id.path(seg_dir, SegmentState::RecoveringOut);
    if staging.exists() {
        std::fs::remove_file(&staging)?;
        fsutil::fsync_dir(seg_dir)?;
    }
    let mut hasher = sha2::Sha256::new();
    hasher.update(crate::segment::SEAL_DOMAIN);
    hasher.update(prefix);
    let meta = crate::segment::SealMeta {
        frame_count: reader.frames.len() as u64,
        first_seq: reader.frames.first().map(|f| f.seq),
        last_seq: reader.frames.last().map(|f| f.seq),
        recovery,
    };
    let (seal_wire, _digest) = crate::segment::build_seal_record(hasher, &meta)?;
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)?;
        f.write_all(prefix)?;
        f.write_all(&seal_wire)?;
        f.sync_all()?;
    }
    fsutil::rename_noreplace(&staging, &id.path(seg_dir, SegmentState::Sealed))?;
    fsutil::fsync_dir(seg_dir)?;
    // Verify the published result, then retire the original (transaction
    // scratch — every retained byte lives verbatim in the published file,
    // the recovery seal is the audit).
    let published = id.path(seg_dir, SegmentState::Sealed);
    SegmentReader::read(&published, true)?.verify_seal()?;
    std::fs::remove_file(&r_path)?;
    fsutil::fsync_dir(seg_dir)?;
    Ok(Reconciled::Recovered)
}

// The STORE (not the codec) is Linux-only in v1: publication needs an
// atomic no-clobber rename, and `rename_noreplace` fails closed off
// Linux (ADR 0039). These tests therefore run where the store runs;
// Windows joins with P3, macOS when it gets a renamex_np arm. The
// pure-codec tests in record.rs/envelope.rs stay on every platform.
#[cfg(all(test, target_os = "linux"))]
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
        let frame_wire = crate::record::encode(crate::record::RecordKind::Frame, body, None).unwrap();
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
        // Running reconciliation AGAIN on the final state is a no-op Sealed.
        assert_eq!(reconcile(dir.path(), &id(0, 1), 5).unwrap(), Reconciled::Sealed);
    }
}
