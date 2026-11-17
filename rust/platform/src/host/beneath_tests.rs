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

/// Probe only, never landed: the NT status behind this thread's last refused call (Win32 keeps it beside its own code),
/// 0 where none was recorded.
#[cfg(windows)]
mod last_nt {
    use std::ffi::c_void;

    type Get = unsafe extern "system" fn() -> i32;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleA(name: *const u8) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> Option<Get>;
    }

    pub fn clear() {
        // The status of a success: the next refusal's status is then that call's own.
        unsafe { windows_sys::Win32::Foundation::RtlNtStatusToDosError(0) };
    }

    pub fn status() -> i32 {
        static GET: std::sync::OnceLock<Option<Get>> = std::sync::OnceLock::new();
        let get = GET.get_or_init(|| unsafe { GetProcAddress(GetModuleHandleA(b"ntdll.dll\0".as_ptr()), b"RtlGetLastNtStatus\0".as_ptr()) });
        get.map_or(0, |get| unsafe { get() })
    }
}

#[cfg(not(windows))]
mod last_nt {
    pub fn clear() {}
    pub fn status() -> i32 {
        0
    }
}

/// Probe only: one read of `path` by the calls `std::fs::read` makes (the open, the size, the reads), saying which
/// call was refused and the NT status behind its code. A refused size is counted and not an error, as in std.
fn read_by_steps(path: &Path, sizes_refused: &mut u64) -> Result<Vec<u8>, (&'static str, io::Error, i32)> {
    use std::io::Read;
    last_nt::clear();
    let file = crate::host::open_regular(path).map_err(|e| ("open", e, last_nt::status()))?;
    let size = file.metadata().map(|m| m.len() as usize).unwrap_or_else(|_| {
        *sizes_refused += 1;
        0
    });
    let mut bytes = Vec::with_capacity(size);
    last_nt::clear();
    (&file).take(u64::MAX).read_to_end(&mut bytes).map_err(|e| ("read", e, last_nt::status()))?;
    Ok(bytes)
}

/// Probe only: what one run of writers and readers of one name saw.
struct Tally {
    reads: u64,
    /// One line per refused read: the call, its code and NT status, what the writers were doing, and how soon the
    /// name answered again.
    refusals: Vec<String>,
    /// A refused read that did not answer again within two seconds.
    unanswered: u64,
    sizes_refused: u64,
    writes_refused: Vec<String>,
    counts: String,
}

/// Probe only: `writers` threads each replace one name `writes` times while `readers` threads read it back to back.
/// Every read that answers must be one writer's whole content. A refused read is asked again at once, with no sleep,
/// until it answers or two seconds pass, and is reported with what it took.
fn replace_under_readers(writers: usize, writes: usize, readers: usize, classic_only: bool) -> Tally {
    let (_g, root, _outside) = setup();
    replace_in(&root, writers, writes, readers, classic_only)
}

