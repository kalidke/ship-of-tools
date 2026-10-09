//! Kernel-held file locks: the writer fence, the supervisor fence and the daemon's single-instance lock.

use crate::{Error, Result};
use std::fs::File;
use std::path::Path;
use std::path::PathBuf;
use super::{RETRY_DEADLINE_MS, RETRY_STEP_MS};
#[cfg(windows)]
use super::io_ctx;

/// The writer fence: one kernel-held exclusive lock, pinned (ADR 0039).
/// It is also the guard inside `SupervisorLock` and `DaemonLock`, so its `Drop` unlocks all three.
/// Both platform arms collapse into one std call (`File::try_lock`, Rust ≥
/// 1.89): `flock(LOCK_EX | LOCK_NB)` on unix, `LockFileEx(EXCLUSIVE |
/// FAIL_IMMEDIATELY)` on Windows. Unlocked when the guard is dropped (its
/// `Drop`: a forked child that has not yet exec'd shares the open file
/// description, so closing the guard's descriptor alone would not release
/// the lock). A killed holder runs no `Drop`; the kernel then releases the
/// lock when the handle closes — including on hard kills, with a documented
/// timing transient on both platforms that the bounded retry absorbs. Both
/// arms are mandatory, cross-process, cross-thread exclusive locks: a
/// second handle -- from another process OR another thread of the SAME
/// process -- conflicting with an already-granted lock is refused
/// (`WouldBlock`/`ERROR_LOCK_VIOLATION`), never silently granted
/// alongside it. (ADR 0041 U0 round 3: an earlier revision of this doc
/// claimed Windows admits a same-process double grant under racing
/// first acquisitions; that claim was refuted -- Microsoft's own
/// LockFileEx documentation, the normative MS-FSA conflict algorithm,
/// and Rust std's own Windows implementation all describe unconditional
/// conflict checking with no same-process exception, and the CI failure
/// that prompted the claim was a test-observation bug, not a primitive
/// gap -- see fence.rs's own test history for the corrected story.)
pub struct WriterLock {
    file: File,
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub fn lock_writer(lock_path: &Path) -> Result<WriterLock> {
    let file = open_lock_file(lock_path)?;
    // Bounded retry on WouldBlock: a just-dead writer's lock can outlive it
    // — on unix through a forked-but-not-yet-exec'd producer child holding
    // the open file description (the window closes at the child's
    // close_range, but a SIGKILLed-and-reaped capsule doesn't wait for its
    // child's schedule); on Windows through post-termination release lag,
    // which the API documents as resource-dependent with NO bound. The
    // deadline absorbs the common fast case only: a slower release fails
    // CLOSED ("lock held") — an availability error the caller retries, never
    // a second writer. A GENUINE live writer holds indefinitely and still
    // fails here within the deadline.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(RETRY_DEADLINE_MS);
    loop {
        #[allow(clippy::disallowed_methods, reason = "the WriterLock built from this lock unlocks it in its Drop")]
        let locked = file.try_lock();
        match locked {
            Ok(()) => return Ok(WriterLock { file }),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(e)) => return Err(Error::Io(e)),
        }
        if std::time::Instant::now() >= deadline {
            // Generic on purpose (round-2 finding 6): `lock_writer` is
            // also `lock_supervisor`'s own mechanism (fence.rs), so a
            // message hardcoding "voyage writer lock" would misdescribe
            // a contended supervisor.lock. Naming the path is both more
            // honest and more useful than a fixed noun either way.
            return Err(Error::State(format!(
                "lock held by another process: {lock_path:?}"
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(RETRY_STEP_MS));
    }
}

/// Bootstrap `<lock_path>` if absent: `create_new` — atomically `CREATE_NEW`
/// on Windows, `O_CREAT|O_EXCL` on unix, both mapped by std's own
/// `OpenOptions` — so two processes racing to become the first supervisor
/// can never each mint a rival inode for the same fence (ADR 0041
/// Lifecycle: "created CREATE_NEW when absent ... an atomic create stops
/// two supervisors minting rival inodes"). Idempotent over an
/// already-bootstrapped file, unlike the writer fence's own bootstrap
/// (`create_dir_protected`, which deliberately REFUSES residue): that
/// caller already removes a crashed attempt first, so finding its path
/// occupied means a concurrent bootstrap; `supervisor.lock` has no such
/// prior sweep; a bootstrap step lower in a persistent, no-history fence,
/// so surviving-and-reusing an existing file is the correct idempotent
/// behavior, not residue to refuse.
fn bootstrap_supervisor_lock(lock_path: &Path) -> Result<()> {
    match std::fs::OpenOptions::new().write(true).create_new(true).open(lock_path) {
        Ok(mut f) => {
            use std::io::Write as _;
            f.write_all(b"{}")?;
            f.sync_all()?;
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// `supervisor.lock`: the ONE-AUTHORITY fence (ADR 0041 Lifecycle "one
/// authority, one fence"). Bootstrapped with `CREATE_NEW` when absent
/// (above), then taken with the SAME kernel-lock mechanics [`lock_writer`]
/// uses — open-existing, bounded-retry `try_lock`, kernel-released on any
/// death, so it is never stale. The only difference from the writer
/// fence: this lock has no bootstrap step upstream of it (there is no
/// `sot-capsule supervise` equivalent of voyage bootstrap), so its own
/// first caller must be able to mint the inode. [`lock_writer`] and
/// [`open_lock_file`] themselves are UNCHANGED — this is a second CALLER
/// of the same open-existing arm, not a new one.
pub fn lock_supervisor(lock_path: &Path) -> Result<WriterLock> {
    bootstrap_supervisor_lock(lock_path)?;
    lock_writer(lock_path)
}

/// A supervisor fence taken to be HANDED to the process that becomes the authority (R4: the claim a capsule's
/// birth carries from acceptance to the supervisor's first act). Unlike [`WriterLock`] it has no `Drop` that
/// unlocks: dropping it only closes its descriptor, and a kernel `flock` belongs to the open file description, so
/// the lock stays held for as long as any descriptor of that description is open anywhere, a forked or exec'd
/// child's copy included. Handing it over is leaving that copy open in the child and closing this one. The
/// descriptor is close-on-exec here; the launcher clears the flag in the child's copy only
/// (`process_tree::Launch::inherit_across_exec`).
#[cfg(unix)]
pub struct HandoverLock {
    file: File,
}

#[cfg(unix)]
impl HandoverLock {
    pub fn as_raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd as _;
        self.file.as_raw_fd()
    }

    /// Take over a fence descriptor inherited across an exec: it must be the open file the lock path names, and
    /// the lock must be held on it (a second descriptor on the path is refused). The descriptor becomes
    /// close-on-exec at once, so nothing the new owner starts inherits the fence.
    pub fn adopt(fd: std::os::fd::RawFd, lock_path: &Path) -> Result<HandoverLock> {
        use std::os::fd::FromRawFd as _;
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: the caller names a descriptor it was handed for this purpose and owns nothing else with it.
        let file = unsafe { File::from_raw_fd(fd) };
        let theirs = file.metadata()?;
        let ours = std::fs::metadata(lock_path)?;
        if (theirs.dev(), theirs.ino()) != (ours.dev(), ours.ino()) {
            return Err(Error::State(format!(
                "the inherited descriptor is not the fence {lock_path:?}"
            )));
        }
        let probe = open_lock_file(lock_path)?;
        #[allow(
            clippy::disallowed_methods,
            reason = "a probe that must fail: it proves the inherited descriptor holds the lock, and unlocks nothing"
        )]
        let probed = probe.try_lock();
        match probed {
            Err(std::fs::TryLockError::WouldBlock) => {}
            Ok(()) => {
                return Err(Error::State(format!(
                    "the inherited descriptor does not hold the fence {lock_path:?}"
                )))
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(Error::Io(e)),
        }
        // SAFETY: a plain flag change on a descriptor this value owns.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        Ok(HandoverLock { file })
    }
}

/// [`lock_supervisor`] for a fence that will be handed on: bootstrapped and taken the same way, held by a guard
/// that only closes (see [`HandoverLock`]).
#[cfg(unix)]
pub fn lock_supervisor_for_handover(lock_path: &Path) -> Result<HandoverLock> {
    bootstrap_supervisor_lock(lock_path)?;
    let file = open_lock_file(lock_path)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(RETRY_DEADLINE_MS);
    loop {
        #[allow(
            clippy::disallowed_methods,
            reason = "the HandoverLock built from this lock is released by the kernel when the last descriptor of the description closes, never by an unlock a copy would defeat"
        )]
        let locked = file.try_lock();
        match locked {
            Ok(()) => return Ok(HandoverLock { file }),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(e)) => return Err(Error::Io(e)),
        }
        if std::time::Instant::now() >= deadline {
            return Err(Error::State(format!(
                "lock held by another process: {lock_path:?}"
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(RETRY_STEP_MS));
    }
}

/// Unix lock open: open-existing ONLY, matching the Windows arm — bootstrap
/// created the fence, and absence means a mutilated store. With `create`,
/// unlinking the held lock path would let the next writer mint a fresh
/// inode and take an independent flock: two live fences.
#[cfg(unix)]
fn open_lock_file(lock_path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC) // no child inherits the fence
        .open(lock_path)?;
    Ok(f)
}

/// Windows lock open (ADR 0041): open-existing ONLY — never silently
/// recreate a missing persistent fence (bootstrap created it; absence means
/// a mutilated store). Opened WITHOUT `FILE_SHARE_DELETE` (std's default
/// shares delete/rename) so the locked path cannot be replaced out from
/// under the fence to mint a second one — and with
/// `FILE_FLAG_OPEN_REPARSE_POINT` + a post-open attribute check, because
/// the share deny protects the OPENED object: without the flag CreateFileW
/// follows a symlink/junction planted at the lock path and the fence would
/// bind (and deny sharing on) the TARGET, leaving the link free to be
/// re-pointed for a second fence. Std opens its files non-inheritable.
#[cfg(windows)]
fn open_lock_file(lock_path: &Path) -> Result<File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    };
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(lock_path)
        .map_err(|e| io_ctx(e, format_args!("open writer.lock (open-existing) {lock_path:?}")))?;
    // Handle-derived attributes (GetFileInformationByHandle) — checking the
    // opened object itself, not a raced re-stat of the path.
    if f.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(Error::State(format!(
            "writer.lock at {lock_path:?} is a reparse point — refusing a redirected fence"
        )));
    }
    Ok(f)
}

