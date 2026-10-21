use super::*;
use super::super::support_tests::self_file_env_guarded;

#[test]
fn nesting_env_scrub_list_matches_ccb() {
    // Mirrors agents/claude/bin/ccb's own `unset` line
    // exactly -- see that file for the reasoning per variable.
    assert_eq!(
        NESTING_ENV_VARS_TO_SCRUB,
        &[
            "CLAUDE_CODE_FORK_SUBAGENT",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_TEAMMATE_MODE",
            "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
            "CLAUDECODE",
            "AI_AGENT",
            "CLAUDE_CODE_SESSION_ID",
            "NO_COLOR",
        ]
    );
}

#[test]
#[cfg(not(windows))]
fn agent_env_prepends_local_bin_to_path_once() {
    // The ccb launcher's PATH rule, now the ONE shared function (ADR
    // 0046 decision 4): PATH with no ~/.local/bin gets it prepended
    // -- and a PATH that already reaches it is left alone (no
    // duplicate entry). Pure/DI'd (mirrors `resolve_claude`'s own
    // tests) -- no env mutation, no guard needed.
    let env = agent_env(
        Some(std::ffi::OsStr::new("/usr/bin:/bin")),
        Some(Path::new("/fake-home")),
    );
    let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    assert_eq!(get("PATH"), Some("/fake-home/.local/bin:/usr/bin:/bin"));

    let env = agent_env(
        Some(std::ffi::OsStr::new("/fake-home/.local/bin:/usr/bin")),
        Some(Path::new("/fake-home")),
    );
    assert!(env.is_empty(), "already on PATH: nothing to stamp");
}

#[test]
#[cfg(not(windows))]
fn agent_env_is_empty_with_no_home_to_derive_it_from() {
    let env = agent_env(Some(std::ffi::OsStr::new("/usr/bin:/bin")), None);
    assert!(env.is_empty());
}

#[test]
fn capsule_supervisor_env_carries_slug_workspace_and_comm_home() {
    // Codex round: SOT_WORKSPACE is keyed on SLUG (finding 1 — the
    // frontend keys results/the active workspace by slug, not the
    // internal ws-<slug>-<hex> id), while the id is used only for the
    // self-file path. SOT_COMM_HOME and SOT_COMM_SELF_FILE
    // (comm-lib.sh's existing pin-the-self-file seam) are stamped
    // unconditionally so a capsule never falls back to the shared
    // per-host __nopane slot.
    //
    // ADR 0046 decision 1 (S2/S4): the leg ALSO
    // carries a bare SOT_SOCKET and SOT_WORKSPACE_ID — every
    // pane/capsule alike, so `comm-join.sh`'s `agent.join` can always
    // find its owner daemon. No SOT_SELF_HOST pin (an override on the
    // daemon's own process already reaches this child by inheritance).
    // `set_own_endpoint` is idempotent (the daemon binds exactly one
    // listener per process) and process-global (`OnceLock`), so this
    // pins the value itself here rather than trusting whatever another
    // test in this binary may have already set it to.
    crate::agents::awareness::set_own_endpoint(Path::new("/fake-home/.local/state/sot/session.sock"));
    let _guard = self_file_env_guarded();
    std::env::set_var("SOT_SELF_HOST", "testhost");
    std::env::set_var("SOT_COMM_HOME", "/fake-home/.sot-comm");
    let env = capsule_supervisor_env(
        "ws-myrepo-1a2b",
        "myrepo",
        Path::new("/home/me/myrepo"),
        "myrepo-myhost",
    );
    let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    assert_eq!(get("SOT_WORKSPACE"), Some("myrepo"));
    assert_eq!(get("SOT_WORKSPACE_ID"), Some("ws-myrepo-1a2b"));
    assert_eq!(get("SOT_WORKSPACE_ROOT"), Some("/home/me/myrepo"));
    assert_eq!(get("SOT_COMM_NAME"), Some("myrepo-myhost"));
    assert_eq!(get("SOT_SESSION"), Some("1"));
    assert_eq!(get("SOT_HOST"), None, "no SOT_SELF_HOST pin -- inheritance carries an override, if any");
    if cfg!(unix) {
        // `OWN_ENDPOINT` is process-global (`OnceLock`) -- another test
        // in this binary may have already pinned it to a different
        // exact path, so this asserts the BARE SHAPE only (S4: no
        // typed unix:/pipe: prefix), never an exact value.
        assert!(
            get("SOT_SOCKET").is_some_and(|s| !s.starts_with("unix:") && !s.starts_with("pipe:")),
            "SOT_SOCKET must be a bare path, got {:?}",
            get("SOT_SOCKET")
        );
    } else {
        assert_eq!(get("SOT_SOCKET"), None, "S4: SOT_SOCKET is pinned on Unix only");
    }
    assert_eq!(get("SOT_COMM_HOME"), Some("/fake-home/.sot-comm"));
    assert_eq!(
        get("SOT_COMM_SELF_FILE"),
        Some("/fake-home/.sot-comm/self/testhost__ws-myrepo-1a2b.txt")
    );
    assert_eq!(
        get("SOTD_BIN"),
        own_sotd_bin(),
        "SOTD_BIN is this process's own start path, made once"
    );
}

