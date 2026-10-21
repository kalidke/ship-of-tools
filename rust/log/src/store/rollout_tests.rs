use super::*;

fn installed(features: &[&str]) -> RolloutEvidence {
    RolloutEvidence::Installed {
        release: "0.5.9".to_string(),
        target: "test-target".to_string(),
        reader_features: features.iter().map(|f| f.to_string()).collect(),
    }
}

#[test]
fn no_rollback_target_is_a_no_op() {
    gate(&RolloutEvidence::NoRollbackTarget, "sot.capsule.run-end-requested-v1").unwrap();
}

#[test]
fn installed_target_supporting_the_feature_passes() {
    gate(&installed(&["sot.capsule.run-end-requested-v1"]), "sot.capsule.run-end-requested-v1")
        .unwrap();
}

#[test]
fn installed_target_missing_the_feature_fails_closed() {
    let err = gate(&installed(&["sot.producer.json-f64-v1"]), "sot.capsule.run-end-requested-v1")
        .unwrap_err();
    assert!(format!("{err}").contains("cannot decode"), "got: {err}");
    assert!(format!("{err}").contains("0.5.9"), "expected the release identity in the error: {err}");
}

#[test]
fn empty_installed_set_fails_closed() {
    assert!(gate(&installed(&[]), "sot.capsule.run-end-requested-v1").is_err());
}

#[test]
fn absent_file_reads_as_none_never_as_no_rollback_target() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_rollout_evidence(dir.path()).unwrap(), None);
}

#[test]
fn present_file_reads_no_rollback_target() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("rollout-evidence.json"),
        br#"{"state":"no_rollback_target"}"#,
    )
    .unwrap();
    assert_eq!(
        read_rollout_evidence(dir.path()).unwrap(),
        Some(RolloutEvidence::NoRollbackTarget)
    );
}

#[test]
fn present_file_reads_installed_evidence() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("rollout-evidence.json"),
        br#"{"state":"installed","release":"0.5.9","target":"test-target",
                 "reader_features":["sot.capsule.run-end-requested-v1"]}"#,
    )
    .unwrap();
    assert_eq!(
        read_rollout_evidence(dir.path()).unwrap(),
        Some(installed(&["sot.capsule.run-end-requested-v1"]))
    );
}

#[test]
fn corrupt_file_is_loud_not_absent() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("rollout-evidence.json"), b"not json").unwrap();
    assert!(read_rollout_evidence(dir.path()).is_err());
}
