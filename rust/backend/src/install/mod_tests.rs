use super::*;

#[test]
fn a_commit_is_forty_lower_case_hex_digits() {
    assert!(is_commit(&"a".repeat(40)) && !is_commit(&"A".repeat(40)) && !is_commit("abc") && !is_commit(&"g".repeat(40)));
}

/// Never backwards unasked: an implicit run refuses an older target, and one it cannot compare while the installed
/// version is readable, whether or not its binary is there (the prefix is laid down fresh only after the direction says
/// it may be); a named run goes either way; a build of the line is told by ancestry which way the release moves it,
/// and stops. The installed version is the record's, else the installed sotd's own.
#[test]
fn a_run_that_names_no_target_never_moves_back() {
    let p = std::path::Path::new("/p");
    assert_eq!(guard_direction(true, "0.7.1", "older", "0.7.0", p, "--version").unwrap_err().code, 2);
    assert_eq!(guard_direction(true, "0.7.1", "unknown", "0.7.0", p, "-Version").unwrap_err().code, 2);
    assert!(guard_direction(true, "", "unknown", "0.7.0", p, "--version").is_ok(), "a record with no readable version is rewritten");
    assert!(guard_direction(false, "0.7.1", "older", "0.7.0", p, "--version").is_ok(), "a named run moves back");
    assert!(guard_direction(true, "0.7.0", "newer", "0.7.1", p, "--version").is_ok());

    let refuse = || Err(verdict::Stop::at("direction", 2, "older"));
    assert_eq!(meet("0.7.1", false, refuse).unwrap_err().code, 2);
    assert_eq!(meet("0.7.1", true, refuse).unwrap_err().code, 2);
    assert_eq!(meet("", false, refuse).unwrap(), Meet::Fresh, "nothing installed: nothing to guard");
    assert_eq!(meet("0.7.0", false, || Ok(())).unwrap(), Meet::Fresh);
    assert_eq!(meet("0.7.0", true, || Ok(())).unwrap(), Meet::Update);

    let line = build_stop("0.6.7-dev+abc", "v0.6.7", "forward", "--archive", "--version");
    assert!(line.starts_with("this computer runs 0.6.7-dev+abc, a build of the line, not a release: install a pack of the line with --archive <folder>; the newest release, v0.6.7, contains this build: naming it with --version moves this computer forward"), "{line}");
    assert!(build_stop("x", "v1", "unknown: no base clone at /b", "-Archive", "-Version").contains("(no base clone at /b)"));
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(build_direction(dir.path(), "0.6.7-dev+src", "0".repeat(40).as_str()), "unknown: this build names no commit");
    assert!(build_direction(dir.path(), "0.6.7-dev+abcdef1", "0".repeat(40).as_str()).starts_with("unknown: no base clone at"));

    let dir = tempfile::tempdir().unwrap();
    assert_eq!(installed_version(dir.path()), Ok(String::new()), "no install is no version, not an error");
    std::fs::write(dir.path().join("install.json"), "{\"version\": \"0.6.6\", \"commit\": \"x\"}").unwrap();
    assert_eq!(installed_version(dir.path()), Ok("0.6.6".to_string()));
}

/// The names under `dir`, sorted, to show a stop wrote nothing.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            out.push(e.path().to_string_lossy().into_owned());
            if e.path().is_dir() {
                walk(&e.path(), out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

/// An installed `sotd` whose version cannot be read is an install that is there, never none: a run that names no target
/// stops (exit 2, nothing written), one that names its target goes on. A `sotd` that does not answer in time is the same.
#[test]
fn an_installed_sotd_that_cannot_say_its_version_is_not_no_install() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // The test binary refuses `--version`: an installed program that exits without printing one.
    std::fs::copy(std::env::current_exe().unwrap(), bin.join(pack::exe_name("sotd"))).unwrap();
    let before = tree(dir.path());
    let why = installed_version_within(dir.path(), std::time::Duration::from_secs(30)).unwrap_err();
    assert!(why.contains("printed no version"), "{why}");
    let stop = installed_for_with(Err(why.clone()), dir.path(), true, "--version").unwrap_err();
    assert!(stop.code == 2 && stop.detail.contains("cannot be read") && stop.detail.contains("--version vX.Y.Z"), "{stop:?}");
    assert_eq!(installed_for_with(Err(why), dir.path(), false, "--version").unwrap(), "", "a named target moves either way");
    assert_eq!(tree(dir.path()), before, "the decision wrote nothing");

    #[cfg(unix)]
    {
        let stall = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(stall.path().join("bin")).unwrap();
        sot_platform::test_exec::write_executable(&stall.path().join("bin/sotd"), "#!/bin/sh\nsleep 30\n");
        let start = std::time::Instant::now();
        let why = installed_version_within(stall.path(), std::time::Duration::from_secs(2)).unwrap_err();
        assert!(why.contains("gave no answer") && why.contains("did not finish within"), "{why}");
        assert!(start.elapsed() < std::time::Duration::from_secs(10), "the cut took {:?}", start.elapsed());
    }
}

/// The daemon's endpoint name is asked before the run writes anything: a stalled `session-socket-path`, a failing one and
/// an invalid name each stop the run (exit 1, naming why), and none is read as "no daemon here".
#[test]
fn the_endpoint_name_is_a_decision_input_not_a_default() {
    let (prefix, scratch) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::create_dir_all(prefix.path().join("bin")).unwrap();
    let before = tree(prefix.path());
    let pulse = scratch.path().join("pulse");
    let start = std::time::Instant::now();
    let stalled = cmd::ask_within(&mut cmd::tests::command("stall", &pulse), std::time::Duration::from_secs(2));
    let stop = endpoint_name(stalled, |name| !name.is_empty()).unwrap_err();
    assert!(stop.code == 1 && stop.detail.contains("gave no usable name") && stop.detail.contains("nothing was changed"), "{stop:?}");
    assert!(stop.detail.contains("did not finish within"), "the cut is named: {stop:?}");
    assert!(start.elapsed() < std::time::Duration::from_secs(10), "the cut took {:?}", start.elapsed());
    let said = |code: i32, out: &str| Ok(cmd::Ran { code: Some(code), out: out.into(), err: String::new() });
    assert_eq!(endpoint_name(said(0, "/run/sot.sock\n"), |name| !name.is_empty()).unwrap(), "/run/sot.sock");
    assert!(endpoint_name(said(0, ""), |name| !name.is_empty()).is_err(), "an empty name is no name");
    assert!(endpoint_name(said(1, "/run/sot.sock\n"), |name| !name.is_empty()).is_err(), "a name from a failing exit is no name");
    assert!(endpoint_name(said(0, "C:\\other"), |name| name.starts_with("\\\\.\\pipe\\")).is_err(), "an invalid pipe name is no name");
    assert_eq!(tree(prefix.path()), before, "the stop wrote nothing under the prefix");
}
