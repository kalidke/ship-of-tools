//! The hold tool, in one process. Each test names its own point: an arm is exclusive per point, and the tests run
//! beside each other. Every wait here is a condition with a stated bound that fails with what it saw.

use super::*;
use crate::hold::{arrive, waiting_arms};
use std::fmt::Display;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const SEEN: Duration = Duration::from_secs(10);

/// An arrival on its own thread; the receiver hears when it has gone on.
fn arrives(point: &'static str, key: Option<&str>) -> mpsc::Receiver<()> {
    let key = key.map(str::to_string);
    let (done, gone) = mpsc::channel();
    std::thread::spawn(move || {
        arrive(point, key.as_ref().map(|key| key as &dyn Display));
        done.send(()).ok();
    });
    gone
}

fn goes_on(gone: &mpsc::Receiver<()>, what: &str) {
    gone.recv_timeout(SEEN).unwrap_or_else(|_| panic!("{what}: the arrival did not go on within {SEEN:?}"));
}

fn panic_text(result: std::thread::Result<()>) -> String {
    let payload = result.expect_err("expected a panic");
    payload.downcast_ref::<String>().cloned().or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
}

#[test]
fn a_held_arrival_waits_at_its_line_until_released() {
    let arm = Holds::here().arm("t.waits").hold(1);
    let gone = arrives("t.waits", None);
    arm.reached(0);
    assert!(matches!(gone.try_recv(), Err(mpsc::TryRecvError::Empty)));
    arm.release(0);
    goes_on(&gone, "after release");
}

#[test]
fn arrivals_take_tickets_in_order_and_only_the_held_ones_wait() {
    let arm = Holds::here().arm("t.tickets").hold(2);
    let first = arrives("t.tickets", None);
    arm.reached(0);
    let second = arrives("t.tickets", None);
    arm.reached(1);
    goes_on(&arrives("t.tickets", None), "the third, past the hold count");
    assert_eq!(arm.arrivals(), 3);
    assert!(panic_text(catch_unwind(AssertUnwindSafe(|| arm.release(2)))).contains("Passed"), "a passed ticket is not held");
    arm.release(1);
    goes_on(&second, "ticket 1");
    arm.release(0);
    goes_on(&first, "ticket 0");
}

#[test]
fn an_unarmed_point_holds_and_counts_nothing() {
    let started = std::time::Instant::now();
    crate::hold_point!("t.unarmed");
    crate::hold_point!("t.unarmed", "key");
    assert!(started.elapsed() < WAIT, "an unarmed point must not wait");
    let arm = Holds::here().arm("t.unarmed").hold(0);
    assert_eq!(arm.arrivals(), 0, "arrivals before the arm are not seen");
}

#[test]
fn a_keyed_arm_sees_only_its_key() {
    let arm = Holds::here().arm("t.keyed").key("mine").hold(1);
    goes_on(&arrives("t.keyed", Some("theirs")), "another key");
    goes_on(&arrives("t.keyed", None), "no key");
    assert_eq!(arm.arrivals(), 0);
    let held = arrives("t.keyed", Some("mine"));
    arm.reached(0);
    arm.release(0);
    goes_on(&held, "the matching key");
    assert_eq!(arm.arrivals(), 1);
}

#[test]
fn an_arrival_nobody_releases_gives_up_within_its_bound_and_fails_the_arm() {
    let arm = Holds::here().arm("t.giveup").give_up_after(Duration::from_millis(200)).hold(1);
    let gone = arrives("t.giveup", None);
    goes_on(&gone, "after its give-up");
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| arm.release(0))));
    assert!(text.contains("t.giveup") && text.contains("arrival 0") && text.contains("went on"), "{text}");
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| drop(arm))));
    assert!(text.contains("t.giveup") && text.contains("arrival 0"), "{text}");
}

#[test]
fn reached_fails_by_name_within_its_bound() {
    let arm = Holds::here().arm("t.never").hold(1);
    let started = std::time::Instant::now();
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| arm.reached_within(0, Duration::from_millis(200)))));
    assert!(text.contains("t.never") && text.contains("arrival 0") && text.contains("0 arrivals seen"), "{text}");
    assert!(started.elapsed() < SEEN);
}

#[test]
fn release_before_the_arrival_is_refused() {
    let arm = Holds::here().arm("t.early").hold(1);
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| arm.release(0))));
    assert!(text.contains("has not arrived"), "{text}");
}

#[test]
fn dropping_an_arm_releases_every_held_arrival() {
    let arm = Holds::here().arm("t.drop").hold(2);
    let first = arrives("t.drop", None);
    let second = arrives("t.drop", None);
    arm.reached(1);
    drop(arm);
    goes_on(&first, "ticket 0 after the drop");
    goes_on(&second, "ticket 1 after the drop");
    goes_on(&arrives("t.drop", None), "an arrival after the drop");
}

#[test]
fn dropping_an_arm_while_the_test_panics_does_not_abort() {
    let result = catch_unwind(|| {
        let _arm = Holds::here().arm("t.panicking").give_up_after(Duration::from_millis(100)).hold(1);
        let gone = arrives("t.panicking", None);
        goes_on(&gone, "after its give-up");
        panic!("the test's own failure");
    });
    assert_eq!(panic_text(result), "the test's own failure");
}

