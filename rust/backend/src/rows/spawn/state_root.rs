//! The state root a capsule launch qualifies: its hint text, per-row state dir and refusal rules.

use std::path::{Path, PathBuf};

/// The env-var hint named in every "could not resolve this machine's
/// state root" error text (`server.rs`'s boot resume-scan and `pty.open`
/// start-on-attach; `handlers.rs`'s create/destroy/list capsule gates) —
/// ONE shared constant so the two platforms' wording can never drift out
/// of step with `sot_log::state_dir::sot_state_dir`'s own actual
/// resolution order (`%LOCALAPPDATA%` on Windows; `$XDG_STATE_HOME` or
/// `$HOME` elsewhere). LU5a: the non-Windows arm is `not(windows)`, not
/// `target_os = "linux"` — [`qualified_state_root`]'s own BODY is
/// portable (it must at least TYPECHECK on every host `mod
/// capsule_workspace` compiles for, macOS included) and references this
/// constant unconditionally, so it must exist wherever that function's
/// body does; the text is identical to what the Linux-only arm already
/// said, since `sot_state_dir`'s own resolution order is the same on
/// every non-Windows host.
#[cfg(windows)]
pub(crate) const STATE_ROOT_HINT: &str = "%LOCALAPPDATA%";
#[cfg(not(windows))]
pub(crate) const STATE_ROOT_HINT: &str = "$XDG_STATE_HOME or $HOME";

/// `<state-root>/workspaces/<workspace_id>/` — the capsule's own state
/// directory (ADR 0041/0042: `supervisor.lock`, `drawer.voyage`, the
/// journal, the voyages — all owned and created by `sot-capsule
/// supervise` itself, as its own first act after it actually runs; rule
/// C, shrink round: this daemon never creates it — a synchronous spawn
/// failure then leaves nothing on disk at all). `state_root` is
/// `sot_log::state_dir::sot_state_dir()`, injected rather than resolved
/// here so this stays a pure function of its inputs (real callers
/// resolve it once; a test supplies a tempdir root).
pub fn state_dir_for(state_root: &Path, workspace_id: &str) -> PathBuf {
    state_root.join("workspaces").join(workspace_id)
}

