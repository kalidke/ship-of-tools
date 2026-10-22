
#[test]
fn a_missing_parent_is_created_and_the_leaf_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let leaf = root.path().join("a").join("b").join("sot");
    secure_private_dir(&leaf).unwrap();
    assert_eq!(std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777, 0o700);
    secure_private_dir(&leaf).unwrap(); // idempotent on the second boot
}
use super::secure_private_dir;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// A fresh folder and a path `name` inside it; the folder's drop removes both, even when the test panics.
fn scratch_path(name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    (dir, path)
}

#[test]
fn absent_dir_is_created_owner_only() {
    let (_d_dir, d) = scratch_path("absent");
    assert!(secure_private_dir(&d).is_ok());
    let meta = std::fs::symlink_metadata(&d).unwrap();
    assert!(meta.is_dir());
    assert_eq!(meta.permissions().mode() & 0o777, 0o700);
}

#[test]
fn existing_owner_only_dir_is_accepted() {
    let (_d_dir, d) = scratch_path("existing-ok");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(secure_private_dir(&d).is_ok());
}

#[test]
fn existing_group_readable_dir_is_rejected() {
    let (_d_dir, d) = scratch_path("existing-group-readable");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o750)).unwrap();
    assert!(secure_private_dir(&d).is_err());
}

#[test]
fn existing_symlink_is_rejected_even_to_a_valid_dir() {
    let (_target_dir, target) = scratch_path("symlink-target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (_link_dir, link) = scratch_path("symlink-link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(secure_private_dir(&link).is_err());
}

#[test]
fn existing_regular_file_is_rejected() {
    let (_d_dir, d) = scratch_path("a-file");
    std::fs::write(&d, b"not a dir").unwrap();
    assert!(secure_private_dir(&d).is_err());
}

#[test]
fn ensure_private_dir_makes_an_owner_only_folder_that_is_private() {
    let root = tempfile::tempdir().unwrap();
    let leaf = root.path().join("a").join("b");
    super::ensure_private_dir(&leaf).unwrap();
    assert_eq!(std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777, 0o700);
    assert!(crate::host::state_dir::is_private_dir(&leaf));
}

/// A private-folder refusal that comes from the OS keeps the OS's own error (its kind and raw code) for the caller.
#[test]
fn a_private_dir_error_from_the_os_keeps_its_native_code() {
    use std::error::Error as _;
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    let err = super::secure_private_dir(&file.join("below")).unwrap_err();
    let source = err.source().and_then(|s| s.downcast_ref::<std::io::Error>()).expect("the OS error is the source");
    assert_eq!(source.raw_os_error(), Some(libc::ENOTDIR), "{err}");
    assert!(err.to_string().contains("below"), "the path stays in the text: {err}");
}
