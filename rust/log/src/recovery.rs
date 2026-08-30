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
    /// Published `.sotseg` present — nothing NEW to build, but its
    /// publication barrier is restated on every call (Part 3 finding).
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
        [Sealed] => {
            // Restate the publication barrier: a `.sotseg` found already in
            // place was published by SOME incarnation, not necessarily one
            // whose renamed-file/parent flush completed before it crashed —
            // finding it here is exactly the case that needs restating, not
            // skipping (Part 3 finding: an after-loop, once-only flush
            // cannot stand in for this, and reconciliation runs it per
            // segment, which costs nothing — `open_for_writing` already
            // reads and verifies every sealed segment).
            fsutil::finish_publication(&id.path(seg_dir, Sealed))?;
            Ok(Reconciled::Sealed)
        }

        // R + P: the publish landed; the quarantined original is scratch.
        [Recovering, Sealed] => {
            let p = id.path(seg_dir, Sealed);
            SegmentReader::read(&p, true)?.verify_seal()?;
            // Complete the barrier BEFORE retiring the quarantine copy:
            // `.recovering` is our only fallback if `.sotseg`'s publication
            // never finished flushing before a prior incarnation crashed,
            // so it must survive until AFTER we've restated that flush
            // ourselves, never before.
            fsutil::finish_publication(&p)?;
            std::fs::remove_file(id.path(seg_dir, Recovering))?;
            fsutil::fsync_dir(seg_dir)?;
            Ok(Reconciled::Recovered)
        }

        // R + S: staging may be partial — rebuild it from the original.
        // Deleting it here, before `recover_from_quarantine`, would mutate
        // BEFORE that function's own `.recovering` barrier restatement runs
        // (a failure there would then return after destructive progress).
        // `.recovering-out` is disposable staging either way — the same
        // function already removes a pre-existing one itself, AFTER the
        // barrier — so there is nothing left for this arm to do but share
        // the R-alone path.
        //
        // R alone: resume forward.
        [Recovering, RecoveringOut] | [Recovering] => {
            recover_from_quarantine(seg_dir, id, recovering_epoch)
        }

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
        // Seal at EOF: publish as-is. The dead writer may have been killed
        // BETWEEN write_all(seal) and its fsync — a cache-visible seal is
        // indistinguishable from a durable one, so the publisher restates
        // the source flush before the rename (pinned publication order;
        // P3 round-1 review blocker, reachable on Linux too).
        reader.verify_seal()?;
        fsutil::fsync_file(&open_path)?;
        let to = id.path(seg_dir, SegmentState::Sealed);
        fsutil::publish_noreplace(&open_path, &to)?;
        return Ok(Reconciled::PublishedAsIs);
    }
    if reader.tail_tear.is_none() {
        return Ok(Reconciled::StillOpen);
    }

    // Provable tear: quarantine, then rebuild.
    fsutil::publish_noreplace(&open_path, &id.path(seg_dir, SegmentState::Recovering))?;
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
    // `.recovering` can itself be crash residue: the rename that quarantined
    // it — by this very call a few lines up in `reconcile_open`/`reconcile`,
    // or by a prior incarnation that crashed before restating ITS barrier —
    // is a publication like any other. This function is reached both right
    // after a fresh quarantine and on a `.recovering` resumed from a prior
    // crash (the `[Recovering]` and `[Recovering, RecoveringOut]` rows), so
    // restating unconditionally here covers both; the fresh case is simply
    // a harmless repeat (Part 3 finding — before building anything ON TOP
    // of this file, its own publication must be durable, not just visible).
    fsutil::finish_publication(&r_path)?;
    let reader = SegmentReader::read(&r_path, false)?;
    if reader.seal.is_some() {
        // Post-seal records are loud in the reader, so a sealed quarantine
        // can only be a seal at EOF: publish it as-is — with the source
        // flush restated first (same cache-visible-seal hazard as
        // reconcile_open's publish-as-is row).
        reader.verify_seal()?;
        fsutil::fsync_file(&r_path)?;
        fsutil::publish_noreplace(&r_path, &id.path(seg_dir, SegmentState::Sealed))?;
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
    fsutil::publish_noreplace(&staging, &id.path(seg_dir, SegmentState::Sealed))?;
    // Verify the published result, then retire the original (transaction
    // scratch — every retained byte lives verbatim in the published file,
    // the recovery seal is the audit).
    let published = id.path(seg_dir, SegmentState::Sealed);
    SegmentReader::read(&published, true)?.verify_seal()?;
    std::fs::remove_file(&r_path)?;
    fsutil::fsync_dir(seg_dir)?;
    Ok(Reconciled::Recovered)
}

