//! Verifier tests: feature opt-ins, spilled frames, turn closure, f64 gates, rotation and attached_to.

use super::support_tests::store;
use super::*;
use crate::store::envelope::*;
use crate::store::segment::{tests::test_env, Commit};
use serde_json::json;

#[test]
fn inline_input_before_optin_fails_after_optin_passes() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "v");
    let mut w = s.open_segment(0).unwrap();

    let mut input = test_env(1, 1);
    input.class = Class::Input;
    input.payload = Some(json!({
        "idem_key": "0".repeat(32),
        "content": {"inline": "secret"}
    }));
    w.append(&input, Commit::Immediate).unwrap();
    let d = w.seal(None).unwrap();
    s.advance_chain(d);
    let root = dir.path().join("v");
    assert!(verify_voyage(&root, "v").is_err());

    // A fresh voyage with optin FIRST verifies green.
    let mut s2 = store(dir.path(), "v2");
    let mut w2 = s2.open_segment(0).unwrap();
    let mut optin = test_env(1, 1);
    optin.class = Class::Lifecycle;
    optin.payload = Some(json!({"kind": "capture_optin"}));
    w2.append(&optin, Commit::Immediate).unwrap();
    let mut input2 = test_env(1, 2);
    input2.class = Class::Input;
    input2.payload = Some(json!({
        "idem_key": "0".repeat(32),
        "content": {"inline": "ok now"}
    }));
    w2.append(&input2, Commit::Immediate).unwrap();
    w2.seal(None).unwrap();
    verify_voyage(&dir.path().join("v2"), "v2").unwrap();
}

#[test]
fn forward_ref_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "v3");
    let mut w = s.open_segment(0).unwrap();
    let mut e = test_env(1, 1);
    e.class = Class::Lifecycle;
    e.payload = Some(json!({"kind": "producer_ready"}));
    e.refs = vec![FrameRef {
        kind: RefKind::CausedBy,
        frame: Seq { epoch: 1, n: 5 }, // later frame
    }];
    w.append(&e, Commit::Immediate).unwrap();
    w.seal(None).unwrap();
    assert!(verify_voyage(&dir.path().join("v3"), "v3").is_err());
}

/// Locator-must-declare (ADR 0039 registry): scheme "cgroup" requires
/// the segment to declare cgroup-fence-v1; "none"/absent claim no
/// authority; unknown schemes and empty paths fail closed.
#[test]
fn locator_must_declare_cgroup_fence() {
    let run = |name: &str, features: Vec<String>, detail: serde_json::Value| {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), name);
        let mut w = s.open_segment_with_features(0, features).unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "producer_spawn", "detail": detail}));
        w.append(&e, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        verify_voyage(&dir.path().join(name), name).map(|_| ())
    };
    let fence = || vec!["sot.capsule.cgroup-fence-v1".to_string()];
    let err = run(
        "l1",
        vec![],
        json!({"kill_domain": {"scheme": "cgroup", "path": "/sys/fs/cgroup/x"}}),
    )
    .unwrap_err();
    assert!(format!("{err}").contains("does not declare"), "got: {err}");
    run(
        "l2",
        fence(),
        json!({"kill_domain": {"scheme": "cgroup", "path": "/sys/fs/cgroup/x"}}),
    )
    .unwrap();
    // No authority claimed — explicitly ("none") or by absence (the P1
    // PTY capsule's spawn detail): no feature needed.
    run("l3", vec![], json!({"kill_domain": {"scheme": "none"}})).unwrap();
    run("l4", vec![], json!({"argv": ["sh"]})).unwrap();
    // Unknown scheme and empty path fail closed even when declared.
    assert!(run("l5", fence(), json!({"kill_domain": {"scheme": "jail"}})).is_err());
    assert!(run("l6", fence(), json!({"kill_domain": {"scheme": "cgroup", "path": ""}})).is_err());
}