#[test]
fn capsule_supervisor_env_disables_the_feedback_survey() {
    // Every row's agent, named or not, on every OS: the survey's
    // modal panel would otherwise hold the row's session until
    // someone answers it. Only that switch, not telemetry's.
    let _guard = self_file_env_guarded();
    std::env::set_var("SOT_SELF_HOST", "testhost");
    std::env::set_var("SOT_COMM_HOME", "/fake-home/.sot-comm");
    for name in ["myrepo-myhost", ""] {
        let env = capsule_supervisor_env("ws-myrepo-1a2b", "myrepo", Path::new("/home/me/myrepo"), name);
        let hits: Vec<_> = env.iter().filter(|(k, _)| k == "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY").collect();
        assert_eq!(hits.len(), 1, "exactly one survey switch for agent_name {name:?}");
        assert_eq!(hits[0].1, "1");
        assert!(!env
            .iter()
            .any(|(k, _)| k == "DISABLE_TELEMETRY" || k == "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"));
    }
}

#[test]
fn capsule_supervisor_env_keeps_the_conversation_in_its_row() {
    // Agent view would move a row's conversation into Claude Code's own
    // daemon, outside the capsule. Only that switch: background tasks
    // and the bg exit handoff stay as they are.
    let _guard = self_file_env_guarded();
    std::env::set_var("SOT_SELF_HOST", "testhost");
    std::env::set_var("SOT_COMM_HOME", "/fake-home/.sot-comm");
    for agent_name in ["myrepo-myhost", ""] {
        let env = capsule_supervisor_env("ws-myrepo-1a2b", "myrepo", Path::new("/home/me/myrepo"), agent_name);
        let hits: Vec<_> = env.iter().filter(|(k, _)| k == "CLAUDE_CODE_DISABLE_AGENT_VIEW").collect();
        assert_eq!(hits.len(), 1, "exactly one agent-view switch for agent_name {agent_name:?}");
        assert_eq!(hits[0].1, "1");
        assert!(!env
            .iter()
            .any(|(k, _)| k == "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS" || k == "CLAUDE_CODE_DISABLE_BG_EXIT_HANDOFF"));
    }
}

#[test]
fn capsule_supervisor_env_omits_comm_name_when_unnamed() {
    // Codex round finding 2: NO synthesized default here — an
    // un-pinned autostart (no explicit agent_name in the request)
    // gets no SOT_COMM_NAME at all. comm-join.sh's own #148
    // auto-disambiguating derivation decides the handle instead
    // (reading SOT_COMM_SELF_FILE to know where to write it), exactly
    // as a hand-started shell would — a synthesized <slug>-<host>
    // pin would become an explicit overwrite of any existing row of
    // that name, which PROTOCOL.md's "never reuse a handle" forbids.
    // SOT_WORKSPACE/SOT_COMM_HOME/SOT_COMM_SELF_FILE stay
    // unconditional regardless.
    let _guard = self_file_env_guarded();
    std::env::set_var("SOT_SELF_HOST", "testhost");
    std::env::set_var("SOT_COMM_HOME", "/fake-home/.sot-comm");
    let env = capsule_supervisor_env("ws-anon-9f9f", "anon", Path::new("/home/me/anon"), "");
    let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    assert_eq!(get("SOT_WORKSPACE"), Some("anon"));
    assert_eq!(get("SOT_COMM_NAME"), None);
    assert_eq!(
        get("SOT_COMM_SELF_FILE"),
        Some("/fake-home/.sot-comm/self/testhost__ws-anon-9f9f.txt")
    );
}

