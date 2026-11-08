// folder_trust.rs — the folder-trust record the daemon pre-answers for a declared root prefix.

use std::path::{Path, PathBuf};

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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TrustOutcome {
    Recorded,
    AlreadyTrusted,
    Outside,
    NotDeclared,
}

/// Serializes this daemon's preparations: two rows starting at once would each read the file and one's entry would
/// replace the other's. Another process's edit (claude's own) is not covered, as the writer's limit says.
static PREPARING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Where a root lies against the declared prefix.
enum Scope {
    Undeclared,
    Outside,
    Inside,
}

/// The record's scope test, the one place it is made: a declared prefix, absolute, and the OS-resolved root under the
/// OS-resolved prefix. [`ensure_folder_trusted`] and [`under_declared_prefix`] both ask it.
fn scope_of(root: &Path, prefix: Option<&Path>) -> Result<Scope, String> {
    let Some(prefix) = prefix else {
        return Ok(Scope::Undeclared);
    };
    if !prefix.is_absolute() {
        return Err(format!(
            "declared trusted-folder prefix {prefix:?} ([trust] root_prefix) is not an absolute path: nothing is trusted"
        ));
    }
    if !root.is_absolute() {
        return Err(format!("project root {root:?} is not an absolute path: nothing is trusted"));
    }
    let resolved_prefix = sot_platform::host::paths::simplify_verbatim(
        prefix
            .canonicalize()
            .map_err(|e| format!("declared prefix {prefix:?}: {e}"))?,
    );
    let resolved_root = sot_platform::host::paths::simplify_verbatim(
        root.canonicalize()
            .map_err(|e| format!("project root {root:?}: {e}"))?,
    );
    if !resolved_root.starts_with(&resolved_prefix) {
        return Ok(Scope::Outside);
    }
    Ok(Scope::Inside)
}

/// Whether the declaration (read now) names a prefix the root lies under: the question the record answers, asked by
/// the trust restart so that it never restarts a root the record would not have written. No declaration is `false`;
/// a relative prefix, a root that is not absolute, or a path that cannot be resolved is an error.
pub fn under_declared_prefix(root: &Path) -> Result<bool, String> {
    Ok(matches!(scope_of(root, trusted_root_prefix()?.as_deref())?, Scope::Inside))
}

/// The instant of the last Claude preparation, per trust file.
static LAST_CLAUDE_PREPARED: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<PathBuf, std::time::Instant>>> =
    std::sync::LazyLock::new(Default::default);

/// Note that a Claude leg is being prepared against `trust_file` now. Memory only.
pub(crate) fn mark_claude_prepared(trust_file: &Path) {
    let mut map = LAST_CLAUDE_PREPARED.lock().unwrap_or_else(|e| e.into_inner());
    map.insert(trust_file.to_path_buf(), std::time::Instant::now());
}

/// Whether a Claude leg on `trust_file` was prepared less than `within` ago. The trust restart waits while it is
/// true: Claude Code's own start saves this file, and the restart's record is the same unlocked write.
pub fn claude_prepared_within(trust_file: &Path, within: std::time::Duration) -> bool {
    let map = LAST_CLAUDE_PREPARED.lock().unwrap_or_else(|e| e.into_inner());
    map.get(trust_file).is_some_and(|at| at.elapsed() < within)
}

/// Compare OS-resolved scope while recording the child cwd's spelling.
/// Existing accepted bytes and unrelated JSON keys are preserved.
/// Concurrent external edits remain the existing writer's scoped limit.
pub fn ensure_folder_trusted(
    path: &Path,
    root: &Path,
    prefix: Option<&Path>,
) -> Result<TrustOutcome, String> {
    match scope_of(root, prefix)? {
        Scope::Undeclared => return Ok(TrustOutcome::NotDeclared),
        Scope::Outside => return Ok(TrustOutcome::Outside),
        Scope::Inside => (),
    }

    let _one_at_a_time = PREPARING.lock().unwrap_or_else(|e| e.into_inner());
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
    publish_trust_file(path, &bytes).map_err(|e| at(&e))?;
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

/// A temporary name no other preparation of this process or another shares: the process id and a per-process counter.
fn trust_temp_name(name: &str) -> String {
    static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!("{name}.sot-{}-{}.tmp", std::process::id(), CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
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
    let tmp = dir.join(trust_temp_name(name));
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
