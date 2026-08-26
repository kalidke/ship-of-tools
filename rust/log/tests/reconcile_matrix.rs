#![cfg(target_os = "linux")]
//! ADR 0039 §"Segment lifecycle" step 4 — reconciliation-table conformance
//! suite. Enters EVERY row of the startup-reconciliation table by explicit
//! file surgery on freshly built segment files, then asserts the exact
//! `Reconciled` outcome (or `Err`). Where `recovery.rs`'s own unit tests
//! already exercise a row, it is still reimplemented here from scratch via
//! only the crate's public API + `std::fs` — this file is the ADR-table
//! conformance suite and must stand alone.
//!
//! Row list, in the ADR's precedence order:
//!   1. headerless `.open`                              -> ReinitializedOpen
//!   2. header-partial `.open` (header torn mid-body)    -> ReinitializedOpen
//!   3. `.open` with a valid seal at EOF                 -> PublishedAsIs
//!   4. `.open` with a provable torn tail                -> Recovered
//!   5. `.open` with a loud condition                    -> Err, untouched
//!   6. `.sotseg` alone                                  -> Sealed
//!   7. `.recovering` + `.sotseg`                        -> Recovered (verify + unlink scratch)
//!   8. `.recovering` + `.recovering-out`                -> Recovered (staging deleted, rebuilt)
//!   9. `.recovering` alone                               -> Recovered (resume)
//!  10. `.recovering-out` alone                           -> Err (invalid)
//!  11. every impossible state combination                -> Err
//!
//! After every non-Err outcome, `verify_voyage` must be green.

use sot_log::record;
use sot_log::recovery::{reconcile, Reconciled};
use sot_log::segment::{Commit, HeaderBody, RetentionClass, SegmentReader, SegmentWriter};
use sot_log::verify::verify_voyage;
use sot_log::{Actor, ActorKind, Class, Derivation, Emitter, Envelope, Seq, Source};
use sot_log::{SegmentIdentity, SegmentState};
use std::path::{Path, PathBuf};

const VOYAGE: &str = "conformance-voyage";

/// A minimal frame `verify_voyage` accepts standalone — no `attached_to`,
/// no capture-optin gating, nothing that needs sibling frames.
fn frame(epoch: u64, n: u64) -> Envelope {
    Envelope {
        seq: Seq { epoch, n },
        class: Class::Lifecycle,
        source: Source {
            emitter: Emitter::Capsule,
            actor: Actor {
                kind: ActorKind::Unknown,
                controller_id: None,
                take_epoch: None,
            },
            derivation: Derivation::Synthetic,
        },
        t_wall_ms: 1_756_000_000_000,
        t_mono_us: n * 1_000,
        stream: None,
        transformed: None,
        refs: vec![],
        payload: Some(serde_json::json!({"kind": "producer_ready"})),
        payload_ref: None,
    }
}

fn genesis_header(epoch: u64) -> HeaderBody {
    HeaderBody {
        version: 1,
        required_features: vec![],
        voyage_id: VOYAGE.into(),
        segment_index: 0,
        epoch,
        prev_seal_digest: None,
        created_wall_ms: 1_756_000_000_000,
        retention_class: Some(RetentionClass::Discard),
    }
}

fn id(epoch: u64) -> SegmentIdentity {
    SegmentIdentity {
        voyage_id: VOYAGE.into(),
        segment_index: 0,
        epoch,
    }
}

/// A fresh voyage root with an empty `seg/` dir. `verify_voyage` only reads
/// `seg/`, so no `blobs/`/`writer.lock` scaffolding is needed here.
fn voyage_root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(VOYAGE);
    std::fs::create_dir_all(root.join("seg")).unwrap();
    (dir, root)
}

fn seg_dir(root: &Path) -> PathBuf {
    root.join("seg")
}

/// A live `.open` segment: durable header + `n_frames` durable frames.
fn write_open(sd: &Path, epoch: u64, n_frames: u64) -> SegmentWriter {
    let mut w = SegmentWriter::create(sd, genesis_header(epoch)).unwrap();
    for n in 1..=n_frames {
        w.append(&frame(epoch, n), Commit::Immediate).unwrap();
    }
    w
}

fn assert_green(root: &Path) {
    verify_voyage(root, VOYAGE).expect("verify_voyage must be green after a non-Err outcome");
}

// --- Row 6: `.sotseg` alone -------------------------------------------------

#[test]
fn sotseg_alone_is_sealed() {
    let (_dir, root) = voyage_root();
    let sd = seg_dir(&root);
    let w = write_open(&sd, 1, 2);
    w.seal(None).unwrap();
    assert_eq!(reconcile(&sd, &id(1), 5).unwrap(), Reconciled::Sealed);
    assert_green(&root);
}

// --- Row 7: `.recovering` + `.sotseg` --------------------------------------

