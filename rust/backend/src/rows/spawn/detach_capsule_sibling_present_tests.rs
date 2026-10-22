use super::*;

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sot-capsule-sibling-test-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[test]
fn present_when_executable_sibling_exists() {
    let dir = scratch_dir("present");
    let daemon = dir.join("sotd");
    let sibling = dir.join(CAPSULE_SIBLING_NAME);
    sot_platform::test_exec::write_executable(&sibling, b"#!/bin/sh\n");
    assert!(capsule_sibling_present(&daemon));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn absent_when_sibling_missing() {
    let dir = scratch_dir("absent");
    let daemon = dir.join("sotd");
    assert!(!capsule_sibling_present(&daemon));
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn absent_when_sibling_not_executable() {
    let dir = scratch_dir("noexec");
    let daemon = dir.join("sotd");
    let sibling = dir.join(CAPSULE_SIBLING_NAME);
    std::fs::write(&sibling, b"#!/bin/sh\n").expect("write sibling");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&sibling, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert!(!capsule_sibling_present(&daemon));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn sot_capsule_exe_is_the_sibling_name_beside_this_executable() {
    let literal = if cfg!(windows) { "sot-capsule.exe" } else { "sot-capsule" };
    let beside = std::env::current_exe().expect("current_exe").parent().expect("parent").join(literal);
    assert_eq!(sot_capsule_exe().expect("sot_capsule_exe"), beside);
    assert_eq!(CAPSULE_SIBLING_NAME, literal);
}
