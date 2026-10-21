//! The comm folder's layout and its making: `inbox/`, `read/`, `self/` and `registry.json`, each its user's alone.

use std::path::{Path, PathBuf};

/// The registry's schema version, stamped into a new registry.
pub(crate) const PROTOCOL_VERSION: u32 = 1;

/// The comm folder, absolute: [`super::sot_comm_home`] made absolute against the working folder, so a command and its
/// children never mean two folders.
pub(crate) fn comm_home() -> Option<PathBuf> {
    let h = super::sot_comm_home()?;
    Some(std::path::absolute(&h).unwrap_or(h))
}

/// Makes the comm folder whole when it is not: its folders (0700 under the process's 077 umask), what an older
/// release left open closed ([`tighten`]), and a registry only where there is no file. The registry is published by a
/// hard link of a synced skeleton, which fails on an existing name, so a registry is never replaced, truncated or
/// repaired here. Returns the lines to show (a folder that could not be made private).
pub(crate) fn ensure(home: &Path) -> Vec<String> {
    for d in ["", "inbox", "self", "read"] {
        let _ = std::fs::create_dir_all(home.join(d));
    }
    let notes = tighten(home);
    let reg = home.join("registry.json");
    if !reg.is_file() {
        use std::io::Write;
        let tmp = home.join(format!("registry.json.new.{}.{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos())));
        let made = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).and_then(|mut f| {
            f.write_all(format!("{{\"protocol_version\": {PROTOCOL_VERSION}, \"agents\": {{}}}}\n").as_bytes())?;
            f.sync_all()
        });
        if made.is_ok() {
            let _ = std::fs::hard_link(&tmp, &reg);
        }
        let _ = std::fs::remove_file(&tmp);
    }
    notes
}

/// Whether the folder is whole already: a command that finds it so writes nothing to make it.
pub(crate) fn is_whole(home: &Path) -> bool {
    home.join("registry.json").is_file() && ["inbox", "self", "read"].iter().all(|d| home.join(d).is_dir())
}

/// Closes to group and other what an older release left open in the comm folder (0031, isolation): the folder, its
/// layout folders and the layout's own files, and nothing else; never a link's target. A folder that is the root, the
/// home folder or a git checkout (a mistaken `SOT_COMM_HOME`) is left as it is, and said so. Not on Windows, where the
/// profile's access list is the mechanism.
#[cfg(unix)]
pub(crate) fn tighten(home: &Path) -> Vec<String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Ok(phys) = std::fs::canonicalize(home) else { return Vec::new() };
    let user_home = std::env::var_os("HOME").and_then(|h| std::fs::canonicalize(h).ok());
    let refused = if phys == Path::new("/") {
        Some("the root folder")
    } else if user_home.as_deref() == Some(phys.as_path()) {
        Some("the home folder")
    } else if phys.join(".git").exists() {
        Some("a git checkout")
    } else {
        None
    };
    if let Some(what) = refused {
        return vec![format!("WARNING: the comm folder {} is {what}, so its permissions were not changed", home.display())];
    }
    let open = |p: &Path| std::fs::symlink_metadata(p).ok().filter(|m| !m.file_type().is_symlink() && m.mode() & 0o077 != 0);
    // Only while the folder itself is open: once it is 0700 nothing new is reached through it.
    if open(home).is_none() {
        return Vec::new();
    }
    let close = |p: &Path| {
        if let Some(m) = open(p) {
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(m.mode() & 0o7700));
        }
    };
    let files_in = |dir: &str, keep: &dyn Fn(&str) -> bool| -> Vec<PathBuf> {
        let Ok(rd) = std::fs::read_dir(home.join(dir)) else { return Vec::new() };
        rd.flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && keep(&e.file_name().to_string_lossy()))
            .map(|e| e.path())
            .collect()
    };
    let top = |n: &str| {
        matches!(n, "inbox-lock-manager" | "gh-device-auth.json" | "registry.json" | "registry.json.tmp" | ".registry.lock")
            || [".inbox-lock-manager.", "registry.json.new.", ".registry.lock.tmp.", ".registry.lock.reclaim."].iter().any(|p| n.starts_with(p))
    };
    let mut entries: Vec<PathBuf> = Vec::new();
    for d in ["inbox", "read", "self", "state", "probe"] {
        if std::fs::symlink_metadata(home.join(d)).is_ok_and(|m| m.is_dir()) {
            entries.push(home.join(d));
        }
    }
    if let Ok(rd) = std::fs::read_dir(home.join("probe")) {
        entries.extend(rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()));
    }
    entries.extend(files_in("", &top));
    entries.extend(files_in("inbox", &|n| n.ends_with(".jsonl") || n.ends_with(".lock")));
    entries.extend(files_in("read", &|n| n.ends_with(".cursor")));
    entries.extend(files_in("self", &|n| n.ends_with(".txt")));
    entries.extend(files_in("state", &|_| true));
    // The folder itself last: a pass cut short leaves it open, and the next one walks again.
    entries.push(home.to_path_buf());
    for p in &entries {
        close(p);
    }
    entries
        .iter()
        .find(|p| open(p).is_some())
        .map(|p| vec![format!("WARNING: the comm folder {} could not be made private: {} is still open", home.display(), p.display())])
        .unwrap_or_default()
}

#[cfg(not(unix))]
pub(crate) fn tighten(_home: &Path) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
#[path = "home_tests.rs"]
mod tests;
