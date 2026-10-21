use super::*;

// ---- (b) TakeTransaction ------------------------------------------

#[test]
fn first_input_while_watching_sends_take_and_queues() {
    let mut t = TakeTransaction::new();
    let actions = t.on_input_while_watching(b"hello");
    assert_eq!(actions, vec![TakeAction::SendTake]);
    assert_eq!(t.role(), Role::Taking);
}

/// Codex review round, finding 3: `take_ok` must send ONLY resize —
/// the queue is released ONLY after `resize_ok`, never bundled into
/// the same action batch (the wire allows exactly one outstanding
/// request; a bundled `SendInput` would violate lockstep the moment
/// a real transport interleaves it before `resize`'s own reply).
#[test]
fn take_ok_sends_only_resize_never_input() {
    let mut t = TakeTransaction::new();
    t.on_input_while_watching(b"ab");
    t.on_input_while_pending(b"cd");
    let actions = t.on_take_ok(80, 24);
    assert_eq!(actions, vec![TakeAction::SendResize { cols: 80, rows: 24 }]);
    assert_eq!(t.role(), Role::Resizing);
    // The queue is untouched -- take_queued is not callable yet
    // (still RESIZING), proving the bytes were not silently flushed.
}

/// ADR 0042 amendment (2026-09-07): a headless client's take sends NO
/// resize, ever — `take_ok` promotes straight to DRIVING (skipping
/// RESIZING) and returns no actions at all, so the wire `take` frame
/// really is the last thing this transaction ever sends.
#[test]
fn headless_take_ok_sends_no_resize_and_promotes_straight_to_driving() {
    let mut t = TakeTransaction::new_headless();
    t.on_input_while_watching(b"ab");
    let actions = t.on_take_ok(80, 24);
    assert_eq!(actions, Vec::<TakeAction>::new());
    assert_eq!(t.role(), Role::Driving);
    // The queue is immediately drainable — no `resize_ok` needed.
    assert_eq!(t.take_queued(), Some(b"ab".to_vec()));
}

#[test]
fn ordinary_take_ok_is_unaffected_by_the_headless_constructor_existing() {
    // Guards against the headless flag leaking into `new()`'s default.
    let mut t = TakeTransaction::new();
    t.on_input_while_watching(b"ab");
    let actions = t.on_take_ok(80, 24);
    assert_eq!(actions, vec![TakeAction::SendResize { cols: 80, rows: 24 }]);
    assert_eq!(t.role(), Role::Resizing);
}

#[test]
fn resize_ok_promotes_to_driving_and_releases_the_queue() {
    let mut t = TakeTransaction::new();
    t.on_input_while_watching(b"ab");
    t.on_input_while_pending(b"cd");
    t.on_take_ok(80, 24);
    t.on_resize_ok();
    assert_eq!(t.role(), Role::Driving);
    assert_eq!(t.take_queued(), Some(b"abcd".to_vec()));
    // Draining twice returns None -- the queue is truly consumed.
    assert_eq!(t.take_queued(), None);
}

#[test]
fn take_ok_with_empty_queue_then_resize_ok_yields_no_queued_input() {
    // A driver that never typed anything before take_ok.
    let mut t = TakeTransaction::new();
    t.role = Role::Taking; // synthesize: take sent with no queued bytes yet
    let actions = t.on_take_ok(10, 5);
    assert_eq!(actions, vec![TakeAction::SendResize { cols: 10, rows: 5 }]);
    t.on_resize_ok();
    assert_eq!(t.take_queued(), None);
}

#[test]
fn queue_caps_at_8kib_and_discards_visibly() {
    let mut t = TakeTransaction::new();
    let paste = vec![b'x'; TAKE_QUEUE_CAP + 100];
    let actions = t.on_input_while_watching(&paste);
    assert!(actions.contains(&TakeAction::QueueDiscarded));
    t.on_take_ok(80, 24);
    t.on_resize_ok();
    let bytes = t.take_queued().expect("queue had bytes");
    assert_eq!(bytes.len(), TAKE_QUEUE_CAP);
}

#[test]
fn an_input_dropped_whole_on_a_full_queue_is_reported() {
    let mut t = TakeTransaction::new();
    t.on_input_while_watching(&vec![b'a'; TAKE_QUEUE_CAP]);
    let actions = t.on_input_while_pending(b"z");
    assert!(actions.contains(&TakeAction::InputDropped));
    let partly = TakeTransaction::new().on_input_while_watching(&vec![b'x'; TAKE_QUEUE_CAP + 1]);
    assert!(!partly.contains(&TakeAction::InputDropped), "a partly queued input is not a whole drop");
}

#[test]
fn queue_accumulates_across_multiple_calls_up_to_the_cap() {
    let mut t = TakeTransaction::new();
    t.on_input_while_watching(&vec![b'a'; TAKE_QUEUE_CAP - 10]);
    let overflow_actions = t.on_input_while_pending(&[b'b'; 50]);
    assert!(overflow_actions.contains(&TakeAction::QueueDiscarded));
    t.on_take_ok(80, 24);
    t.on_resize_ok();
    let bytes = t.take_queued().expect("queue had bytes");
    assert_eq!(bytes.len(), TAKE_QUEUE_CAP);
}

