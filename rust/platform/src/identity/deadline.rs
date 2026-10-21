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
        // unwind, before `thread::scope`'s own join. It is disarmed the
        // moment `body` returns, so it settles exactly the runs whose
        // `body` did not return, whatever the thread was doing before:
        // a run made during another panic's unwind (a `Drop`'s cleanup)
        // that returns is judged by `body` like any other, and one whose
        // `body` panics, even inside a `catch_unwind` in such a cleanup,
        // settles at once. It never reads `std::thread::panicking()`. The
        // explicit post-`body()` logic below settles every normal return.
        struct SettleOnPanic<'a> {
            state: &'a AtomicU8,
            on_timeout: &'a (dyn Fn() + Sync),
            notify_watchdog: &'a dyn Fn(),
            armed: bool,
        }
        impl Drop for SettleOnPanic<'_> {
            fn drop(&mut self) {
                if self.armed
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
            let mut settle = SettleOnPanic {
                state: &state,
                on_timeout: &on_timeout,
                notify_watchdog: &notify_watchdog,
                armed: true,
            };
            let result = body();
            settle.armed = false;
            result
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
#[path = "deadline_tests.rs"]
mod tests;
