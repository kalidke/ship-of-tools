//! What a named account gets where the daemon cannot make a symbolic link (Windows without Developer Mode or the
//! symlink privilege): a junction for a folder, and for a file a one-time copy the account then owns.

#[cfg(any(windows, test))]
use std::io;
#[cfg(any(windows, test))]
use std::path::Path;

/// `ERROR_PRIVILEGE_NOT_HELD`: the OS refusal a symlink gets without Developer Mode or the symlink privilege.
#[cfg(any(windows, test))]
const PRIVILEGE_NOT_HELD: i32 = 1314;

/// One shared entry for a daemon whose symlink may be refused. `symlink` is tried first and its result stands, unless
/// the OS refused symlinks outright (error 1314). Then a folder becomes a junction that the daemon proves it can
/// traverse (a process enforcing redirection trust cannot cross one a non-admin made, error 448), else the call fails
/// with the OS reason. A file is copied once and the account owns it after. Each entry is built under a name unique to
/// the call and put in place without replacing anything, so the real name only ever holds a complete entry and a
/// sibling spawn's `AlreadyExists` always means one is there. Nothing is written to the default folder.
#[cfg(any(windows, test))]
pub(super) fn link_with_fallback(
    target: &Path,
    link: &Path,
    target_is_dir: bool,
    symlink: impl Fn(&Path, &Path, bool) -> io::Result<()>,
    junction: impl Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    match symlink(target, link, target_is_dir) {
        Err(err) if err.raw_os_error() == Some(PRIVILEGE_NOT_HELD) => {
            if target_is_dir {
                junction_or_refuse(target, link, junction)
            } else {
                seed_file(target, link)
            }
        }
        other => other,
    }
}

/// `link` with a name no other call in any process shares: the pid, a per-process counter and the entry's own name.
#[cfg(any(windows, test))]
fn unique_beside(link: &Path) -> std::path::PathBuf {
    static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = link.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    link.with_file_name(format!(".{name}.{}-{call}.tmp", std::process::id()))
}

/// Remove this call's own temp junction: the reparse point only, never what it names (`remove_dir` on Windows; the
/// Unix stand-in is a symlink).
#[cfg(any(windows, test))]
fn remove_own(temp: &Path) {
    let _ = std::fs::remove_dir(temp).or_else(|_| std::fs::remove_file(temp));
}

