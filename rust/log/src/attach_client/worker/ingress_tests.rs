//! Tests: Bounded ingress, checkpoint deadlines and queued-byte wakeups.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use super::support_tests::*;

/// An input sent after the worker has exited is never delivered, and
/// is counted.
#[test]
fn an_input_sent_after_the_worker_exited_is_counted() {
    let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();
    drop(msg_rx);
    let worker = AttachWorker::<TestEndpoint> {
        msg_tx,
        ingress_bytes: Arc::new(AtomicUsize::new(0)),
        attach_gen: Arc::new(AtomicU64::new(0)),
        discarded: Arc::new(AtomicUsize::new(0)),
        ingress_bound: 64,
        queued_bytes: Arc::new(QueuedBytes::new()),
        viewed: Arc::new(AtomicBool::new(true)),
        worker_handle: None,
        _endpoint: PhantomData,
    };
    assert_eq!(worker.send_input(b"x".to_vec()), Ok(()));
    assert_eq!(worker.inputs_discarded(), 1);
}

/// The manager's own ruling on top of Codex round 2: the ingress
/// bound limits ACCUMULATION, never a single send. An input bigger
/// than the bound is admitted outright when nothing else is queued
/// (a large paste, or a long `sot-fe type --stdin`, must never
/// silently vanish just for being bigger than a bound sized for
/// steady-state typing); once it is sitting in the channel
/// un-consumed, a second send that would push the total past the
/// bound is refused, and draining the first (releasing its
/// reservation, exactly as the worker's own loop would when it pops
/// the message) makes the bound honest again.
#[test]
fn an_oversize_input_is_admitted_alone_but_blocks_further_sends_until_drained() {
    let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();
    let worker = AttachWorker::<TestEndpoint> {
        msg_tx,
        ingress_bytes: Arc::new(AtomicUsize::new(0)),
        attach_gen: Arc::new(AtomicU64::new(0)),
        discarded: Arc::new(AtomicUsize::new(0)),
        ingress_bound: 64,
        queued_bytes: Arc::new(QueuedBytes::new()),
        viewed: Arc::new(AtomicBool::new(true)),
        worker_handle: None,
        _endpoint: PhantomData,
    };

    // Bigger than the 64-byte bound, but the queue is idle -- admitted.
    assert_eq!(
        worker.send_input(vec![0u8; 100]),
        Ok(()),
        "a single oversize input must be admitted when nothing else is queued"
    );

    // Nothing has drained the channel yet, so the first reservation
    // is still outstanding: a second send is refused, even a tiny one.
    assert_eq!(
        worker.send_input(vec![0u8; 1]),
        Err(IngressRefused),
        "a second send while the first is still queued must be refused"
    );
    assert_eq!(worker.discarded.load(Ordering::SeqCst), 1, "a refusal is counted as a discarded input");

    // Draining the channel (as the worker's own loop would, popping
    // the message and letting its reservation drop) releases the
    // first charge.
    match msg_rx.recv().expect("the first send's own message is queued") {
        WorkerMsg::Input(bytes, _reservation, _) => assert_eq!(bytes.len(), 100),
        _ => panic!("expected WorkerMsg::Input"),
    }

    assert_eq!(
        worker.send_input(vec![0u8; 1]),
        Ok(()),
        "the bound must be honest again once the outstanding reservation is released"
    );
}

/// Codex round on #194, finding 3: a per-frame deadline that keeps
/// re-arming itself, forever, is not a bound at all. This proves the
/// clamp both ways -- an aggregate deadline far in the future never
/// shortens an ordinary per-frame budget, and one that has already
/// arrived is what a faulty capsule dripping a technically-legal
/// frame every `STATUS_BUDGET` eventually runs into.
#[test]
fn checkpoint_frame_deadline_never_exceeds_the_aggregate_one() {
    let now = Instant::now();
    let far_off = now + Duration::from_secs(3600);
    assert_eq!(checkpoint_frame_deadline(now, far_off), now + STATUS_BUDGET);

    let already_here = now + Duration::from_millis(1);
    assert_eq!(checkpoint_frame_deadline(now, already_here), already_here);
}

