//! The pen, the ground-gated attach and snapshot slot, and checkpoint streaming tests.

use super::*;
use super::support_tests::*;
use crate::lane::wire::encode_attach_client;

// -- the pen --------------------------------------------------------

#[test]
fn take_demotes_the_previous_driver_which_stays_a_watcher() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    attach_to_done(&mut p, 2, now);

    drive_take(&mut p, 1, "alice", 1, now);
    drive_take(&mut p, 2, "bob", 2, now);

    // conn 1 can no longer resize (not the driver any more).
    let resize = decode_one(&encode_attach_client(&AttachClient::Resize { cols: 100, rows: 40 }).unwrap());
    let a = p.frame(1, resize, now);
    let decoded = decode_one(a[0].send_bytes());
    assert_eq!(
        decoded,
        DecodedFrame::AttachServer(AttachServer::ResizeRefused {
            reason: ResizeRefusedReason::NotDriver
        })
    );
    // conn 1 is still a subscriber: output still reaches it.
    p.sent(1, a[0].send_marker(), now);
    let out = p.output_committed(b"hello", now);
    assert!(out.iter().any(|a| matches!(a, Action::Send { conn: 1, .. })));
}

#[test]
fn driver_eof_clears_the_capability_only() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    drive_take(&mut p, 1, "alice", 1, now);

    p.connection_closed(1, now);
    // No durable action is emitted for the EOF -- capability-only.
    // A NEW connection taking (as the very first ever driver) succeeds
    // without any special-casing, proving no stale state lingered.
    attach_to_done(&mut p, 2, now);
    let a = p.frame(2, take_frame("carol"), now);
    assert!(matches!(a.as_slice(), [Action::CommitTake { conn: 2, controller_id, .. }] if controller_id == "carol"));
}

#[test]
fn stale_input_from_a_non_driver_connection_is_flagged_unauthorized() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    attach_to_done(&mut p, 2, now);
    drive_take(&mut p, 1, "alice", 1, now);

    // conn 2 was never granted the capability; even claiming the SAME
    // (controller_id, take_epoch) as the current driver must not be
    // trusted purely from the wire fields -- the connection itself must
    // hold the capability.
    let input = decode_one(
        &encode_attach_client(&AttachClient::Input {
            controller_id: "alice".into(),
            take_epoch: 1,
            idem_key: [7u8; 16],
            payload: b"ls\n".to_vec(),
        })
        .unwrap(),
    );
    let a = p.frame(2, input, now);
    match a.as_slice() {
        [Action::ForwardInput {
            conn,
            controller_id,
            take_epoch,
            idem_key,
            payload,
            connection_authorized,
            request_id: _,
        }] => {
            assert_eq!(*conn, 2);
            assert_eq!(controller_id, "alice");
            assert_eq!(*take_epoch, 1);
            assert_eq!(*idem_key, [7u8; 16]);
            assert_eq!(payload, b"ls\n");
            assert!(!connection_authorized);
        }
        other => panic!("expected ForwardInput: {other:?}"),
    }
}

#[test]
fn take_before_attach_is_not_attached() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let a = p.frame(1, hello_frame(), now);
    p.sent(1, a[0].send_marker(), now);
    let a = p.frame(1, take_frame("alice"), now);
    let decoded = decode_one(a[0].send_bytes());
    assert_eq!(
        decoded,
        DecodedFrame::AttachServer(AttachServer::TakeRefused {
            reason: TakeRefusedReason::NotAttached
        })
    );
}

#[test]
fn take_while_own_checkpoint_in_flight_is_refused() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let a = p.frame(1, hello_frame(), now);
    p.sent(1, a[0].send_marker(), now);
    p.frame(1, attach_frame("alice"), now);
    p.ground_reached(now);
    let big = vec![0u8; wire::MAX_CHECKPOINT_CHUNK_PAYLOAD + 1]; // 2 chunks
    let chunk1 = p.checkpoint_ready(1, big, now);
    assert_eq!(chunk1.len(), 1);
    assert!(matches!(
        chunk1[0].send_marker(),
        Some(SentMarker::CheckpointChunk { clears_request: Some(_), is_last: false })
    ));
    // The FIRST chunk's completion clears lockstep (the attach success
    // signal) and requests the SECOND (last) chunk.
    let chunk2 = p.sent(1, chunk1[0].send_marker(), now);
    assert_eq!(chunk2.len(), 1);
    assert!(matches!(
        chunk2[0].send_marker(),
        Some(SentMarker::CheckpointChunk { clears_request: None, is_last: true })
    ));

    // `take` is legal to send now (lockstep already cleared), but the
    // transfer is not Done (chunk2 not yet confirmed sent).
    let a = p.frame(1, take_frame("alice"), now);
    let decoded = decode_one(a[0].send_bytes());
    assert_eq!(
        decoded,
        DecodedFrame::AttachServer(AttachServer::TakeRefused {
            reason: TakeRefusedReason::CheckpointInFlight
        })
    );
    // Report THIS refusal's own reply sent (clearing its lockstep)
    // before trying again.
    p.sent(1, a[0].send_marker(), now);
    // Once the final checkpoint chunk is ALSO reported sent, take
    // succeeds.
    p.sent(1, chunk2[0].send_marker(), now);
    let a = p.frame(1, take_frame("alice"), now);
    assert!(matches!(a.as_slice(), [Action::CommitTake { conn: 1, controller_id, .. }] if controller_id == "alice"));
}

