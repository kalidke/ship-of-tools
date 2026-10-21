use super::super::support_tests::{touch_dir, touch_file};
use super::*;

#[test]
fn is_account_name_matches_the_pinned_class() {
    assert!(is_account_name("team"));
    assert!(is_account_name("team-2"));
    assert!(is_account_name("a_b"));
    assert!(is_account_name("9x"));
    assert!(!is_account_name(""));
    assert!(!is_account_name("Team"));
    assert!(!is_account_name("-team"));
    assert!(!is_account_name("team.x"));
    assert!(!is_account_name("_team"));
}

#[test]
fn discover_accounts_finds_default_and_named_subdirectories_of_the_auth_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    touch_dir(&home.join(".claude"));
    touch_file(&home.join(".claude").join(".credentials.json"));
    touch_dir(&home.join(".codex"));
    touch_file(&home.join(".codex").join("auth.json"));

    touch_dir(&home.join(".claude-auth").join("team"));
    touch_file(&home.join(".claude-auth").join("team").join(".credentials.json"));
    // Never logged in yet -- still a valid (empty) account.
    touch_dir(&home.join(".claude-auth").join("fresh"));
    // Fails is_account_name -> ignored.
    touch_dir(&home.join(".claude-auth").join("Bad.Name"));

    let accounts = discover_accounts(home);
    let names: Vec<&str> = accounts.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, vec!["default", "fresh", "team"], "default first, then alphabetical");

    let default = &accounts[0];
    assert_eq!(default.kinds, vec!["claude", "codex"]);
    assert_eq!(default.logged_in.get("claude"), Some(&true));
    assert_eq!(default.logged_in.get("codex"), Some(&true));

    let fresh = accounts.iter().find(|a| a.name == "fresh").unwrap();
    assert_eq!(fresh.kinds, vec!["claude"]);
    assert_eq!(fresh.logged_in.get("claude"), Some(&false), "empty -- never logged in");

    let team = accounts.iter().find(|a| a.name == "team").unwrap();
    assert_eq!(team.kinds, vec!["claude"]);
    assert_eq!(team.logged_in.get("claude"), Some(&true));
}

#[test]
fn discover_accounts_ignores_a_sibling_dot_claude_dash_name_folder() {
    // The retired scheme: a `.claude-<name>` folder directly in home,
    // OUTSIDE the `.claude-auth` parent, must never be discovered --
    // this is exactly what the new rule deletes the ambiguity of.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    touch_dir(&home.join(".claude-team"));
    touch_file(&home.join(".claude-team").join(".credentials.json"));
    assert!(discover_accounts(home).is_empty());
}

#[test]
fn discover_accounts_skips_a_plain_file_inside_the_auth_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    touch_dir(&home.join(".claude-auth"));
    touch_file(&home.join(".claude-auth").join("notadir"));
    assert!(discover_accounts(home).is_empty());
}

#[test]
fn account_env_default_is_a_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(account_env("claude", "", tmp.path()), Ok(Vec::new()));
    assert_eq!(account_env("claude", "default", tmp.path()), Ok(Vec::new()));
}

#[test]
fn account_env_accepts_an_existing_but_never_logged_in_folder() {
    // Clarification: a folder that exists but has never been
    // logged into is ALLOWED -- claude runs its own login flow (then
    // the folder-trust prompt) the first time it starts against a
    // fresh CLAUDE_CONFIG_DIR. `logged_in` is display-only (the
    // picker/status table); it must never gate this function.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".claude-auth").join("fresh");
    touch_dir(&dir);
    assert!(!dir.join(".credentials.json").exists(), "never logged in");
    let env = account_env("claude", "fresh", tmp.path()).unwrap();
    assert_eq!(
        env,
        vec![
            ("CLAUDE_CONFIG_DIR".to_string(), dir.to_string_lossy().into_owned()),
            ("SOT_ACCOUNT".to_string(), "fresh".to_string()),
        ]
    );
}

#[test]
fn account_env_known_claude_account_carries_config_dir_and_sot_account() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".claude-auth").join("team");
    touch_dir(&dir);
    let env = account_env("claude", "team", tmp.path()).unwrap();
    assert_eq!(
        env,
        vec![
            ("CLAUDE_CONFIG_DIR".to_string(), dir.to_string_lossy().into_owned()),
            ("SOT_ACCOUNT".to_string(), "team".to_string()),
        ]
    );
}

#[test]
fn account_env_refuses_a_named_codex_account() {
    let tmp = tempfile::tempdir().unwrap();
    // Even a folder that superficially looks right must not matter --
    // codex accounts are refused outright this release, folder or not.
    let err = account_env("codex", "team", tmp.path()).unwrap_err();
    assert!(err.contains("deferred"), "{err}");
}

#[test]
fn account_env_unknown_account_names_the_fix_command() {
    let tmp = tempfile::tempdir().unwrap();
    let err = account_env("claude", "team", tmp.path()).unwrap_err();
    assert!(err.contains("mkdir -p ~/.claude-auth/team"), "{err}");
}

#[test]
fn account_env_never_falls_back_to_the_default_directory() {
    let tmp = tempfile::tempdir().unwrap();
    // Even though the default claude dir exists, a named account
    // that has no subdirectory of its own must still refuse -- never
    // spend the default subscription in its place.
    touch_dir(&tmp.path().join(".claude"));
    assert!(account_env("claude", "team", tmp.path()).is_err());
}

