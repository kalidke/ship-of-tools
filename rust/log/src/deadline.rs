//! A three-state deadline race, shared by anything in this crate that
//! must bound a blocking operation it does not otherwise control (today:
//! `challenge::exchange_identity`, shared by every platform's own
//! `challenge()`). Portable — no OS
//! dependency at all, just `std::thread`/`std::sync::atomic`/
//! `std::sync::{Mutex, Condvar}`/`std::time` — so its race logic is
//! exercised by REAL executed tests on every CI platform, not merely
//! compile-checked on Windows.
//!
//! switch-latency Phase 1: the watchdog used to `thread::sleep(10ms)` in
//! a loop, so an idle-but-still-running watchdog woke ~100 times a
//! second, and — worse — `body` finishing early still cost this
//! function's caller up to a 10ms wait on `watchdog.join()` (the
//! watchdog only notices `state` left `PENDING` on its next poll tick).
//! The watchdog now blocks in `Condvar::wait_timeout`, armed for
//! whatever is left until `deadline`, and both places that move `state`
//! out of `PENDING` after entry (the normal completion path and
//! `SettleOnPanic::drop`) notify it right after their own CAS — an idle
//! watchdog parks rather than polling, so it produces no PERIODIC
//! wakeup (a spurious OS wakeup can still happen; the loop just
//! re-checks and re-arms). `run_with_deadline`'s return still requires
//! joining the watchdog thread, same as before this change — the
//! notify is what usually makes that join prompt rather than a
//! guarantee that it always is (see the `on_time`/`notify_watchdog`
//! ordering comment at the return path for the one case where the
//! notify itself can be delayed, and why that no longer affects
//! correctness).

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Condvar, Mutex};
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
    run_with_deadline_traced(deadline, on_timeout, body, || {})
}

