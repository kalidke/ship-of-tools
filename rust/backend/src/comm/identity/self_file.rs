//! The self file: where a session keeps its handle between commands. Three lines, `<handle>`, `repo=<repo>`,
//! `root=<canonical root>`; a row's is `self/<host>__<row id>.txt`, pinned by the daemon as `SOT_COMM_SELF_FILE`, and a
//! session outside any row has `self/<host>__nopane.txt`, which every such session on the box shares.

use std::path::{Path, PathBuf};

use super::paths::{self, same_root};

/// The self-file leaf for `host` and row `workspace_id`: `<host>__<id>.txt`, the id's bytes outside `A-Za-z0-9._-`
/// made `_`, `nopane` for no row. `Err` when the leaf is not one file name this file system can hold as written (the
/// host is declared text and is never rewritten to fit).
pub(crate) fn slot_leaf(host: &str, workspace_id: &str) -> Result<String, String> {
    if host.is_empty() {
        return Err("comm: no declared host to name a self slot".into());
    }
    let ws: String = workspace_id
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-') { b as char } else { '_' })
        .collect();
    let leaf = format!("{host}__{}.txt", if ws.is_empty() { "nopane" } else { &ws });
    if !leaf_valid(&leaf, cfg!(windows)) {
        return Err("comm: the declared host cannot name a self slot on this filesystem".into());
    }
    Ok(leaf)
}

/// Whether `leaf` is one file name as written: no separator or control character, at most 255 bytes, and on
/// Windows none of `<>:"|?*`, no trailing dot or space, no device stem and at most 255 UTF-16 units.
pub(crate) fn leaf_valid(leaf: &str, windows: bool) -> bool {
    if leaf.is_empty() || leaf.len() > 255 || leaf.chars().any(|c| c == '/' || c == '\\' || c.is_control()) {
        return false;
    }
    if windows {
        if leaf.chars().any(|c| "<>:\"|?*".contains(c)) || leaf.ends_with('.') || leaf.ends_with(' ') {
            return false;
        }
        let stem = leaf.split('.').next().unwrap_or("").to_ascii_uppercase();
        let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && matches!(stem.as_bytes()[3], b'1'..=b'9'));
        if device || leaf.encode_utf16().count() > 255 {
            return false;
        }
    }
    true
}

/// Whether `path` is the slot every session outside a row on the box shares.
pub(crate) fn is_nopane(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n.to_string_lossy().ends_with("__nopane.txt"))
}

/// The row a self file's name says it is a row's: `<host>__<id>.txt`, cut after this box's declared `host` when the
/// name holds more than one `__`, else at the first; `None` for `nopane` or any other name.
pub(crate) fn row_of(path: &Path, host: &str) -> Option<String> {
    let base = path.file_name()?.to_string_lossy().into_owned();
    let stem = base.strip_suffix(".txt")?;
    let (_, mut id) = stem.split_once("__")?;
    if id.contains("__") && !host.is_empty() {
        if let Some(rest) = stem.strip_prefix(&format!("{host}__")) {
            id = rest;
        }
    }
    (!id.is_empty() && id != "nopane").then(|| id.to_string())
}

/// What a self file says, read in one open.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SelfFile {
    pub name: String,
    pub repo: Option<String>,
    /// The `root=` line; `None` when there is no third line.
    pub root: Option<String>,
    /// A third line that is not `root=...`.
    pub malformed: bool,
}

/// `path`'s three lines, or `None` when it cannot be read.
pub(crate) fn read(path: &Path) -> Option<SelfFile> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let name = lines.next().unwrap_or("").to_string();
    let repo = lines.next().and_then(|l| l.strip_prefix("repo=")).map(str::to_string);
    let third = lines.next();
    let root = third.and_then(|l| l.strip_prefix("root=")).map(str::to_string);
    Some(SelfFile { name, repo, malformed: third.is_some() && root.is_none(), root })
}

