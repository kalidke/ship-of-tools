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

/// A repair run keeps the environments' untracked manifests, this checkout's own resolution; any other run drops them.
#[test]
fn a_repair_keeps_the_julia_manifests() {
    let dir = tempfile::tempdir().unwrap();
    let ran = sot_log::test_wait::run_within(std::process::Command::new("git").arg("init").arg("-q").arg(dir.path()), std::time::Duration::from_secs(20));
    assert_eq!(ran.status.code(), Some(0), "{ran}");
    std::fs::create_dir_all(dir.path().join("julia/kernel")).unwrap();
    std::fs::write(dir.path().join("julia/kernel/Manifest.toml"), "resolved\n").unwrap();
    assert_eq!(stale_manifests(dir.path(), false).len(), 1);
    assert!(stale_manifests(dir.path(), true).is_empty());
}

