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
    fn files(&self) -> FilesMode {
        FilesMode::new(self.0.clone()).unwrap()
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
    let res = write_file(&s.files(), "files:sub/dir/new.txt", "hi", None).unwrap();
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
    let res = write_file(&s.files(), "files:a.txt", "v2", Some(&v1)).unwrap();
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
    let res = write_file(&s.files(), "files:a.txt", "mine", Some(&stale)).unwrap();
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
    let res = write_file(&s.files(), "files:gone.txt", "mine", Some("anyhash")).unwrap();
    assert!(matches!(res, WriteResult::Conflict { .. }));
    assert!(!p.exists()); // not recreated
}

/// A save is written beneath the project root, following no link: nothing is ever written at `<file>.sot-tmp`.
#[cfg(unix)]
#[test]
fn a_link_at_the_old_temporary_name_is_never_written() {
    let s = Scratch::new("write_old_tmp");
    let outside = Scratch::new("write_old_tmp_outside");
    let target = outside.path("target.txt");
    std::fs::write(&target, "outside").unwrap();
    std::fs::write(s.path("a.txt"), "v1").unwrap();
    std::fs::hard_link(&target, s.path("a.sot-tmp")).unwrap();
    write_file(&s.files(), "files:a.txt", "v2", None).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "outside");
    std::fs::remove_file(s.path("a.sot-tmp")).unwrap();
    std::os::unix::fs::symlink(&target, s.path("a.sot-tmp")).unwrap();
    write_file(&s.files(), "files:a.txt", "v3", None).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "outside");
    assert_eq!(std::fs::read_to_string(s.path("a.txt")).unwrap(), "v3");
}

/// A save through a link at a folder of its path is refused as a node id and writes nothing.
#[cfg(unix)]
#[test]
fn a_save_through_a_folder_link_takes_no_write() {
    let s = Scratch::new("write_folder_link");
    let outside = Scratch::new("write_folder_link_outside");
    std::os::unix::fs::symlink(&outside.0, s.path("lnk")).unwrap();
    let inside = s.path("real");
    std::fs::create_dir(&inside).unwrap();
    std::os::unix::fs::symlink(&inside, s.path("alias")).unwrap();
    for id in ["files:lnk/a.txt", "files:lnk/new/a.txt", "files:alias/a.txt"] {
        let e = write_file(&s.files(), id, "x", None).unwrap_err();
        let kind = e.downcast_ref::<std::io::Error>().map(|e| e.kind());
        assert_eq!(kind, Some(std::io::ErrorKind::InvalidInput), "{id}: {e:#}");
    }
    assert_eq!(std::fs::read_dir(&outside.0).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&inside).unwrap().count(), 0);
}

/// A save at a name that is a link is refused as a node id and writes nothing, whether the link leads in or out.
#[cfg(unix)]
#[test]
fn a_save_at_a_name_that_is_a_link_takes_no_write() {
    let s = Scratch::new("write_name_link");
    let outside = Scratch::new("write_name_link_outside");
    let out_file = outside.path("target.txt");
    std::fs::write(&out_file, "old").unwrap();
    std::fs::write(s.path("real.txt"), "old").unwrap();
    std::os::unix::fs::symlink(&out_file, s.path("out.txt")).unwrap();
    std::os::unix::fs::symlink(s.path("real.txt"), s.path("in.txt")).unwrap();
    for id in ["files:out.txt", "files:in.txt"] {
        for expected in [None, Some(content_version(b"old"))] {
            let e = write_file(&s.files(), id, "x", expected.as_deref()).unwrap_err();
            let kind = e.downcast_ref::<std::io::Error>().map(|e| e.kind());
            assert_eq!(kind, Some(std::io::ErrorKind::InvalidInput), "{id}: {e:#}");
        }
    }
    assert_eq!(std::fs::read_to_string(&out_file).unwrap(), "old");
    assert_eq!(std::fs::read_to_string(s.path("real.txt")).unwrap(), "old");
    assert!(std::fs::symlink_metadata(s.path("out.txt")).unwrap().file_type().is_symlink());
    assert!(std::fs::symlink_metadata(s.path("in.txt")).unwrap().file_type().is_symlink());
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
