//! Tests of a launch's acceptance.

use super::*;
use std::os::unix::ffi::OsStringExt;

/// A launch whose row's fence is already claimed (a live authority, or another birth's claim) is refused `Contended`
/// before anything is forked: the claim is taken before the launch is built, and its refusal returns at once.
#[test]
fn a_claimed_fence_forks_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("row");
    std::fs::create_dir_all(&state_dir).expect("state folder");
    let held = BirthClaim::take(&state_dir).expect("the first claim");
    let spec = LaunchSpec {
        id: 1,
        state_dir: state_dir.clone(),
        program: dir.path().join("no-such-supervisor").into_os_string().into_vec(),
        args: vec![b"supervise".to_vec()],
        inject_at: 1,
        env: Vec::new(),
        cwd: dir.path().to_path_buf(),
    };
    let stderr = OwnedFd::from(std::fs::File::open("/dev/null").expect("/dev/null"));
    assert!(matches!(accept(&spec, stderr, &[]), Err(Refusal::Contended)), "a claimed fence was not refused Contended");
    drop(held);
}