/// Same as [`run_with_deadline`], plus `on_watchdog_wake` — called once
/// per iteration of the watchdog's own loop (its initial entry, and
/// again each time `Condvar::wait_timeout` returns, by notify, a
/// spurious OS wakeup, or its own timeout). Exists so tests can tell a
/// genuinely notified wait (entry, then ordinarily one more call, never
/// growing with how long `body` takes) apart from a poll loop (one call
/// per tick FOR AS LONG AS `body` runs) WITHOUT depending on wall-clock
/// timing, which scheduler noise makes an unreliable witness either
/// way. `run_with_deadline` is this with a no-op hook — the two share
/// every line of the actual race logic, so a test run through this
/// entry point exercises the exact same code the real callers do.
pub(crate) fn run_with_deadline_traced<T>(
    deadline: Instant,
    on_timeout: impl Fn() + Sync,
    body: impl FnOnce() -> T,
    on_watchdog_wake: impl Fn() + Sync,
) -> Option<T> {
    const PENDING: u8 = 0;
    const COMPLETED: u8 = 1;
    const TIMED_OUT: u8 = 2;
    let state = AtomicU8::new(PENDING);
    // The watchdog's own doorbell: `body` settling (normally or via
    // panic) locks `gate` and calls `notify_all` right after its CAS, so
    // the watchdog — parked in `wait_timeout` holding the SAME lock —
    // can never miss the wakeup (it can only be mid-`wait` or about to
    // re-check `state` while holding `gate`; a notifier blocked on that
    // same lock cannot slip a wakeup into the gap between the two). A
    // plain `Condvar`/`Mutex` pair, not tied to `state`'s own encoding —
    // `state` stays the single source of truth for WHO won the race,
    // this only ever wakes a waiter early.
    let gate = Mutex::new(());
    let signal = Condvar::new();
    let notify_watchdog = || {
        drop(gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        signal.notify_all();
    };

    std::thread::scope(|scope| {
        // Already too late to even start: never attempt `body` at all --
        // there is no result an already-expired deadline could accept.
        // `on_timeout` still runs, so the connection is left in the same
        // "spent" state every other over-deadline outcome leaves it in.
        if Instant::now() >= deadline {
            on_timeout();
            return None;
        }

        let watchdog = std::thread::Builder::new().spawn_scoped(scope, || {
            let mut guard = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                on_watchdog_wake();
                if state.load(Ordering::Acquire) != PENDING {
                    return;
                }
                let now = Instant::now();
                if now >= deadline {
                    if state
                        .compare_exchange(PENDING, TIMED_OUT, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        on_timeout();
                    }
                    return;
                }
                // Armed for whatever is left until `deadline`, not a
                // fixed poll tick: a spurious wakeup (or one from
                // `notify_watchdog` that lost the race to a state change
                // from elsewhere) just loops back to the top and
                // re-checks `state`/re-arms the remaining wait.
                let (g, _timed_out) = signal
                    .wait_timeout(guard, deadline - now)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                guard = g;
            }
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
            notify_watchdog: &'a dyn Fn(),
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
                    // Wake the watchdog immediately rather than leaving
                    // it parked until `deadline` — see this function's
                    // own `notify_watchdog` doc.
                    (self.notify_watchdog)();
                }
            }
        }
        let result = {
            let _settle =
                SettleOnPanic { state: &state, on_timeout: &on_timeout, notify_watchdog: &notify_watchdog };
            body()
        };

        let claimed_completed = state
            .compare_exchange(PENDING, COMPLETED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        // Codex review round finding 1: evaluated IMMEDIATELY after the
        // CAS above, before `notify_watchdog()` below -- `notify_watchdog`
        // takes `gate`, which the watchdog can be holding (descheduled
        // between its own `state` check and its own `wait_timeout` call),
        // so it can block for an unbounded stretch of real time. Reading
        // `Instant::now()` only after that possible block let an
        // on-time `body` be misjudged late purely by how long the notify
        // happened to wait for the lock (reproduced: a `body` completing
        // well inside the deadline still came back cancelled). Timeliness
        // must be settled before touching anything that can stall.
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
        // Wake the watchdog now that this thread has fully settled --
        // see this function's own `notify_watchdog` doc. A no-op if the
        // CAS above lost (the watchdog already won its own and is not
        // waiting any more). Safe to let this block on `gate` here: it
        // can no longer feed back into `on_time`, which is already
        // decided above.
        notify_watchdog();
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
        // runs past: the watchdog (waiting out `deadline`, whether by
        // polling or by a timed park) discovers the expiry while `body`
        // is still sleeping, wins the CAS, and calls
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

    /// switch-latency Phase 1, via `run_with_deadline_traced`'s wake
    /// counter: proves the watchdog is genuinely NOTIFIED rather than
    /// polled, without depending on wall-clock latency (which scheduler
    /// noise makes an unreliable witness -- a loose bound admits the old
    /// 10ms poll just as easily as the new wait). `body` takes a real,
    /// deliberate 200ms on the calling thread, well inside the 30s
    /// deadline. The watchdog's own loop, ideally, wakes exactly twice:
    /// once at entry (parks, since 30s is still ahead) and once more
    /// when `notify_watchdog` runs right after `body` settles. A 10ms
    /// poll loop, by contrast, would wake roughly 200ms / 10ms ~= 20
    /// times over the same stretch -- the assertion below sits between
    /// those two numbers with slack for the rare spurious OS wakeup, not
    /// for a specific latency.
    #[test]
    fn completion_wakes_the_watchdog_a_bounded_number_of_times_not_once_per_poll_tick() {
        let wakes = AtomicUsize::new(0);
        let deadline = Instant::now() + Duration::from_secs(30);
        let result = run_with_deadline_traced(
            deadline,
            || {},
            || {
                std::thread::sleep(Duration::from_millis(200));
                42
            },
            || {
                wakes.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert_eq!(result, Some(42));
        let wake_count = wakes.load(Ordering::SeqCst);
        assert!(
            wake_count <= 6,
            "watchdog woke {wake_count} times across a 200ms body -- expected entry plus one \
             notified wake (2, with slack for a loaded CI runner's own spurious wakeups), not \
             a 10ms poll cadence (which would be ~20)"
        );
    }

    /// The timeout-path counterpart of the test above, proven the same
    /// way (wake count, not latency): `body` blocks on a channel that
    /// only `on_timeout` itself ever signals (mirrors
    /// `cancels_a_body_parked_mid_operation`), so this costs only the
    /// deadline itself, never a multi-second sleep on the calling
    /// thread. The watchdog's own loop ideally wakes exactly twice here
    /// too: once at entry (parks for the deadline's duration) and once
    /// when its own `wait_timeout` expires and it wins the CAS itself.
    /// No assertion depends on how CLOSE to the deadline that second
    /// wake lands -- only on how MANY wakes happened -- so a loaded CI
    /// runner delivering the timeout late costs this test time, never
    /// correctness.
    #[test]
    fn deadline_still_fires_on_a_stalled_body_without_repeated_polling() {
        let wakes = AtomicUsize::new(0);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let deadline = Instant::now() + Duration::from_millis(200);
        let result = run_with_deadline_traced(
            deadline,
            move || {
                let _ = tx.send(());
            },
            move || rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            || {
                wakes.fetch_add(1, Ordering::SeqCst);
            },
        );
        assert_eq!(result, None, "the deadline must win before body's own cancellation-triggered return");
        let wake_count = wakes.load(Ordering::SeqCst);
        assert!(
            wake_count <= 6,
            "watchdog woke {wake_count} times waiting out one 200ms deadline -- expected entry \
             plus its own timeout wake (2, with slack for a loaded CI runner's own spurious \
             wakeups), not a 10ms poll cadence (which would be ~20)"
        );
    }

    /// Regression test for Codex review round finding 1:
    /// `notify_watchdog` takes `gate`, which the watchdog can be
    /// holding while descheduled between its own `state` check and its
    /// own `wait_timeout` call -- `on_time` must be decided BEFORE that
    /// notify call, never after, or a `body` that genuinely finished on
    /// time can be judged late purely by how long the notify happened to
    /// block waiting for the lock. Reproduced with the old ordering:
    /// this returned `None` for a `body` that finished in nanoseconds.
    ///
    /// The interleaving is pinned deterministically, not hoped for:
    /// `on_watchdog_wake`'s first call runs ON THE WATCHDOG THREAD while
    /// it still holds `gate` (the hook sits at the very top of its
    /// loop, inside the guard scope). It signals `body` over a channel
    /// before sleeping well past `deadline` -- `body` waits for that
    /// signal before returning, so by the time this thread calls
    /// `notify_watchdog()` right after, the watchdog is GUARANTEED to
    /// still be holding `gate` for the whole stretch. The channel (with
    /// a bounded `recv_timeout`, rather than an unbounded rendezvous)
    /// keeps this test's own worst case bounded even if some future
    /// change stopped the watchdog loop from running at all.
    ///
    /// `deadline` (400ms) and the hook's hold (900ms) are both generous
    /// on purpose, not tight: the only two facts this test needs are
    /// "the channel handshake plus a thread spawn finishes well inside
    /// 400ms" (true on any CI runner this crate targets, loaded or not
    /// -- that handshake is microseconds of real work) and "900ms is
    /// unambiguously longer than 400ms" -- neither depends on exactly
    /// how fast either runs, only on that first bound being generous
    /// enough and the second exceeding it.
    #[test]
    fn on_time_completion_is_never_misjudged_late_by_a_delayed_notify() {
        let (holding_gate_tx, holding_gate_rx) = std::sync::mpsc::channel::<()>();
        let first_wake = AtomicUsize::new(0);
        let deadline = Instant::now() + Duration::from_millis(400);
        let result = run_with_deadline_traced(
            deadline,
            || {},
            move || {
                holding_gate_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("the watchdog must reach its first loop iteration");
                42 // finishes essentially instantly once signalled -- comfortably on time
            },
            move || {
                if first_wake.fetch_add(1, Ordering::SeqCst) == 0 {
                    let _ = holding_gate_tx.send(());
                    // Hold `gate` for well past `deadline` -- this
                    // thread's own `notify_watchdog()` call, made right
                    // after `body` returns, must block on this same
                    // lock for the whole stretch.
                    std::thread::sleep(Duration::from_millis(900));
                }
            },
        );
        assert_eq!(
            result,
            Some(42),
            "a body that finished on time must be accepted even though notifying the \
             watchdog afterward blocked past the deadline"
        );
    }
}
