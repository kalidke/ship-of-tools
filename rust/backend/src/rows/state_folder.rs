//! Removing a row's state folder: a close renames it to a tombstone and removes that; the start sweep removes tombstones and any folder no registry names (an older daemon's, a window close's, one a close kept).

use crate::rows::run::end_run::{absence_proof_then, leg_absent, NotProven};
use crate::rows::Workspaces;
use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// A folder no registry names is removed only when nothing in it was written for this long, so the sweep never races a
/// row being created (ADR 0042).
pub(crate) const AGE_FLOOR: Duration = Duration::from_secs(24 * 60 * 60);

/// What a tombstone is called: the closed row's folder name behind this prefix, in the same `workspaces` folder.
const TOMBSTONE_PREFIX: &str = ".closed-";

/// What one attempt did. `Kept` carries why, and `Gone` means there was nothing to remove.
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

/// Whether `name` is a tombstone: the prefix and a row folder name.
fn is_tombstone_name(name: &str) -> bool {
    name.strip_prefix(TOMBSTONE_PREFIX).is_some_and(is_row_folder_name)
}

/// Whether `path` is a directory itself, never one reached through a link.
fn real_dir(path: &Path) -> io::Result<bool> {
    Ok(std::fs::symlink_metadata(path)?.file_type().is_dir())
}

/// Check that `workspaces_dir/name` is a real directory below a real `workspaces` folder.
fn check_real_dirs(workspaces_dir: &Path, name: &str) -> Result<(), Removal> {
    match real_dir(workspaces_dir) {
        Ok(true) => {}
        Ok(false) => return Err(Removal::Kept("the workspaces folder is not a real directory")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Removal::Gone),
        Err(_) => return Err(Removal::Kept("the workspaces folder cannot be read")),
    }
    match real_dir(&workspaces_dir.join(name)) {
        Ok(true) => Ok(()),
        Ok(false) => Err(Removal::Kept("not a real directory")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(Removal::Gone),
        Err(_) => Err(Removal::Kept("cannot be read")),
    }
}

/// The leg half of the proof that nothing runs in a row folder: the codebase's `leg_absent`, and, for a folder whose row
/// never started (no pointer and no voyage in it), nothing to hold.
fn leg_absent_or_never_started(folder: &Path) -> Result<bool, String> {
    let no_pointer = matches!(
        sot_log::supervisor::journal::pointer::validate(folder),
        sot_log::supervisor::journal::pointer::PointerState::NotFound
    );
    let no_voyage = match std::fs::read_dir(folder.join("voyages")) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) => e.kind() == io::ErrorKind::NotFound,
    };
    if no_pointer && no_voyage {
        return Ok(true);
    }
    leg_absent(folder)
}