#[test]
fn a_second_arm_of_a_point_waits_for_the_first() {
    let first = Holds::here().arm("t.second").hold(1);
    let installed = Arc::new(AtomicBool::new(false));
    let flag = installed.clone();
    let (made, second) = mpsc::channel();
    std::thread::spawn(move || {
        let arm = Holds::here().arm("t.second").hold(0);
        flag.store(true, Ordering::SeqCst);
        made.send(arm).ok();
    });
    let queued = std::time::Instant::now();
    while waiting_arms() == 0 {
        assert!(queued.elapsed() < SEEN, "the second arm never queued behind the first");
        std::thread::yield_now();
    }
    let held = arrives("t.second", None);
    first.reached(0);
    assert!(!installed.load(Ordering::SeqCst), "the second arm was installed while the first lived");
    drop(first);
    goes_on(&held, "released by the first arm's drop");
    let _second = second.recv_timeout(SEEN).expect("the second arm after the first's drop");
}

#[tokio::test(flavor = "current_thread")]
async fn an_async_point_never_blocks_its_runtime_thread() {
    let arm = Holds::here().arm("t.async.thread").hold(1);
    let task = tokio::spawn(async {
        crate::hold_point_async!("t.async.thread");
    });
    arm.reached_async(0).await;
    assert!(!task.is_finished());
    arm.release(0);
    task.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_async_point_holds_under_paused_time() {
    let arm = Holds::here().arm("t.async.paused").give_up_after(Duration::from_millis(300)).hold(1);
    let task = tokio::spawn(async {
        crate::hold_point_async!("t.async.paused", "key");
    });
    // On paused time this timer fires the moment the runtime goes idle; it must not while the arrival is held.
    let ended = tokio::time::timeout(Duration::from_secs(3600), task).await;
    assert!(ended.is_ok(), "paused time advanced past a held async arrival");
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| drop(arm))));
    assert!(text.contains("t.async.paused"), "the arrival was held until its own bound: {text}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_sync_point_in_a_task_on_a_current_thread_runtime_fails_the_arm_by_name() {
    let arm = Holds::here().arm("t.sync.task").give_up_after(Duration::from_millis(200)).hold(1);
    let task = tokio::spawn(async {
        crate::hold_point!("t.sync.task");
    });
    // The task holds the runtime's only thread until its give-up, so this resumes after it.
    arm.reached_async(0).await;
    task.await.expect("the product side never panics");
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| drop(arm))));
    assert!(text.contains("t.sync.task") && text.contains("hold_point_async!"), "{text}");
}

#[test]
fn every_call_of_an_arm_that_gave_up_fails_by_name() {
    let arm = Holds::here().arm("t.gaveup.calls").give_up_after(Duration::from_millis(100)).hold(1);
    goes_on(&arrives("t.gaveup.calls", None), "after its give-up");
    for (call, text) in [
        ("reached", panic_text(catch_unwind(AssertUnwindSafe(|| arm.reached(0))))),
        ("release", panic_text(catch_unwind(AssertUnwindSafe(|| arm.release(0))))),
        ("arrivals", panic_text(catch_unwind(AssertUnwindSafe(|| { arm.arrivals(); })))),
    ] {
        assert!(text.contains("t.gaveup.calls") && text.contains("went on"), "{call}: {text}");
    }
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| drop(arm))));
    assert!(text.contains("t.gaveup.calls"), "{text}");
}

#[test]
fn a_bound_longer_than_the_tools_own_is_refused() {
    let spec = Holds::here().arm("t.bounds");
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| drop(spec.give_up_after(Duration::MAX)))));
    assert!(text.contains("give_up_after") && text.contains("t.bounds"), "{text}");
    let arm = Holds::here().arm("t.bounds").hold(0);
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| arm.reached_within(0, WAIT + Duration::from_secs(1)))));
    assert!(text.contains("reached_within") && text.contains("t.bounds"), "{text}");
}

/// Each arm has its own wake, and a missing notify is only a wait to the bound (the wait re-reads the table when it
/// times out), so the test reads the wait's length: `reached` parks first (its arrival waits on a signal sent just
/// before it) and must be woken well inside its bound.
#[test]
fn an_arrival_wakes_the_reached_that_waits_for_it() {
    const BOUND: Duration = Duration::from_secs(4);
    let arm = Holds::here().arm("t.wake.claim").hold(0);
    for ticket in 0..3 {
        let (go, wait) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            wait.recv().ok();
            arrive("t.wake.claim", None);
        });
        let started = std::time::Instant::now();
        go.send(()).unwrap();
        arm.reached_within(ticket, BOUND);
        assert!(started.elapsed() < BOUND / 2, "reached woke only at its bound: {:?}", started.elapsed());
    }
}

#[test]
fn a_drop_wakes_every_arrival_held_at_the_point() {
    for _ in 0..5 {
        let arm = Holds::here().arm("t.wake.drop").hold(1);
        let gone = arrives("t.wake.drop", None);
        arm.reached(0);
        drop(arm);
        goes_on(&gone, "after the drop");
    }
}