/// [`replace_under_readers`] in `root`.
fn replace_in(root: &Path, writers: usize, writes: usize, readers: usize, classic_only: bool) -> Tally {
    use std::sync::atomic::Ordering::SeqCst;
    let contents: Vec<Vec<u8>> = (0..writers).map(|w| vec![b'a' + w as u8; (64 - 16 * w) * 1024]).collect();
    std::fs::write(root.join("f.txt"), &contents[0]).unwrap();
    let probe: &'static super::probe::Probe = Box::leak(Box::default());
    // Looked up once here, so the first refusal's status is not the lookup's.
    last_nt::status();
    probe.classic_only.store(classic_only, SeqCst);
    let done = std::sync::atomic::AtomicBool::new(false);
    let asked_again = std::sync::atomic::AtomicU64::new(0);
    let mut tally = Tally { reads: 0, refusals: Vec::new(), unanswered: 0, sizes_refused: 0, writes_refused: Vec::new(), counts: String::new() };
    std::thread::scope(|sc| {
        let writing: Vec<_> = contents
            .iter()
            .map(|c| {
                sc.spawn(move || {
                    super::probe::MINE.with(|m| m.set(Some(probe)));
                    let mut refused = Vec::new();
                    for i in 0..writes {
                        // A refused write says how far it got and how long it took: `Written` after the retry window
                        // is the rename, refused for the whole window.
                        let (mut last, began) = (None, std::time::Instant::now());
                        let wrote = write_beneath_at(root, "f.txt", c, None, |s| {
                            last = Some(s);
                            Ok(())
                        });
                        if let Err(e) = wrote {
                            refused.push(format!("write {i} answered {e:?} after {:?}, its last step {last:?}", began.elapsed()));
                        }
                    }
                    refused
                })
            })
            .collect();
        let reading: Vec<_> = (0..readers)
            .map(|_| {
                sc.spawn(|| {
                    let path = root.join("f.txt");
                    let whole = |got: &[u8]| assert!(contents.iter().any(|c| *c == got), "a read of {} bytes is neither writer's", got.len());
                    let (mut reads, mut refusals, mut unanswered, mut sizes_refused) = (0u64, Vec::new(), 0u64, 0u64);
                    while !done.load(SeqCst) {
                        let before = probe.now();
                        match read_by_steps(&path, &mut sizes_refused) {
                            Ok(got) => {
                                whole(&got);
                                reads += 1;
                            }
                            // A network folder may answer a stale handle for a name replaced under the read.
                            Err((_, e, _)) if e.kind() == io::ErrorKind::StaleNetworkFileHandle => {}
                            Err((call, e, status)) => {
                                let (after, began) = (probe.now(), std::time::Instant::now());
                                let (mut tries, mut others) = (0u32, std::collections::BTreeSet::new());
                                let answered = loop {
                                    tries += 1;
                                    match read_by_steps(&path, &mut sizes_refused) {
                                        Ok(got) => {
                                            whole(&got);
                                            break Some(began.elapsed());
                                        }
                                        Err((call, e, status)) => {
                                            others.insert(format!("{call} {:?} {status:#010x}", e.raw_os_error()));
                                            if began.elapsed() > std::time::Duration::from_secs(2) {
                                                break None;
                                            }
                                        }
                                    }
                                };
                                unanswered += u64::from(answered.is_none());
                                refusals.push(format!(
                                    "read {reads}: a read answered {e:?} at its {call}, NT status {status:#010x}; rename calls in flight, handles \
                                     on the name and renames done [before, after, at the answer]: {before:?} {after:?} {:?}; asked again {tries} \
                                     times, answered after {answered:?}; other answers {others:?}",
                                    probe.now()
                                ));
                            }
                        }
                    }
                    asked_again.fetch_add(crate::host::regular::ASKED_AGAIN.with(|n| n.get()), SeqCst);
                    (reads, refusals, unanswered, sizes_refused)
                })
            })
            .collect();
        for w in writing {
            tally.writes_refused.extend(w.join().expect("a writer panicked"));
        }
        done.store(true, SeqCst);
        for r in reading {
            let (reads, refusals, unanswered, sizes_refused) = r.join().expect("a reader panicked");
            tally.reads += reads;
            tally.refusals.extend(refusals);
            tally.unanswered += unanswered;
            tally.sizes_refused += sizes_refused;
        }
    });
    assert_eq!(entries(root), ["f.txt"]);
    tally.counts = format!(
        "writers {writers} x {writes}, readers {readers}, classic only {classic_only}: reads {}, refused reads {}, unanswered {}, refused \
         sizes {}, renames {}, refused rename attempts {}, fallbacks {}, refused writes {}, opens asked again {}",
        tally.reads,
        tally.refusals.len(),
        tally.unanswered,
        tally.sizes_refused,
        probe.renamed.load(SeqCst),
        probe.refused.load(SeqCst),
        probe.fallbacks.load(SeqCst),
        tally.writes_refused.len(),
        asked_again.load(SeqCst)
    );
    tally
}

#[test]
fn two_writers_of_one_name_each_publish_a_whole_file() {
    let tally = replace_under_readers(2, 200, 1, false);
    eprintln!("two-writers probe: {}", tally.counts);
    for line in tally.refusals.iter().chain(&tally.writes_refused) {
        eprintln!("two-writers probe: {line}");
    }
    assert!(tally.writes_refused.is_empty(), "a writer erred");
    assert!(tally.refusals.is_empty(), "a read was refused");
    assert!(tally.reads > 0);
}

/// Probe only, run by name: more readers and writers of the one name, for a count of refused reads per replace.
#[test]
#[ignore = "probe: run by name"]
fn probe_more_readers_and_writers_of_one_name() {
    let tally = replace_under_readers(4, 1000, 4, false);
    eprintln!("stress probe: {}", tally.counts);
    for line in tally.refusals.iter().chain(&tally.writes_refused) {
        eprintln!("stress probe: {line}");
    }
    assert!(tally.reads > 0);
}

/// Probe only, run by name: the classic replace alone, which is what the fallback does, under one reader. A writer the
/// reader's open handle refuses for the whole retry window is counted, not a failure.
#[cfg(windows)]
#[test]
#[ignore = "probe: run by name"]
fn probe_the_classic_replace_under_a_reader() {
    let tally = replace_under_readers(2, 200, 1, true);
    eprintln!("classic probe: {}", tally.counts);
    for line in tally.refusals.iter().chain(&tally.writes_refused) {
        eprintln!("classic probe: {line}");
    }
    assert!(tally.reads > 0);
}