/// Sanity on the constant itself: finite, and generous enough to
/// cover at least one ordinary frame -- a zero or absurdly small
/// budget would defeat its own purpose (refusing a checkpoint that
/// could otherwise complete in time).
#[test]
fn checkpoint_transfer_budget_is_a_real_bound() {
    assert!(CHECKPOINT_TRANSFER_BUDGET >= STATUS_BUDGET);
    assert!(CHECKPOINT_TRANSFER_BUDGET <= Duration::from_secs(300));
}
/// switch-latency Phase 1, via `wait_below_cap_traced`'s wake
/// counter: proves `wait_below_cap` is genuinely NOTIFIED rather than
/// polled, without depending on wall-clock latency (a loose bound
/// would admit the old 20ms poll just as easily as the new wait, and
/// a tight one can miss on a loaded CI runner regardless of which
/// mechanism is really running). The waiter parks for a real 200ms
/// before `sub` drains it below the cap; ideally it wakes exactly
/// twice: once at entry (parks, since the count is still at the cap)
/// and once more when `sub`'s notify runs. A 20ms poll loop, by
/// contrast, would wake roughly 200ms / 20ms ~= 10 times over the
/// same stretch.
#[test]
fn queued_bytes_wait_below_cap_wakes_a_bounded_number_of_times_when_released_by_a_drain() {
    let q = Arc::new(QueuedBytes::new());
    q.add(10);
    let stop = Arc::new(AtomicBool::new(false));
    let wakes = Arc::new(AtomicUsize::new(0));

    let waiter_q = Arc::clone(&q);
    let waiter_stop = Arc::clone(&stop);
    let waiter_wakes = Arc::clone(&wakes);
    let waiter = thread::spawn(move || {
        waiter_q.wait_below_cap_traced(10, &waiter_stop, || {
            waiter_wakes.fetch_add(1, Ordering::SeqCst);
        });
    });

    thread::sleep(Duration::from_millis(200));
    q.sub(1); // 9 < 10: below the cap
    waiter.join().expect("waiter thread must not panic");

    let wake_count = wakes.load(Ordering::SeqCst);
    assert!(
        wake_count <= 6,
        "waiter woke {wake_count} times across a 200ms hold -- expected entry plus one \
         notified wake (2, with slack for a loaded CI runner's own spurious wakeups), not \
         a 20ms poll cadence (which would be ~10)"
    );
}

/// The other release path, proven the same way: `stop` alone (via
/// `notify_stop`) must unblock a waiter that would otherwise stay
/// above the cap forever, and must do so without a poll cadence --
/// this is exactly the mechanism `run_attach_reader`'s own episode
/// teardown depends on, exercised here with NOTHING ever draining
/// the queue (no `FeAttachClient`, no `pump()` call at all): `stop`
/// is the ONLY way out.
#[test]
fn queued_bytes_wait_below_cap_wakes_a_bounded_number_of_times_when_released_by_stop_with_no_drain_ever_happening()
{
    let q = Arc::new(QueuedBytes::new());
    q.add(10); // stays at/above the cap for the whole test -- nothing ever calls sub()
    let stop = Arc::new(AtomicBool::new(false));
    let wakes = Arc::new(AtomicUsize::new(0));

    let waiter_q = Arc::clone(&q);
    let waiter_stop = Arc::clone(&stop);
    let waiter_wakes = Arc::clone(&wakes);
    let waiter = thread::spawn(move || {
        waiter_q.wait_below_cap_traced(10, &waiter_stop, || {
            waiter_wakes.fetch_add(1, Ordering::SeqCst);
        });
    });

    thread::sleep(Duration::from_millis(200));
    stop.store(true, Ordering::Release);
    q.notify_stop();
    waiter.join().expect("waiter thread must not panic");

    let wake_count = wakes.load(Ordering::SeqCst);
    assert!(
        wake_count <= 6,
        "waiter woke {wake_count} times across a 200ms hold -- expected entry plus one \
         notified wake (2, with slack for a loaded CI runner's own spurious wakeups), not \
         a 20ms poll cadence (which would be ~10)"
    );
}