/// ADR 0041 step 6 U1b: `run_end_requested`'s registered feature,
/// enforced bidirectionally like `cgroup-fence-v1` above — refused in
/// a segment that doesn't declare it; a declared, present, string
/// `reason` (empty legal) verifies green; a missing/non-string reason
/// or a take/fact alongside it fails closed even when declared.
#[test]
fn run_end_requested_needs_its_declared_feature() {
    let run = |name: &str, features: Vec<String>, payload: serde_json::Value| {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), name);
        let mut w = s.open_segment_with_features(0, features).unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(payload);
        w.append(&e, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        verify_voyage(&dir.path().join(name), name).map(|_| ())
    };
    let feat = || vec!["sot.capsule.run-end-requested-v1".to_string()];
    let err = run("re1", vec![], json!({"kind": "run_end_requested", "reason": "quit"}))
        .unwrap_err();
    assert!(format!("{err}").contains("does not declare"), "got: {err}");
    run("re2", feat(), json!({"kind": "run_end_requested", "reason": "quit"})).unwrap();
    // The wire's shutdown.reason permits empty (require_nonempty=false
    // in lane/wire/) — the marker carries it verbatim.
    run("re3", feat(), json!({"kind": "run_end_requested", "reason": ""})).unwrap();
    assert!(run("re4", feat(), json!({"kind": "run_end_requested"})).is_err());
    assert!(run("re5", feat(), json!({"kind": "run_end_requested", "reason": 1})).is_err());
    assert!(run(
        "re6",
        feat(),
        json!({"kind": "run_end_requested", "reason": "quit",
               "take": {"take_epoch": 1, "holder": null}})
    )
    .is_err());
    // Codex round-1 Minor 10: reason obeys str128 (128 UTF-8 bytes)
    // like every other ADR 0039 str128 site -- exactly at the bound
    // verifies green, one byte over fails closed.
    run("re7", feat(), json!({"kind": "run_end_requested", "reason": "a".repeat(128)})).unwrap();
    assert!(run("re8", feat(), json!({"kind": "run_end_requested", "reason": "a".repeat(129)})).is_err());
}

/// Codex round-1 Major 7: at most one `run_end_requested` per writer
/// epoch -- a second well-formed marker in the SAME epoch, in a
/// segment that declares the feature, must fail verification even
/// though each frame is individually well-formed.
#[test]
fn verifier_refuses_two_run_end_markers_in_one_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "dup1");
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
    w.seal(None).unwrap();
    let err = verify_voyage(&dir.path().join("dup1"), "dup1").unwrap_err();
    assert!(format!("{err}").contains("second run_end_requested"), "got: {err}");
}

/// The bidirectional half `run_end_requested_needs_its_declared_feature`
/// doesn't reach: a segment declaring a feature name this build's
/// `REGISTERED_FEATURES` doesn't know is refused WHOLESALE, before any
/// frame inside it is even decoded — the exact mechanism a reader
/// shipped before an entry existed relies on to refuse a writer's
/// segment it cannot safely interpret (ADR 0041 "reader lands one
/// release before the writer"). A fictitious name stands in for "not
/// yet in this build" since `REGISTERED_FEATURES` is a compile-time
/// const.
#[test]
fn unknown_feature_name_refuses_the_whole_segment() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "unk1");
    let mut w = s
        .open_segment_with_features(0, vec!["sot.capsule.not-yet-registered-v1".to_string()])
        .unwrap();
    let mut e = test_env(1, 1);
    e.class = Class::Lifecycle;
    e.payload = Some(json!({"kind": "producer_ready"}));
    w.append(&e, Commit::Immediate).unwrap();
    w.seal(None).unwrap();
    let err = verify_voyage(&dir.path().join("unk1"), "unk1").unwrap_err();
    assert!(format!("{err}").contains("unknown feature"), "got: {err}");
}

/// Review pin: payload_ref is producer-class only — a spilled
/// control-plane frame would carry its cross-field obligations (the
/// take matrix, the WAL lattice, locator-must-declare) out of the
/// verifier's inline walk. Enforced in Envelope::validate(), which
/// runs on BOTH append and segment read, so writer and verifier can
/// never disagree: the frame cannot even be written.
#[test]
fn spilled_control_frame_is_refused() {
    use crate::store::envelope::{PayloadEncoding, PayloadRef};
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "sp2");
    let content =
        br#"{"kind":"producer_spawn","detail":{"kill_domain":{"scheme":"cgroup","path":"/x"}}}"#;
    let digest = s.publish_blob(content).unwrap();
    let mut w = s.open_segment(0).unwrap();
    let mut e = test_env(1, 1);
    e.class = Class::Lifecycle;
    e.payload = None;
    e.payload_ref = Some(PayloadRef {
        blob: crate::store::envelope::BlobRef {
            algo: "sha256".into(),
            digest,
            length: content.len() as u64,
            media_type: "application/json".into(),
        },
        encoding: PayloadEncoding::JsonUtf8,
    });
    let err = w.append(&e, Commit::Immediate).unwrap_err();
    assert!(format!("{err}").contains("producer-class only"), "got: {err}");
}

