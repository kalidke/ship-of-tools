//! birth_claim.rs — the claim a capsule's birth carries: the row's own `supervisor.lock`, taken before the birth is
//! accepted and kept, by the same open file description, through the native launch, the target's exec and the new
//! supervisor's takeover. There is one fence per row and no second lock, receipt or reclaim; this is a facade over
//! `host::lock_supervisor_for_handover` that pins the file name the way [`super::journal::fence`] does.
//!
//! The claim is close-only (`host::HandoverLock`): the process that takes it passes the descriptor to the child
//! through the launcher and drops its own copy; the fence stays contended until the last copy closes, whether the
//! parent closed it, died, or the child exited.

#![cfg(unix)]

use crate::host;
use crate::Result;
use std::os::fd::RawFd;
use std::path::Path;

/// The fence of `state_dir`, held for a birth.
pub struct BirthClaim {
    lock: host::HandoverLock,
}

impl BirthClaim {
    /// Take the fence under `state_dir` (bootstrapping it when absent). `Error::State` when another holder has it:
    /// a live authority, or another birth's claim.
    pub fn take(state_dir: &Path) -> Result<BirthClaim> {
        let path = super::journal::fence::supervisor_lock_path(state_dir);
        host::lock_supervisor_for_handover(&path).map(|lock| BirthClaim { lock })
    }

    /// The descriptor the launcher leaves open in the child.
    pub fn as_raw_fd(&self) -> RawFd {
        self.lock.as_raw_fd()
    }

    /// The new supervisor's takeover of the claim it was born holding (`fd`).
    pub fn adopt(fd: RawFd, state_dir: &Path) -> Result<BirthClaim> {
        let path = super::journal::fence::supervisor_lock_path(state_dir);
        host::HandoverLock::adopt(fd, &path).map(|lock| BirthClaim { lock })
    }
}
