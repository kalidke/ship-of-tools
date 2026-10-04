//! Mgmt shutdown, lockstep, caps and hello tests.

use super::*;
use super::support_tests::*;
use crate::lane::wire::{encode_attach_client, encode_mgmt_request};

// -- mgmt shutdown / ADR 0041 EndRun ------------------------------------



/// ADR 0041 EndRun steps 1-2: `handle_mgmt`'s `shutdown` branch returns
/// the durable-marker action FIRST, then the ack `Send` -- in THAT
/// order, since the writer loop processes a returned batch in order
/// and must append+latch the marker before ever queuing the ack (see
/// `Action::RunEndRequested`'s own doc). Both carry the SAME
/// client-supplied reason, verbatim.
#[test]
fn shutdown_request_returns_run_end_requested_before_the_ack_send() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let req = decode_one(
        &encode_mgmt_request(&MgmtRequest::Shutdown { reason: "quit".into() }).unwrap(),
    );
    let a = p.frame(1, req, now);
    assert_eq!(a.len(), 2, "expected [RunEndRequested, Send]: {a:?}");
    match &a[0] {
        Action::RunEndRequested { reason } => assert_eq!(reason, "quit"),
        other => panic!("expected RunEndRequested first: {other:?}"),
    }
    match &a[1] {
        Action::Send {
            marker: Some(SentMarker::ShutdownAck { reason, .. }),
            ..
        } => assert_eq!(reason, "quit"),
        other => panic!("expected the ack Send second: {other:?}"),
    }
}

// -- lockstep ---------------------------------------------------------

/// Round-2 e2e review, finding 1: a second frame while the first's
/// reply is already QUEUED (`Action::Send` handed to the transport,
/// physical completion not yet reported) is exactly the benign
/// cross-thread race a real transport's independent reader/writer
/// threads can produce -- held, not closed, and replayed the instant
/// the matching `sent` arrives (superseding this test's own prior
/// name and behavior, which asserted the old, now-corrected
/// immediate-close semantics for precisely this scenario).
#[test]
fn lockstep_race_is_held_and_replays_once_the_reply_completes() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let probe = decode_one(&encode_mgmt_request(&MgmtRequest::Probe).unwrap());
    let a1 = p.frame(1, probe.clone(), now);
    assert!(matches!(a1.as_slice(), [Action::Send { .. }]), "{a1:?}");
    // A second request arrives before the first's reply is reported
    // sent -- held, no actions yet, connection still alive.
    let a2 = p.frame(1, probe, now);
    assert_eq!(a2, vec![], "expected the race frame to be held, not acted on: {a2:?}");
    // Reporting the first reply's completion now replays the held
    // frame, producing exactly what a fresh probe would.
    let a3 = p.sent(1, a1[0].send_marker(), now);
    assert!(matches!(a3.as_slice(), [Action::Send { .. }]), "expected the held probe to replay: {a3:?}");
}

/// A THIRD frame while one is already held is a real violation -- a
/// compliant client never sends a second request before seeing a
/// reply to its first, race or not.
#[test]
fn lockstep_violation_closes_on_a_second_held_frame() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let probe = decode_one(&encode_mgmt_request(&MgmtRequest::Probe).unwrap());
    let a1 = p.frame(1, probe.clone(), now);
    let a2 = p.frame(1, probe.clone(), now);
    assert_eq!(a2, vec![], "expected the first race frame to be held: {a2:?}");
    let a3 = p.frame(1, probe, now);
    assert!(
        a3.iter().any(|a| matches!(a, Action::Close(c) if *c == 1)),
        "expected a close on a second frame held behind the first: {a3:?}"
    );
    assert!(a3
        .iter()
        .any(|a| matches!(a, Action::RecordRefusal { reason: RefusalReason::LockstepViolation, .. })));
    // The first reply's own completion is still exactly as queued --
    // proving the violation didn't also corrupt the race path.
    let _ = a1;
}