#[test]
fn turn_closure_modes() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "tc1");
    let mut w = s.open_segment(0).unwrap();
    let mut open = test_env(1, 1);
    open.class = Class::TurnOpen;
    open.payload = Some(json!({"admitted_by": "test/rule"}));
    w.append(&open, Commit::Immediate).unwrap();
    // Segment stays OPEN (tip) with an unclosed turn.
    w.commit().unwrap();
    drop(w);
    let root = dir.path().join("tc1");
    // Complete: unmatched open is loud.
    assert!(verify_voyage(&root, "tc1").is_err());
    // AllowOpenTip: tolerated (one unmatched, in the open tip's epoch).
    verify_voyage_mode(&root, "tc1", VerifyMode::AllowOpenTip).unwrap();

    // Now a CLOSED turn in a sealed segment passes complete.
    let dir2 = tempfile::tempdir().unwrap();
    let mut s2 = store(dir2.path(), "tc2");
    let mut w2 = s2.open_segment(0).unwrap();
    let mut o2 = test_env(1, 1);
    o2.class = Class::TurnOpen;
    o2.payload = Some(json!({"admitted_by": "test/rule"}));
    w2.append(&o2, Commit::Immediate).unwrap();
    let mut c2 = test_env(1, 2);
    c2.class = Class::TurnClose;
    c2.payload = Some(json!({"reason": "producer_done"}));
    c2.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: Seq { epoch: 1, n: 1 } }];
    w2.append(&c2, Commit::Immediate).unwrap();
    w2.seal(None).unwrap();
    verify_voyage(&dir2.path().join("tc2"), "tc2").unwrap();
}

#[test]
fn f64_feature_gates_fractional_producer_numbers() {
    use crate::store::segment::{HeaderBody, RetentionClass, SegmentWriter};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("f1");
    crate::store::voyage::VoyageStore::bootstrap(&root, "f1", RetentionClass::Discard).unwrap();
    let build = |features: Vec<String>, name: &str, dir: &std::path::Path| {
        let root = dir.join(name);
        crate::store::voyage::VoyageStore::bootstrap(&root, name, RetentionClass::Discard).ok();
        let header = HeaderBody {
            version: 1,
            required_features: features,
            voyage_id: name.into(),
            segment_index: 0,
            epoch: 1,
            prev_seal_digest: None,
            created_wall_ms: 0,
            retention_class: Some(RetentionClass::Discard),
        };
        let mut w = SegmentWriter::create(&root.join("seg"), header).unwrap();
        let mut att = test_env(1, 1);
        att.class = Class::ProducerAttached;
        att.payload = Some(json!({
            "producer_kind": "t", "version": "1",
            "profile_def": {"id": "d", "sha256": "0".repeat(64), "rules": {}}
        }));
        w.append(&att, Commit::Immediate).unwrap();
        let mut prod = test_env(1, 2);
        prod.refs = vec![FrameRef { kind: RefKind::AttachedTo, frame: Seq { epoch: 1, n: 1 } }];
        prod.payload = Some(json!({"cost": 0.0123, "exp": 1.5e-8, "n": 3}));
        w.append(&prod, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        root
    };
    // Without the feature: fractional producer numbers are loud.
    let r1 = build(vec![], "f_no", dir.path());
    assert!(verify_voyage(&r1, "f_no").is_err());
    // With the registered feature: green.
    let r2 = build(vec!["sot.producer.json-f64-v1".into()], "f_yes", dir.path());
    verify_voyage(&r2, "f_yes").unwrap();
    // Unknown feature: loud.
    let r3 = build(vec!["sot.future.unknown-v9".into()], "f_unk", dir.path());
    assert!(verify_voyage(&r3, "f_unk").is_err());
}

/// Review F1: a producer frame whose JSON payload rides a payload_ref
/// (spilled) must hit the same f64 gate as an inline payload.
#[test]
fn f64_gate_covers_spilled_json_payload_ref() {
    use crate::store::envelope::{PayloadEncoding, PayloadRef};
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "sp1");
    let content = br#"{"cost": 0.5}"#;
    let digest = s.publish_blob(content).unwrap();
    let mut w = s.open_segment(0).unwrap();
    let mut att = test_env(1, 1);
    att.class = Class::ProducerAttached;
    att.payload = Some(json!({
        "producer_kind": "t", "version": "1",
        "profile_def": {"id": "d", "sha256": "0".repeat(64), "rules": {}}
    }));
    w.append(&att, Commit::Immediate).unwrap();
    let mut prod = test_env(1, 2);
    prod.refs = vec![FrameRef { kind: RefKind::AttachedTo, frame: Seq { epoch: 1, n: 1 } }];
    prod.payload = None;
    prod.payload_ref = Some(PayloadRef {
        blob: crate::store::envelope::BlobRef {
            algo: "sha256".into(),
            digest,
            length: content.len() as u64,
            media_type: "application/json".into(),
        },
        encoding: PayloadEncoding::JsonUtf8,
    });
    w.append(&prod, Commit::Immediate).unwrap();
    w.seal(None).unwrap();
    let err = verify_voyage(&dir.path().join("sp1"), "sp1").unwrap_err();
    assert!(format!("{err}").contains("via payload_ref"), "got: {err}");
}

