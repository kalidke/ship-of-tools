//! A write beneath a root that follows no link: the root is opened once, each folder below it is opened relative to the
//! one before, and the file is created, renamed and synced relative to the last folder. No path is used after the root.
//! The arms are `beneath_unix.rs` (`openat`) and `beneath_windows.rs` (relative `NtCreateFile`).

use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
#[path = "beneath_unix.rs"]
mod arm;
#[cfg(windows)]
#[path = "beneath_windows.rs"]
mod arm;

use arm::write_beneath_at;

/// The points a test may stop at, each after the step it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Root,
    Folder(usize),
    /// A folder's open found nothing; the make follows.
    Missing(usize),
    Temp,
    Written,
    Renamed,
}

/// Writes `bytes` at `rel` beneath `root`, creating missing folders, durably, following no link. `root` is its own
/// canonical path and `rel` names joined by `/`; the result is `root` joined with `rel`, for replies and not for
/// IO. A root, name or folder the rule refuses is `InvalidInput`.
pub fn write_beneath(root: &Path, rel: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    write_beneath_at(root, rel, bytes, None, |_| Ok(()))
}

/// Names joined by `/`, each non-empty, not `.` or `..`, none holding a NUL; on Windows also none holding `\` or `:`
/// (a second folder, a drive or a stream).
fn plain_names(rel: &str) -> io::Result<Vec<&str>> {
    let bad = |n: &str| n.is_empty() || n == "." || n == ".." || n.contains('\0') || (cfg!(windows) && n.contains(['\\', ':']));
    rel.split('/').map(|n| if bad(n) { Err(invalid(format!("{n:?} is not one plain name"))) } else { Ok(n) }).collect()
}

/// The temporary file's name: `fixed` for a test, else `.sot-` and 16 random hex digits.
fn temp_name(fixed: Option<&str>) -> io::Result<String> {
    if let Some(t) = fixed {
        return Ok(t.to_owned());
    }
    let mut r = [0u8; 8];
    getrandom::fill(&mut r)?;
    Ok(format!(".sot-{}.tmp", r.iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

/// A relative root is never its own canonical path; refused before any call.
fn absolute_root(root: &Path) -> io::Result<()> {
    if root.is_absolute() { Ok(()) } else { Err(invalid(format!("{root:?} is not its own canonical path"))) }
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

#[cfg(test)]
#[path = "beneath_tests.rs"]
mod tests;
