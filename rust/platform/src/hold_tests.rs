//! The hold tool, in one process. Each test names its own point: an arm is exclusive per point, and the tests run
//! beside each other. Every wait here is a condition with a stated bound that fails with what it saw.

use super::*;
use crate::hold::{arrive, waiting_arms};
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
        arrive(point, key);
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
    assert!(text.contains("t.giveup") && text.contains("[0]"), "{text}");
}

#[test]
fn a_release_that_comes_first_beats_the_give_up() {
    let arm = Holds::here().arm("t.race").give_up_after(Duration::from_millis(200)).hold(1);
    let gone = arrives("t.race", None);
    arm.reached(0);
    arm.release(0);
    goes_on(&gone, "released");
    drop(arm);
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
    while waiting_arms("t.second") == 0 {
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
async fn a_sync_reached_on_a_current_thread_runtime_is_refused() {
    let arm = Holds::here().arm("t.sync.reached").hold(1);
    let text = panic_text(catch_unwind(AssertUnwindSafe(|| arm.reached(0))));
    assert!(text.contains("reached_async"), "{text}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_sync_point_held_on_a_current_thread_runtime_is_refused_but_not_on_its_blocking_thread() {
    let arm = Holds::here().arm("t.sync.held").hold(2);
    let text = panic_text(catch_unwind(|| crate::hold_point!("t.sync.held")));
    assert!(text.contains("hold_point_async!"), "{text}");
    let blocking = tokio::task::spawn_blocking(|| crate::hold_point!("t.sync.held"));
    arm.reached_async(1).await;
    arm.release(1);
    blocking.await.unwrap();
    assert_eq!(arm.arrivals(), 2);
}
