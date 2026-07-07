// file_io.rs — raw source-file read/write for the in-frontend editor.
//
// The kernel renders *previews*; this is the *editor's* path: it shuttles raw
// file bytes plus a content version, so a save can detect that the file changed
// underfoot (another writer, the file watcher, git, $EDITOR) since it was read.
// Pure backend IO — no Julia, no kernel. Mirrors the optimistic-concurrency
// shape of `concept.rs` (which guards on a `synced_against` AST hash), but for
// arbitrary source files the guard is a hash of the exact bytes.
//
// Version = FNV-1a 64-bit over the bytes, hex. Deterministic and dependency-
// free — and crucially *stable across backend rebuilds* (unlike
// `DefaultHasher`), so a version handed to the frontend before a restart still
// compares correctly after one.

use std::io::Write as _;
use std::path::Path;

use anyhow::{anyhow, Context, Result};

/// FNV-1a 64-bit hash of `bytes`, lowercase zero-padded hex.
pub fn content_version(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// A successful read: the file's text + its current version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadResult {
    pub content: String,
    pub version: String,
}

/// Outcome of a write attempt under optimistic concurrency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteResult {
    /// Written to disk. Carries the new content version.
    Written { version: String },
    /// Refused: the on-disk file no longer matches the `expected` version
    /// (it changed since it was read). Carries the current on-disk content +
    /// version so the caller can surface a reconcile/diff instead of clobbering.
    Conflict {
        current_content: String,
        current_version: String,
    },
}

