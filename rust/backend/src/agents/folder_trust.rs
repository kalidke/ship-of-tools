// folder_trust.rs — the folder-trust record the daemon pre-answers for a declared root prefix.

use std::path::{Path, PathBuf};

use super::accounts::claude_config_dir;

/// Claude Code's own per-folder trust record, a JSON object keyed by
/// absolute project path under `projects`, each entry carrying
/// `hasTrustDialogAccepted` (see [`ensure_folder_trusted`]).
const CLAUDE_TRUST_FILE: &str = ".claude.json";
/// This box's own settings file, in the daemon's config directory --
/// where the installed declaration lives (see below).
#[cfg(test)]
const SETTINGS_FILE: &str = "settings.toml";
/// The bool inside a `projects` entry that means "this folder's trust
/// dialog is answered" -- claude's own key name, not ours.
const TRUST_ACCEPTED_KEY: &str = "hasTrustDialogAccepted";
/// Read the typed user-level declaration at each spawn.
pub fn trusted_root_prefix() -> Result<Option<PathBuf>, String> {
    super::trust_declaration::read_trust_declaration(&super::trust_declaration::declaration_file())
}

#[cfg(test)]
fn declared_root_prefix(config_dir: &Path) -> Option<PathBuf> {
    super::trust_declaration::read_trust_declaration(&config_dir.join(SETTINGS_FILE)).unwrap()
}
#[cfg(test)]
fn parse_declared_root_prefix(text: &str) -> Option<PathBuf> {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join(SETTINGS_FILE);
    std::fs::write(&path, text).unwrap();
    super::trust_declaration::read_trust_declaration(&path).unwrap()
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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TrustOutcome {
    Recorded,
    AlreadyTrusted,
    Outside,
    NotDeclared,
}

/// Compare OS-resolved scope while recording the child cwd's spelling.
/// Existing accepted bytes and unrelated JSON keys are preserved.
/// Concurrent external edits remain the existing writer's scoped limit.
pub fn ensure_folder_trusted(
    home: &Path,
    account: &str,
    root: &Path,
    prefix: Option<&Path>,
) -> Result<TrustOutcome, String> {
    let Some(prefix) = prefix else {
        return Ok(TrustOutcome::NotDeclared);
    };
    if !prefix.is_absolute() {
        return Err(format!(
            "declared trusted-folder prefix {prefix:?} ([trust] root_prefix) is not an absolute path: nothing is trusted"
        ));
    }
    if !root.is_absolute() {
        return Err(format!("project root {root:?} is not an absolute path: nothing is trusted"));
    }
    if !root.starts_with(prefix) {
        return Ok(TrustOutcome::Outside);
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
        .entry(claude_project_key(&root.to_string_lossy(), cfg!(windows)))
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| at(&"this project's own entry is not a JSON object"))?;
    // Already answered: leave the file exactly as it is rather than
    // rewriting it on every spawn -- claude may be writing it right now.
    if entry.get(TRUST_ACCEPTED_KEY).and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(TrustOutcome::AlreadyTrusted);
    }
    entry.insert(TRUST_ACCEPTED_KEY.to_string(), serde_json::Value::Bool(true));

    let bytes = serde_json::to_vec_pretty(&doc).map_err(|e| at(&e))?;
    publish_trust_file(&path, &bytes).map_err(|e| at(&e))?;
    Ok(TrustOutcome::Recorded)
}

/// The key Claude Code files a project under in `.claude.json`. On Windows
/// its own keys read `C:/Users/...` (forward slashes), while a root may
/// arrive with `\`; spelled differently, the entry would never be found
/// and the dialog would still appear. Elsewhere the root is the key as is.
fn claude_project_key(root: &str, windows: bool) -> String {
    if windows {
        root.replace('\\', "/")
    } else {
        root.to_string()
    }
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
#[path = "folder_trust_tests.rs"]
mod tests;
