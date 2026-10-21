use super::super::support_tests::store;
use super::*;
use crate::store::envelope::*;
use crate::store::segment::{tests::test_env, Commit};
use serde_json::json;

/// `leg_carries_run_end_marker` — the small typed accessor a later
/// unit's respawn decision reads. Must see the marker on a leg's OWN
/// epoch whether the segment is still `.open` (a hard-killed leg's
/// tail) or `.sotseg`, and must not see it on a different epoch or an
/// ordinary lifecycle frame.
#[test]
fn leg_carries_run_end_marker_reads_open_and_sealed_legs() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "mk1");
    let mut w = s
        .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
        .unwrap();
    let mut e = test_env(1, 1);
    e.class = Class::Lifecycle;
    e.payload = Some(json!({"kind": "run_end_requested", "reason": "quit"}));
    w.append(&e, Commit::Immediate).unwrap();
    let seg_dir = dir.path().join("mk1").join("seg");
    // Still .open (never sealed yet): the marker must already be
    // readable — a hard-killed leg's tail segment stays .open.
    assert!(leg_carries_run_end_marker(&seg_dir, "mk1", 1).unwrap());
    assert!(!leg_carries_run_end_marker(&seg_dir, "mk1", 2).unwrap());
    w.seal(None).unwrap();
    assert!(leg_carries_run_end_marker(&seg_dir, "mk1", 1).unwrap());
}

/// Codex round-1 Major 8: a marker frame present in a segment that
/// does NOT declare the feature must fail LOUD (`Err`), never
/// silently `false` -- an authority-changing frame smuggled past its
/// own registry rule is corrupt, not "no marker".
#[test]
fn leg_carries_run_end_marker_errs_on_undeclared_feature() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "mk3");
    let mut w = s.open_segment(0).unwrap(); // no features declared
    let mut e = test_env(1, 1);
    e.class = Class::Lifecycle;
    e.payload = Some(json!({"kind": "run_end_requested", "reason": "quit"}));
    w.append(&e, Commit::Immediate).unwrap();
    let seg_dir = dir.path().join("mk3").join("seg");
    let err = leg_carries_run_end_marker(&seg_dir, "mk3", 1).unwrap_err();
    assert!(format!("{err}").contains("does not declare"), "got: {err}");
}

/// Codex round-1 Major 8: a malformed reason (missing, or over the
/// str128 bound) on an otherwise feature-declared marker also errs
/// loud rather than silently reporting "no marker".
#[test]
fn leg_carries_run_end_marker_errs_on_malformed_reason() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "mk4");
    let mut w = s
        .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
        .unwrap();
    let mut e = test_env(1, 1);
    e.class = Class::Lifecycle;
    e.payload = Some(json!({"kind": "run_end_requested", "reason": "a".repeat(200)}));
    w.append(&e, Commit::Immediate).unwrap();
    let seg_dir = dir.path().join("mk4").join("seg");
    let err = leg_carries_run_end_marker(&seg_dir, "mk4", 1).unwrap_err();
    assert!(format!("{err}").contains("str128"), "got: {err}");
}

/// Codex round-1 Major 8: two well-formed markers in the SAME epoch
/// (the writer's own first-commit-wins latch is a promise about ITS
/// process lifetime, not a proof a crafted/corrupted voyage can't
/// carry two) must err loud, matching the main verifier's own
/// per-epoch uniqueness rule (Major 7) rather than reporting the
/// FIRST one found and silently ignoring the second.
#[test]
fn leg_carries_run_end_marker_errs_on_duplicate_marker() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "mk5");
    let mut w = s
        .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
        .unwrap();
    let mut e1 = test_env(1, 1);
    e1.class = Class::Lifecycle;
    e1.payload = Some(json!({"kind": "run_end_requested", "reason": "first"}));
    w.append(&e1, Commit::Immediate).unwrap();
    let mut e2 = test_env(1, 2);
    e2.class = Class::Lifecycle;
    e2.payload = Some(json!({"kind": "run_end_requested", "reason": "second"}));
    w.append(&e2, Commit::Immediate).unwrap();
    let seg_dir = dir.path().join("mk5").join("seg");
    let err = leg_carries_run_end_marker(&seg_dir, "mk5", 1).unwrap_err();
    assert!(format!("{err}").contains("carries two"), "got: {err}");
}

