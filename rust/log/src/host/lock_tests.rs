use super::*;

#[test]
fn lock_supervisor_bootstraps_an_absent_lock_file_and_holds_it() {
    let dir = tempfile::tempdir().unwrap();
    let lock_path = dir.path().join("supervisor.lock");
    assert!(!lock_path.exists());
    let guard = lock_supervisor(&lock_path).unwrap();
    assert!(lock_path.is_file());
    drop(guard);
}

#[test]
fn lock_supervisor_is_idempotent_over_an_already_bootstrapped_file() {
    let dir = tempfile::tempdir().unwrap();
    let lock_path = dir.path().join("supervisor.lock");
    {
        let guard = lock_supervisor(&lock_path).unwrap();
        drop(guard);
    }
    // A second bootstrap-then-lock over the SAME (already-created) file
    // must not treat the existing inode as a conflict.
    let guard = lock_supervisor(&lock_path).unwrap();
    drop(guard);
}

#[test]
fn lock_supervisor_refuses_a_second_concurrent_holder() {
    let dir = tempfile::tempdir().unwrap();
    let lock_path = dir.path().join("supervisor.lock");
    let _held = lock_supervisor(&lock_path).unwrap();
    match lock_supervisor(&lock_path) {
        Err(e) => assert!(format!("{e}").contains("held by another process"), "{e}"),
        Ok(_) => panic!("expected a second lock_supervisor call to fail while the first is held"),
    }
}

#[test]
fn lock_supervisor_two_racing_bootstraps_leave_exactly_one_winner() {
    // The `CREATE_NEW` race itself: two threads bootstrapping the SAME
    // absent path concurrently must never both believe they created
    // it, and neither may error out on the other's win (ADR 0041:
    // "an atomic create stops two supervisors minting rival inodes").
    let dir = tempfile::tempdir().unwrap();
    let lock_path = dir.path().join("supervisor.lock");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let results: Vec<_> = (0..2)
        .map(|_| {
            let lock_path = lock_path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                bootstrap_supervisor_lock(&lock_path)
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();
    assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
    assert!(lock_path.is_file());
}

/// Runs `check` while a forked child of this process sits between fork
/// and exec holding copies of every descriptor open now (the state a
/// concurrent test's `Command::spawn` creates). The child blocks on a
/// pipe, not a clock: it writes one byte to `ready`, then reads `go`.
#[cfg(unix)]
fn while_a_forked_child_waits_to_exec(check: impl FnOnce()) {
    use std::os::unix::io::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;
    use std::io::Write as _;
    let (mut ready_r, ready_w) = std::io::pipe().unwrap();
    let (go_r, go_w) = std::io::pipe().unwrap();
    let (ready_fd, go_fd) = (ready_w.as_raw_fd(), go_r.as_raw_fd());
    let spawner = std::thread::spawn(move || {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "exit 0"]);
        // SAFETY: the closure calls only async-signal-safe `write` and `read`.
        unsafe {
            cmd.pre_exec(move || {
                let byte = 1u8;
                libc::write(ready_fd, (&byte as *const u8).cast(), 1);
                let mut got = 0u8;
                libc::read(go_fd, (&mut got as *mut u8).cast(), 1);
                Ok(())
            });
        }
        // `spawn` returns only after the child has exec'd.
        let mut child = cmd.spawn().unwrap();
        drop((ready_w, go_r));
        crate::test_isolated::wait_within(&mut child, crate::test_isolated::ISOLATION_TIMEOUT)
    });
    // Writes `go` on drop, so a failing `check` still lets the child exec.
    struct Go(std::io::PipeWriter);
    impl Drop for Go {
        fn drop(&mut self) {
            let _ = self.0.write_all(b"g");
        }
    }
    let go = Go(go_w);
    let mut byte = [0u8; 1];
    std::io::Read::read_exact(&mut ready_r, &mut byte).unwrap();
    check();
    drop(go);
    assert!(spawner.join().unwrap().success());
}

