//! A three-state deadline race, shared by anything in this crate that
//! must bound a blocking operation it does not otherwise control (today:
//! `challenge::challenge`'s post-SID identity exchange). Portable — no OS
//! dependency at all, just `std::thread`/`std::sync::atomic`/`std::time`
//! — so its race logic is exercised by REAL executed tests on every CI
//! platform, not merely compile-checked on Windows.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

/// Run `body` to completion on the CALLING thread, but accept its result
/// only if `body` claims completion before `deadline` — enforced by a
/// three-state race (`PENDING` -> `COMPLETED` or `PENDING` -> `TIMED_OUT`,
/// whichever side wins the compare-exchange), never a plain flag (ADR
/// 0041 U0 round-1 finding 4): a body finishing exactly as the deadline
/// passes can never be BOTH accepted as on-time by one racer AND
/// separately cancelled by the other, because only the winner of the CAS
/// acts at all.
///
/// `on_timeout` runs at most once, and ONLY when either racer discovers
/// `body` must not be trusted: the watchdog thread runs it after winning
/// the race outright, and — since a body that wins the CAS an instant
/// after the deadline has already passed must still be rejected — the
/// CALLING thread runs it itself when ITS claim of `COMPLETED` turns out
/// to be too late (the watchdog would never otherwise get a chance to,
/// having already lost that same CAS). Two edge cases never even run
/// `body`, calling `on_timeout` directly instead: a `deadline` already
/// past AT ENTRY (there is no result an expired deadline could ever
/// accept, so attempting the exchange at all would be pure waste), and a
/// watchdog thread that fails to spawn at all (OS resource exhaustion) —
/// the safe default when a deadline cannot be enforced is to never trust
/// an unbounded run, not to attempt one anyway.
pub fn run_with_deadline<T>(
    deadline: Instant,
    on_timeout: impl Fn() + Sync,
    body: impl FnOnce() -> T,
) -> Option<T> {
    const PENDING: u8 = 0;
    const COMPLETED: u8 = 1;
    const TIMED_OUT: u8 = 2;
    let state = AtomicU8::new(PENDING);

    std::thread::scope(|scope| {
        // Already too late to even start: never attempt `body` at all --
        // there is no result an already-expired deadline could accept.
        // `on_timeout` still runs, so the connection is left in the same
        // "spent" state every other over-deadline outcome leaves it in.
        if Instant::now() >= deadline {
            on_timeout();
            return None;
        }

        let watchdog = std::thread::Builder::new().spawn_scoped(scope, || loop {
            if state.load(Ordering::Acquire) != PENDING {
                return;
            }
            if Instant::now() >= deadline {
                if state
                    .compare_exchange(PENDING, TIMED_OUT, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    on_timeout();
                }
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        });
        let Ok(watchdog) = watchdog else {
            // Cannot bound this exchange at all: never run `body`, never
            // trust a result we could not have cancelled.
            on_timeout();
            return None;
        };

        let result = body();

        let claimed_completed = state
            .compare_exchange(PENDING, COMPLETED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        let on_time = Instant::now() < deadline;
        let outcome = if claimed_completed && on_time {
            Some(result)
        } else {
            if claimed_completed {
                // We won the CAS, but only after the deadline had already
                // passed: the watchdog will see `COMPLETED` (not
                // `PENDING`) and return without ever cancelling, so this
                // thread — the only one left that can — does it instead.
                on_timeout();
            }
            None
        };
        let _ = watchdog.join();
        outcome
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    // Pure, deterministic race-logic tests: no pipes, no Windows calls —
    // every scenario is driven by fake bodies and injected (already-past
    // or comfortably-future) deadlines, so these can never flake under CI
    // scheduling contention.

    #[test]
    fn expired_at_entry_never_runs_body() {
        let body_ran = AtomicUsize::new(0);
        let cancels = AtomicUsize::new(0);
        let deadline = Instant::now() - Duration::from_millis(1);
        let result = run_with_deadline(
            deadline,
            || {
                cancels.fetch_add(1, Ordering::SeqCst);
            },
            || {
                body_ran.fetch_add(1, Ordering::SeqCst);
                42
            },
        );
        assert_eq!(result, None);
        assert_eq!(body_ran.load(Ordering::SeqCst), 0, "body must never run past an already-expired deadline");
        assert_eq!(cancels.load(Ordering::SeqCst), 1, "the watchdog must still fire exactly once");
    }

    #[test]
    fn body_finishing_well_before_deadline_succeeds_uncancelled() {
        let cancels = AtomicUsize::new(0);
        let deadline = Instant::now() + Duration::from_secs(30);
        let result = run_with_deadline(
            deadline,
            || {
                cancels.fetch_add(1, Ordering::SeqCst);
            },
            || 7,
        );
        assert_eq!(result, Some(7));
        assert_eq!(cancels.load(Ordering::SeqCst), 0, "a body that finishes on time must never be cancelled");
    }

    #[test]
    fn body_finishing_after_deadline_is_rejected_and_cancelled_once() {
        // The reply/deadline race: `body` "completes" (returns a value)
        // strictly after `deadline` has already passed, simulating a
        // reply that physically arrived too late. The deadline is set in
        // the PAST, so the outcome is deterministic regardless of which
        // side's CAS happens to win first (see the doc on
        // `run_with_deadline`'s own "too late" self-check): either the
        // watchdog wins outright, or `body` wins the CAS but is then
        // rejected by its own on-time check -- both paths call
        // `on_timeout` exactly once and reject the result.
        let cancels = AtomicUsize::new(0);
        let deadline = Instant::now() - Duration::from_millis(50);
        let result = run_with_deadline(
            deadline,
            || {
                cancels.fetch_add(1, Ordering::SeqCst);
            },
            || {
                std::thread::sleep(Duration::from_millis(20));
                99
            },
        );
        assert_eq!(result, None);
        assert_eq!(cancels.load(Ordering::SeqCst), 1, "on_timeout must fire exactly once, whichever side wins");
    }

    #[test]
    fn success_leaves_the_connection_uncancelled_for_reuse() {
        // "success connection reuse": proves the on-time path never
        // cancels, which is the exact invariant a caller reusing the
        // SAME connection for a later mgmt round trip depends on.
        let cancels = AtomicUsize::new(0);
        let deadline = Instant::now() + Duration::from_secs(30);
        let result = run_with_deadline(deadline, || { cancels.fetch_add(1, Ordering::SeqCst); }, || "ok");
        assert_eq!(result, Some("ok"));
        assert_eq!(cancels.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cancels_a_body_parked_mid_operation() {
        // Simulates cancellation arriving while `body` is blocked
        // "mid write" or "mid read": `body` waits on a channel that only
        // the deadline's own cancel callback ever signals, so the test
        // can only pass if `on_timeout` genuinely unblocks it -- exactly
        // the shape a real blocked `write_all`/`read` cancelled by
        // `conn.cancel()` has.
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let deadline = Instant::now() + Duration::from_millis(50);
        let result = run_with_deadline(
            deadline,
            move || {
                let _ = tx.send(());
            },
            move || rx.recv_timeout(Duration::from_secs(5)).is_ok(),
        );
        assert_eq!(result, None, "the deadline must win before body's own timeout would");
    }

    #[test]
    fn result_needs_no_send_bound() {
        // Regression guard: `body` runs on the CALLING thread (only the
        // watchdog is spawned), so a `T`/closure capturing a non-`Send`
        // value must still compile. `Rc` is the standard non-Send probe.
        let rc = std::rc::Rc::new(5);
        let deadline = Instant::now() + Duration::from_secs(30);
        let result = run_with_deadline(deadline, || {}, || *rc);
        assert_eq!(result, Some(5));
    }
}
