use super::*;

/// A build of the line is told by ancestry which way the release a run found moves it, and stops.
#[test]
fn a_build_of_the_line_stops_and_is_told_the_way() {
    let line = build_stop("0.6.7-dev+abc", "v0.6.7", "forward", "--archive", "--version");
    assert!(line.starts_with("this computer runs 0.6.7-dev+abc, a build of the line, not a release: install a pack of the line with --archive <folder>; the newest release, v0.6.7, contains this build: naming it with --version moves this computer forward"), "{line}");
    assert!(build_stop("x", "v1", "unknown: no base clone at /b", "-Archive", "-Version").contains("(no base clone at /b)"));
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(build_direction(dir.path(), "0.6.7-dev+src", "0".repeat(40).as_str()), "unknown: this build names no commit");
    assert!(build_direction(dir.path(), "0.6.7-dev+abcdef1", "0".repeat(40).as_str()).starts_with("unknown: no base clone at"));
}

#[test]
fn a_commit_is_forty_lower_case_hex_digits() {
    assert!(is_commit(&"a".repeat(40)) && !is_commit(&"A".repeat(40)) && !is_commit("abc") && !is_commit(&"g".repeat(40)));
}

/// The installed version is the record's, else the installed sotd's own, so a record cut inside its version hides
/// nothing.
#[test]
fn the_installed_version_is_the_records_first() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(installed_version(dir.path()), "");
    std::fs::write(dir.path().join("install.json"), "{\"version\": \"0.6.6\", \"commit\": \"x\"}").unwrap();
    assert_eq!(installed_version(dir.path()), "0.6.6");
}

/// The words the Windows install reads (the converge's wait, the window's launcher check) are the words the launcher and
/// the apply write, and its launcher code id hashes the files `Get-SotLauncherCodeId` hashes: a change on either side
/// fails here, on every platform, since it reads files.
#[test]
fn the_windows_install_reads_the_words_the_launcher_writes() {
    let launch = include_str!("../../../../scripts/launch-sot.ps1");
    let apply = include_str!("../../../../scripts/sot-apply.ps1");
    let layout = include_str!("../../../../scripts/sot-install-layout.ps1");
    let install = include_str!("windows.rs");
    let reader = include_str!("supervisor.rs");
    assert!(launch.contains("Write-SupLog \"supervisor start (relaunched=$Relaunched) depth=") && launch.contains(" code=$(if ($script:launcherCodeId)"));
    assert!(reader.contains("\"supervisor start\"") && reader.contains("strip_prefix(\"code=\")"));
    assert!(launch.contains("Set-LaunchStatus 'DONE'") && launch.contains("Write-SupLog \"status: $Message\"") && reader.contains("\"status: DONE\""));
    assert!(apply.contains("Write-ApplyLog \"APPLIED $tag") && reader.contains("\"sot-apply: APPLIED {tag}\""));
    assert!(launch.contains("Join-Path $logDir 'launcher.pid'") && install.contains("join(\"launcher.pid\")"));
    let files = "'launch-sot.ps1', 'sot-hosts.ps1', 'sot-install-layout.ps1', 'sot-freshness.ps1', 'sot-lease.ps1'";
    assert!(layout.contains(&format!("foreach ($f in @({files}))")));
    assert!(install.contains(&format!("[{}]", files.replace('\'', "\""))));
}

/// Never backwards unasked, the shared rule: an implicit run refuses an older target, and one it cannot compare while
/// the installed version is readable; a named run goes either way.
#[test]
fn a_run_that_names_no_target_never_moves_back() {
    let p = std::path::Path::new("/p");
    assert_eq!(guard_direction(true, "0.7.1", "older", "0.7.0", p, "--version").unwrap_err().code, 2);
    assert_eq!(guard_direction(true, "0.7.1", "unknown", "0.7.0", p, "-Version").unwrap_err().code, 2);
    assert!(guard_direction(true, "", "unknown", "0.7.0", p, "--version").is_ok(), "a record with no readable version is rewritten");
    assert!(guard_direction(false, "0.7.1", "older", "0.7.0", p, "--version").is_ok(), "a named run moves back");
    assert!(guard_direction(true, "0.7.0", "newer", "0.7.1", p, "--version").is_ok());
}

/// A record that names a version beside a missing binary is still guarded: the prefix is laid down fresh only after
/// the direction says it may be.
#[test]
fn a_missing_binary_beside_a_record_is_guarded() {
    let refuse = || Err(verdict::Stop::at("direction", 2, "older"));
    assert_eq!(meet("0.7.1", false, refuse).unwrap_err().code, 2);
    assert_eq!(meet("0.7.1", true, refuse).unwrap_err().code, 2);
    assert_eq!(meet("", false, refuse).unwrap(), Meet::Fresh, "nothing installed: nothing to guard");
    assert_eq!(meet("0.7.0", false, || Ok(())).unwrap(), Meet::Fresh);
    assert_eq!(meet("0.7.0", true, || Ok(())).unwrap(), Meet::Update);
}


/// A build of the line is told the way without its install being written: a release commit its base clone lacks is not
/// fetched into it (a refused run writes nothing under the prefix), and the advice says it could not tell.
#[test]
fn the_build_advice_writes_nothing_under_the_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let ran = sot_log::test_wait::run_within(
            std::process::Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"]).args(args),
            std::time::Duration::from_secs(30),
        );
        assert_eq!(ran.status.code(), Some(0), "git {args:?}: {ran}");
        ran.stdout.trim().to_string()
    };
    let origin = dir.path().join("origin");
    let o = origin.to_string_lossy().into_owned();
    git(&["init", "-q", &o]);
    git(&["-C", &o, "commit", "-q", "--allow-empty", "-m", "built"]);
    let built = git(&["-C", &o, "rev-parse", "HEAD"]);
    let prefix = dir.path().join("prefix");
    let base = prefix.join("repo/base");
    git(&["clone", "-q", &o, &base.to_string_lossy()]);
    git(&["-C", &o, "commit", "-q", "--allow-empty", "-m", "released"]);
    let release = git(&["-C", &o, "rev-parse", "HEAD"]);
    let objects = |p: &std::path::Path| git(&["-C", &p.to_string_lossy(), "count-objects", "-v"]);
    let before = objects(&base);
    let way = build_direction(&prefix, &format!("0.6.7-dev+{}", &built[..9]), &release);
    assert!(way.starts_with("unknown: "), "{way}");
    assert_eq!(objects(&base), before, "the advice fetched into the install's base clone");
    git(&["-C", &base.to_string_lossy(), "fetch", "-q", "origin"]);
    assert_eq!(build_direction(&prefix, &format!("0.6.7-dev+{}", &built[..9]), &release), "forward");
}
