use super::*;
use crate::paths::{EnvGuard, ENV_TEST_LOCK};

// --- looks_like_fake_julia (Unix: executable-bit check) ---------------

#[cfg(unix)]
#[test]
fn rejects_a_non_executable_file_regardless_of_size() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("julia");
    // Deliberately LARGE (proves the old 4 KiB rule is gone — size is
    // no longer part of the check at all): only the exec bit matters.
    std::fs::write(&path, vec![0u8; 64 * 1024]).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&path, perms).unwrap();
    assert!(looks_like_fake_julia(&path).is_some());
}

#[cfg(unix)]
#[test]
fn accepts_a_tiny_executable_file() {
    // Proves the old size floor is gone from the other direction too:
    // a small but genuinely executable file is accepted.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("julia");
    sot_log::test_exec::write_executable(&path, b"#!/bin/sh\nexit 0\n");
    assert_eq!(looks_like_fake_julia(&path), None);
}

// --- resolve_juliaup_julia_from_config (table test) --------------------

fn write_fake_juliaup_home(juliaup_dir: &Path, version_dir_name: &str) {
    let bin_dir = juliaup_dir.join(version_dir_name).join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let exe_name = if cfg!(windows) { "julia.exe" } else { "julia" };
    sot_log::test_exec::write_executable(&bin_dir.join(exe_name), vec![0u8; 8192]);
}

#[test]
fn resolves_the_default_channel_real_binary() {
    let dir = tempfile::tempdir().unwrap();
    let juliaup_dir = dir.path();
    write_fake_juliaup_home(juliaup_dir, "julia-1.12.6+0.x64.linux.gnu");
    let config = r#"{
            "Default": "release",
            "InstalledChannels": { "release": { "Version": "1.12.6+0.x64.linux.gnu" } },
            "InstalledVersions": { "1.12.6+0.x64.linux.gnu": { "Path": "./julia-1.12.6+0.x64.linux.gnu" } }
        }"#;
    let resolved = resolve_juliaup_julia_from_config(juliaup_dir, config).unwrap();
    let exe_name = if cfg!(windows) { "julia.exe" } else { "julia" };
    assert_eq!(
        resolved,
        juliaup_dir.join("julia-1.12.6+0.x64.linux.gnu").join("bin").join(exe_name)
    );
}

const APP_CONFIG: &str = r#"{
        "Default": "release",
        "InstalledChannels": { "release": { "Version": "1.12.7+0.aarch64.apple.darwin14" } },
        "InstalledVersions": { "1.12.7+0.aarch64.apple.darwin14": { "Path": "./julia-1.12.7+0.aarch64.apple.darwin14" } }
    }"#;
const APP_VERSION_DIR: &str = "julia-1.12.7+0.aarch64.apple.darwin14";

fn app_bundle_bin(version_dir: &Path) -> PathBuf {
    version_dir.join("Julia-1.12.app").join("Contents").join("Resources").join("julia").join("bin").join(JULIA_EXE)
}

fn write_exe(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    sot_log::test_exec::write_executable(path, vec![0u8; 8192]);
}

#[test]
fn resolves_an_app_bundle_version_dir() {
    let dir = tempfile::tempdir().unwrap();
    let version_dir = dir.path().join(APP_VERSION_DIR);
    write_exe(&app_bundle_bin(&version_dir));
    let resolved = resolve_juliaup_julia_from_config(dir.path(), APP_CONFIG).unwrap();
    assert_eq!(resolved, app_bundle_bin(&version_dir));
}

#[test]
fn an_app_bundle_wins_over_bin_when_both_layouts_exist() {
    let dir = tempfile::tempdir().unwrap();
    let version_dir = dir.path().join(APP_VERSION_DIR);
    write_exe(&app_bundle_bin(&version_dir));
    write_exe(&version_dir.join("bin").join(JULIA_EXE));
    let resolved = resolve_juliaup_julia_from_config(dir.path(), APP_CONFIG).unwrap();
    assert_eq!(resolved, app_bundle_bin(&version_dir));
}

