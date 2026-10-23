use super::*;
use super::super::support_tests::{platform_spelling, self_file_env_guarded};

#[test]
fn claude_recipe_places_extra_flags_before_the_skill() {
    assert_eq!(
        claude_recipe(&["--x".to_string()], None, None),
        vec!["claude", "--permission-mode", "auto", "--x", "/sot-session-start"]
    );
}

#[test]
#[cfg(unix)]
fn agent_argv_claude_starts_a_fresh_conversation() {
    // A capsule leg resumes nothing by recency: no `--continue` on any leg. A conversation is resumed only when
    // asked: a reauth's `--resume <id>` (below) or a person.
    let _guard = self_file_env_guarded();
    let dir = tempfile::tempdir().expect("tempdir");
    let claude = dir.path().join("claude");
    sot_platform::test_exec::write_executable(&claude, b"#!/bin/sh\n");
    // `claude_recipe` reads HOME (the account home); the guard restores it.
    std::env::remove_var("HOME");
    let env = AgentEnv { path: Some(dir.path().into()), home: None, shell: None, comm_home: None };
    assert_eq!(
        agent_argv_in(&env, "claude", None).unwrap(),
        vec![
            claude.to_string_lossy().into_owned(),
            "--permission-mode".to_string(),
            "auto".to_string(),
            "/sot-session-start".to_string(),
        ]
    );
}

#[test]
#[cfg(unix)]
fn agent_exec_argv_claude_never_adds_continue() {
    // ADR 0046 decision 4's own invariant: `agent-exec` never adds
    // `--continue` itself — it builds the SAME recipe shape with the
    // caller's own flags. Widened
    // from Linux-only to any Unix (CI fix): this is the exact case
    // that broke on macOS when `agent_exec_argv` routed resolution
    // through the capsule launcher's own, narrower refusal there.
    let _guard = self_file_env_guarded();
    let dir = tempfile::tempdir().expect("tempdir");
    let claude = dir.path().join("claude");
    sot_platform::test_exec::write_executable(&claude, b"#!/bin/sh\n");
    // `claude_recipe` reads HOME (the account home); the guard restores it.
    std::env::remove_var("HOME");
    let env = AgentEnv { path: Some(dir.path().into()), home: None, shell: None, comm_home: None };
    let argv = agent_exec_argv_in(
        &env,
        "claude",
        &["--continue".to_string(), "--x".to_string()],
    )
    .unwrap();
    // The caller MAY pass its own "--continue" through `extra` (as
    // `ccb --continue` does) -- `agent_exec_argv` never adds a SECOND
    // one of its own; this only asserts the recipe's fixed shape
    // around whatever the caller supplied.
    assert_eq!(
        argv,
        vec![
            claude.to_string_lossy().into_owned(),
            "--permission-mode".to_string(),
            "auto".to_string(),
            "--continue".to_string(),
            "--x".to_string(),
            "/sot-session-start".to_string(),
        ]
    );
}

#[test]
#[cfg(unix)]
fn agent_exec_argv_none_is_refused_it_has_no_shared_recipe() {
    // "none" is a legitimate `agent_argv` kind (the bare platform
    // shell) but shares no `--permission-mode`/skill recipe shape
    // with "claude" -- `agent-exec` refuses it regardless of whether
    // PATH/HOME would resolve anything, so neither is touched here.
    let err = agent_exec_argv("none", &[]).unwrap_err();
    assert!(err.contains("none"), "got: {err}");
}

#[test]
#[cfg(unix)]
fn agent_exec_argv_unknown_kind_prints_agent_argvs_own_error() {
    assert_eq!(
        agent_exec_argv("bogus", &[]).unwrap_err(),
        agent_argv("bogus", None).unwrap_err()
    );
}

#[test]
#[cfg(windows)]
fn agent_argv_claude_matches_the_daemons_own_recipe() {
    // Renamed (ADR 0046 decision 4): `ccb` no longer bakes these
    // flags in itself -- it execs through `sotd agent-exec claude
    // "$@"`, which shares this SAME `claude_recipe` builder. This
    // still pins the Windows capsule-spawn shape (`claude_recipe(&[],
    // None)` verbatim, no absolute-path resolution -- Windows relies
    // on the daemon's own `PATH`, `claude_argv`'s own doc).
    assert_eq!(
        agent_argv("claude", None).unwrap(),
        vec!["claude", "--permission-mode", "auto", "/sot-session-start"]
    );
}

