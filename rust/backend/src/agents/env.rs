// env.rs — the spawn env: nesting-marker scrub list, the supervisor env and `agent_env`.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Environment variables scrubbed from the spawned supervisor's (and
/// hence its capsule leg's) environment before launch — the exact list
/// `agents/claude/bin/ccb` unsets, for the identical reason: a
/// spawning parent's own Claude Code nesting markers make a fresh
/// `claude` mis-detect itself as nested/forked and exit silently.
/// `CLAUDECODE`/`AI_AGENT`/`CLAUDE_CODE_SESSION_ID` make it think it is
/// running INSIDE another claude; `CLAUDE_CODE_FORK_SUBAGENT`/
/// `CLAUDE_CODE_CHILD_SESSION`/`CLAUDE_CODE_TEAMMATE_MODE`/
/// `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS` make it think it is a forked/
/// teammate session. A daemon started from within a claude session (or
/// restarted by one) would otherwise propagate every one of these into
/// every capsule it spawns (ADR 0042 L1a, Codex review finding 9).
/// `NO_COLOR` rides the same path for the same reason: a daemon that
/// inherited it (a relaunch fired from inside a capsule hands its env to
/// the next daemon) painted every row's agent colourless on Windows,
/// where the pty sets no `TERM`/`COLORTERM` and `NO_COLOR` overrides
/// ConPTY's own VT detection (field report, 2026-09-17).
pub const NESTING_ENV_VARS_TO_SCRUB: &[&str] = &[
    "CLAUDE_CODE_FORK_SUBAGENT",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_TEAMMATE_MODE",
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
    "CLAUDECODE",
    "AI_AGENT",
    "CLAUDE_CODE_SESSION_ID",
    "NO_COLOR",
];

/// The `SOT_COMM_HOME` value (forward-slash string form — a git-bash/MSYS
/// shell, not this native Windows process, is what reads it; MSYS accepts
/// either slash spelling for a drive-letter-absolute path) to hand a
/// capsule's producer env, so its own comm scripts resolve the EXACT SAME
/// home this builds `SOT_COMM_SELF_FILE` from below — Codex round finding
/// 8: ONE resolver ([`crate::comm::sot_comm_home`], also what the
/// daemon's own registry reads use, `comm::registry::registry::comm_registry_path`),
/// injected into the child explicitly rather than left to each side's own
/// HOME/USERPROFILE guess landing on two different answers. `None` when
/// the resolver itself found nothing (matches comm-lib.sh: nothing to
/// pin).
fn capsule_comm_home_str() -> Option<String> {
    Some(crate::comm::sot_comm_home()?.to_string_lossy().replace('\\', "/"))
}

