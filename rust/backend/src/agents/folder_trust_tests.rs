//! W1 controls at H: actual spawn preparation, with no Claude launch.
//! File consumption, child-observed keys and interactive recognition remain unproven.

use super::account_spawn_env;
use crate::agents::folder_trust::{claude_trust_file, ensure_folder_trusted};
use crate::agents::support_tests::{platform_spelling, self_file_env_guarded, SelfFileEnvGuard};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

struct Fixture {
    _guard: SelfFileEnvGuard,
    _temp: tempfile::TempDir,
    home: PathBuf,
    prefix: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let guard = self_file_env_guarded();
        let temp = tempfile::tempdir().expect("temporary premise root");
        let root = platform_spelling(temp.path());
        let home = root.join("home");
        let prefix = root.join("projects");
        for dir in [&home, &prefix, &root.join("config"), &root.join("local")] {
            std::fs::create_dir_all(dir).expect("temporary fixture directory");
        }
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("LOCALAPPDATA", root.join("local"));
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        let config = crate::rows::store::app_config_dir();
        assert!(
            config.starts_with(&root),
            "declaration must stay in the fixture"
        );
        std::fs::create_dir_all(&config).unwrap();
        // Ordinary header and slash spelling: this control deliberately does not exercise C1's parser fix.
        let declaration = format!("[trust]\nroot_prefix = \"{}\"\n", project_key(&prefix));
        std::fs::write(config.join("settings.toml"), declaration).unwrap();
        Self {
            _guard: guard,
            _temp: temp,
            home,
            prefix,
        }
    }

    fn repository(&self, name: &str) -> PathBuf {
        let cwd = self.prefix.join(name);
        std::fs::create_dir_all(&cwd).unwrap();
        let mut command = std::process::Command::new("git");
        command
            .args(["init", "--quiet", "--template="])
            .current_dir(&cwd);
        command.env("GIT_CONFIG_NOSYSTEM", "1");
        command.env("GIT_CONFIG_GLOBAL", self.home.join("absent-git-config"));
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        ] {
            command.env_remove(key);
        }
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("SOT_") {
                command.env_remove(key);
            }
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "test-only git fixture initialization; the owned child is bounded and drained"
        )]
        let child = command.spawn().expect("git fixture initialization");
        let (status, _, _) =
            sot_log::test_isolated::drain(child).wait_within(Duration::from_secs(10));
        assert!(status.success(), "git fixture initialization failed");
        assert!(
            cwd.join(".git").is_dir(),
            "fresh git fixture was not initialized"
        );
        cwd
    }

    fn prepare(&self, account: &str, cwd: &Path) {
        let additions = account_spawn_env("claude", account, cwd, "premise-row")
            .expect("actual account preparation");
        if account == "default" {
            assert!(
                additions.is_empty(),
                "default preparation changed the account environment"
            );
        } else {
            let config = crate::agents::accounts::claude_config_dir(&self.home, account);
            assert!(
                additions
                    .iter()
                    .any(|(key, value)| key == "CLAUDE_CONFIG_DIR" && Path::new(value) == config),
                "named preparation changed its config addition"
            );
        }
    }
}

fn project_key(path: &Path) -> String {
    let spelling = path.to_str().expect("Unicode fixture path");
    if cfg!(windows) {
        spelling.replace('\\', "/")
    } else {
        spelling.to_owned()
    }
}