#[test]
fn recovering_plus_sotseg_verifies_and_unlinks_scratch() {
    let (_dir, root) = voyage_root();
    let sd = seg_dir(&root);
    let w = write_open(&sd, 1, 1);
    w.seal(None).unwrap();
    // Crash between "sealed published" and "quarantine unlinked": drop
    // arbitrary scratch bytes at the `.recovering` path — the reconciler
    // trusts the published `.sotseg`, not the leftover quarantine's content.
    std::fs::write(
        id(1).path(&sd, SegmentState::Recovering),
        b"stale quarantine scratch",
    )
    .unwrap();
    assert_eq!(reconcile(&sd, &id(1), 9).unwrap(), Reconciled::Recovered);
    assert!(!id(1).path(&sd, SegmentState::Recovering).exists());
    assert!(id(1).path(&sd, SegmentState::Sealed).exists());
    assert_green(&root);
}

// --- Row 8: `.recovering` + `.recovering-out` ------------------------------

#[test]
fn recovering_plus_recovering_out_rebuilds_from_original() {
    let (_dir, root) = voyage_root();
    let sd = seg_dir(&root);
    let w = write_open(&sd, 1, 3);
    drop(w);
    let open_path = id(1).path(&sd, SegmentState::Open);
    // Tear the tail, as if an earlier crash caught the writer mid-append...
    let bytes = std::fs::read(&open_path).unwrap();
    std::fs::write(&open_path, &bytes[..bytes.len() - 4]).unwrap();
    // ...then quarantine it as a real `.recovering` would be, but leave a
    // half-built `.recovering-out` from a SECOND, also-interrupted attempt.
    std::fs::rename(&open_path, id(1).path(&sd, SegmentState::Recovering)).unwrap();
    std::fs::write(
        id(1).path(&sd, SegmentState::RecoveringOut),
        b"half-built garbage from an interrupted rebuild",
    )
    .unwrap();
    assert_eq!(reconcile(&sd, &id(1), 11).unwrap(), Reconciled::Recovered);
    assert!(!id(1).path(&sd, SegmentState::Recovering).exists());
    assert!(!id(1).path(&sd, SegmentState::RecoveringOut).exists());
    let r = SegmentReader::read(&id(1).path(&sd, SegmentState::Sealed), true).unwrap();
    r.verify_seal().unwrap();
    assert_eq!(r.frames.len(), 2); // the third frame was the torn one
    assert_green(&root);
}

// --- Row 9: `.recovering` alone --------------------------------------------

#[test]
fn recovering_alone_resumes_recovery() {
    let (_dir, root) = voyage_root();
    let sd = seg_dir(&root);
    let w = write_open(&sd, 1, 2);
    drop(w); // clean .open, no tear — e.g. a survivor quarantined for resealing
    let open_path = id(1).path(&sd, SegmentState::Open);
    std::fs::rename(&open_path, id(1).path(&sd, SegmentState::Recovering)).unwrap();
    assert_eq!(reconcile(&sd, &id(1), 3).unwrap(), Reconciled::Recovered);
    let r = SegmentReader::read(&id(1).path(&sd, SegmentState::Sealed), true).unwrap();
    r.verify_seal().unwrap();
    assert_eq!(r.frames.len(), 2);
    // Clean quarantine (no tear) gets a PLAIN seal — recovery metadata
    // appears iff bytes were discarded (post-review ADR semantics; a false
    // "torn tail" here would be a permanent lie in read-forever bytes).
    assert_eq!(r.seal.as_ref().unwrap().truncated_bytes, None);
    assert_eq!(r.seal.as_ref().unwrap().recovered, None);
    assert_green(&root);
}

// --- Row 10: `.recovering-out` alone ---------------------------------------

#[test]
fn recovering_out_alone_is_invalid() {
    let (_dir, root) = voyage_root();
    let sd = seg_dir(&root);
    std::fs::write(id(1).path(&sd, SegmentState::RecoveringOut), b"orphan").unwrap();
    assert!(reconcile(&sd, &id(1), 2).is_err());
}

// --- Row 1: headerless `.open` ----------------------------------------------

#[test]
fn open_headerless_reinitializes() {
    let (_dir, root) = voyage_root();
    let sd = seg_dir(&root);
    std::fs::write(id(1).path(&sd, SegmentState::Open), b"").unwrap();
    assert_eq!(
        reconcile(&sd, &id(1), 2).unwrap(),
        Reconciled::ReinitializedOpen
    );
    assert!(!id(1).path(&sd, SegmentState::Open).exists());
    assert_green(&root); // empty seg/ dir verifies trivially
}

// --- Row 2: header-partial `.open` (header record torn mid-body) ----------