#[test]
#[cfg(unix)]
fn agent_argv_claude_fails_closed_when_nothing_resolves() {
    // No PATH, no HOME: nothing to search, so this must refuse
    // rather than hand `sot-capsule` an unresolved bare "claude" it
    // would only fail to spawn later, one layer down. The environment
    // is passed in, so the precondition holds on any host, a real
    // `claude` on its `PATH` included.
    let env = AgentEnv { path: None, home: None, shell: None, comm_home: None };
    let result = agent_argv_in(&env, "claude", None);
    assert!(result.is_err());
}

#[test]
#[cfg(windows)]
fn agent_argv_none_is_the_bare_shell() {
    assert_eq!(agent_argv("none", None).unwrap(), vec!["cmd.exe"]);
}

#[test]
#[cfg(not(windows))]
fn agent_argv_none_is_the_login_shell_or_bin_sh() {
    assert_eq!(login_shell(Some("/bin/zsh")), "/bin/zsh");
    assert_eq!(login_shell(None), "/bin/sh");
}

/// `agent-exec` passes `ccb`'s own `"$@"` through, so a caller that
/// brought `--settings` must keep it: two of them contend for one key.
#[test]
fn caller_brought_settings_spots_both_spellings() {
    assert!(caller_brought_settings(&["--settings".to_string(), "{}".to_string()]));
    assert!(caller_brought_settings(&["--settings={}".to_string()]));
    assert!(!caller_brought_settings(&["--continue".to_string()]));
}

/// The WIRING, which nothing reached before: that [`claude_recipe`]
/// actually emits the flag, where it sits in argv, and that a caller's
/// own `--settings` suppresses ours instead of joining it.
///
/// The version of this test the review caught passed `memory_cwd: None`,
/// so no flag could ever be built — which made an inverted
/// `if !caller_brought_settings(…)` invisible to it, not merely
/// environment-dependent. A real cwd is what gives the assertion teeth.
#[test]
#[cfg(unix)]
fn claude_recipe_emits_our_settings_unless_the_caller_brought_one() {
    // The guard holds the env-serialization lock AND snapshots both vars
    // this test moves, restoring them in `Drop` — so a failing assertion
    // below cannot leave `HOME` pointing at a deleted tempdir for every
    // later test in the process, which a hand-rolled restore would.
    let _guard = self_file_env_guarded();
    let home = tempfile::tempdir().expect("tempdir");
    let root = platform_spelling(home.path());
    std::fs::create_dir_all(root.join(".claude").join("projects")).unwrap();
    let proj = root.join("proj");
    std::fs::create_dir_all(&proj).unwrap();

    std::env::set_var("HOME", &root);
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    let ours = claude_recipe(&[], Some(&proj), None);
    let caller = claude_recipe(
        &["--settings".to_string(), "{\"a\":1}".to_string()],
        Some(&proj),
        None,
    );

    let at = ours
        .iter()
        .position(|a| a == "--settings")
        .unwrap_or_else(|| panic!("our flag is never emitted: {ours:?}"));
    assert_eq!(ours.iter().filter(|a| *a == "--settings").count(), 1);
    assert!(
        ours[at + 1].contains("autoMemoryDirectory"),
        "the value is not ours: {}",
        ours[at + 1]
    );
    // Ahead of the trailing skill argument, which must stay last.
    assert!(at < ours.iter().position(|a| a == "/sot-session-start").unwrap());
    // And the caller's own wins outright — ours is not appended beside it.
    assert_eq!(caller.iter().filter(|a| *a == "--settings").count(), 1);
    let at = caller.iter().position(|a| a == "--settings").unwrap();
    assert_eq!(caller[at + 1], "{\"a\":1}");
}

#[test]
fn agent_argv_rejects_unsupported_kinds() {
    assert!(agent_argv("bogus", None).is_err());
}

/// `agent_argv("codex")` over a `PATH` and `HOME` of the test's own.
#[cfg(unix)]
fn codex_over(path: Option<&std::path::Path>, home: Option<&std::path::Path>) -> Result<Vec<String>, String> {
    let env = AgentEnv { path: path.map(Into::into), home: home.map(Into::into), shell: None, comm_home: None };
    agent_argv_in(&env, "codex", None)
}

