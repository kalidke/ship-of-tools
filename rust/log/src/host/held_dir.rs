//! A held folder: one that lives as long as the process that made it. The maker holds a kernel lock on a file in the
//! folder for the folder's whole life, and a later process removes the folder only once it can take that lock, so a
//! live maker's folder stays wherever the maker runs: on another computer sharing the folder, in another pid namespace,
//! or under a pid the OS has since given to another process; on NFS, only while the maker's client holds its lease, for
//! a client cut off past its lease (90 s by default) can lose its lock and its folder with it. A pid
//! names a process on one computer and in one pid namespace only, and after the process ends it names the next one.

use super::lock::{lock_held, lock_new, WriterLock};
use crate::Result;
use std::path::{Path, PathBuf};

/// The lock file in every held folder. It gets this name only once it is taken ([`HeldDir::create`]).
const HOLDER: &str = "holder.lock";

/// The name the lock file is taken under before it is renamed to [`HOLDER`].
const STAGING: &str = "holder.lock.new";

/// A folder this process holds until it ends or [`remove`](Self::remove) removes the folder. Dropped, it frees the lock
/// and leaves the folder, as the process's end does, for a later [`remove_ended`].
pub struct HeldDir {
    path: PathBuf,
    lock: WriterLock,
}

impl HeldDir {
    /// Makes `<prefix><random>` in `parent`, owner-only, and holds it. Its lock file is made and taken under a staging
    /// name and only then renamed to the name a removal looks for, so no process finds a live maker's lock free.
    pub fn create(parent: &Path, prefix: &str) -> Result<Self> {
        let path = loop {
            let mut random = [0u8; 4];
            getrandom::fill(&mut random).map_err(std::io::Error::from)?;
            let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
            let path = parent.join(format!("{prefix}{name}"));
            match create_private(&path) {
                Ok(()) => break path,
                Err(crate::Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        };
        let held = lock_new(&path.join(STAGING)).and_then(|lock| {
            std::fs::rename(path.join(STAGING), path.join(HOLDER))?;
            Ok(lock)
        });
        match held {
            Ok(lock) => Ok(Self { path, lock }),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&path);
                Err(e)
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Removes the folder: everything in it while the lock is held, then the lock file and the folder. A removal that
    /// stops early leaves the lock file, so once this process has ended a later [`remove_ended`] finishes it.
    pub fn remove(self) -> std::io::Result<()> {
        remove_held(&self.path, self.lock)
    }
}

/// Removes every folder named `<prefix>*` in `parent` that this account owns and whose maker has ended (its lock can be
/// taken), and returns how many went. A folder whose lock is held, cannot be opened or is not there (one not yet named
/// into place, one no maker held) stays, and so does a file or a link. A folder that does not go is logged and stays.
/// On Windows, whose temporary folder is the account's own, a directory is enough.
pub fn remove_ended(parent: &Path, prefix: &str) -> usize {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !entry.file_name().to_str().is_some_and(|name| name.starts_with(prefix)) || !own_folder(&entry) {
            continue;
        }
        let path = entry.path();
        // A held lock answers within the retry deadline; one that is not there or cannot be opened, at once.
        let Ok(lock) = lock_held(&path.join(HOLDER)) else {
            continue;
        };
        match remove_held(&path, lock) {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "held folder not removed"),
        }
    }
    removed
}

/// Makes the folder at `path`, which must not exist, owner-only whatever the umask.
fn create_private(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path)?;
        Ok(())
    }
    #[cfg(windows)]
    super::create_dir_protected(path)
}

/// Removes everything in `path` but its lock file while `lock` holds it, then frees the lock and removes the lock file
/// and the folder. An entry that does not go stops it with the lock file left in place; one another removal took first
/// counts as gone.
fn remove_held(path: &Path, lock: WriterLock) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        entries => entries?,
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_name() == HOLDER {
            continue;
        }
        // A link is removed as itself, never followed.
        let removed = if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())
        } else {
            std::fs::remove_file(entry.path())
        };
        gone(removed)?;
    }
    // Freed and closed before its file goes: where a file system deletes a file only at its last close (one without
    // POSIX deletion), this handle would keep the file, and so the folder, in place.
    drop(lock);
    gone(std::fs::remove_file(path.join(HOLDER)))?;
    gone(std::fs::remove_dir(path))
}