/// Retire the row folder `id`: after the same proof of absence a close uses (the supervisor's fence free and no leg
/// holding the voyage's `writer.lock`, `end_run::absence_proof_then`), rename it to its tombstone and return the
/// tombstone's name. Refused (kept) unless `id` is a row folder name, the `workspaces` folder and the folder are real
/// directories (a link at either is never followed), the folder holds no `row-scopes` record (the only record of scopes
/// that may still hold processes), `supervisor.lock` is not a link, and, with an `age_floor`, nothing in the folder was
/// written within it as of `now`. Every unreadable answer keeps the folder. On Unix the rename happens while the fence is
/// held; Windows cannot rename a folder with an open file in it, so it follows the release.
fn retire(workspaces_dir: &Path, id: &str, age_floor: Option<Duration>, now: SystemTime) -> Result<String, Removal> {
    if !is_row_folder_name(id) {
        return Err(Removal::Kept("not a name the registry makes"));
    }
    check_real_dirs(workspaces_dir, id)?;
    let folder = workspaces_dir.join(id);
    // Only Linux writes the record (a row's systemd scopes); no other OS has one to keep.
    #[cfg(target_os = "linux")]
    match std::fs::symlink_metadata(folder.join(crate::rows::spawn::row_scope::SCOPES_FILE)) {
        Ok(_) => return Err(Removal::Kept("a row-scopes record names scopes that may still hold processes")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(Removal::Kept("row-scopes cannot be read")),
    }
    let lock_path = sot_log::supervisor::journal::fence::supervisor_lock_path(&folder);
    match std::fs::symlink_metadata(&lock_path) {
        Ok(m) if !m.file_type().is_file() => return Err(Removal::Kept("supervisor.lock is not a regular file")),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(Removal::Kept("supervisor.lock cannot be read")),
    }
    if let Some(floor) = age_floor {
        match newest_write(&folder) {
            Ok(newest) => match now.duration_since(newest) {
                Ok(age) if age >= floor => {}
                _ => return Err(Removal::Kept("written within the age floor")),
            },
            Err(_) => return Err(Removal::Kept("its age cannot be read")),
        }
    }
    let tombstone = format!("{TOMBSTONE_PREFIX}{id}");
    let to = workspaces_dir.join(&tombstone);
    let renamed_under_fence = absence_proof_then(&folder, leg_absent_or_never_started, || {
        if cfg!(unix) {
            std::fs::rename(&folder, &to)
        } else {
            Ok(())
        }
    });
    let renamed = match renamed_under_fence {
        Ok(Some(done)) => done,
        Ok(None) => return Err(Removal::Kept("a leg holds the voyage's writer.lock")),
        Err(NotProven::FenceUnavailable) => return Err(Removal::Kept("supervisor.lock is held or cannot be checked")),
        Err(NotProven::LegCheckFailed(_)) => return Err(Removal::Kept("no leg check could be made")),
    };
    let renamed = if cfg!(windows) { std::fs::rename(&folder, &to) } else { renamed };
    match renamed {
        Ok(()) => Ok(tombstone),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(Removal::Gone),
        Err(e) => Err(Removal::Failed(format!("rename to {to:?}: {e}"))),
    }
}

/// Remove a tombstone: `remove_dir_all`, which unlinks a link inside and never follows it. A tombstone that is not a real
/// directory is kept.
fn finish_tombstone(workspaces_dir: &Path, tombstone: &str) -> Removal {
    if !is_tombstone_name(tombstone) {
        return Removal::Kept("not a name a close makes");
    }
    if let Err(removal) = check_real_dirs(workspaces_dir, tombstone) {
        return removal;
    }
    match std::fs::remove_dir_all(workspaces_dir.join(tombstone)) {
        Ok(()) => Removal::Removed,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Removal::Gone,
        Err(e) => Removal::Failed(e.to_string()),
    }
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

/// Log the end of removing `folder` (the row's folder or its tombstone); `why` says what led to it.
fn log_removal(folder: &Path, why: &str, removal: &Removal) {
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

/// A closed row's state folder, its run confirmed ended and the caller holding the row's guard: renamed to its tombstone,
/// which a detached thread then removes, so neither the reply nor the shutdown deadline waits on a large recording. A
/// folder kept (the lock still held, say) logs at info with the reason; the start sweep finds it later. Blocking, but
/// only for the proof and the rename.
pub(crate) fn close_row_folder(state_root: &Path, id: &str) {
    let workspaces_dir = state_root.join("workspaces");
    match retire(&workspaces_dir, id, None, SystemTime::now()) {
        Ok(tombstone) => {
            let spawned = std::thread::Builder::new().name("state-folder-remove".into()).spawn(move || {
                let removal = finish_tombstone(&workspaces_dir, &tombstone);
                log_removal(&workspaces_dir.join(&tombstone), "the row was closed", &removal);
            });
            if let Err(e) = spawned {
                tracing::warn!(error = %e, "could not start the thread that removes a closed row's state folder; the next daemon start removes it");
            }
        }
        Err(Removal::Kept(reason)) => {
            tracing::info!(folder = ?workspaces_dir.join(id), reason, "a closed row's state folder was kept; a later daemon start removes it when nothing runs in it");
        }
        Err(Removal::Failed(e)) => tracing::warn!(folder = ?workspaces_dir.join(id), error = %e, "a closed row's state folder could not be retired; a later daemon start removes it"),
        Err(Removal::Gone | Removal::Removed) => {}
    }
}

/// Start sweep, in two parts, returning the names it left. First every tombstone is removed, with no registry consulted:
/// a tombstone is a close's own leftover. Then a row folder no registration names (one an older daemon left, a window close's, or one a close kept)
/// is retired and removed when the same proof and the age floor allow. "No registration names it" is true only of a whole
/// load, so `named` is every id every registry folder names (`store::named_workspace_ids`) or the reason that could not be
/// said, and an `Err` skips the second part with one warn line. Blocking.
pub(crate) fn sweep_with(
    state_root: &Path,
    workspaces: &Workspaces,
    named: &Result<HashSet<String>, String>,
    now: SystemTime,
) -> Vec<String> {
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
    for name in names.iter().filter(|n| is_tombstone_name(n)) {
        let removal = finish_tombstone(&workspaces_dir, name);
        log_removal(&workspaces_dir.join(name), "a close left it", &removal);
        if matches!(removal, Removal::Kept(_) | Removal::Failed(_)) {
            kept.push(name.clone());
        }
    }
    let named = match named {
        Ok(named) => named,
        Err(why) => {
            tracing::warn!(reason = %why, "state folder sweep: the registries could not all be read, so no folder is removed as unnamed this start");
            return kept;
        }
    };
    for name in names.iter().filter(|n| is_row_folder_name(n)) {
        if named.contains(name) || workspaces.resolve(Some(name)).is_some() {
            continue;
        }
        let removal = match retire(&workspaces_dir, name, Some(AGE_FLOOR), now) {
            Ok(tombstone) => finish_tombstone(&workspaces_dir, &tombstone),
            Err(removal) => removal,
        };
        log_removal(&workspaces_dir.join(name), "no registry names it", &removal);
        if matches!(removal, Removal::Kept(_) | Removal::Failed(_)) {
            kept.push(name.clone());
        }
    }
    if !kept.is_empty() {
        tracing::warn!(
            count = kept.len(), ids = ?kept,
            "state folder sweep: folders kept (a lock or leg held, written within 24 hours, or not removable)"
        );
    }
    kept
}

/// The file in the `workspaces` folder naming the config folder whose registries last named its folders.
const CONFIG_RECORD: &str = ".registries-of";

/// [`sweep_with`] against the registries on disk, for the daemon's start-up resume scan. The state root outlasts a change
/// of config folder (another `XDG_CONFIG_HOME`, a moved home) and the registries do not follow it, so the config folder
/// each whole start read is recorded in [`CONFIG_RECORD`]; a start under a different one removes no unnamed folder (the
/// tombstones still go) and records nothing, so the old registries still decide until a start reads them whole again.
pub(crate) fn sweep_state_folders(state_root: &Path, workspaces: &Workspaces) {
    sweep_state_folders_at(state_root, workspaces, SystemTime::now());
}

/// [`sweep_state_folders`] as of `now`.
fn sweep_state_folders_at(state_root: &Path, workspaces: &Workspaces, now: SystemTime) {
    let record = state_root.join("workspaces").join(CONFIG_RECORD);
    let config = crate::rows::store::app_config_dir().to_string_lossy().into_owned();
    let mut named = crate::rows::store::named_workspace_ids();
    match std::fs::read_to_string(&record) {
        Ok(recorded) if recorded != config => {
            named = Err(format!("the registries that named these folders were in {recorded}, and this start reads {config}"));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => named = Err(format!("{record:?}: {e}")),
    }
    let whole = named.is_ok();
    sweep_with(state_root, workspaces, &named, now);
    if whole && real_dir(&state_root.join("workspaces")).unwrap_or(false) {
        if let Err(e) = sot_platform::host::write_durable(&record, config.as_bytes()) {
            tracing::warn!(error = %e, "state folder sweep: could not record the config folder whose registries were read");
        }
    }
}

#[cfg(test)]
#[path = "state_folder_tests.rs"]
mod tests;
