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
