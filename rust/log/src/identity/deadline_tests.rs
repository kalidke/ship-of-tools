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

/// A run made from a `Drop` while its thread unwinds a panic that began outside the run (a guard's cleanup, a
/// test's teardown) is judged by its own body: a body that returns in time is accepted, and nothing is cancelled.
/// The settle-on-panic guard acts only on a panic that began inside `body`.
#[test]
fn a_run_from_a_drop_during_an_unwind_is_judged_by_its_own_body() {
    struct RunsOnDrop<'a> {
        result: &'a std::sync::Mutex<Option<Option<u32>>>,
        cancels: &'a AtomicUsize,
    }
    impl Drop for RunsOnDrop<'_> {
        fn drop(&mut self) {
            let run = run_with_deadline(
                Instant::now() + Duration::from_secs(30),
                || {
                    self.cancels.fetch_add(1, Ordering::SeqCst);
                },
                || 7,
            );
            *self
                .result
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(run);
        }
    }
    let result = std::sync::Mutex::new(None);
    let cancels = AtomicUsize::new(0);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _cleanup = RunsOnDrop {
            result: &result,
            cancels: &cancels,
        };
        panic!("a panic outside the run, which the cleanup's run happens during");
    }));
    assert!(unwound.is_err(), "the closure's own panic propagates");
    let run = *result
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(
        run,
        Some(Some(7)),
        "a run made during an unwind is judged by its own body"
    );
    assert_eq!(
        cancels.load(Ordering::SeqCst),
        0,
        "and an on-time body is never cancelled"
    );
}

/// A body that panics is settled at once even when its run is made during an unwind and its panic is caught there
/// (a cleanup that catches its own failure): the guard settles every run whose body did not return, whatever the
/// thread was doing before, so the caller does not wait out the deadline.
#[test]
fn a_caught_panic_in_a_run_made_during_an_unwind_settles_promptly() {
    struct CatchesOnDrop<'a> {
        seen: &'a std::sync::Mutex<Option<(bool, Duration)>>,
        cancels: &'a AtomicUsize,
    }
    impl Drop for CatchesOnDrop<'_> {
        fn drop(&mut self) {
            let started = Instant::now();
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_with_deadline(
                    Instant::now() + Duration::from_secs(10),
                    || {
                        self.cancels.fetch_add(1, Ordering::SeqCst);
                    },
                    || -> u32 { panic!("the body's own panic, caught by the cleanup") },
                )
            }));
            let seen = (caught.is_err(), started.elapsed());
            *self
                .seen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(seen);
        }
    }
    let seen = std::sync::Mutex::new(None);
    let cancels = AtomicUsize::new(0);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _cleanup = CatchesOnDrop {
            seen: &seen,
            cancels: &cancels,
        };
        panic!("a panic outside the run, which the cleanup's run happens during");
    }));
    assert!(unwound.is_err(), "the closure's own panic propagates");
    let seen = *seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (caught, took) = seen.expect("the cleanup ran");
    assert!(
        caught,
        "the body's panic reaches the cleanup's catch_unwind"
    );
    assert!(
        took < Duration::from_secs(5),
        "the run waited {took:?} for a deadline its panicked body never met"
    );
    assert_eq!(
        cancels.load(Ordering::SeqCst),
        1,
        "the run is settled, and cancelled, exactly once"
    );
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