#[test]
fn account_env_refuses_a_bash_row() {
    let tmp = tempfile::tempdir().unwrap();
    touch_dir(&tmp.path().join(".claude-auth").join("team"));
    let err = account_env("none", "team", tmp.path()).unwrap_err();
    assert!(err.contains("no account"), "{err}");
}

#[test]
fn account_env_refuses_an_invalid_account_name_before_any_path_join() {
    let tmp = tempfile::tempdir().unwrap();
    // Each of these would otherwise resolve outside .claude-auth/<name>
    // (a directory traversal, or clean out of the join entirely) --
    // refused before account_env ever builds a path from it.
    for bad in ["..", "../.claude", "/etc/passwd", "a/b", "Bad.Name"] {
        let err = account_env("claude", bad, tmp.path()).unwrap_err();
        assert!(err.contains("invalid account name"), "{bad:?}: {err}");
    }
}

#[test]
fn account_env_keeps_empty_and_default_as_no_ops_despite_the_name_check() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(account_env("claude", "", tmp.path()), Ok(Vec::new()));
    assert_eq!(account_env("claude", "default", tmp.path()), Ok(Vec::new()));
}

#[test]
fn ensure_account_links_links_every_shared_entry_the_default_has() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    touch_dir(&home.join(".claude"));
    touch_file(&home.join(".claude").join("CLAUDE.md"));
    touch_file(&home.join(".claude").join("settings.json"));
    touch_dir(&home.join(".claude").join("skills").join("foo"));
    touch_file(&home.join(".claude").join("history.jsonl"));
    // agents, commands, plugins, output-styles, projects: the default
    // lacks all five, so none of them may be created.
    let account_dir = home.join(".claude-auth").join("team");
    touch_dir(&account_dir);

    let result = ensure_account_links(home, "team");

    super::super::support_tests::assert_shared_as_the_box_allows(
        &result,
        &home.join(".claude"),
        &account_dir,
        &["skills"],
        &["CLAUDE.md", "settings.json", "history.jsonl"],
    );
    for name in ["agents", "commands", "plugins", "output-styles", "projects"] {
        assert!(
            std::fs::symlink_metadata(account_dir.join(name)).is_err(),
            "{name}: the default lacks it, so it must not be created"
        );
    }
}

#[test]
fn ensure_account_links_never_links_login_or_runtime_state() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    touch_dir(&home.join(".claude"));
    touch_file(&home.join(".claude").join(".credentials.json"));
    touch_file(&home.join(".claude").join(".claude.json"));
    touch_dir(&home.join(".claude").join("daemon"));
    touch_dir(&home.join(".claude").join("sessions"));
    let account_dir = home.join(".claude-auth").join("team");
    touch_dir(&account_dir);

    ensure_account_links(home, "team").unwrap();

    for name in [".credentials.json", ".claude.json", "daemon", "sessions"] {
        assert!(
            std::fs::symlink_metadata(account_dir.join(name)).is_err(),
            "{name} must never be linked -- it's runtime state or the login itself"
        );
    }
}

#[test]
fn ensure_account_links_leaves_an_existing_real_entry_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    touch_dir(&home.join(".claude"));
    std::fs::write(home.join(".claude").join("settings.json"), b"default").unwrap();
    touch_dir(&home.join(".claude").join("skills").join("shared-skill"));

    let account_dir = home.join(".claude-auth").join("team");
    touch_dir(&account_dir);
    std::fs::write(account_dir.join("settings.json"), b"account-own").unwrap();
    touch_dir(&account_dir.join("skills").join("own-skill"));

    ensure_account_links(home, "team").unwrap();

    let settings_meta = std::fs::symlink_metadata(account_dir.join("settings.json")).unwrap();
    assert!(!settings_meta.file_type().is_symlink(), "must stay the account's own real file");
    assert_eq!(std::fs::read(account_dir.join("settings.json")).unwrap(), b"account-own");

    let skills_meta = std::fs::symlink_metadata(account_dir.join("skills")).unwrap();
    assert!(!skills_meta.file_type().is_symlink(), "must stay the account's own real dir");
    assert!(account_dir.join("skills").join("own-skill").is_dir());
}

#[test]
fn ensure_account_links_refuses_an_invalid_account_name() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    touch_dir(&home.join(".claude"));
    for bad in ["..", "../.claude", "/etc/passwd", "a/b", "Bad.Name"] {
        let err = ensure_account_links(home, bad).unwrap_err();
        assert!(err.contains("invalid account name"), "{bad:?}: {err}");
    }
}

#[test]
fn link_or_skip_if_racing_tolerates_a_link_already_created_by_a_sibling() {
    // The race resume_all's parallel spawns hit: two rows on one new
    // account both pass ensure_account_links' existence check before
    // either has linked anything, so the slower one's own OS call
    // must not fail just because the link now exists.
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("target-file");
    touch_file(&target);
    let link = tmp.path().join("link");

    link_or_skip_if_racing(&make_shared_link, &target, &link, false).unwrap();
    assert!(
        link_or_skip_if_racing(&make_shared_link, &target, &link, false).is_ok(),
        "a second call on the same link must not fail"
    );
}

#[test]
fn ensure_account_links_is_a_no_op_for_empty_or_default_account() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    touch_dir(&home.join(".claude"));
    touch_file(&home.join(".claude").join("CLAUDE.md"));

    assert_eq!(ensure_account_links(home, ""), Ok(()));
    assert_eq!(ensure_account_links(home, "default"), Ok(()));
    assert!(!home.join(".claude-auth").exists(), "must never create anything for default");
}
