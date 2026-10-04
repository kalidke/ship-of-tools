//! Volume preflight: proves the store's own primitives work on a state root's filesystem.

use crate::{Error, Result};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use super::{fsync_dir, rename_noreplace_raw};
#[cfg(windows)]
use super::{io_ctx, open_dir_handle};

/// Volume preflight (ADR 0041 Windows arm; ADR 0043 decision 23 Linux +
/// macOS arm): proves the store's OWN primitives work on `dir`'s
/// filesystem BEFORE any `.creating` mutation in bootstrap, and again on
/// the resolved voyage dir at `open_for_writing` (so the Claude producer
/// and every direct store user get it too). This checks two things and
/// ONLY two things: known EXCLUSIONS (a `statfs` denylist of remote
/// filesystem types, shared by Linux and macOS — see
/// [`remote_volume_name`]; local NTFS by allowlist on Windows) and the
/// filesystem OPERATIONS this crate actually depends on
/// (`RENAME_NOREPLACE`, a directory fsync). Durable, host-exclusive
/// backing and retention are deployment prerequisites this cannot observe
/// from a live probe (ADR 0043 decision 23's own open item 4) — proving
/// them is out of scope by design, not an oversight.
#[cfg(unix)]
pub fn preflight_volume(dir: &Path) -> Result<()> {
    if let Some(name) = remote_volume_name(dir)? {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "state root {dir:?} is on {name}: capsule records need a local filesystem \
                 (set XDG_STATE_HOME to a local disk; ADR 0043 decision 23)"
            ),
        )));
    }
    // The store's own two primitives, proven directly rather than
    // inferred from the filesystem type — tmpfs (developer `/tmp`, which
    // every suite must keep running on) is not on the denylist above but
    // DOES support both, so it passes here; a type this denylist doesn't
    // know about but that genuinely lacks RENAME_NOREPLACE fails HERE
    // instead of silently proceeding. This is also what makes a non-APFS
    // macOS volume (RENAME_EXCL is APFS-only) fail closed: there is no
    // separate macOS check for it above, because the probe below already
    // calls the real `rename_noreplace_raw` and surfaces whatever the
    // kernel refuses it with.
    let nonce = preflight_nonce();
    probe_rename_noreplace_pair(dir, PreflightEntryKind::Dir, &nonce)?;
    probe_rename_noreplace_pair(dir, PreflightEntryKind::File, &nonce)?;
    fsync_dir(dir).map_err(|e| preflight_refusal(dir, format_args!("could not fsync the directory after the probes ({e})")))?;
    Ok(())
}

/// `dir`'s filesystem type magic number (`statfs(2)`'s own `f_type`),
/// resolved on the path directly — this IS the "on the resolved
/// destination" check for a caller (`voyage.rs`) that already canonicalized
/// `dir` before calling here; `preflight_volume` does no canonicalization
/// of its own (the daemon's own `qualified_state_root` does that once,
/// ahead of calling this).
#[cfg(target_os = "linux")]
fn statfs_type(dir: &Path) -> Result<i64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c_dir =
        CString::new(dir.as_os_str().as_bytes()).map_err(|_| Error::State("nul in path".into()))?;
    let mut buf: std::mem::MaybeUninit<libc::statfs> = std::mem::MaybeUninit::uninit();
    // SAFETY: `buf` is a valid out-param for `statfs(2)`, written only on
    // success (rc == 0), which is the only case this reads it back.
    let rc = unsafe { libc::statfs(c_dir.as_ptr(), buf.as_mut_ptr()) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        return Err(Error::Io(std::io::Error::new(e.kind(), format!("statfs {dir:?}: {e}"))));
    }
    let buf = unsafe { buf.assume_init() };
    Ok(buf.f_type as i64)
}

/// Remote filesystem names `statfs(2)` can report (ADR 0043 decision
/// 23's own deny list) — ONE list shared by every unix arm, not forked
/// per platform: Linux's `statfs(2)` gives only a numeric magic, so
/// [`remote_fs_name`] keys into this by the `i64` column; macOS's own
/// `statfs(2)` gives the name directly in `f_fstypename` instead (its
/// `f_type` is NOT the stable, ABI-fixed magic Linux's is — Apple assigns
/// it per registered VFS at boot, not a documented constant — so relying
/// on it there would be inventing a second, unstable denylist rather than
/// reusing this one), so [`remote_volume_name`]'s macOS arm keys into the
/// SAME `&str` column instead, by substring. tmpfs is deliberately
/// ABSENT: developer `/tmp` is often tmpfs and every suite must keep
/// running there; the DAEMON's own, separate volatile-type refusal
/// (`capsule_workspace::qualified_state_root`) is what refuses tmpfs, on
/// the resolved destination, not this probe. Unknown types are not
/// refused by this list at all — the two `RENAME_NOREPLACE` probes below
/// are what actually decides an unlisted type.
#[cfg(unix)]
const REMOTE_FS_TYPES: &[(i64, &str)] = &[
    (0x6969, "NFS"),
    (0x517B, "SMB"),
    (0xFF534D42u32 as i64, "CIFS"),
    (0xFE534D42u32 as i64, "SMB2"),
    (0x01021997, "9p"),
    (0x65735546, "FUSE"),
];