#[test]
fn an_app_entry_without_a_binary_falls_back_to_bin() {
    let dir = tempfile::tempdir().unwrap();
    let version_dir = dir.path().join(APP_VERSION_DIR);
    std::fs::create_dir_all(version_dir.join("Julia-1.12.app").join("Contents")).unwrap();
    let err = resolve_juliaup_julia_from_config(dir.path(), APP_CONFIG).unwrap_err();
    assert!(err.contains("missing on disk") && err.ends_with(&version_dir.join("bin").join(JULIA_EXE).display().to_string()), "{err}");
    write_exe(&version_dir.join("bin").join(JULIA_EXE));
    let resolved = resolve_juliaup_julia_from_config(dir.path(), APP_CONFIG).unwrap();
    assert_eq!(resolved, version_dir.join("bin").join(JULIA_EXE));
}

#[test]
fn juliaup_config_parse_failures_report_a_useful_reason() {
    let dir = tempfile::tempdir().unwrap();
    let cases: &[(&str, &str, &str)] = &[
        ("malformed json", "not json", "juliaup.json"),
        ("missing Default", r#"{"InstalledChannels":{}}"#, "Default"),
        (
            "channel not in InstalledChannels",
            r#"{"Default":"release","InstalledChannels":{}}"#,
            "InstalledChannels",
        ),
        (
            "version not in InstalledVersions",
            r#"{"Default":"release","InstalledChannels":{"release":{"Version":"9.9.9+0"}},"InstalledVersions":{}}"#,
            "InstalledVersions",
        ),
        (
            "resolved binary missing on disk",
            r#"{"Default":"release","InstalledChannels":{"release":{"Version":"1.12.6+0.x64.linux.gnu"}},"InstalledVersions":{"1.12.6+0.x64.linux.gnu":{"Path":"./julia-1.12.6+0.x64.linux.gnu"}}}"#,
            "missing on disk",
        ),
    ];
    for (label, config, expect_substr) in cases {
        let err = resolve_juliaup_julia_from_config(dir.path(), config).unwrap_err();
        assert!(err.contains(expect_substr), "{label}: unexpected message: {err}");
    }
}

// --- resolve_bin (full precedence chain, env-mutating) ------------------

#[test]
fn env_guard_restores_a_set_variable_and_unsets_an_unset_one() {
    let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    const KEY: &str = "SOT_MW14_GUARD_PIN";
    std::env::set_var(KEY, "before");
    {
        let _g = EnvGuard::capture(KEY);
        std::env::set_var(KEY, "during");
        assert_eq!(std::env::var(KEY).as_deref(), Ok("during"));
    }
    assert_eq!(std::env::var(KEY).as_deref(), Ok("before"));
    std::env::remove_var(KEY);
    {
        let _g = EnvGuard::capture(KEY);
        std::env::set_var(KEY, "x");
    }
    assert!(std::env::var_os(KEY).is_none());
}

#[test]
fn sot_julia_bin_wins_outright_when_absolute() {
    let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
    let _g2 = EnvGuard::capture("HOME");
    // An absolute path on THIS platform (a Unix-style "/x/y" is not
    // absolute on Windows and fails the juliaup rule there).
    let abs = std::env::temp_dir().join("explicit-override-julia");
    let abs_str = abs.to_string_lossy().into_owned();
    std::env::set_var("SOT_JULIA_BIN", format!("  {abs_str}  "));
    std::env::set_var("HOME", "/should/not/matter");
    let (bin, source) = resolve_bin().unwrap();
    assert_eq!(bin, abs_str); // trimmed
    assert_eq!(source, "SOT_JULIA_BIN");
}

#[test]
fn sot_julia_bin_relative_is_a_hard_error() {
    let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
    std::env::set_var("SOT_JULIA_BIN", "relative/julia");
    let err = resolve_bin().unwrap_err();
    assert!(matches!(&err, JuliaError::Refused(why) if why.contains("absolute")), "unexpected error: {err:?}");
}

#[test]
fn falls_back_to_juliaup_default_channel_when_no_override() {
    let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let juliaup_dir = home.path().join(".julia").join("juliaup");
    std::fs::create_dir_all(&juliaup_dir).unwrap();
    write_fake_juliaup_home(&juliaup_dir, "julia-1.12.6+0.x64.linux.gnu");
    let config = r#"{
            "Default": "release",
            "InstalledChannels": { "release": { "Version": "1.12.6+0.x64.linux.gnu" } },
            "InstalledVersions": { "1.12.6+0.x64.linux.gnu": { "Path": "./julia-1.12.6+0.x64.linux.gnu" } }
        }"#;
    std::fs::write(juliaup_dir.join("juliaup.json"), config).unwrap();

    let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
    let _g2 = EnvGuard::capture("HOME");
    let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
    std::env::remove_var("SOT_JULIA_BIN");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("JULIAUP_DEPOT_PATH");

    let (bin, source) = resolve_bin().unwrap();
    assert_eq!(source, "juliaup");
    let exe_name = if cfg!(windows) { "julia.exe" } else { "julia" };
    assert_eq!(
        bin,
        juliaup_dir.join("julia-1.12.6+0.x64.linux.gnu").join("bin").join(exe_name).to_string_lossy()
    );
}