/// U2's own read-only need (ADR 0041 Lifecycle "Respawn is gated by the
/// typed marker, read from the LATEST LEG AFTER RECONCILIATION"): without
/// ever taking the writer fence (a live capsule may hold it), what is the
/// aggregate state of the highest-epoch leg present under `seg_dir`? A
/// leg can span several segment INDICES at the same epoch (rotation
/// within one leg); it is `Unsealed` if ANY of them still carries an
/// `.open`/`.recovering`/`.recovering-out` file at that epoch — "an
/// unfinished leg is never a requested end" governs regardless of what a
/// sealed sibling in the SAME epoch says — and `Sealed` only when every
/// file at that epoch is `.sotseg`.
///
/// Performs NO reconciliation: [`reconcile`] mutates and must run under
/// the writer lock, which the supervisor does not hold merely to decide
/// a start mode. This is the supervisor's own classification of what it
/// SEES; a `--resume`/`--start` that goes on to spawn a fresh leg lets
/// that child's own `VoyageStore::open_for_writing` perform the real,
/// authoritative reconciliation the ADR's table already accounts for
/// ("open or recovering, no live capsule -> RECOVER and spawn a new
/// leg").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatestLegState {
    /// `seg_dir` does not exist, or names no segment identity at all.
    NoLeg,
    /// Every file at the highest epoch present is `.sotseg`.
    Sealed { epoch: u64 },
    /// At least one file at the highest epoch present is `.open`,
    /// `.recovering`, or `.recovering-out`.
    Unsealed { epoch: u64 },
}

pub fn latest_leg_state(seg_dir: &Path) -> std::io::Result<LatestLegState> {
    let mut max_epoch: Option<u64> = None;
    let mut sealed_at_max = false;
    let mut unsealed_at_max = false;
    let entries = match std::fs::read_dir(seg_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(LatestLegState::NoLeg),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some((_, epoch, state)) = SegmentIdentity::parse_file_name(name) else { continue };
        match max_epoch {
            Some(m) if epoch < m => continue,
            Some(m) if epoch == m => {}
            _ => {
                // A strictly higher epoch supersedes every prior
                // max-epoch observation.
                max_epoch = Some(epoch);
                sealed_at_max = false;
                unsealed_at_max = false;
            }
        }
        match state {
            SegmentState::Sealed => sealed_at_max = true,
            SegmentState::Open | SegmentState::Recovering | SegmentState::RecoveringOut => {
                unsealed_at_max = true;
            }
        }
    }
    Ok(match max_epoch {
        None => LatestLegState::NoLeg,
        Some(epoch) if unsealed_at_max => LatestLegState::Unsealed { epoch },
        Some(epoch) => {
            debug_assert!(sealed_at_max, "an epoch with no unsealed file must have a sealed one");
            LatestLegState::Sealed { epoch }
        }
    })
}

#[cfg(test)]
mod latest_leg_state_tests {
    use super::*;

    fn seg_dir_for(id: &SegmentIdentity, state: SegmentState, seg_dir: &Path) -> PathBuf {
        std::fs::create_dir_all(seg_dir).unwrap();
        id.path(seg_dir, state)
    }