/// Pure lookup, unit-tested directly against [`REMOTE_FS_TYPES`] — never by
/// faking `statfs`. Linux only: the `i64` it takes is a Linux `statfs(2)`
/// magic, meaningless on macOS (see [`REMOTE_FS_TYPES`]'s own doc) — that
/// arm is [`remote_volume_name`]'s macOS half instead.
#[cfg(target_os = "linux")]
fn remote_fs_name(f_type: i64) -> Option<&'static str> {
    REMOTE_FS_TYPES.iter().find(|(magic, _)| *magic == f_type).map(|(_, name)| *name)
}

/// `preflight_volume`'s own "is this a remote/incompatible filesystem"
/// check, one implementation per unix arm so each reads `dir`'s
/// filesystem type off `statfs(2)` the way ITS OWN os actually reports it
/// (see [`REMOTE_FS_TYPES`]'s own doc for why that split is real, not a
/// forked denylist).
#[cfg(target_os = "linux")]
fn remote_volume_name(dir: &Path) -> Result<Option<&'static str>> {
    Ok(remote_fs_name(statfs_type(dir)?))
}

/// macOS's `f_fstypename` is a short, null-terminated name (e.g. "apfs",
/// "nfs", "smbfs", "msdos", "macfuse") — read directly, lowercased, and
/// matched by SUBSTRING against [`REMOTE_FS_TYPES`]'s own names: macOS
/// doesn't distinguish CIFS from SMB2 the way Linux's magics do (every
/// SMB dialect mounts as "smbfs"), and third-party FUSE mounts vary their
/// exact name ("macfuse", "osxfuse", "fuse-t"), so substring-on-"FUSE" is
/// what actually catches them, not an exact match.
#[cfg(target_os = "macos")]
fn remote_volume_name(dir: &Path) -> Result<Option<&'static str>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c_dir =
        CString::new(dir.as_os_str().as_bytes()).map_err(|_| Error::State("nul in path".into()))?;
    let mut buf: std::mem::MaybeUninit<libc::statfs> = std::mem::MaybeUninit::uninit();
    // SAFETY: `buf` is a valid out-param for `statfs(2)`, written only on
    // success (rc == 0), which is the only case this reads it back.
    let rc = unsafe { libc::statfs(c_dir.as_ptr(), buf.as_mut_ptr()) };
    if rc != 0 {
        let e = std::io::Error::last_os_error();
        return Err(Error::Io(std::io::Error::new(e.kind(), format!("statfs {dir:?}: {e}"))));
    }
    let buf = unsafe { buf.assume_init() };
    let raw: Vec<u8> = buf.f_fstypename.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    let fstypename = String::from_utf8_lossy(&raw).to_ascii_lowercase();
    Ok(REMOTE_FS_TYPES
        .iter()
        .map(|&(_, name)| name)
        .find(|name| fstypename.contains(&name.to_ascii_lowercase())))
}

/// One `Error::Io(Unsupported)` shape shared by every preflight refusal —
/// the fs-type check above builds its own (the wording is pinned
/// verbatim), every probe failure below goes through this instead so a
/// caller matching on `ErrorKind::Unsupported` sees the identical kind
/// regardless of which check inside `preflight_volume` actually failed.
#[cfg(unix)]
fn preflight_refusal(dir: &Path, detail: std::fmt::Arguments<'_>) -> Error {
    Error::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "state root {dir:?}: preflight {detail} — capsule records need a local filesystem \
             (set XDG_STATE_HOME to a local disk; ADR 0043 decision 23)"
        ),
    ))
}

/// Which kind of filesystem entry a probe pair creates — a directory pair
/// and a file pair are both required (decision 23: "a temp directory pair
/// AND a temp file pair"), since a store publishes both kinds and a
/// filesystem could in principle support `RENAME_NOREPLACE` for one but
/// not the other.
#[cfg(unix)]
#[derive(Clone, Copy)]
enum PreflightEntryKind {
    Dir,
    File,
}