/// ADR 0043 decision 23: the ONE seam a capsule launch passes through
/// before it is ever allowed to touch disk on the row's behalf — called by
/// `handlers.rs`'s `workspace.create` so it can refuse an unqualified root
/// BEFORE any row persists, and again by [`runtime::spawn_detached_supervisor`]
/// — the one mechanism every later launch (attach start-on-attach, boot
/// resume, the watchdog's own restart) shares. This FUNCTION is portable
/// (no platform gate on the item
/// itself — every host `mod capsule_workspace` compiles for must be able
/// to typecheck it), but every real CALLER stays gated to Windows and
/// Linux exactly like the capsule runtime's own availability elsewhere in
/// this module — macOS never actually calls this (`allow(dead_code)`
/// below). Three steps:
///
/// 1. [`sot_log::state_dir::sot_state_dir`] resolves the root at all —
///    else the same [`STATE_ROOT_HINT`] wording every other "could not
///    resolve this machine's state root" caller already uses.
/// 2. Created if missing (this daemon's OWN private-dir helper — rule C
///    is about per-CAPSULE directories, never the daemon's own root) and
///    canonicalized: everything downstream judges the RESOLVED
///    destination, a symlink or a nested mount included, never `$HOME` by
///    inference.
/// 3. `statfs` the resolved root and refuse a root that is not durable
///    enough to keep RESUMING capsule rows from across a daemon restart
///    — a SECOND, daemon-side deny list answering a different question
///    than [`sot_log::state_dir::preflight_volume`]'s own remote-fs one:
///    that one asks whether the store's primitives work at all (tmpfs
///    passes — see its own doc); this one asks about durability, which
///    tmpfs/ramfs answer no to regardless of how well they support
///    rename/fsync. Each Unix asks it the way ITS OWN `statfs(2)`
///    actually answers, exactly as `sot_log::fsutil::remote_volume_name`
///    already splits: Linux matches the `f_type` magic against its own
///    volatile list ([`linux_only`]); macOS has no tmpfs or ramfs to
///    name at all, and a RAM disk there mounts as plain `apfs`/`hfs`, so
///    a fstype list would be an empty gesture — Darwin's own honest
///    answer is the `MNT_LOCAL` mount flag, "stored locally"
///    ([`macos_only`]), which is also the one check that catches a
///    network home whose fstype spelling `preflight_volume`'s substring
///    list happens to miss. Then `preflight_volume` itself, mapped to
///    its `Display` text. Windows: unchanged — the existing NTFS-only
///    arm already refuses everything this would and more.
pub fn qualified_state_root() -> Result<PathBuf, String> {
    let root = sot_log::state_dir::sot_state_dir()
        .ok_or_else(|| format!("could not resolve this machine's state root ({STATE_ROOT_HINT} unset)"))?;
    // Security review addendum (item 2b, v0.6.5 macOS field report): the
    // runtime dir and sockets already get `is_private_dir`'s
    // symlink/ownership/mode check; this root -- what `XDG_STATE_HOME`
    // selects -- did not, and on a shared host it can sit under a
    // world-writable sticky parent (e.g. /scratch), where an attacker-
    // precreated or symlinked directory would receive session records.
    // `secure_private_dir` is the create-or-verify guard the tmux socket
    // dir already uses for exactly this threat model (atomic 0700 create
    // when absent; symlink/owner/mode-checked, never trusted, when
    // present) -- reused here rather than a second bespoke check.
    crate::paths::secure_private_dir(&root)
        .map_err(|e| format!("could not secure the state root {root:?}: {e}"))?;
    let root = std::fs::canonicalize(&root)
        .map_err(|e| format!("could not resolve the state root {root:?}: {e}"))?;
    #[cfg(target_os = "linux")]
    {
        let f_type = linux_only::statfs_type(&root)
            .map_err(|e| format!("could not statfs the state root {root:?}: {e}"))?;
        if let Some(name) = linux_only::volatile_fs_name(f_type) {
            return Err(format!(
                "state root {root:?} is on {name}: capsule records need durable, non-volatile \
                 storage (set XDG_STATE_HOME to a local disk; ADR 0043 decision 23)"
            ));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if !macos_only::mounted_locally(&root)
            .map_err(|e| format!("could not statfs the state root {root:?}: {e}"))?
        {
            return Err(format!(
                "state root {root:?} is not on a locally attached filesystem: capsule records need \
                 durable, non-volatile storage (set XDG_STATE_HOME to a local disk; ADR 0043 decision 23)"
            ));
        }
    }
    sot_log::state_dir::preflight_volume(&root).map_err(|e| e.to_string())?;
    Ok(root)
}

/// The invariant [`qualified_state_root`] alone cannot enforce (it never
/// sees a project root): a workspace's file watcher recursively watches
/// `project_root`, holding directory handles under it, and on Windows an
/// open handle blocks a rename of the directory it is in -- so
/// `voyages\<uuid>` publication fails with a sharing violation whenever a
/// capsule's state tree sits inside a directory this daemon watches. A
/// capsule row's state directory must never lie inside (or at) its
/// project root. Each side is canonicalized first, falling back to its
/// given spelling if that fails, so a symlinked root is judged by what it
/// resolves to, not its spelling.
pub fn state_root_inside_project(state_dir: &Path, project_root: &Path) -> bool {
    let state_dir = std::fs::canonicalize(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    let project_root = std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    crate::paths::path_within_root(&state_dir, &project_root)
}

/// LU5a: the daemon's OWN volatile-filesystem deny list plus the raw
/// `statfs` call it is judged against — split into its own tiny module so
/// [`qualified_state_root`] above (portable) can gate just this one
/// `#[cfg(target_os = "linux")]` block rather than the whole function.
#[cfg(target_os = "linux")]
mod linux_only {
    use std::path::Path;

    /// Volatile filesystem magic numbers `statfs(2)` can report — refused
    /// on the state root itself regardless of how well they support the
    /// store's own primitives (tmpfs/ramfs support `RENAME_NOREPLACE`
    /// and directory fsync fine — [`sot_log::state_dir::preflight_volume`]'s
    /// own remote-fs deny list does NOT refuse them, deliberately, since
    /// developer `/tmp` is often tmpfs). A durable capsule RECORD ROOT is
    /// a different, stricter question this second list answers.
    const VOLATILE_FS_TYPES: &[(i64, &str)] = &[
        (0x0102_1994, "tmpfs"),
        (0x8584_58F6u32 as i64, "ramfs"),
    ];

    /// Pure lookup, mirroring `sot_log::fsutil`'s own `remote_fs_name` —
    /// unit-tested directly against the constants above.
    pub(super) fn volatile_fs_name(f_type: i64) -> Option<&'static str> {
        VOLATILE_FS_TYPES.iter().find(|(magic, _)| *magic == f_type).map(|(_, name)| *name)
    }

    pub(super) fn statfs_type(dir: &Path) -> std::io::Result<i64> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c_dir = CString::new(dir.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "nul byte in state root path"))?;
        let mut buf: std::mem::MaybeUninit<libc::statfs> = std::mem::MaybeUninit::uninit();
        // SAFETY: `buf` is a valid out-param for `statfs(2)`, read back
        // only once the call itself has reported success.
        let rc = unsafe { libc::statfs(c_dir.as_ptr(), buf.as_mut_ptr()) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let buf = unsafe { buf.assume_init() };
        Ok(buf.f_type as i64)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn volatile_fs_name_matches_the_constants_only() {
            assert_eq!(volatile_fs_name(0x0102_1994), Some("tmpfs"));
            assert_eq!(volatile_fs_name(0x8584_58F6u32 as i64), Some("ramfs"));
            assert_eq!(volatile_fs_name(0x6969), None, "NFS is fsutil's own remote-fs list, not this one");
        }
    }
}

