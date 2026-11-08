use super::*;
#[cfg(unix)]
use std::os::unix::fs::symlink;

/// A folder link: a symlink on Unix, a junction on Windows (which needs no privilege).
fn link_dir(target: &Path, at: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return symlink(target, at);
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new("cmd");
        // `mklink` reads a `/` in a path as a switch.
        let back = |p: &Path| p.to_string_lossy().replace('/', "\\");
        cmd.args(["/C", "mklink", "/J"]).arg(back(at)).arg(back(target));
        let ran = crate::test_wait::run_within(&mut cmd, std::time::Duration::from_secs(30));
        if ran.status.success() { Ok(()) } else { Err(io::Error::other(ran.to_string())) }
    }
}

/// A folder link whose target is gone.
fn link_dangling_dir(at: &Path, gone: &Path) {
    #[cfg(unix)]
    symlink(gone, at).unwrap();
    #[cfg(windows)]
    {
        std::fs::create_dir(gone).unwrap();
        link_dir(gone, at).unwrap();
        std::fs::remove_dir(gone).unwrap();
    }
}

/// A file symlink; false, saying so, where this account may not make one (Windows without the privilege).
fn link_file(target: &Path, at: &Path) -> bool {
    #[cfg(unix)]
    symlink(target, at).unwrap();
    #[cfg(windows)]
    return made(std::os::windows::fs::symlink_file(target, at));
    #[cfg(unix)]
    true
}

/// Whether a Windows symlink was made; false, saying so, where this account may not make one.
#[cfg(windows)]
fn made(r: io::Result<()>) -> bool {
    match r {
        Ok(()) => true,
        Err(e) if e.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_PRIVILEGE_NOT_HELD as i32) => {
            eprintln!("skipped: this account may not create symlinks");
            false
        }
        Err(e) => panic!("{e}"),
    }
}

fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let base = crate::host::paths::simplify_verbatim(std::fs::canonicalize(dir.path()).unwrap());
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
        #[cfg_attr(unix, allow(unused_mut))]
        let mut cases = vec!["outside folder", "dangling", "file"];
        #[cfg(windows)]
        cases.push("directory symlink");
        for case in cases {
            let (_g, root, outside) = setup();
            let (parent, rel_prefix) = if depth == 1 {
                (root.clone(), "d".to_string())
            } else {
                std::fs::create_dir(root.join("a")).unwrap();
                (root.join("a"), "a/d".to_string())
            };
            let at = parent.join("d");
            match case {
                "outside folder" => link_dir(&outside, &at).unwrap(),
                "dangling" => link_dangling_dir(&at, &outside.join("nothing")),
                #[cfg(windows)]
                "directory symlink" => {
                    if !made(std::os::windows::fs::symlink_dir(&outside, &at)) {
                        continue;
                    }
                }
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
    link_dir(&outside, &link).unwrap();
    let l = link.to_str().unwrap();
    let base = root.parent().unwrap();
    let deep = base.join("far").join("deep");
    std::fs::create_dir_all(&deep).unwrap();
    link_dir(&base.join("far").join("deep"), &base.join("l2")).unwrap();
    let l2 = base.join("l2");
    let l2 = l2.to_str().unwrap();
    let lb = base.join("lb");
    link_dir(base, &lb).unwrap();
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
        // Windows reads `..` by its text, so `l2/../deep` is a folder that does not exist.
        let want = if cfg!(windows) && spelled.ends_with("/../deep") { io::ErrorKind::NotFound } else { io::ErrorKind::InvalidInput };
        assert_eq!(kind(got), want, "{spelled}");
        assert!(entries(&outside).is_empty(), "{spelled}");
        assert!(entries(&deep).is_empty(), "{spelled}");
        assert!(entries(&root).is_empty(), "{spelled}");
    }
}

