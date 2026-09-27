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
//
// The sharing ruling (2026-09-15): a named account folder shares
// EVERYTHING the default `.claude` folder carries except the login —
// symlinked in by [`ensure_account_links`] the first time a row spawns
// against that account, so a folder made with a bare `mkdir` is fully
// linked before its first session starts. It is an ALLOWLIST
// ([`SHARED_ENTRIES`]), not a denylist: an unknown or new entry stays
// per-account by default, and the OAuth identity plus any runtime state a
// live process owns are never on that list — sharing those would let two
// accounts' processes collide on the same lock file, or let a session
// spend another account's login. An entry the account folder already has,
// in any form, is left exactly as it is: it deliberately overrides the
// shared one rather than being replaced by it.

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

/// The allowlist behind the sharing ruling (see the module doc) — each
/// name is linked (see [`ensure_account_links`]) only if the default
/// folder actually has it, and every other entry stays per-account by
/// default, never shared by accident. Grouped by the invariant each group
/// serves:
///
/// - what a session reads to know how to behave, independent of which
///   subscription it spends: `CLAUDE.md`, `settings.json` (hooks,
///   permissions, model, status line), `agents`, `commands`, `skills`,
///   `plugins`, `output-styles`;
/// - continuity of what the user has been doing, which must survive a
///   switch of which account a session runs as: `projects` (per-project
///   memory) and `history.jsonl` (prompt history).
///
/// Deliberately NEVER in this list, and never added to it implicitly: the
/// OAuth identity (`.claude.json`, `.credentials.json` — the entire reason
/// a named account exists is to hold a SEPARATE login) and any runtime
/// state a live process owns or mutates (a background daemon directory,
/// `sessions`, `teams`, `tasks`, `jobs`, `ide`, caches) — sharing those
/// would let two accounts' processes collide on the same lock file or
/// session state while running under different credentials.
///
/// `settings.json` is shared as written, so it must never carry a
/// credential-selecting setting (`apiKeyHelper`, `env.ANTHROPIC_API_KEY`,
/// `forceLoginOrgUUID`) — any of those would let the shared file itself
/// pick which login a session spends, defeating the separate-login
/// invariant this whole module exists for.
const SHARED_ENTRIES: &[&str] = &[
    "CLAUDE.md",
    "settings.json",
    "agents",
    "commands",
    "skills",
    "plugins",
    "output-styles",
    "projects",
    "history.jsonl",
];

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

