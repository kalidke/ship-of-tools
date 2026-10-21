use super::state_dir_hash;

#[test]
fn state_dir_hash_is_stable_for_the_same_path() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(state_dir_hash(dir.path()), state_dir_hash(dir.path()));
}

#[test]
fn state_dir_hash_differs_for_different_paths() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    assert_ne!(state_dir_hash(a.path()), state_dir_hash(b.path()));
}