/// [`linux_only`]'s Darwin twin: the same "is this root durable enough
/// to resume capsule rows from" question, asked the way macOS actually
/// answers it. There is no `f_type` magic to match (that field does not
/// exist in Darwin's `statfs`) and no tmpfs or ramfs to match it
/// against — a macOS RAM disk is `hdiutil` + `newfs_hfs`, and it mounts
/// as ordinary `apfs`/`hfs`, indistinguishable by type from the
/// internal disk. So this asks the mount flag instead: `MNT_LOCAL`, "the
/// filesystem is stored locally", which is false for exactly the mounts
/// a capsule state root must never sit on (NFS, SMB, WebDAV, AFP, a
/// `fuse-t` network bridge) and true for the internal disk and any
/// directly attached volume. Strictly wider than the fstype spelling
/// `sot_log::fsutil::remote_volume_name` matches by substring, which is
/// why it is worth having as this daemon's own second gate rather than
/// leaving the whole question to `preflight_volume`.
#[cfg(target_os = "macos")]
pub(crate) mod macos_only {
    use std::path::Path;

    /// `true` iff `dir`'s mount carries `MNT_LOCAL`. Mirrors
    /// `sot_log::fsutil`'s own macOS `statfs` call shape rather than
    /// adding a second one with different error handling.
    pub(crate) fn mounted_locally(dir: &Path) -> std::io::Result<bool> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c_dir = CString::new(dir.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "nul byte in state root path"))?;
        let mut buf: std::mem::MaybeUninit<libc::statfs> = std::mem::MaybeUninit::uninit();
        // SAFETY: `buf` is a valid out-param for `statfs(2)`, read back
        // only once the call itself has reported success.
        let rc = unsafe { libc::statfs(c_dir.as_ptr(), buf.as_mut_ptr()) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let buf = unsafe { buf.assume_init() };
        Ok((buf.f_flags & libc::MNT_LOCAL as u32) != 0)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_build_directory_is_mounted_locally() {
            // A CI runner's and a developer's checkout are both on the
            // boot volume; a state root that is NOT local is exactly what
            // `qualified_state_root` exists to refuse, so this asserts
            // the true half against a directory that certainly qualifies.
            assert!(mounted_locally(std::path::Path::new(".")).expect("statfs ."));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_joins_workspaces_and_the_id() {
        let root = Path::new("/tmp/sot-state-root");
        assert_eq!(
            state_dir_for(root, "ws-alpha-1a2b"),
            PathBuf::from("/tmp/sot-state-root/workspaces/ws-alpha-1a2b")
        );
    }

    // Field defect: a scratch daemon watched a project root that
    // CONTAINED its own state root, so the daemon's file watcher held
    // directory handles inside it; on Windows an open directory handle
    // blocks a rename, so `voyages\<uuid>` publication failed with a
    // sharing violation and the supervisor exited terminal 69. These
    // cover the predicate that now refuses that layout up front, at
    // capsule create and again right before every spawn.
    #[test]
    fn state_root_inside_project_true_when_a_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().join("project");
        let state_root = project_root.join("state").join("workspaces").join("ws-1");
        std::fs::create_dir_all(&state_root).unwrap();
        assert!(state_root_inside_project(&state_root, &project_root));
    }

    #[test]
    fn state_root_inside_project_true_when_equal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shared-root");
        std::fs::create_dir_all(&root).unwrap();
        assert!(state_root_inside_project(&root, &root));
    }

    #[test]
    fn state_root_inside_project_false_for_a_same_prefix_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().join("project");
        let state_root = dir.path().join("project-state"); // same prefix, NOT nested
        std::fs::create_dir_all(&project_root).unwrap();
        std::fs::create_dir_all(&state_root).unwrap();
        assert!(!state_root_inside_project(&state_root, &project_root));
    }

    #[test]
    #[cfg(unix)]
    fn state_root_inside_project_resolves_a_symlinked_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let real_project = dir.path().join("real-project");
        let state_root = real_project.join("state");
        std::fs::create_dir_all(&state_root).unwrap();
        let link = dir.path().join("project-link");
        std::os::unix::fs::symlink(&real_project, &link).unwrap();
        // The caller passes the SYMLINK as the project root; the state
        // root is a real descendant of what it resolves to.
        assert!(state_root_inside_project(&state_root, &link));
    }
}
