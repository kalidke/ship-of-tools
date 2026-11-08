use super::*;
use std::os::unix::fs::symlink;

fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    (dir, root, outside)
}

fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> =
        std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

fn kind_of(r: io::Result<PathBuf>) -> io::ErrorKind {
    r.unwrap_err().kind()
}

fn kind(r: io::Result<PathBuf>) -> io::ErrorKind {
    r.unwrap_err().kind()
}

#[test]
fn a_link_at_any_folder_takes_no_write() {
    for depth in [1, 2] {
        for case in ["outside folder", "dangling", "file"] {
            let (_g, root, outside) = setup();
            let (parent, rel_prefix) = if depth == 1 {
                (root.clone(), "d".to_string())
            } else {
                std::fs::create_dir(root.join("a")).unwrap();
                (root.join("a"), "a/d".to_string())
            };
            let at = parent.join("d");
            match case {
                "outside folder" => symlink(&outside, &at).unwrap(),
                "dangling" => symlink(outside.join("nothing"), &at).unwrap(),
                _ => std::fs::write(&at, b"x").unwrap(),
            }
            let before = entries(&parent);
            let got = write_beneath(&root, &format!("{rel_prefix}/f.txt"), b"data");
            assert_eq!(kind(got), io::ErrorKind::InvalidInput, "depth {depth} {case}");
            assert!(entries(&outside).is_empty(), "depth {depth} {case}");
            assert_eq!(entries(&parent), before, "depth {depth} {case}");
        }
    }
}

#[test]
fn the_root_is_refused_when_its_entry_is_a_link() {
    let (_g, root, outside) = setup();
    let link = root.parent().unwrap().join("link");
    symlink(&outside, &link).unwrap();
    assert_eq!(kind(write_beneath(&link, "f.txt", b"data")), io::ErrorKind::InvalidInput);
    assert!(entries(&outside).is_empty());
}

#[test]
fn a_name_that_is_not_one_plain_entry_is_refused() {
    for rel in ["..", ".", "", "/x", "a//b", "a/", "a/../b", "a\0b"] {
        let (_g, root, outside) = setup();
        assert_eq!(kind(write_beneath(&root, rel, b"data")), io::ErrorKind::InvalidInput, "{rel:?}");
        assert!(entries(&root).is_empty(), "{rel:?}");
        assert!(entries(&outside).is_empty(), "{rel:?}");
    }
}

#[test]
fn a_planted_temporary_name_is_never_written_through() {
    let (_g, root, outside) = setup();
    let target = outside.join("keep.txt");
    std::fs::write(&target, b"original").unwrap();
    let temp = ".sot-fixed.tmp";
    let plant = |kind: &str| {
        let _ = std::fs::remove_file(root.join(temp));
        match kind {
            "symlink" => symlink(&target, root.join(temp)).unwrap(),
            "dangling" => symlink(outside.join("made.txt"), root.join(temp)).unwrap(),
            _ => std::fs::hard_link(&target, root.join(temp)).unwrap(),
        }
    };
    for kind in ["symlink", "dangling", "hard link"] {
        plant(kind);
        let got = write_beneath_at(&root, "f.txt", b"data", Some(temp), |_| Ok(()));
        assert_eq!(kind_of(got), io::ErrorKind::AlreadyExists, "{kind}");
        assert_eq!(std::fs::read(&target).unwrap(), b"original", "{kind}");
        assert!(!outside.join("made.txt").exists(), "{kind}");
        assert!(!root.join("f.txt").exists(), "{kind}");
    }
}

#[test]
fn a_link_at_the_final_name_is_replaced_not_followed() {
    for kind in ["symlink", "hard link"] {
        let (_g, root, outside) = setup();
        let target = outside.join("keep.txt");
        std::fs::write(&target, b"original").unwrap();
        if kind == "symlink" {
            symlink(&target, root.join("f.txt")).unwrap();
        } else {
            std::fs::hard_link(&target, root.join("f.txt")).unwrap();
        }
        write_beneath(&root, "f.txt", b"data").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"original", "{kind}");
        let meta = std::fs::symlink_metadata(root.join("f.txt")).unwrap();
        assert!(meta.is_file(), "{kind}");
        assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"data", "{kind}");
    }
}

#[test]
fn a_failed_write_leaves_the_old_file_and_no_temporary_file() {
    let (_g, root, _outside) = setup();
    std::fs::write(root.join("f.txt"), b"old").unwrap();
    let got = write_beneath_at(&root, "f.txt", b"new", None, |s| {
        if s == Step::Written {
            Err(io::Error::other("injected"))
        } else {
            Ok(())
        }
    });
    assert_eq!(got.unwrap_err().to_string(), "injected");
    assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"old");
    assert_eq!(entries(&root), ["f.txt"]);
}

#[test]
fn missing_folders_are_created_and_the_path_is_returned() {
    let (_g, root, _outside) = setup();
    let got = write_beneath(&root, "a/b/f.txt", b"data").unwrap();
    assert_eq!(got, root.join("a/b/f.txt"));
    assert_eq!(std::fs::read(&got).unwrap(), b"data");
    assert_eq!(entries(&root.join("a/b")), ["f.txt"]);
}