#[test]
fn a_root_that_does_not_resolve_answers_by_what_is_wrong() {
    let (_g, root, _outside) = setup();
    let base = root.parent().unwrap();
    link_dangling_dir(&base.join("dangling"), &base.join("nothing"));
    #[cfg(unix)]
    symlink(base.join("selfloop"), base.join("selfloop")).unwrap();
    std::fs::write(base.join("file"), b"x").unwrap();
    #[cfg_attr(windows, allow(unused_mut))]
    let mut cases = vec![
        ("file", io::ErrorKind::InvalidInput),
        ("missing", io::ErrorKind::NotFound),
        ("dangling", io::ErrorKind::NotFound),
        // Windows answers a path under a file as a path that is not found.
        ("file/under", if cfg!(windows) { io::ErrorKind::NotFound } else { io::ErrorKind::InvalidInput }),
    ];
    #[cfg(unix)]
    cases.push(("selfloop", io::ErrorKind::InvalidInput));
    for (what, want) in cases {
        let err = write_beneath(&base.join(what), "f.txt", b"data").unwrap_err();
        assert_eq!(err.kind(), want, "{what}");
        if what == "file" {
            assert!(err.to_string().contains("is not a plain folder"), "{err}");
        }
        assert_eq!(std::fs::read(base.join("file")).unwrap(), b"x", "{what}");
    }
}

#[test]
fn a_name_that_is_not_one_plain_entry_is_refused() {
    #[cfg_attr(unix, allow(unused_mut))]
    let mut rels = vec!["..", ".", "", "/x", "a//b", "a/", "a/../b", "a\0b"];
    // Windows names no second folder, drive or stream through one name.
    #[cfg(windows)]
    rels.extend([r"\x", r"a\b", "C:x", "a:stream", "a/b:c"]);
    for rel in rels {
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
        let _ = std::fs::remove_dir(root.join(temp));
        match what {
            "junction" => link_dir(&outside, &root.join(temp)).map(|_| true).unwrap(),
            "folder" => std::fs::create_dir(root.join(temp)).map(|_| true).unwrap(),
            "symlink" => link_file(&target, &root.join(temp)),
            "dangling" => link_file(&outside.join("made.txt"), &root.join(temp)),
            _ => std::fs::hard_link(&target, root.join(temp)).map(|_| true).unwrap(),
        }
    };
    for what in ["symlink", "dangling", "hard link", "junction", "folder"] {
        if !plant(what) {
            continue;
        }
        let got = write_beneath_at(&root, "f.txt", b"data", Some(temp), |_| Ok(()));
        // Windows refuses a create of a file over a folder (a junction is one) as access denied, before the name collides.
        let want = if cfg!(windows) && matches!(what, "junction" | "folder") { io::ErrorKind::PermissionDenied } else { io::ErrorKind::AlreadyExists };
        assert_eq!(kind(got), want, "{what}");
        assert!(std::fs::symlink_metadata(root.join(temp)).is_ok(), "{what}");
        assert_eq!(std::fs::read(&target).unwrap(), b"original", "{what}");
        assert!(!outside.join("made.txt").exists(), "{what}");
        assert!(!root.join("f.txt").exists(), "{what}");
        assert_eq!(entries(&outside), ["keep.txt"], "{what}");
    }
}

#[test]
fn a_link_at_the_final_name_is_replaced_not_followed() {
    for kind in ["symlink", "hard link", "folder link", "directory symlink"] {
        let (_g, root, outside) = setup();
        let target = outside.join("keep.txt");
        std::fs::write(&target, b"original").unwrap_or_else(|e| panic!("{kind}: {e}"));
        match kind {
            "symlink" => {
                if !link_file(&target, &root.join("f.txt")) {
                    continue;
                }
            }
            "hard link" => std::fs::hard_link(&target, root.join("f.txt")).unwrap_or_else(|e| panic!("{kind}: {e}")),
            "folder link" => link_dir(&outside, &root.join("f.txt")).unwrap_or_else(|e| panic!("{kind}: {e}")),
            _ => {
                #[cfg(unix)]
                symlink(&outside, root.join("f.txt")).unwrap_or_else(|e| panic!("{kind}: {e}"));
                #[cfg(windows)]
                if !made(std::os::windows::fs::symlink_dir(&outside, root.join("f.txt"))) {
                    continue;
                }
            }
        }
        write_beneath(&root, "f.txt", b"data").unwrap_or_else(|e| panic!("{kind}: {e}"));
        assert_eq!(std::fs::read(&target).unwrap_or_else(|e| panic!("{kind}: {e}")), b"original", "{kind}");
        assert_eq!(entries(&outside), ["keep.txt"], "{kind}");
        assert_eq!(entries(&root), ["f.txt"], "{kind}");
        let meta = std::fs::symlink_metadata(root.join("f.txt")).unwrap_or_else(|e| panic!("{kind}: {e}"));
        assert!(meta.is_file(), "{kind}");
        assert_eq!(std::fs::read(root.join("f.txt")).unwrap_or_else(|e| panic!("{kind}: {e}")), b"data", "{kind}");
    }
}