#[cfg(unix)]
fn write_stub_ccx(path: &Path) {
    sot_platform::test_exec::write_executable(path, "#!/bin/sh\nexit 0\n");
}

#[test]
#[cfg(unix)]
fn agent_argv_codex_resolves_ccx_and_starts_fresh() {
    let dir = tempfile_test_dir();
    let ccx = dir.path().join("ccx");
    write_stub_ccx(&ccx);

    let result = codex_over(Some(dir.path()), None);
    assert_eq!(
        result.unwrap(),
        vec![ccx.to_string_lossy().into_owned(), "--capsule".to_string()]
    );
}

#[test]
#[cfg(unix)]
fn agent_argv_codex_fails_closed_when_nothing_resolves() {
    let result = codex_over(None, None);
    assert!(result.is_err());
}

#[test]
#[cfg(windows)]
fn agent_argv_codex_is_refused_on_windows() {
    let err = agent_argv("codex", None).unwrap_err();
    assert!(err.contains("ccx"), "got: {err}");
}

#[test]
#[cfg(unix)]
fn resolve_claude_finds_an_executable_on_path() {
    let dir = tempfile_test_dir();
    let claude = dir.path().join("claude");
    sot_platform::test_exec::write_executable(&claude, b"#!/bin/sh\nexit 0\n");
    let path_var = std::ffi::OsString::from(dir.path());
    let resolved = resolve_claude(Some(&path_var), None).unwrap();
    assert_eq!(resolved, claude.to_string_lossy());
}

#[test]
#[cfg(unix)]
fn resolve_claude_falls_back_to_local_bin_under_home() {
    let dir = tempfile_test_dir();
    let local_bin = dir.path().join(".local/bin");
    std::fs::create_dir_all(&local_bin).unwrap();
    let claude = local_bin.join("claude");
    sot_platform::test_exec::write_executable(&claude, b"#!/bin/sh\nexit 0\n");
    // An empty PATH still finds it via the HOME-derived fallback dirs.
    let resolved = resolve_claude(None, Some(dir.path())).unwrap();
    assert_eq!(resolved, claude.to_string_lossy());
}

#[test]
#[cfg(unix)]
fn resolve_claude_ignores_a_non_executable_file() {
    let dir = tempfile_test_dir();
    let claude = dir.path().join("claude");
    std::fs::write(&claude, b"not a real program").unwrap(); // no +x
    let path_var = std::ffi::OsString::from(dir.path());
    let err = resolve_claude(Some(&path_var), None).unwrap_err();
    assert!(err.contains("claude not found"), "{err}");
}

#[test]
#[cfg(unix)]
fn resolve_claude_names_every_directory_it_searched() {
    let err = resolve_claude(None, None).unwrap_err();
    assert!(err.contains("claude not found"), "{err}");
}

#[test]
#[cfg(unix)]
fn resolve_claude_skips_a_same_named_directory_for_a_later_real_file() {
    // Review round, reproduced: a directory named `claude` passes
    // `access(X_OK)` (the execute bit on a directory means
    // "searchable") -- without the regular-file check this would
    // have wrongly "resolved" to the directory in `dir1`, never
    // reaching the REAL executable in `dir2`.
    let dir1 = tempfile_test_dir();
    std::fs::create_dir(dir1.path().join("claude")).unwrap();
    let dir2 = tempfile_test_dir();
    let real = dir2.path().join("claude");
    sot_platform::test_exec::write_executable(&real, b"#!/bin/sh\nexit 0\n");
    let path_var = std::env::join_paths([dir1.path(), dir2.path()]).unwrap();
    let resolved = resolve_claude(Some(&path_var), None).unwrap();
    assert_eq!(resolved, real.to_string_lossy());
}

#[test]
#[cfg(unix)]
fn resolve_claude_skips_a_relative_path_entry() {
    // Review round, reproduced: a relative PATH entry resolves
    // against the daemon's own current directory, not the eventual
    // workspace -- it must be skipped outright, never joined against
    // anything, even when (as here) it happens to be the ONLY entry.
    let path_var = std::ffi::OsString::from("relative/bin");
    let err = resolve_claude(Some(&path_var), None).unwrap_err();
    assert!(err.contains("claude not found"), "{err}");
    assert!(!err.contains("relative/bin"), "a relative entry must never even be searched: {err}");
}

