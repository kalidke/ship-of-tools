use super::*;

fn test_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("sot-updater-lock-{label}-{}", std::process::id()))
}

/// A process this account may not signal is still alive: the probe must not read it as dead. pid 1 is never this
/// account's (the hosted runners run tests as a user who is not root), and signal 0 delivers nothing. On macOS this
/// runs `kill`'s EPERM path; on Linux the probe reads `/proc/1`, so there it shows only that a live pid reads alive.
#[cfg(unix)]
#[test]
fn a_pid_this_account_cannot_signal_is_alive() {
    assert!(pid_alive(1), "pid 1 read as dead");
}

#[tokio::test]
async fn lock_excludes_and_releases() {
    let root = test_root("basic");
    let _ = std::fs::remove_dir_all(&root);
    let l1 = StageLock::acquire(&root, Duration::from_millis(100)).await.unwrap();
    // Second acquire times out while held.
    assert!(StageLock::acquire(&root, Duration::from_millis(200)).await.is_err());
    l1.release();
    // Freed — acquirable again.
    let l2 = StageLock::acquire(&root, Duration::from_millis(100)).await.unwrap();
    drop(l2);
    std::fs::remove_dir_all(&root).unwrap();
}

#[cfg(any(target_os = "linux", windows))]
#[tokio::test]
async fn dead_pid_lock_is_broken() {
    let root = test_root("stale");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(LOCK_DIR)).unwrap();
    // Pid 4194304 is beyond default pid_max; certainly dead.
    std::fs::write(
        root.join(LOCK_DIR).join("owner"),
        format!("4194304@{}#123\n", this_host()),
    )
    .unwrap();
    let l = StageLock::acquire(&root, Duration::from_secs(2)).await.unwrap();
    drop(l);
    std::fs::remove_dir_all(&root).unwrap();
}

/// A lock whose owner is a live process on this host is kept, on every platform that probes the pid.
#[cfg(any(target_os = "linux", windows))]
#[tokio::test]
async fn a_live_pid_lock_is_kept() {
    let root = test_root("live");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join(LOCK_DIR)).unwrap();
    std::fs::write(root.join(LOCK_DIR).join("owner"), format!("{}@{}#123\n", std::process::id(), this_host())).unwrap();
    assert!(StageLock::acquire(&root, Duration::from_millis(300)).await.is_err());
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn release_refuses_foreign_lock() {
    let root = test_root("foreign");
    let _ = std::fs::remove_dir_all(&root);
    let l1 = StageLock::acquire(&root, Duration::from_millis(100)).await.unwrap();
    // Simulate a takeover: overwrite the owner file with someone else's.
    std::fs::write(root.join(LOCK_DIR).join("owner"), "999@other#42\n").unwrap();
    l1.release(); // must NOT remove the (now foreign) lock
    assert!(root.join(LOCK_DIR).exists());
    std::fs::remove_dir_all(&root).unwrap();
}
