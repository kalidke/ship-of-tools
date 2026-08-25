//! Durability primitives: volume preflight, dir fsync, no-clobber rename,
//! exclusive lock. Two real platform arms (Linux since P1, Windows since P3 —
//! ADR 0041 §store port); the pure codec compiles everywhere, but voyage
//! stores refuse to open where the OS can't guarantee these semantics
//! (non-Linux unix fails closed in `rename_noreplace`).

use crate::{Error, Result};
use std::fs::File;
use std::path::Path;

/// Bounded-retry deadline shared by every transient-absorbing loop here:
/// long enough to outlive an AV/indexer hold or a just-released kernel
/// lock, short enough that a persistent condition still fails loudly.
const RETRY_DEADLINE_MS: u64 = 250;
const RETRY_STEP_MS: u64 = 10;

/// Volume preflight (ADR 0041): the durability contract holds on local NTFS
/// only — the Windows mirror of "requires renameat2". Runs BEFORE any
/// `.creating` mutation in bootstrap and again on the resolved voyage dir at
/// `open_for_writing`. SMB failing the handle-info call is usefully
/// fail-closed; no fallback. ReFS stays refused until it passes the same
/// suite (ADR 0041 scope). On unix this is a no-op: `renameat2` itself
/// refusing (EINVAL on filesystems without RENAME_NOREPLACE) is the guard.
#[cfg(unix)]
pub fn preflight_volume(_dir: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
pub fn preflight_volume(dir: &Path) -> Result<()> {
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationByHandleW;
    let f = open_dir_handle(dir)?;
    let mut fs_name = [0u16; 64];
    let ok = unsafe {
        use std::os::windows::io::AsRawHandle;
        GetVolumeInformationByHandleW(
            f.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        // SMB and friends commonly fail this call outright — exactly the
        // fail-closed we want (voyages are local-FS pinned).
        return Err(Error::Io(std::io::Error::last_os_error()));
    }
    let len = fs_name.iter().position(|&c| c == 0).unwrap_or(fs_name.len());
    let name = String::from_utf16_lossy(&fs_name[..len]);
    if !name.eq_ignore_ascii_case("NTFS") {
        return Err(Error::State(format!(
            "volume preflight: filesystem {name:?} at {dir:?} — voyage stores require local NTFS (ADR 0041)"
        )));
    }
    Ok(())
}

#[cfg(unix)]
pub fn fsync_dir(dir: &Path) -> Result<()> {
    let f = File::open(dir)?;
    f.sync_all()?;
    Ok(())
}

/// Windows dir flush: `FlushFileBuffers` on a directory handle. NTFS
/// metadata journaling gives crash CONSISTENCY (old-or-new name, never
/// corrupt), NOT durability-at-return — the log flushes lazily, so a
/// completed publication can roll back on power cut without this. Strictly
/// stronger than what SQLite/RocksDB/PostgreSQL ship on Windows (all no-op
/// their dir fsync there); our contract needs the real flush.
///
/// Operational caveat (ADR 0041): the per-disk "turn off write-cache buffer
/// flushing" checkbox makes `FlushFileBuffers` silently vacuous —
/// undetectable from here, the peer of Linux `barrier=off`, acceptable only
/// on a UPS.
#[cfg(windows)]
pub fn fsync_dir(dir: &Path) -> Result<()> {
    let f = open_dir_handle(dir)?;
    f.sync_all()?; // FlushFileBuffers on the directory handle
    Ok(())
}

/// Open a directory handle usable for `FlushFileBuffers` and volume info:
/// `FILE_FLAG_BACKUP_SEMANTICS` is what makes `CreateFileW` open a
/// directory at all; write access is required by `FlushFileBuffers` (on a
/// directory it maps to FILE_ADD_FILE — grantable). Std's default share
/// mode (read|write|delete) is right for a short-lived flush handle.
#[cfg(windows)]
fn open_dir_handle(dir: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)?;
    Ok(f)
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
/// the thing this crate exists to refuse; macOS gets a real arm
/// (renamex_np) when a macOS FE exists to dogfood it (ADR 0041 scope note).
#[cfg(all(unix, not(target_os = "linux")))]
pub fn rename_noreplace(_from: &Path, _to: &Path) -> Result<()> {
    Err(Error::Unsupported(
        "atomic no-clobber rename requires Linux renameat2 in v1",
    ))
}

/// Windows arm (ADR 0041 §store port): `MoveFileExW` with flags 0 —
/// kernel `FileRenameInformation` with ReplaceIfExists=FALSE, so the
/// existence check and the rename are ONE kernel op (no TOCTOU) and a
/// same-volume rename is one $LogFile transaction (old-or-new after crash).
/// A collision fails with ERROR_ALREADY_EXISTS, which std maps to
/// `ErrorKind::AlreadyExists` — the same loud condition callers match on.
/// `std::fs::rename` is unusable here: it passes REPLACE_EXISTING and
/// clobbers.
///
/// Two Windows-only extras, both pinned in the ADR:
/// - Bounded retry on `ERROR_SHARING_VIOLATION` and spurious
///   `ERROR_ACCESS_DENIED` from AV/indexer holders (rust-lang/rust#123985)
///   — a transient with no Linux analog; a persistent holder still fails at
///   the deadline.
/// - Belt-and-braces flush of the RENAMED target after success (the
///   doc-implied corner of the directory-flush contract); the caller's
///   parent-dir flush then completes the pinned publication order
///   (source flush → rename → renamed-file flush → dir flush).
#[cfg(windows)]
pub fn rename_noreplace(from: &Path, to: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }
    let (f, t) = (wide(from), wide(to));
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(RETRY_DEADLINE_MS);
    loop {
        if unsafe { MoveFileExW(f.as_ptr(), t.as_ptr(), 0) } != 0 {
            break;
        }
        let e = std::io::Error::last_os_error();
        if !is_transient_hold(&e) || std::time::Instant::now() >= deadline {
            return Err(Error::Io(e));
        }
        std::thread::sleep(std::time::Duration::from_millis(RETRY_STEP_MS));
    }
    flush_renamed(to)
}

/// AV/indexer transient hold codes worth absorbing (bounded).
#[cfg(windows)]
fn is_transient_hold(e: &std::io::Error) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};
    matches!(e.raw_os_error(),
        Some(c) if c == ERROR_SHARING_VIOLATION as i32 || c == ERROR_ACCESS_DENIED as i32)
}

