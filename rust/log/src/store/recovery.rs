//! Startup reconciliation + tear recovery (ADR 0039 §Segment lifecycle 4).
//!
//! Recovery applies ONLY to provable tears; every loud condition halts. The
//! transaction: `.open` → `.recovering` (quarantined original) → build
//! `.recovering-out` (valid prefix + recovery seal) → publish `.sotseg`
//! (RENAME_NOREPLACE) → verify → unlink the original. Idempotent at every
//! crash point; states are distinguished by filename.

use crate::host;
use crate::store::segment::{RecoveryMeta, SegmentIdentity, SegmentReader, SegmentState};
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
            host::finish_publication(&id.path(seg_dir, Sealed))?;
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
            host::finish_publication(&p)?;
            std::fs::remove_file(id.path(seg_dir, Recovering))?;
            host::fsync_dir(seg_dir)?;
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
    let headerless = match crate::store::record::decode_at(&bytes, 0) {
        Ok(None) => true,                          // empty file
        Err(Error::TornTail { .. }) => true,       // partial header record
        Ok(Some(rec)) => rec.kind != crate::store::record::RecordKind::Header,
        Err(_) => false, // complete-but-corrupt first record: falls through to loud below
    };
    if headerless {
        std::fs::remove_file(&open_path)?;
        host::fsync_dir(seg_dir)?;
        return Ok(Reconciled::ReinitializedOpen);
    }

    // Full structural read; unsealed host-3 (tears permitted at tail).
    let reader = SegmentReader::read(&open_path, false)?; // loud conditions propagate here
    if reader.seal_at_eof() {
        // Seal at EOF: publish as-is. The dead writer may have been killed
        // BETWEEN write_all(seal) and its fsync — a cache-visible seal is
        // indistinguishable from a durable one, so the publisher restates
        // the source flush before the rename (pinned publication order;
        // P3 round-1 review blocker, reachable on Linux too).
        reader.verify_seal()?;
        host::fsync_file(&open_path)?;
        let to = id.path(seg_dir, SegmentState::Sealed);
        host::publish_noreplace(&open_path, &to)?;
        return Ok(Reconciled::PublishedAsIs);
    }
    if reader.tail_tear.is_none() {
        return Ok(Reconciled::StillOpen);
    }

    // Provable tear: quarantine, then rebuild.
    host::publish_noreplace(&open_path, &id.path(seg_dir, SegmentState::Recovering))?;
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
    host::finish_publication(&r_path)?;
    let reader = SegmentReader::read(&r_path, false)?;
    if reader.seal.is_some() {
        // Post-seal records are loud in the reader, so a sealed quarantine
        // can only be a seal at EOF: publish it as-is — with the source
        // flush restated first (same cache-visible-seal hazard as
        // reconcile_open's publish-as-is row).
        reader.verify_seal()?;
        host::fsync_file(&r_path)?;
        host::publish_noreplace(&r_path, &id.path(seg_dir, SegmentState::Sealed))?;
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
        host::fsync_dir(seg_dir)?;
    }
    let mut hasher = sha2::Sha256::new();
    hasher.update(crate::store::segment::SEAL_DOMAIN);
    hasher.update(prefix);
    let meta = crate::store::segment::SealMeta {
        frame_count: reader.frames.len() as u64,
        first_seq: reader.frames.first().map(|f| f.seq),
        last_seq: reader.frames.last().map(|f| f.seq),
        recovery,
    };
    let (seal_wire, _digest) = crate::store::segment::build_seal_record(hasher, &meta)?;
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
    host::publish_noreplace(&staging, &id.path(seg_dir, SegmentState::Sealed))?;
    // Verify the published result, then retire the original (transaction
    // scratch — every retained byte lives verbatim in the published file,
    // the recovery seal is the audit).
    let published = id.path(seg_dir, SegmentState::Sealed);
    SegmentReader::read(&published, true)?.verify_seal()?;
    std::fs::remove_file(&r_path)?;
    host::fsync_dir(seg_dir)?;
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
#[path = "recovery_latest_leg_state_tests.rs"]
mod latest_leg_state_tests;

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
#[path = "recovery_tests.rs"]
mod tests;