/// The `SOT_*` awareness env a capsule supervisor spawn stamps on its
/// producer — capsule-comm-identity fix: a capsule has no tmux pane, so
/// `comm-context.sh`'s pane-keyed self-file slot never applies to it, and
/// without `SOT_COMM_HOME`/`SOT_COMM_SELF_FILE` it fell back to the
/// shared per-host `__nopane` slot, colliding with any other no-pane
/// session on the same host (e.g. the frontend itself — the field bug
/// this fixes). Bare `SOT_SOCKET` (ADR 0046 decision 1, S4: no typed
/// prefix, Unix only), `SOT_WORKSPACE`/`SOT_WORKSPACE_ID` (and
/// `SOT_WORKSPACE_ROOT`/
/// `SOT_SESSION`/`SOT_MANUAL`) reuse [`crate::agents::awareness::awareness_env`]
/// verbatim — ONE builder, not a second copy that could drift — keyed on
/// `slug` (Codex round finding 1: the frontend keys results and the
/// active workspace by SLUG, not the internal `ws-<slug>-<hex>` id;
/// `workspace_id` is used ALSO below, for the state dir and self-file
/// paths, where stability and uniqueness matter more than the display
/// shape). `SOT_COMM_NAME` is set ONLY for an explicitly requested
/// `agent_name` (Codex round finding 2: a synthesized default here would
/// become an explicit pin that OVERWRITES any existing registry row of
/// that name — exactly what PROTOCOL.md's "never reuse a handle" forbids,
/// and a hand-started session in the same repo on the same host derives
/// precisely `<slug>-<host>` on its own). `SOT_COMM_SELF_FILE`
/// (`<comm_home>/self/<host>__<workspace_id>.txt`, comm-lib.sh's EXISTING
/// pin-the-self-file-path seam — already used by its own test suite, and
/// already honoured unchanged by both `comm-context.sh`, the reader, and
/// `comm-join.sh`, the writer) is what actually gives the capsule its own
/// slot: `comm-join.sh`'s #148 auto-disambiguating derivation decides the
/// handle and writes it there. The session inside the capsule now also
/// DECLARES it via `agent.join` over this same pinned `SOT_SOCKET`
/// (`Workspace.agent_handle`, `comm::registry::join::handle_agent_join`); a declared
/// `agent_handle` wins when present, but the daemon's OWN read-back of
/// that same file (`comm::registry::registry::capsule_comm_handle`) stays as the
/// FALLBACK for a row with no declaration yet (manager review, S5) —
/// deleted only with family H once every row has cycled onto `agent.join`.
pub fn capsule_supervisor_env(workspace_id: &str, slug: &str, cwd: &Path, agent_name: &str) -> Vec<(String, String)> {
    let mut env = crate::agents::awareness::awareness_env(Some(slug), Some(cwd), Some(workspace_id));
    if !agent_name.is_empty() {
        env.push(("SOT_COMM_NAME".to_string(), agent_name.to_string()));
    }
    if let Some(comm_home) = capsule_comm_home_str() {
        let host = crate::rows::store::declared_host();
        let self_file = format!("{}/self/{}__{}.txt", comm_home.trim_end_matches('/'), host, workspace_id);
        env.push(("SOT_COMM_HOME".to_string(), comm_home));
        env.push(("SOT_COMM_SELF_FILE".to_string(), self_file));
    }
    // Claude Code's feedback survey is a modal panel that holds a row's
    // session until someone answers it, so no row's agent shows it, on
    // any OS. Only this switch: telemetry and nonessential traffic stay
    // the user's own choice.
    env.push(("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY".to_string(), "1".to_string()));
    // A row's conversation never leaves its row (agent view moves it into Claude Code's own
    // daemon, outside the capsule); an env var, not a settings key, as --settings is not passed to every row.
    env.push(("CLAUDE_CODE_DISABLE_AGENT_VIEW".to_string(), "1".to_string()));
    // The `ccb` launcher's own PATH rule, promoted to the daemon: a leg
    // inherits the SERVICE's PATH, which has no `~/.local/bin` (CLAUDE.md's
    // documented gotcha — the same reason [`claude_argv`] full-paths
    // `claude`), so a capsule session found neither `gh`, the comm
    // launchers, nor any user-installed tool a tmux row's login shell
    // sees (found 2026-09-12: the first capsule-row release cut failed
    // its preflight on the system's ancient `gh`). Windows relies on the
    // daemon's own PATH already reaching everything, as `claude_argv`
    // documents.
    #[cfg(not(windows))]
    env.extend(agent_env(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
    ));
    env
}