/// Flush the just-renamed target. `FlushFileBuffers` needs write access, so
/// files are briefly reopened for write (same bounded transient-hold retry
/// as the rename — the fresh name is exactly what AV scans). A directory
/// target (bootstrap's `.creating` publish) flushes via the dir handle.
#[cfg(windows)]
fn flush_renamed(to: &Path) -> Result<()> {
    if std::fs::metadata(to)?.is_dir() {
        return fsync_dir(to);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(RETRY_DEADLINE_MS);
    loop {
        match std::fs::OpenOptions::new().write(true).open(to) {
            Ok(fh) => {
                fh.sync_all()?;
                return Ok(());
            }
            Err(e) => {
                if !is_transient_hold(&e) || std::time::Instant::now() >= deadline {
                    return Err(Error::Io(e));
                }
                std::thread::sleep(std::time::Duration::from_millis(RETRY_STEP_MS));
            }
        }
    }
}

/// The writer fence: one kernel-held exclusive lock, pinned (ADR 0039).
/// Both platform arms collapse into one std call (`File::try_lock`, Rust ≥
/// 1.89): `flock(LOCK_EX | LOCK_NB)` on unix, `LockFileEx(EXCLUSIVE |
/// FAIL_IMMEDIATELY)` on Windows. Released by the kernel when the guard's
/// handle closes — including on hard kills, with a documented timing
/// transient on both platforms that the bounded retry absorbs.
pub struct WriterLock {
    #[allow(dead_code)] // held for its Drop (kernel releases the lock)
    file: File,
}

pub fn lock_writer(lock_path: &Path) -> Result<WriterLock> {
    let file = open_lock_file(lock_path)?;
    // Bounded retry on WouldBlock: a just-dead writer's lock can outlive it
    // by milliseconds — on unix through a forked-but-not-yet-exec'd producer
    // child holding the open file description (the window closes at the
    // child's close_range, but a SIGKILLed-and-reaped capsule doesn't wait
    // for its child's schedule); on Windows through the kernel's own
    // documented post-kill release lag. Those holds resolve in ms; a GENUINE
    // live writer holds indefinitely and still fails here within the
    // deadline — fail-closed is preserved, only the transient is absorbed.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(RETRY_DEADLINE_MS);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(WriterLock { file }),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(e)) => return Err(Error::Io(e)),
        }
        if std::time::Instant::now() >= deadline {
            return Err(Error::State(
                "voyage writer lock held by another process".into(),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(RETRY_STEP_MS));
    }
}

#[cfg(unix)]
fn open_lock_file(lock_path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .custom_flags(libc::O_CLOEXEC) // no child inherits the fence
        .open(lock_path)?;
    Ok(f)
}

/// Windows lock open (ADR 0041): open-existing ONLY — never silently
/// recreate a missing persistent fence (bootstrap created it; absence means
/// a mutilated store). Opened WITHOUT `FILE_SHARE_DELETE` (std's default
/// shares delete/rename) so the locked path cannot be replaced out from
/// under the fence to mint a second one. Std never makes handles
/// inheritable.
#[cfg(windows)]
fn open_lock_file(lock_path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(lock_path)?;
    Ok(f)
}