#[test]
fn missing_folders_are_created_and_the_path_is_returned() {
    let (_g, root, _outside) = setup();
    let got = write_beneath(&root, "a/b/f.txt", b"data").unwrap();
    assert_eq!(got, root.join("a/b/f.txt"));
    assert_eq!(std::fs::read(&got).unwrap(), b"data");
    assert_eq!(entries(&root.join("a/b")), ["f.txt"]);
    // One-letter names: the rename's argument is shorter than its fixed part.
    for rel in ["x", "a/b/1"] {
        write_beneath(&root, rel, b"one").unwrap();
        assert_eq!(std::fs::read(root.join(rel)).unwrap(), b"one", "{rel}");
    }
}

// On Windows the temporary file cannot be replaced while it is held: its own test is below.
#[cfg(unix)]
#[test]
fn a_temporary_file_replaced_before_the_rename_writes_nothing_outside() {
    let (_g, root, outside) = setup();
    let target = outside.join("keep.txt");
    std::fs::write(&target, b"original").unwrap();
    let temp = ".sot-fixed.tmp";
    // Replaced by a link at either step before the rename: the name holds the link that was renamed into place.
    for step in [Step::Temp, Step::Written] {
        write_beneath_at(&root, "f.txt", b"data", Some(temp), |s| {
            if s == step {
                std::fs::remove_file(root.join(temp)).unwrap();
                symlink(&target, root.join(temp)).unwrap();
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"original", "{step:?}");
        assert_eq!(std::fs::read_link(root.join("f.txt")).unwrap(), target, "{step:?}");
        assert_eq!(entries(&root), ["f.txt"], "{step:?}");
        std::fs::remove_file(root.join("f.txt")).unwrap();
    }
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
    link_dir(to, at).unwrap();
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
        assert_eq!(entries(&moved), ["f.txt"], "depth {depth}");
    }
}

#[test]
fn a_folder_with_another_below_it_swapped_after_its_open_keeps_the_write_inside() {
    // The folder below is present, or missing and made relative to the folder that was opened.
    for below in ["present", "missing"] {
        let (_g, root, outside) = setup();
        std::fs::create_dir_all(root.join("a")).unwrap();
        if below == "present" {
            std::fs::create_dir(root.join("a/d")).unwrap();
            std::fs::create_dir(outside.join("d")).unwrap();
        }
        write_beneath_at(&root, "a/d/f.txt", b"data", None, |s| {
            if s == Step::Folder(0) {
                swap_for_link(&root.join("a"), &outside);
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(std::fs::read(root.join("a.moved/d/f.txt")).unwrap(), b"data", "{below}");
        assert_eq!(entries(&root.join("a.moved/d")), ["f.txt"], "{below}");
        if below == "present" {
            assert_eq!(entries(&outside), ["d"], "{below}");
            assert!(entries(&outside.join("d")).is_empty(), "{below}");
        } else {
            assert!(entries(&outside).is_empty(), "{below}");
        }
    }
}

#[test]
fn a_folder_removed_after_its_open_takes_no_write() {
    let (_g, root, outside) = setup();
    std::fs::create_dir(root.join("d")).unwrap();
    let got = write_beneath_at(&root, "d/f.txt", b"data", None, |s| {
        if s == Step::Folder(0) {
            std::fs::remove_dir(root.join("d")).unwrap();
            link_dir(&outside, &root.join("d")).unwrap();
        }
        Ok(())
    });
    // Windows answers a write into a folder that is being deleted as access denied.
    assert_eq!(kind(got), if cfg!(windows) { io::ErrorKind::PermissionDenied } else { io::ErrorKind::NotFound });
    assert!(entries(&outside).is_empty());
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
fn a_missing_folder_is_opened_after_its_make_like_any_folder() {
    // `Missing(i)` falls after the open found nothing and before the make.
    for depth in [1, 2] {
        let (rel, folder) = if depth == 1 { ("d/f.txt", "d") } else { ("a/d/f.txt", "a/d") };
        // A link put at the name is refused after the make answers that the name exists.
        let (_g, root, outside) = setup();
        if depth == 2 {
            std::fs::create_dir(root.join("a")).unwrap();
        }
        let got = write_beneath_at(&root, rel, b"data", None, |s| {
            if s == Step::Missing(depth - 1) {
                link_dir(&outside, &root.join(folder)).unwrap();
            }
            Ok(())
        });
        assert_eq!(kind(got), io::ErrorKind::InvalidInput, "depth {depth}");
        assert!(entries(&outside).is_empty(), "depth {depth}");
        // A folder put at the name is the one opened.
        let (_g, root, _outside) = setup();
        if depth == 2 {
            std::fs::create_dir(root.join("a")).unwrap();
        }
        write_beneath_at(&root, rel, b"data", None, |s| {
            if s == Step::Missing(depth - 1) {
                std::fs::create_dir(root.join(folder)).unwrap();
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(std::fs::read(root.join(rel)).unwrap(), b"data", "depth {depth}");
    }
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
                    link_file(&target, &root.join("f.txt"));
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


#[test]
fn an_error_at_every_step_leaves_no_temporary_file() {
    // The steps come from a clean run of each layout, so a later step that a clean run passes is covered. Layout "present" has the
    // folders and an old file; layout "missing" has neither, so its run sees the `Missing` steps.
    for layout in ["present", "missing"] {
        let prepare = |root: &Path| {
            if layout == "present" {
                std::fs::create_dir_all(root.join("a/b")).unwrap();
                std::fs::write(root.join("a/b/f.txt"), b"old").unwrap();
            }
        };
        let (_g, root, _outside) = setup();
        prepare(&root);
        let mut steps = Vec::new();
        write_beneath_at(&root, "a/b/f.txt", b"new", None, |s| {
            steps.push(s);
            Ok(())
        })
        .unwrap();
        assert!(steps.contains(&Step::Renamed) && (layout == "present") != steps.contains(&Step::Missing(0)));
        for step in steps {
            let (_g, root, _outside) = setup();
            prepare(&root);
            let got = write_beneath_at(&root, "a/b/f.txt", b"new", None, |s| {
                if s == step { Err(io::Error::other("injected")) } else { Ok(()) }
            });
            assert_eq!(got.unwrap_err().to_string(), "injected", "{layout} {step:?}");
            let b = root.join("a/b");
            if step == Step::Renamed {
                assert_eq!(entries(&b), ["f.txt"], "{layout} {step:?}");
                assert_eq!(std::fs::read(b.join("f.txt")).unwrap(), b"new", "{layout} {step:?}");
            } else if layout == "present" {
                assert_eq!(entries(&b), ["f.txt"], "{layout} {step:?}");
                assert_eq!(std::fs::read(b.join("f.txt")).unwrap(), b"old", "{layout} {step:?}");
            } else if b.exists() {
                assert!(entries(&b).is_empty(), "{layout} {step:?}");
            }
        }
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
                match std::fs::read(root.join("f.txt")) {
                    Ok(got) => {
                        assert!(contents.iter().any(|c| *c == got), "a read of {} bytes is neither writer's", got.len());
                        reads += 1;
                    }
                    // A network folder may answer a stale handle for a name replaced under the read.
                    Err(e) if e.kind() == io::ErrorKind::StaleNetworkFileHandle => {}
                    Err(e) => panic!("a read answered {e:?}"),
                }
            }
            reads
        });
        let finished: Vec<_> = writers.into_iter().map(|w| w.join()).collect();
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        let reads = reader.join();
        assert!(finished.iter().all(|r| r.is_ok()), "a writer erred");
        assert!(reads.unwrap() > 0);
    });
    assert_eq!(entries(&root), ["f.txt"]);
}

#[test]
fn a_folder_at_the_final_name_is_an_error_and_is_kept() {
    let (_g, root, _outside) = setup();
    std::fs::create_dir(root.join("f.txt")).unwrap();
    assert!(write_beneath(&root, "f.txt", b"data").is_err());
    assert!(std::fs::symlink_metadata(root.join("f.txt")).unwrap().is_dir());
    assert_eq!(entries(&root), ["f.txt"]);
}

#[cfg(windows)]
#[test]
fn a_root_spelled_in_another_case_is_its_own_canonical_path() {
    let (_g, root, _outside) = setup();
    let upper = PathBuf::from(root.to_string_lossy().to_ascii_uppercase());
    write_beneath(&upper, "f.txt", b"data").unwrap();
    assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"data");
}

#[cfg(windows)]
#[test]
fn the_temporary_file_cannot_be_renamed_away_while_it_is_written() {
    let (_g, root, outside) = setup();
    let temp = ".sot-fixed.tmp";
    for step in [Step::Temp, Step::Written] {
        write_beneath_at(&root, "f.txt", b"data", Some(temp), |s| {
            if s == step {
                let t = root.join(temp);
                assert!(std::fs::remove_file(&t).is_err());
                assert!(std::fs::rename(&t, outside.join("taken")).is_err());
                assert!(std::fs::rename(&t, root.join("g.txt")).is_err());
                assert!(std::fs::OpenOptions::new().write(true).open(&t).is_err());
                // Another file renamed over its name is refused too.
                let other = root.join("other.txt");
                std::fs::write(&other, b"o").unwrap();
                assert!(std::fs::rename(&other, &t).is_err());
                std::fs::remove_file(&other).unwrap();
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"data", "{step:?}");
        assert_eq!(entries(&root), ["f.txt"], "{step:?}");
        assert!(entries(&outside).is_empty(), "{step:?}");
        std::fs::remove_file(root.join("f.txt")).unwrap();
    }
}

#[cfg(windows)]
#[test]
fn a_final_name_a_reader_holds_open_is_replaced() {
    let (_g, root, _outside) = setup();
    std::fs::write(root.join("f.txt"), b"old").unwrap();
    // std opens with all three sharing modes, delete included.
    let mut held = std::fs::File::open(root.join("f.txt")).unwrap();
    write_beneath(&root, "f.txt", b"new").unwrap();
    assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"new");
    let mut old = Vec::new();
    std::io::Read::read_to_end(&mut held, &mut old).unwrap();
    assert_eq!(old, b"old");
    assert_eq!(entries(&root), ["f.txt"]);
}

#[cfg(windows)]
#[test]
fn a_final_name_held_without_delete_sharing_fails_the_write() {
    use std::os::windows::fs::OpenOptionsExt;
    let (_g, root, _outside) = setup();
    std::fs::write(root.join("f.txt"), b"old").unwrap();
    let _held = std::fs::OpenOptions::new().read(true).share_mode(1 | 2).open(root.join("f.txt")).unwrap();
    let err = write_beneath(&root, "f.txt", b"new").unwrap_err();
    assert_eq!(err.raw_os_error(), Some(5));
    assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"old");
    assert_eq!(entries(&root), ["f.txt"]);
}

#[test]
fn only_a_name_surrogate_tag_is_a_link() {
    const REPARSE: u32 = 0x400;
    for (tag, link) in [
        (0xA000_0003, true), // mount point
        (0xA000_000C, true), // symlink
        (0xA000_001D, true), // another name surrogate
        (0x2000_0001, true), // a surrogate without the Microsoft bit
        (0x9000_601A, false), // cloud placeholder
        (0x8000_0013, false), // deduplicated
        (0x8000_001B, false), // app execution link
    ] {
        assert_eq!(names_another_place(REPARSE, tag), link, "{tag:#x}");
        // Without the reparse attribute the tag is not read.
        assert!(!names_another_place(0x10, tag), "{tag:#x}");
    }
}

#[test]
fn a_folder_is_created_at_its_name_and_the_path_is_returned() {
    let (_g, root, _outside) = setup();
    std::fs::create_dir(root.join("a")).unwrap();
    assert_eq!(create_dir_beneath(&root, "a/new").unwrap(), root.join("a/new"));
    assert!(root.join("a/new").is_dir());
}

#[test]
fn a_link_at_any_parent_takes_no_folder() {
    for depth in [1, 2] {
        #[cfg_attr(unix, allow(unused_mut))]
        let mut cases = vec!["outside folder", "dangling", "file"];
        #[cfg(windows)]
        cases.push("directory symlink");
        for case in cases {
            let (_g, root, outside) = setup();
            let (parent, rel_prefix) = if depth == 1 {
                (root.clone(), "d".to_string())
            } else {
                std::fs::create_dir(root.join("a")).unwrap();
                (root.join("a"), "a/d".to_string())
            };
            let at = parent.join("d");
            match case {
                "outside folder" => link_dir(&outside, &at).unwrap(),
                "dangling" => link_dangling_dir(&at, &outside.join("nothing")),
                #[cfg(windows)]
                "directory symlink" => {
                    if !made(std::os::windows::fs::symlink_dir(&outside, &at)) {
                        continue;
                    }
                }
                _ => std::fs::write(&at, b"x").unwrap(),
            }
            let before = entries(&parent);
            let got = create_dir_beneath(&root, &format!("{rel_prefix}/new"));
            assert_eq!(kind(got), io::ErrorKind::InvalidInput, "depth {depth} {case}");
            assert!(entries(&outside).is_empty(), "depth {depth} {case}");
            assert_eq!(entries(&parent), before, "depth {depth} {case}");
        }
    }
}

#[test]
fn an_entry_at_the_name_is_already_exists() {
    for case in ["file", "folder", "outside folder", "dangling"] {
        let (_g, root, outside) = setup();
        let at = root.join("new");
        match case {
            "file" => std::fs::write(&at, b"x").unwrap(),
            "folder" => std::fs::create_dir(&at).unwrap(),
            "outside folder" => link_dir(&outside, &at).unwrap(),
            _ => link_dangling_dir(&at, &outside.join("nothing")),
        }
        let before = std::fs::symlink_metadata(&at).unwrap().file_type();
        assert_eq!(kind(create_dir_beneath(&root, "new")), io::ErrorKind::AlreadyExists, "{case}");
        assert_eq!(std::fs::symlink_metadata(&at).unwrap().file_type(), before, "{case}");
        assert!(entries(&outside).is_empty(), "{case}");
    }
}

#[test]
fn a_missing_parent_is_not_found_and_nothing_is_made() {
    let (_g, root, _outside) = setup();
    assert_eq!(kind(create_dir_beneath(&root, "a/b/new")), io::ErrorKind::NotFound);
    assert!(entries(&root).is_empty());
}

#[test]
fn a_name_that_is_not_one_plain_entry_takes_no_folder() {
    let (_g, root, _outside) = setup();
    for rel in ["", "..", "a/../b", "/x", "a//b", "a/"] {
        assert_eq!(kind(create_dir_beneath(&root, rel)), io::ErrorKind::InvalidInput, "{rel:?}");
    }
}

#[test]
fn a_parent_swapped_for_a_link_after_its_open_takes_no_folder() {
    for depth in [1, 2] {
        let (_g, root, outside) = setup();
        let (rel, folder) = if depth == 1 { ("d/new", root.join("d")) } else { ("a/d/new", root.join("a/d")) };
        std::fs::create_dir_all(&folder).unwrap();
        create_dir_beneath_at(&root, rel, |s| {
            if s == Step::Folder(depth - 1) {
                swap_for_link(&folder, &outside);
            }
            Ok(())
        })
        .unwrap();
        assert!(entries(&outside).is_empty(), "depth {depth}");
        assert_eq!(entries(&folder.with_file_name("d.moved")), ["new"], "depth {depth}");
    }
}
