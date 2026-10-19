//! Removing a row's state folder: when the row is closed and its run is confirmed ended, and at daemon start for a folder no row names.

use crate::rows::Workspaces;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// A folder no row names is removed only when nothing in it was written for this long, so the sweep never races a
/// row being created (ADR 0042).
pub(crate) const AGE_FLOOR: Duration = Duration::from_secs(24 * 60 * 60);

/// What one removal attempt did. `Kept` carries why, and `Gone` means there was nothing to remove.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Removal {
    Removed,
    Gone,
    Kept(&'static str),
    Failed(String),
}

/// Whether `name` is one the registry could have made: `ws-` and a slug (lowercase letters, digits, `_`, `-`) and a hex
/// suffix (`Workspace::from_label`). Anything else, dots and separators included, is never touched.
pub(crate) fn is_row_folder_name(name: &str) -> bool {
    name.len() <= 200
        && name.starts_with("ws-")
        && name.bytes().all(|b| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'_' || b == b'-')
}

/// Remove `<state-root>/workspaces/<id>`. Refused (kept) unless `id` is a row folder name, the `workspaces` folder and
/// the folder are real directories (a link at either is never followed), the folder's `supervisor.lock` is not a link and
/// can be taken (so no supervisor holds it), and, with an `age_floor`, nothing in the folder was written within it as of
/// `now`. Every unreadable answer keeps the folder. The tree is removed by `remove_dir_all`, which unlinks a link inside
/// and never follows it.
pub(crate) fn remove_state_folder(workspaces_dir: &Path, id: &str, age_floor: Option<Duration>, now: SystemTime) -> Removal {
    if !is_row_folder_name(id) {
        return Removal::Kept("not a name the registry makes");
    }
    match real_dir(workspaces_dir) {
        Ok(true) => {}
        Ok(false) => return Removal::Kept("the workspaces folder is not a real directory"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Removal::Gone,
        Err(_) => return Removal::Kept("the workspaces folder cannot be read"),
    }
    let folder = workspaces_dir.join(id);
    match real_dir(&folder) {
        Ok(true) => {}
        Ok(false) => return Removal::Kept("not a real directory"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Removal::Gone,
        Err(_) => return Removal::Kept("cannot be read"),
    }
    let lock_path = sot_log::supervisor::journal::fence::supervisor_lock_path(&folder);
    match std::fs::symlink_metadata(&lock_path) {
        Ok(m) if !m.file_type().is_file() => return Removal::Kept("supervisor.lock is not a regular file"),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Removal::Kept("supervisor.lock cannot be read"),
    }
    if let Some(floor) = age_floor {
        match newest_write(&folder) {
            Ok(newest) => match now.duration_since(newest) {
                Ok(age) if age >= floor => {}
                _ => return Removal::Kept("written within the age floor"),
            },
            Err(_) => return Removal::Kept("its age cannot be read"),
        }
    }
    // The supervisor's own fence: taken here, so a held or unreadable lock keeps the folder. Released before the removal
    // (Windows cannot delete an open file); a supervisor born in between would have to race the age floor or the row's guard.
    match sot_log::supervisor::journal::fence::lock_supervisor(&folder) {
        Ok(fence) => drop(fence),
        Err(_) => return Removal::Kept("supervisor.lock is held or cannot be checked"),
    }
    match std::fs::remove_dir_all(&folder) {
        Ok(()) => Removal::Removed,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Removal::Gone,
        Err(e) => Removal::Failed(e.to_string()),
    }
}

/// Whether `path` is a directory itself, never one reached through a link.
fn real_dir(path: &Path) -> io::Result<bool> {
    Ok(std::fs::symlink_metadata(path)?.file_type().is_dir())
}

/// The newest modification time of `root` and everything under it, reading each entry's own metadata and never
/// following a link.
fn newest_write(root: &Path) -> io::Result<SystemTime> {
    let mut newest = std::fs::symlink_metadata(root)?.modified()?;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let meta = std::fs::symlink_metadata(entry.path())?;
            newest = newest.max(meta.modified()?);
            if meta.file_type().is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(newest)
}

/// Log the outcome of removing `id`'s folder; `why` says what led to it. A removal logs one line naming the folder, what
/// goes and that the conversation stays resumable.
fn log_removal(workspaces_dir: &Path, id: &str, why: &str, removal: &Removal) {
    let folder = workspaces_dir.join(id);
    match removal {
        Removal::Removed => tracing::info!(
            folder = ?folder, why,
            "removed a row's state folder: its terminal recording is gone; the conversation stays resumable with /resume in the same project folder"
        ),
        Removal::Failed(e) => tracing::warn!(folder = ?folder, error = %e, why, "removing a row's state folder failed part way; the next daemon start tries again"),
        Removal::Kept(reason) => tracing::debug!(folder = ?folder, reason, why, "kept a row's state folder"),
        Removal::Gone => {}
    }
}

/// The state folder of a row just closed with its run confirmed ended, removed under the caller's hold of the row's guard.
/// Blocking.
pub(crate) fn remove_closed_row_folder(state_root: &Path, id: &str) -> Removal {
    let workspaces_dir = state_root.join("workspaces");
    let removal = remove_state_folder(&workspaces_dir, id, None, SystemTime::now());
    log_removal(&workspaces_dir, id, "the row was closed", &removal);
    removal
}

/// Boot sweep: remove every `<state-root>/workspaces/*` folder no registered row names that [`remove_state_folder`]
/// allows, and return the names of the folders it left. Reactive: it runs once, in the daemon's start-up resume scan, the
/// one moment the registry is whole; a folder left by a crash between a close's steps, or by an older daemon, is found
/// there, and a close removes its own folder at once. Blocking.
pub(crate) fn sweep_unnamed_folders(state_root: &Path, workspaces: &Workspaces, now: SystemTime) -> Vec<String> {
    let workspaces_dir = state_root.join("workspaces");
    let entries = match std::fs::read_dir(&workspaces_dir) {
        Ok(e) => e,
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                tracing::debug!(dir = ?workspaces_dir, error = %e, "state folder sweep: could not read the workspaces folder");
            }
            return Vec::new();
        }
    };
    let names: Vec<String> = entries.flatten().filter_map(|e| e.file_name().to_str().map(str::to_string)).collect();
    let mut kept = Vec::new();
    for name in names {
        if workspaces.resolve(Some(&name)).is_some() {
            continue;
        }
        let removal = remove_state_folder(&workspaces_dir, &name, Some(AGE_FLOOR), now);
        log_removal(&workspaces_dir, &name, "no row names it", &removal);
        if matches!(removal, Removal::Kept(_) | Removal::Failed(_)) {
            kept.push(name);
        }
    }
    kept
}

#[cfg(test)]
#[path = "prune_tests.rs"]
mod tests;
