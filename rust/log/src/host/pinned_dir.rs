//! The directory pin: kernel identity of a directory and a handle that holds it against rename.

use crate::Result;
use std::fs::File;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use super::{io_ctx, open_dir_handle};

/// The kernel's own identity for a directory — NOT its path. Two opens of
/// the SAME underlying object (however many path strings lead to it,
/// including a symlink/reparse-point chain) yield the same identity; two
/// opens of DIFFERENT objects yield different ones EVEN AT THE EXACT SAME
/// PATHNAME — e.g. after a rename-swap replaced whatever that pathname
/// used to name. ADR 0041 Codex round-2 (`voyage::VoyageStore::
/// prepare_root`/`open_prepared`): a canonicalized PATH STRING cannot tell
/// a same-pathname directory-entry replacement apart from an unchanged
/// directory — re-canonicalizing after such a swap returns the identical
/// string, because canonicalization only follows symlinks, and a plain
/// `rename()` swap involves none. Kernel identity is the only thing that
/// actually distinguishes the two: `(st_dev, st_ino)` on unix,
/// `(volume serial, 64-bit file index)` on Windows — both stable for the
/// lifetime of the underlying file/directory regardless of which name(s)
/// currently point at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    volume_serial: u32,
    #[cfg(windows)]
    file_index: u64,
}

/// Read `f`'s kernel identity off the HANDLE ITSELF
/// (`fstat`/`GetFileInformationByHandle`) — never by a second,
/// independent stat-by-path call, which would just reintroduce the same
/// TOCTOU a path-only check cannot close. Shared by [`dir_identity`]
/// (opens a transient handle, for `prepare_root`'s one-shot snapshot) and
/// [`PinnedDir::identity`] (reads the SAME handle the pin itself holds).
#[cfg(unix)]
fn identity_of_open_handle(f: &File) -> Result<DirIdentity> {
    use std::os::unix::fs::MetadataExt;
    let m = f.metadata()?; // fstat on the OPEN fd, not stat(2) on the path
    Ok(DirIdentity { dev: m.dev(), ino: m.ino() })
}

#[cfg(windows)]
fn identity_of_open_handle(f: &File) -> Result<DirIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    // SAFETY: `info` is a stack-local out-param, valid to write into
    // regardless of the call's outcome; `f`'s handle stays open (and thus
    // valid) for the whole call.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle(), &mut info) };
    if ok == 0 {
        let e = std::io::Error::last_os_error();
        return Err(io_ctx(e, format_args!("GetFileInformationByHandle")));
    }
    let file_index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok(DirIdentity {
        volume_serial: info.dwVolumeSerialNumber,
        file_index,
    })
}

/// Capture `dir`'s kernel identity via a TRANSIENT open — for
/// `VoyageStore::prepare_root`'s one-shot snapshot, taken (possibly) in a
/// different process from the one that later verifies it, so there is
/// nothing useful to hold open here. [`PinnedDir::open`] is the sibling
/// entry point that holds its handle instead, for a caller that needs the
/// object PINNED, not merely sampled.
#[cfg(unix)]
pub fn dir_identity(dir: &Path) -> Result<DirIdentity> {
    let f = File::open(dir)?;
    identity_of_open_handle(&f)
}

#[cfg(windows)]
pub fn dir_identity(dir: &Path) -> Result<DirIdentity> {
    let f = open_dir_handle(dir)?;
    identity_of_open_handle(&f)
}