// -- attach: ground gate + snapshot slot -----------------------------

#[test]
fn attach_pends_for_ground_then_begins_checkpoint() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let a = p.frame(1, hello_frame(), now);
    p.sent(1, a[0].send_marker(), now);
    let a = p.frame(1, attach_frame("alice"), now);
    assert!(a.is_empty(), "must pend, not reply, before ground: {a:?}");
    let a = p.ground_reached(now);
    assert_eq!(a, vec![Action::BeginCheckpoint { conn: 1 }]);
}

#[test]
fn attach_pends_for_the_snapshot_slot_behind_another_attach() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let a = p.frame(1, hello_frame(), now);
    p.sent(1, a[0].send_marker(), now);
    p.frame(1, attach_frame("alice"), now); // takes the slot

    p.connection_opened(2, now);
    let a = p.frame(2, hello_frame(), now);
    p.sent(2, a[0].send_marker(), now);
    let a = p.frame(2, attach_frame("bob"), now);
    assert!(a.is_empty(), "queued, no reply yet: {a:?}");
    // Ground now reached: only conn 1 (the slot holder) may proceed.
    let a = p.ground_reached(now);
    assert_eq!(a, vec![Action::BeginCheckpoint { conn: 1 }]);
    drive_checkpoint_to_done(&mut p, 1, vec![0xAB], now);
    // conn 1's slot is freed and conn 2 is promoted -- but still needs
    // its OWN ground_reached call to actually begin.
    let a = p.ground_reached(now);
    assert_eq!(a, vec![Action::BeginCheckpoint { conn: 2 }]);
}

/// Real CI failure (windows-latest): attaching to an ALREADY-idle,
/// already-at-ground session (a shell sitting at its prompt -- THE
/// ordinary attach) pended for the full 5 s `GroundTimeout` instead of
/// completing immediately, because the wiring only ever called
/// `ground_reached` from fresh-output-triggered paths. This pins the
/// MACHINE-level half of the fix: `ground_gate_pending()` must report
/// an admitted attach as awaiting ground the instant it is admitted
/// (so the wiring's own eager check has something to act on), and
/// `ground_reached` must complete it from THAT SAME state with no
/// further event -- no `tick`, no `output_committed`, no time advance
/// beyond the admission step itself.
#[test]
fn attach_admitted_at_ground_can_begin_checkpoint_immediately_no_tick_needed() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let a = p.frame(1, hello_frame(), now);
    p.sent(1, a[0].send_marker(), now);
    let a = p.frame(1, attach_frame("alice"), now);
    assert!(a.is_empty(), "attach itself never begins the checkpoint directly: {a:?}");
    assert!(
        p.ground_gate_pending(),
        "an admitted attach must be reported as awaiting ground immediately"
    );
    // No tick, no output_committed, no time advance -- exactly what
    // the wiring's eager check now calls at admission time.
    let a = p.ground_reached(now);
    assert_eq!(a, vec![Action::BeginCheckpoint { conn: 1 }]);
}

/// The negative case `ground_gate_pending` must also get right: once
/// nothing is actually awaiting ground (no attach ever happened, or
/// the one that did already finished), the eager wiring check must
/// cost nothing and do nothing -- it must not report pending forever.
#[test]
fn ground_gate_pending_is_false_with_nothing_awaiting_ground() {
    let mut p = proto();
    let now = t0();
    assert!(!p.ground_gate_pending(), "nothing has ever attached");
    p.connection_opened(1, now);
    let a = p.frame(1, hello_frame(), now);
    p.sent(1, a[0].send_marker(), now);
    assert!(!p.ground_gate_pending(), "hello alone never awaits ground");
    p.frame(1, attach_frame("alice"), now);
    assert!(p.ground_gate_pending(), "the attach just admitted must be reported as pending");
    drive_checkpoint_to_done(&mut p, 1, vec![0xAB], now);
    assert!(!p.ground_gate_pending(), "once Done, nothing is awaiting ground any longer");
}