#[test]
fn juliaup_depot_path_override_relocates_the_search() {
    let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let depot_root = tempfile::tempdir().unwrap();
    let juliaup_dir = depot_root.path().join("juliaup");
    std::fs::create_dir_all(&juliaup_dir).unwrap();
    write_fake_juliaup_home(&juliaup_dir, "julia-1.10.11+0.x64.linux.gnu");
    let config = r#"{
            "Default": "1.10",
            "InstalledChannels": { "1.10": { "Version": "1.10.11+0.x64.linux.gnu" } },
            "InstalledVersions": { "1.10.11+0.x64.linux.gnu": { "Path": "./julia-1.10.11+0.x64.linux.gnu" } }
        }"#;
    std::fs::write(juliaup_dir.join("juliaup.json"), config).unwrap();

    let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
    let _g2 = EnvGuard::capture("HOME");
    let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
    std::env::remove_var("SOT_JULIA_BIN");
    let unrelated_home = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", unrelated_home.path());
    std::env::set_var("JULIAUP_DEPOT_PATH", depot_root.path());

    let (bin, source) = resolve_bin().unwrap();
    assert_eq!(source, "juliaup");
    assert!(
        PathBuf::from(&bin).starts_with(&juliaup_dir),
        "expected {bin} to resolve under the overridden depot {juliaup_dir:?}"
    );
}

#[test]
fn falls_back_to_path_when_juliaup_absent() {
    let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap(); // no .julia/juliaup here
    let path_dir = tempfile::tempdir().unwrap();
    let exe_name = if cfg!(windows) { "julia.exe" } else { "julia" };
    let real_julia = path_dir.path().join(exe_name);
    sot_log::test_exec::write_executable(&real_julia, vec![0u8; 8192]);

    let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
    let _g2 = EnvGuard::capture("HOME");
    let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
    std::env::remove_var("SOT_JULIA_BIN");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("JULIAUP_DEPOT_PATH");

    let (bin, source) = resolve_bin_on(Some(path_dir.path().as_os_str())).unwrap();
    assert_eq!(source, "PATH");
    assert_eq!(bin, real_julia.to_string_lossy());
}

#[cfg(unix)]
#[test]
fn path_search_skips_a_non_executable_candidate_for_a_real_one_further_along() {
    let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let fake_dir = tempfile::tempdir().unwrap();
    let real_dir = tempfile::tempdir().unwrap();
    std::fs::write(fake_dir.path().join("julia"), b"not executable").unwrap();
    let real_julia = real_dir.path().join("julia");
    sot_log::test_exec::write_executable(&real_julia, vec![0u8; 8192]);

    let joined_path = std::env::join_paths([fake_dir.path(), real_dir.path()]).unwrap();

    let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
    let _g2 = EnvGuard::capture("HOME");
    let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
    std::env::remove_var("SOT_JULIA_BIN");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("JULIAUP_DEPOT_PATH");

    let (bin, source) = resolve_bin_on(Some(joined_path.as_os_str())).unwrap();
    assert_eq!(source, "PATH");
    assert_eq!(bin, real_julia.to_string_lossy());
}

#[cfg(unix)]
#[test]
fn every_path_candidate_rejected_is_an_error_not_a_bare_name_fallback() {
    let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let path_dir = tempfile::tempdir().unwrap();
    std::fs::write(path_dir.path().join("julia"), b"not executable").unwrap();

    let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
    let _g2 = EnvGuard::capture("HOME");
    let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
    std::env::remove_var("SOT_JULIA_BIN");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("JULIAUP_DEPOT_PATH");

    let err = resolve_bin_on(Some(path_dir.path().as_os_str())).unwrap_err();
    assert!(matches!(&err, JuliaError::Refused(why) if why.contains("rejected")), "unexpected error: {err:?}");
}

