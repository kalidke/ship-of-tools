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
    assert_eq!(installed_version(dir.path()), "");
    std::fs::write(dir.path().join("install.json"), "{\"version\": \"0.6.6\", \"commit\": \"x\"}").unwrap();
    assert_eq!(installed_version(dir.path()), "0.6.6");
}