#[cfg(unix)]
impl PreflightEntryKind {
    fn label(self) -> &'static str {
        match self {
            PreflightEntryKind::Dir => "dir",
            PreflightEntryKind::File => "file",
        }
    }
    fn create(self, path: &Path, content: &[u8]) -> std::io::Result<()> {
        match self {
            PreflightEntryKind::Dir => std::fs::create_dir(path),
            PreflightEntryKind::File => std::fs::write(path, content),
        }
    }
    /// `true` iff `path` names a live entry of THIS kind with `content`
    /// (files only — a directory has no content to compare, only its own
    /// presence and type).
    fn intact(self, path: &Path, content: &[u8]) -> bool {
        match self {
            PreflightEntryKind::Dir => path.is_dir(),
            PreflightEntryKind::File => std::fs::read(path).map(|got| got == content).unwrap_or(false),
        }
    }
    fn remove(self, path: &Path) {
        let _ = match self {
            PreflightEntryKind::Dir => std::fs::remove_dir(path),
            PreflightEntryKind::File => std::fs::remove_file(path),
        };
    }
}

/// The probe itself (decision 23, item 2): create `a`; `rename_noreplace_raw(a,
/// b)` must be `Ok`; create `a` again; `rename_noreplace_raw(a, b)` must
/// FAIL with `AlreadyExists`, with both `a` (the fresh recreation) and `b`
/// (the original, UNCLOBBERED) still present with their own contents.
/// Temp names are removed on EVERY path — success, refusal, or an
/// unexpected outcome partway through — EXCEPT `b` when the very first
/// rename is what failed: that means `b` was already occupied by
/// something this probe never created (an unrelated pre-existing entry,
/// or — the whitebox unit test below — a deliberately seeded collision),
/// which this has no business deleting. `own_b` tracks exactly that: it
/// flips true only once THIS probe's own first rename has actually landed
/// `b`, which is also the earliest point `b` is safe to remove.
///
/// The nonce ([`preflight_nonce`]) is this process's id plus a
/// per-process sequence number: concurrent DIFFERENT processes
/// preflighting the same directory never collide, and neither do
/// concurrent THREADS of one process — two capsule rows starting at once
/// on one daemon, or the concurrent-bootstrap test — which a pid-only
/// nonce let collide on `b` ("File exists") and refuse a perfectly good
/// root. A unit test proves the seeded refusal by calling this probe with
/// a nonce of its own choosing and pre-occupying that `b`.
#[cfg(unix)]
fn probe_rename_noreplace_pair(dir: &Path, kind: PreflightEntryKind, nonce: &str) -> Result<()> {
    let (a, b) = preflight_pair_paths(dir, kind, nonce);
    let first = b"sot-preflight-first";
    let second = b"sot-preflight-second";
    let mut own_b = false;
    let result = (|| -> Result<()> {
        kind.create(&a, first)
            .map_err(|e| preflight_refusal(dir, format_args!("could not create the {} probe entry ({e})", kind.label())))?;
        rename_noreplace_raw(&a, &b)
            .map_err(|e| preflight_refusal(dir, format_args!("could not rename the {} probe entry into place ({e})", kind.label())))?;
        own_b = true;
        kind.create(&a, second).map_err(|e| {
            preflight_refusal(dir, format_args!("could not recreate a colliding {} probe entry ({e})", kind.label()))
        })?;
        match rename_noreplace_raw(&a, &b) {
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(preflight_refusal(
                    dir,
                    format_args!("a colliding {} rename failed for the wrong reason ({e})", kind.label()),
                ))
            }
            Ok(()) => {
                return Err(preflight_refusal(
                    dir,
                    format_args!(
                        "a colliding {} rename was NOT refused — capsule records need RENAME_NOREPLACE",
                        kind.label()
                    ),
                ))
            }
        }
        if !kind.intact(&a, second) || !kind.intact(&b, first) {
            return Err(preflight_refusal(
                dir,
                format_args!("a refused {} rename left an entry missing or altered", kind.label()),
            ));
        }
        Ok(())
    })();
    kind.remove(&a);
    if own_b {
        kind.remove(&b);
    }
    result
}

/// `<pid, 8 hex>-<per-process sequence, hex>`: unique per
/// [`preflight_volume`] call across processes AND threads (see
/// [`probe_rename_noreplace_pair`]'s own doc for why both matter).
#[cfg(unix)]
fn preflight_nonce() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{:08x}-{seq:x}", std::process::id())
}

