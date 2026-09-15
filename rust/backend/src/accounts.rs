// accounts.rs — per-session accounts, discovered rather than declared.
//
// The owner's ruling (superseding the earlier `[account.*]`-in-hosts.toml
// plan, and later refined again): an account is nothing but a FOLDER the
// user creates and logs into with the agent's own CLI, outside Ship of
// Tools entirely. Nothing is declared anywhere — the daemon DISCOVERS
// accounts in its own home at request time by listing what is actually
// there. This module owns both halves: [`discover_accounts`]
// (informational — `accounts.list`) and [`account_env`] (load-bearing —
// what a capsule spawn actually sets).
//
// Convention: the DEFAULT account is the agent's own normal config folder
// (`~/.claude`, `~/.codex`). A NAMED account is a SUBDIRECTORY of one
// dedicated parent folder, `~/.claude-auth/<name>` — never a sibling
// `.claude-<name>` folder (that scheme is retired: on a real machine it
// collided with unrelated folders that merely matched the naming pattern,
// e.g. a session-state directory or an old per-account auth layout, and
// there was no way to tell those apart from a real one without opening
// files). A subdirectory needs nothing inside it to count — even empty it
// is a valid, not-yet-logged-in account (the agent logs in on first run).
// Nothing else about an account is ever recorded — no path, no token — so
// the SAME rule re-derives the same folder on every box that shares this
// home, and a Windows host applies it under `%USERPROFILE%` unchanged.
//
// Codex accounts are OUT of this release (deferred, not designed away):
// this deployment keeps its Codex home on local disk with one login, so
// there is nothing to discover yet. [`discover_accounts`] never looks for
// a `.claude-auth`-shaped parent under `.codex`, and [`account_env`]
// refuses a named account on a codex row outright rather than pretending
// to support it. Adding Codex accounts later is a matter of mirroring the
// claude half of this module, not a redesign.

use std::path::{Path, PathBuf};

/// Claude's own config dir basename (`CLAUDE_CONFIG_DIR`'s default).
const CLAUDE_DIR_PREFIX: &str = ".claude";
/// Codex's own config dir basename — `ccx`'s own resolution
/// (`CODEX_HOME_DIR="${CODEX_HOME:-$HOME/.codex}"`, `comm/adapters/codex/bin/ccx`).
const CODEX_DIR_PREFIX: &str = ".codex";
/// The one dedicated parent directory named accounts live under, directly
/// in the home — `~/.claude-auth/<name>`. Never a sibling `.claude-<name>`
/// folder (see the module doc comment for why that scheme was retired).
const CLAUDE_ACCOUNTS_DIR: &str = ".claude-auth";
/// Claude Code's on-disk OAuth token file, directly under its config
/// dir. Checked for EXISTENCE ONLY (never opened) — this is what
/// `logged_in` means for a claude account.
const CLAUDE_CREDENTIALS_FILE: &str = ".credentials.json";
/// Codex's own on-disk auth file, directly under `$CODEX_HOME`. Checked
/// for EXISTENCE ONLY (never opened) — this is what `logged_in` means
/// for the default codex account (the only one this release has).
const CODEX_CREDENTIALS_FILE: &str = "auth.json";

/// `true` iff `name` is a valid account name: `^[a-z0-9][a-z0-9_-]*$`,
/// the exact character class the brief pins. A subdirectory of
/// `.claude-auth` whose name fails this is ignored outright by
/// [`discover_accounts`] — never partially matched, never a source of a
/// mangled account name.
pub fn is_account_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// One discovered account: which agent kinds had a folder found for this
/// name, and — for each of those same kinds only — whether that folder's
/// own credentials file exists. `kinds`/`logged_in` never mention a kind
/// this account has no folder for at all. A NAMED account's `kinds` is
/// always exactly `["claude"]` this release (Codex accounts are
/// deferred); only `"default"` can carry `"codex"` too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredAccount {
    pub name: String,
    pub kinds: Vec<String>,
    pub logged_in: std::collections::BTreeMap<String, bool>,
}

