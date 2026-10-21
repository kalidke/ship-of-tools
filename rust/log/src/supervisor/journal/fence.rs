//! `<state-dir>/supervisor.lock`: the public facade over
//! `host::lock_supervisor` (ADR 0041 Lifecycle "one authority, one
//! fence"). `host::lock_supervisor` takes a lock PATH and hands back the
//! writer lock's own type, so this is where an external caller
//! takes the supervisor fence for a state dir, including the
//! `sot-capsule` binary target, which Cargo treats as a SEPARATE crate
//! from this package's library. Mirrors `pointer::pointer_path`'s
//! discipline: callers pass the STATE DIR; the fixed file name is pinned
//! here, never re-derived by the caller, so two callers can never
//! accidentally mint different-named authority fences for the same
//! drawer.

use sot_platform::host;
use crate::Result;
use std::path::{Path, PathBuf};

const SUPERVISOR_LOCK_FILE_NAME: &str = "supervisor.lock";

/// `<state_dir>/supervisor.lock`.
pub fn supervisor_lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join(SUPERVISOR_LOCK_FILE_NAME)
}

/// The held supervisor fence — unlocked on drop, kernel-released on any
/// death (including hard kills), exactly like the voyage writer fence's own guard. A DISTINCT
/// type (not a re-export of `host::WriterLock`), so external callers'
/// code reads as holding THE authority fence, not an unrelated per-voyage
/// writer lock that merely happens to share its kernel mechanics — and so
/// this facade's public signature names no type from `host`.
pub struct SupervisorLock(#[allow(dead_code)] host::WriterLock); // held for its Drop

/// Acquire the ONE-AUTHORITY fence under `state_dir` — bootstraps it
/// (`CREATE_NEW`) if absent, then takes it with the same kernel-lock
/// mechanics the writer fence uses. See `host::lock_supervisor`'s own
/// doc for the mechanism; this is its only public entry point. Mandatory
/// and cross-process on both platforms (see `host::WriterLock`'s own
/// doc) — the real topology is CROSS-PROCESS (two separate
/// `sot-capsule supervise` instances), and that is exactly what the
/// kernel arbitrates.
pub fn lock_supervisor(state_dir: &Path) -> Result<SupervisorLock> {
    Ok(SupervisorLock(host::lock_supervisor(&supervisor_lock_path(state_dir))?))
}

#[cfg(test)]
#[path = "fence_tests.rs"]
mod tests;
