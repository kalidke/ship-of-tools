use super::*;
use std::thread;

/// Spins until exactly one reserve is parked in `Condvar::wait`,
/// observed via the test-only `waiters` witness under the same mutex
/// the wait releases atomically — the deterministic guarantee that the
/// wake path (not a lucky early bound-check) is what the test then
/// exercises.
fn await_one_waiter(budget: &OutputBudget) {
    loop {
        if budget.state.lock().unwrap().waiters == 1 {
            return;
        }
        thread::yield_now();
    }
}

#[test]
fn idle_output_commits_early_but_never_faster_than_the_fsync_gap() {
    let ms = Duration::from_millis;
    // flush: the window elapsed; or pending, output 2 ms quiet, last fsync >= 10 ms ago.
    assert!(should_flush_output(ms(50), 0, ms(0), ms(0)));
    assert!(should_flush_output(ms(5), 10, ms(2), ms(10)));
    // no flush: output under 2 ms ago; nothing pending before the window; fsync too recent.
    assert!(!should_flush_output(ms(5), 10, ms(1), ms(40)));
    assert!(!should_flush_output(ms(5), 0, ms(40), ms(40)));
    assert!(!should_flush_output(ms(5), 10, ms(40), ms(9)));
}

#[test]
fn output_wait_targets_the_earliest_allowed_commit() {
    let ms = Duration::from_millis;
    assert_eq!(output_wait(0, ms(5), ms(5), ms(5)), ms(50));
    assert_eq!(output_wait(10, ms(5), ms(0), ms(100)), ms(2));
    assert_eq!(output_wait(10, ms(5), ms(1), ms(3)), ms(7));
    assert!(output_wait(10, ms(49), ms(0), ms(0)) <= ms(1));
    assert_eq!(output_wait(10, ms(5), ms(5), ms(20)), ms(0));
}

/// A reserve that finds the budget full parks in the condvar wait
/// (proven by the waiter witness, not scheduling luck) and completes
/// exactly when room is released — so a lost `notify_all` cannot
/// escape this test on any interleaving.
#[test]
fn output_budget_blocks_at_the_bound_and_unblocks_on_release() {
    let budget = Arc::new(OutputBudget::new());
    assert!(budget.reserve(OUTPUT_QUEUE_BUDGET_BYTES));

    let (done_tx, done_rx) = mpsc::channel();
    let b = Arc::clone(&budget);
    let worker = thread::spawn(move || {
        done_tx.send(b.reserve(1)).unwrap();
    });

    await_one_waiter(&budget);
    budget.release(1);
    assert!(done_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("parked reserve never completed after release"));
    worker.join().unwrap();
    assert_eq!(budget.state.lock().unwrap().outstanding, OUTPUT_QUEUE_BUDGET_BYTES);
}

/// `cancel` wakes a PARKED reserve (same witness) and makes it return
/// `false` — the reader-must-stop signal — without any release ever
/// happening; and a cancelled budget refuses every future reserve.
#[test]
fn output_budget_cancel_unblocks_a_parked_reserve_with_false() {
    let budget = Arc::new(OutputBudget::new());
    assert!(budget.reserve(OUTPUT_QUEUE_BUDGET_BYTES));

    let (done_tx, done_rx) = mpsc::channel();
    let b = Arc::clone(&budget);
    let worker = thread::spawn(move || {
        done_tx.send(b.reserve(1)).unwrap();
    });

    await_one_waiter(&budget);
    budget.cancel();
    assert!(!done_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("parked reserve never returned after cancel"));
    worker.join().unwrap();
    assert!(!budget.reserve(1), "a cancelled budget must refuse every future reserve");
}
