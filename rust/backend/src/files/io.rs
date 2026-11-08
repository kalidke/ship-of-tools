// files/io.rs — raw source-file read/write for the in-frontend editor.
//
// The kernel renders *previews*; this is the *editor's* path: it shuttles raw
// file bytes plus a content version, so a save can detect that the file changed
// underfoot (another writer, the file watcher, git, $EDITOR) since it was read.
// Pure backend IO — no Julia, no kernel. Mirrors the optimistic-concurrency
// shape of `concept.rs` (which guards on a `synced_against` AST hash), but for
// arbitrary source files the guard is a hash of the exact bytes.
//
// Version = FNV-1a 64-bit over the bytes, hex. Deterministic and dependency-
// free — and crucially *stable across backend rebuilds* (unlike
// `DefaultHasher`), so a version handed to the frontend before a restart still
// compares correctly after one.

use std::path::Path;

use anyhow::{anyhow, Context, Result};

use super::tree::FilesMode;

/// FNV-1a 64-bit hash of `bytes`, lowercase zero-padded hex.
pub fn content_version(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// A successful read: the file's text + its current version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadResult {
    pub content: String,
    pub version: String,
}

/// Outcome of a write attempt under optimistic concurrency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteResult {
    /// Written to disk. Carries the new content version.
    Written { version: String },
    /// Refused: the on-disk file no longer matches the `expected` version
    /// (it changed since it was read). Carries the current on-disk content +
    /// version so the caller can surface a reconcile/diff instead of clobbering.
    Conflict {
        current_content: String,
        current_version: String,
    },
}

/// Read a UTF-8 text file. `Ok(None)` when the file doesn't exist; `Err` on any
/// other IO error or on non-UTF-8 bytes (the editor is text-only).
pub fn read_file(abs: &Path) -> Result<Option<ReadResult>> {
    match std::fs::read(abs) {
        Ok(bytes) => {
            let version = content_version(&bytes);
            let content = String::from_utf8(bytes)
                .map_err(|_| anyhow!("file is not valid UTF-8: {}", abs.display()))?;
            Ok(Some(ReadResult { content, version }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow!("read {}: {e}", abs.display())),
    }
}

/// Write `content` at the node `node_id` with optimistic concurrency.
///
/// - `expected = Some(v)` — the current on-disk version (read through the read resolver, as `file.read`
///   does) must equal `v`, else `Conflict` and **no write**. A missing file counts as version `""`, so a
///   file that vanished after you read it is a conflict (not a silent recreate).
/// - `expected = None` — create/overwrite unconditionally (new file, or an
///   explicit force-save).
///
/// The write is `FilesMode::write`: beneath the project root, following no link, atomic, parent
/// folders created as needed. A name that is itself a link is refused (`InvalidInput`), before any version check.
pub fn write_file(
    files: &FilesMode,
    node_id: &str,
    content: &str,
    expected: Option<&str>,
) -> Result<WriteResult> {
    let abs = files.node_id_to_path(node_id)?;
    if std::fs::symlink_metadata(&abs).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("a save at a link is refused: {node_id}"),
        )
        .into());
    }
    if let Some(expected) = expected {
        let current = match std::fs::read(&abs) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(anyhow!("read {} (conflict check): {e}", abs.display())),
        };
        let current_version = current.as_deref().map(content_version).unwrap_or_default();
        if current_version != expected {
            let current_content = current
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            return Ok(WriteResult::Conflict {
                current_content,
                current_version,
            });
        }
    }
    files.write(node_id, content.as_bytes())?;
    Ok(WriteResult::Written {
        version: content_version(content.as_bytes()),
    })
}