#[cfg(any(windows, test))]
fn junction_or_refuse(target: &Path, link: &Path, junction: impl Fn(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    let temp = unique_beside(link);
    // Whatever stops the call's own temp from being made is never "the entry is there": only the publish says that.
    if let Err(err) = junction(target, &temp) {
        return Err(io::Error::other(format!("could not make the junction {}: {err}", temp.display())));
    }
    if let Err(err) = std::fs::metadata(&temp) {
        remove_own(&temp);
        return Err(io::Error::other(format!("the junction to {} cannot be traversed: {err}", target.display())));
    }
    if let Err(err) = publish_dir(&temp, link) {
        remove_own(&temp);
        return Err(err);
    }
    tracing::debug!(link = %link.display(), "account folder shared through a junction");
    Ok(())
}

/// Move a verified junction to its real name, failing with `AlreadyExists` when anything is there.
#[cfg(windows)]
fn publish_dir(temp: &Path, link: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let wide = |p: &Path| p.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let (from, to) = (wide(temp), wide(link));
    // SAFETY: both buffers are NUL-terminated and outlive the call. Flags 0: no replace, no copy across volumes.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// The Unix stand-in's publish: the temp is a symlink, and `symlink` fails on an existing name.
#[cfg(all(unix, test))]
fn publish_dir(temp: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(std::fs::read_link(temp)?, link)?;
    remove_own(temp);
    Ok(())
}

/// How a named account's `CLAUDE.md` is seeded where symlinks are refused: `false` is a one-time copy the account owns
/// (rule S); `true` is a one-line import stub, `@<the default's CLAUDE.md>`, so the account follows the default's edits.
/// The one line to change once the import form is confirmed to work for a named account.
#[cfg(any(windows, test))]
const CLAUDE_MD_AS_IMPORT_STUB: bool = false;

/// The stub's one line, in the form Claude Code's import reader takes: `@~/.claude/CLAUDE.md`, for the default's file only,
/// `<home>/.claude/CLAUDE.md` as the daemon builds it from the home it resolves (the reader ends a match at a backslash or a
/// space, so a Windows path never imports as written, and this spelling has no path to get right). `None` for any other
/// target, even one in a folder named `.claude` elsewhere: it names another file, so it gets the copy.
#[cfg(any(windows, test))]
fn import_line(target: &Path, home: &Path) -> Option<&'static str> {
    (target == super::accounts::claude_config_dir(home, "").join("CLAUDE.md")).then_some("@~/.claude/CLAUDE.md\n")
}

/// Whether `link` takes the import-stub form under `switch`: only the `CLAUDE.md`.
#[cfg(any(windows, test))]
fn uses_import_stub(switch: bool, link: &Path) -> bool {
    switch && link.file_name().is_some_and(|n| n == "CLAUDE.md")
}

#[cfg(any(windows, test))]
fn seed_file(target: &Path, link: &Path) -> io::Result<()> {
    seed_with(target, link, CLAUDE_MD_AS_IMPORT_STUB, super::accounts::account_home().as_deref())
}

/// Seed `link` from `target` under `switch`: the import stub when the switch is on, the entry is `CLAUDE.md` and the target is
/// the default's under `home` (`import_line`), else a copy.
#[cfg(any(windows, test))]
fn seed_with(target: &Path, link: &Path, switch: bool, home: Option<&Path>) -> io::Result<()> {
    let line = home.filter(|_| uses_import_stub(switch, link)).and_then(|h| import_line(target, h));
    seed_contents(target, link, line)?;
    let name = link.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if line.is_some() {
        tracing::warn!(entry = %name, "symlinks are unavailable here: this account's {name} imports the default's");
    } else {
        tracing::warn!(entry = %name, "symlinks are unavailable here: this account's {name} is a copy it now owns");
    }
    Ok(())
}

/// Put the account's file in place without a reader ever seeing half of it: write a temp file beside it (a copy of
/// `target`, or with `stub` the one-line import of it), then `hard_link` it onto the entry name (`AlreadyExists` means
/// a sibling won), then drop the temp.
#[cfg(any(windows, test))]
fn seed_contents(target: &Path, link: &Path, stub: Option<&str>) -> io::Result<()> {
    let temp = unique_beside(link);
    #[allow(clippy::disallowed_methods, reason = "a data copy of an account's file; the link's target is never a program this process runs")]
    let (written, what) = if let Some(line) = stub {
        (std::fs::write(&temp, line), "write the import of")
    } else {
        (std::fs::copy(target, &temp).map(|_| ()), "copy")
    };
    if let Err(err) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(io::Error::other(format!("could not {what} {} to {}: {err}", target.display(), temp.display())));
    }
    let made = std::fs::hard_link(&temp, link);
    let _ = std::fs::remove_file(&temp);
    made
}

