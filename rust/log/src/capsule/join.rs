//! The capsule's bounded join: a worker thread is waited for against one shared deadline, never with the blocking
//! `JoinHandle::join`, which std gives no timeout.

use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// The aggregate budget the capsule's closer and reader joins share: expiry is loud, and a thread still unfinished
/// stays unjoined.
pub(crate) const TEARDOWN_AGGREGATE_DEADLINE: Duration = Duration::from_secs(20);

/// [`join_within`]'s poll granularity: small enough that a fast, healthy teardown never visibly waits for it.
const JOIN_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Wait for `jh` to finish without ever calling the BLOCKING `JoinHandle::join`, which std gives no timeout. Polls
/// [`JoinHandle::is_finished`] until it reports true (join it for real, which cannot then block, and return `true`) or
/// `deadline` passes (`false`: LOUD, since this crate cannot force an OS thread to stop; the handle is dropped, which
/// detaches rather than kills it). `is_finished` is checked FIRST on every poll, including the one that observes the
/// expired deadline, so a thread that finished at or before that instant is accepted even though the deadline has
/// also passed: the invariant is "never proceed past an UNFINISHED worker", and a finished thread leaves nothing
/// dangling. A thread that panicked still counts as finished.
pub(crate) fn join_within(jh: JoinHandle<()>, deadline: Instant) -> bool {
    loop {
        if jh.is_finished() {
            let _ = jh.join();
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(JOIN_POLL_INTERVAL);
    }
}

#[cfg(test)]
#[path = "join_tests.rs"]
mod tests;
