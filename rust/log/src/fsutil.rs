//! Durability primitives: volume preflight, dir fsync, no-clobber rename,
//! exclusive lock. Two real platform arms (Linux since P1, Windows since P3 —
//! ADR 0041 §store port); the pure codec compiles everywhere, but voyage
//! stores refuse to open where the OS can't guarantee these semantics
//! (non-Linux unix fails closed in `rename_noreplace_raw`).

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

/// Resolve a caller-supplied voyage root ONCE, and make its container exist
/// durably. Returns the absolute root that every later operation must use —
/// re-resolving a relative config path at each step lets a concurrent
/// `set_current_dir` point the existence check, the bootstrap, and the
/// fenced open at different stores.
///
/// This crate creates AT MOST the container level (the root's parent). The
/// container's own parent is the durability boundary: it must already exist,
/// and flushing it is exactly what anchors the container's directory entry —
/// nothing above it is ever created, walked, or flushed, since its
/// durability is the installer's/operator's contract (and a standard user
/// cannot open a volume root for write on Windows).
///
/// Idempotent, and deliberately unconditional: a container that is merely
/// cache-visible is indistinguishable from a durable one, so a replay after
/// a crash must redo the flush rather than trust the residue.
///
/// Two cases, split on the LEXICAL container's `file_name()` — which is
/// `None` exactly when a path terminates in `..`, or IS itself a root or
/// prefix (`Path`'s own definition; reused here as the discriminator rather
/// than inventing a second one):
///
/// - **Normal final component**: this is the one level we're allowed to
///   create. Canonicalize the container's own PARENT and rebuild the
///   container by appending that final component — never canonicalize the
///   lexical container directly to decide what to fsync. A container like
///   `a/b/..` (root `a/b/../v1`) lexically parents, under naive
///   `Path::parent()`, to `a/b` — a DIFFERENT directory than the semantic
///   parent of the real container `a`, which is one level further up. That
///   was the bug: fsyncing `a/b` instead of the real parent is silently
///   wrong when `b` is readable, and loudly wrong (EACCES) the moment `b` is
///   traversable-but-not-openable (execute-only, no read bit — a directory
///   you can resolve THROUGH but not open). Rebuilding from a canonicalized
///   parent sidesteps this: `canonicalize` only needs search permission on
///   ancestors, never open permission on the final one, so it resolves fine
///   through such a `b` — only ever opening it (to fsync it) was the bug.
/// - **Container ends in `..`, or is itself a volume/filesystem root**:
///   there is no level here for THIS crate to create — `..` names an
///   ancestor that must already exist, and a root always exists. Canonicalize
///   the WHOLE container (it must already exist) and anchor its entry in
///   its own resolved parent, if it has one.
///
/// REJECTED alternative: refuse a root whose container ends in `..`. This
/// was considered and dropped — the container above doesn't have to end in
/// `..` for naive `Path::parent()` to land on the wrong directory (see
/// `a/b/../c/v1`, whose container ends in the ordinary `c`), so refusing `..`
/// would not even close the bug it was proposed for; it would also break an
/// accepted CLI shape, since both callers here take arbitrary caller-supplied
/// `PathBuf`s and an existing test establishes `..` is supported.
///
/// HONESTY BOUND: this is path-bound, not object-bound. An ancestor can
/// still be swapped between the create below and the fsync that follows it;
/// strict binding would need handle-relative creation or identity checks,
/// which this does not do.
pub fn ensure_container(root: &Path) -> Result<std::path::PathBuf> {
    let abs = std::path::absolute(root)?;
    let container = abs
        .parent()
        .ok_or_else(|| Error::State(format!("voyage root {abs:?} has no parent")))?;
    let name = abs
        .file_name()
        .ok_or_else(|| Error::State(format!("voyage root {abs:?} has no final component")))?;

    match container.file_name() {
        // `..` or a volume/filesystem root: nothing to create, only
        // something to resolve (it must already exist) and anchor.
        None => {
            let resolved = std::fs::canonicalize(container).map_err(|e| {
                Error::State(format!("voyage container {container:?} must exist first: {e}"))
            })?;
            if let Some(base) = resolved.parent() {
                fsync_dir(base)?; // anchors the container's own entry
            }
            Ok(resolved.join(name))
        }
        // Normal final component: the one level this crate may create.
        Some(final_component) => {
            let lexical_parent = container.parent().ok_or_else(|| {
                Error::State(format!("voyage container {container:?} has no parent"))
            })?;
            let base = std::fs::canonicalize(lexical_parent).map_err(|e| {
                Error::State(format!(
                    "voyage container's parent {lexical_parent:?} must exist first: {e}"
                ))
            })?;
            let reconstructed = base.join(final_component);
            match std::fs::create_dir(&reconstructed) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // AlreadyExists names SOME entity, often a file — not a
                    // sufficient idempotence discriminator by itself.
                    // `create_dir_all` as a fallback was rejected: if `base`
                    // vanished concurrently, `create_dir_all` could recreate
                    // more than the one permitted level (ADR 0039's bootstrap
                    // step: one level is the explicit contract, and no
                    // caller here needs a missing grandparent).
                    if !std::fs::metadata(&reconstructed)?.is_dir() {
                        return Err(Error::State(format!(
                            "voyage container path {reconstructed:?} exists and is not a directory"
                        )));
                    }
                }
                Err(e) => return Err(e.into()),
            }
            fsync_dir(&base)?; // anchors the container's entry
            // Canonicalize the RECONSTRUCTED path, never the original
            // lexical container: re-resolving the lexical alias here would
            // re-walk through the same intermediate and reopen the race
            // this function exists to close.
            Ok(std::fs::canonicalize(&reconstructed)?.join(name))
        }
    }
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
pub fn rename_noreplace_raw(from: &Path, to: &Path) -> Result<()> {
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
pub fn rename_noreplace_raw(_from: &Path, _to: &Path) -> Result<()> {
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
/// Windows-only extra, pinned in the ADR: bounded retry on
/// `ERROR_SHARING_VIOLATION` and spurious `ERROR_ACCESS_DENIED` from
/// AV/indexer holders (rust-lang/rust#123985) — a transient with no Linux
/// analog; a persistent holder still fails at the deadline.
///
/// This is the raw rename ONLY. The renamed-target flush that used to run
/// here on success moved out to `finish_publication`, so the reconciliation
/// rows can invoke the SAME flush over a target this process never itself
/// renamed — residue a prior incarnation renamed into place before crashing.
/// Every ordinary caller gets both steps via `publish_noreplace`.
#[cfg(windows)]
pub fn rename_noreplace_raw(from: &Path, to: &Path) -> Result<()> {
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let (f, t) = (wide_verbatim(from)?, wide_verbatim(to)?);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(RETRY_DEADLINE_MS);
    loop {
        if unsafe { MoveFileExW(f.as_ptr(), t.as_ptr(), 0) } != 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if !is_transient_hold(&e) || std::time::Instant::now() >= deadline {
            return Err(io_ctx(e, format_args!("MoveFileExW {from:?} -> {to:?}")));
        }
        std::thread::sleep(std::time::Duration::from_millis(RETRY_STEP_MS));
    }
}

/// Complete the publication barrier for `target`, independent of whether
/// THIS call is what just renamed it there. On Windows: flush the renamed
/// target itself (belt-and-braces — the doc-implied corner of the
/// directory-flush contract). On every platform: fsync `target`'s parent,
/// which anchors its directory entry (ADR 0039/0041's publication order —
/// source flush → rename → renamed-file flush → parent-directory flush; the
/// first is per-source and stays at each call site, the last two are
/// exactly this function).
///
/// Reconciliation rows call this DIRECTLY, with no rename alongside it, on
/// content that arrived some other way: a `.sotseg` a prior incarnation
/// renamed into place but crashed before flushing, an already-published CAS
/// blob this process only verified matches. The barrier is what makes a
/// publication durable, not the rename syscall by itself — finding the
/// target already there is exactly the case that needs restating, not
/// skipping.
pub fn finish_publication(target: &Path) -> Result<()> {
    #[cfg(windows)]
    flush_renamed(target)?;
    let parent = target
        .parent()
        .ok_or_else(|| Error::State(format!("publication target {target:?} has no parent")))?;
    fsync_dir(parent)
}

/// The common case: atomic no-clobber rename, then complete the barrier.
/// Every ordinary publish site uses this instead of pairing the raw rename
/// with its own `fsync_dir(parent)` call.
pub fn publish_noreplace(source: &Path, target: &Path) -> Result<()> {
    rename_noreplace_raw(source, target)?;
    finish_publication(target)
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

/// Flush a target `finish_publication` is restating the barrier over — the
/// name may be freshly renamed by this process or residue from an earlier
/// one; either way `FlushFileBuffers` needs write access, so files are
/// briefly reopened for write (same bounded transient-hold retry as the
/// rename — the fresh name is exactly what AV scans). A directory target
/// (bootstrap's `.creating` publish) flushes via the dir handle.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_container_creates_one_level_and_replays() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voyages").join("v1");
        let got = ensure_container(&root).unwrap();
        assert_eq!(got, std::fs::canonicalize(dir.path()).unwrap().join("voyages").join("v1"));
        assert!(dir.path().join("voyages").is_dir());
        // The ROOT is bootstrap's job, never this helper's.
        assert!(!root.exists());
        // Replay after a crash must redo the anchoring, not trust residue.
        ensure_container(&root).unwrap();
    }

    #[test]
    fn ensure_container_refuses_an_unanchorable_boundary() {
        let dir = tempfile::tempdir().unwrap();
        // The container's parent is missing too: creating BOTH levels would
        // leave the outer one unanchored, so this is loud by design.
        let root = dir.path().join("a").join("b").join("v1");
        let e = ensure_container(&root).unwrap_err();
        assert!(format!("{e}").contains("must exist first"), "{e}");
    }

    #[test]
    fn ensure_container_resolves_dot_dot_without_escaping() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a").join("b")).unwrap();
        let root = dir.path().join("a").join("b").join("..").join("c").join("v1");
        let got = ensure_container(&root).unwrap();
        assert!(dir.path().join("a").join("c").is_dir());
        // The returned identity is fully resolved — no `..` left to alias.
        assert_eq!(got, std::fs::canonicalize(dir.path()).unwrap().join("a").join("c").join("v1"));
    }

    /// The real repro (round-5 finding): a container whose lexical parent
    /// (under naive `Path::parent()`, which just strips the raw `..`
    /// component) is NOT its semantic parent must never be opened for
    /// fsync. `a/b` is execute-only here — traversable (canonicalize can
    /// walk THROUGH it) but not openable (`File::open` needs the read bit
    /// `b` doesn't have) — so this fails loudly on the old code, which
    /// fsyncs `a/b` instead of the real container `a`'s parent (this
    /// tempdir). Permissions are restored by a guard that runs before the
    /// tempdir's own cleanup, even if an assertion below panics — otherwise
    /// a failing run leaves an undeletable directory behind.
    #[test]
    #[cfg(unix)]
    fn trailing_dotdot_flushes_semantic_parent() {
        use std::os::unix::fs::PermissionsExt;

        struct RestorePerms(std::path::PathBuf, std::fs::Permissions);
        impl Drop for RestorePerms {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, self.1.clone());
            }
        }

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a").join("b")).unwrap();
        let b = dir.path().join("a").join("b");
        let original_perms = std::fs::metadata(&b).unwrap().permissions();
        // Declared AFTER `dir`, so it drops (and restores permissions)
        // BEFORE `dir`'s own `Drop` tries to `remove_dir_all` it.
        let _restore = RestorePerms(b.clone(), original_perms);
        std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o111)).unwrap();

        // Semantic container of `a/b/../v1` is `a`, whose entry lives in
        // this tempdir — never `b`.
        let root = dir.path().join("a").join("b").join("..").join("v1");
        let got = ensure_container(&root).unwrap();
        assert_eq!(
            got,
            std::fs::canonicalize(dir.path()).unwrap().join("a").join("v1")
        );
    }
}
