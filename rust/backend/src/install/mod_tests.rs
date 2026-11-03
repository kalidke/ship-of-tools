use super::*;

#[test]
fn a_commit_is_forty_lower_case_hex_digits() {
    assert!(is_commit(&"a".repeat(40)) && !is_commit(&"A".repeat(40)) && !is_commit("abc") && !is_commit(&"g".repeat(40)));
}

/// Old preludes pass only SOT_INSTALL_TAG. It selects the release, while only the person's version/archive argument
/// pins it or permits a downgrade. Current preludes' implicit marker cannot override those arguments either.
#[test]
fn only_a_runs_arguments_pin_it_even_with_a_bootstrap_tag() {
    let body = {
        let _env = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        sot_platform::test_isolated::run_isolated_until(
            "install::tests::only_a_runs_arguments_pin_it_even_with_a_bootstrap_tag",
            std::time::Instant::now() + std::time::Duration::from_secs(15),
        ).unwrap()
    };
    if !body { return; }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("install.json");
    std::env::set_var("SOT_INSTALL_TAG", "v0.7.0");
    let version_flag = if cfg!(windows) { "-Version" } else { "--version" };
    let archive_flag = if cfg!(windows) { "-Archive" } else { "--archive" };
    for marker in [None, Some("0"), Some("1")] {
        match marker {
            Some(value) => std::env::set_var("SOT_INSTALL_IMPLICIT", value),
            None => std::env::remove_var("SOT_INSTALL_IMPLICIT"),
        }
        for target in [None, Some((version_flag, "v0.7.0")), Some((archive_flag, "pack"))] {
            let mut args = vec!["--pack".to_string(), "pack".to_string()];
            if let Some((flag, value)) = target { args.extend([flag.to_string(), value.to_string()]); }
            let opts = options::parse(&args).unwrap();
            let implicit = implicit_run(&opts);
            let named = !implicit;
            assert_eq!(bootstrap_tag().as_deref(), Some("v0.7.0"));
            assert_eq!(named, target.is_some(), "target={target:?}, marker={marker:?}");

            // The installer's own key order, the pin last, as `set_pin` finds a record.
            let prefix = serde_json::to_string(dir.path()).unwrap();
            std::fs::write(&path, format!("{{\n  \"schema\": 1,\n  \"prefix\": {prefix},\n  \"version\": \"0.7.0\",\n  \"pinned\": true\n}}\n")).unwrap();
            assert_eq!(record::set_pin(&path, dir.path(), named).unwrap(), named);
            let manifest: sot_updater::InstallManifest = serde_json::from_str(&record::read(&path)).unwrap();
            assert_eq!(manifest.pinned == Some(true), named, "the record's pin follows only the arguments");
            let pin = if named { verdict::pin_tail("0.7.0") } else { String::new() };
            let (line, _) = verdict::line(&verdict::Found::default(), &pin);
            assert_eq!(line.contains("pinned at 0.7.0"), named, "{line}");

            let direction = guard_direction(implicit, "0.7.1", "older", "0.7.0", dir.path(), version_flag);
            if named {
                assert!(direction.is_ok(), "an explicit target permits the downgrade");
            } else {
                let stop = direction.unwrap_err();
                assert_eq!(stop.code, 2);
                assert!(stop.detail.contains(&format!("{version_flag} v0.7.0")), "{}", stop.detail);
            }
        }
    }
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
