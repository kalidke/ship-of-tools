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

#[test]
fn publish_noreplace_refuses_an_existing_target_and_leaves_both_files() {
    let dir = tempfile::tempdir().unwrap();
    let (source, target) = (dir.path().join("source"), dir.path().join("target"));
    std::fs::write(&source, b"new").unwrap();
    std::fs::write(&target, b"first").unwrap();
    match publish_noreplace(&source, &target) {
        Err(Error::Io(e)) => assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists),
        other => panic!("a publish over an existing target must refuse: {other:?}"),
    }
    assert_eq!(std::fs::read(&target).unwrap(), b"first");
    assert_eq!(std::fs::read(&source).unwrap(), b"new");
    std::fs::remove_file(&target).unwrap();
    publish_noreplace(&source, &target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
    assert!(!source.exists());
}

#[test]
fn write_durable_replaces_a_record_and_leaves_no_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("record");
    write_durable(&path, b"one").unwrap();
    write_durable(&path, b"two").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"two");
    let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("record")]);
}

#[test]
fn remove_durable_is_idempotent_and_accepts_a_missing_folder() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("record");
    write_durable(&path, b"x").unwrap();
    remove_durable(&path).unwrap();
    assert!(!path.exists());
    remove_durable(&path).unwrap();
    remove_durable(&dir.path().join("gone").join("record")).unwrap();
}

/// Runs one durable write over a stale entry at the temporary name and checks the outside file and the target.
fn durable_over_stale_temp(plant: impl FnOnce(&Path, &Path)) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("root");
    std::fs::create_dir(&folder).unwrap();
    let outside = dir.path().join("outside.txt");
    plant(&folder.join("record.tmp"), &outside);
    let target = folder.join("record");
    write_durable(&target, b"new").unwrap();
    (dir, outside, target)
}

#[cfg(unix)]
#[test]
fn write_durable_over_a_symlinked_temporary_name_leaves_the_linked_file_alone() {
    let (_d, outside, target) = durable_over_stale_temp(|tmp, outside| {
        std::fs::write(outside, b"keep").unwrap();
        std::os::unix::fs::symlink(outside, tmp).unwrap();
    });
    assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
}

#[cfg(unix)]
#[test]
fn write_durable_over_a_dangling_temporary_name_creates_nothing_outside() {
    let (_d, outside, target) = durable_over_stale_temp(|tmp, outside| std::os::unix::fs::symlink(outside, tmp).unwrap());
    assert!(!outside.exists());
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
}

#[test]
fn write_durable_over_a_hard_linked_temporary_name_leaves_the_linked_file_alone() {
    let (_d, outside, target) = durable_over_stale_temp(|tmp, outside| {
        std::fs::write(outside, b"keep").unwrap();
        std::fs::hard_link(outside, tmp).unwrap();
    });
    assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
}

#[test]
fn write_durable_refuses_an_entry_that_reappears_at_the_temporary_name() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("root");
    std::fs::create_dir(&folder).unwrap();
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, b"keep").unwrap();
    let target = folder.join("record");
    let tmp = folder.join("record.tmp");
    let err = write_durable_between(&target, b"new", || std::fs::hard_link(&outside, &tmp).unwrap()).unwrap_err();
    assert!(err.to_string().contains("record.tmp"), "{err}");
    assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
    assert!(!target.exists());
}

#[test]
#[cfg(unix)]
fn a_folder_swapped_between_the_create_and_the_rename_keeps_the_record_in_the_opened_folder() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("root");
    let moved = dir.path().join("moved");
    let other = dir.path().join("other");
    std::fs::create_dir(&folder).unwrap();
    std::fs::create_dir(&other).unwrap();
    let target = folder.join("record");
    super::super::beneath::write_in_folder(&target, b"new", || {}, |step| {
        if step == super::super::beneath::Step::Temp {
            std::fs::rename(&folder, &moved)?;
            std::os::unix::fs::symlink(&other, &folder)?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(std::fs::read(moved.join("record")).unwrap(), b"new");
    assert_eq!(std::fs::read_dir(&other).unwrap().count(), 0);
}