#[test]
#[cfg(unix)]
fn resolve_ccx_finds_an_executable_on_path() {
    let dir = tempfile_test_dir();
    let ccx = dir.path().join("ccx");
    sot_platform::test_exec::write_executable(&ccx, b"#!/bin/sh\nexit 0\n");
    let path_var = std::ffi::OsString::from(dir.path());
    assert_eq!(resolve_ccx(Some(&path_var), None).unwrap(), ccx.to_string_lossy());
}

#[test]
#[cfg(unix)]
fn resolve_ccx_falls_back_to_local_bin_under_home() {
    let dir = tempfile_test_dir();
    let local_bin = dir.path().join(".local/bin");
    std::fs::create_dir_all(&local_bin).unwrap();
    let ccx = local_bin.join("ccx");
    sot_platform::test_exec::write_executable(&ccx, b"#!/bin/sh\nexit 0\n");
    assert_eq!(resolve_ccx(None, Some(dir.path())).unwrap(), ccx.to_string_lossy());
}

#[test]
#[cfg(unix)]
fn resolve_ccx_names_every_directory_it_searched() {
    let err = resolve_ccx(None, None).unwrap_err();
    assert!(err.contains("ccx not found"), "{err}");
}

#[cfg(unix)]
fn tempfile_test_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

// ADR 0046 decision 6: a reauth leg carries an explicit `--resume
// <id>` and NEVER `--continue`. Asserted on the recipe rather than on
// `claude_resume_argv` itself, which resolves a real `claude` binary
// from the environment — the FLAG shape is the decision; resolution is
// `resolve_claude`'s own, already tested above.
#[test]
fn a_reauth_leg_resumes_by_id_and_never_continues() {
    let argv = claude_recipe(&["--resume".to_string(), "abc-123".to_string()], None, None);
    assert!(!argv.iter().any(|a| a == "--continue"), "{argv:?}");
    let at = argv.iter().position(|a| a == "--resume").expect("--resume present");
    assert_eq!(argv[at + 1], "abc-123");
    assert_eq!(argv.last().map(String::as_str), Some("/sot-session-start"));
    assert!(argv.windows(2).any(|w| w[0] == "--permission-mode" && w[1] == "auto"), "{argv:?}");
}

/// A comm home whose layer holds a skills folder, and the layer's path.
fn comm_home_with_layer() -> (tempfile::TempDir, PathBuf) {
    let comm = tempfile::tempdir().expect("tempdir");
    let layer = comm.path().join(super::super::layer::LAYER_DIR);
    let bootstrap = layer.join(".claude").join("skills").join("sot-session-start");
    std::fs::create_dir_all(&bootstrap).unwrap();
    std::fs::write(bootstrap.join("SKILL.md"), "---\nname: sot-session-start\n---\n").unwrap();
    (comm, layer)
}

/// The three legs the recipe builds on Unix, over a stub `claude` found on `path`: the capsule leg, the reauth leg and
/// `sotd agent-exec claude`.
#[cfg(unix)]
fn the_three_legs(comm_home: Option<&Path>) -> Vec<Vec<String>> {
    let bin = tempfile::tempdir().expect("tempdir");
    sot_platform::test_exec::write_executable(&bin.path().join("claude"), b"#!/bin/sh\n");
    let env = AgentEnv {
        path: Some(bin.path().into()),
        home: None,
        shell: None,
        comm_home: comm_home.map(Into::into),
    };
    vec![
        agent_argv_in(&env, "claude", None).unwrap(),
        claude_resume_argv_in(&env, "abc-123", None).unwrap(),
        agent_exec_argv_in(&env, "claude", &[]).unwrap(),
    ]
}

#[test]
#[cfg(unix)]
fn every_claude_leg_runs_auto_mode_and_never_skips_permissions() {
    let _guard = self_file_env_guarded();
    let (comm, _) = comm_home_with_layer();
    for comm_home in [None, Some(comm.path())] {
        for argv in the_three_legs(comm_home) {
            assert!(argv.windows(2).any(|w| w[0] == "--permission-mode" && w[1] == "auto"), "{argv:?}");
            assert!(!argv.iter().any(|a| a == "--dangerously-skip-permissions"), "{argv:?}");
            assert_eq!(argv.last().map(String::as_str), Some("/sot-session-start"), "{argv:?}");
        }
    }
}

