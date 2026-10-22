//! The `ended` file: what the leg tells its supervisor about how it ended. The supervisor reads it where it once read
//! the voyage's last frames: whether an end was asked for, and how long the producer lived.
use super::*;

/// The file's name in the leg's folder.
const ENDED_FILE: &str = "ended";

/// How a leg ended. The leg writes it with `requested` set and nothing else the moment an end is asked for, and again
/// whole, `finished`, once the producer is gone.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ended {
    /// The leg has exited; false in the file written when an end is asked for, while the leg still runs.
    pub finished: bool,
    /// An end was asked for (a `shutdown`, or the caller's `Kill`); a supervisor never respawns over it.
    pub requested: bool,
    /// The producer's lifetime, once it has ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_ms: Option<u64>,
    /// The producer's exit code, or `None` for a signal death.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The output stream ended before teardown closed it and the producer's own exit explained it: the one case
    /// admitted, and admitted recorded (the kernel's revoke on a session leader's exit, macOS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ended_early: Option<String>,
}

pub(super) fn path(root: &std::path::Path) -> std::path::PathBuf {
    root.join(ENDED_FILE)
}

/// Writes the file durably, replacing an earlier one.
pub(super) fn write(root: &std::path::Path, ended: &Ended) -> Result<()> {
    let bytes = serde_json::to_vec(ended)?;
    sot_platform::host::write_durable(&path(root), &bytes)?;
    Ok(())
}

/// Records that an end was asked for, on the file the run left: an acknowledged `end_run` is a requested end even when the
/// agent ended on its own before the run was told, so a resume never starts another run over it.
pub fn mark_requested(root: &std::path::Path, reason: &str) -> Result<()> {
    let mut ended = read(root)?.unwrap_or(Ended { finished: true, ..Ended::default() });
    ended.requested = true;
    ended.reason.get_or_insert_with(|| reason.to_string());
    write(root, &ended)
}

/// Removes the file a former leg left, so one leg's ending is never read as the next's.
pub(super) fn clear(root: &std::path::Path) -> Result<()> {
    sot_platform::host::remove_durable(&path(root))?;
    Ok(())
}

/// What the last leg of the folder at `root` left; `None` when no leg has ended there (none ran, or one is running).
pub fn read(root: &std::path::Path) -> Result<Option<Ended>> {
    match std::fs::read(path(root)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[path = "ended_tests.rs"]
mod tests;
