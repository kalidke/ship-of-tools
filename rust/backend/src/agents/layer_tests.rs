use super::*;

fn skills_folder(comm_home: &Path) -> PathBuf {
    let layer = comm_home.join(LAYER_DIR);
    std::fs::create_dir_all(layer.join(".claude").join("skills")).unwrap();
    layer
}

#[test]
fn the_layer_is_installed_only_where_it_holds_a_skills_folder() {
    let comm = tempfile::tempdir().unwrap();
    assert_eq!(layer_under(comm.path()), None);
    std::fs::create_dir_all(comm.path().join(LAYER_DIR)).unwrap();
    assert_eq!(layer_under(comm.path()), None, "an empty layer folder is no layer");
    let layer = skills_folder(comm.path());
    assert_eq!(layer_under(comm.path()), Some(layer));
}

#[test]
fn the_hooks_table_is_the_five_work_state_hooks_with_their_matchers() {
    assert_eq!(
        HOOKS,
        &[
            ("UserPromptSubmit", "comm-status-working.sh", None),
            ("PreToolUse", "comm-status-blocked.sh", Some("AskUserQuestion")),
            ("PreToolUse", "comm-subagent-guard.sh", Some("Bash")),
            ("Stop", "comm-status-idle.sh", None),
            ("PostToolUse", "comm-status-heartbeat.sh", None),
        ]
    );
}

#[test]
fn a_hook_runs_its_installed_script_by_a_quoted_absolute_path() {
    let comm = Path::new("/home/o'neil/.sot-comm");
    let settings = hooks_settings(comm);
    let idle = &settings["hooks"]["Stop"][0];
    assert_eq!(idle.get("matcher"), None);
    assert_eq!(idle["hooks"][0]["type"], "command");
    assert_eq!(idle["hooks"][0]["command"], r"'/home/o'\''neil/.sot-comm/bin/comm-status-idle.sh'");
    let pre = settings["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre.len(), 2, "one group per matcher: {settings}");
    assert_eq!(pre[0]["matcher"], "AskUserQuestion");
    assert_eq!(pre[1]["matcher"], "Bash");
    assert_eq!(settings["hooks"].as_object().unwrap().len(), 4);
}

#[test]
fn the_add_dir_flag_is_one_token() {
    let comm = tempfile::tempdir().unwrap();
    let layer = skills_folder(comm.path());
    assert_eq!(add_dir_arg(&layer), format!("--add-dir={}", layer.display()));
}

#[test]
fn merged_settings_hold_the_memory_key_and_the_hooks_in_one_object() {
    let hooks = hooks_settings(Path::new("/c"));
    let both: serde_json::Value = serde_json::from_str(
        &merged_settings(Some(r#"{"autoMemoryDirectory":"/m"}"#.into()), Some(hooks.clone())).unwrap(),
    )
    .unwrap();
    assert_eq!(both["autoMemoryDirectory"], "/m");
    assert_eq!(both["hooks"], hooks["hooks"]);
    let memory_only = merged_settings(Some(r#"{"autoMemoryDirectory":"/m"}"#.into()), None).unwrap();
    assert_eq!(memory_only, r#"{"autoMemoryDirectory":"/m"}"#);
    assert_eq!(merged_settings(None, None), None);
    let hooks_only: serde_json::Value = serde_json::from_str(&merged_settings(None, Some(hooks.clone())).unwrap()).unwrap();
    assert_eq!(hooks_only, hooks);
}