/// The account half of a spawn's env: the account's env, its shared links and, for a claude row, the
/// folder-trust record, prepared in the config dir this spawn will use.
pub(crate) fn account_spawn_env(
    agent_kind: &str,
    account: &str,
    cwd: &Path,
    workspace_id: &str,
) -> std::io::Result<Vec<(String, String)>> {
    // Accounts brief: the SAME refusal `workspace.create` already
    // ran once, re-run here so a folder that vanished BETWEEN create
    // and this spawn (or a watchdog restart) refuses loudly instead
    // of silently starting on the default directory. `ErrorKind::
    // Unsupported` (never retried by the watchdog -- see its own
    // "no retry, marking terminal" arm) matches `qualified_state_root`'s
    // own classification just above: both are operator-fixable, not
    // transient.
    match crate::agents::accounts::account_home() {
        Some(home) => {
            let extra = crate::agents::accounts::account_env(agent_kind, account, &home)
                .map_err(|msg| std::io::Error::new(ErrorKind::Unsupported, msg))?;
            // Accounts brief: link the shared entries now that account_env
            // has proved the folder exists (sharing ruling: accounts.rs
            // module doc). Same refusal shape as account_env's own error
            // just above; ensure_account_links itself no-ops for an empty
            // account or "default", so no guard is needed here.
            crate::agents::accounts::ensure_account_links(&home, account)
                .map_err(|msg| std::io::Error::new(ErrorKind::Unsupported, msg))?;
            // Trusted-folder brief: with a root under the prefix the
            // owner declared, pre-answer claude's folder-trust dialog
            // in the config dir THIS spawn is about to use -- here,
            // where the config dir is prepared, so a first spawn and a
            // leg resumed against another account share the one call.
            // NEVER a refusal, unlike the two above it: the degraded
            // outcome is the dialog appearing, which is what happened
            // before this existed, and a row that will not start is
            // worse. Claude rows only -- no other agent has this dialog.
            if agent_kind == "claude" {
                use crate::agents::folder_trust::TrustOutcome;
                let prepared = effective_claude_trust_file(&home, &extra).and_then(|file| {
                    crate::agents::folder_trust::trusted_root_prefix().and_then(|prefix| {
                        crate::agents::folder_trust::ensure_folder_trusted(
                            &file,
                            cwd,
                            prefix.as_deref(),
                        )
                        .map(|outcome| (outcome, prefix, file))
                    })
                });
                match prepared {
                    Ok((TrustOutcome::Outside, prefix, _)) => tracing::warn!(
                        workspace_id, cwd = ?cwd, prefix = ?prefix,
                        outcome = ?TrustOutcome::Outside, "folder trust preparation skipped"
                    ),
                    Ok((TrustOutcome::NotDeclared, _, _)) => tracing::warn!(
                        workspace_id, declaration = ?crate::agents::trust_declaration::declaration_file(),
                        outcome = ?TrustOutcome::NotDeclared, "folder trust preparation skipped"
                    ),
                    Ok((outcome, _, file)) => {
                        tracing::debug!(workspace_id, cwd = ?cwd, outcome = ?outcome, trust_file = ?file, "folder trust preparation")
                    }
                    Err(msg) => tracing::warn!(
                        workspace_id, cwd = ?cwd, error = %msg,
                        "folder trust not recorded; the agent starts anyway"
                    ),
                }
            }
            Ok(extra)
        }
        None if account.is_empty() || account == "default" => Ok(Vec::new()),
        None => {
            return Err(std::io::Error::new(
                ErrorKind::Unsupported,
                format!("no home directory to resolve account {account:?} against"),
            ));
        }
    }
}

/// Select only the effective child's file; unsupported forms never guess another destination.
fn effective_claude_trust_file(
    home: &Path,
    additions: &[(String, String)],
) -> Result<PathBuf, String> {
    let config = additions
        .iter()
        .rev()
        .find(|(key, _)| key == "CLAUDE_CONFIG_DIR")
        .map(|(_, value)| std::ffi::OsString::from(value))
        .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR"));
    let Some(config) = config else {
        return Ok(home.join(".claude.json"));
    };
    let directory = PathBuf::from(config);
    if !directory.is_absolute() || directory.to_str().is_none() {
        return Err("CLAUDE_CONFIG_DIR form has no proven trust-file destination; nothing recorded and child configuration unchanged".into());
    }
    Ok(directory.join(".claude.json"))
}