#[test]
fn open_header_partial_reinitializes() {
    let (_dir, root) = voyage_root();
    let sd = seg_dir(&root);
    let w = SegmentWriter::create(&sd, genesis_header(1)).unwrap();
    drop(w); // durable header record, zero frames
    let open_path = id(1).path(&sd, SegmentState::Open);
    let bytes = std::fs::read(&open_path).unwrap();
    assert!(
        bytes.len() > sot_log::PRELUDE_LEN + 4,
        "header record too small to tear mid-body"
    );
    // Valid 18-byte prelude present, but fewer than `len` body bytes follow —
    // a provable tear of the HEADER record itself, not an empty file.
    std::fs::write(&open_path, &bytes[..sot_log::PRELUDE_LEN + 4]).unwrap();
    assert_eq!(
        reconcile(&sd, &id(1), 2).unwrap(),
        Reconciled::ReinitializedOpen
    );
    assert!(!open_path.exists());
    assert_green(&root);
}

// --- Row 3: `.open` with a valid seal at EOF -------------------------------

#[test]
fn open_with_valid_seal_at_eof_publishes_as_is() {
    let (_dir, root) = voyage_root();
    let sd = seg_dir(&root);
    let w = write_open(&sd, 1, 1);
    w.seal(None).unwrap();
    let sealed = id(1).path(&sd, SegmentState::Sealed);
    let open_path = id(1).path(&sd, SegmentState::Open);
    // Crash between "seal fsynced" and "RENAME_NOREPLACE to .sotseg".
    std::fs::rename(&sealed, &open_path).unwrap();
    assert_eq!(
        reconcile(&sd, &id(1), 2).unwrap(),
        Reconciled::PublishedAsIs
    );
    assert!(sealed.exists());
    assert!(!open_path.exists());
    assert_green(&root);
}

// --- Row 4: `.open` with a provable torn tail ------------------------------

#[test]
fn open_with_provable_torn_tail_recovers() {
    let (_dir, root) = voyage_root();
    let sd = seg_dir(&root);
    let w = write_open(&sd, 1, 3);
    drop(w);
    let open_path = id(1).path(&sd, SegmentState::Open);
    let bytes = std::fs::read(&open_path).unwrap();
    std::fs::write(&open_path, &bytes[..bytes.len() - 6]).unwrap();
    assert_eq!(reconcile(&sd, &id(1), 4).unwrap(), Reconciled::Recovered);
    let r = SegmentReader::read(&id(1).path(&sd, SegmentState::Sealed), true).unwrap();
    r.verify_seal().unwrap();
    assert_eq!(r.frames.len(), 2);
    assert!(r.seal.as_ref().unwrap().truncated_bytes.unwrap() > 0);
    assert_green(&root);
}

// --- Row 5: `.open` with a loud condition ----------------------------------

#[test]
fn open_with_loud_midfile_corruption_halts_untouched() {
    let (_dir, root) = voyage_root();
    let sd = seg_dir(&root);
    let w = write_open(&sd, 1, 3);
    drop(w);
    let open_path = id(1).path(&sd, SegmentState::Open);
    let mut bytes = std::fs::read(&open_path).unwrap();
    // A COMPLETE record with a bad body CRC, inside the first frame — not
    // the final record, so it can never be mistaken for a tear.
    let hdr = record::decode_at(&bytes, 0).unwrap().unwrap();
    bytes[hdr.wire_len + 20] ^= 0xFF;
    std::fs::write(&open_path, &bytes).unwrap();
    let result = reconcile(&sd, &id(1), 2);
    assert!(
        result.is_err(),
        "mid-file corruption must be loud, not recovered"
    );
    assert_eq!(
        std::fs::read(&open_path).unwrap(),
        bytes,
        "a loud condition must never touch the file"
    );
}

// --- Row 11: every impossible state combination ----------------------------

#[test]
fn every_impossible_state_combination_is_loud() {
    // None of these arise from the real state machine — every real
    // transition renames the source OUT before the destination exists — so
    // only file surgery can construct them.
    let combos: &[&[SegmentState]] = &[
        &[SegmentState::Open, SegmentState::Recovering],
        &[SegmentState::Open, SegmentState::Sealed],
        &[SegmentState::Open, SegmentState::RecoveringOut],
        &[SegmentState::RecoveringOut, SegmentState::Sealed],
        &[
            SegmentState::Open,
            SegmentState::Recovering,
            SegmentState::Sealed,
        ],
        &[
            SegmentState::Open,
            SegmentState::Recovering,
            SegmentState::RecoveringOut,
            SegmentState::Sealed,
        ],
    ];
    for states in combos {
        let (_dir, root) = voyage_root();
        let sd = seg_dir(&root);
        for state in *states {
            std::fs::write(id(1).path(&sd, *state), b"surgically implanted").unwrap();
        }
        assert!(
            reconcile(&sd, &id(1), 2).is_err(),
            "combination {states:?} must be loud"
        );
    }
}

/// Bonus: not a named ADR-table row (an identity with nothing on disk isn't
/// a "combination"), but it's the match's final catch-all and costs nothing
/// to pin down.
#[test]
fn no_files_for_identity_is_loud() {
    let (_dir, root) = voyage_root();
    let sd = seg_dir(&root);
    assert!(reconcile(&sd, &id(1), 2).is_err());
}
