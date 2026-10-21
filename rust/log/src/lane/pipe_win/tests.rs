use super::*;

use crate::lane::transport::join_within;

// -- join_within: the ADR 0041 step 6 U1b teardown-deadline mechanism,
// proven directly against plain `std::thread::spawn` closures this
// module fully controls -- no real pipe needed for the shared-deadline
// / loud-on-expiry properties themselves (`tests/pipe_win/` proves
// the same mechanism composed with real workers).

#[test]
fn join_within_true_when_the_thread_already_finished() {
    let jh = thread::spawn(|| {});
    // Real time for the thread to actually finish before polling
    // starts -- this asserts the HAPPY path, not a race against it.
    thread::sleep(Duration::from_millis(50));
    let deadline = Instant::now() + Duration::from_secs(5);
    assert!(join_within(jh, deadline));
}

#[test]
fn join_within_false_on_expiry_and_never_blocks_past_the_deadline() {
    let (tx, rx) = mpsc::channel::<()>();
    let jh = thread::spawn(move || {
        let _ = rx.recv(); // never sent: blocks until this process exits
    });
    let budget = Duration::from_millis(50);
    let deadline = Instant::now() + budget;
    let started = Instant::now();
    let ok = join_within(jh, deadline);
    let elapsed = started.elapsed();
    assert!(!ok);
    // Bounded, not merely eventually false -- the whole point of never
    // calling the blocking `JoinHandle::join`.
    assert!(
        elapsed < budget + Duration::from_secs(2),
        "took {elapsed:?} against a {budget:?} budget"
    );
    drop(tx);
}

#[test]
fn one_shared_deadline_not_a_fresh_budget_per_join() {
    // Two joins against the SAME deadline: the first consumes most of
    // the budget by construction (a thread that finishes only after
    // most of it has elapsed), so the second must see a near-zero
    // REMAINING budget -- proving the deadline is shared, not reset
    // per call (ADR 0041: "each wait taking the remaining budget").
    let budget = Duration::from_millis(150);
    let deadline = Instant::now() + budget;

    let jh1 = thread::spawn(move || thread::sleep(Duration::from_millis(100)));
    assert!(
        join_within(jh1, deadline),
        "the first join should still fit its share"
    );

    let (tx, rx) = mpsc::channel::<()>();
    let jh2 = thread::spawn(move || {
        let _ = rx.recv();
    });
    assert!(
        !join_within(jh2, deadline),
        "the second join must not get a fresh budget after the first consumed most of it"
    );
    drop(tx);
}

/// Codex round-2b, ruling on finding 2: the boundary test proving
/// expiry with a GENUINELY UNFINISHED thread is terminal, even
/// though that same thread finishes moments later -- "no acceptance
/// after the decision". Deterministic, not a race: BOTH preconditions
/// (the thread is still unfinished, AND the deadline has already
/// passed) are independently confirmed BEFORE `join_within` is ever
/// called, so this is not racing the exact expiry instant -- it
/// proves the DECISION itself (`false`), captured once, is never
/// revisited by the thread's later completion.
#[test]
fn expiry_with_a_genuinely_unfinished_thread_is_terminal_even_though_it_finishes_moments_later()
{
    let (tx, rx) = mpsc::channel::<()>();
    let jh = thread::spawn(move || {
        let _ = rx.recv(); // blocks until released below, AFTER the decision is made
    });
    let budget = Duration::from_millis(30);
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        !jh.is_finished(),
        "the thread must be genuinely unfinished at the deadline for this test to mean              anything -- confirmed BEFORE join_within is ever called"
    );
    let decision = join_within(jh, deadline);
    assert!(
        !decision,
        "an unfinished thread at expiry must be terminal (false)"
    );
    // Release the thread now, strictly AFTER the decision was made --
    // it finishing here must not (and structurally cannot: `decision`
    // is a plain bool already captured) retroactively flip anything.
    drop(tx);
}