/// `~/.local/bin` on `PATH`, prepended once — the ONE copy of this rule
/// (ADR 0046 decision 4), shared by every Unix agent-launch path: a
/// capsule supervisor spawn ([`capsule_supervisor_env`] above) and `sotd
/// agent-exec` (`main.rs`, execing THIS process into the agent directly)
/// both call it rather than keeping two copies of "prepend once, only
/// when absent." Pure — takes `PATH`/`HOME` explicitly rather than
/// reading `std::env` itself (mirrors [`resolve_claude`]'s own
/// testability rule) — so it is unit-tested without mutating global
/// process state. Returns a single `PATH` entry to add, or nothing when
/// `~/.local/bin` is already on it or there is no `HOME` to derive it
/// from. Windows relies on the daemon's/`claude`'s own `PATH` already
/// reaching everything (`claude_argv`'s doc) — this is never called
/// there.
#[cfg(not(windows))]
pub fn agent_env(path_var: Option<&std::ffi::OsStr>, home: Option<&Path>) -> Vec<(String, String)> {
    let Some(home) = home else {
        return Vec::new();
    };
    let local_bin = home.join(".local").join("bin");
    let inherited = path_var.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    if inherited.split(':').any(|dir| Path::new(dir) == local_bin) {
        return Vec::new();
    }
    let joined = if inherited.is_empty() {
        local_bin.display().to_string()
    } else {
        format!("{}:{inherited}", local_bin.display())
    };
    vec![("PATH".to_string(), joined)]
}

#[cfg(test)]
mod tests {
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
        // ADR 0046 decision 1 (manager review, S2/S4): the leg ALSO
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
}

#[cfg(test)]
mod trust_declaration_controls {
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
        if !sot_log::test_isolated::run_isolated(
            "agents::env::trust_declaration_controls::toml_declaration_handles_comments_and_literals") { return; }
        preparation_case(false);
        println!("W1 C1 commented declaration PASS");
    }

    #[test]
    fn invalid_declaration_is_diagnostic_and_writes_nothing() {
        if !sot_log::test_isolated::run_isolated(
            "agents::env::trust_declaration_controls::invalid_declaration_is_diagnostic_and_writes_nothing") { return; }
        preparation_case(true);
        println!("W1 C1 invalid declaration diagnostic PASS");
    }
}

#[cfg(test)]
mod trust_scope_controls {
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
        let capture = sot_log::test_log::capture();
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
        if !sot_log::test_isolated::run_isolated(
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
        if !sot_log::test_isolated::run_isolated(
            "agents::env::trust_scope_controls::symlink_escape_records_nothing",
        ) {
            return;
        }
        scope_case("symlink");
        println!("W1 C2 symlink PASS: no write; Outside with cwd and prefix");
    }
    #[test]
    fn outside_is_observable_and_preserves_the_account_env() {
        if !sot_log::test_isolated::run_isolated("agents::env::trust_scope_controls::outside_is_observable_and_preserves_the_account_env") { return; }
        scope_case("outside");
        println!("W1 C2 Outside PASS: cwd and prefix fields; account env unchanged");
    }
    #[test]
    fn absent_declaration_is_observable_and_preserves_the_account_env() {
        if !sot_log::test_isolated::run_isolated("agents::env::trust_scope_controls::absent_declaration_is_observable_and_preserves_the_account_env") { return; }
        scope_case("undeclared");
        println!("W1 C2 NotDeclared PASS: declaration file field; account env unchanged");
    }
}

#[cfg(test)]
mod trust_config_controls {
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
        if !sot_log::test_isolated::run_isolated(
            "agents::env::trust_config_controls::inherited_absolute_config_receives_the_record",
        ) {
            return;
        }
        config_case("inherited");
    }
    #[test]
    fn unset_config_uses_the_home_level_file() {
        if !sot_log::test_isolated::run_isolated(
            "agents::env::trust_config_controls::unset_config_uses_the_home_level_file",
        ) {
            return;
        }
        config_case("unset");
    }
    #[test]
    fn named_addition_overrides_inherited_config() {
        if !sot_log::test_isolated::run_isolated(
            "agents::env::trust_config_controls::named_addition_overrides_inherited_config",
        ) {
            return;
        }
        config_case("named");
    }
    #[test]
    fn config_guard_restores_config_directories_on_unwind() {
        if !sot_log::test_isolated::run_isolated("agents::env::trust_config_controls::config_guard_restores_config_directories_on_unwind") { return; }
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
}
