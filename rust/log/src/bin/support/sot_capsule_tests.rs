#[test]
fn supervisor_lock_facade_is_reachable_and_works_from_this_binary_crate() {
    let dir = tempfile::tempdir().unwrap();
    let guard = sot_log::supervisor::journal::fence::lock_supervisor(dir.path()).unwrap();
    assert!(sot_log::supervisor::journal::fence::supervisor_lock_path(dir.path()).is_file());
    drop(guard);
}
