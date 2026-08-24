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

/// Windows cannot open a directory handle through std, and the v1 store's
/// durability contract is unix-only anyway (ADR 0039 writer fencing). The
/// no-op keeps the codec + state-machine logic testable on Windows CI
/// without claiming directory-entry durability there.
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

#[cfg(all(unix, not(target_os = "linux")))]
pub fn rename_noreplace(from: &Path, to: &Path) -> Result<()> {
    // Non-linux unix: hard-link + unlink gives no-clobber semantics.
    std::fs::hard_link(from, to)?;
    std::fs::remove_file(from)?;
    Ok(())
}

/// Windows: best-effort no-clobber (exists-check + rename). NOT atomic —
/// acceptable only because the v1 store never runs for real off unix (the
/// writer lock refuses); this keeps the state-machine tests meaningful on
/// Windows CI.
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

/// The writer fence: `flock(LOCK_EX | LOCK_NB)` on a persistent inode,
/// exactly (ADR 0039 — one primitive, pinned). O_CLOEXEC so no child can
/// inherit the lock. Held for the guard's lifetime.
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
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        return Err(if e.kind() == std::io::ErrorKind::WouldBlock {
            Error::State("voyage writer lock held by another process".into())
        } else {
            Error::Io(e)
        });
    }
    Ok(WriterLock { file })
}

#[cfg(not(unix))]
pub fn lock_writer(_lock_path: &Path) -> Result<WriterLock> {
    Err(Error::Unsupported("voyage writer lock requires unix in v1"))
}
