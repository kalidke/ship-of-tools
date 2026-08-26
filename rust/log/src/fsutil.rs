//! Durability primitives: dir fsync, no-clobber rename, exclusive lock.
//! Unix-only where the OS must guarantee semantics; the pure codec compiles
//! everywhere, but voyage stores refuse to open on non-unix (v1).

use crate::{Error, Result};
use std::fs::File;
use std::path::Path;

#[cfg(unix)]
pub fn fsync_dir(dir: &Path) -> Result<()> {
    let f = File::open(dir)?;
    f.sync_all()?;
    Ok(())
}

/// Windows placeholder until the P3 implementation (`FlushFileBuffers` on a
/// `FILE_FLAG_BACKUP_SEMANTICS` directory handle — std cannot open directory
/// handles, so it needs windows-sys). The format is OS-neutral; only these
/// durability ops are platform code, and the store FAILS CLOSED on Windows
/// meanwhile (`lock_writer` refuses), so this no-op is reachable only by
/// tests exercising the state-machine logic — never by a real store.
#[cfg(not(unix))]
pub fn fsync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

/// RENAME_NOREPLACE: the commit point of every publication. Destination
/// existing is the caller's loud condition, surfaced as AlreadyExists.
#[cfg(target_os = "linux")]
pub fn rename_noreplace(from: &Path, to: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let f = CString::new(from.as_os_str().as_bytes()).map_err(|_| Error::State("nul in path".into()))?;
    let t = CString::new(to.as_os_str().as_bytes()).map_err(|_| Error::State("nul in path".into()))?;
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(Error::Io(std::io::Error::last_os_error()))
    }
}

/// Non-Linux unix FAILS CLOSED (review finding on the first cut, which used
/// hard_link + unlink here): that pair is not atomic, so a crash between the
/// two syscalls leaves `.open` and `.sotseg` coexisting — a state the
/// reconciliation table rightly treats as loud. Silently weaker atomicity is
/// the thing this crate exists to refuse; macOS lands with P3 alongside
/// Windows, each with a real atomic primitive (renamex_np / MoveFileExW).
#[cfg(all(unix, not(target_os = "linux")))]
pub fn rename_noreplace(_from: &Path, _to: &Path) -> Result<()> {
    Err(Error::Unsupported(
        "atomic no-clobber rename requires Linux renameat2 in v1",
    ))
}

/// Windows placeholder until the P3 implementation (`MoveFileExW` WITHOUT
/// `MOVEFILE_REPLACE_EXISTING` — an atomic no-replace rename on NTFS; std's
/// rename passes REPLACE_EXISTING and clobbers, hence not usable). This
/// exists-check + rename is NOT atomic and is reachable only by tests — the
/// store fails closed on Windows via `lock_writer` until P3.
#[cfg(not(unix))]
pub fn rename_noreplace(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "destination exists",
        )));
    }
    std::fs::rename(from, to)?;
    Ok(())
}

/// The writer fence: one kernel-held exclusive lock per platform, pinned
/// (ADR 0039). Unix: `flock(LOCK_EX | LOCK_NB)` on a persistent inode,
/// O_CLOEXEC so no child inherits it; held for the guard's lifetime.
/// Windows: `LockFileEx` exclusive, non-inheritable handle — lands with P3
/// (FE-local capsules); until then `lock_writer` FAILS CLOSED on non-unix so
/// an undurable store can never silently run.
pub struct WriterLock {
    #[allow(dead_code)] // held for its Drop (kernel releases the lock)
    file: File,
}

#[cfg(unix)]
pub fn lock_writer(lock_path: &Path) -> Result<WriterLock> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .custom_flags(libc::O_CLOEXEC)
        .open(lock_path)?;
    // Bounded retry on WouldBlock: a flock lives on the open file
    // description, and a just-dead writer's lock can outlive it by
    // milliseconds through a forked-but-not-yet-exec'd producer child (the
    // fork window closes at the child's close_range, but a SIGKILLed-and-
    // reaped capsule doesn't wait for its child's schedule). Those holds
    // resolve in ms; a GENUINE live writer holds indefinitely and still
    // fails here within the deadline — fail-closed is preserved, only the
    // transient artifact is absorbed.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    loop {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(WriterLock { file });
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::WouldBlock {
            return Err(Error::Io(e));
        }
        if std::time::Instant::now() >= deadline {
            return Err(Error::State(
                "voyage writer lock held by another process".into(),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(not(unix))]
pub fn lock_writer(_lock_path: &Path) -> Result<WriterLock> {
    Err(Error::Unsupported("voyage writer lock requires unix in v1"))
}
