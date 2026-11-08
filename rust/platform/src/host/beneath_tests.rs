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
fn the_root_is_refused_when_its_entry_is_a_link_or_it_is_not_canonical() {
    let (_g, root, outside) = setup();
    let link = root.parent().unwrap().join("link");
    symlink(&outside, &link).unwrap();
    let l = link.to_str().unwrap();
    let base = root.parent().unwrap();
    let deep = base.join("far").join("deep");
    std::fs::create_dir_all(&deep).unwrap();
    symlink(base.join("far").join("deep"), base.join("l2")).unwrap();
    let l2 = base.join("l2");
    let l2 = l2.to_str().unwrap();
    let lb = base.join("lb");
    symlink(base, &lb).unwrap();
    let r = root.to_str().unwrap();
    let spellings = [
        format!("{r}/"),
        format!("{r}//"),
        format!("{r}/."),
        format!("{}/root", lb.display()),
        l.to_string(),
        format!("{l}/"),
        format!("{l}/."),
        format!("{l}//"),
        format!("{l}/.."),
        format!("{l2}/../deep"),
        "relative".to_string(),
    ];
    for spelled in spellings {
        let got = write_beneath(Path::new(&spelled), "f.txt", b"data");
        assert_eq!(kind(got), io::ErrorKind::InvalidInput, "{spelled}");
        assert!(entries(&outside).is_empty(), "{spelled}");
        assert!(entries(&deep).is_empty(), "{spelled}");
        assert!(entries(&root).is_empty(), "{spelled}");
    }
}

#[test]
fn a_root_that_does_not_resolve_answers_by_what_is_wrong() {
    let (_g, root, _outside) = setup();
    let base = root.parent().unwrap();
    symlink(base.join("nothing"), base.join("dangling")).unwrap();
    symlink(base.join("selfloop"), base.join("selfloop")).unwrap();
    std::fs::write(base.join("file"), b"x").unwrap();
    for (what, want) in [
        ("missing", io::ErrorKind::NotFound),
        ("dangling", io::ErrorKind::NotFound),
        ("selfloop", io::ErrorKind::InvalidInput),
        ("file/under", io::ErrorKind::InvalidInput),
    ] {
        assert_eq!(kind(write_beneath(&base.join(what), "f.txt", b"data")), want, "{what}");
    }
}

#[test]
fn a_name_that_is_not_one_plain_entry_is_refused() {
    for rel in ["..", ".", "", "/x", "a//b", "a/", "a/../b", "a\0b"] {
        let (_g, root, outside) = setup();
        // Under a root that does not exist, only the rule answers InvalidInput; any call would answer NotFound.
        let missing = root.join("missing");
        assert_eq!(kind(write_beneath(&missing, rel, b"data")), io::ErrorKind::InvalidInput, "{rel:?}");
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
    let plant = |what: &str| {
        let _ = std::fs::remove_file(root.join(temp));
        match what {
            "symlink" => symlink(&target, root.join(temp)).unwrap(),
            "dangling" => symlink(outside.join("made.txt"), root.join(temp)).unwrap(),
            _ => std::fs::hard_link(&target, root.join(temp)).unwrap(),
        }
    };
    for what in ["symlink", "dangling", "hard link"] {
        plant(what);
        let got = write_beneath_at(&root, "f.txt", b"data", Some(temp), |_| Ok(()));
        assert_eq!(kind(got), io::ErrorKind::AlreadyExists, "{what}");
        assert_eq!(std::fs::read(&target).unwrap(), b"original", "{what}");
        assert!(!outside.join("made.txt").exists(), "{what}");
        assert!(!root.join("f.txt").exists(), "{what}");
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

#[test]
fn a_temporary_file_replaced_before_the_rename_is_an_error_and_writes_nothing_outside() {
    let (_g, root, outside) = setup();
    let target = outside.join("keep.txt");
    std::fs::write(&target, b"original").unwrap();
    let temp = ".sot-fixed.tmp";
    let got = write_beneath_at(&root, "f.txt", b"data", Some(temp), |s| {
        if s == Step::Written {
            std::fs::remove_file(root.join(temp)).unwrap();
            symlink(&target, root.join(temp)).unwrap();
        }
        Ok(())
    });
    assert!(got.unwrap_err().to_string().contains("not the entry at its name"));
    assert_eq!(std::fs::read(&target).unwrap(), b"original");
    // A link to the moved file at the temporary name is not the file written.
    let got = write_beneath_at(&root, "g.txt", b"data", Some(temp), |s| {
        if s == Step::Written {
            std::fs::rename(root.join(temp), root.join("moved")).unwrap();
            symlink("moved", root.join(temp)).unwrap();
        }
        Ok(())
    });
    assert!(got.unwrap_err().to_string().contains("not the entry at its name"));
}

#[test]
fn each_write_creates_its_temporary_file_under_a_fresh_name() {
    let (_g, root, _outside) = setup();
    let mut seen = Vec::new();
    for _ in 0..2 {
        write_beneath_at(&root, "f.txt", b"data", None, |s| {
            if s == Step::Temp {
                seen.extend(entries(&root).into_iter().filter(|n| n.starts_with(".sot-")));
            }
            Ok(())
        })
        .unwrap();
    }
    assert_eq!(seen.len(), 2);
    assert_ne!(seen[0], seen[1]);
    for n in &seen {
        let hex = n.strip_prefix(".sot-").and_then(|n| n.strip_suffix(".tmp")).unwrap();
        assert!(hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "{n}");
    }
}
