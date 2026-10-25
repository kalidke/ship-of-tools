use super::*;
use std::time::Duration;

struct Fixture {
    dir: tempfile::TempDir,
    env: Env,
    source: PathBuf,
    base: PathBuf,
    caller: PathBuf,
    commit: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_path_buf();
        for folder in ["home", "config", "local", "tmp", "source/scripts/hooks"] {
            std::fs::create_dir_all(d.join(folder)).unwrap();
        }
        std::fs::write(d.join("system"), "").unwrap();
        std::fs::write(d.join("global"), "[gc]\n auto = 0\n[maintenance]\n auto = false\n").unwrap();
        // Retain the OS tools, but no account/session/product configuration or identity.
        let mut env = Env::empty();
        for key in ["PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT"] {
            if let Some(value) = Env::process().get(key) { env.set(key, value); }
        }
        env = env.with("HOME", d.join("home")).with("USERPROFILE", d.join("home"))
            .with("LOCALAPPDATA", d.join("local")).with("APPDATA", d.join("config"))
            .with("XDG_CONFIG_HOME", d.join("config")).with("TMPDIR", d.join("tmp"))
            .with("TEMP", d.join("tmp")).with("TMP", d.join("tmp"))
            .with("GIT_CONFIG_GLOBAL", d.join("global")).with("GIT_CONFIG_SYSTEM", d.join("system"))
            .with("GIT_CONFIG_NOSYSTEM", "1").with("GIT_TERMINAL_PROMPT", "0").with("GIT_ALLOW_PROTOCOL", "file")
            .with("GIT_AUTHOR_NAME", "Fixture").with("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .with("GIT_COMMITTER_NAME", "Fixture").with("GIT_COMMITTER_EMAIL", "fixture@example.invalid");
        let source = d.join("source");
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for rel in ["scripts/setup-publish-guard.sh", "scripts/hooks/pre-push", "scripts/hooks/pre-push-shim", "scripts/hooks/publish-guard-lib.sh"] {
            sot_platform::test_exec::write_executable(&source.join(rel), std::fs::read(root.join(rel)).unwrap());
        }
        let mut f = Self { dir, env, source, base: d.join("base"), caller: d.join("caller with spaces"), commit: String::new() };
        f.git(&f.source, &["init", "-q"]).ok();
        f.git(&f.source, &["config", "core.autocrlf", "false"]).ok();
        f.git(&f.source, &["add", "."]).ok();
        f.git(&f.source, &["-c", "commit.gpgsign=false", "commit", "-qm", "fixture"]).ok();
        f.commit = f.git(&f.source, &["rev-parse", "HEAD"]).ok().stdout.trim().into();
        for target in [&f.base, &f.caller] {
            f.git(&d, &["clone", "-q", f.source.to_str().unwrap(), target.to_str().unwrap()]).ok();
        }
        f.git(&d, &["init", "--bare", "-q", d.join("remote").to_str().unwrap()]).ok();
        f
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> sot_platform::test_wait::Ran {
        let mut c = Command::new(if cfg!(windows) { "git.exe" } else { "git" });
        c.env_clear().envs(self.env.iter()).arg("-C").arg(cwd).args(args);
        sot_platform::test_wait::run_within(&mut c, Duration::from_secs(60))
    }

    fn plan(&self, cwd: &Path) -> Plan {
        Plan::discover(cwd, self.source.to_str().unwrap(), self.env.clone()).unwrap()
    }

    fn policy(&self, plan: &Plan) {
        std::fs::create_dir_all(plan.list.parent().unwrap()).unwrap();
        std::fs::write(&plan.list, "# synthetic only\nglimmerdeny\n").unwrap();
    }

    fn push(&self, checkout: &Path, name: &str, denied: bool) {
        let remote = self.dir.path().join("remote");
        let before = self.git(&remote, &["for-each-ref", "--format=%(refname) %(objectname)"]).ok().stdout;
        let ran = self.git(checkout, &["push", remote.to_str().unwrap(), &format!("HEAD:refs/heads/{name}")]);
        if denied {
            assert!(!ran.status.success() && ran.stderr.contains("list line 2"), "{ran}");
            let after = self.git(&remote, &["for-each-ref", "--format=%(refname) %(objectname)"]).ok().stdout;
            assert_eq!(before, after, "denied destination advanced");
        } else { ran.ok(); }
    }
}

#[test]
fn installer_enrolls_distinct_clones_and_repair_restores_its_missing_hook() {
    let f = Fixture::new();
    let plan = f.plan(&f.caller);
    f.policy(&plan);
    let mut found = Found::default();
    let enrolled = plan.enroll(&f.source, &f.commit, &f.base, &mut found).unwrap();
    assert_eq!(enrolled.clones.len(), 2);
    enrolled.check(&mut found);
    assert!(found.fail.is_none() && found.steps.is_empty(), "{found:?}");
    for clone in [&f.base, &f.caller] {
        f.push(clone, "clean", false);
        f.push(clone, "glimmerdeny", true);
        let location = f.git(clone, &["config", "--local", "--get", "sot.publishList"]).ok().stdout;
        assert_eq!(location.trim(), bash_path(&plan.list).unwrap());
    }
    std::fs::remove_file(enrolled.clones[0].common.join("hooks/pre-push")).unwrap();
    let mut damaged = Found::default();
    enrolled.check(&mut damaged);
    assert!(damaged.fail.is_some(), "a check must not repair the missing hook");
    assert!(!enrolled.clones[0].common.join("hooks/pre-push").exists());
    let mut repaired = Found::default();
    plan.enroll(&f.source, &f.commit, &f.base, &mut repaired).unwrap().check(&mut repaired);
    assert!(repaired.fail.is_none() && repaired.steps.is_empty(), "{repaired:?}");
    f.push(&f.base, "glimmerdeny", true);
}

#[test]
fn installer_records_platform_location_without_creating_policy_and_reports_one_step() {
    let f = Fixture::new();
    let plan = f.plan(&f.caller);
    let expected = f.dir.path().join(if cfg!(windows) { "local/sot/config/publish-list.txt" } else { "config/sot/publish-list.txt" });
    assert_eq!(plan.list, expected);
    if !cfg!(windows) {
        let fallback = Plan::discover(&f.caller, f.source.to_str().unwrap(), f.env.clone().without("XDG_CONFIG_HOME")).unwrap();
        assert_eq!(fallback.list, f.dir.path().join("home/.config/sot/publish-list.txt"));
    }
    let mut found = Found::default();
    plan.enroll(&f.source, &f.commit, &f.base, &mut found).unwrap().check(&mut found);
    assert!(!plan.list.exists());
    let (line, code) = super::super::verdict::line(&found, "");
    assert_eq!(code, 0);
    assert!(line.starts_with("ONE STEP LEFT:") && line.contains("provision"), "{line}");
    let denied = f.git(&f.caller, &["push", f.dir.path().join("remote").to_str().unwrap(), "HEAD:refs/heads/clean"]);
    assert!(!denied.status.success() && denied.stderr.contains("readable regular file"), "{denied}");
    assert!(f.git(&f.dir.path().join("remote"), &["for-each-ref"]).ok().stdout.is_empty());
}

#[test]
fn installer_preserves_hooks_configuration_and_modified_owned_slots() {
    let f = Fixture::new();
    let plan = f.plan(&f.caller);
    f.policy(&plan);
    f.git(&f.caller, &["config", "core.hooksPath", "foreign-hooks"]).ok();
    let config = std::fs::read(f.caller.join(".git/config")).unwrap();
    let mut found = Found::default();
    plan.enroll(&f.source, &f.commit, &f.base, &mut found).unwrap().check(&mut found);
    assert_eq!(std::fs::read(f.caller.join(".git/config")).unwrap(), config);
    assert!(found.fail.is_none() && found.steps.iter().any(|s| s.contains("preserved publish hook conflict")), "{found:?}");
    assert!(!f.caller.join(".git/hooks/pre-push").exists());
    f.git(&f.caller, &["config", "--unset-all", "core.hooksPath"]).ok();
    let mut found = Found::default();
    let enrolled = plan.enroll(&f.source, &f.commit, &f.base, &mut found).unwrap();
    let slot = plan.caller.as_ref().unwrap().common.join("hooks/pre-push");
    std::fs::write(&slot, "# person's edited hook\n").unwrap();
    enrolled.check(&mut found);
    assert!(found.fail.is_some(), "an installed hook changed during the run is FAIL");
    let mut repeated = Found::default();
    plan.enroll(&f.source, &f.commit, &f.base, &mut repeated).unwrap().check(&mut repeated);
    assert_eq!(std::fs::read_to_string(slot).unwrap(), "# person's edited hook\n");
    assert!(repeated.fail.is_none() && !repeated.steps.is_empty(), "{repeated:?}");
}

#[test]
fn discovery_is_read_only_exact_and_deduplicates_linked_worktrees() {
    let f = Fixture::new();
    let before = std::fs::read(f.caller.join(".git/config")).unwrap();
    let env = f.env.clone().with("GIT_DIR", f.base.join(".git")).with("GIT_WORK_TREE", &f.base)
        .with("GIT_CONFIG_COUNT", "1").with("GIT_CONFIG_KEY_0", "remote.origin.url").with("GIT_CONFIG_VALUE_0", "wrong");
    let plan = Plan::discover(&f.caller, f.source.to_str().unwrap(), env).unwrap();
    assert_eq!(plan.caller.as_ref().unwrap().common, f.caller.join(".git").canonicalize().unwrap());
    assert_eq!(std::fs::read(f.caller.join(".git/config")).unwrap(), before);
    assert!(!f.caller.join(".git/hooks/pre-push").exists(), "a later refused gate has no enrollment to undo");
    assert!(f.plan(f.dir.path()).caller.is_none());
    f.git(&f.caller, &["config", "remote.origin.url", "https://example.invalid/owner/repository.git"]).ok();
    let unrelated = f.plan(&f.caller);
    assert!(unrelated.caller.is_none());
    let before = std::fs::read(f.caller.join(".git/config")).unwrap();
    unrelated.enroll(&f.source, &f.commit, &f.base, &mut Found::default()).unwrap();
    assert_eq!(std::fs::read(f.caller.join(".git/config")).unwrap(), before);
    assert!(!f.caller.join(".git/hooks/pre-push").exists());
    let linked = f.dir.path().join("linked");
    f.git(&f.base, &["worktree", "add", "--detach", linked.to_str().unwrap(), "HEAD"]).ok();
    let plan = f.plan(&linked);
    f.policy(&plan);
    let mut found = Found::default();
    let enrolled = plan.enroll(&f.source, &f.commit, &f.base, &mut found).unwrap();
    assert_eq!(enrolled.clones.len(), 1);
    enrolled.check(&mut found);
    assert!(found.fail.is_none() && found.steps.is_empty(), "{found:?}");
    f.push(&linked, "glimmerdeny", true);
    assert!(same_origin("git@github.com:owner/project.git", "https://github.com/Owner/Project"));
    assert!(same_origin("ssh://git@github.com/owner/project.git", "https://github.com/owner/project.git"));
    for wrong in ["https://github.com.evil/owner/project", "https://github.com/owner/project/extra", "https://example.invalid/github.com/owner/project", "https://github.com/owner/project?query"] {
        assert!(!same_origin(wrong, "https://github.com/owner/project"), "{wrong}");
    }
}

#[test]
fn pending_apply_never_claims_unavailable_setup_and_uses_verified_staged_setup_when_present() {
    let f = Fixture::new();
    let plan = f.plan(&f.caller);
    let mut found = Found::default();
    plan.pending(&f.dir.path().join("unavailable"), &f.commit, &f.base, &mut found).unwrap();
    assert!(found.fail.is_none() && found.steps.iter().any(|s| s.contains("enrollment is pending")), "{found:?}");
    assert!(!f.base.join(".git/hooks/pre-push").exists());
    assert!(!f.caller.join(".git/hooks/pre-push").exists());
    assert!(plan.pending(&f.source, &"0".repeat(40), &f.base, &mut Found::default()).is_err(), "an available but unverified source is a failure, not pending enrollment");
    f.policy(&plan);
    let mut found = Found::default();
    plan.pending(&f.source, &f.commit, &f.base, &mut found).unwrap();
    assert!(found.fail.is_none() && found.steps.is_empty(), "{found:?}");
    f.push(&f.caller, "glimmerdeny", true);
    f.push(&f.base, "glimmerdeny", true);
}

#[test]
fn missing_interpreter_unverified_source_and_git_write_errors_are_failures() {
    let f = Fixture::new();
    assert!(interpreter(&f.env.clone().with("PATH", ""), Path::new("missing/git.exe")).is_err());
    let plan = f.plan(&f.caller);
    f.policy(&plan);
    assert!(plan.enroll(&f.source, &"0".repeat(40), &f.base, &mut Found::default()).is_err());
    // A real Git config lock provides an OS-independent write failure, including Windows.
    std::fs::write(f.base.join(".git/config.lock"), "another writer\n").unwrap();
    assert!(plan.enroll(&f.source, &f.commit, &f.base, &mut Found::default()).is_err());
    assert_eq!(std::fs::read_to_string(f.base.join(".git/config.lock")).unwrap(), "another writer\n");
}
