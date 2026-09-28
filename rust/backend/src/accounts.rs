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

/// Claude Code's own per-folder trust record, a JSON object keyed by
/// absolute project path under `projects`, each entry carrying
/// `hasTrustDialogAccepted` (see [`ensure_folder_trusted`]).
const CLAUDE_TRUST_FILE: &str = ".claude.json";
/// This box's own settings file, in the daemon's config directory --
/// where the installed declaration lives (see below).
const SETTINGS_FILE: &str = "settings.toml";
/// The bool inside a `projects` entry that means "this folder's trust
/// dialog is answered" -- claude's own key name, not ours.
const TRUST_ACCEPTED_KEY: &str = "hasTrustDialogAccepted";
/// Where the owner DECLARES which folders he has already trusted: one
/// absolute path prefix, and every row root under it counts as declared.
/// It lives in THIS BOX's own config -- `[trust] root_prefix` in the
/// user-level `settings.toml` -- and the installer writes it there for the
/// box it runs on, which is the only place a real path belongs. NOT
/// `hosts.toml`, and NOT a repo's project-level `.sot/settings.toml`,
/// which a checkout could use to declare itself trusted -- the user-level
/// file only. Absent or empty means NOTHING is declared and nothing is
/// ever written.
const TRUSTED_ROOT_PREFIX_SECTION: &str = "trust";
const TRUSTED_ROOT_PREFIX_KEY: &str = "root_prefix";
/// Overrides the declaration above for a daemon running outside an install
/// (a scratch or test daemon, which has no config of its own to edit).
const TRUSTED_ROOT_PREFIX_ENV: &str = "SOT_TRUSTED_ROOT_PREFIX";

/// The declared prefix for this daemon, or `None` when nothing is declared
/// -- in which case the folder-trust dialog is answered by hand exactly as
/// it was before this existed. This repo ships no default and names no
/// path: a default here would either leak a path into a public repo or
/// trust folders nobody declared.
pub fn trusted_root_prefix() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os(TRUSTED_ROOT_PREFIX_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(v));
    }
    declared_root_prefix(&crate::workspaces::app_config_dir())
}

/// Read the declaration out of `<config_dir>/settings.toml`. Read at every
/// spawn rather than cached at boot, so an install -- or the owner editing
/// the line -- takes effect on the next row without restarting the daemon.
/// Unreadable or absent declares nothing, exactly as an empty value does.
fn declared_root_prefix(config_dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(config_dir.join(SETTINGS_FILE)).ok()?;
    parse_declared_root_prefix(&text)
}

/// Pick the one key out of that file by hand, the way every other `.toml`
/// in this workspace is read (there is no toml dependency anywhere here --
/// see the frontend's own `settings` parser). Section headers and
/// `key = value`, quotes stripped; every other key belongs to somebody
/// else and is ignored, and the last declaration wins as it does there.
fn parse_declared_root_prefix(text: &str) -> Option<PathBuf> {
    let mut section = String::new();
    let mut declared = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        if section != TRUSTED_ROOT_PREFIX_SECTION || key.trim() != TRUSTED_ROOT_PREFIX_KEY {
            continue;
        }
        let value = value.trim().trim_matches('"').trim();
        declared = (!value.is_empty()).then(|| PathBuf::from(value));
    }
    declared
}

/// Which `.claude.json` records trust for `account`. NOT
/// `claude_config_dir(..).join(..)` for the default account: with
/// `CLAUDE_CONFIG_DIR` unset claude keeps this file BESIDE its config
/// folder, in the home itself (`<home>/.claude/.claude.json` exists too
/// on a real box but carries no `projects` table, so writing there would
/// write a file claude never reads). A NAMED account has
/// `CLAUDE_CONFIG_DIR` set to its own folder ([`account_env`]) and the
/// file is inside it.
pub fn claude_trust_file(home: &Path, account: &str) -> PathBuf {
    if account.is_empty() || account == "default" {
        home.join(CLAUDE_TRUST_FILE)
    } else {
        claude_config_dir(home, account).join(CLAUDE_TRUST_FILE)
    }
}