/// Wait budget for the system-trash leader, not a ceiling on OS termination or reap.
const TRASH_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Move `abs` to trash — never hard-unlinks (the file.delete v1 contract:
/// every delete is recoverable). System trash first (`gio trash`); a missing
/// gio, a non-zero exit or a wait past [`TRASH_WAIT`] routes to the in-workspace
/// fallback, and the reason is logged. Only a confirmed zero exit returns `None`
/// (system trash); `Some(destination)` is the fallback — the caller
/// surfaces which path was taken (no quiet substitution). An unconfirmed trash
/// after which the file is gone is an error that says so: where the file went
/// is not confirmed.
pub fn trash_file(abs: &Path, workspace_root: &Path) -> Result<Option<std::path::PathBuf>> {
    trash_with_command(
        sot_containment::process(),
        &mut trash_command("gio", abs),
        abs,
        workspace_root,
        TRASH_WAIT,
        |_| {},
    )
}

/// The system-trash command: `program trash <abs>`. The daemon's own standard handles are not the program's, so all
/// three are null.
fn trash_command(program: impl AsRef<std::ffi::OsStr>, abs: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.arg("trash")
        .arg(abs)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// The absolute-command seam keeps fixtures independent of the process PATH. The observer runs before fallback;
/// production supplies a no-op, and tests inspect the contained owner's confirmed reap without a reusable PID probe.
fn trash_with_command(
    signal: &'static sot_containment::Signal,
    command: &mut std::process::Command,
    abs: &Path,
    workspace_root: &Path,
    wait_budget: std::time::Duration,
    observe: impl FnOnce(&sot_containment::ContainedStd),
) -> Result<Option<std::path::PathBuf>> {
    match signal.spawn_std(command) {
        Ok(mut child) => {
            let waited = child.wait_within(wait_budget);
            match &waited {
                Ok(Some(status)) if status.success() => {}
                Ok(Some(status)) => {
                    tracing::warn!(%status, "system trash exited unsuccessfully; taking recoverable fallback")
                }
                Ok(None) => {
                    tracing::warn!(?wait_budget, "system trash timed out; termination requests succeeded and direct child reaped; taking recoverable fallback")
                }
                Err(error) => {
                    tracing::warn!(%error, "system trash wait or cleanup failed; cleanup success unconfirmed; taking recoverable fallback")
                }
            }
            observe(&child);
            if waited.is_ok_and(|status| status.is_some_and(|status| status.success())) {
                return Ok(None);
            }
            // A trash that did not confirm may have moved the file before it was stopped, or another process removed it:
            // a file already gone is neither confirmed in the system trash nor left for the fallback to move.
            if std::fs::symlink_metadata(abs)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                tracing::warn!("system trash did not confirm, and the file is already gone: not reported as trashed");
                return Err(anyhow!(
                    "the system trash did not confirm, and {} is already gone: whether the system trash holds it is not known",
                    abs.display()
                ));
            }
        }
        Err(error) => tracing::warn!(%error, "system trash spawn failed; taking recoverable fallback"),
    }
    match trash_file_fallback(abs, workspace_root) {
        Ok(destination) => {
            tracing::info!(destination = %destination.display(), "recoverable trash fallback succeeded");
            Ok(Some(destination))
        }
        Err(error) => {
            tracing::error!(%error, "recoverable trash fallback failed");
            Err(error)
        }
    }
}

/// The no-gio path: move into `<workspace_root>/.sot-trash/<unix-ts>-<name>`,
/// suffixing `-N` on collision. Same filesystem as the source (both under the
/// workspace root), so this is a rename, not a copy.
fn trash_file_fallback(abs: &Path, workspace_root: &Path) -> Result<std::path::PathBuf> {
    let trash_dir = workspace_root.join(".sot-trash");
    std::fs::create_dir_all(&trash_dir)
        .with_context(|| format!("create_dir_all {}", trash_dir.display()))?;
    let name = abs
        .file_name()
        .ok_or_else(|| anyhow!("no file name in {}", abs.display()))?
        .to_string_lossy()
        .into_owned();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut dest = trash_dir.join(format!("{ts}-{name}"));
    let mut n = 1u32;
    while dest.exists() {
        dest = trash_dir.join(format!("{ts}-{n}-{name}"));
        n += 1;
    }
    std::fs::rename(abs, &dest)
        .with_context(|| format!("rename {} -> {}", abs.display(), dest.display()))?;
    Ok(dest)
}

#[cfg(test)]
#[path = "io_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "io_trash_tests.rs"]
mod trash_tests;