#[test]
fn no_julia_anywhere_is_an_error_not_a_bare_name() {
    let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let empty_path_dir = tempfile::tempdir().unwrap();

    let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
    let _g2 = EnvGuard::capture("HOME");
    let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
    std::env::remove_var("SOT_JULIA_BIN");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("JULIAUP_DEPOT_PATH");

    let err = resolve_bin_on(Some(empty_path_dir.path().as_os_str())).unwrap_err();
    assert!(matches!(&err, JuliaError::NotFound(why) if why.contains("no `julia` found")), "unexpected error: {err:?}");
}

/// juliaup is installed and its default channel's binary is gone, with no julia on the PATH: a Julia exists and is
/// unusable, so the answer is a refusal, not "none found" (a caller that can live without Julia must not skip it).
#[test]
fn a_juliaup_default_whose_binary_is_gone_is_a_refusal_not_a_miss() {
    let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let juliaup_dir = home.path().join(".julia").join("juliaup");
    std::fs::create_dir_all(&juliaup_dir).unwrap();
    let config = r#"{
            "Default": "release",
            "InstalledChannels": { "release": { "Version": "1.12.6" } },
            "InstalledVersions": { "1.12.6": { "Path": "./julia-1.12.6" } }
        }"#;
    std::fs::write(juliaup_dir.join("juliaup.json"), config).unwrap();
    let empty_path_dir = tempfile::tempdir().unwrap();

    let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
    let _g2 = EnvGuard::capture("HOME");
    let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
    std::env::remove_var("SOT_JULIA_BIN");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("JULIAUP_DEPOT_PATH");

    let err = resolve_bin_on(Some(empty_path_dir.path().as_os_str())).unwrap_err();
    assert!(matches!(&err, JuliaError::Refused(why) if why.contains("missing on disk")), "unexpected error: {err:?}");
}

/// A directory that happens to be called `WindowsApps` (any case) holds an ordinary executable: the name is not
/// evidence of an alias, so the override, the juliaup answer and the PATH search all accept it.
#[test]
fn ordinary_windowsapps_executable_is_accepted() {
    let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
    let _g2 = EnvGuard::capture("HOME");
    let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
    std::env::set_var("HOME", home.path());
    std::env::remove_var("JULIAUP_DEPOT_PATH");

    for folder in ["WindowsApps", "windowsapps"] {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join(folder).join(JULIA_EXE);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        sot_log::test_exec::write_executable(&exe, vec![0u8; 8192]);
        let exe_str = exe.to_string_lossy().into_owned();

        std::env::set_var("SOT_JULIA_BIN", &exe);
        assert_eq!(resolve_bin_on(Some(std::ffi::OsStr::new(""))), Ok((exe_str.clone(), "SOT_JULIA_BIN")), "{folder}: override");

        std::env::remove_var("SOT_JULIA_BIN");
        let path = std::env::join_paths([exe.parent().unwrap()]).unwrap();
        assert_eq!(resolve_bin_on(Some(path.as_os_str())), Ok((exe_str, "PATH")), "{folder}: PATH");
    }

    // A juliaup version kept under such a folder.
    let juliaup_dir = home.path().join(".julia").join("juliaup");
    std::fs::create_dir_all(&juliaup_dir).unwrap();
    let version_dir = home.path().join("WindowsApps").join("jl");
    std::fs::create_dir_all(version_dir.join("bin")).unwrap();
    sot_log::test_exec::write_executable(&version_dir.join("bin").join(JULIA_EXE), vec![0u8; 8192]);
    let config = serde_json::json!({
        "Default": "release",
        "InstalledChannels": { "release": { "Version": "1.12.6" } },
        "InstalledVersions": { "1.12.6": { "Path": version_dir.to_string_lossy() } },
    });
    std::fs::write(juliaup_dir.join("juliaup.json"), config.to_string()).unwrap();
    let (bin, source) = resolve_bin_on(Some(std::ffi::OsStr::new(""))).unwrap();
    assert_eq!(source, "juliaup");
    assert_eq!(bin, version_dir.join("bin").join(JULIA_EXE).to_string_lossy());
}

/// The real-alias cases (Windows): a runner-owned app-execution alias, reached by spellings with no `WindowsApps`
/// component. The alias is found, never faked: none present is a setup failure.
#[cfg(windows)]
mod real_alias {
    use super::*;

