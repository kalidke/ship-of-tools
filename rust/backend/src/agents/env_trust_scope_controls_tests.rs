use super::*;
use crate::agents::support_tests::{platform_spelling, self_file_env_guarded};

fn scope_case(kind: &str) {
    let _guard = self_file_env_guarded();
    let temp = tempfile::tempdir().unwrap();
    let home = platform_spelling(temp.path());
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    std::env::set_var("XDG_CONFIG_HOME", home.join("config"));
    std::env::set_var("LOCALAPPDATA", home.join("local"));
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    let prefix = home.join("projects");
    let inside = prefix.join("repo");
    let outside = home.join("outside");
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let config = crate::rows::store::app_config_dir();
    std::fs::create_dir_all(&config).unwrap();
    if kind != "undeclared" {
        let text = toml::to_string(
            &serde_json::json!({"trust": {"root_prefix": prefix.to_str().unwrap()}}),
        )
        .unwrap();
        std::fs::write(config.join("settings.toml"), text).unwrap();
    }
    let cwd = match kind {
        "parent" => prefix.join("..").join("outside"),
        #[cfg(unix)]
        "symlink" => {
            let link = prefix.join("escape");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(!platform_spelling(&link).starts_with(platform_spelling(&prefix)));
            link
        }
        "outside" => outside,
        _ => inside,
    };
    let capture = sot_platform::test_log::capture();
    let extra = account_spawn_env("claude", "default", &cwd, "fixture-row").unwrap();
    assert!(extra.is_empty());
    assert!(
        !home.join(".claude.json").exists(),
        "W1 scope violation: resolved escape wrote a trust file"
    );
    let log = capture.text();
    if kind == "undeclared" {
        assert!(
            log.contains("outcome=NotDeclared")
                && log.contains("declaration=")
                && log.contains("settings.toml"),
            "W1 undeclared outcome lacks declaration field"
        );
    } else {
        assert!(
            log.contains("outcome=Outside") && log.contains("cwd=") && log.contains("prefix="),
            "W1 outside outcome lacks scope fields"
        );
    }
}

#[test]
fn parent_component_escape_records_nothing() {
    if !sot_platform::test_isolated::run_isolated(
        "agents::env::trust_scope_controls::parent_component_escape_records_nothing",
    ) {
        return;
    }
    scope_case("parent");
    println!("W1 C2 parent-component PASS: no write; Outside with cwd and prefix");
}
#[cfg(unix)]
#[test]
fn symlink_escape_records_nothing() {
    if !sot_platform::test_isolated::run_isolated(
        "agents::env::trust_scope_controls::symlink_escape_records_nothing",
    ) {
        return;
    }
    scope_case("symlink");
    println!("W1 C2 symlink PASS: no write; Outside with cwd and prefix");
}
#[test]
fn outside_is_observable_and_preserves_the_account_env() {
    if !sot_platform::test_isolated::run_isolated("agents::env::trust_scope_controls::outside_is_observable_and_preserves_the_account_env") { return; }
    scope_case("outside");
    println!("W1 C2 Outside PASS: cwd and prefix fields; account env unchanged");
}
#[test]
fn absent_declaration_is_observable_and_preserves_the_account_env() {
    if !sot_platform::test_isolated::run_isolated("agents::env::trust_scope_controls::absent_declaration_is_observable_and_preserves_the_account_env") { return; }
    scope_case("undeclared");
    println!("W1 C2 NotDeclared PASS: declaration file field; account env unchanged");
}

#[test]
fn a_recorded_or_already_trusted_preparation_is_one_info_line_naming_the_file() {
    if !sot_platform::test_isolated::run_isolated(
        "agents::env::trust_scope_controls::a_recorded_or_already_trusted_preparation_is_one_info_line_naming_the_file",
    ) {
        return;
    }
    let _guard = self_file_env_guarded();
    let temp = tempfile::tempdir().unwrap();
    let home = platform_spelling(temp.path());
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    std::env::set_var("XDG_CONFIG_HOME", home.join("config"));
    std::env::set_var("LOCALAPPDATA", home.join("local"));
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    let prefix = home.join("projects");
    let inside = prefix.join("repo");
    std::fs::create_dir_all(&inside).unwrap();
    let config = crate::rows::store::app_config_dir();
    std::fs::create_dir_all(&config).unwrap();
    let text = toml::to_string(
        &serde_json::json!({"trust": {"root_prefix": prefix.to_str().unwrap()}}),
    )
    .unwrap();
    std::fs::write(config.join("settings.toml"), text).unwrap();
    let trust_file = home.join(".claude.json");
    let named = format!("{:?}", trust_file);

    let first = sot_platform::test_log::capture();
    account_spawn_env("claude", "default", &inside, "fixture-row").unwrap();
    let recorded = first.text();
    drop(first);
    assert!(trust_file.exists(), "a first spawn inside the prefix records the entry");
    let lines: Vec<&str> = recorded.lines().filter(|l| l.contains("folder trust")).collect();
    assert_eq!(lines.len(), 1, "one folder trust line per preparation: {recorded}");
    assert!(lines[0].contains("INFO") && lines[0].contains("outcome=Recorded"), "{}", lines[0]);
    assert!(lines[0].contains(&named) && lines[0].contains("fixture-row"), "{}", lines[0]);

    let second = sot_platform::test_log::capture();
    account_spawn_env("claude", "default", &inside, "fixture-row").unwrap();
    let already = second.text();
    drop(second);
    let lines: Vec<&str> = already.lines().filter(|l| l.contains("folder trust")).collect();
    assert_eq!(lines.len(), 1, "one folder trust line per preparation: {already}");
    assert!(lines[0].contains("INFO") && lines[0].contains("outcome=AlreadyTrusted"), "{}", lines[0]);
    assert!(lines[0].contains(&named) && lines[0].contains("fixture-row"), "{}", lines[0]);
}

#[test]
fn a_claude_preparation_marks_its_trust_file_and_no_other_file() {
    if !sot_platform::test_isolated::run_isolated(
        "agents::env::trust_scope_controls::a_claude_preparation_marks_its_trust_file_and_no_other_file",
    ) {
        return;
    }
    use crate::agents::folder_trust::claude_prepared_within;
    use std::time::Duration;
    let _guard = self_file_env_guarded();
    let temp = tempfile::tempdir().unwrap();
    let home = platform_spelling(temp.path());
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    std::env::set_var("XDG_CONFIG_HOME", home.join("config"));
    std::env::set_var("LOCALAPPDATA", home.join("local"));
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    let cwd = home.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let file = home.join(".claude.json");
    let other = home.join("other").join(".claude.json");
    let minute = Duration::from_secs(60);
    assert!(!claude_prepared_within(&file, minute), "nothing prepared yet");

    account_spawn_env("claude", "default", &cwd, "fixture-row").unwrap();
    assert!(claude_prepared_within(&file, minute), "a Claude preparation marks its trust file, declared or not");
    assert!(!claude_prepared_within(&other, minute), "another file stays unmarked");
    std::thread::sleep(Duration::from_millis(60));
    assert!(!claude_prepared_within(&file, Duration::from_millis(30)), "the answer turns false after the time");

    let before = std::time::Instant::now();
    account_spawn_env("codex", "default", &cwd, "fixture-row").unwrap();
    assert!(!claude_prepared_within(&file, before.elapsed()), "a Codex preparation marks nothing");
}