/// Record `root` as trusted in `account`'s trust file when -- and only
/// when -- it lies under the owner's declared `prefix`, so a row the
/// daemon spawns does not stop at claude's folder-trust dialog for a
/// folder the owner has already declared trusted. That 2026-09-27 ruling
/// supersedes the earlier "the daemon must never pre-trust a root" rule
/// ONLY inside the declared set: outside it the older rule still stands
/// and this function writes nothing whatsoever.
///
/// Pure in the same sense as [`account_env`] -- `home` and `prefix` are
/// arguments, never read from the environment here -- so the one caller
/// ([`crate::capsule_workspace::runtime::spawn_detached_supervisor`])
/// resolves the declaration once and the tests need no process state.
/// Called where the config dir is prepared, which is why a first spawn
/// and a leg resumed after a reauth need no case of their own: the
/// reauth's target account has a config dir that never trusted this
/// folder, so it stops for exactly the same reason and is fixed by
/// exactly the same call.
///
/// `Ok(true)` means this call recorded the flag; `Ok(false)` that there
/// was nothing to do -- nothing declared, `root` outside the declared
/// set, or claude already trusts it. `Err` is a REPORT, never a refusal:
/// the caller logs it and starts the agent anyway. The worst acceptable
/// outcome is the owner answering the dialog by hand (what happened
/// before this existed); a row that will not start is a worse one.
///
/// `Path::starts_with` is a COMPONENT-wise test, not a string one: a
/// sibling directory whose name merely begins with the declared one is
/// outside the declared set. A `root` equal to the prefix is inside it --
/// that tree is what the owner declared.
///
/// Claude Code owns this file, so nothing here assumes it is ours: every
/// other key survives, a shape we cannot parse is refused rather than
/// replaced, and the new bytes are published with a temp file plus a
/// plain `rename` (never `renameat2`'s flags -- `RENAME_NOREPLACE` and
/// `RENAME_EXCHANGE` return `EINVAL` on an NFS home).
pub fn ensure_folder_trusted(
    home: &Path,
    account: &str,
    root: &Path,
    prefix: Option<&Path>,
) -> Result<bool, String> {
    let Some(prefix) = prefix else { return Ok(false) };
    if !prefix.is_absolute() {
        return Err(format!(
            "declared trusted-folder prefix {prefix:?} (${TRUSTED_ROOT_PREFIX_ENV}) is not an absolute path: nothing is trusted"
        ));
    }
    if !root.is_absolute() {
        return Err(format!("project root {root:?} is not an absolute path: nothing is trusted"));
    }
    if !root.starts_with(prefix) {
        return Ok(false);
    }

    let path = claude_trust_file(home, account);
    let at = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let existing = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(at(&e)),
    };
    let mut doc = match existing.as_deref() {
        Some(bytes) => serde_json::from_slice::<serde_json::Value>(bytes).map_err(|e| at(&e))?,
        None => serde_json::Value::Object(serde_json::Map::new()),
    };
    let top = doc.as_object_mut().ok_or_else(|| at(&"not a JSON object"))?;
    let projects = top
        .entry("projects")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| at(&"\"projects\" is not a JSON object"))?;
    let entry = projects
        .entry(root.to_string_lossy().into_owned())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| at(&"this project's own entry is not a JSON object"))?;
    // Already answered: leave the file exactly as it is rather than
    // rewriting it on every spawn -- claude may be writing it right now.
    if entry.get(TRUST_ACCEPTED_KEY).and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(false);
    }
    entry.insert(TRUST_ACCEPTED_KEY.to_string(), serde_json::Value::Bool(true));

    let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| at(&e))?;
    publish_trust_file(&path, &bytes).map_err(|e| at(&e))?;
    Ok(true)
}

