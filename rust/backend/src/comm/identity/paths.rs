//! A project's root, and paths in one spelling: on Windows one folder has several (`/c/Users/...` from Git Bash,
//! `C:/Users/...` from git, `C:\Users\...` and `\\?\C:\...` from Windows), and every root the registry and the self
//! files hold is compared through [`same_root`].

use std::path::Path;

/// `path` as this box writes a root: as written off Windows; on Windows with `\` read as `/`, a verbatim `\\?\` prefix
/// dropped, and Git Bash's `/<drive>/...` written `<DRIVE>:/...`.
pub(crate) fn native(path: &str) -> String {
    if !cfg!(windows) {
        return path.to_string();
    }
    windows_native(path)
}

/// [`native`]'s Windows rule, on every OS so its tests run everywhere.
pub(crate) fn windows_native(path: &str) -> String {
    let p = path.replace('\\', "/");
    let p = p.strip_prefix("//?/").unwrap_or(&p);
    let b = p.as_bytes();
    if b.len() >= 2 && b[0] == b'/' && b[1].is_ascii_alphabetic() && (b.len() == 2 || b[2] == b'/') {
        let rest = if b.len() == 2 { "/" } else { &p[2..] };
        return format!("{}:{rest}", (b[1] as char).to_ascii_uppercase());
    }
    p.to_string()
}

/// Whether `a` and `b` name one folder by their spelling: equal, or on Windows equal once both are [`native`] and in
/// one case (a Windows folder answers to its name in any case). Nothing is resolved on disk.
pub(crate) fn same_root(a: &str, b: &str) -> bool {
    a == b || (cfg!(windows) && windows_same_root(a, b))
}

/// [`same_root`]'s Windows rule, on every OS so its tests run everywhere.
pub(crate) fn windows_same_root(a: &str, b: &str) -> bool {
    let key = |p: &str| windows_native(p).trim_end_matches('/').to_ascii_lowercase();
    key(a) == key(b)
}

/// The folder a command run in `cwd` belongs to: git's top level when `cwd` is in a checkout, else `cwd` itself, in
/// this box's spelling ([`native`]).
pub(crate) fn raw_root(cwd: &Path) -> String {
    let mut git = std::process::Command::new("git");
    git.args(["rev-parse", "--show-toplevel"]).current_dir(cwd);
    let top = crate::lifecycle::child_signal::process()
        .output(&mut git, std::time::Duration::from_secs(10))
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim_end_matches(['\n', '\r']).to_string())
        .filter(|s| !s.is_empty());
    native(&top.unwrap_or_else(|| cwd.to_string_lossy().into_owned()))
}

/// `path` absolute with every link resolved, in this box's spelling: the canonical root the derivation and every root
/// comparison use. `Err` names the path when it cannot be resolved; a relative or unresolved path is never a root.
pub(crate) fn canonical(path: &str) -> Result<String, String> {
    std::fs::canonicalize(path)
        .map(|p| native(&p.to_string_lossy()))
        .map_err(|e| format!("could not resolve a canonical path for '{path}' ({e}); refusing to record an unresolved project root"))
}

/// The last component of `root` (`basename`): what a derived handle and the registry's `repo` are built from.
pub(crate) fn basename(root: &str) -> String {
    let t = root.trim_end_matches(['/', '\\']);
    t.rsplit(['/', '\\']).next().unwrap_or(t).to_string()
}

/// The folder above `root` (`dirname`), or `.` when it has none.
pub(crate) fn dirname(root: &str) -> String {
    let t = root.trim_end_matches(['/', '\\']);
    match t.rfind(['/', '\\']) {
        Some(0) => "/".to_string(),
        Some(i) => t[..i].to_string(),
        None => ".".to_string(),
    }
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod tests;
