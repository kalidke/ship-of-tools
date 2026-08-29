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
/// doc for the mechanism; this is its only public entry point.
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

    #[test]
    fn lock_supervisor_refuses_a_second_concurrent_holder_through_the_facade() {
        let dir = tempfile::tempdir().unwrap();
        let _held = lock_supervisor(dir.path()).unwrap();
        match lock_supervisor(dir.path()) {
            Err(e) => assert!(format!("{e}").contains("held by another process"), "{e}"),
            Ok(_) => panic!("expected a second lock_supervisor call to fail while the first is held"),
        }
    }

    /// The real concurrent race (round-1 required test): two THREADS
    /// racing `lock_supervisor` over the SAME absent path — unlike the
    /// bootstrap-only race test in `fsutil.rs`, this drives the FULL
    /// facade (bootstrap-or-reuse, then try_lock) under contention and
    /// proves the kernel lock itself arbitrates: exactly one thread ever
    /// holds the guard at a time, and across the whole race, exactly one
    /// `Ok` is observed WHILE its holder has not yet released — never two
    /// simultaneous holders.
    #[test]
    fn lock_supervisor_concurrent_race_grants_exactly_one_holder_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let concurrent_holders = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_observed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                let concurrent_holders = concurrent_holders.clone();
                let max_observed = max_observed.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    // Round-2 finding 6: bounded, and retries ONLY the
                    // expected contention error -- a real regression
                    // (a permission/path failure, say) must fail this
                    // test loudly and promptly, never retry forever.
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                    loop {
                        match lock_supervisor(&path) {
                            Ok(guard) => {
                                let now = concurrent_holders.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                                max_observed.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                                // Hold briefly, long enough that a second
                                // holder sneaking in would be caught by
                                // `max_observed`.
                                std::thread::sleep(std::time::Duration::from_millis(5));
                                // Decrement AFTER the guard drops (round-2
                                // finding 6): the "held" interval this test
                                // observes must cover the WHOLE time the
                                // kernel lock is actually held, not end one
                                // instruction early and leave a small
                                // unobserved ownership gap.
                                drop(guard);
                                concurrent_holders.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                                return;
                            }
                            Err(e) => {
                                assert!(
                                    format!("{e}").contains("held by another process"),
                                    "unexpected lock_supervisor failure (not ordinary contention): {e}"
                                );
                                assert!(
                                    std::time::Instant::now() < deadline,
                                    "gave up waiting for the supervisor fence after 30s -- looks wedged"
                                );
                                std::thread::sleep(std::time::Duration::from_millis(1));
                            }
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            max_observed.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "at most one thread may ever hold the supervisor fence at a time"
        );
    }
}