fn read_document(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn config_environment_is_restored() {
    if !sot_log::test_isolated::run_isolated(
        "agents::env::trust_premises::config_environment_is_restored",
    ) {
        return;
    }
    let keys = [
        "HOME",
        "USERPROFILE",
        "XDG_CONFIG_HOME",
        "LOCALAPPDATA",
        "CLAUDE_CONFIG_DIR",
    ];
    let before: Vec<_> = keys.iter().map(std::env::var_os).collect();
    {
        let fixture = Fixture::new();
        fixture.prepare("default", &fixture.repository("restore-control"));
    }
    let after: Vec<_> = keys.iter().map(std::env::var_os).collect();
    assert!(before == after, "W1 fixture leaked its config environment");
    println!("W1 isolation PASS: config environment restored after actual preparation");
}

#[test]
fn p2_existing_records_survive_actual_preparation() {
    if !sot_log::test_isolated::run_isolated(
        "agents::env::trust_premises::p2_existing_records_survive_actual_preparation",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let accepted = fixture.repository("accepted");
    let new_root = fixture.repository("new-root");
    for account in ["default", "fixture"] {
        let trust = claude_trust_file(&fixture.home, account);
        std::fs::create_dir_all(trust.parent().unwrap()).unwrap();
        let before = format!("{{\n \"projects\": {{\n {}: {{\"hasTrustDialogAccepted\": true, \"keep\": 7}},\n \"unrelated\": {{\"keep\": [1, 2]}}\n }}, \"other\": 42\n}}\n",
            serde_json::to_string(&project_key(&accepted)).unwrap());
        std::fs::write(&trust, before.as_bytes()).unwrap();
        fixture.prepare(account, &accepted);
        assert_eq!(
            std::fs::read(&trust).unwrap(),
            before.as_bytes(),
            "W1 P2 accepted record was rewritten"
        );
        assert!(
            !ensure_folder_trusted(&fixture.home, account, &accepted, Some(&fixture.prefix))
                .unwrap()
        );
        let original: Value = serde_json::from_str(&before).unwrap();
        fixture.prepare(account, &new_root);
        let after = read_document(&trust);
        assert_eq!(
            after["projects"][project_key(&accepted)],
            original["projects"][project_key(&accepted)]
        );
        assert_eq!(
            after["projects"]["unrelated"],
            original["projects"]["unrelated"]
        );
        assert_eq!(after["other"], original["other"]);
        assert_eq!(
            after["projects"][project_key(&new_root)]["hasTrustDialogAccepted"],
            true
        );
    }
    println!("W1 P2 preparation PASS: accepted bytes unchanged; unrelated records preserved; default/named controls");
}

#[test]
fn p3_parent_record_keeps_a_separate_child_key() {
    if !sot_log::test_isolated::run_isolated(
        "agents::env::trust_premises::p3_parent_record_keeps_a_separate_child_key",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let child = fixture.repository("child");
    for account in ["default", "fixture"] {
        let trust = claude_trust_file(&fixture.home, account);
        std::fs::create_dir_all(trust.parent().unwrap()).unwrap();
        let parent_record = json!({ "hasTrustDialogAccepted": true, "keep": "parent" });
        let before = json!({ "projects": { project_key(&fixture.prefix): parent_record } });
        std::fs::write(&trust, serde_json::to_vec(&before).unwrap()).unwrap();
        fixture.prepare(account, &child);
        let after = read_document(&trust);
        assert_eq!(
            after["projects"][project_key(&fixture.prefix)],
            parent_record
        );
        assert_eq!(
            after["projects"][project_key(&child)]["hasTrustDialogAccepted"],
            true,
            "W1 P3 separate child key missing"
        );
        assert_eq!(after["projects"].as_object().unwrap().len(), 2);
    }
    println!("W1 P3 preparation PASS: separate exact-cwd child key; parent preserved; default/named controls");
    println!("W1 P1 NOT CHECKED: child-observed spelling and actual file consumption require provisioned Claude");
}

#[cfg(unix)]
#[test]
fn p4_os_resolves_symlinks_and_parent_components() {
    if !sot_log::test_isolated::run_isolated(
        "agents::env::trust_premises::p4_os_resolves_symlinks_and_parent_components",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let inside = fixture.repository("inside");
    let outside = fixture.home.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let alias = fixture.prefix.join("alias");
    let escape = fixture.prefix.join("escape");
    std::os::unix::fs::symlink(&inside, &alias).unwrap();
    std::os::unix::fs::symlink(&outside, &escape).unwrap();
    assert_eq!(platform_spelling(&alias), platform_spelling(&inside));
    assert!(platform_spelling(&alias).starts_with(platform_spelling(&fixture.prefix)));
    assert!(!platform_spelling(&escape).starts_with(platform_spelling(&fixture.prefix)));
    assert!(
        !platform_spelling(&fixture.prefix.join("..").join("home").join("outside"))
            .starts_with(platform_spelling(&fixture.prefix))
    );
    println!("W1 P4 OS comparison PASS: symlink alias converges; symlink and parent-component escapes resolve outside");
}

#[cfg(windows)]
#[test]
fn p4_os_resolves_mixed_directory_case() {
    if !sot_log::test_isolated::run_isolated(
        "agents::env::trust_premises::p4_os_resolves_mixed_directory_case",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let inside = fixture.repository("mixed-case");
    let alias = fixture.prefix.join("MIXED-CASE");
    assert_eq!(
        platform_spelling(&alias),
        platform_spelling(&inside),
        "W1 P4 STOP: canonical comparison does not converge for directory case"
    );
    println!("W1 P4 OS comparison PASS: mixed directory case converges");
    println!("W1 P4 NOT CHECKED: junction, short-name and drive-alias facilities require native witnesses");
}

#[test]
fn p4_parent_component_escape_records_nothing() {
    if !sot_log::test_isolated::run_isolated(
        "agents::env::trust_premises::p4_parent_component_escape_records_nothing",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let outside = fixture.home.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let cwd = fixture.prefix.join("..").join("home").join("outside");
    fixture.prepare("default", &cwd);
    assert!(
        !claude_trust_file(&fixture.home, "default").exists(),
        "W1 P4 scope violation: parent-component escape wrote a trust file"
    );
    println!("W1 P4 preparation PASS: parent-component escape records nothing");
}

#[cfg(unix)]
#[test]
fn p4_symlink_escape_records_nothing() {
    if !sot_log::test_isolated::run_isolated(
        "agents::env::trust_premises::p4_symlink_escape_records_nothing",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let outside = fixture.home.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let cwd = fixture.prefix.join("escape");
    std::os::unix::fs::symlink(&outside, &cwd).unwrap();
    fixture.prepare("default", &cwd);
    assert!(
        !claude_trust_file(&fixture.home, "default").exists(),
        "W1 P4 scope violation: symlink escape wrote a trust file"
    );
    println!("W1 P4 preparation PASS: symlink escape records nothing");
}