#[test]
fn attach_ground_timeout_is_retryable() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let a = p.frame(1, hello_frame(), now);
    p.sent(1, a[0].send_marker(), now);
    p.frame(1, attach_frame("alice"), now);

    let later = now + Duration::from_secs(6);
    let a = p.tick(later);
    assert!(a
        .iter()
        .any(|x| matches!(x, Action::Send { marker: Some(SentMarker::Reply { .. }), .. })));
    let send = a.iter().find_map(|x| match x {
        Action::Send { frame_bytes, marker, .. } => Some((frame_bytes.clone(), marker.clone())),
        _ => None,
    });
    let (bytes, marker) = send.unwrap();
    let decoded = decode_one(&bytes);
    assert_eq!(
        decoded,
        DecodedFrame::AttachServer(AttachServer::AttachRefused {
            reason: AttachRefusedReason::GroundTimeout
        })
    );
    p.sent(1, marker, later);

    // Retry succeeds: the connection reverted, not lost.
    let a = p.frame(1, attach_frame("alice"), later);
    assert!(a.is_empty());
    let a = p.ground_reached(later);
    assert_eq!(a, vec![Action::BeginCheckpoint { conn: 1 }]);
}

// -- checkpoint streaming + output queue-behind (findings 3, 10) -----

#[test]
fn checkpoint_streams_one_chunk_at_a_time_not_all_up_front() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let a = p.frame(1, hello_frame(), now);
    p.sent(1, a[0].send_marker(), now);
    p.frame(1, attach_frame("alice"), now);
    p.ground_reached(now);
    let big = vec![0u8; wire::MAX_CHECKPOINT_CHUNK_PAYLOAD * 3]; // 3 chunks
    let chunk1 = p.checkpoint_ready(1, big, now);
    assert_eq!(chunk1.len(), 1, "must emit exactly one chunk per step, not all up front");
    assert!(matches!(
        chunk1[0].send_marker(),
        Some(SentMarker::CheckpointChunk { is_last: false, .. })
    ));
    let chunk2 = p.sent(1, chunk1[0].send_marker(), now);
    assert_eq!(chunk2.len(), 1);
    assert!(matches!(
        chunk2[0].send_marker(),
        Some(SentMarker::CheckpointChunk { is_last: false, .. })
    ));
    let chunk3 = p.sent(1, chunk2[0].send_marker(), now);
    assert_eq!(chunk3.len(), 1);
    assert!(matches!(
        chunk3[0].send_marker(),
        Some(SentMarker::CheckpointChunk { is_last: true, .. })
    ));
}

#[test]
fn output_queues_behind_an_in_flight_checkpoint_and_flushes_once_done() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let a = p.frame(1, hello_frame(), now);
    p.sent(1, a[0].send_marker(), now);
    p.frame(1, attach_frame("alice"), now);
    p.ground_reached(now);
    let big = vec![0u8; wire::MAX_CHECKPOINT_CHUNK_PAYLOAD + 1]; // 2 chunks
    let chunk1 = p.checkpoint_ready(1, big, now);

    // Output committed WHILE still mid-transfer must not be dropped.
    let a = p.output_committed(b"queued-behind", now);
    assert!(a.is_empty(), "no Output send yet -- not Done: {a:?}");

    let chunk2 = p.sent(1, chunk1[0].send_marker(), now);
    assert!(matches!(
        chunk2[0].send_marker(),
        Some(SentMarker::CheckpointChunk { is_last: true, .. })
    ));
    let flushed = p.sent(1, chunk2[0].send_marker(), now);
    assert_eq!(flushed.len(), 1, "the queued output must flush exactly once Done: {flushed:?}");
    let decoded = decode_one(flushed[0].send_bytes());
    assert_eq!(
        decoded,
        DecodedFrame::AttachServer(AttachServer::Output { bytes: b"queued-behind".to_vec() })
    );
}