/// Pure discovery over `home` — never touches `std::env` itself, so it
/// is exercised directly against a temp dir in tests; the `accounts.list`
/// op handler is the one real caller that resolves `$HOME`/`%USERPROFILE%`
/// (via [`account_home`]) and passes it in.
///
/// Two, unrelated sources: the DEFAULT row (one entry per agent kind
/// whose own config folder — `.claude` or `.codex` — exists directly in
/// `home`), and NAMED claude accounts (one entry per direct subdirectory
/// of `home/.claude-auth` whose name passes [`is_account_name`]; a
/// non-directory entry there — e.g. a stray file — is skipped outright).
/// Grouped by name and returned sorted with "default" first, the rest
/// alphabetically after it (folder order on disk is never trusted).
pub fn discover_accounts(home: &Path) -> Vec<DiscoveredAccount> {
    let mut by_name: std::collections::BTreeMap<String, DiscoveredAccount> = std::collections::BTreeMap::new();

    let mut add = |name: String, kind: &str, logged_in: bool| {
        let acct = by_name.entry(name.clone()).or_insert_with(|| DiscoveredAccount {
            name,
            kinds: Vec::new(),
            logged_in: std::collections::BTreeMap::new(),
        });
        acct.kinds.push(kind.to_string());
        acct.logged_in.insert(kind.to_string(), logged_in);
    };

    for (prefix, kind, credentials_file) in [
        (CLAUDE_DIR_PREFIX, "claude", CLAUDE_CREDENTIALS_FILE),
        (CODEX_DIR_PREFIX, "codex", CODEX_CREDENTIALS_FILE),
    ] {
        let dir = home.join(prefix);
        if !dir.is_dir() {
            continue;
        }
        add("default".to_string(), kind, dir.join(credentials_file).is_file());
    }

    if let Ok(entries) = std::fs::read_dir(home.join(CLAUDE_ACCOUNTS_DIR)) {
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
            if !is_account_name(&name) || !entry.path().is_dir() {
                continue;
            }
            let logged_in = entry.path().join(CLAUDE_CREDENTIALS_FILE).is_file();
            add(name, "claude", logged_in);
        }
    }

    let mut out = Vec::with_capacity(by_name.len());
    if let Some(default) = by_name.remove("default") {
        out.push(default);
    }
    out.extend(by_name.into_values());
    out
}

/// `$HOME` (Unix) / `%USERPROFILE%` (Windows) — the one home this
/// module's real (non-test) callers discover and resolve accounts
/// against. `None` when neither is set, same "nothing to resolve
/// against" shape [`account_env`]'s caller already handles.
pub fn account_home() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// The load-bearing half: resolve `agent_kind`'s account env additions
/// for `account`, against `home` — pure (no `std::env` read: `home` is
/// explicit) so `workspace.create`'s fast-failure check and the real
/// spawn path ([`crate::capsule_workspace::runtime::spawn_detached_supervisor`])
/// share this ONE rule rather than a copy each could drift from.
///
/// `account` empty (or literally `"default"`) returns no additions at
/// all — today's behaviour, byte-for-byte: the default directory is
/// never named on the wire or in the child's env. Otherwise: only
/// `"claude"` has named accounts — `home/.claude-auth/<account>` becomes
/// `CLAUDE_CONFIG_DIR`, paired with `SOT_ACCOUNT=<name>`. `"codex"`
/// refuses outright (accounts deferred there — see the module doc
/// comment); any other kind (a bash/`"none"` row) refuses too — a bash
/// row has no account to spend. Refuses LOUDLY on a missing claude
/// subdirectory too, never silently falling back to the default
/// directory (spending the wrong subscription while the row claims the
/// right one): the message names the exact one-line command that creates
/// it.
pub fn account_env(agent_kind: &str, account: &str, home: &Path) -> Result<Vec<(String, String)>, String> {
    if account.is_empty() || account == "default" {
        return Ok(Vec::new());
    }
    match agent_kind {
        "claude" => {
            let dir = home.join(CLAUDE_ACCOUNTS_DIR).join(account);
            if !dir.is_dir() {
                return Err(format!(
                    "unknown account {account:?}: mkdir -p ~/{CLAUDE_ACCOUNTS_DIR}/{account} -- the first session in it logs in"
                ));
            }
            Ok(vec![
                ("CLAUDE_CONFIG_DIR".to_string(), dir.to_string_lossy().into_owned()),
                ("SOT_ACCOUNT".to_string(), account.to_string()),
            ])
        }
        "codex" => Err(format!(
            "codex accounts are deferred this release: a codex row always runs the default login, account {account:?} has no folder to use"
        )),
        other => Err(format!("a {other:?} row has no account (only claude rows do)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch_dir(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
    }
    fn touch_file(path: &Path) {
        std::fs::write(path, b"").unwrap();
    }

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
        // Owner clarification: a folder that exists but has never been
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
}
