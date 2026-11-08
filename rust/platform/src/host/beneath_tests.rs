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

/// Moves the folder at `at` aside (as `<name>.moved`) and puts a link to `to` at its name.
fn swap_for_link(at: &Path, to: &Path) {
    let moved = at.with_file_name(format!("{}.moved", at.file_name().unwrap().to_string_lossy()));
    std::fs::rename(at, moved).unwrap();
    symlink(to, at).unwrap();
}

#[test]
fn a_folder_swapped_for_a_link_after_its_open_keeps_the_write_inside() {
    for depth in [1, 2] {
        let (_g, root, outside) = setup();
        let (rel, folder) = if depth == 1 { ("d/f.txt", root.join("d")) } else { ("a/d/f.txt", root.join("a/d")) };
        std::fs::create_dir_all(&folder).unwrap();
        write_beneath_at(&root, rel, b"data", None, |s| {
            if s == Step::Folder(depth - 1) {
                swap_for_link(&folder, &outside);
            }
            Ok(())
        })
        .unwrap();
        let moved = folder.with_file_name("d.moved");
        assert_eq!(std::fs::read(moved.join("f.txt")).unwrap(), b"data", "depth {depth}");
        assert!(entries(&outside).is_empty(), "depth {depth}");
        assert!(entries(&moved).iter().all(|n| n == "f.txt"), "depth {depth}");
    }
}

#[test]
fn a_folder_swapped_for_a_link_before_its_open_takes_no_write() {
    // The step before each folder's open: Root for the first, Folder(0) for the second.
    for (before, rel, folder) in [(Step::Root, "d/f.txt", "d"), (Step::Folder(0), "a/d/f.txt", "a/d")] {
        let (_g, root, outside) = setup();
        std::fs::create_dir_all(root.join(folder)).unwrap();
        let got = write_beneath_at(&root, rel, b"data", None, |s| {
            if s == before {
                swap_for_link(&root.join(folder), &outside);
            }
            Ok(())
        });
        assert_eq!(kind(got), io::ErrorKind::InvalidInput, "{rel}");
        assert!(entries(&outside).is_empty(), "{rel}");
    }
}

#[test]
fn a_folder_made_and_swapped_before_its_open_takes_no_write() {
    // No step falls between a folder's `mkdirat` and its open; the first step that can see a made folder is its own
    // `Folder(i)`. So the name still to be made below it is the one taken: at `Folder(0)` a link is put at the name
    // the next `mkdirat` would make.
    let (_g, root, outside) = setup();
    let got = write_beneath_at(&root, "a/b/f.txt", b"data", None, |s| {
        if s == Step::Folder(0) {
            symlink(&outside, root.join("a/b")).unwrap();
        }
        Ok(())
    });
    assert_eq!(kind(got), io::ErrorKind::InvalidInput);
    assert!(entries(&outside).is_empty());
    // A folder made by the call and swapped as soon as it is open: the file lands in the moved folder.
    let got = write_beneath_at(&root, "c/f.txt", b"data", None, |s| {
        if s == Step::Folder(0) {
            swap_for_link(&root.join("c"), &outside);
        }
        Ok(())
    });
    got.unwrap();
    assert_eq!(std::fs::read(root.join("c.moved/f.txt")).unwrap(), b"data");
    assert!(entries(&outside).is_empty());
}

#[test]
fn a_final_name_planted_before_the_rename_is_replaced_not_followed() {
    for what in ["symlink", "hard link"] {
        let (_g, root, outside) = setup();
        let target = outside.join("keep.txt");
        std::fs::write(&target, b"original").unwrap();
        write_beneath_at(&root, "f.txt", b"data", None, |s| {
            if s == Step::Written {
                if what == "symlink" {
                    symlink(&target, root.join("f.txt")).unwrap();
                } else {
                    std::fs::hard_link(&target, root.join("f.txt")).unwrap();
                }
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"original", "{what}");
        assert!(std::fs::symlink_metadata(root.join("f.txt")).unwrap().is_file(), "{what}");
        assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"data", "{what}");
        assert_eq!(entries(&outside), ["keep.txt"], "{what}");
    }
}

fn temp_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            found.extend(temp_files(&e.path()));
        } else if e.file_name().to_string_lossy().starts_with(".sot-") {
            found.push(e.path());
        }
    }
    found
}

#[test]
fn an_error_at_every_step_leaves_no_temporary_file() {
    for step in [Step::Root, Step::Folder(0), Step::Folder(1), Step::Temp, Step::Written, Step::Renamed] {
        let (_g, root, _outside) = setup();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/b/f.txt"), b"old").unwrap();
        let got = write_beneath_at(&root, "a/b/f.txt", b"new", None, |s| {
            if s == step { Err(io::Error::other("injected")) } else { Ok(()) }
        });
        assert_eq!(got.unwrap_err().to_string(), "injected", "{step:?}");
        assert!(temp_files(&root).is_empty(), "{step:?}");
        let want: &[u8] = if step == Step::Renamed { b"new" } else { b"old" };
        assert_eq!(std::fs::read(root.join("a/b/f.txt")).unwrap(), want, "{step:?}");
    }
}

#[test]
fn two_writers_of_one_name_each_publish_a_whole_file() {
    let (_g, root, _outside) = setup();
    let contents: [Vec<u8>; 2] = [vec![b'a'; 64 * 1024], vec![b'b'; 48 * 1024]];
    std::fs::write(root.join("f.txt"), &contents[0]).unwrap();
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|sc| {
        let writers: Vec<_> = contents
            .iter()
            .map(|c| {
                let root = &root;
                sc.spawn(move || {
                    for _ in 0..200 {
                        write_beneath(root, "f.txt", c).unwrap();
                    }
                })
            })
            .collect();
        let reader = sc.spawn(|| {
            let mut reads = 0;
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                let got = std::fs::read(root.join("f.txt")).unwrap();
                assert!(contents.iter().any(|c| *c == got), "a read of {} bytes is neither writer's", got.len());
                reads += 1;
            }
            reads
        });
        let finished: Vec<_> = writers.into_iter().map(|w| w.join()).collect();
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        let reads = reader.join();
        assert!(finished.iter().all(|r| r.is_ok()), "a writer erred");
        assert!(reads.unwrap() > 0);
    });
    assert!(temp_files(&root).is_empty());
}