#[test]
fn capsule_comm_home_str_none_when_no_home_var_is_set() {
    let _guard = self_file_env_guarded();
    std::env::remove_var("SOT_COMM_HOME");
    std::env::remove_var("HOME");
    std::env::remove_var("USERPROFILE");
    assert_eq!(capsule_comm_home_str(), None);
}

#[test]
fn sotd_bin_value_makes_the_start_path_absolute_as_its_start_found_it() {
    use std::ffi::OsStr;
    let work = tempfile::tempdir().expect("a work folder");
    let bin = work.path().join("bin");
    std::fs::create_dir_all(&bin).expect("mkdir bin");
    // The program as the platform names it: on Windows a bare name gets `.exe` added, so the file has it.
    let name = "sotd-start-probe";
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    sot_log::test_exec::write_executable(&bin.join(&file), "#!/bin/sh\n");
    let cwd = std::env::current_dir().expect("the working folder");
    let shown = |p: &Path| {
        let text = p.to_string_lossy().into_owned();
        if cfg!(windows) {
            text.replace('\\', "/")
        } else {
            text
        }
    };
    let path_var =
        std::env::join_paths([work.path().join("empty"), bin.clone()]).expect("a PATH");
    // Absolute: itself.
    assert_eq!(
        sotd_bin_value(bin.join(&file).as_os_str(), None),
        Some(shown(&bin.join(&file)))
    );
    // Relative: against the working folder, with no lookup.
    let relative = Path::new("target").join("release").join(&file);
    assert_eq!(
        sotd_bin_value(relative.as_os_str(), None),
        Some(shown(&cwd.join(&relative)))
    );
    // Bare: the first PATH entry holding it, by the name with `.exe` added on Windows, or as given.
    assert_eq!(
        sotd_bin_value(OsStr::new(name), Some(&path_var)),
        Some(shown(&bin.join(&file)))
    );
    assert_eq!(
        sotd_bin_value(OsStr::new(&file), Some(&path_var)),
        Some(shown(&bin.join(&file)))
    );
    // Bare, found nowhere, or with no PATH; and an empty start path.
    assert_eq!(
        sotd_bin_value(OsStr::new("no-such-sotd"), Some(&path_var)),
        None
    );
    assert_eq!(sotd_bin_value(OsStr::new(name), None), None);
    assert_eq!(sotd_bin_value(OsStr::new(""), Some(&path_var)), None);
    // Windows: a drive-relative start (`D:sotd.exe`) is against that drive's working folder, here this process's.
    #[cfg(windows)]
    {
        let drive = cwd
            .to_string_lossy()
            .chars()
            .next()
            .filter(char::is_ascii_alphabetic);
        let drive = drive.expect("a working folder on a lettered drive");
        assert_eq!(
            sotd_bin_value(OsStr::new(&format!("{drive}:{file}")), None),
            Some(shown(&cwd.join(&file)))
        );
    }
    // Unix: a link on PATH is the path given, never its target.
    #[cfg(unix)]
    {
        let link = work.path().join("link");
        std::fs::create_dir_all(&link).expect("mkdir link");
        std::os::unix::fs::symlink(bin.join(name), link.join(name)).expect("link the probe");
        let link_path = std::env::join_paths([link.clone()]).expect("a PATH");
        assert_eq!(
            sotd_bin_value(OsStr::new(name), Some(&link_path)),
            Some(shown(&link.join(name)))
        );
    }
}
