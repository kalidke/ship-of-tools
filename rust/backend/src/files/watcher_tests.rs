use super::*;

#[test]
fn should_skip_known_high_churn_dirs() {
    assert!(should_skip(Path::new("/a/.git/HEAD"), &[]));
    assert!(should_skip(Path::new("/a/target/debug/foo"), &[]));
    assert!(should_skip(Path::new("/a/sub/node_modules/x"), &[]));
    assert!(should_skip(Path::new("/a/.julia/registries/General.toml"), &[]));
}

#[test]
fn should_skip_lets_normal_paths_through() {
    assert!(!should_skip(Path::new("/a/src/lib.rs"), &[]));
    assert!(!should_skip(Path::new("/a/.concept/types/Foo.md"), &[]));
    assert!(!should_skip(Path::new("/a/docs/readme.md"), &[]));
    // ADR 0022: crop output is skipped, but other .sot config + an
    // unrelated captures/ are not.
    assert!(should_skip(Path::new("/a/.sot/captures/img-roi-1.png"), &[]));
    assert!(!should_skip(Path::new("/a/.sot/settings.toml"), &[]));
    assert!(!should_skip(Path::new("/a/data/captures/run.csv"), &[]));
}

// A unique scratch dir for a filesystem test, following the pattern the other
// backend tests use (no tempfile dep). Caller removes it.
fn scratch(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("sot-watch-{}-{}", tag, std::process::id()))
}

fn noop_watcher() -> notify::RecommendedWatcher {
    notify::recommended_watcher(|_res: notify::Result<notify::Event>| {}).unwrap()
}

#[test]
fn register_subtree_skips_build_vcs_and_symlink_dirs() {
    use std::fs;
    let base = scratch("filter");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("src/inner")).unwrap();
    fs::create_dir_all(base.join("docs")).unwrap();
    fs::create_dir_all(base.join(".git/objects")).unwrap(); // skip (VCS)
    fs::create_dir_all(base.join("target/debug")).unwrap(); // skip (build)
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(base.join("src"), base.join("link_to_src"));

    let mut w = noop_watcher();
    let root_dev = fs::symlink_metadata(&base).map(|m| device_of(&m)).unwrap_or(0);
    let mut watched: HashSet<PathBuf> = HashSet::new();
    register_subtree(&mut w, &base, root_dev, &[], &mut watched, 10_000);

    // Kept: the real project dirs.
    assert!(watched.contains(&base));
    assert!(watched.contains(&base.join("src")));
    assert!(watched.contains(&base.join("src/inner")));
    assert!(watched.contains(&base.join("docs")));
    // Skipped at REGISTRATION: build/VCS subtrees consume no watch descriptor.
    assert!(!watched.iter().any(|p| p.starts_with(base.join(".git"))));
    assert!(!watched.iter().any(|p| p.starts_with(base.join("target"))));
    // A symlinked directory is never followed.
    #[cfg(unix)]
    assert!(!watched.contains(&base.join("link_to_src")));

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn register_subtree_respects_budget_cap() {
    use std::fs;
    let base = scratch("cap");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    for i in 0..5 {
        fs::create_dir_all(base.join(format!("d{i}"))).unwrap();
    }
    // 6 dirs available (base + 5); a cap of 3 must bound registration.
    let mut w = noop_watcher();
    let root_dev = fs::symlink_metadata(&base).map(|m| device_of(&m)).unwrap_or(0);
    let mut watched: HashSet<PathBuf> = HashSet::new();
    register_subtree(&mut w, &base, root_dev, &[], &mut watched, 3);
    assert_eq!(watched.len(), 3, "the budget cap must bound the watch count");

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn watch_budget_honors_env_override() {
    std::env::set_var("SOT_WATCH_BUDGET", "1234");
    assert_eq!(watch_budget(), 1234);
    std::env::remove_var("SOT_WATCH_BUDGET");
    // Without the override it must be a sane positive cap.
    assert!(watch_budget() >= 256);
}

#[test]
fn should_skip_excludes_our_own_state_tree() {
    let ours = vec![PathBuf::from("/h/.local/state/sot")];
    // The root itself and everything under it: the updater creates its
    // staging directory here and renames it into place, and a watch open
    // on that directory is what makes the rename fail on Windows.
    assert!(should_skip(Path::new("/h/.local/state/sot"), &ours));
    assert!(should_skip(
        Path::new("/h/.local/state/sot/updates/tmp-v0.6.6-target/bin"),
        &ours
    ));
    // Without the exclusion it is an ordinary directory — which is exactly
    // how the watcher came to hold a handle on the stager's temp dir.
    assert!(!should_skip(
        Path::new("/h/.local/state/sot/updates/tmp-v0.6.6-target/bin"),
        &[]
    ));
}

#[test]
fn should_skip_keeps_a_sibling_whose_name_shares_the_prefix() {
    let ours = vec![PathBuf::from("/h/.local/state/sot")];
    // Component-wise, never a string prefix — a user directory that merely
    // begins with the same characters is a real project and stays watched.
    assert!(!should_skip(
        Path::new("/h/.local/state/sot-notes/a.md"),
        &ours
    ));
    assert!(!should_skip(Path::new("/h/.local/state/sotalike"), &ours));
}

#[test]
fn should_skip_still_passes_ordinary_project_paths_when_excluding() {
    let ours = vec![PathBuf::from("/h/.local/state/sot")];
    // The exclusion must not silently turn file watching off.
    assert!(!should_skip(Path::new("/h/projects/app/src/lib.rs"), &ours));
    assert!(!should_skip(Path::new("/h/README.md"), &ours));
}

#[test]
fn register_subtree_never_watches_our_own_tree() {
    use std::fs;
    let base = scratch("self");
    let _ = fs::remove_dir_all(&base);
    // `sot` is ours; `sot-notes` merely starts with the same characters.
    fs::create_dir_all(base.join("sot/updates/tmp-stage")).unwrap();
    fs::create_dir_all(base.join("sot-notes/deep")).unwrap();
    fs::create_dir_all(base.join("src")).unwrap();

    let ours = vec![base.join("sot")];
    let mut w = noop_watcher();
    let root_dev = fs::symlink_metadata(&base).map(|m| device_of(&m)).unwrap_or(0);
    let mut watched: HashSet<PathBuf> = HashSet::new();
    register_subtree(&mut w, &base, root_dev, &ours, &mut watched, 10_000);

    assert!(
        !watched.iter().any(|p| p.starts_with(base.join("sot"))),
        "no watch descriptor may be spent anywhere under our own tree"
    );
    assert!(watched.contains(&base.join("sot-notes")));
    assert!(watched.contains(&base.join("sot-notes/deep")));
    assert!(watched.contains(&base.join("src")));

    let _ = fs::remove_dir_all(&base);
}
