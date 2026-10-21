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
    assert_eq!(std::fs::read_link(&link).unwrap(), sot_platform::host::paths::simplify_verbatim(target.clone()));
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
