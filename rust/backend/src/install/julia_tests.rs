use super::*;

/// The floor is the first major.minor of the kernel's compat, whatever its spelling.
#[test]
fn the_floor_is_the_first_major_minor_of_the_compat() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("julia/kernel")).unwrap();
    for spelling in ["\"1.12.4\"", "\"~1.12\"", "\"^1.12, 1.13\"", "\">= 1.12\""] {
        std::fs::write(dir.path().join("julia/kernel/Project.toml"), format!("[compat]\njulia = {spelling}\n")).unwrap();
        assert_eq!(floor(dir.path()).as_deref(), Some("1.12"), "{spelling}");
    }
    assert!(at_floor("1.12.0", "1.12") && at_floor("1.13.2+0.x64", "1.12") && at_floor("2.0", "1.12"));
    assert!(!at_floor("1.11.7", "1.12") && !at_floor("v1.12", "1.12") && !at_floor("", "1.12"));
    let json: serde_json::Value = serde_json::from_str(r#"{"Default":"release","InstalledChannels":{"release":{"Version":"1.11.7+0.x64.linux.gnu"}}}"#).unwrap();
    assert_eq!(default_version(&json).as_deref(), Some("1.11.7"));
}

fn facts(source: Source, refresh: Option<&str>) -> Facts {
    Facts { floor: "1.12".into(), refresh: refresh.map(String::from), source, juliaup_on_path: true, floor_channel: false, selected: false, path_julia: "/usr/bin/julia".into() }
}

/// The Julia the daemon will run meets the floor, or the run ends ONE STEP LEFT naming the version and the fix; a PATH
/// julia below it ends the run before a step runs on it.
#[test]
fn a_julia_below_the_floor_is_named_with_its_fix() {
    assert_eq!(judge(&facts(Source::Up("1.12.1".into()), Some("1.12.1"))), Judged::Ok);
    let Judged::Step(line) = judge(&facts(Source::Up("1.11.7".into()), Some("1.12.1"))) else { panic!() };
    assert!(line.starts_with("the daemon runs Julia 1.11.7, below the 1.12 the kernel needs: run  juliaup add 1.12"), "{line}");
    let mut f = facts(Source::Up("1.11.7".into()), Some("1.12.1"));
    f.floor_channel = true;
    let Judged::Step(line) = judge(&f) else { panic!() };
    assert!(line.ends_with("run  juliaup default 1.12"), "{line}");
    assert_eq!(judge(&facts(Source::Path, Some("1.10.5"))), Judged::Stop("the daemon runs Julia 1.10.5, below the 1.12 the kernel needs: run  install Julia 1.12 or later and put it first on PATH".into()));
    let mut f = facts(Source::Path, Some("1.10.5"));
    f.selected = true;
    assert!(matches!(judge(&f), Judged::Step(_)));
    let Judged::Step(line) = judge(&facts(Source::Relative("julia".into()), Some("1.12.0"))) else { panic!() };
    assert!(line.contains("SOT_JULIA_BIN=julia is not an absolute path") && line.ends_with("make SOT_JULIA_BIN an absolute path or unset it"), "{line}");
    let Judged::Step(line) = judge(&facts(Source::Bin(Some("1.12.0".into())), None)) else { panic!() };
    assert!(line.starts_with("the Julia the comm refresh runs could not be asked") && line.ends_with("ahead of /usr/bin/julia on PATH"), "{line}");
}

/// The comm step reads no precompile cache: its arguments name the checkout's project, carry `--compiled-modules=no`
/// before `-e`, and keep the same `-e` expression.
#[test]
fn the_comm_step_reads_no_precompile_cache() {
    let checkout = Path::new("/checkout");
    let want = [format!("--project={}", checkout.display()), "--compiled-modules=no".to_string(), "-e".to_string(), "using ShipTools; ShipTools.update_comm()".to_string()];
    assert_eq!(comm_args(checkout), want);
}

#[test]
fn each_install_step_skips_startup_on_path_and_on_a_selected_channel() {
    let dir = Path::new("checkout");
    let mut steps = vec![comm_args(dir)];
    for (env, code) in ENVS {
        steps.push(vec![format!("--project=julia/{env}"), "-e".into(), code.into()]);
    }
    for args in steps {
        let args: Vec<_> = args.iter().map(String::as_str).collect();
        for selected in [None, Some(CHANNEL)] {
            let command = step_command(selected, dir, &args);
            let mut want = Vec::new();
            if let Some(channel) = selected {
                want.push(format!("+{channel}"));
            }
            want.push("--startup-file=no".into());
            want.extend(args.iter().map(|arg| (*arg).to_string()));
            assert_eq!(command.get_args().collect::<Vec<_>>(), want.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>());
        }
    }
}

#[cfg(unix)]
#[test]
fn source_check_reports_the_package_after_julia_diagnostics() {
    if !sot_platform::test_isolated::run_isolated("install::julia::tests::source_check_reports_the_package_after_julia_diagnostics") { return; }
    let dir = tempfile::tempdir().unwrap();
    let julia = dir.path().join("julia");
    sot_platform::test_exec::write_executable(&julia, "#!/bin/sh\ncat \"${0%/*}/stderr\" >&2\nexit 1\n");
    std::env::set_var("SOT_JULIA_BIN", &julia);
    for (stderr, expected) in [
        ("loading Pkg\nSOT-SOURCE-CHECK: missing source for Leaf\nanother diagnostic\n", "missing source for Leaf"),
        ("\nfirst diagnostic\nsecond diagnostic\n", "first diagnostic"),
        ("", "Julia did not confirm package sources"),
    ] {
        std::fs::write(dir.path().join("stderr"), stderr).unwrap();
        let why = check_environments(dir.path()).unwrap_err();
        assert_eq!(why, format!("julia/kernel: {expected}; rerun this installer to instantiate its packages"));
    }
}

#[test]
fn each_source_check_allows_two_minutes() {
    assert_eq!(SOURCE_CHECK_BOUND, std::time::Duration::from_secs(120));
}
