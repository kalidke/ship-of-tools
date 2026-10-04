//! Durability primitives: volume preflight, dir fsync, no-clobber rename,
//! exclusive lock. Three real platform arms (Linux since P1, Windows since
//! P3, macOS since the M1 capsule-lane port — ADR 0041 §store port); the
//! pure codec compiles everywhere, but `rename_noreplace_raw`/
//! `preflight_volume` have no fourth arm — every other unix does not build
//! this crate at all, rather than carry a stub that fails closed at
//! runtime.

use crate::{Error, Result};
#[cfg(windows)]
use super::{io_ctx, owner_protected_descriptor, RETRY_DEADLINE_MS, RETRY_STEP_MS};
use std::fs::File;
use std::path::Path;

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

/// Create `path` as a directory that is born with its final protection —
/// ADR 0041's "attach protocol" §Security split: "never create-then-repair".
/// Used ONLY for the voyage staging root (bootstrap's `.creating`); every
/// interior directory and file created inside it afterward (`seg/`,
/// `blobs/`, `writer.lock`, ...) uses plain creation and INHERITS the
/// protection — Windows ACE inheritance cascades through arbitrary depth
/// on its own, so nothing else needs per-file work, and unix has no
/// equivalent step at all (see the Windows arm's doc for why).
///
/// Deliberately NOT tolerant of an existing directory: the caller
/// (bootstrap) removes crashed-attempt residue first, so the path being
/// present here means a CONCURRENT bootstrap of the same root — and failing
/// loudly now is strictly better than the alternative this replaced, two
/// bootstraps interleaving writes into one shared staging directory.
#[cfg(unix)]
pub fn create_dir_protected(path: &Path) -> Result<()> {
    // Unix's protection is the existing owner-only file modes (umask +
    // ownership) — not in scope here; ADR 0041's DACL work is Windows-only,
    // so this arm is a plain create.
    std::fs::create_dir(path)?;
    Ok(())
}

/// Windows arm: `CreateDirectoryW` with `SECURITY_ATTRIBUTES` carrying a
/// descriptor for the STABLE ACCOUNT SID (the token user — explicitly NOT
/// the logon SID, which differs per logon session and would strand voyages
/// at reboot), `SE_DACL_PROTECTED` (a permissive parent directory can never
/// inject ACEs into this tree), one ACE granting the trustee full access
/// with OBJECT_INHERIT + CONTAINER_INHERIT (the whole tree inherits without
/// per-file work). Threat model: other local users and anonymous access,
/// not the owner. The subsequent staging→root publish is a same-volume
/// rename (`publish_noreplace`), which preserves the security descriptor —
/// so protecting `.creating` at birth protects the voyage forever.
#[cfg(windows)]
pub fn create_dir_protected(path: &Path) -> Result<()> {
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;

    let descriptor = owner_protected_descriptor()?;
    let sa = windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_ptr(),
        bInheritHandle: 0,
    };
    let wide = wide_verbatim(path)?;
    if unsafe { CreateDirectoryW(wide.as_ptr(), &sa) } != 0 {
        return Ok(());
    }
    // AlreadyExists included: see the unix arm's doc — residue is the
    // caller's to remove, so presence here means a concurrent bootstrap.
    Err(io_ctx(
        std::io::Error::last_os_error(),
        format_args!("CreateDirectoryW {path:?}"),
    ))
    // `descriptor` drops here (after the call, whichever path returns),
    // freeing the LocalAlloc'd security descriptor: CreateDirectoryW copies
    // what it needs into the new object's own security descriptor at
    // creation time, so nothing above retains a live reference into it.
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
pub(super) fn open_dir_handle(dir: &Path) -> Result<File> {
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
    // Raw syscall, not `libc::renameat2`: the wrapper exists only in the
    // glibc bindings, and the release build targets musl (the backend has
    // depended on this crate since ADR 0042 L1a). Same shape `voyage.rs`
    // already uses for RENAME_EXCHANGE.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE as libc::c_uint,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(Error::Io(std::io::Error::last_os_error()))
    }
}

/// macOS arm (ADR 0041 §store port; ADR 0039 rename rule): `renamex_np`
/// with `RENAME_EXCL` is the kernel's own atomic no-replace rename, same
/// shape as Linux's `renameat2` above — the existence check and the
/// rename are ONE kernel op, so a collision surfaces as the same
/// `AlreadyExists` a caller already matches on (macOS reports it as
/// `EEXIST`, exactly like Linux's `RENAME_NOREPLACE`). Guaranteed atomic
/// only on APFS; a volume that can't honor `RENAME_EXCL` fails this call
/// with `ENOTSUP`/`EINVAL` rather than silently falling back to a
/// non-atomic rename — `preflight_volume`'s probe below calls this SAME
/// function for real, so that failure is what turns "not actually atomic
/// here" into a loud refusal before any store opens.
#[cfg(target_os = "macos")]
pub fn rename_noreplace_raw(from: &Path, to: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let f = CString::new(from.as_os_str().as_bytes()).map_err(|_| Error::State("nul in path".into()))?;
    let t = CString::new(to.as_os_str().as_bytes()).map_err(|_| Error::State("nul in path".into()))?;
    // SAFETY: `f` and `t` are valid, NUL-terminated C strings for the
    // duration of this call; `renamex_np` only reads them.
    let rc = unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(Error::Io(std::io::Error::last_os_error()))
    }
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
///
/// `ERROR_PATH_NOT_FOUND` was added here in an earlier round on an AV-scan
/// hypothesis for a `reconcile_reset` CI failure, then REVERTED (Codex
/// review round 1, finding 11): that failure reproduced deterministically
/// across repeated real-CI runs, which disproves a transient/AV cause —
/// code 3 is PATH not FILE, meaning a real code path was constructing a
/// path whose parent directory never existed. Broadening the transient
/// set papered over that with a bounded retry-then-fail-anyway instead of
/// fixing the actual missing-directory bug (see `supervisor.rs`'s own
/// fix). `ERROR_SHARING_VIOLATION` and `ERROR_ACCESS_DENIED` remain: both
/// are genuinely transient AV/indexer holds on a name this process itself
/// just created (rust-lang/rust#123985), unrelated to path existence.
#[cfg(windows)]
fn is_transient_hold(e: &std::io::Error) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};
    matches!(e.raw_os_error(),
        Some(c) if c == ERROR_SHARING_VIOLATION as i32
            || c == ERROR_ACCESS_DENIED as i32)
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