/// Codex round-1 Major 8: the FILENAME'S epoch is not trusted on its
/// own — a segment renamed to claim a DIFFERENT epoch than its own
/// header still carries must err loud rather than silently answering
/// against the wrong epoch's identity.
#[test]
fn leg_carries_run_end_marker_errs_on_filename_header_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "mk6");
    let mut w = s
        .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
        .unwrap();
    let mut e = test_env(1, 1);
    e.class = Class::Lifecycle;
    e.payload = Some(json!({"kind": "run_end_requested", "reason": "quit"}));
    w.append(&e, Commit::Immediate).unwrap();
    drop(w);
    let seg_dir = dir.path().join("mk6").join("seg");
    // The real (unsealed) file is named for epoch 1; rename it to
    // CLAIM epoch 2 on disk while its header still says 1.
    let real_name = std::fs::read_dir(&seg_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name();
    let renamed = SegmentIdentity {
        voyage_id: "mk6".to_string(),
        segment_index: 0,
        epoch: 2,
    }
    .path(&seg_dir, SegmentState::Open);
    std::fs::rename(seg_dir.join(&real_name), &renamed).unwrap();
    let err = leg_carries_run_end_marker(&seg_dir, "mk6", 2).unwrap_err();
    assert!(format!("{err}").contains("identity disagree"), "got: {err}");
}

/// Codex round-2b Blocker 3: a missing/unknown lifecycle `kind` on
/// ANY lifecycle frame in the scanned epoch must err loud -- exactly
/// as the full verifier treats it -- never silently `continue` as
/// "not this kind" the way a bare-string-compare accessor would.
#[test]
fn leg_carries_run_end_marker_errs_on_invalid_lifecycle_kind() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "mk7");
    let mut w = s
        .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
        .unwrap();
    let mut e = test_env(1, 1);
    e.class = Class::Lifecycle;
    e.payload = Some(json!({"kind": "not_a_real_lifecycle_kind"}));
    w.append(&e, Commit::Immediate).unwrap();
    let seg_dir = dir.path().join("mk7").join("seg");
    let err = leg_carries_run_end_marker(&seg_dir, "mk7", 1).unwrap_err();
    assert!(format!("{err}").contains("invalid/missing kind"), "got: {err}");
}

/// Codex round-2b Blocker 3: a `run_end_requested` frame that ALSO
/// carries `take` or `fact` (forbidden by the cross-field matrix)
/// must err loud rather than being counted as a valid marker.
#[test]
fn leg_carries_run_end_marker_errs_on_marker_with_forbidden_take() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "mk8");
    let mut w = s
        .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
        .unwrap();
    let mut e = test_env(1, 1);
    e.class = Class::Lifecycle;
    e.payload = Some(json!({
        "kind": "run_end_requested",
        "reason": "quit",
        "take": {"take_epoch": 1, "holder": null}
    }));
    w.append(&e, Commit::Immediate).unwrap();
    let seg_dir = dir.path().join("mk8").join("seg");
    let err = leg_carries_run_end_marker(&seg_dir, "mk8", 1).unwrap_err();
    assert!(format!("{err}").contains("forbids take and fact"), "got: {err}");
}

#[test]
fn leg_without_the_marker_reads_false() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "mk2");
    let mut w = s.open_segment(0).unwrap();
    let mut e = test_env(1, 1);
    e.class = Class::Lifecycle;
    e.payload = Some(json!({"kind": "producer_dead", "detail": {"exit_code": 0}}));
    w.append(&e, Commit::Immediate).unwrap();
    w.seal(None).unwrap();
    let seg_dir = dir.path().join("mk2").join("seg");
    assert!(!leg_carries_run_end_marker(&seg_dir, "mk2", 1).unwrap());
}
