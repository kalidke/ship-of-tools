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