#[test]
fn the_layer_rides_the_recipe_and_a_plain_claude_has_none() {
    let (comm, layer) = comm_home_with_layer();
    let bare = tempfile::tempdir().expect("tempdir");
    // No comm home, or one with no layer: the recipe's own shape and nothing of the layer.
    for none in [None, Some(bare.path())] {
        assert_eq!(
            claude_recipe(&[], None, none),
            vec!["claude", "--permission-mode", "auto", "/sot-session-start"]
        );
    }
    // An installed layer: one `--add-dir=` token ahead of the flags, and the hooks in the one `--settings`.
    let argv = claude_recipe(&["--x".to_string()], None, Some(comm.path()));
    assert_eq!(argv[1], format!("--add-dir={}", layer.display()), "{argv:?}");
    assert_eq!(&argv[2..4], ["--permission-mode", "auto"], "{argv:?}");
    assert!(!argv.iter().any(|a| a == "--add-dir"), "the flag is variadic: {argv:?}");
    assert_eq!(argv.iter().filter(|a| *a == "--settings").count(), 1, "{argv:?}");
    let at = argv.iter().position(|a| a == "--settings").unwrap();
    let json: serde_json::Value = serde_json::from_str(&argv[at + 1]).unwrap();
    assert_eq!(json["hooks"].as_object().unwrap().len(), 3, "{json}");
    assert_eq!(argv.last().map(String::as_str), Some("/sot-session-start"));
    // A caller's own `--settings` stays the one, with its own keys, and gains the hooks (the next test); the skills load.
    let caller = claude_recipe(&["--settings".to_string(), "{\"a\":1}".to_string()], None, Some(comm.path()));
    assert_eq!(caller[1], format!("--add-dir={}", layer.display()), "{caller:?}");
    assert_eq!(caller.iter().filter(|a| *a == "--settings").count(), 1, "{caller:?}");
    let at = caller.iter().position(|a| a == "--settings").unwrap();
    let json: serde_json::Value = serde_json::from_str(&caller[at + 1]).unwrap();
    assert_eq!(json["a"], 1, "{json}");
    assert_eq!(json["hooks"].as_object().unwrap().len(), 3, "{json}");
}

