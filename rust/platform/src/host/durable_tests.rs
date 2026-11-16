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

/// A private write's temporary file is owner-only at the step that creates it, and the record it replaces becomes so,
/// one that other accounts could read included.
#[test]
fn a_private_write_is_owner_only_from_its_creation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("record");
    std::fs::write(&path, b"old").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mode = |p: &Path| std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777;
        let mut at_create = None;
        let temp = dir.path().join("record.tmp");
        super::super::beneath::write_in_folder_at(&path, b"new", true, || {}, |s| {
            if s == super::super::beneath::Step::Temp {
                at_create = Some(mode(&temp));
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(at_create, Some(0o600), "the temporary file's mode at its creation");
        assert_eq!(mode(&path), 0o600, "the record's mode after the rename");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_durable_private(&path, b"new").unwrap();
        assert_eq!(mode(&path), 0o600, "the record's mode after the public write");
    }
    #[cfg(windows)]
    write_durable_private(&path, b"new").unwrap();
    #[cfg(windows)]
    {
        let expected = sddl::of_descriptor(super::super::owner_protected_pipe_descriptor().unwrap().as_ptr());
        let sid = super::super::token_user_sid_string().unwrap();
        assert!(expected.starts_with("D:P(A;;FA;;;") && expected.matches('(').count() == 1, "{expected}");
        assert!(sid.starts_with("S-1-"), "{sid}");
        assert_eq!(sddl::of_file(&path), expected, "the record's DACL: protected, one full-access entry for this user");
    }
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
}

/// A DACL as SDDL text, through the converter both sides share, so the expected and the actual speak one dialect.
#[cfg(windows)]
mod sddl {
    use std::path::Path;
    use windows_sys::Win32::Foundation::{LocalFree, HLOCAL};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

    pub fn of_descriptor(sd: PSECURITY_DESCRIPTOR) -> String {
        let mut text: *mut u16 = std::ptr::null_mut();
        let ok = unsafe { ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, SDDL_REVISION_1, DACL_SECURITY_INFORMATION, &mut text, std::ptr::null_mut()) };
        assert_ne!(ok, 0, "ConvertSecurityDescriptorToStringSecurityDescriptorW");
        let len = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
        unsafe { LocalFree(text as HLOCAL) };
        s
    }

    pub fn of_file(path: &Path) -> String {
        let wide = crate::host::wide_null(path.to_str().unwrap());
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let rc = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        assert_eq!(rc, 0, "GetNamedSecurityInfoW");
        let s = of_descriptor(sd);
        unsafe { LocalFree(sd as HLOCAL) };
        s
    }
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

/// A scanner or indexer holding the stale name open with delete sharing does not stop the next write.
#[cfg(windows)]
#[test]
fn a_stale_temporary_name_held_open_with_delete_sharing_does_not_stop_the_write() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("record");
    std::fs::write(dir.path().join("record.tmp"), b"stale").unwrap();
    let held = std::fs::OpenOptions::new().read(true).share_mode(0x7).open(dir.path().join("record.tmp")).unwrap();
    write_durable(&path, b"new").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
    drop(held);
}