/// Codex review round, finding 5: driving input while a request is
/// already outstanding must be queued, not dropped.
#[test]
fn driving_input_while_outstanding_is_queued_not_dropped() {
    let mut t = TakeTransaction::new();
    t.on_input_while_watching(b"x");
    t.on_take_ok(1, 1);
    t.on_resize_ok();
    t.take_queued(); // flush the first byte "x" -- now nothing queued
    let actions = t.queue_while_driving(b"second keystroke");
    assert_eq!(actions, vec![]);
    assert_eq!(t.take_queued(), Some(b"second keystroke".to_vec()));
}

#[test]
fn take_refused_not_attached_leaves_the_queue_to_be_counted_at_the_reset() {
    let mut t = TakeTransaction::new();
    t.on_input_while_watching(b"x");
    t.on_input_while_pending(b"y");
    let actions = t.on_take_refused_not_attached();
    assert_eq!(actions, vec![TakeAction::Reattach]);
    assert_eq!(t.role(), Role::Taking);
    // The reattach resets the transaction and reports both queued inputs.
    assert_eq!(t.reset_to_watching(), 2);
    assert_eq!(t.role(), Role::Watching);
    assert_eq!(t.reset_to_watching(), 0);
}

/// Codex review round, finding 6: a stale-epoch refusal while
/// DRIVING must re-take BEFORE minting a new key (the epoch is only
/// known once the fresh `take_ok` arrives).
#[test]
fn retake_while_driving_transitions_to_taking() {
    let mut t = TakeTransaction::new();
    t.on_input_while_watching(b"x");
    t.on_take_ok(1, 1);
    t.on_resize_ok();
    assert_eq!(t.role(), Role::Driving);
    let actions = t.retake_while_driving();
    assert_eq!(actions, vec![TakeAction::SendTake]);
    assert_eq!(t.role(), Role::Taking);
}

#[test]
fn headless_retake_while_driving_never_re_takes() {
    let mut t = TakeTransaction::new_headless();
    t.on_input_while_watching(b"x");
    t.on_take_ok(1, 1); // headless: promotes straight to Driving
    assert_eq!(t.role(), Role::Driving);
    let actions = t.retake_while_driving();
    assert_eq!(actions, vec![], "a headless transaction never sends a fresh take on its own");
    assert_eq!(t.role(), Role::Watching);
}

#[test]
fn checkpoint_in_flight_gated_at_250ms_then_expires_and_discards() {
    let mut t = TakeTransaction::new();
    t.on_input_while_watching(b"typed-while-checkpoint-in-flight");
    let t0 = Instant::now();
    // First refusal opens the window; no immediate retry action.
    assert_eq!(t.on_take_refused_checkpoint_in_flight(t0), vec![]);
    assert!(t.checkpoint_retry_pending());
    // A tick well before the 250ms gate is a no-op -- Codex review
    // finding 4: the original fired on every call.
    assert_eq!(t.tick_checkpoint_retry(t0 + Duration::from_millis(50)), vec![]);
    assert_eq!(t.tick_checkpoint_retry(t0 + Duration::from_millis(250)), vec![TakeAction::SendTake]);
    // Immediately after that retry, another tick before the NEXT
    // 250ms boundary is again a no-op.
    assert_eq!(t.tick_checkpoint_retry(t0 + Duration::from_millis(300)), vec![]);
    assert_eq!(t.tick_checkpoint_retry(t0 + Duration::from_millis(500)), vec![TakeAction::SendTake]);
    assert_eq!(t.role(), Role::Taking);
    // At/after 30s the queue is discarded and role returns to Watching.
    let expiry = t0 + CHECKPOINT_IN_FLIGHT_BUDGET;
    assert_eq!(t.tick_checkpoint_retry(expiry), vec![TakeAction::QueueDiscarded]);
    assert_eq!(t.role(), Role::Watching);
    assert!(!t.checkpoint_retry_pending());
}

#[test]
fn resize_refused_out_of_budget_keeps_the_pen_and_releases_the_queue() {
    let mut t = TakeTransaction::new();
    t.on_input_while_watching(b"x");
    t.on_take_ok(80, 24);
    assert_eq!(t.role(), Role::Resizing);
    let actions = t.on_resize_refused(ResizeRefusedReason::OutOfBudget);
    assert_eq!(actions, vec![TakeAction::GeometryUnrepresentable]);
    assert_eq!(t.role(), Role::Driving);
    assert_eq!(t.take_queued(), Some(b"x".to_vec()));
}

#[test]
fn resize_refused_not_driver_loses_the_pen() {
    let mut t = TakeTransaction::new();
    t.on_input_while_watching(b"x");
    t.on_take_ok(80, 24);
    let actions = t.on_resize_refused(ResizeRefusedReason::NotDriver);
    assert_eq!(actions, vec![TakeAction::PenLost]);
    assert_eq!(t.role(), Role::Watching);
}