/// Read a UTF-8 text file. `Ok(None)` when the file doesn't exist; `Err` on any
/// other IO error or on non-UTF-8 bytes (the editor is text-only).
pub fn read_file(abs: &Path) -> Result<Option<ReadResult>> {
    match std::fs::read(abs) {
        Ok(bytes) => {
            let version = content_version(&bytes);
            let content = String::from_utf8(bytes)
                .map_err(|_| anyhow!("file is not valid UTF-8: {}", abs.display()))?;
            Ok(Some(ReadResult { content, version }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow!("read {}: {e}", abs.display())),
    }
}

/// Write `content` to `abs` with optimistic concurrency.
///
/// - `expected = Some(v)` — the current on-disk version must equal `v`, else
///   `Conflict` and **no write**. A missing file counts as version `""`, so a
///   file that vanished after you read it is a conflict (not a silent recreate).
/// - `expected = None` — create/overwrite unconditionally (new file, or an
///   explicit force-save).
///
/// On a match (or `None`) the write is atomic: a sibling temp file is written
/// then renamed over the target, so a crash mid-write never truncates the
/// original. Parent directories are created as needed.
pub fn write_file(abs: &Path, content: &str, expected: Option<&str>) -> Result<WriteResult> {
    if let Some(expected) = expected {
        let current = match std::fs::read(abs) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(anyhow!("read {} (conflict check): {e}", abs.display())),
        };
        let current_version = current.as_deref().map(content_version).unwrap_or_default();
        if current_version != expected {
            let current_content = current
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            return Ok(WriteResult::Conflict {
                current_content,
                current_version,
            });
        }
    }
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let tmp = abs.with_extension("sot-tmp");
    // TOCTOU guard (security review): the confinement check that got the
    // caller `abs` in the first place only proves the path resolved inside
    // the workspace root AT CHECK TIME. Between then and here, a writer with
    // access to this same workspace directory could plant a symlink at
    // `tmp`'s path pointing anywhere this process can write (e.g. a dotfile
    // outside the workspace). Plain `std::fs::write` uses `File::create`
    // semantics, which FOLLOWS an existing symlink and would happily write
    // `content` through it. `O_NOFOLLOW` makes that open fail instead
    // (typically `ELOOP`) rather than silently escaping through the symlink.
    // The following `rename` does NOT need the same treatment: POSIX
    // `rename(2)` never dereferences its destination — if `abs` itself were
    // a symlink, rename replaces that directory entry atomically rather than
    // writing through to whatever it points at, so the existing call is
    // already safe.
    //
    // Scope of the guarantee: exploiting this requires the attacker to
    // already have WRITE access to this workspace directory (to race the
    // symlink into place before this open) — a foreign local user does NOT
    // have that under normal `0700`/`0755` workspace-root permissions. This
    // closes the window for a same-workspace-writable attacker (e.g. a
    // shared/group-writable workspace, or a race with another Ship of Tools
    // operation); it is not a claim of an absolute guarantee against every
    // possible permission configuration.
    let mut open_opts = std::fs::OpenOptions::new();
    open_opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open_opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut tmp_file = open_opts
        .open(&tmp)
        .with_context(|| format!("open {}", tmp.display()))?;
    tmp_file
        .write_all(content.as_bytes())
        .with_context(|| format!("write {}", tmp.display()))?;
    drop(tmp_file);
    std::fs::rename(&tmp, abs)
        .with_context(|| format!("rename {} -> {}", tmp.display(), abs.display()))?;
    Ok(WriteResult::Written {
        version: content_version(content.as_bytes()),
    })
}

/// Move `abs` to trash — never hard-unlinks (the file.delete v1 contract:
/// every delete is recoverable). System trash first (`gio trash`); a missing
/// gio or non-zero exit routes to the in-workspace fallback. Returns `None`
/// for system trash, `Some(destination)` for the fallback — the caller
/// surfaces which path was taken (no quiet substitution).
pub fn trash_file(abs: &Path, workspace_root: &Path) -> Result<Option<std::path::PathBuf>> {
    match std::process::Command::new("gio").arg("trash").arg(abs).status() {
        Ok(status) if status.success() => return Ok(None),
        _ => {}
    }
    trash_file_fallback(abs, workspace_root).map(Some)
}

/// The no-gio path: move into `<workspace_root>/.sot-trash/<unix-ts>-<name>`,
/// suffixing `-N` on collision. Same filesystem as the source (both under the
/// workspace root), so this is a rename, not a copy.
fn trash_file_fallback(abs: &Path, workspace_root: &Path) -> Result<std::path::PathBuf> {
    let trash_dir = workspace_root.join(".sot-trash");
    std::fs::create_dir_all(&trash_dir)
        .with_context(|| format!("create_dir_all {}", trash_dir.display()))?;
    let name = abs
        .file_name()
        .ok_or_else(|| anyhow!("no file name in {}", abs.display()))?
        .to_string_lossy()
        .into_owned();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut dest = trash_dir.join(format!("{ts}-{name}"));
    let mut n = 1u32;
    while dest.exists() {
        dest = trash_dir.join(format!("{ts}-{n}-{name}"));
        n += 1;
    }
    std::fs::rename(abs, &dest)
        .with_context(|| format!("rename {} -> {}", abs.display(), dest.display()))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Per-test scratch dir under the OS temp dir, removed on drop. Named by the
    /// test so parallel tests in this binary don't collide.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("sot_fileio_{}_{}", std::process::id(), name));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
        fn path(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn version_is_stable_and_content_sensitive() {
        assert_eq!(content_version(b"hello"), content_version(b"hello"));
        assert_ne!(content_version(b"hello"), content_version(b"hellp"));
        // Known FNV-1a/64 anchor so a refactor can't silently change the scheme.
        assert_eq!(content_version(b""), "cbf29ce484222325");
    }

    #[test]
    fn read_missing_is_none() {
        let s = Scratch::new("read_missing");
        assert!(read_file(&s.path("nope.rs")).unwrap().is_none());
    }

    #[test]
    fn read_roundtrips_content_and_version() {
        let s = Scratch::new("read_roundtrip");
        let p = s.path("a.txt");
        std::fs::write(&p, "abc").unwrap();
        let r = read_file(&p).unwrap().unwrap();
        assert_eq!(r.content, "abc");
        assert_eq!(r.version, content_version(b"abc"));
    }

    #[test]
    fn read_non_utf8_errors() {
        let s = Scratch::new("read_binary");
        let p = s.path("b.bin");
        std::fs::write(&p, [0xff, 0xfe, 0x00]).unwrap();
        assert!(read_file(&p).is_err());
    }

    #[test]
    fn write_new_file_unconditional() {
        let s = Scratch::new("write_new");
        let p = s.path("sub/dir/new.txt"); // parents created
        let res = write_file(&p, "hi", None).unwrap();
        match res {
            WriteResult::Written { version } => assert_eq!(version, content_version(b"hi")),
            _ => panic!("expected Written"),
        }
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hi");
    }

    #[test]
    fn write_with_matching_version_succeeds_and_bumps() {
        let s = Scratch::new("write_match");
        let p = s.path("a.txt");
        std::fs::write(&p, "v1").unwrap();
        let v1 = content_version(b"v1");
        let res = write_file(&p, "v2", Some(&v1)).unwrap();
        match res {
            WriteResult::Written { version } => assert_eq!(version, content_version(b"v2")),
            _ => panic!("expected Written"),
        }
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "v2");
    }

    #[test]
    fn write_with_stale_version_conflicts_without_clobbering() {
        let s = Scratch::new("write_stale");
        let p = s.path("a.txt");
        std::fs::write(&p, "ondisk").unwrap();
        // The frontend thinks the file is still at the version of "old".
        let stale = content_version(b"old");
        let res = write_file(&p, "mine", Some(&stale)).unwrap();
        match res {
            WriteResult::Conflict {
                current_content,
                current_version,
            } => {
                assert_eq!(current_content, "ondisk");
                assert_eq!(current_version, content_version(b"ondisk"));
            }
            _ => panic!("expected Conflict"),
        }
        // File untouched.
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "ondisk");
    }

    #[test]
    fn write_expected_but_file_vanished_is_conflict() {
        let s = Scratch::new("write_vanished");
        let p = s.path("gone.txt");
        let res = write_file(&p, "mine", Some("anyhash")).unwrap();
        assert!(matches!(res, WriteResult::Conflict { .. }));
        assert!(!p.exists()); // not recreated
    }

    #[test]
    fn trash_fallback_moves_into_sot_trash() {
        let s = Scratch::new("trash_fallback");
        let p = s.path("doomed.txt");
        std::fs::write(&p, "bytes").unwrap();
        let dest = trash_file_fallback(&p, &s.0).unwrap();
        assert!(!p.exists());
        assert!(dest.starts_with(s.0.join(".sot-trash")));
        assert!(dest.file_name().unwrap().to_string_lossy().ends_with("-doomed.txt"));
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "bytes");
    }

    #[test]
    fn trash_fallback_collision_gets_suffix() {
        let s = Scratch::new("trash_collision");
        // Two same-named files trashed back-to-back within the same second
        // must land at distinct destinations.
        let p1 = s.path("dup.txt");
        std::fs::write(&p1, "one").unwrap();
        let d1 = trash_file_fallback(&p1, &s.0).unwrap();
        let p2 = s.path("dup.txt");
        std::fs::write(&p2, "two").unwrap();
        let d2 = trash_file_fallback(&p2, &s.0).unwrap();
        assert_ne!(d1, d2);
        assert_eq!(std::fs::read_to_string(&d1).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(&d2).unwrap(), "two");
    }
}