/// A caller's own `--settings` JSON keeps its own keys and gains the recipe's hooks beside its own: per event the
/// caller's groups come first. A value that is not a JSON object (a file path) and a second `--settings` are passed as
/// given, with no hooks, and say so.
#[test]
#[cfg(unix)]
fn a_callers_settings_json_is_merged_with_the_recipes_hooks() {
    let _guard = self_file_env_guarded();
    let (comm, _layer) = comm_home_with_layer();
    let mine = r#"{"model":"opus","hooks":{"PostToolUse":[{"hooks":[{"type":"command","command":"mine"}]}],"Stop":[{"hooks":[{"type":"command","command":"stop"}]}]}}"#;
    for flags in [
        vec!["--settings".to_string(), mine.to_string()],
        vec![format!("--settings={mine}")],
    ] {
        let argv = claude_recipe(&flags, None, Some(comm.path()));
        let value = argv
            .iter()
            .find_map(|a| a.strip_prefix("--settings="))
            .or_else(|| argv.iter().position(|a| a == "--settings").map(|i| argv[i + 1].as_str()))
            .unwrap_or_else(|| panic!("no --settings: {argv:?}"));
        assert_eq!(argv.iter().filter(|a| a.starts_with("--settings")).count(), 1, "{argv:?}");
        let json: serde_json::Value = serde_json::from_str(value).unwrap();
        assert_eq!(json["model"], "opus", "{json}");
        assert_eq!(json["hooks"]["Stop"].as_array().unwrap().len(), 1, "{json}");
        let post = json["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 2, "{json}");
        assert_eq!(post[0]["hooks"][0]["command"], "mine", "{json}");
        assert!(post[1]["hooks"][0]["command"].as_str().unwrap().contains("heartbeat"), "{json}");
        for event in ["UserPromptSubmit", "PreToolUse"] {
            assert_eq!(json["hooks"][event].as_array().unwrap().len(), 1, "{event}: {json}");
        }
    }
    let path = claude_recipe(&["--settings".to_string(), "/some/file.json".to_string()], None, Some(comm.path()));
    assert!(!path.iter().any(|a| a.contains("\"hooks\"")), "{path:?}");
    let twice = claude_recipe(
        &["--settings".to_string(), "{}".to_string(), "--settings".to_string(), "{}".to_string()],
        None,
        Some(comm.path()),
    );
    assert!(!twice.iter().any(|a| a.contains("\"hooks\"")), "{twice:?}");
}

/// A recipe hook the caller's JSON already carries (the same command) is not added a second time; its other events
/// still gain theirs.
#[test]
#[cfg(unix)]
fn a_recipe_hook_the_caller_already_has_is_not_added_twice() {
    let _guard = self_file_env_guarded();
    let (comm, _layer) = comm_home_with_layer();
    let ours = layer::hooks_settings(comm.path());
    let caller = serde_json::json!({"hooks": {"PostToolUse": ours["hooks"]["PostToolUse"].clone()}}).to_string();
    let argv = claude_recipe(&["--settings".to_string(), caller], None, Some(comm.path()));
    let at = argv.iter().position(|a| a == "--settings").unwrap();
    let json: serde_json::Value = serde_json::from_str(&argv[at + 1]).unwrap();
    assert_eq!(json["hooks"]["PostToolUse"].as_array().unwrap().len(), 1, "{json}");
    for event in ["UserPromptSubmit", "PreToolUse"] {
        assert_eq!(json["hooks"][event].as_array().unwrap().len(), 1, "{event}: {json}");
    }
}

/// The shape rule: a caller's `--settings` is merged into only when it is a JSON object whose `hooks`, if it has any,
/// is an object whose every event the recipe also names is an array. Anything else is passed as given, with no hooks.
#[test]
#[cfg(unix)]
fn a_callers_settings_of_another_shape_is_passed_as_given() {
    let _guard = self_file_env_guarded();
    let (comm, _layer) = comm_home_with_layer();
    for value in [
        "{",
        "[1]",
        "\"text\"",
        r#"{"hooks":null}"#,
        r#"{"hooks":[]}"#,
        r#"{"hooks":{"PostToolUse":{}}}"#,
    ] {
        let argv = claude_recipe(&["--settings".to_string(), value.to_string()], None, Some(comm.path()));
        let at = argv.iter().position(|a| a == "--settings").unwrap();
        assert_eq!(argv[at + 1], value, "{argv:?}");
        assert!(!argv.iter().any(|a| a.contains("heartbeat")), "{value}: {argv:?}");
    }
}

#[test]
#[cfg(unix)]
fn the_layer_reaches_the_capsule_leg_the_reauth_leg_and_agent_exec() {
    let _guard = self_file_env_guarded();
    let (comm, layer) = comm_home_with_layer();
    let want = format!("--add-dir={}", layer.display());
    for argv in the_three_legs(Some(comm.path())) {
        assert_eq!(argv[1], want, "{argv:?}");
        assert!(argv.iter().any(|a| a.contains("\"hooks\"")), "{argv:?}");
    }
    for argv in the_three_legs(None) {
        assert!(!argv.iter().any(|a| a.starts_with("--add-dir") || a.contains("\"hooks\"")), "{argv:?}");
    }
}

#[test]
fn a_layer_without_the_bootstrap_skill_is_left_out_and_the_prompt_goes_with_it() {
    let comm = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(comm.path().join(super::super::layer::LAYER_DIR).join(".claude").join("skills").join("sitrep")).unwrap();
    let argv = claude_recipe(&[], None, Some(comm.path()));
    assert_eq!(argv, vec!["claude", "--permission-mode", "auto"], "{argv:?}");
}

#[test]
fn a_setting_sources_without_project_is_known_to_the_recipe() {
    let extra = ["--setting-sources".to_string(), "user".to_string()];
    assert!(setting_sources_exclude_project(&extra));
    assert!(!setting_sources_exclude_project(&["--setting-sources=user,project".to_string()]));
    assert!(!setting_sources_exclude_project(&["--continue".to_string()]));
}
