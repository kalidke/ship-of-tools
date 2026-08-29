//! `<state-dir>/supervisor.lock`: the public facade over
//! `fsutil::lock_supervisor` (ADR 0041 Lifecycle "one authority, one
//! fence"). `fsutil` itself is a private module — invisible from ANY
//! other crate, including a future `sot-capsule` binary target, which
//! Cargo treats as a SEPARATE crate from this package's library even
//! though they share one `Cargo.toml` (ADR 0041 U0 round-1 blocker 3: a
//! `pub fn` inside a private module is unreachable from outside the
//! defining crate no matter how public the function itself is) — so this
//! is the one place outside `fsutil` that ANY external caller may take
//! the supervisor fence from. Mirrors `pointer::pointer_path`'s
//! discipline: callers pass the STATE DIR; the fixed file name is pinned
//! here, never re-derived by the caller, so two callers can never
//! accidentally mint different-named authority fences for the same
//! drawer.

use crate::fsutil;
use crate::Result;
use std::path::{Path, PathBuf};

const SUPERVISOR_LOCK_FILE_NAME: &str = "supervisor.lock";

/// `<state_dir>/supervisor.lock`.
pub fn supervisor_lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join(SUPERVISOR_LOCK_FILE_NAME)
}

/// The held supervisor fence — kernel-released on drop (including hard
/// kills), exactly like the voyage writer fence's own guard. A DISTINCT
/// type (not a re-export of `fsutil::WriterLock`), so external callers'
/// code reads as holding THE authority fence, not an unrelated per-voyage
/// writer lock that merely happens to share its kernel mechanics — and so
/// this crate never has to expose a private-module type through a public
/// signature to make the facade work.
pub struct SupervisorLock(#[allow(dead_code)] fsutil::WriterLock); // held for its Drop

/// Acquire the ONE-AUTHORITY fence under `state_dir` — bootstraps it
/// (`CREATE_NEW`) if absent, then takes it with the same kernel-lock
/// mechanics the writer fence uses. See `fsutil::lock_supervisor`'s own
/// doc for the mechanism; this is its only public entry point. See
/// `fsutil::WriterLock`'s own doc for a same-process, cross-platform
/// caveat found via a real CI failure (ADR 0041 U0 round 3): the
/// guarantee this fence actually provides on Windows is CROSS-PROCESS,
/// which is the real topology (two separate `sot-capsule supervise`
/// instances) — not necessarily "two threads of one process racing to
/// acquire at the same instant," which this crate never relies on.
pub fn lock_supervisor(state_dir: &Path) -> Result<SupervisorLock> {
    fsutil::lock_supervisor(&supervisor_lock_path(state_dir)).map(SupervisorLock)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_supervisor_pins_the_file_name_under_the_given_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        let guard = lock_supervisor(dir.path()).unwrap();
        assert!(supervisor_lock_path(dir.path()).is_file());
        drop(guard);
    }

    /// The thread-level guarantee this fence ACTUALLY provides, on both
    /// platforms: a lock already held (sequentially, before a second call
    /// is even attempted) correctly refuses a second acquisition. Unlike
    /// the cross-process race below, this needs no process boundary --
    /// it never races two FIRST acquisitions against each other.
    #[test]
    fn lock_supervisor_refuses_a_second_concurrent_holder_through_the_facade() {
        let dir = tempfile::tempdir().unwrap();
        let _held = lock_supervisor(dir.path()).unwrap();
        match lock_supervisor(dir.path()) {
            Err(e) => assert!(format!("{e}").contains("held by another process"), "{e}"),
            Ok(_) => panic!("expected a second lock_supervisor call to fail while the first is held"),
        }
    }

    /// The "child" role for the cross-process race below: attempts
    /// `lock_supervisor` on the path named by `FENCE_XPROC_STATE_DIR`,
    /// and while holding it, proves SOLE ownership by exclusively
    /// creating a marker file at `FENCE_XPROC_MARKER` (`create_new` --
    /// if another process is ALSO inside its own holding section right
    /// now, this fails: a genuine overlap, the one thing an in-memory
    /// counter cannot detect across process boundaries). A normal test
    /// pass never sets `FENCE_XPROC_STATE_DIR`, so this is a silent
    /// no-op then — only the parent test below ever invokes it BY NAME
    /// with the env vars set, in a dedicated child process (the same
    /// self-re-exec shape `tests/pipe_win.rs`'s own cross-process
    /// challenge test uses).
    #[test]
    fn lock_supervisor_cross_process_race_child_role() {
        let Ok(state_dir) = std::env::var("FENCE_XPROC_STATE_DIR") else {
            return;
        };
        let marker = std::env::var("FENCE_XPROC_MARKER")
            .expect("FENCE_XPROC_MARKER must be set alongside FENCE_XPROC_STATE_DIR");
        let guard = lock_supervisor(Path::new(&state_dir)).expect("child: lock_supervisor failed");
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&marker) {
            Ok(_) => {}
            Err(e) => panic!("child: overlapping holder detected while creating the marker: {e}"),
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::remove_file(&marker).expect("child: remove marker");
        drop(guard);
    }

    /// The real concurrent race (round-1 required test; corrected in
    /// round 3 after a genuine windows-latest CI failure in the original
    /// same-PROCESS, multi-THREAD version of this test).
    ///
    /// DIAGNOSIS: that failure was NOT a test-observation artifact —
    /// every counter update here already used `SeqCst`, and the
    /// increment/decrement bracket the guard's own lifetime correctly.
    /// It was a genuine platform difference in the underlying primitive
    /// (see `fsutil::WriterLock`'s own doc): unix `flock`'s ownership is
    /// the OPEN FILE DESCRIPTION, so two threads in one process, each
    /// with its own handle, correctly serialize — but Windows
    /// `LockFileEx`'s exclusivity is not documented, and was not
    /// observed, to hold that way: two DIFFERENT handles opened by the
    /// SAME PROCESS, racing to acquire at genuinely the same instant, can
    /// BOTH be granted. The real topology this fence exists for is
    /// CROSS-PROCESS (two separate `sot-capsule supervise` instances),
    /// where the kernel-arbitrated guarantee holds on both platforms —
    /// so this test now races real OS PROCESSES instead of threads,
    /// which is both the honest fix and the only way to make this
    /// assertion mean what it claims on Windows. Proof of sole ownership
    /// is an exclusively-created marker FILE (not an in-memory counter,
    /// which cannot be shared across process boundaries at all): if two
    /// children were EVER both inside their holding section at once, the
    /// second `create_new` on the shared marker path fails.
    ///
    /// Deterministic under two-core contention by construction: each
    /// child holds for only 10ms, so even a fully-serialized worst case
    /// across a handful of children stays far inside `lock_writer`'s own
    /// 250ms internal retry deadline (`fsutil::RETRY_DEADLINE_MS`) --
    /// that deadline starts counting only once a child actually calls
    /// `lock_supervisor`, not from process spawn, so slow process
    /// startup under a contended runner does not eat into it.
    #[test]
    fn lock_supervisor_cross_process_race_grants_exactly_one_holder_at_a_time() {
        // Guard against a nested re-exec somehow reaching this branch
        // instead of the dedicated child-role test above.
        if std::env::var("FENCE_XPROC_STATE_DIR").is_ok() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().to_path_buf();
        let marker = dir.path().join("holder.marker");
        let exe = std::env::current_exe().expect("current_exe");

        const CHILDREN: usize = 4;
        let mut children: Vec<std::process::Child> = (0..CHILDREN)
            .map(|_| {
                std::process::Command::new(&exe)
                    .arg("--exact")
                    .arg("fence::tests::lock_supervisor_cross_process_race_child_role")
                    .arg("--nocapture")
                    .arg("--test-threads=1")
                    .env("FENCE_XPROC_STATE_DIR", &state_dir)
                    .env("FENCE_XPROC_MARKER", &marker)
                    .spawn()
                    .expect("failed to spawn cross-process race child")
            })
            .collect();

        let statuses: Vec<_> = children.iter_mut().map(|c| c.wait().expect("wait on race child")).collect();
        let failures: Vec<_> = statuses.iter().filter(|s| !s.success()).collect();
        assert!(
            failures.is_empty(),
            "one or more cross-process race children failed (an overlap was detected, or a real lock error occurred): {statuses:?}"
        );
    }
}