/// Real CI failure (windows-2022, PR #139 discharge round): the
/// rebuilt fidelity test found the wire checkpoint diverging from an
/// independently computed reference of the same prefix. Root cause —
/// bytes committed WHILE a watcher is still `QueuedForSlot`/
/// `AwaitingGround` (before its turn) queue into
/// `pending_post_watermark` the same as any other pre-`Done` output
/// (finding 3's own `Some(_) => queue` arm doesn't distinguish the
/// two) — but the live parser that produces the checkpoint at
/// `checkpoint_ready` time has, by construction, ALREADY consumed
/// everything ever published to this connection up to and including
/// that exact commit round (`capsule_win.rs`'s watermark barrier:
/// fsync -> publish -> checkpoint, one loop step, in that order — the
/// barrier's own ordering was never the bug). Left in the queue, that
/// backlog is a duplicate of what the checkpoint already encodes, and
/// got redelivered a SECOND time once `Done`. Fixed by purging
/// `pending_post_watermark` inside `checkpoint_ready` itself, the one
/// moment the cut point and the queue are both in scope.
#[test]
fn output_committed_before_the_checkpoint_is_taken_is_not_redelivered_after_it() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let a = p.frame(1, hello_frame(), now);
    p.sent(1, a[0].send_marker(), now);
    p.frame(1, attach_frame("alice"), now);

    // Committed WHILE still AwaitingGround -- queues, exactly like any
    // other pre-Done output (finding 3).
    let a = p.output_committed(b"already-in-the-checkpoint", now);
    assert!(a.is_empty(), "no Output send yet -- not Done: {a:?}");

    // Ground reached: the real loop would encode the checkpoint from a
    // live parser that has ALREADY processed the bytes above -- the
    // checkpoint bytes below stand in for a snapshot that already
    // covers "already-in-the-checkpoint".
    let a = p.ground_reached(now);
    assert!(matches!(a.as_slice(), [Action::BeginCheckpoint { conn: c }] if *c == 1));
    let chunk = p.checkpoint_ready(1, b"checkpoint-bytes".to_vec(), now);
    assert!(matches!(
        chunk.as_slice(),
        [Action::Send { marker: Some(SentMarker::CheckpointChunk { is_last: true, .. }), .. }]
    ));

    // Genuinely NEW output, committed AFTER the checkpoint was taken,
    // still queues behind the (still in-flight, one-chunk) transfer
    // normally.
    let a = p.output_committed(b"after-the-checkpoint", now);
    assert!(a.is_empty());

    let flushed = p.sent(1, chunk[0].send_marker(), now);
    assert_eq!(
        flushed.len(),
        1,
        "exactly ONE queued output frame must flush once Done -- the pre-checkpoint backlog must have been purged: {flushed:?}"
    );
    let decoded = decode_one(flushed[0].send_bytes());
    assert_eq!(
        decoded,
        DecodedFrame::AttachServer(AttachServer::Output { bytes: b"after-the-checkpoint".to_vec() }),
        "only output committed AFTER the checkpoint was taken may be redelivered"
    );
}

/// Round-2 review, finding 1 (reproduced by the reviewer's own scratch
/// probe): clearing `pending_post_watermark` at capture retired the
/// BYTES but not the `queued_live_bytes` CHARGE those same bytes had
/// already added -- only an `OutputBytes` `Sent` completion ever
/// decremented it, and the cleared vectors will never produce one.
/// Queue right up to the 4 MiB ceiling pre-capture, capture (which
/// must release that exact charge), then commit one more byte: it
/// must NOT overflow, because nothing is actually outstanding anymore.
#[test]
fn checkpoint_capture_releases_the_cleared_backlogs_own_queue_charge() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let a = p.frame(1, hello_frame(), now);
    p.sent(1, a[0].send_marker(), now);
    p.frame(1, attach_frame("alice"), now);

    // Queue EXACTLY the budget ceiling while still AwaitingGround.
    let chunk = vec![0xAAu8; WATCHER_LIVE_QUEUE_BUDGET_BYTES as usize];
    let a = p.output_committed(&chunk, now);
    assert!(a.is_empty(), "at the ceiling, not over it: {a:?}");

    let a = p.ground_reached(now);
    assert!(matches!(a.as_slice(), [Action::BeginCheckpoint { conn: c }] if *c == 1));
    let a = p.checkpoint_ready(1, b"checkpoint-bytes".to_vec(), now);
    assert!(matches!(
        a.as_slice(),
        [Action::Send { marker: Some(SentMarker::CheckpointChunk { is_last: true, .. }), .. }]
    ));

    // One more byte, post-capture: must not overflow -- the pre-
    // capture backlog's charge was released atomically with the
    // capture that cleared its bytes.
    let a = p.output_committed(b"x", now);
    assert!(
        !a.iter().any(|x| matches!(x, Action::Close(_) | Action::RecordRefusal { .. })),
        "a false eviction one byte after capture: {a:?}"
    );
}