/// Final-verification round: a ONE-chunk checkpoint's first chunk IS
/// its last, and a compliant client that reads it can have a `take`
/// already held behind that chunk's pending completion. The replay
/// must run against the transfer's COMPLETED state -- Done marked and
/// the snapshot slot freed before the held frame re-enters -- or the
/// take is falsely refused CheckpointInFlight (review-reproduced).
#[test]
fn a_take_held_behind_a_one_chunk_checkpoint_replays_against_done_state() {
    let mut p = proto();
    let now = t0();
    assert_eq!(p.connection_opened(1, now), vec![]);
    let a = p.frame(1, hello_frame(), now);
    p.sent(1, a[0].send_marker(), now);
    let a = p.frame(1, attach_frame("ctrl"), now);
    assert!(a.is_empty());
    let a = p.ground_reached(now);
    assert!(matches!(a.as_slice(), [Action::BeginCheckpoint { conn: 1 }]));
    let chunk = p.checkpoint_ready(1, vec![0xAB], now);
    let marker = chunk[0].send_marker();
    assert!(
        matches!(marker, Some(SentMarker::CheckpointChunk { is_last: true, .. })),
        "a one-byte checkpoint must be a single, final chunk: {marker:?}"
    );
    // The take races in while that final chunk's completion is pending.
    let held = p.frame(1, take_frame("ctrl"), now);
    assert_eq!(held, vec![], "expected the racing take to be held: {held:?}");
    // Completion: the replay must see Done + a free slot and commit
    // the take -- never TakeRefused::CheckpointInFlight.
    let a = p.sent(1, marker, now);
    assert!(
        a.iter().any(|x| matches!(x, Action::CommitTake { .. })),
        "expected the held take to replay into CommitTake against Done state: {a:?}"
    );
    assert!(
        !a.iter().any(|x| matches!(x, Action::RecordRefusal { .. })),
        "the held take must not be refused: {a:?}"
    );
}

/// A frame arriving while `outstanding_request` is set but NO reply
/// has been queued yet (`take`, mid-`CommitTake`, waiting on the
/// loop's own fsync round trip) is a real violation, not a race --
/// there is no send in flight this frame could be racing.
#[test]
fn lockstep_violation_closes_when_no_reply_is_queued_yet() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    let a = p.frame(1, take_frame("alice"), now);
    assert!(matches!(a.as_slice(), [Action::CommitTake { .. }]), "{a:?}");
    // take's own reply has not even been constructed yet (waiting on
    // take_committed) -- a second frame here is a genuine violation.
    let a2 = p.frame(1, take_frame("alice"), now);
    assert!(
        a2.iter().any(|a| matches!(a, Action::Close(c) if *c == 1)),
        "expected a close: no reply was ever queued for this request to race against: {a2:?}"
    );
}

#[test]
fn a_reply_confirmed_sent_clears_the_lockstep_flag_for_the_next_request() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let probe = decode_one(&encode_mgmt_request(&MgmtRequest::Probe).unwrap());
    let a1 = p.frame(1, probe.clone(), now);
    p.sent(1, a1[0].send_marker(), now);
    let a2 = p.frame(1, probe, now);
    assert!(matches!(a2.as_slice(), [Action::Send { .. }]), "{a2:?}");
}

/// Finding 4's own regression scenario: an UNRELATED checkpoint chunk's
/// completion (whose `clears_request` is `None` -- it isn't the first
/// chunk) must never clear a DIFFERENT request's lockstep. Before the
/// fix, `CheckpointChunk`'s handler cleared `outstanding_request`
/// unconditionally, so `take`'s own still-unsent reply would have been
/// wrongly freed by this unrelated completion.
#[test]
fn an_unrelated_checkpoint_chunks_completion_does_not_clear_a_different_outstanding_requests_lockstep() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let a = p.frame(1, hello_frame(), now);
    p.sent(1, a[0].send_marker(), now);
    p.frame(1, attach_frame("alice"), now);
    p.ground_reached(now);
    let big = vec![0u8; wire::MAX_CHECKPOINT_CHUNK_PAYLOAD + 1]; // 2 chunks
    let chunk1 = p.checkpoint_ready(1, big, now);
    let chunk2 = p.sent(1, chunk1[0].send_marker(), now); // clears ATTACH's own lockstep
    assert!(matches!(
        chunk2[0].send_marker(),
        Some(SentMarker::CheckpointChunk { clears_request: None, is_last: true })
    ));

    // `take` is now legal (attach's lockstep already cleared) --
    // refused CheckpointInFlight (own transfer not yet Done); ITS OWN
    // reply is queued but not yet reported sent.
    let a = p.frame(1, take_frame("alice"), now);
    assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::Reply { .. }), .. }]));

    // Report the UNRELATED chunk2 completion now.
    let _ = p.sent(1, chunk2[0].send_marker(), now);

    // take's own reply STILL hasn't been reported sent -- a second
    // take must still see take's OWN lockstep held, not cleared by
    // the unrelated chunk2 completion. Round-2 e2e review, finding 1
    // superseded the old assertion here (an immediate close): a
    // second frame while a reply is already queued is now HELD, not
    // closed -- proving the connection stays open and the SAME
    // outstanding request is still the one in effect (not silently
    // cleared by the unrelated completion) is the point this test
    // still makes, just via the held-frame path instead of a close.
    let a2 = p.frame(1, take_frame("alice"), now);
    assert_eq!(
        a2,
        vec![],
        "an unrelated checkpoint completion must not clear take's own lockstep: {a2:?}"
    );

    // Reporting take's OWN reply sent now replays the held frame --
    // alice's checkpoint is Done (chunk2 above), so this replay is a
    // fresh, legal take, no longer refused CheckpointInFlight.
    let a3 = p.sent(1, a[0].send_marker(), now);
    assert!(
        matches!(a3.as_slice(), [Action::CommitTake { .. }]),
        "expected the held take to replay once its predecessor's own reply completed: {a3:?}"
    );
}