/// Review F2 pin: an unmatched open in a SEALED segment of the SAME
/// epoch as the open tip is tolerated by allow-open-tip — rotation
/// within one run is normal, and a live turn may span it (the ADR's
/// predicate is per-EPOCH, deliberately).
#[test]
fn allow_open_tip_tolerates_rotation_spanning_turn() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "rot1");
    let mut w = s.open_segment(0).unwrap();
    let mut open = test_env(1, 1);
    open.class = Class::TurnOpen;
    open.payload = Some(json!({"admitted_by": "test/rule"}));
    w.append(&open, Commit::Immediate).unwrap();
    let d = w.seal(None).unwrap(); // rotation: sealed with the turn open
    s.advance_chain(d);
    let mut w2 = s.open_segment(0).unwrap(); // same epoch, open tip
    let mut lc = test_env(1, 2);
    lc.class = Class::Lifecycle;
    lc.payload = Some(json!({"kind": "producer_ready"}));
    w2.append(&lc, Commit::Immediate).unwrap();
    w2.commit().unwrap();
    drop(w2);
    let root = dir.path().join("rot1");
    assert!(verify_voyage(&root, "rot1").is_err()); // complete: loud
    verify_voyage_mode(&root, "rot1", VerifyMode::AllowOpenTip).unwrap();
}

/// Review F4 pin: the integer-atoms rule is WIRE-FORM integrality —
/// "3.0" is not an integer atom (matches the §3 shortest-decimal rule),
/// deliberately. A refactor to value-integrality must fail here.
#[test]
fn integral_float_wire_form_is_refused_without_f64() {
    assert!(check_integer_numbers(&json!(3.0)).is_err());
    assert!(check_integer_numbers(&json!(3)).is_ok());
    assert!(check_integer_numbers(&json!({"a": [1, {"b": 2}]})).is_ok());
    assert!(check_integer_numbers(&json!({"a": [1, {"b": 2.0}]})).is_err());
}

#[test]
fn producer_frame_requires_attached_to() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(dir.path(), "v4");
    let mut w = s.open_segment(0).unwrap();
    // test_env is class=Producer with no refs — must fail verification.
    w.append(&test_env(1, 1), Commit::Immediate).unwrap();
    w.seal(None).unwrap();
    assert!(verify_voyage(&dir.path().join("v4"), "v4").is_err());

    // With a producer_attached + attached_to it verifies.
    let mut s2 = store(dir.path(), "v5");
    let mut w2 = s2.open_segment(0).unwrap();
    let mut att = test_env(1, 1);
    att.class = Class::ProducerAttached;
    att.payload = Some(json!({
        "producer_kind": "julia-repl", "version": "1",
        "profile_def": {"id": "default", "sha256": "0".repeat(64), "rules": {}}
    }));
    w2.append(&att, Commit::Immediate).unwrap();
    let mut prod = test_env(1, 2);
    prod.refs = vec![FrameRef {
        kind: RefKind::AttachedTo,
        frame: Seq { epoch: 1, n: 1 },
    }];
    w2.append(&prod, Commit::Immediate).unwrap();
    w2.seal(None).unwrap();
    verify_voyage(&dir.path().join("v5"), "v5").unwrap();
}
