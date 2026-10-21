use super::*;
use crate::agents::support_tests::{platform_spelling, self_file_env_guarded};

fn preparation_case(invalid: bool) {
    let _guard = self_file_env_guarded();
    let temp = tempfile::tempdir().unwrap();
    let home = platform_spelling(temp.path());
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    std::env::set_var("XDG_CONFIG_HOME", home.join("config"));
    std::env::set_var("LOCALAPPDATA", home.join("local"));
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    let config = crate::rows::store::app_config_dir();
    assert!(config.starts_with(&home));
    std::fs::create_dir_all(&config).unwrap();
    let cwd = home.join("projects").join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let settings = config.join("settings.toml");
    let trust = home.join(".claude.json");
    let prefix = home.join("projects").to_string_lossy().replace('\\', "/");
    let text = if invalid {
        "[trust]\nroot_prefix = 7\n".to_owned()
    } else {
        format!(
            "[trust] # declaration\nroot_prefix = '{}' # scope\n",
            prefix
        )
    };
    std::fs::write(&settings, text).unwrap();
    let capture = sot_log::test_log::capture();
    let additions = account_spawn_env("claude", "default", &cwd, "fixture-row").unwrap();
    assert!(
        additions.is_empty(),
        "W1 preparation changed the account env"
    );
    if invalid {
        assert!(!trust.exists(), "W1 invalid declaration wrote trust");
        let logged = capture.text();
        assert!(
            logged.contains("settings.toml") && logged.contains("error="),
            "W1 invalid declaration lacked a file-specific diagnostic"
        );
    } else {
        let bytes = std::fs::read(&trust).expect("W1 commented declaration recorded no trust");
        let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let key = cwd.to_string_lossy().replace('\\', "/");
        assert_eq!(
            doc["projects"][key]["hasTrustDialogAccepted"], true,
            "W1 commented declaration missed the child cwd key"
        );
    }
}

#[test]
fn toml_declaration_handles_comments_and_literals() {
    if !sot_platform::test_isolated::run_isolated(
        "agents::env::trust_declaration_controls::toml_declaration_handles_comments_and_literals") { return; }
    preparation_case(false);
    println!("W1 C1 commented declaration PASS");
}

#[test]
fn invalid_declaration_is_diagnostic_and_writes_nothing() {
    if !sot_platform::test_isolated::run_isolated(
        "agents::env::trust_declaration_controls::invalid_declaration_is_diagnostic_and_writes_nothing") { return; }
    preparation_case(true);
    println!("W1 C1 invalid declaration diagnostic PASS");
}