    /// An app-execution alias of the account, confirmed by its reparse tag.
    fn real_alias() -> PathBuf {
        let apps = PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("setup: LOCALAPPDATA"))
            .join("Microsoft")
            .join("WindowsApps");
        std::fs::read_dir(&apps)
            .unwrap_or_else(|e| panic!("setup: {} unreadable: {e}", apps.display()))
            .flatten()
            .map(|entry| entry.path())
            .find(|p| alias::tag_of(p).ok().flatten() == Some(alias::APPEXECLINK))
            .expect("setup: no app-execution alias on this runner")
    }

    /// An owned junction to the alias's folder and an owned `julia.exe` link to the alias: neither spelling has a
    /// `WindowsApps` component.
    struct Spellings {
        _dir: tempfile::TempDir,
        via_junction: PathBuf,
        via_link: PathBuf,
        link_dir: PathBuf,
    }

    fn spellings() -> Spellings {
        let alias = real_alias();
        let dir = tempfile::tempdir().unwrap();
        let junction = dir.path().join("j1");
        let mut mklink = std::process::Command::new("cmd");
        mklink.args(["/C", "mklink", "/J"]).arg(&junction).arg(alias.parent().unwrap());
        sot_log::test_wait::run_within(&mut mklink, std::time::Duration::from_secs(30)).ok();
        let link_dir = dir.path().join("links");
        std::fs::create_dir_all(&link_dir).unwrap();
        let via_link = link_dir.join(JULIA_EXE);
        std::os::windows::fs::symlink_file(&alias, &via_link).expect("setup: the owned file link could not be made");
        Spellings {
            via_junction: junction.join(alias.file_name().unwrap()),
            via_link,
            link_dir,
            _dir: dir,
        }
    }

    #[test]
    fn alternate_alias_override_is_refused() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _g = EnvGuard::capture("SOT_JULIA_BIN");
        let sp = spellings();
        for candidate in [&sp.via_junction, &sp.via_link] {
            std::env::set_var("SOT_JULIA_BIN", candidate);
            let err = resolve_bin_on(Some(std::ffi::OsStr::new(""))).unwrap_err();
            assert!(matches!(&err, JuliaError::Refused(why) if why.contains("app-execution alias")), "{}: {err:?}", candidate.display());
        }
    }

    #[test]
    fn alternate_alias_juliaup_is_refused() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let sp = spellings();
        let juliaup_dir = home.path().join(".julia").join("juliaup");
        std::fs::create_dir_all(&juliaup_dir).unwrap();
        let version_dir = sp.link_dir.parent().unwrap().join("jl");
        std::fs::create_dir_all(version_dir.join("bin")).unwrap();
        std::os::windows::fs::symlink_file(&sp.via_link, version_dir.join("bin").join(JULIA_EXE)).unwrap();
        let config = serde_json::json!({
            "Default": "release",
            "InstalledChannels": { "release": { "Version": "1.12.6" } },
            "InstalledVersions": { "1.12.6": { "Path": version_dir.to_string_lossy() } },
        });
        std::fs::write(juliaup_dir.join("juliaup.json"), config.to_string()).unwrap();
        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        let _g2 = EnvGuard::capture("HOME");
        let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
        std::env::remove_var("SOT_JULIA_BIN");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("JULIAUP_DEPOT_PATH");
        let err = resolve_bin_on(Some(std::ffi::OsStr::new(""))).unwrap_err();
        assert!(matches!(&err, JuliaError::Refused(why) if why.contains("app-execution alias")), "{err:?}");
    }

    #[test]
    fn path_skips_alternate_alias() {
        let _serial = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let sp = spellings();
        let real_dir = tempfile::tempdir().unwrap();
        let real = real_dir.path().join(JULIA_EXE);
        sot_log::test_exec::write_executable(&real, vec![0u8; 8192]);
        let _g1 = EnvGuard::capture("SOT_JULIA_BIN");
        let _g2 = EnvGuard::capture("HOME");
        let _g3 = EnvGuard::capture("JULIAUP_DEPOT_PATH");
        std::env::remove_var("SOT_JULIA_BIN");
        std::env::set_var("HOME", home.path());
        std::env::remove_var("JULIAUP_DEPOT_PATH");
        let path = std::env::join_paths([sp.link_dir.as_path(), real_dir.path()]).unwrap();
        assert_eq!(resolve_bin_on(Some(path.as_os_str())), Ok((real.to_string_lossy().into_owned(), "PATH")));
        let only_alias = std::env::join_paths([sp.link_dir.as_path()]).unwrap();
        assert!(matches!(resolve_bin_on(Some(only_alias.as_os_str())), Err(JuliaError::Refused(why)) if why.contains("rejected")));
    }
}
