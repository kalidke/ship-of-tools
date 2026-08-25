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

/// Wrap an OS error with the failing op + path, preserving the `ErrorKind`
/// (callers match on it — the CAS race needs `AlreadyExists`). A bare
/// "Access is denied" from deep inside a publication sequence is
/// undiagnosable; a loud failure must say where.
#[cfg(windows)]
fn io_ctx(e: std::io::Error, what: std::fmt::Arguments<'_>) -> Error {
    Error::Io(std::io::Error::new(e.kind(), format!("{what}: {e}")))
}

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
        let e = std::io::Error::last_os_error();
        return Err(io_ctx(e, format_args!("GetVolumeInformationByHandleW {dir:?}")));
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

/// `create_dir_all` that durably ANCHORS what it creates: every created
/// level's directory entry is pinned by flushing its parent — including the
/// deepest PRE-EXISTING ancestor, which holds the first created entry.
/// Plain `create_dir_all` leaves the whole new chain cache-only, so a
/// "successful" setup could vanish on power loss. This is the container
/// half of bootstrap's contract: bootstrap refuses a missing container, and
/// the container's creator (capsule/adapter run(), or the operator) calls
/// this to create one durably.
/// Resolve a caller-supplied voyage root ONCE, and make its container exist
/// durably. Returns the absolute root that every later operation must use —
/// re-resolving a relative config path at each step lets a concurrent
/// `set_current_dir` point the existence check, the bootstrap, and the
/// fenced open at different stores.
///
/// The container (the root's parent) is created when missing and anchored in
/// the root's GRANDPARENT, which must already exist: this crate creates at
/// most one level, and whoever owns that grandparent (installer, launcher,
/// operator) owns its durability. A missing grandparent is a loud,
/// actionable error rather than a silent chain of unanchored levels.
/// Idempotent — safe (and required) to call when the store already exists.
pub fn ensure_container(root: &Path) -> Result<std::path::PathBuf> {
    let root = std::path::absolute(root)?;
    {
        let container = root
            .parent()
            .ok_or_else(|| Error::State(format!("voyage root {root:?} has no parent")))?;
        let base = container.parent().ok_or_else(|| {
            Error::State(format!("voyage container {container:?} has no parent"))
        })?;
        create_dir_all_durable(base, container)?;
    }
    Ok(root)
}

pub fn create_dir_all_durable(base: &Path, dir: &Path) -> Result<()> {
    let base = std::path::absolute(base)?;
    let dir = std::path::absolute(dir)?;
    if !base.is_dir() {
        return Err(Error::State(format!(
            "durability base {base:?} does not exist — create it before {dir:?}"
        )));
    }
    if !dir.starts_with(&base) {
        return Err(Error::State(format!("{dir:?} is not under base {base:?}")));
    }
    std::fs::create_dir_all(&dir)?;
    // Anchor UNCONDITIONALLY — never skip because the levels already exist.
    // A crash between `create_dir_all` and the last flush leaves levels that
    // are cache-visible but unanchored, and those are indistinguishable from
    // durable ones; a replaying caller that trusted the residue would leave
    // the chain losable under acknowledged writes (round-3 blocker).
    //
    // The span is PATH-derived, not existence-derived, so every replay
    // repeats exactly the same flushes. It stops at `base`, whose own
    // durability is the caller's/operator's contract — walking to the
    // filesystem root is neither needed nor possible (a standard user
    // cannot open a volume root for write on Windows).
    let mut cur = dir.as_path();
    while let Some(parent) = cur.parent() {
        fsync_dir(parent)?; // a level's entry lives in its PARENT's data
        if parent == base {
            break;
        }
        cur = parent;
    }
    Ok(())
}

/// Flush an existing file's contents by path (write-open + `sync_all`).
/// Recovery's publish-as-is rows need this: a writer killed between
/// `write_all(seal)` and its own fsync leaves a complete, cache-visible seal
/// indistinguishable from crash-after-fsync — the pinned publication order
/// (source flush BEFORE the publish rename) must be restated by whoever
/// publishes, not assumed from the dead writer.
pub fn fsync_file(path: &Path) -> Result<()> {
    let f = std::fs::OpenOptions::new().write(true).open(path)?;
    f.sync_all()?;
    Ok(())
}