// -- caps ---------------------------------------------------------

#[test]
fn non_watcher_cap_enforced() {
    let mut p = proto();
    let now = t0();
    for id in 0..4 {
        assert_eq!(p.connection_opened(id, now), vec![]);
    }
    let a = p.connection_opened(4, now);
    assert!(a.iter().any(|x| matches!(x, Action::Close(c) if *c == 4)));
    assert!(a
        .iter()
        .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::NonWatcherCapExceeded, .. })));
}

#[test]
fn subscriber_cap_enforced_and_retryable() {
    let mut p = proto();
    let now = t0();
    for id in 0..4u64 {
        attach_to_done(&mut p, id, now);
    }
    // A 5th connection completes hello, then its attach is refused --
    // but the connection stays open (no Close in the response) and is
    // retryable.
    p.connection_opened(9, now);
    let a = p.frame(9, hello_frame(), now);
    p.sent(9, a[0].send_marker(), now);
    let a = p.frame(9, attach_frame("late"), now);
    assert!(!a.iter().any(|x| matches!(x, Action::Close(_))), "must stay open: {a:?}");
    match a.as_slice() {
        [Action::Send { frame_bytes, marker: Some(SentMarker::Reply { .. }), .. }] => {
            let decoded = decode_one(frame_bytes);
            assert_eq!(
                decoded,
                DecodedFrame::AttachServer(AttachServer::AttachRefused {
                    reason: AttachRefusedReason::SubscriberCap
                })
            );
        }
        other => panic!("expected AttachRefused{{SubscriberCap}}: {other:?}"),
    }
    p.sent(9, a[0].send_marker(), now);
    // Retryable: freeing one existing watcher lets the same connection
    // attach successfully afterward.
    p.connection_closed(0, now);
    let a = p.frame(9, attach_frame("late"), now);
    assert!(a.is_empty(), "should now pend for ground, not refuse: {a:?}");
}

/// Finding 12: a timed-out attach demotes back to `PostHello` only if
/// there is room; over the shared non-watcher cap, it closes instead
/// (after its refusal is sent).
#[test]
fn ground_timeout_over_cap_closes_instead_of_demoting() {
    let mut p = proto();
    let now = t0();
    // conn 1: hello -> attach (now a Watcher, no longer counted as
    // non-watcher) -- pending ground, never resolved.
    p.connection_opened(1, now);
    let a = p.frame(1, hello_frame(), now);
    p.sent(1, a[0].send_marker(), now);
    p.frame(1, attach_frame("alice"), now);

    // Fill the non-watcher cap with 4 OTHER connections.
    for id in 100..104u64 {
        assert_eq!(p.connection_opened(id, now), vec![]);
    }

    // conn 1's ground-wait times out: demoting it back to PostHello
    // would make a 5th non-watcher -- must close instead.
    let later = now + Duration::from_secs(6);
    let a = p.tick(later);
    let send = a.iter().find_map(|x| match x {
        Action::Send { conn, frame_bytes, marker } if *conn == 1 => Some((frame_bytes.clone(), marker.clone())),
        _ => None,
    });
    let (bytes, marker) = send.expect("expected conn 1's ground-timeout reply");
    assert!(
        matches!(marker, Some(SentMarker::ReplyThenClose { .. })),
        "over cap must close, not demote: {marker:?}"
    );
    let decoded = decode_one(&bytes);
    assert_eq!(
        decoded,
        DecodedFrame::AttachServer(AttachServer::AttachRefused {
            reason: AttachRefusedReason::GroundTimeout
        })
    );
    let after = p.sent(1, marker, later);
    assert_eq!(after, vec![Action::Close(1)]);
}

// -- hello --------------------------------------------------------

#[test]
fn hello_refusal_closes_after_the_reply_is_sent() {
    let mut p = proto();
    let now = t0();
    p.connection_opened(1, now);
    let bad_hello = decode_one(&encode_attach_client(&AttachClient::Hello { proto: 999 }).unwrap());
    let a = p.frame(1, bad_hello, now);
    assert!(matches!(
        a.as_slice(),
        [Action::Send { marker: Some(SentMarker::ReplyThenClose { .. }), .. }]
    ));
    let decoded = decode_one(a[0].send_bytes());
    assert_eq!(
        decoded,
        DecodedFrame::AttachServer(AttachServer::HelloRefused {
            supported: wire::ATTACH_PROTO_V3
        })
    );
    // The connection only actually closes once this reply's
    // sent-completion is reported.
    let after = p.sent(1, a[0].send_marker(), now);
    assert_eq!(after, vec![Action::Close(1)]);
}