/// `dir/.sot-preflight-<nonce>-<dir|file>.{a,b}` — the exact pair of
/// paths [`probe_rename_noreplace_pair`] uses for `kind` and `nonce`,
/// factored out so this module's own unit tests can reconstruct the
/// identical name and pre-seed `b` (see that function's own doc) without
/// any test-only seam into the probe itself.
#[cfg(unix)]
fn preflight_pair_paths(dir: &Path, kind: PreflightEntryKind, nonce: &str) -> (PathBuf, PathBuf) {
    let label = kind.label();
    (
        dir.join(format!(".sot-preflight-{nonce}-{label}.a")),
        dir.join(format!(".sot-preflight-{nonce}-{label}.b")),
    )
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // ADR 0043 decision 23: `preflight_volume` on Linux and macOS
    // -----------------------------------------------------------------

    #[cfg(unix)]
    fn no_preflight_residue(dir: &Path) -> bool {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .all(|e| !e.file_name().to_string_lossy().starts_with(".sot-preflight-"))
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn remote_fs_name_matches_the_deny_list_constants_only() {
        for &(magic, name) in REMOTE_FS_TYPES {
            assert_eq!(remote_fs_name(magic), Some(name));
        }
        // A magic not on the list is not refused BY THIS FUNCTION — the
        // rename/fsync probes are what judges an unlisted type.
        assert_eq!(remote_fs_name(0x0102_3456), None);
        // tmpfs is deliberately not on the deny list (developer `/tmp`).
        const TMPFS_MAGIC: i64 = 0x0102_1994;
        assert_eq!(remote_fs_name(TMPFS_MAGIC), None);
    }

    #[test]
    #[cfg(unix)]
    fn preflight_volume_passes_on_an_ordinary_tempdir_and_leaves_no_residue() {
        let dir = tempfile::tempdir().unwrap();
        preflight_volume(dir.path()).unwrap();
        assert!(no_preflight_residue(dir.path()), "preflight must remove its own temp entries");
    }

    #[test]
    #[cfg(unix)]
    fn preflight_volume_passes_on_tmpfs() {
        // tmpfs is ALLOWED here (decision 23: developer `/tmp` is often
        // tmpfs, and every suite must keep running there) — this is the
        // daemon's own, separate volatile-type refusal to make
        // (`capsule_workspace::qualified_state_root`), never this probe's.
        // Skipped, not failed, when `/dev/shm` isn't mounted — some
        // container images omit it, and macOS never mounts tmpfs there at
        // all (no `/dev/shm`), so this always skips on macOS rather than
        // asserting anything about its filesystem.
        let base = Path::new("/dev/shm");
        if !base.is_dir() {
            eprintln!("skipping preflight_volume_passes_on_tmpfs: /dev/shm is not mounted here");
            return;
        }
        let dir = tempfile::Builder::new().prefix("sot-fsutil-tmpfs-").tempdir_in(base).unwrap();
        preflight_volume(dir.path()).unwrap();
        assert!(no_preflight_residue(dir.path()), "preflight must remove its own temp entries");
    }

    /// The whitebox half of "seed `b` yourself and run the probe": this
    /// test reconstructs the exact `b` path [`probe_rename_noreplace_pair`]
    /// will use for `kind` under a nonce of the test's own choosing (the
    /// same private [`preflight_pair_paths`] the probe itself calls — no
    /// separate seam exposed for this) and pre-occupies it BEFORE calling
    /// the probe. `RENAME_NOREPLACE`'s own atomicity means the very first
    /// (otherwise-unconditional) rename now collides too, so this also
    /// proves the refusal fires even outside the probe's own
    /// self-generated second-attempt collision, and that cleanup never
    /// deletes an entry the probe did not itself create —
    /// [`probe_rename_noreplace_pair`]'s own `own_b` tracking. (The public
    /// [`preflight_volume`] mints a fresh per-call nonce precisely so no
    /// outside party can predict or collide with it.)
    #[cfg(unix)]
    fn assert_preflight_volume_refuses_a_seeded_collision(kind: PreflightEntryKind) {
        let dir = tempfile::tempdir().unwrap();
        let nonce = "seeded-test";
        let (a, b) = preflight_pair_paths(dir.path(), kind, nonce);
        let seeded = b"seeded-before-preflight-ran";
        kind.create(&b, seeded).unwrap();

        let err = probe_rename_noreplace_pair(dir.path(), kind, nonce).unwrap_err();
        assert!(format!("{err}").contains("could not rename"), "{err}");

        // Not ours to delete: the seeded entry must survive untouched.
        assert!(kind.intact(&b, seeded), "a pre-existing, un-owned `b` must survive a refused preflight");
        // But OUR OWN dangling `a` (created before the refused rename)
        // must still be cleaned up.
        assert!(!a.exists(), "preflight must still remove its own `a` even when `b` was never its own");

        kind.remove(&b);
    }

    #[test]
    #[cfg(unix)]
    fn preflight_volume_refuses_a_seeded_collision_for_the_dir_pair() {
        assert_preflight_volume_refuses_a_seeded_collision(PreflightEntryKind::Dir);
    }

    #[test]
    #[cfg(unix)]
    fn preflight_volume_refuses_a_seeded_collision_for_the_file_pair() {
        assert_preflight_volume_refuses_a_seeded_collision(PreflightEntryKind::File);
    }
}
