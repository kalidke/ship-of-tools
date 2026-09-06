//! A three-state deadline race, shared by anything in this crate that
//! must bound a blocking operation it does not otherwise control (today:
//! `challenge::exchange_identity`, shared by every platform's own
//! `challenge()`). Portable — no OS
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

        // Round-2 finding 4: settle the race IMMEDIATELY if `body` panics
        // while still `PENDING`, rather than leaving the watchdog to
        // poll all the way out to `deadline` before it notices (a
        // distant deadline would otherwise make a panic look hung — the
        // panic itself unwinds instantly, but `thread::scope` will not
        // let it past this frame until the watchdog thread has been
        // joined, and the watchdog only stops polling once `state`
        // leaves `PENDING`). `SettleOnPanic`'s `Drop` runs during that
        // unwind, before `thread::scope`'s own join, and does nothing on
        // a normal return (`std::thread::panicking()` is false then) —
        // the explicit post-`body()` logic below is what settles the
        // NORMAL-return case, unchanged.
        struct SettleOnPanic<'a> {
            state: &'a AtomicU8,
            on_timeout: &'a (dyn Fn() + Sync),
        }
        impl Drop for SettleOnPanic<'_> {
            fn drop(&mut self) {
                if std::thread::panicking()
                    && self
                        .state
                        .compare_exchange(PENDING, TIMED_OUT, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    (self.on_timeout)();
                }
            }
        }
        let result = {
            let _settle = SettleOnPanic { state: &state, on_timeout: &on_timeout };
            body()
        };

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
        // Propagate a watchdog panic (round-2 finding 4) rather than
        // silently discarding it — `on_timeout` is caller-supplied and a
        // bug in it must not vanish just because it happened to run on
        // the watchdog thread instead of this one.
        match watchdog.join() {
            Ok(()) => outcome,
            Err(payload) => std::panic::resume_unwind(payload),
        }
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
        // Round-2 finding 3: the round-1 version of this test passed an
        // ALREADY-PAST deadline, which the entry check rejects before
        // `body` ever runs -- it re-tested `expired_at_entry_never_runs_body`
        // under a different name, not the reply/deadline race at all.
        // The GENUINE race needs a FUTURE deadline that `body` actually
        // runs past: the watchdog (polling every 10ms) discovers the
        // expiry while `body` is still sleeping, wins the CAS, and calls
        // `on_timeout` -- `body`'s own later completion then loses its
        // own CAS attempt (state is already `TIMED_OUT`, not `PENDING`),
        // so its result is discarded without a second `on_timeout` call.
        let cancels = AtomicUsize::new(0);
        let deadline = Instant::now() + Duration::from_millis(50);
        let result = run_with_deadline(
            deadline,
            || {
                cancels.fetch_add(1, Ordering::SeqCst);
            },
            || {
                std::thread::sleep(Duration::from_millis(300)); // well past the 50ms deadline
                99
            },
        );
        assert_eq!(result, None);
        assert_eq!(cancels.load(Ordering::SeqCst), 1, "on_timeout must fire exactly once, from the watchdog's own genuine win");
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

    /// Round-2 finding 4: a body that panics while `PENDING` must settle
    /// (and cancel) IMMEDIATELY, not leave the watchdog polling all the
    /// way out to a distant deadline before it notices `state` changed.
    /// `deadline` here is deliberately 30s away -- if the fix regressed,
    /// this test would take that long (or hang the whole suite) instead
    /// of finishing in well under a second.
    #[test]
    fn a_panicking_body_settles_promptly_instead_of_hanging_until_a_distant_deadline() {
        let cancels = AtomicUsize::new(0);
        let deadline = Instant::now() + Duration::from_secs(30);
        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_with_deadline(
                deadline,
                || {
                    cancels.fetch_add(1, Ordering::SeqCst);
                },
                || -> i32 { panic!("body panicked") },
            )
        }));
        assert!(result.is_err(), "the panic must propagate out of run_with_deadline, not be swallowed");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "must settle promptly, not hang until the distant deadline: took {:?}",
            started.elapsed()
        );
        assert_eq!(cancels.load(Ordering::SeqCst), 1, "a panicking body must still settle+cancel exactly once");
    }

    /// Round-2 finding 4: a panic inside `on_timeout`, running on the
    /// WATCHDOG thread (the genuine future-deadline race, not the
    /// synchronous expired-at-entry path), must propagate to the caller
    /// via the join, not be discarded by a swallowed `let _ =
    /// watchdog.join()`.
    #[test]
    fn watchdog_panic_in_on_timeout_propagates_to_the_caller() {
        let deadline = Instant::now() + Duration::from_millis(50);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_with_deadline(
                deadline,
                || panic!("on_timeout panicked"),
                || {
                    std::thread::sleep(Duration::from_millis(300)); // outlive the 50ms deadline
                    42
                },
            )
        }));
        assert!(result.is_err(), "a watchdog-thread panic in on_timeout must propagate, not be swallowed");
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
