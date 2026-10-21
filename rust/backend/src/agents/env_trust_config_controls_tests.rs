use super::*;
use crate::agents::support_tests::{platform_spelling, self_file_env_guarded};

fn config_case(kind: &str) {
    let _guard = self_file_env_guarded();
    let temp = tempfile::tempdir().unwrap();
    let home = platform_spelling(temp.path());
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    std::env::set_var("XDG_CONFIG_HOME", home.join("config"));
    std::env::set_var("LOCALAPPDATA", home.join("local"));
    let prefix = home.join("projects");
    let cwd = prefix.join("exact child spelling");
    std::fs::create_dir_all(&cwd).unwrap();
    let inherited = home.join("inherited config");
    std::fs::create_dir_all(&inherited).unwrap();
    if kind == "unset" {
        std::env::remove_var("CLAUDE_CONFIG_DIR");
    } else {
        std::env::set_var("CLAUDE_CONFIG_DIR", &inherited);
    }
    let account = if kind == "named" {
        "fixture"
    } else {
        "default"
    };
    let named = home
        .join(crate::agents::accounts::CLAUDE_ACCOUNTS_DIR)
        .join("fixture");
    std::fs::create_dir_all(&named).unwrap();
    let selected = match kind {
        "unset" => home.join(".claude.json"),
        "named" => named.join(".claude.json"),
        _ => inherited.join(".claude.json"),
    };
    let parent_key = prefix.to_string_lossy().replace('\\', "/");
    let original = serde_json::to_vec(&serde_json::json!({"sentinel": "preserved", "projects": {&parent_key: {"hasTrustDialogAccepted": true}}})).unwrap();
    for file in [
        home.join(".claude.json"),
        inherited.join(".claude.json"),
        named.join(".claude.json"),
    ] {
        std::fs::write(file, &original).unwrap();
    }
    let config = crate::rows::store::app_config_dir();
    std::fs::create_dir_all(&config).unwrap();
    let declaration = toml::to_string(
        &serde_json::json!({"trust": {"root_prefix": prefix.to_str().unwrap()}}),
    )
    .unwrap();
    std::fs::write(config.join("settings.toml"), declaration).unwrap();
    let expected_env = crate::agents::accounts::account_env("claude", account, &home).unwrap();
    let capture = sot_log::test_log::capture();
    assert_eq!(
        account_spawn_env("claude", account, &cwd, "fixture-row").unwrap(),
        expected_env
    );
    let bytes = std::fs::read(&selected).unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let child_key = cwd.to_string_lossy().replace('\\', "/");
    assert_eq!(
        doc["projects"][&child_key]["hasTrustDialogAccepted"], true,
        "W1 effective child config missed the separate exact-cwd key"
    );
    assert_eq!(doc["projects"][parent_key]["hasTrustDialogAccepted"], true);
    assert_eq!(doc["sentinel"], "preserved");
    assert_eq!(doc["projects"].as_object().unwrap().len(), 2);
    for file in [
        home.join(".claude.json"),
        inherited.join(".claude.json"),
        named.join(".claude.json"),
    ] {
        if file != selected {
            assert_eq!(
                std::fs::read(file).unwrap(),
                original,
                "W1 preparation changed an unselected trust file"
            );
        }
    }
    account_spawn_env("claude", account, &cwd, "fixture-row").unwrap();
    assert_eq!(
        std::fs::read(&selected).unwrap(),
        bytes,
        "W1 accepted selected entry was rewritten"
    );
    let log = capture.text();
    assert!(log.contains("outcome=Recorded") && log.contains("outcome=AlreadyTrusted"));
    println!("W1 C3 destination PASS: {kind}; exact child key; parent and unselected bytes preserved; account env unchanged");
}

#[test]
fn inherited_absolute_config_receives_the_record() {
    if !sot_platform::test_isolated::run_isolated(
        "agents::env::trust_config_controls::inherited_absolute_config_receives_the_record",
    ) {
        return;
    }
    config_case("inherited");
}
#[test]
fn unset_config_uses_the_home_level_file() {
    if !sot_platform::test_isolated::run_isolated(
        "agents::env::trust_config_controls::unset_config_uses_the_home_level_file",
    ) {
        return;
    }
    config_case("unset");
}
#[test]
fn named_addition_overrides_inherited_config() {
    if !sot_platform::test_isolated::run_isolated(
        "agents::env::trust_config_controls::named_addition_overrides_inherited_config",
    ) {
        return;
    }
    config_case("named");
}
#[test]
fn config_guard_restores_config_directories_on_unwind() {
    if !sot_platform::test_isolated::run_isolated("agents::env::trust_config_controls::config_guard_restores_config_directories_on_unwind") { return; }
    let before = ["XDG_CONFIG_HOME", "LOCALAPPDATA", "CLAUDE_CONFIG_DIR"].map(std::env::var_os);
    let temp = tempfile::tempdir().unwrap();
    let result = std::panic::catch_unwind(|| {
        let _guard = self_file_env_guarded();
        for key in ["XDG_CONFIG_HOME", "LOCALAPPDATA", "CLAUDE_CONFIG_DIR"] {
            std::env::set_var(key, temp.path());
        }
        panic!("fixture unwind");
    });
    assert!(result.is_err());
    let after = ["XDG_CONFIG_HOME", "LOCALAPPDATA", "CLAUDE_CONFIG_DIR"].map(std::env::var_os);
    assert!(
        before == after,
        "W1 config guard did not restore configuration directories"
    );
    println!("W1 C3 restoration PASS: configuration directories restored on unwind");
}