    #[test]
    fn no_seg_dir_at_all_is_no_leg() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            latest_leg_state(&dir.path().join("seg")).unwrap(),
            LatestLegState::NoLeg
        );
    }

    #[test]
    fn empty_seg_dir_is_no_leg() {
        let dir = tempfile::tempdir().unwrap();
        let seg_dir = dir.path().join("seg");
        std::fs::create_dir_all(&seg_dir).unwrap();
        assert_eq!(latest_leg_state(&seg_dir).unwrap(), LatestLegState::NoLeg);
    }

    #[test]
    fn a_lone_sealed_segment_is_sealed_at_its_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let seg_dir = dir.path().join("seg");
        let id = SegmentIdentity { voyage_id: "v".into(), segment_index: 0, epoch: 3 };
        std::fs::write(seg_dir_for(&id, SegmentState::Sealed, &seg_dir), b"x").unwrap();
        assert_eq!(latest_leg_state(&seg_dir).unwrap(), LatestLegState::Sealed { epoch: 3 });
    }

    #[test]
    fn a_lone_open_segment_is_unsealed_at_its_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let seg_dir = dir.path().join("seg");
        let id = SegmentIdentity { voyage_id: "v".into(), segment_index: 0, epoch: 3 };
        std::fs::write(seg_dir_for(&id, SegmentState::Open, &seg_dir), b"x").unwrap();
        assert_eq!(latest_leg_state(&seg_dir).unwrap(), LatestLegState::Unsealed { epoch: 3 });
    }

    #[test]
    fn a_recovering_scratch_file_alone_is_unsealed() {
        let dir = tempfile::tempdir().unwrap();
        let seg_dir = dir.path().join("seg");
        let id = SegmentIdentity { voyage_id: "v".into(), segment_index: 0, epoch: 3 };
        std::fs::write(seg_dir_for(&id, SegmentState::Recovering, &seg_dir), b"x").unwrap();
        assert_eq!(latest_leg_state(&seg_dir).unwrap(), LatestLegState::Unsealed { epoch: 3 });
    }

    #[test]
    fn a_sealed_predecessor_plus_an_open_tip_in_the_same_epoch_is_unsealed() {
        let dir = tempfile::tempdir().unwrap();
        let seg_dir = dir.path().join("seg");
        let sealed = SegmentIdentity { voyage_id: "v".into(), segment_index: 0, epoch: 5 };
        let open = SegmentIdentity { voyage_id: "v".into(), segment_index: 1, epoch: 5 };
        std::fs::write(seg_dir_for(&sealed, SegmentState::Sealed, &seg_dir), b"x").unwrap();
        std::fs::write(seg_dir_for(&open, SegmentState::Open, &seg_dir), b"x").unwrap();
        assert_eq!(latest_leg_state(&seg_dir).unwrap(), LatestLegState::Unsealed { epoch: 5 });
    }

    #[test]
    fn only_the_highest_epoch_is_considered() {
        let dir = tempfile::tempdir().unwrap();
        let seg_dir = dir.path().join("seg");
        let old_open = SegmentIdentity { voyage_id: "v".into(), segment_index: 0, epoch: 1 };
        let new_sealed = SegmentIdentity { voyage_id: "v".into(), segment_index: 0, epoch: 2 };
        // An OLDER epoch left unsealed (impossible in practice once a
        // newer epoch exists, but the classification must still ignore
        // it rather than let a stale leg's shape leak into "latest").
        std::fs::write(seg_dir_for(&old_open, SegmentState::Open, &seg_dir), b"x").unwrap();
        std::fs::write(seg_dir_for(&new_sealed, SegmentState::Sealed, &seg_dir), b"x").unwrap();
        assert_eq!(latest_leg_state(&seg_dir).unwrap(), LatestLegState::Sealed { epoch: 2 });
    }

    #[test]
    fn non_segment_files_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let seg_dir = dir.path().join("seg");
        std::fs::create_dir_all(&seg_dir).unwrap();
        std::fs::write(seg_dir.join("writer.lock"), b"{}").unwrap();
        assert_eq!(latest_leg_state(&seg_dir).unwrap(), LatestLegState::NoLeg);
    }
}

// The STORE (not the codec) is Linux-only AS OF THIS COMMIT: publication
// needs an atomic no-clobber rename, and `rename_noreplace_raw` fails closed
// off Linux (ADR 0039). These tests therefore run where the store runs;
// the pure-codec tests in record.rs/envelope.rs stay on every platform.
//
// PROVISIONAL, deliberately: the P3 store port (ADR 0041, PR #122) adds
// the real Windows arms and widens these gates to
// `any(target_os = "linux", windows)`. Read this as "where the store
// works today", never as a settled contract — macOS joins when it gets a
// renamex_np arm, which ADR 0039 already anticipates.
#[cfg(all(test, any(target_os = "linux", windows)))]
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
}
