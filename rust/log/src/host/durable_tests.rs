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
