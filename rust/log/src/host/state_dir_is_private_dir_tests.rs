use super::is_private_dir;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch_dir(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "sot-log-state-dir-test-{}-{}-{name}",
        std::process::id(),
        n
    ))
}

#[test]
fn owner_only_dir_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let d = scratch_dir("owner-only");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(is_private_dir(&d));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn group_readable_dir_is_not_private() {
    use std::os::unix::fs::PermissionsExt;
    let d = scratch_dir("group-readable");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o750)).unwrap();
    assert!(!is_private_dir(&d));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn world_readable_dir_is_not_private() {
    use std::os::unix::fs::PermissionsExt;
    let d = scratch_dir("world-readable");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!is_private_dir(&d));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn missing_dir_is_not_private() {
    let d = scratch_dir("missing");
    assert!(!is_private_dir(&d));
}

#[test]
fn a_file_is_not_a_private_dir() {
    let d = scratch_dir("a-file");
    std::fs::write(&d, b"not a dir").unwrap();
    assert!(!is_private_dir(&d));
    let _ = std::fs::remove_file(&d);
}

#[test]
fn symlink_to_a_private_dir_is_rejected() {
    // The hijack case (F1): even a symlink pointing at an otherwise-
    // valid owner-only dir must be rejected — trusting it would let an
    // attacker who controls the symlink redirect us anywhere later by
    // repointing it, and `is_private_dir` must reject based on the
    // PATH's own type (lstat), not what it resolves to.
    use std::os::unix::fs::PermissionsExt;
    let target = scratch_dir("symlink-target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = scratch_dir("symlink-link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(!is_private_dir(&link));
    let _ = std::fs::remove_file(&link);
    let _ = std::fs::remove_dir_all(&target);
}
