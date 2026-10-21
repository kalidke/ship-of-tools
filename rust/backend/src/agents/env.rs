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

/// The `SOTD_BIN` value for a session: the path this daemon was started by, made absolute the way its start found it,
/// without following a link. A path (absolute, relative, or drive-relative such as `C:sotd.exe`) is made absolute by
/// `std::path::absolute`: against the working folder, and a drive-relative one against that drive's. A bare name is
/// looked up as a start looks it up: on Windows in the working folder and then each `PATH` entry, with `.exe` added to
/// a name that has no extension; elsewhere in each `PATH` entry. The first runnable file is the value. Forward slashes
/// on Windows, where a git-bash/MSYS shell reads it, as with `SOT_COMM_HOME`. The start path, never the resolved one: a
/// session outlives its daemon, and an update replaces the file at that path. `None` for an empty start path, or a bare
/// name found nowhere; the session's shell then finds `sotd` by its own ladder.
fn sotd_bin_value(start: &std::ffi::OsStr, path_var: Option<&std::ffi::OsStr>) -> Option<String> {
    if start.is_empty() {
        return None;
    }
    let start = Path::new(start);
    let mut parts = start.components();
    let found = if matches!(
        (parts.next(), parts.next()),
        (Some(std::path::Component::Normal(_)), None)
    ) {
        let name = match start.extension() {
            None if cfg!(windows) => start.with_extension("exe"),
            _ => start.to_path_buf(),
        };
        cfg!(windows)
            .then(PathBuf::new)
            .into_iter()
            .chain(path_var.into_iter().flat_map(std::env::split_paths))
            .filter_map(|dir| std::path::absolute(dir.join(&name)).ok())
            .find(|candidate| crate::rows::spawn::detach::runnable_file(candidate))?
    } else {
        std::path::absolute(start).ok()?
    };
    let text = found.to_string_lossy();
    Some(if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.into_owned()
    })
}

static OWN_SOTD_BIN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// This daemon's `SOTD_BIN` value ([`sotd_bin_value`] of its argv[0] and `PATH`), made once: `main` calls this at
/// startup, before it serves anything.
pub fn own_sotd_bin() -> Option<&'static str> {
    OWN_SOTD_BIN
        .get_or_init(|| {
            sotd_bin_value(
                &std::env::args_os().next()?,
                std::env::var_os("PATH").as_deref(),
            )
        })
        .as_deref()
}

/// A row's comm home and its pinned self file, `<comm home>/self/<declared host>__<row id>.txt`, in the forward-slash
/// form a Git Bash shell reads; `None` when no comm home can be named.
pub fn row_self_file(workspace_id: &str) -> Option<(String, String)> {
    let comm_home = capsule_comm_home_str()?;
    let host = crate::rows::store::declared_host();
    let self_file = format!("{}/self/{}__{}.txt", comm_home.trim_end_matches('/'), host, workspace_id);
    Some((comm_home, self_file))
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
/// shape). No `SOT_COMM_NAME`: no command chooses a handle (decision 0031). A row created with a handle keeps it
/// through its pinned self file, which the start writes before the session first joins
/// (`comm::identity::self_file::pin_row`, from `rows::spawn::detach`); an unnamed row's first join derives one.
/// `SOT_COMM_SELF_FILE`
/// (`<comm_home>/self/<host>__<workspace_id>.txt`, comm-lib.sh's EXISTING
/// pin-the-self-file-path seam — already used by its own test suite, and
/// already honoured unchanged by both `comm-context.sh`, the reader, and
/// `comm-join.sh`, the writer) is what actually gives the capsule its own
/// slot: `comm-join.sh`'s #148 auto-disambiguating derivation decides the
/// handle and writes it there. The session inside the capsule now also
/// DECLARES it via `agent.join` over this same pinned `SOT_SOCKET`
/// (`Workspace.agent_handle`, `rows::ops::join::handle_agent_join`), and the
/// declared handle is the row's (`rows::handle::bound_handle`); the daemon never
/// reads the self-file back.
/// `SOTD_BIN` is this daemon's own start path ([`own_sotd_bin`]), so the session's comm shell bridges with the daemon's
/// own binary.
pub fn capsule_supervisor_env(workspace_id: &str, slug: &str, cwd: &Path) -> Vec<(String, String)> {
    let mut env = crate::agents::awareness::awareness_env(Some(slug), Some(cwd), Some(workspace_id));
    if let Some((comm_home, self_file)) = row_self_file(workspace_id) {
        env.push(("SOT_COMM_HOME".to_string(), comm_home));
        env.push(("SOT_COMM_SELF_FILE".to_string(), self_file));
    }
    // The comm shell in the session bridges with SOTD_BIN first, so it is this daemon's own binary, over any value the
    // daemon inherited (detach.rs applies these pairs with `Command::env`).
    if let Some(sotd) = own_sotd_bin() {
        env.push(("SOTD_BIN".to_string(), sotd.to_string()));
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
            // has proved the folder exists (sharing rule: accounts.rs
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
#[path = "env_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "env_trust_declaration_controls_tests.rs"]
mod trust_declaration_controls;

#[cfg(test)]
#[path = "env_trust_scope_controls_tests.rs"]
mod trust_scope_controls;

#[cfg(test)]
#[path = "env_trust_config_controls_tests.rs"]
mod trust_config_controls;