/// Probe only, started by the test below: a reader in a process of its own, of the name `SOT_PROBE_READ` names, until
/// the file `SOT_PROBE_STOP` names exists or two minutes pass. It prints what it saw.
#[test]
#[ignore = "probe: the child of probe_a_reader_in_another_process"]
fn probe_child_reader() {
    let (Some(path), Some(stop)) = (std::env::var_os("SOT_PROBE_READ"), std::env::var_os("SOT_PROBE_STOP")) else {
        return;
    };
    let (path, stop, began) = (PathBuf::from(path), PathBuf::from(stop), std::time::Instant::now());
    let (mut reads, mut torn, mut sizes_refused, mut refusals) = (0u64, 0u64, 0u64, Vec::new());
    let whole = |got: &[u8]| !got.is_empty() && got.len() % (16 * 1024) == 0 && got.iter().all(|b| *b == got[0]);
    last_nt::status();
    while !(reads % 64 == 0 && (stop.exists() || began.elapsed() > std::time::Duration::from_secs(120))) {
        match read_by_steps(&path, &mut sizes_refused) {
            Ok(got) => torn += u64::from(!whole(&got)),
            Err((call, e, status)) => {
                let (asked, mut tries) = (std::time::Instant::now(), 0u32);
                let answered = loop {
                    tries += 1;
                    if read_by_steps(&path, &mut sizes_refused).is_ok() {
                        break Some(asked.elapsed());
                    }
                    if asked.elapsed() > std::time::Duration::from_secs(2) {
                        break None;
                    }
                };
                refusals.push(format!("read {reads}: a read answered {e:?} at its {call}, NT status {status:#010x}; asked again {tries} times, answered after {answered:?}"));
            }
        }
        reads += 1;
    }
    let again = crate::host::regular::ASKED_AGAIN.with(|n| n.get());
    println!("child probe: reads {reads}, refused reads {}, torn reads {torn}, refused sizes {sizes_refused}, opens asked again {again}", refusals.len());
    for line in refusals {
        println!("child probe: {line}");
    }
}

/// Probe only, run by name: the writers in this process, two readers each in a process of its own.
#[test]
#[ignore = "probe: run by name"]
fn probe_a_reader_in_another_process() {
    let (_g, root, outside) = setup();
    std::fs::write(root.join("f.txt"), vec![b'a'; 64 * 1024]).unwrap();
    let stop = outside.join("stop");
    let children: Vec<_> = (0..2)
        .map(|_| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "host::beneath::tests::probe_child_reader", "--nocapture"])
                .env("SOT_PROBE_READ", root.join("f.txt"))
                .env("SOT_PROBE_STOP", &stop)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    let tally = replace_in(&root, 4, 1000, 0, false);
    std::fs::write(&stop, b"").unwrap();
    eprintln!("child probe: {}", tally.counts);
    for line in &tally.writes_refused {
        eprintln!("child probe: {line}");
    }
    for child in children {
        let (_, out, _) = crate::test_isolated::drain(child).wait_within(std::time::Duration::from_secs(60));
        for line in out.lines().filter(|l| l.starts_with("child probe:")) {
            eprintln!("{line}");
        }
    }
}

/// Probe only: a read of the final name while the writer's handle (share-read alone) is still open on it.
#[cfg(windows)]
#[test]
fn probe_a_read_while_the_writers_handle_is_still_on_the_name() {
    let (_g, root, _outside) = setup();
    // No other test of this binary writes this name, so only this test's writer carries the key.
    let name = "read-at-its-rename.txt";
    std::fs::write(root.join(name), b"old").unwrap();
    let arm = crate::test_hold::Holds::here().arm("platform.beneath.renamed").key(name).hold(1);
    std::thread::scope(|sc| {
        let writer = sc.spawn(|| write_beneath(&root, name, b"new").map_err(|e| e.raw_os_error()));
        arm.reached(0);
        let got = read_by_steps(&root.join(name), &mut 0).map_err(|(call, e, status)| format!("{call} {e:?} {status:#010x}"));
        eprintln!("held-handle probe: {got:?}");
        arm.release(0);
        assert_eq!(writer.join().unwrap(), Ok(root.join(name)));
        assert_eq!(got, Ok(b"new".to_vec()));
    });
}

