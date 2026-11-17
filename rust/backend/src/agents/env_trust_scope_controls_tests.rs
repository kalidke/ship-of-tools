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
    assert!(!claude_prepared_within(&file, Duration::ZERO), "the answer turns false after the time");

    let before = std::time::Instant::now();
    account_spawn_env("codex", "default", &cwd, "fixture-row").unwrap();
    assert!(!claude_prepared_within(&file, before.elapsed()), "a Codex preparation marks nothing");
}

/// A fixture: a ready layer under the guarded comm folder (outside any declared prefix), and a declaration of `prefix`
/// when asked. Returns the home, the row's folder, the layer and the trust file.
fn layer_fixture(declare: bool) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let home = platform_spelling(temp.path());
    std::mem::forget(temp);
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    std::env::set_var("XDG_CONFIG_HOME", home.join("config"));
    std::env::set_var("LOCALAPPDATA", home.join("local"));
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    let prefix = home.join("projects");
    let inside = prefix.join("repo");
    std::fs::create_dir_all(&inside).unwrap();
    let comm = std::path::PathBuf::from(std::env::var_os("SOT_COMM_HOME").unwrap());
    let skill = comm.join("claude-layer").join(".claude").join("skills").join("sot-session-start");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "---\nname: sot-session-start\n---\n").unwrap();
    let layer = platform_spelling(&comm.join("claude-layer"));
    assert!(!layer.starts_with(&prefix), "the layer lies outside the declared prefix");
    if declare {
        let config = crate::rows::store::app_config_dir();
        std::fs::create_dir_all(&config).unwrap();
        let text = toml::to_string(&serde_json::json!({"trust": {"root_prefix": prefix.to_str().unwrap()}})).unwrap();
        std::fs::write(config.join("settings.toml"), text).unwrap();
    }
    let trust_file = home.join(".claude.json");
    (home, inside, layer, trust_file)
}

fn accepted(file: &Path, folder: &Path) -> bool {
    let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    doc["projects"][folder.to_str().unwrap()]["hasTrustDialogAccepted"] == serde_json::Value::Bool(true)
}

#[test]
fn a_declared_box_records_the_ready_layer_beside_the_row_and_an_undeclared_one_neither() {
    if !sot_platform::test_isolated::run_isolated(
        "agents::env::trust_scope_controls::a_declared_box_records_the_ready_layer_beside_the_row_and_an_undeclared_one_neither",
    ) {
        return;
    }
    let _guard = self_file_env_guarded();
    let (_home, inside, layer, trust_file) = layer_fixture(true);
    let capture = sot_platform::test_log::capture();
    account_spawn_env("claude", "default", &inside, "fixture-row").unwrap();
    let log = capture.text();
    drop(capture);
    assert!(accepted(&trust_file, &inside), "the row's folder is recorded");
    assert!(accepted(&trust_file, &layer), "Claude Code asks about an added folder without a record, and a leg cannot answer");
    let lines: Vec<&str> = log.lines().filter(|l| l.contains("folder trust")).collect();
    assert_eq!(lines.len(), 2, "one line per folder: {log}");
    assert!(lines.iter().all(|l| l.contains("INFO") && l.contains("outcome=Recorded") && l.contains("fixture-row")), "{log}");
    assert!(lines.iter().any(|l| l.contains(layer.to_str().unwrap())), "one line names the layer: {log}");

    // A second spawn rewrites nothing.
    let before = std::fs::read(&trust_file).unwrap();
    account_spawn_env("claude", "default", &inside, "fixture-row").unwrap();
    assert_eq!(std::fs::read(&trust_file).unwrap(), before, "both already trusted: byte-identical");

    // A Codex row has no such dialog and no layer.
    let (_home, inside, _layer, trust_file) = layer_fixture(true);
    account_spawn_env("codex", "default", &inside, "fixture-row").unwrap();
    assert!(!trust_file.exists(), "a Codex preparation records nothing");

    // No declaration: the owner has not opted in, for the layer as for the row.
    let (_home, inside, _layer, trust_file) = layer_fixture(false);
    account_spawn_env("claude", "default", &inside, "fixture-row").unwrap();
    assert!(!trust_file.exists(), "undeclared records neither the row's folder nor the layer");
}

#[test]
fn a_layer_that_is_not_ready_is_not_recorded() {
    if !sot_platform::test_isolated::run_isolated("agents::env::trust_scope_controls::a_layer_that_is_not_ready_is_not_recorded") {
        return;
    }
    let _guard = self_file_env_guarded();
    let (_home, inside, layer, trust_file) = layer_fixture(true);
    std::fs::remove_dir_all(layer.join(".claude")).unwrap();
    account_spawn_env("claude", "default", &inside, "fixture-row").unwrap();
    assert!(accepted(&trust_file, &inside));
    assert!(!accepted(&trust_file, &layer), "a leg without the layer adds no folder, so none needs a record");
}