/// The daemon's single-instance lock file name under the state dir.
const DAEMON_LOCK_FILE_NAME: &str = "daemon.lock";

/// `<state_dir>/daemon.lock`.
pub fn daemon_lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join(DAEMON_LOCK_FILE_NAME)
}

/// The held daemon lock: unlocked when dropped, kernel-released on any death.
pub struct DaemonLock(#[allow(dead_code)] WriterLock); // unlocked by `WriterLock`'s Drop

/// One attempt at the daemon lock under `state_dir`: `Ok(None)` when
/// another holder has it. std's `File::try_lock` is the same kernel lock
/// `host` uses for the supervisor fence.
pub fn try_lock_daemon(state_dir: &Path) -> std::io::Result<Option<DaemonLock>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(daemon_lock_path(state_dir))?;
    #[allow(clippy::disallowed_methods, reason = "the WriterLock built from this lock unlocks it in its Drop")]
    let locked = file.try_lock();
    match locked {
        Ok(()) => Ok(Some(DaemonLock(WriterLock { file }))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
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
            let status = cmd.status().unwrap();
            drop((ready_w, go_r));
            status
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
        let out = spawn_role("daemon", dir.path()).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "daemon role failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
        // Guards the zero-tests-matched hazard: exit 0 alone cannot catch it.
        assert!(stdout.contains("1 passed"), "daemon role ran no test:\n{stdout}");
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
}