/// Windows dir flush: `FlushFileBuffers` on a directory handle. NTFS
/// metadata journaling gives crash CONSISTENCY (old-or-new name, never
/// corrupt), NOT durability-at-return — the log flushes lazily, so a
/// completed publication can roll back on power cut without this. Strictly
/// stronger than what SQLite/RocksDB/PostgreSQL ship on Windows (all no-op
/// their dir fsync there). Honest scope: directory handles as
/// `FlushFileBuffers` targets are DOC-IMPLIED + empirically verified (the
/// P3 spike), not an explicit API contract — which is exactly why the
/// pinned order also flushes the renamed file itself, and why real-machine
/// power-cut testing stays on the acceptance list rather than being claimed
/// here.
///
/// Operational caveat (ADR 0041): the per-disk "turn off write-cache buffer
/// flushing" checkbox makes `FlushFileBuffers` silently vacuous —
/// undetectable from here, the peer of Linux `barrier=off`, acceptable only
/// on a UPS.
#[cfg(windows)]
pub fn fsync_dir(dir: &Path) -> Result<()> {
    let f = open_dir_handle(dir)?;
    // FlushFileBuffers on the directory handle
    f.sync_all()
        .map_err(|e| io_ctx(e, format_args!("FlushFileBuffers dir {dir:?}")))?;
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
        .open(dir)
        .map_err(|e| io_ctx(e, format_args!("open dir handle {dir:?}")))?;
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
/// existence check and the rename are ONE kernel op (no TOCTOU). On NTFS a
/// same-volume rename is journaled (old-or-new after crash) — an
/// implementation property of NTFS, not a documented `MoveFileExW`
/// contract; the preflight pinning us to NTFS is what makes relying on it
/// honest. A collision fails with ERROR_ALREADY_EXISTS, which std maps to
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
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let (f, t) = (wide_verbatim(from)?, wide_verbatim(to)?);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(RETRY_DEADLINE_MS);
    loop {
        if unsafe { MoveFileExW(f.as_ptr(), t.as_ptr(), 0) } != 0 {
            break;
        }
        let e = std::io::Error::last_os_error();
        if !is_transient_hold(&e) || std::time::Instant::now() >= deadline {
            return Err(io_ctx(e, format_args!("MoveFileExW {from:?} -> {to:?}")));
        }
        std::thread::sleep(std::time::Duration::from_millis(RETRY_STEP_MS));
    }
    flush_renamed(to)
}

/// NUL-terminated UTF-16 in extended-length form. std's own fs ops
/// verbatim-normalize long paths internally, so a store std could create
/// and write would then fail to PUBLISH through a raw `MoveFileExW` given
/// the un-prefixed path (default MAX_PATH limit). `std::path::absolute` is
/// `GetFullPathNameW`-backed on Windows (separators, dots, and
/// drive-relative forms normalized), so the remaining prefix rules mirror
/// std's own conversion: verbatim (`\\?\`) and device (`\\.\`) namespaces
/// pass through untouched; UNC gets the extended `\\?\UNC\` form (mostly
/// moot here — the preflight refuses non-local volumes); everything else
/// (drive paths) gets the plain `\\?\` prefix.
#[cfg(windows)]
fn wide_verbatim(p: &Path) -> Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let abs = std::path::absolute(p)?;
    let raw: Vec<u16> = abs.as_os_str().encode_wide().collect();
    let bs = b'\\' as u16;
    let starts = |pre: &str| {
        let pw: Vec<u16> = std::ffi::OsStr::new(pre).encode_wide().collect();
        raw.len() >= pw.len() && raw[..pw.len()] == pw[..]
    };
    let mut w: Vec<u16> = if starts(r"\\?\") || starts(r"\\.\") {
        raw
    } else if raw.starts_with(&[bs, bs]) {
        std::ffi::OsStr::new(r"\\?\UNC\")
            .encode_wide()
            .chain(raw[2..].iter().copied())
            .collect()
    } else {
        std::ffi::OsStr::new(r"\\?\")
            .encode_wide()
            .chain(raw.into_iter())
            .collect()
    };
    w.push(0);
    Ok(w)
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
    if std::fs::metadata(to)
        .map_err(|e| io_ctx(e, format_args!("stat renamed {to:?}")))?
        .is_dir()
    {
        return fsync_dir(to);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(RETRY_DEADLINE_MS);
    loop {
        match std::fs::OpenOptions::new().write(true).open(to) {
            Ok(fh) => {
                fh.sync_all()
                    .map_err(|e| io_ctx(e, format_args!("flush renamed {to:?}")))?;
                return Ok(());
            }
            Err(e) => {
                if !is_transient_hold(&e) || std::time::Instant::now() >= deadline {
                    return Err(io_ctx(e, format_args!("open renamed for flush {to:?}")));
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
            return Err(Error::State(
                "voyage writer lock held by another process".into(),
            ));
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
