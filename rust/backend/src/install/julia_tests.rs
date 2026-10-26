use super::*;
use super::super::observe::tests::{says, Canned, Fake};
use super::super::observe::Live;

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

fn version(v: Option<&str>) -> Fact<String> {
    v.map_or_else(|| Fact::unknown(Unknown::new("julia --startup-file=no -e print(VERSION)", Why::Cut(std::time::Duration::from_secs(60)))), |v| Fact::known(v.to_string()))
}

fn facts(source: Source, refresh: Option<&str>) -> Facts {
    Facts { floor: "1.12".into(), refresh: version(refresh), source, juliaup_on_path: true, floor_channel: false, selected: false, path_julia: "/usr/bin/julia".into() }
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
    let Judged::Step(line) = judge(&facts(Source::Bin(version(Some("1.12.0"))), None)) else { panic!() };
    assert!(line.starts_with("the Julia the comm refresh runs could not be asked") && line.contains("did not answer within 60 s") && line.ends_with("ahead of /usr/bin/julia on PATH"), "{line}");
    let Judged::Step(line) = judge(&facts(Source::Path, None)) else { panic!("a PATH julia that gave no version is a step, not a stop") };
    assert!(line.contains("the daemon could not be asked") || line.contains("the Julia the daemon runs could not be asked"), "{line}");
}

/// A manifest is dropped only on git's answer that it is untracked; a checkout git cannot read keeps its manifests, and
/// one whose question gave no answer is kept and named in the verdict.
#[test]
fn a_manifest_is_dropped_only_when_git_says_untracked() {
    let git = |dir: &Path, args: &[&str]| {
        let ran = sot_platform::test_wait::run_within(std::process::Command::new("git").arg("-C").arg(dir).args(args), std::time::Duration::from_secs(30));
        assert_eq!(ran.status.code(), Some(0), "git {args:?}: {ran}");
    };
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    for env in ["kernel", "repl"] {
        std::fs::create_dir_all(repo.join("julia").join(env)).unwrap();
        std::fs::write(repo.join("julia").join(env).join("Manifest.toml"), "m").unwrap();
    }
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "julia/kernel/Manifest.toml"]);
    let mut found = Found::default();
    let stale: Vec<&str> = stale_manifests(&Live, &repo, false, &mut found).iter().map(|(env, _)| *env).collect();
    assert_eq!(stale, ["repl"], "tracked is kept, untracked is dropped");
    assert!(stale_manifests(&Live, &repo, true, &mut found).is_empty(), "a repair keeps them");
    let plain = dir.path().join("plain");
    std::fs::create_dir_all(plain.join("julia/kernel")).unwrap();
    std::fs::write(plain.join("julia/kernel/Manifest.toml"), "m").unwrap();
    assert!(stale_manifests(&Live, &plain, false, &mut found).is_empty(), "git that cannot tell is no 'untracked'");
    assert!(found.checked.iter().any(|c| c.contains("kept julia/kernel/Manifest.toml")), "{found:?}");
    let fake = Fake::new(sot_platform::host::env::Env::empty());
    fake.answer("ls-files", Canned::Stall);
    let mut found = Found::default();
    assert!(stale_manifests(&fake, &repo, false, &mut found).is_empty(), "a cut question drops nothing");
    assert_eq!(found.checked.len(), 2, "each kept manifest is named: {found:?}");
    assert!(found.checked[0].contains("did not answer within 1 s"), "{found:?}");
    fake.answer("ls-files", says(1, ""));
    assert_eq!(stale_manifests(&fake, &repo, false, &mut Found::default()).len(), 2, "exit 1 is untracked");
}
