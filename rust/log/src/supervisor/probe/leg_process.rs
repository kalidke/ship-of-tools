//! What the authority keeps of a live leg: the child it spawned, or a proven identity.

use crate::capsule::producer::ExitStatus;
use std::time::Duration;

#[cfg(target_os = "macos")]
use crate::identity::challenge_macos::ChallengedProcess;
#[cfg(target_os = "linux")]
use crate::identity::challenge_unix::ChallengedProcess;
#[cfg(windows)]
use crate::identity::challenge_win::ChallengedProcess;
#[cfg(target_os = "macos")]
use crate::supervisor::probe::macos::SpawnedChild;
#[cfg(target_os = "linux")]
use crate::supervisor::probe::unix::SpawnedChild;
#[cfg(windows)]
use crate::supervisor::probe::win::SpawnedChild;

/// A live leg as the authority holds it. `Owned` is the child this
/// supervisor spawned, kept from spawn to its one reap, so its exit status
/// is read from the child itself. `Adopted` is a leg proven by a challenge
/// (an earlier supervisor's, or one found at startup): its identity is
/// proven, its exit status is not ours to read.
pub enum LegProcess {
    Owned(SpawnedChild),
    Adopted(ChallengedProcess),
}

impl LegProcess {
    /// Whether the leg has exited, within `timeout`. An owned leg is reaped
    /// by the wait that sees it exit and its status kept; an adopted leg is
    /// reaped only by [`Self::reap`].
    pub fn wait(&self, timeout: Duration) -> std::io::Result<bool> {
        match self {
            LegProcess::Owned(child) => child.wait(timeout),
            LegProcess::Adopted(process) => process.wait(timeout),
        }
    }

    pub fn terminate(&self) -> std::io::Result<()> {
        match self {
            LegProcess::Owned(child) => child.terminate(),
            LegProcess::Adopted(process) => process.terminate(),
        }
    }

    /// The single reap point once the exit is observed, returning how the leg
    /// ended: the owned child's cached status, the same value on every call,
    /// or `None` for an adopted leg, whose status is unknown.
    pub fn reap(&self) -> Option<ExitStatus> {
        match self {
            LegProcess::Owned(child) => child.exit_status(),
            LegProcess::Adopted(process) => {
                #[cfg(unix)]
                process.reap();
                #[cfg(windows)]
                let _ = process;
                None
            }
        }
    }

    /// The `(pid, creation)` pair of a proven leg. An owned leg is never
    /// compared against a proof (A4 compares before it keeps the child).
    pub(crate) fn identity(&self) -> (u32, u64) {
        match self {
            LegProcess::Owned(child) => child.identity().unwrap_or((0, 0)),
            LegProcess::Adopted(process) => (process.pid(), process.created()),
        }
    }
}

impl std::fmt::Debug for LegProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LegProcess::Owned(child) => write!(
                f,
                "LegProcess::Owned(pid {})",
                child.identity().map_or(0, |i| i.0)
            ),
            LegProcess::Adopted(process) => write!(f, "LegProcess::Adopted(pid {})", process.pid()),
        }
    }
}
