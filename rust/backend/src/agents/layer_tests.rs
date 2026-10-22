use super::*;

fn skills_folder(comm_home: &Path) -> PathBuf {
    let layer = comm_home.join(LAYER_DIR);
    let bootstrap = layer.join(".claude").join("skills").join("sot-session-start");
    std::fs::create_dir_all(&bootstrap).unwrap();
    std::fs::write(bootstrap.join("SKILL.md"), "---\nname: sot-session-start\n---\n").unwrap();
    layer
}

#[test]
fn the_layer_is_installed_only_where_it_holds_a_skills_folder() {
    let comm = tempfile::tempdir().unwrap();
    assert_eq!(layer_under(comm.path()), None);
    std::fs::create_dir_all(comm.path().join(LAYER_DIR)).unwrap();
    assert_eq!(layer_under(comm.path()), None, "an empty layer folder is no layer");
    // A skills folder without the bootstrap skill is not ready: the leg's prompt names it.
    std::fs::create_dir_all(comm.path().join(LAYER_DIR).join(".claude").join("skills").join("sitrep")).unwrap();
    assert_eq!(layer_under(comm.path()), None, "a layer without sot-session-start is not ready");
    let layer = skills_folder(comm.path());
    assert_eq!(layer_under(comm.path()), Some(layer));
}

#[test]
fn the_hooks_table_is_the_work_state_hooks_and_not_messagings_protections() {
    // The end-of-turn mail check (Stop) and the subagent guard stay in the user's own settings, for every joined session.
    assert_eq!(
        HOOKS,
        &[
            ("UserPromptSubmit", "comm-status-working.sh", None),
            ("PreToolUse", "comm-status-blocked.sh", Some("AskUserQuestion")),
            ("PostToolUse", "comm-status-heartbeat.sh", None),
        ]
    );
}

#[test]
fn a_hook_runs_its_installed_script_by_a_quoted_absolute_path() {
    let comm = Path::new("/home/o'neil/.sot-comm");
    let settings = hooks_settings(comm);
    let working = &settings["hooks"]["UserPromptSubmit"][0];
    assert_eq!(working.get("matcher"), None);
    assert_eq!(working["hooks"][0]["type"], "command");
    assert_eq!(working["hooks"][0]["command"], r"'/home/o'\''neil/.sot-comm/bin/comm-status-working.sh'");
    let pre = settings["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre.len(), 1, "{settings}");
    assert_eq!(pre[0]["matcher"], "AskUserQuestion");
    assert_eq!(settings["hooks"].as_object().unwrap().len(), 3);
    assert!(settings["hooks"].get("Stop").is_none(), "the mail check is not the layer's: {settings}");
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

#[test]
fn a_relative_comm_home_reaches_the_leg_as_absolute_paths() {
    let cwd = std::env::current_dir().unwrap();
    let hooks = hooks_settings(Path::new("rel/comm"));
    let command = hooks["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"].as_str().unwrap().to_string();
    assert!(command.starts_with(&format!("'{}", cwd.display())), "{command}");
    assert!(Path::new(add_dir_arg(Path::new("rel/layer")).trim_start_matches("--add-dir=")).is_absolute());
}