/// A removal's result, with what was not there counted as removed.
fn gone(removed: std::io::Result<()>) -> std::io::Result<()> {
    match removed {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Whether `entry` is a directory, not a link, that this account owns.
fn own_folder(entry: &std::fs::DirEntry) -> bool {
    // A `DirEntry`'s metadata is the entry's own: a link is not followed.
    let Ok(meta) = entry.metadata() else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.is_dir() && meta.uid() == super::state_dir::current_uid()
    }
    #[cfg(not(unix))]
    meta.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Puts `inner/made` in the folder at `path`, made if absent, so a removal that takes its contents shows.
    fn with_contents(path: &Path) {
        std::fs::create_dir_all(path.join("inner")).unwrap();
        std::fs::write(path.join("inner").join("made"), b"x").unwrap();
    }

    /// The rule, as a table over a folder of the test's own: a held folder goes, with everything in it, once its maker
    /// has ended and only then. A live maker's folder stays (here this process holds it through another open file, as
    /// another process would), and so do a folder whose lock file is not there (not yet renamed into place, or never
    /// made) or cannot be opened, a file, a link to a folder whose lock is free, and a folder of another prefix.
    #[test]
    fn a_held_folder_goes_once_its_maker_has_ended_and_only_then() {
        let root = tempfile::tempdir().unwrap();
        let at = |name: &str| root.path().join(name);
        let live = HeldDir::create(root.path(), "held-").unwrap();
        with_contents(live.path());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(live.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "a held folder is its owner's alone");
        }
        let ended = HeldDir::create(root.path(), "held-").unwrap();
        with_contents(ended.path());
        let ended_path = ended.path().to_path_buf();
        // Its maker's end: the lock is freed and the folder left.
        drop(ended);
        let free = |path: &Path| {
            with_contents(path);
            std::fs::write(path.join(HOLDER), b"").unwrap();
        };
        with_contents(&at("held-unnamed"));
        std::fs::write(at("held-unnamed").join(STAGING), b"").unwrap();
        with_contents(&at("held-never"));
        with_contents(&at("held-unopenable"));
        std::fs::create_dir(at("held-unopenable").join(HOLDER)).unwrap();
        std::fs::write(at("held-file"), b"x").unwrap();
        free(&at("other-ended"));
        free(&at("target"));
        #[cfg(unix)]
        std::os::unix::fs::symlink(at("target"), at("held-link")).unwrap();

        assert_eq!(remove_ended(root.path(), "held-"), 1, "the ended maker's folder alone goes");
        assert!(!ended_path.exists(), "{} is removed", ended_path.display());
        let kept = ["held-unnamed", "held-never", "held-unopenable", "other-ended", "target"].map(at);
        for kept in kept.iter().map(PathBuf::as_path).chain([live.path()]) {
            assert!(kept.join("inner").join("made").exists(), "{} stays, with its contents", kept.display());
        }
        assert!(at("held-file").exists(), "a file stays");
        #[cfg(unix)]
        assert!(at("held-link").symlink_metadata().is_ok(), "a link stays");
        assert!(at("target").join(HOLDER).exists(), "a link's target keeps its lock file");

        // Its maker removes a live folder, contents, lock file and all.
        let live_path = live.path().to_path_buf();
        live.remove().unwrap();
        assert!(!live_path.exists(), "{} is removed by its maker", live_path.display());
    }

    /// A removal holds a folder's lock file open while it waits on a live maker's lock, and the maker removes its
    /// folder all the same: on Windows the removal's open shares deletion, as the maker's does (a fence's does
    /// not), and unix lets any open file go. Here a removal's take of an ended maker's lock holds the file open, as
    /// its wait would.
    #[test]
    fn a_held_folder_goes_while_a_removal_holds_its_lock_file_open() {
        let root = tempfile::tempdir().unwrap();
        let ended = HeldDir::create(root.path(), "held-").unwrap();
        let path = ended.path().to_path_buf();
        drop(ended);
        let open = lock_held(&path.join(HOLDER)).unwrap();
        std::fs::remove_file(path.join(HOLDER)).expect("a lock file a removal holds open goes");
        std::fs::remove_dir(&path).expect("and its folder with it");
        drop(open);
    }
}