#[cfg(unix)]
#[test]
fn a_dropped_daemon_lock_is_free_while_a_forked_child_waits_to_exec() {
    let dir = tempfile::tempdir().unwrap();
    let held = try_lock_daemon(dir.path()).unwrap();
    assert!(held.is_some());
    while_a_forked_child_waits_to_exec(|| {
        drop(held);
        assert!(try_lock_daemon(dir.path()).unwrap().is_some());
    });
}

#[cfg(unix)]
#[test]
fn a_dropped_writer_lock_is_free_while_a_forked_child_waits_to_exec() {
    let dir = tempfile::tempdir().unwrap();
    let lock_path = dir.path().join("writer.lock");
    std::fs::write(&lock_path, b"{}").unwrap();
    let held = lock_writer(&lock_path).unwrap();
    while_a_forked_child_waits_to_exec(|| {
        drop(held);
        lock_writer(&lock_path).unwrap();
    });
}

/// The child role for the test below; a silent no-op unless
/// `DAEMON_LOCK_XPROC_ROLE` is set (the shape of fence.rs's cross-process
/// race). `daemon` takes the lock, spawns a `supervisor` with std's
/// `Command` (as the daemon spawns its supervisors), waits until it is
/// alive, then returns holding the lock without a `Drop`, as a killed
/// daemon does: its handle closes only at process exit.
#[test]
fn daemon_lock_child_role() {
    let Ok(role) = std::env::var("DAEMON_LOCK_XPROC_ROLE") else {
        return;
    };
    let dir = PathBuf::from(std::env::var("DAEMON_LOCK_XPROC_DIR").unwrap());
    let wait_for = |name: &str, secs: u64| {
        let path = dir.join(name);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while !path.exists() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {name}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    match role.as_str() {
        "daemon" => {
            // As the real daemon does before any spawn: its inherited
            // stdio handles must not reach the supervisor.
            #[cfg(windows)]
            crate::host::winhandle::harden_own_stdio(true).unwrap();
            let guard = try_lock_daemon(&dir.join("state")).unwrap().expect("daemon lock free");
            spawn_role("supervisor", &dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            wait_for("alive", 30);
            std::mem::forget(guard);
        }
        "supervisor" => {
            std::fs::write(dir.join("alive"), b"").unwrap();
            wait_for("go", 60);
            std::fs::write(dir.join("done"), b"").unwrap();
        }
        other => panic!("unknown role {other}"),
    }
}

fn spawn_role(role: &str, dir: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "host::lock::tests::daemon_lock_child_role", "--nocapture", "--test-threads=1"])
        .env("DAEMON_LOCK_XPROC_ROLE", role)
        .env("DAEMON_LOCK_XPROC_DIR", dir);
    cmd
}

#[test]
fn a_child_spawned_while_the_daemon_lock_is_held_never_holds_it() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let ran = crate::test_wait::run_within(&mut spawn_role("daemon", dir.path()), crate::test_isolated::ISOLATION_TIMEOUT);
    assert!(ran.status.success(), "daemon role failed:\n{ran}");
    // Guards the zero-tests-matched hazard: exit 0 alone cannot catch it.
    assert!(ran.stdout.contains("1 passed"), "daemon role ran no test:\n{ran}");
    assert!(dir.path().join("alive").exists());
    assert!(!dir.path().join("done").exists());
    // The daemon is gone; the supervisor lives until `go`. A new daemon's
    // start waits for the release the OS leaves unbounded in time.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let taken = loop {
        if let Some(lock) = try_lock_daemon(&state).unwrap() {
            break lock;
        }
        assert!(std::time::Instant::now() < deadline, "a spawned child still holds daemon.lock");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    drop(taken);
    std::fs::write(dir.path().join("go"), b"").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !dir.path().join("done").exists() {
        assert!(std::time::Instant::now() < deadline, "the supervisor did not finish");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn try_lock_daemon_excludes_a_second_holder() {
    let dir = tempfile::tempdir().unwrap();
    let first = try_lock_daemon(dir.path()).unwrap();
    assert!(first.is_some());
    assert!(try_lock_daemon(dir.path()).unwrap().is_none());
    drop(first);
    assert!(try_lock_daemon(dir.path()).unwrap().is_some());
}
