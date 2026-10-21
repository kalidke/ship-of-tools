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
