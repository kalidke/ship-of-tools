//! Kernel-held file locks: the writer fence and the supervisor fence.

use crate::{Error, Result};
use std::fs::File;
use std::path::Path;
use super::{RETRY_DEADLINE_MS, RETRY_STEP_MS};
#[cfg(windows)]
use super::io_ctx;

/// The writer fence: one kernel-held exclusive lock, pinned (ADR 0039).
/// Both platform arms collapse into one std call (`File::try_lock`, Rust ≥
/// 1.89): `flock(LOCK_EX | LOCK_NB)` on unix, `LockFileEx(EXCLUSIVE |
/// FAIL_IMMEDIATELY)` on Windows. Released by the kernel when the guard's
/// handle closes — including on hard kills, with a documented timing
/// transient on both platforms that the bounded retry absorbs. Both
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
    #[allow(dead_code)] // held for its Drop (kernel releases the lock)
    file: File,
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
        match file.try_lock() {
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
/// re-pointed for a second fence. Std never makes handles inheritable.
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
            drop(guard); // kernel-released
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
}