/// Publish [`ensure_folder_trusted`]'s bytes: temp file in the SAME
/// directory, then a plain `rename`. The temp name carries this process's
/// id so two spawns racing on one config dir -- and claude's own temp
/// file, whatever it is called -- never collide, and a failed attempt
/// leaves no litter behind. Mode is carried over from the file being
/// replaced (claude keeps it owner-only); a file we create is owner-only
/// too, never whatever the daemon's umask happens to be.
fn publish_trust_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or(CLAUDE_TRUST_FILE);
    let tmp = dir.join(format!("{name}.sot-{}.tmp", std::process::id()));
    let published = (|| -> std::io::Result<()> {
        std::fs::write(&tmp, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path).map(|m| m.permissions().mode() & 0o777).unwrap_or(0o600);
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
        }
        std::fs::rename(&tmp, path)
    })();
    if published.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    published
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

    // ---- declared-folder trust (the prompt the daemon pre-answers) ----

    /// `trusted_root_prefix` reads the process environment; serialise the
    /// tests that mutate it (same pattern as `proxy`'s own env tests).
    static TRUST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Stand-in for the parent directory the owner declares: made inside a
    /// tempdir, so no real path from any machine appears in this repo.
    fn declared_parent(home: &Path) -> PathBuf {
        let p = home.join("declared-parent");
        touch_dir(&p);
        p
    }

    /// What the trust file actually records for `root` -- `None` when the
    /// file, the `projects` table, or the entry is absent.
    fn recorded_trust(file: &Path, root: &Path) -> Option<bool> {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        v.get("projects")?
            .get(root.to_string_lossy().as_ref())?
            .get("hasTrustDialogAccepted")?
            .as_bool()
    }

    #[test]
    fn a_root_under_the_declared_prefix_is_recorded_as_trusted() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let root = parent.join("some-repo");
        touch_dir(&root);

        assert_eq!(ensure_folder_trusted(home, "", &root, Some(&parent)), Ok(true));
        assert_eq!(recorded_trust(&claude_trust_file(home, ""), &root), Some(true));
    }

    #[test]
    fn a_root_outside_the_declared_prefix_leaves_the_file_byte_identical() {
        // The 2026-09-03 ruling still standing where it applies: outside the
        // declared set the daemon adds no trust flag at all.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let outside = home.join("elsewhere").join("some-repo");
        touch_dir(&outside);
        let file = claude_trust_file(home, "");
        let before = br#"{"numStartups":3,"projects":{}}"#.to_vec();
        std::fs::write(&file, &before).unwrap();

        assert_eq!(ensure_folder_trusted(home, "", &outside, Some(&parent)), Ok(false));
        assert_eq!(std::fs::read(&file).unwrap(), before, "byte-identical");
    }

    #[test]
    fn the_declared_prefix_matches_whole_path_components_only() {
        // `<parent>-other` is NOT under `<parent>`: a plain string-prefix
        // test would trust it, which is precisely the over-grant to refuse.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let sibling = PathBuf::from(format!("{}-other", parent.display())).join("some-repo");
        touch_dir(&sibling);

        assert_eq!(ensure_folder_trusted(home, "", &sibling, Some(&parent)), Ok(false));
        assert!(!claude_trust_file(home, "").exists(), "nothing may be written");
    }

    #[test]
    fn a_dot_dot_spelling_that_escapes_the_prefix_writes_an_inert_key() {
        // `Path::starts_with` is LEXICAL, so a root spelled with `..` passes
        // the prefix test although it RESOLVES outside the declared parent.
        // That is not an over-grant, and this test is what says so rather
        // than leaving it as a property nobody wrote down: the key recorded
        // is the literal spelling (`ensure_folder_trusted` keys on `root` as
        // passed), while claude looks a project up by its RESOLVED `getcwd()`.
        // So the escaping root's real path gets NO entry, the dialog still
        // appears there, and the write grants nothing it should not.
        //
        // The inertness is a property of the KEYING, not a check -- which is
        // exactly why it is pinned here. A future change that normalises the
        // key before writing it would turn this same lexical gap into a real
        // over-grant, and this test is what would go red.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let escaped = home.join("elsewhere").join("some-repo");
        touch_dir(&escaped);
        let spelled = parent.join("..").join("elsewhere").join("some-repo");

        assert!(
            spelled.starts_with(&parent),
            "the lexical prefix test PASSES for this spelling -- that is the premise of the test"
        );
        assert_eq!(ensure_folder_trusted(home, "", &spelled, Some(&parent)), Ok(true));

        let file = claude_trust_file(home, "");
        assert_eq!(
            recorded_trust(&file, &spelled),
            Some(true),
            "the literal spelling is what gets written"
        );
        assert_eq!(
            recorded_trust(&file, &escaped),
            None,
            "the resolved path -- the one claude would look up -- gets nothing, so the write is inert"
        );
    }

    #[test]
    fn recording_trust_preserves_every_other_key_and_other_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let root = parent.join("some-repo");
        touch_dir(&root);
        let other = "/not-under-the-prefix/another-project";
        let file = claude_trust_file(home, "");
        std::fs::write(
            &file,
            serde_json::to_vec_pretty(&serde_json::json!({
                "numStartups": 7,
                "installMethod": "native",
                "projects": {
                    other: { "hasTrustDialogAccepted": false, "allowedTools": ["Bash"] },
                },
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(ensure_folder_trusted(home, "", &root, Some(&parent)), Ok(true));
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(v["numStartups"], 7);
        assert_eq!(v["installMethod"], "native");
        assert_eq!(v["projects"][other]["hasTrustDialogAccepted"], false, "another project's own answer stands");
        assert_eq!(v["projects"][other]["allowedTools"][0], "Bash");
        assert_eq!(recorded_trust(&file, &root), Some(true));
    }

    #[test]
    fn an_absent_trust_file_is_created_with_only_that_one_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let root = parent.join("some-repo");
        touch_dir(&root);
        let file = claude_trust_file(home, "");
        assert!(!file.exists());

        assert_eq!(ensure_folder_trusted(home, "", &root, Some(&parent)), Ok(true));
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 1, "no key invented beyond projects");
        assert_eq!(v["projects"].as_object().unwrap().len(), 1);
        assert_eq!(recorded_trust(&file, &root), Some(true));
    }

    #[test]
    fn no_declared_prefix_writes_nothing_at_all() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let root = declared_parent(home).join("some-repo");
        touch_dir(&root);

        assert_eq!(ensure_folder_trusted(home, "", &root, None), Ok(false));
        assert!(
            !claude_trust_file(home, "").exists(),
            "with nothing declared the dialog is answered by hand -- today's behaviour"
        );
    }

    #[test]
    fn a_named_accounts_trust_file_is_the_one_inside_its_own_config_dir() {
        // The reauth case: another account's config dir has never trusted
        // this folder, which is why a resumed leg stops just like a first
        // spawn -- and why the same call covers both.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let root = parent.join("some-repo");
        touch_dir(&root);
        let dir = claude_config_dir(home, "second");
        touch_dir(&dir);

        assert_eq!(ensure_folder_trusted(home, "second", &root, Some(&parent)), Ok(true));
        assert_eq!(recorded_trust(&dir.join(".claude.json"), &root), Some(true));
        assert!(!home.join(".claude.json").exists(), "the default account's file is untouched");
    }

    #[test]
    fn the_default_accounts_trust_file_sits_beside_its_config_dir_not_inside_it() {
        // Checked against a real installation: with CLAUDE_CONFIG_DIR unset
        // the trust record is `<home>/.claude.json`. `<home>/.claude/.claude.json`
        // also exists there but carries no `projects` table at all, so
        // joining the config dir would write a file claude never reads.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert_eq!(claude_trust_file(home, ""), home.join(".claude.json"));
        assert_eq!(claude_trust_file(home, "default"), home.join(".claude.json"));
        assert_eq!(
            claude_trust_file(home, "second"),
            home.join(CLAUDE_ACCOUNTS_DIR).join("second").join(".claude.json")
        );
    }

    #[test]
    fn a_relative_declared_prefix_is_refused_and_trusts_nothing() {
        // A relative prefix cannot be reasoned about against an absolute
        // root: say so instead of silently trusting or silently skipping.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let root = declared_parent(home).join("some-repo");
        touch_dir(&root);

        let err = ensure_folder_trusted(home, "", &root, Some(Path::new("declared-parent"))).unwrap_err();
        assert!(err.contains("absolute"), "{err}");
        assert!(!claude_trust_file(home, "").exists());
    }

    #[test]
    fn an_unreadable_trust_file_is_refused_rather_than_clobbered() {
        // Claude Code owns this file. A shape we cannot parse is left alone:
        // the caller logs and starts the agent anyway, so the worst outcome
        // is the prompt appearing.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let root = parent.join("some-repo");
        touch_dir(&root);
        let file = claude_trust_file(home, "");
        let before = b"not json at all".to_vec();
        std::fs::write(&file, &before).unwrap();

        assert!(ensure_folder_trusted(home, "", &root, Some(&parent)).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), before, "byte-identical");
    }

    #[test]
    fn an_already_trusted_root_is_left_byte_identical() {
        // Every spawn calls this; a root claude already trusts must not mean
        // rewriting a file claude may be writing at the same moment.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let parent = declared_parent(home);
        let root = parent.join("some-repo");
        touch_dir(&root);
        let file = claude_trust_file(home, "");
        assert_eq!(ensure_folder_trusted(home, "", &root, Some(&parent)), Ok(true));
        let before = std::fs::read(&file).unwrap();

        assert_eq!(ensure_folder_trusted(home, "", &root, Some(&parent)), Ok(false));
        assert_eq!(std::fs::read(&file).unwrap(), before, "byte-identical");
    }

    /// A stand-in for what the installer writes into this box's own
    /// settings file -- built in a tempdir, so no real path from any
    /// machine appears in this repo.
    fn declare_in_settings(config_dir: &Path, prefix: &Path) {
        touch_dir(config_dir);
        std::fs::write(
            config_dir.join(SETTINGS_FILE),
            format!("[layout]\npreset = \"auto\"\n\n[trust]\nroot_prefix = \"{}\"\n", prefix.display()),
        )
        .unwrap();
    }

    #[test]
    fn the_declared_prefix_is_read_from_this_boxs_own_settings_file() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let parent = declared_parent(tmp.path());
        declare_in_settings(&config, &parent);
        assert_eq!(declared_root_prefix(&config), Some(parent));
    }

    #[test]
    fn an_absent_or_silent_settings_file_declares_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        assert_eq!(declared_root_prefix(&config), None, "no file -- nothing is trusted unless declared");
        touch_dir(&config);
        std::fs::write(config.join(SETTINGS_FILE), "[layout]\npreset = \"auto\"\n").unwrap();
        assert_eq!(declared_root_prefix(&config), None, "somebody else's keys declare nothing");
        std::fs::write(config.join(SETTINGS_FILE), "[trust]\nroot_prefix = \"\"\n").unwrap();
        assert_eq!(declared_root_prefix(&config), None, "an empty declaration is not a blanket one");
        std::fs::write(config.join(SETTINGS_FILE), "# root_prefix = \"/x\"\n").unwrap();
        assert_eq!(declared_root_prefix(&config), None, "a commented line is not a declaration");
    }

    #[test]
    fn the_same_key_outside_the_trust_table_is_not_a_declaration() {
        assert_eq!(
            parse_declared_root_prefix("[layout]\nroot_prefix = \"/somebody/elses/key\"\n"),
            None
        );
    }

    /// `install.sh` only writes its declaration when the file has no [trust]
    /// table yet, so its guard has to recognise every header shape THIS
    /// parser accepts -- one it missed would earn the file a second table,
    /// and the loop above takes the last declaration.
    #[test]
    fn a_spaced_trust_header_is_the_same_table() {
        assert_eq!(
            parse_declared_root_prefix("[ trust ]\nroot_prefix = \"/declared/here\"\n"),
            Some(PathBuf::from("/declared/here"))
        );
    }

    #[test]
    fn the_environment_declaration_overrides_the_installed_one() {
        let _g = TRUST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let parent = declared_parent(tmp.path());

        // Only the non-empty arm can assert on `trusted_root_prefix`: with
        // the variable unset it reads THIS box's real config, whose answer
        // is not the test's to know. `declared_root_prefix` above covers
        // the file side.
        std::env::set_var(TRUSTED_ROOT_PREFIX_ENV, &parent);
        assert_eq!(trusted_root_prefix(), Some(parent));
        std::env::remove_var(TRUSTED_ROOT_PREFIX_ENV);
    }
}