/// The handle `path` gives a session in `project_root`, or `""`: only a file whose `root=` line names that root. Every
/// other file names nobody, and the line pushed to `notes` says why (a file older than 0.6.6's three-line form, a root
/// that is another project's, a broken line); the next join derives the handle again.
pub(crate) fn name_for(path: &Path, project_root: &str, notes: &mut Vec<String>) -> String {
    let Some(f) = read(path) else { return String::new() };
    if f.name.is_empty() {
        return String::new();
    }
    let n = &f.name;
    match (&f.root, f.malformed) {
        (Some(r), _) if !r.is_empty() && same_root(r, project_root) => n.clone(),
        (Some(r), _) => {
            let why = if r.is_empty() { "an empty/malformed root=".to_string() } else { format!("root='{r}'") };
            notes.push(format!(
                "comm-context: self-file identity '{n}' has {why} which doesn't match this project ('{project_root}') \u{2014} stale; discarding (forces fresh derivation)"
            ));
            String::new()
        }
        (None, true) => {
            notes.push(format!("comm-context: self-file identity '{n}' has a malformed third line (not a root=... line) \u{2014} stale; discarding"));
            String::new()
        }
        (None, false) => {
            notes.push(format!(
                "comm-context: self-file identity '{n}' has no root= line (a form older than 0.6.6) \u{2014} discarding; the next join derives the handle again"
            ));
            String::new()
        }
    }
}

/// The line naming the identity `path` already holds for another project than (`repo`, `root`), or `None`: the slot is
/// keyed by the row in the environment while the identity comes from the folder, so a slot must not come to name
/// another project's session. `root=` decides; a file without one decides by `repo=`; the shared `nopane` slot is
/// last-writer-wins and never conflicts.
pub(crate) fn conflict(path: &Path, repo: &str, root: &str) -> Option<String> {
    if is_nopane(path) {
        return None;
    }
    let f = read(path)?;
    if f.name.is_empty() {
        return None;
    }
    let claim = match (&f.root, &f.repo) {
        (Some(r), _) if !r.is_empty() => (!same_root(r, root)).then(|| format!("root='{r}'"))?,
        (_, Some(p)) if !p.is_empty() => (p != repo).then(|| format!("repo='{p}' (legacy slot, no root=)"))?,
        _ => return None,
    };
    Some(format!("the identity slot '{}' already names @{}, claimed for {claim}", path.display(), f.name))
}

/// Replaces `path` with the three lines for `name`, through a temp file beside it (its user's alone, whatever the
/// process's umask) and a rename, so a reader never sees half a file. A join holds the registry lock, which its other
/// writes take too. `Err` says why, and `path` is as it was.
pub(crate) fn write(path: &Path, name: &str, repo: &str, root: &str) -> Result<(), String> {
    use std::io::Write;
    let tmp = PathBuf::from(format!("{}.tmp.{}", path.display(), std::process::id()));
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    let written = open
        .open(&tmp)
        .and_then(|mut f| f.write_all(format!("{name}\nrepo={repo}\nroot={root}\n").as_bytes()).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path));
    written.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not write the self file '{}': {e}", path.display())
    })
}

/// A row's start: when the row was created with a handle and its pinned self file `path` names nothing yet, the file
/// names `handle` for the project at `cwd`, so the session's first join takes the handle the row was created with and
/// no command chooses one (decision 0031). A file already there is the row's own record, kept as it is.
pub(crate) fn pin_row(path: &Path, handle: &str, cwd: &Path) -> Result<(), String> {
    if handle.is_empty() || path.exists() {
        return Ok(());
    }
    if !crate::comm::identity::derive::valid_handle(handle) {
        return Err(format!("the row's handle '{handle}' is not a handle (1 to 64 of A-Z a-z 0-9 . _ -)"));
    }
    let raw = paths::raw_root(cwd);
    let root = paths::canonical(&raw)?;
    if let Some(home) = path.parent().and_then(Path::parent) {
        if !crate::comm::home::is_whole(home) {
            crate::comm::home::ensure(home, &crate::comm::Env::process());
        }
    }
    write(path, handle, &paths::basename(&raw), &root)
}

#[cfg(test)]
#[path = "self_file_tests.rs"]
mod tests;