/// Refuse a syntactically invalid account name before it ever reaches a
/// path join — the one guard both [`account_env`] (create and spawn) and
/// [`ensure_account_links`] run at their very top, so `".."`,
/// `"../.claude"`, an absolute path, or a name with a `/` in it can never
/// resolve outside `.claude-auth/<name>` (an empty account or `"default"`
/// never reaches this check — both return earlier as no-ops).
fn check_account_name(account: &str) -> Result<(), String> {
    if is_account_name(account) {
        return Ok(());
    }
    Err(format!(
        "invalid account name {account:?}: an account name is lowercase letters, digits, \
         '-' and '_' only, starting with a letter or digit"
    ))
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

/// Where an account's claude config dir IS — the default folder for the
/// default account (`""` or `"default"`), its own folder under
/// `.claude-auth` for a named one. The ONE place that mapping lives:
/// [`account_env`] hands it to the child as `CLAUDE_CONFIG_DIR`,
/// [`ensure_account_links`] links the shared entries into it, and
/// [`crate::reauth::check`] resolves the target account's transcripts
/// under it. Never validates the name — a caller that takes one off the
/// wire checks it first ([`check_account_name`]).
pub fn claude_config_dir(home: &Path, account: &str) -> PathBuf {
    if account.is_empty() || account == "default" {
        home.join(CLAUDE_DIR_PREFIX)
    } else {
        home.join(CLAUDE_ACCOUNTS_DIR).join(account)
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
    check_account_name(account)?;
    match agent_kind {
        "claude" => {
            let dir = claude_config_dir(home, account);
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

/// Unix half of the one symlink [`ensure_account_links`] creates per
/// entry: `target` is the ABSOLUTE `<home>/.claude/<entry>` path, `link`
/// is where it lands inside the account folder.
#[cfg(unix)]
fn make_shared_link(target: &Path, link: &Path, _target_is_dir: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Windows half: a directory reparse point needs `symlink_dir`, anything
/// else `symlink_file` — the brief pins both, no junctions, no cfg-gated
/// second rule beyond this one already-required split.
#[cfg(windows)]
fn make_shared_link(target: &Path, link: &Path, target_is_dir: bool) -> std::io::Result<()> {
    if target_is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// Link `home/.claude-auth/<account>` up to the default `home/.claude`
/// folder for every [`SHARED_ENTRIES`] name (see the module doc for the
/// sharing ruling this implements). Called once, from the spawn path
/// ([`crate::capsule_workspace::runtime::spawn_detached_supervisor`]),
/// right after [`account_env`] succeeds, so a folder made with a bare
/// `mkdir` is fully linked on its very first session — no separate
/// installer step to forget, and nothing here writes to the DEFAULT
/// folder, only into the account's own.
///
/// Refuses an invalid account name before any path join, same check as
/// [`account_env`] (see [`check_account_name`]) — public, so it must not
/// trust a caller to have validated `account` already. One ABSOLUTE
/// symlink per entry, `<home>/.claude/<entry>` built from the `home` this
/// function is given (never a relative `../../.claude/<entry>`, which
/// resolves to the wrong place when `.claude-auth` or the account folder
/// is itself a symlink): a no-op for an empty account or `"default"`; an
/// entry the default folder does not have is never linked (never produce
/// a dangling symlink); an entry that already exists in the account
/// folder in ANY form — real file or directory, an existing symlink,
/// even a dangling one (`symlink_metadata` succeeding is the test, not
/// `exists`, which follows and would miss a dangling link) — is left
/// alone: it deliberately overrides the shared one, and is NEVER
/// replaced or removed. A sibling spawn racing to link the SAME entry
/// between our existence check and the OS call is tolerated
/// (`ErrorKind::AlreadyExists` — see `resume_all`'s parallel spawns);
/// any other creation error refuses the whole call: a half-linked
/// account folder starting a degraded session is worse than a refused
/// spawn.
pub fn ensure_account_links(home: &Path, account: &str) -> Result<(), String> {
    if account.is_empty() || account == "default" {
        return Ok(());
    }
    check_account_name(account)?;
    let default_dir = claude_config_dir(home, "");
    let account_dir = claude_config_dir(home, account);
    for name in SHARED_ENTRIES {
        let source = default_dir.join(name);
        if !source.exists() {
            continue; // the default folder doesn't have this entry either
        }
        let link = account_dir.join(name);
        if std::fs::symlink_metadata(&link).is_ok() {
            continue; // something is already there -- never replace it
        }
        link_or_skip_if_racing(&source, &link, source.is_dir())
            .map_err(|err| format!("could not link {name} into account {account:?}: {err}"))?;
    }
    Ok(())
}

/// [`make_shared_link`], tolerating a sibling spawn that created the SAME
/// link between two callers' existence checks and this call:
/// `ErrorKind::AlreadyExists` from the OS call itself means the entry is
/// already linked, not a failure (`resume_all` spawns rows in parallel,
/// so two rows on one brand-new account both pass the existence check
/// before either has linked anything). Any other OS error still refuses
/// the whole [`ensure_account_links`] call.
fn link_or_skip_if_racing(target: &Path, link: &Path, target_is_dir: bool) -> std::io::Result<()> {
    match make_shared_link(target, link, target_is_dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(err),
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

        ensure_account_links(home, "team").unwrap();

        for name in ["CLAUDE.md", "settings.json", "skills", "history.jsonl"] {
            let link = account_dir.join(name);
            let meta = std::fs::symlink_metadata(&link).unwrap();
            assert!(meta.file_type().is_symlink(), "{name} should be a symlink");
            let target = std::fs::read_link(&link).unwrap();
            assert!(target.is_absolute(), "{name}'s link target should be absolute: {target:?}");
            assert_eq!(
                target,
                home.join(".claude").join(name),
                "{name} should point straight at the default's own entry"
            );
        }
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

        link_or_skip_if_racing(&target, &link, false).unwrap();
        assert!(
            link_or_skip_if_racing(&target, &link, false).is_ok(),
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
}