/// A directory handle PINNED against rename/delete for as long as it is
/// held (ADR 0041 Codex round-2b). The round-2 identity check closed the
/// gap for the CHECK itself, but everything after it — the writer-lock
/// open, volume preflight, the two directory flushes, the segment
/// directory enumeration for history reconciliation — still re-opened by
/// PATH, and a live repro proved the actual exploit: a tight
/// `RENAME_EXCHANGE` loop against the prepared root let `open_prepared`
/// observe a REPLACEMENT store's `retention_class`, because a swap landing
/// AFTER the identity check but BEFORE (or between) those later opens
/// redirects every one of them just as easily as it would have redirected
/// the check itself.
///
/// The fix pins the OBJECT, not the path — every operation that must be
/// immune to a LATER swap resolves through [`pinned_path`](Self::pinned_path)
/// instead of the original argument:
///
/// - **Windows**: the pin IS the handle. Opened with
///   `FILE_FLAG_BACKUP_SEMANTICS` (required to open a directory via
///   `CreateFileW` at all) and a share mode that OMITS `FILE_SHARE_DELETE`
///   (std's own default share mode includes it — see [`open_dir_handle`],
///   which deliberately does NOT get this treatment, since its own callers
///   only ever hold their handle transiently). Microsoft's documented
///   behavior for `MoveFileExW`/`RemoveDirectoryW` is
///   `ERROR_SHARING_VIOLATION` against any object with an open handle
///   lacking that share right, and this applies to a directory opened
///   with `FILE_FLAG_BACKUP_SEMANTICS` exactly as it does to an ordinary
///   file — the IDENTICAL technique `open_lock_file`'s own writer.lock
///   handle already relies on, one level up, now applied to the
///   CONTAINING directory. A rename or an exchange targeting this
///   directory's current pathname therefore fails AT THE OS LEVEL for as
///   long as the handle stays open, so `pinned_path` here is simply the
///   real path — the OS itself is what makes reusing it safe, not a path
///   substitution. **This claim is argued from documented share-mode
///   semantics; it is not, and cannot be, exercised on this Linux
///   development machine — the windows-2022 CI leg is the standing
///   referee.**
/// - **Linux**: holding an fd does NOT block `rename(2)`/`renameat2(2)` —
///   POSIX carries no such guarantee, so the handle alone cannot protect
///   the real path the way it does on Windows. The pin is instead the
///   fd's OWN identity: `/proc/self/fd/<fd>/` is a magic symlink the
///   kernel resolves directly against the descriptor's inode, immune to
///   any later rename/exchange of the pathname that reached it.
///   `pinned_path` is exactly that string.
/// - **macOS**: no pin, and this branch does not pretend to be one. The
///   Linux arm rests on a `/proc` magic symlink the kernel resolves
///   against the DESCRIPTOR's inode; macOS has no equivalent — its
///   `/dev/fd/<n>` yields a dup of the descriptor, and traversing INTO
///   it as a path component is not a documented property of that
///   interface — and with no Mac on this bench nothing here can settle
///   that either way. An unverifiable claim is not a mechanism, so
///   `pinned_path` is the real path here and what holds this branch up
///   is an argument, stated in full:
///
///   **Which race is being argued about.** The residual is the TOCTOU
///   named at the top of this doc: a rename or exchange of the
///   CONTAINING directory's own pathname landing AFTER [`Self::open`]'s
///   identity check and BEFORE (or between) the later path-based opens.
///   It is NOT the store's own create-or-fail race: `renamex_np` with
///   `RENAME_EXCL` (`rename_noreplace_raw`'s macOS arm) plus
///   `preflight_volume`'s live probe cover that one atomically, and they
///   bear on this one not at all. This bullet's earlier wording declared
///   the branch safe *because the store failed closed on non-Linux* —
///   which the macOS store has since made false, and which was never an
///   argument about this race in the first place. The two are separate;
///   only the second is a pin's business.
///
///   **What is still checked.** Everything up to the pin, on the handle
///   and never by a second stat-by-path: `voyage.rs`'s `open_prepared`
///   compares [`Self::identity`] against the identity captured during
///   preparation and refuses on a mismatch. A swap landing before the
///   pin is therefore loud on every platform, macOS included. Unguarded
///   here is only the window from that comparison to the end of the
///   fenced open sequence.
///
///   **Why that window is accepted, not closed.** Exploiting it needs a
///   process that can rename the store root's own pathname — which means
///   write access to the root's PARENT, the state-root container this
///   crate itself only ever creates one level of, owned by the running
///   user. A process holding that access has no need of this race: it
///   can replace, empty or scribble on the store directly, and no
///   descriptor-level pin on any platform defends against that. Linux
///   takes its pin anyway because the pin costs one `format!`. The macOS
///   equivalent costs a conversion of every consumer of
///   [`Self::pinned_path`] to `*at`-relative resolution through the held
///   dirfd (`openat`/`renameat`/`fstatat`) — the real POSIX pin, and the
///   fix on the day this window stops being acceptable. **Unlike the
///   Windows bullet above, this one has no standing referee: no CI leg
///   exercises it.**
/// - **Every other unix**: unreachable. `rename_noreplace_raw` has no arm
///   outside Linux/macOS/Windows, so this crate does not build there at
///   all — there is no store to race against. `pinned_path` is the real
///   path for the same reason macOS's is.
pub struct PinnedDir {
    handle: File,
    #[cfg(not(target_os = "linux"))]
    real_path: PathBuf,
    #[cfg(target_os = "linux")]
    proc_path: PathBuf,
}

impl PinnedDir {
    /// Open and pin `dir` — the FIRST thing a caller acquires (before the
    /// writer fence, before the lease check, before any other I/O), so
    /// nothing downstream can ever run in an unpinned window.
    #[cfg(unix)]
    pub fn open(dir: &Path) -> Result<Self> {
        let handle = File::open(dir)?;
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            let fd = handle.as_raw_fd();
            let proc_path = PathBuf::from(format!("/proc/self/fd/{fd}/"));
            Ok(Self { handle, proc_path })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(Self { handle, real_path: dir.to_path_buf() })
        }
    }

    #[cfg(windows)]
    pub fn open(dir: &Path) -> Result<Self> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            // Deliberately OMITS FILE_SHARE_DELETE (std's default includes
            // it) -- see this type's own doc for the share-mode argument.
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(dir)
            .map_err(|e| io_ctx(e, format_args!("open+pin dir handle {dir:?}")))?;
        Ok(Self { handle, real_path: dir.to_path_buf() })
    }

    /// The path every subsequent operation on this directory (or anything
    /// inside it) must resolve through — see this type's own doc for why
    /// this is the real path on Windows/non-Linux-unix and the
    /// `/proc/self/fd` alias on Linux.
    pub fn pinned_path(&self) -> &Path {
        #[cfg(target_os = "linux")]
        {
            &self.proc_path
        }
        #[cfg(not(target_os = "linux"))]
        {
            &self.real_path
        }
    }

    /// This object's kernel identity, read off the SAME handle this type
    /// holds — never a fresh, independent stat-by-path.
    pub fn identity(&self) -> Result<DirIdentity> {
        identity_of_open_handle(&self.handle)
    }
}