#[test]
fn a_writer_of_one_name_publishes_while_another_syncs_its_folder() {
    // The second write runs at the first's `Renamed` step, before its folder's sync: by then the first holds no handle on
    // the name (Windows refuses a rename over a name held open without delete sharing).
    let (_g, root, _outside) = setup();
    let mut second = None;
    write_beneath_at(&root, "f.txt", b"first", None, |s| {
        if s == Step::Renamed {
            second = Some(write_beneath(&root, "f.txt", b"second").map_err(|e| e.raw_os_error()));
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(second, Some(Ok(root.join("f.txt"))));
    assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"second");
    assert_eq!(entries(&root), ["f.txt"]);
}

#[cfg(windows)]
#[test]
fn a_writer_stopped_between_its_rename_and_its_close_does_not_fail_another() {
    // The first writer is stopped after its rename, its handle (which shares no deletion) still open on the name, for
    // twice the retry window. The second writer of the name waits its turn and publishes; without the turn its rename
    // is refused for the whole window and it answers a sharing violation (os error 32).
    let (_g, root, _outside) = setup();
    // No other test of this binary writes this name, so only this test's writers carry the key.
    let name = "stopped-at-its-rename.txt";
    let arm = crate::test_hold::Holds::here().arm("platform.beneath.renamed").key(name).hold(1);
    std::thread::scope(|sc| {
        let first = sc.spawn(|| write_beneath(&root, name, b"first").map_err(|e| e.raw_os_error()));
        arm.reached(0);
        let second = sc.spawn(|| write_beneath(&root, name, b"second").map_err(|e| e.raw_os_error()));
        std::thread::sleep(std::time::Duration::from_millis(2 * crate::host::RETRY_DEADLINE_MS));
        arm.release(0);
        assert_eq!(second.join().unwrap(), Ok(root.join(name)));
        assert_eq!(first.join().unwrap(), Ok(root.join(name)));
    });
    assert_eq!(std::fs::read(root.join(name)).unwrap(), b"second");
    assert_eq!(entries(&root), [name]);
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
    // The rename with POSIX semantics answers a sharing violation (32); the classic replace it falls back to answers 5.
    assert_eq!(err.raw_os_error(), Some(32));
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
    for rel in ["new", "a/new"] {
        let (_g, root, _outside) = setup();
        std::fs::create_dir(root.join("a")).unwrap();
        assert_eq!(create_dir_beneath(&root, rel).unwrap(), root.join(rel));
        assert!(root.join(rel).is_dir(), "{rel}");
    }
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

#[test]
fn a_folder_swapped_before_the_create_keeps_the_record_in_the_opened_folder() {
    let (_g, root, outside) = setup();
    let folder = root.join("d");
    std::fs::create_dir(&folder).unwrap();
    write_in_folder(&folder.join("record"), b"new", false, || swap_for_link(&folder, &outside)).unwrap();
    assert_eq!(std::fs::read(root.join("d.moved/record")).unwrap(), b"new");
    assert!(entries(&outside).is_empty());
}

/// A folder cannot be renamed while a file in it is open on Windows, so this swap is Unix only.
#[cfg(unix)]
#[test]
fn a_folder_swapped_after_the_create_keeps_the_record_in_the_opened_folder() {
    let (_g, root, outside) = setup();
    let folder = root.join("d");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(outside.join("record.tmp"), b"keep").unwrap();
    write_in_folder_at(&folder.join("record"), b"new", false, || {}, |s| {
        if s == Step::Temp {
            swap_for_link(&folder, &outside);
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(std::fs::read(root.join("d.moved/record")).unwrap(), b"new");
    assert_eq!(entries(&outside), ["record.tmp"]);
    assert_eq!(std::fs::read(outside.join("record.tmp")).unwrap(), b"keep");
}

#[test]
fn a_record_whose_folder_is_reached_through_a_link_is_written_in_the_target() {
    let (_g, root, outside) = setup();
    link_dir(&outside, &root.join("link")).unwrap();
    write_in_folder(&root.join("link/record"), b"new", false, || {}).unwrap();
    assert_eq!(std::fs::read(outside.join("record")).unwrap(), b"new");
    assert_eq!(entries(&outside), ["record"]);
}

#[cfg(unix)]
#[test]
fn a_fifo_at_the_records_folder_answers_at_once() {
    use std::os::unix::ffi::OsStrExt;
    let (_g, root, _outside) = setup();
    let fifo = root.join("fifo");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: a plain mkfifo(3) of a path in this test's own folder.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    // The write runs on its own thread so a write that blocks fails this test instead of hanging the suite.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(write_in_folder(&fifo.join("record"), b"x", false, || {}));
    });
    let got = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("the write answered within the bound");
    assert_eq!(got.unwrap_err().kind(), io::ErrorKind::NotADirectory);
}

#[test]
fn a_file_at_the_records_folder_answers_at_once() {
    let (_g, root, _outside) = setup();
    std::fs::write(root.join("f"), b"x").unwrap();
    let err = write_in_folder(&root.join("f/record"), b"new", false, || {}).unwrap_err();
    #[cfg(unix)]
    assert_eq!(err.kind(), io::ErrorKind::NotADirectory);
    #[cfg(not(unix))]
    let _ = err;
    assert_eq!(entries(&root), ["f"]);
}