/// A directory junction (no privilege needed, local NTFS only) at `link`, a name the caller owns: create the folder,
/// then set its reparse point.
#[cfg(windows)]
pub(super) fn make_junction(target: &Path, link: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
    // The reparse buffer wants the NT spelling `\??\C:\..`; a verbatim `\\?\` prefix is dropped first.
    let wide: Vec<u16> = target.as_os_str().encode_wide().collect();
    let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
    let print: Vec<u16> = wide.strip_prefix(verbatim.as_slice()).unwrap_or(&wide).to_vec();
    let subst: Vec<u16> = r"\??\".encode_utf16().chain(print.iter().copied()).collect();
    let data_len = u16::try_from(8 + (subst.len() + 1 + print.len() + 1) * 2)
        .map_err(|_| io::Error::other("junction target path too long"))?;
    let mut buf: Vec<u8> = Vec::with_capacity(8 + usize::from(data_len));
    for half in [
        (IO_REPARSE_TAG_MOUNT_POINT & 0xFFFF) as u16,
        (IO_REPARSE_TAG_MOUNT_POINT >> 16) as u16,
        data_len,
        0,
        0,
        (subst.len() * 2) as u16,
        ((subst.len() + 1) * 2) as u16,
        (print.len() * 2) as u16,
    ] {
        buf.extend_from_slice(&half.to_le_bytes());
    }
    for w in subst.iter().chain(&[0]).chain(print.iter()).chain(&[0]) {
        buf.extend_from_slice(&w.to_le_bytes());
    }
    std::fs::create_dir(link)?;
    let set = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(link)
        .and_then(|dir| {
            let mut returned = 0u32;
            // SAFETY: `buf` is a complete REPARSE_DATA_BUFFER that outlives the call; there is no output buffer.
            let ok = unsafe {
                DeviceIoControl(
                    dir.as_raw_handle(),
                    FSCTL_SET_REPARSE_POINT,
                    buf.as_ptr().cast(),
                    buf.len() as u32,
                    std::ptr::null_mut(),
                    0,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
        });
    if set.is_err() {
        let _ = std::fs::remove_dir(link);
    }
    set
}

#[cfg(test)]
mod tests {
    use super::super::accounts::ensure_account_links_with;
    #[cfg(windows)]
    use super::super::support_tests::assert_shared_as_the_box_allows;
    use super::super::support_tests::{link_facts, touch_dir, touch_file};
    use super::*;

    fn refused(_: &Path, _: &Path, _: bool) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(PRIVILEGE_NOT_HELD))
    }

    /// The junction maker: a symlink stands in on Unix, the real call on Windows.
    fn fake_junction(target: &Path, link: &Path) -> io::Result<()> {
        #[cfg(unix)]
        return std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        make_junction(target, link)
    }

    fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let default = home.join(".claude");
        touch_dir(&default.join("skills").join("foo"));
        std::fs::write(default.join("CLAUDE.md"), "global rules").unwrap();
        std::fs::write(default.join("settings.json"), "{\"theme\":\"dark\"}").unwrap();
        // history.jsonl, agents, commands, plugins, output-styles, projects: absent in the default.
        let account = home.join(".claude-auth").join("team");
        touch_dir(&account);
        (tmp, account)
    }

    #[test]
    fn a_refused_symlink_becomes_a_junction_and_copies() {
        if !link_facts().crosses_junction {
            println!("NOT CHECKED: this token cannot cross a junction it makes (os error 448); the real-call test asserts the refusal");
            return;
        }
        let (tmp, account) = setup();
        let home = tmp.path();
        ensure_account_links_with(home, "team", |t, l, d| link_with_fallback(t, l, d, refused, fake_junction)).unwrap();

        assert!(account.join("skills").join("foo").is_dir(), "the folder is reachable through the junction");
        assert_eq!(std::fs::read_to_string(account.join("CLAUDE.md")).unwrap(), "global rules");
        assert_eq!(std::fs::read_to_string(account.join("settings.json")).unwrap(), "{\"theme\":\"dark\"}");
        assert!(!std::fs::symlink_metadata(account.join("settings.json")).unwrap().file_type().is_symlink());
        for absent in ["history.jsonl", "agents", "commands", "plugins", "output-styles", "projects"] {
            assert!(std::fs::symlink_metadata(account.join(absent)).is_err(), "{absent}: the default lacks it");
        }
        let leftovers: Vec<_> = std::fs::read_dir(&account)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    #[test]
    fn a_junction_the_daemon_cannot_traverse_is_removed_and_the_spawn_refused() {
        let (tmp, account) = setup();
        // Made fine, but it leads nowhere a read can follow: the stand-in for os error 448.
        let dangling = |_: &Path, link: &Path| fake_junction(&tmp.path().join("gone"), link);
        let err = ensure_account_links_with(tmp.path(), "team", |t, l, d| {
            link_with_fallback(t, l, d, refused, dangling)
        })
        .unwrap_err();
        assert!(err.contains("could not link"), "{err}");
        assert!(err.contains("junction"), "{err}");
        assert!(std::fs::symlink_metadata(account.join("skills")).is_err(), "the reparse point is removed");
        for name in ["CLAUDE.md", "settings.json"] {
            assert!(std::fs::symlink_metadata(account.join(name)).is_err(), "{name}: a refused folder leaves no copy");
        }
        let left: Vec<_> = std::fs::read_dir(&account).unwrap().collect();
        assert!(left.is_empty(), "the call's own temps are gone: {left:?}");
    }

    #[test]
    fn two_first_spawns_of_one_account_never_refuse_each_other() {
        let crosses = link_facts().crosses_junction;
        for round in 0..100 {
            let (tmp, account) = setup();
            let default = tmp.path().join(".claude");
            std::fs::write(default.join("history.jsonl"), "x".repeat(1 << 20)).unwrap();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let joins: Vec<_> = (0..2)
                .map(|_| {
                    let (home, barrier) = (tmp.path().to_path_buf(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        ensure_account_links_with(&home, "team", |t, l, d| {
                            link_with_fallback(t, l, d, refused, fake_junction)
                        })
                    })
                })
                .collect();
            let results: Vec<_> = joins.into_iter().map(|j| j.join().unwrap()).collect();
            // A box that cannot cross its own junction refuses every call; any other box refuses none.
            for r in &results {
                assert_eq!(r.is_ok(), crosses, "round {round}: {r:?}");
            }
            let names: Vec<_> = std::fs::read_dir(&account)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            if crosses {
                assert!(account.join("skills").join("foo").is_dir());
                assert_eq!(std::fs::read(account.join("history.jsonl")).unwrap().len(), 1 << 20);
                assert!(names.iter().all(|n| !n.contains(".tmp")), "round {round}: temps left: {names:?}");
            } else {
                assert!(names.is_empty(), "round {round}: a refused account holds {names:?}");
            }
        }
    }

    // Hand-made junctions, symlinks, hard links and real files are all left alone, whatever the link maker would do.
    #[cfg(unix)]
    #[test]
    fn every_hand_made_entry_form_is_left_untouched() {
        let (tmp, account) = setup();
        let default = tmp.path().join(".claude");
        std::fs::write(default.join("history.jsonl"), "h").unwrap();
        touch_dir(&default.join("projects"));
        fake_junction(&default.join("skills"), &account.join("skills")).unwrap();
        std::os::unix::fs::symlink(default.join("CLAUDE.md"), account.join("CLAUDE.md")).unwrap();
        std::fs::hard_link(default.join("settings.json"), account.join("settings.json")).unwrap();
        std::fs::write(account.join("history.jsonl"), "own").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("gone"), account.join("projects")).unwrap();
        ensure_account_links_with(tmp.path(), "team", |_, _, _| panic!("no entry may be made")).unwrap();
        assert_eq!(std::fs::read_link(account.join("CLAUDE.md")).unwrap(), default.join("CLAUDE.md"));
        assert_eq!(std::fs::read_to_string(account.join("history.jsonl")).unwrap(), "own");
        assert!(std::fs::symlink_metadata(account.join("projects")).unwrap().file_type().is_symlink());
    }

    #[test]
    fn a_claude_md_is_a_copy_by_default_and_an_import_stub_when_the_switch_says() {
        assert!(!CLAUDE_MD_AS_IMPORT_STUB, "the shipped default is the copy");
        let (tmp, account) = setup();
        let home = tmp.path();
        let target = home.join(".claude").join("CLAUDE.md");
        let copy = account.join("CLAUDE.md");
        seed_contents(&target, &copy, None).unwrap();
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "global rules");
        let stub = account.join("stub").join("CLAUDE.md");
        std::fs::create_dir_all(stub.parent().unwrap()).unwrap();
        seed_contents(&target, &stub, import_line(&target, home)).unwrap();
        assert_eq!(std::fs::read_to_string(&stub).unwrap(), "@~/.claude/CLAUDE.md\n");
        // Either form is published without replacing, and leaves no temp.
        assert_eq!(seed_contents(&target, &stub, import_line(&target, home)).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        let temps: Vec<_> = std::fs::read_dir(stub.parent().unwrap()).unwrap().filter(|e| e.as_ref().unwrap().file_name() != "CLAUDE.md").collect();
        assert!(temps.is_empty(), "temps left: {temps:?}");
    }

    /// Claude Code's import reader: `@` then characters that are not a space or a backslash, with `\ ` read as a space.
    fn imported_path(line: &str) -> String {
        let rest = line.trim_end().strip_prefix('@').unwrap();
        let mut out = String::new();
        let mut chars = rest.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' if chars.peek() == Some(&' ') => {
                    out.push(' ');
                    chars.next();
                }
                '\\' | ' ' => break,
                c => out.push(c),
            }
        }
        out
    }

    #[test]
    fn the_stub_line_is_one_the_import_reader_takes_back_whole() {
        // The default's file, wherever the home is: the home-relative spelling, and it reads back whole.
        let line = import_line(Path::new("/home/a/.claude/CLAUDE.md"), Path::new("/home/a")).unwrap();
        assert_eq!(line, "@~/.claude/CLAUDE.md\n");
        assert_eq!(imported_path(line), "~/.claude/CLAUDE.md");
        // A Windows path as written ends the match at `C:`: why no path is ever written.
        assert_eq!(imported_path(r"@C:\Users\a b\.claude\CLAUDE.md"), "C:");
    }

    #[test]
    fn a_target_that_is_not_the_defaults_gets_the_copy_even_with_the_switch_on() {
        let (tmp, account) = setup();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let target = elsewhere.join("CLAUDE.md");
        std::fs::write(&target, "other rules").unwrap();
        let home = tmp.path();
        assert_eq!(import_line(&target, home), None);
        // A folder named `.claude` elsewhere is another file, not the home's.
        let other = tmp.path().join("x").join(".claude");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("CLAUDE.md"), "x rules").unwrap();
        assert_eq!(import_line(&other.join("CLAUDE.md"), home), None);
        assert_eq!(import_line(Path::new(r"D:\x\.claude\CLAUDE.md"), Path::new(r"C:\Users\a")), None);
        let link = account.join("CLAUDE.md");
        seed_with(&target, &link, true, Some(home)).unwrap();
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "other rules");
        // The default's own file, switch on, is the stub; switch off, the copy.
        let default = tmp.path().join(".claude").join("CLAUDE.md");
        let stub = account.join("a").join("CLAUDE.md");
        std::fs::create_dir_all(stub.parent().unwrap()).unwrap();
        seed_with(&default, &stub, true, Some(home)).unwrap();
        assert_eq!(std::fs::read_to_string(&stub).unwrap(), "@~/.claude/CLAUDE.md\n");
        // No resolved home: the copy.
        let none = account.join("b").join("CLAUDE.md");
        std::fs::create_dir_all(none.parent().unwrap()).unwrap();
        seed_with(&default, &none, true, None).unwrap();
        assert_eq!(std::fs::read_to_string(&none).unwrap(), "global rules");
    }

    #[test]
    fn only_claude_md_takes_the_stub_form() {
        assert!(!uses_import_stub(true, Path::new("a/settings.json")));
        assert!(!uses_import_stub(false, Path::new("a/CLAUDE.md")));
        assert!(uses_import_stub(true, Path::new("a/CLAUDE.md")));
    }

    #[test]
    fn a_symlink_that_works_is_used_and_another_error_is_not_a_fallback() {
        let (_tmp, account) = setup();
        let target = account.join("t");
        touch_file(&target);
        let ok = link_with_fallback(&target, &account.join("l"), false, |_, _, _| Ok(()), |_, _| panic!("no junction"));
        assert!(ok.is_ok());
        let denied = |_: &Path, _: &Path, _: bool| Err(io::Error::from(io::ErrorKind::PermissionDenied));
        let err = link_with_fallback(&target, &account.join("m"), false, denied, |_, _| panic!("no junction"));
        assert_eq!(err.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    /// The real calls, whatever token runs the test: symlinks, junctions plus copies, or the clean refusal.
    #[cfg(windows)]
    #[test]
    fn the_real_calls_give_the_form_this_token_allows() {
        let (tmp, account) = setup();
        let result = crate::agents::accounts::ensure_account_links(tmp.path(), "team");
        assert_shared_as_the_box_allows(
            &result,
            &tmp.path().join(".claude"),
            &account,
            &["skills"],
            &["CLAUDE.md", "settings.json"],
        );
    }

    /// A junction to a verbatim (`\\?\`) target reads back as the plain spelling, and removing it keeps its target.
    #[cfg(windows)]
    #[test]
    fn a_junction_reads_back_and_removes_without_touching_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let target = std::fs::canonicalize(tmp.path()).unwrap().join("t");
        touch_dir(&target.join("foo"));
        let link = tmp.path().join("l");
        make_junction(&target, &link).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(&link).unwrap(), crate::paths::simplify_verbatim(target.clone()));
        if std::fs::metadata(&link).is_ok() {
            assert!(link.join("foo").is_dir());
        }
        std::fs::remove_dir(&link).unwrap();
        assert!(target.join("foo").is_dir());
    }

    #[test]
    fn a_failure_making_the_calls_own_temp_is_never_read_as_a_present_entry() {
        let (tmp, account) = setup();
        let stale = |_: &Path, _: &Path| Err(io::Error::from(io::ErrorKind::AlreadyExists));
        let result = ensure_account_links_with(tmp.path(), "team", |t, l, d| link_with_fallback(t, l, d, refused, stale));
        assert!(result.unwrap_err().contains("could not make the junction"));
        assert!(std::fs::symlink_metadata(account.join("skills")).is_err());
    }

    #[test]
    fn a_failed_copy_leaves_no_temp() {
        let (_tmp, account) = setup();
        let missing = account.join("no-such-source");
        let err = link_with_fallback(&missing, &account.join("settings.json"), false, refused, |_, _| unreachable!());
        assert!(err.unwrap_err().to_string().contains("could not copy"));
        assert_eq!(std::fs::read_dir(&account).unwrap().count(), 0);
    }
}
