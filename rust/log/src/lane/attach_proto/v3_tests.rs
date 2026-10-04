//! Owner-emitted pen and geometry tests.

use super::*;
use super::support_tests::*;
use crate::wire::encode_attach_client;

// -- ADR 0046 decision 3 (lane B3b1): owner-emitted pen/geometry -----

#[test]
fn pen_snapshot_sent_after_final_chunk_with_none_when_no_driver() {
    let mut p = proto();
    let now = t0();
    assert_eq!(p.connection_opened(1, now), vec![]);
    let a = p.frame(1, hello_frame_at(wire::ATTACH_PROTO_V3), now);
    p.sent(1, a[0].send_marker(), now);
    let a = p.frame(1, attach_frame("ctrl"), now);
    assert!(a.is_empty());
    let a = p.ground_reached(now);
    assert!(matches!(a.as_slice(), [Action::BeginCheckpoint { conn: 1 }]));
    let final_actions = drive_checkpoint_to_done_capturing_final(&mut p, 1, vec![0xAB], now);

    assert_eq!(final_actions.len(), 1, "expected exactly one PenSnapshot Send: {final_actions:?}");
    let decoded = decode_one(final_actions[0].send_bytes());
    assert_eq!(
        decoded,
        DecodedFrame::AttachServer(AttachServer::PenSnapshot { holder: None, take_epoch: 0 }),
        "a v3 watcher's FIRST v3 event, always -- None when nobody has ever taken the pen"
    );
}

#[test]
fn pen_changed_broadcasts_to_every_v3_watcher_on_take_and_none_on_driver_eof() {
    let mut p = proto();
    let now = t0();
    attach_to_done_at(&mut p, 1, wire::ATTACH_PROTO_V3, now);
    attach_to_done_at(&mut p, 2, wire::ATTACH_PROTO_V3, now);

    // conn 1 takes -- both conn 1 (the new driver, still underneath a
    // Watcher) and conn 2 (a plain watcher) hear about it, uniformly.
    let a = p.frame(1, take_frame("alice"), now);
    let request_id = match a.as_slice() {
        [Action::CommitTake { request_id, .. }] => *request_id,
        other => panic!("expected CommitTake: {other:?}"),
    };
    let a = p.take_committed(1, "alice".to_string(), 1, request_id, now);
    let expected = DecodedFrame::AttachServer(AttachServer::PenChanged {
        holder: Some("alice".to_string()),
        take_epoch: 1,
    });
    let saw_conn1_pen_changed = a
        .iter()
        .any(|act| matches!(act, Action::Send { conn: 1, frame_bytes, .. } if decode_one(frame_bytes) == expected));
    let conn2_frame = a.iter().find_map(|act| match act {
        Action::Send { conn: 2, frame_bytes, .. } => Some(decode_one(frame_bytes)),
        _ => None,
    });
    assert!(saw_conn1_pen_changed, "expected the new driver's OWN connection to also receive PenChanged: {a:?}");
    assert_eq!(conn2_frame, Some(expected), "expected the other v3 watcher to receive PenChanged: {a:?}");

    for action in &a {
        if let Action::Send { conn, marker, .. } = action {
            p.sent(*conn, marker.clone(), now);
        }
    }

    // The driver (conn 1) disconnects -- capability-only EOF, no
    // durable transition, but every REMAINING v3 watcher learns the
    // pen is gone from the voyage itself.
    let eof_actions = p.connection_closed(1, now);
    assert_eq!(eof_actions.len(), 1, "expected exactly one PenChanged{{None}} Send to conn 2: {eof_actions:?}");
    match &eof_actions[0] {
        Action::Send { conn: 2, frame_bytes, .. } => {
            assert_eq!(
                decode_one(frame_bytes),
                DecodedFrame::AttachServer(AttachServer::PenChanged { holder: None, take_epoch: 1 })
            );
        }
        other => panic!("expected a PenChanged Send to conn 2: {other:?}"),
    }
}

#[test]
fn geometry_reaches_a_done_v3_watcher_immediately_and_ahead_of_the_next_output() {
    let mut p = proto();
    let now = t0();
    attach_to_done_at(&mut p, 1, wire::ATTACH_PROTO_V3, now);

    let a = p.frame(1, take_frame("alice"), now);
    let request_id = match a.as_slice() {
        [Action::CommitTake { request_id, .. }] => *request_id,
        other => panic!("expected CommitTake: {other:?}"),
    };
    let a = p.take_committed(1, "alice".to_string(), 1, request_id, now);
    for action in &a {
        if let Action::Send { conn, marker, .. } = action {
            p.sent(*conn, marker.clone(), now);
        }
    }

    let resize = decode_one(&encode_attach_client(&AttachClient::Resize { cols: 120, rows: 45 }).unwrap());
    let a = p.frame(1, resize, now);
    let request_id = match a.as_slice() {
        [Action::ApplyResize { request_id, conn: 1, .. }] => *request_id,
        other => panic!("expected ApplyResize: {other:?}"),
    };
    let a = p.resize_outcome(1, true, 120, 45, request_id, now);
    let has_geometry = a.iter().any(|act| {
        matches!(
            act,
            Action::Send { conn: 1, frame_bytes, .. }
                if decode_one(frame_bytes) == DecodedFrame::AttachServer(AttachServer::Geometry { cols: 120, rows: 45 })
        )
    });
    assert!(has_geometry, "expected an immediate Geometry Send for the already-Done watcher: {a:?}");
    for action in &a {
        if let Action::Send { conn, marker, .. } = action {
            p.sent(*conn, marker.clone(), now);
        }
    }

    // The next output, committed strictly AFTER the resize's own
    // Geometry, arrives as an ordinary Output -- Geometry is never
    // delayed behind it for a watcher that is already Done.
    let out = p.output_committed(b"hi", now);
    assert!(
        matches!(out.as_slice(), [Action::Send { conn: 1, marker: Some(SentMarker::OutputBytes { n: 2 }), .. }]),
        "{out:?}"
    );
    assert_eq!(
        decode_one(out[0].send_bytes()),
        DecodedFrame::AttachServer(AttachServer::Output { bytes: b"hi".to_vec() })
    );
}

/// The ordering fix this lane makes: a `PenChanged`/`Geometry` sent
/// while a watcher is still mid-transfer is queued behind its
/// checkpoint and drained, in order, right after that watcher's OWN
/// final chunk -- `PenSnapshot` (synthesized fresh, reflecting the
/// driver as of THAT moment) is always first, ahead of anything
/// drained from the queue, so it can legitimately restate a fact the
/// queued `PenChanged` also carries. Never applied before this
/// watcher's own checkpoint `restore_screen`, so `Geometry` can never
/// be silently overwritten by the checkpoint's own baked-in
/// dimensions (the bug this queue extension exists to close).
#[test]
fn pen_changed_and_geometry_mid_transfer_are_queued_and_drained_in_order_after_pen_snapshot() {
    let mut p = proto();
    let now = t0();
    // conn 1 reaches Done first and becomes the driver.
    attach_to_done_at(&mut p, 1, wire::ATTACH_PROTO_V3, now);

    // conn 2: a SECOND v3 watcher, held mid-transfer -- its one and
    // only chunk has been SENT but not yet reported completed.
    assert_eq!(p.connection_opened(2, now), vec![]);
    let a = p.frame(2, hello_frame_at(wire::ATTACH_PROTO_V3), now);
    p.sent(2, a[0].send_marker(), now);
    let a = p.frame(2, attach_frame("watcher"), now);
    assert!(a.is_empty());
    let a = p.ground_reached(now);
    assert!(matches!(a.as_slice(), [Action::BeginCheckpoint { conn: 2 }]));
    let chunk = p.checkpoint_ready(2, vec![0xCD], now);
    let marker = chunk[0].send_marker();
    assert!(
        matches!(marker, Some(SentMarker::CheckpointChunk { is_last: true, .. })),
        "expected a one-chunk transfer: {marker:?}"
    );

    // conn 1 takes -- conn 2 is mid-transfer and must receive NOTHING
    // yet, not even queued as a visible Send.
    let a = p.frame(1, take_frame("alice"), now);
    let request_id = match a.as_slice() {
        [Action::CommitTake { request_id, .. }] => *request_id,
        other => panic!("expected CommitTake: {other:?}"),
    };
    let a = p.take_committed(1, "alice".to_string(), 1, request_id, now);
    assert!(
        !a.iter().any(|act| matches!(act, Action::Send { conn: 2, .. })),
        "conn 2 must not receive anything while its own transfer is still in flight: {a:?}"
    );
    for action in &a {
        if let Action::Send { conn, marker, .. } = action {
            p.sent(*conn, marker.clone(), now);
        }
    }

    // conn 1 (now driver) resizes -- conn 2 still sees nothing yet.
    let resize = decode_one(&encode_attach_client(&AttachClient::Resize { cols: 120, rows: 45 }).unwrap());
    let a = p.frame(1, resize, now);
    let request_id = match a.as_slice() {
        [Action::ApplyResize { request_id, conn: 1, .. }] => *request_id,
        other => panic!("expected ApplyResize: {other:?}"),
    };
    let a = p.resize_outcome(1, true, 120, 45, request_id, now);
    assert!(
        !a.iter().any(|act| matches!(act, Action::Send { conn: 2, .. })),
        "conn 2 must still see nothing while its own transfer is still in flight: {a:?}"
    );
    for action in &a {
        if let Action::Send { conn, marker, .. } = action {
            p.sent(*conn, marker.clone(), now);
        }
    }

    // NOW complete conn 2's transfer: PenSnapshot first (reflecting
    // the ALREADY-committed take), then the queued PenChanged, then
    // the queued Geometry -- in the order they were emitted.
    let final_actions = p.sent(2, marker, now);
    let decoded: Vec<DecodedFrame> = final_actions.iter().map(|a| decode_one(a.send_bytes())).collect();
    assert_eq!(
        decoded,
        vec![
            DecodedFrame::AttachServer(AttachServer::PenSnapshot {
                holder: Some("alice".to_string()),
                take_epoch: 1
            }),
            DecodedFrame::AttachServer(AttachServer::PenChanged {
                holder: Some("alice".to_string()),
                take_epoch: 1
            }),
            DecodedFrame::AttachServer(AttachServer::Geometry { cols: 120, rows: 45 }),
        ],
        "PenSnapshot must be first, then the queued PenChanged and Geometry in emission order: {final_actions:?}"
    );
}

/// Codex review round, should-fix 5 (capsule half): a queued
/// `PenChanged`/`Geometry` shares the SAME `queued_live_bytes`
/// budget as ordinary output, not a second, unbounded list --
/// exhaustion takes the identical visible-termination path. Proven
/// the same way the pre-existing `queue_overflow_closes_with_no_
/// wire_frame` proves it for output: pre-load the budget to exactly
/// its cap via the same public `bytes_queued` call, then show that
/// queuing ONE more event (here, a `Geometry` broadcast reaching a
/// watcher still mid-transfer) is what tips it over and closes the
/// connection.
#[test]
fn queued_geometry_shares_the_watcher_live_queue_budget_and_overflows_the_same_way() {
    let mut p = proto();
    let now = t0();

    // conn 2: reaches Done and becomes the driver first, alone --
    // its own take's broadcast has nobody else to reach yet.
    attach_to_done_at(&mut p, 2, wire::ATTACH_PROTO_V3, now);
    let a = p.frame(2, take_frame("bob"), now);
    let request_id = match a.as_slice() {
        [Action::CommitTake { request_id, .. }] => *request_id,
        other => panic!("expected CommitTake: {other:?}"),
    };
    let a = p.take_committed(2, "bob".to_string(), 1, request_id, now);
    for action in &a {
        if let Action::Send { conn, marker, .. } = action {
            p.sent(*conn, marker.clone(), now);
        }
    }

    // conn 1: a SECOND v3 watcher, held mid-transfer (Sending, not
    // Done) so a broadcast queues for it rather than sending.
    assert_eq!(p.connection_opened(1, now), vec![]);
    let a = p.frame(1, hello_frame_at(wire::ATTACH_PROTO_V3), now);
    p.sent(1, a[0].send_marker(), now);
    let a = p.frame(1, attach_frame("watcher"), now);
    assert!(a.is_empty());
    let a = p.ground_reached(now);
    assert!(matches!(a.as_slice(), [Action::BeginCheckpoint { conn: 1 }]));
    let chunk = p.checkpoint_ready(1, vec![0xCD], now);
    assert!(matches!(chunk[0].send_marker(), Some(SentMarker::CheckpointChunk { is_last: true, .. })));
    // conn 1's own final-chunk `sent` is deliberately never called
    // here -- it stays Sending for the rest of this test.

    // Pre-load conn 1 right up to the cap, exactly as the ordinary-
    // output overflow test does.
    let a = p.bytes_queued(1, WATCHER_LIVE_QUEUE_BUDGET_BYTES, now);
    assert!(a.is_empty(), "exactly at budget must not overflow yet: {a:?}");

    // conn 2 (the driver) resizes -- Geometry broadcasts to both
    // watchers: conn 2 (Done) gets it immediately; conn 1
    // (mid-transfer, already at the cap) gets it QUEUED, and that
    // charge is what tips conn 1 over budget.
    let resize = decode_one(&encode_attach_client(&AttachClient::Resize { cols: 100, rows: 40 }).unwrap());
    let a = p.frame(2, resize, now);
    let request_id = match a.as_slice() {
        [Action::ApplyResize { request_id, conn: 2, .. }] => *request_id,
        other => panic!("expected ApplyResize: {other:?}"),
    };
    let a = p.resize_outcome(2, true, 100, 40, request_id, now);

    assert!(
        a.iter().any(|act| matches!(act, Action::Close(1))),
        "conn 1 must be closed once the queued Geometry tips it over budget: {a:?}"
    );
    assert!(
        a.iter().any(|act| matches!(
            act,
            Action::RecordRefusal { conn: Some(1), reason: RefusalReason::QueueOverflow }
        )),
        "expected the SAME visible-termination reason ordinary output overflow uses: {a:?}"
    );
    assert!(
        !a.iter().any(|act| matches!(act, Action::Send { conn: 1, .. })),
        "no wire frame exists for eviction, by design (same as ordinary output overflow): {a:?}"
    );
    // conn 2 (the driver, Done, not over budget) still gets its own
    // ResizeOk reply and Geometry broadcast normally.
    assert!(a.iter().any(|act| matches!(act, Action::Send { conn: 2, marker: Some(SentMarker::Reply { .. }), .. })));
}

/// Codex round-2 review, blocker: reproduces the exact defect --
/// during a take's `PenChanged(Some(holder))` broadcast, an overflow
/// evicting the NEW DRIVER'S OWN connection recursed into a
/// `PenChanged(None)` broadcast immediately, and the OUTER broadcast
/// then resumed publishing its own now-stale `Some(holder)` to
/// watchers it had not reached yet -- a surviving watcher saw `None`
/// then `Some(the connection that was just evicted)`, wrong
/// indefinitely.
///
/// `broadcast_pen_changed`'s own loop iterates `self.watcher_conns()`,
/// backed by a `HashMap` whose iteration order is not controllable
/// (a randomized per-process hasher) -- unrolled by hand here,
/// calling the SAME private methods that loop calls
/// (`begin_broadcast`, `send_or_queue_pen_geometry`, `end_broadcast`),
/// in the ONE order that actually exercises the bug (the driver's own
/// charge processed BEFORE the bystander's), so this reproduces
/// deterministically regardless of hash seed rather than depending on
/// which order a real `take` would happen to visit connections in.
#[test]
fn overflow_evicting_the_driver_mid_broadcast_defers_its_announcement_past_the_broadcast() {
    let mut p = proto();
    let now = t0();
    const D: ConnId = 1;
    const W: ConnId = 2;
    attach_to_done_at(&mut p, D, wire::ATTACH_PROTO_V3, now);
    attach_to_done_at(&mut p, W, wire::ATTACH_PROTO_V3, now);

    // Install D as the current driver directly -- this test targets
    // the re-entrancy guard itself, not take's own admission path
    // (covered elsewhere, e.g. take_demotes_the_previous_driver_
    // which_stays_a_watcher).
    p.driver = Some(DriverState {
        conn: D,
        controller_id: "d".to_string(),
        take_epoch: 1,
        keepalive_outstanding: None,
        keepalive_deadline: None,
    });

    // Pre-load D right up to the cap -- its OWN PenChanged charge
    // below is what tips it over.
    let pre = p.bytes_queued(D, WATCHER_LIVE_QUEUE_BUDGET_BYTES, now);
    assert!(pre.is_empty(), "exactly at budget must not overflow yet: {pre:?}");

    // Unrolled `broadcast_pen_changed(Some("d"), 1, now)`, D visited
    // first: charging D's OWN copy of its own PenChanged overflows
    // it, evicting D -- deferred (not published immediately) because
    // `broadcasting` is held for the whole unrolled sequence below,
    // exactly as it would be for the real loop's whole duration.
    let was_broadcasting = p.begin_broadcast();
    assert!(!was_broadcasting, "this IS the outermost broadcast");
    let mut actions = p.send_or_queue_pen_geometry(
        D,
        QueuedPostWatermark::PenChanged { holder: Some("d".to_string()), take_epoch: 1 },
        now,
    );
    assert!(
        actions.iter().any(|a| matches!(a, Action::Close(c) if *c == D)),
        "D's own charge must overflow and close it: {actions:?}"
    );
    assert!(
        p.deferred_driver_eviction.is_some(),
        "the eviction must be DEFERRED, not published while still inside this broadcast"
    );
    // The outer broadcast "resumes" with the bystander -- still using
    // the SAME (now-stale) Some("d") parameter, exactly as
    // broadcast_pen_changed's own loop would for a connection it
    // reaches AFTER the one that just overflowed.
    actions.extend(p.send_or_queue_pen_geometry(
        W,
        QueuedPostWatermark::PenChanged { holder: Some("d".to_string()), take_epoch: 1 },
        now,
    ));
    actions.extend(p.end_broadcast(was_broadcasting, now));
    assert!(p.deferred_driver_eviction.is_none(), "end_broadcast must drain the deferred eviction");

    // W's own frames, in the order they were sent.
    let w_frames: Vec<DecodedFrame> = actions
        .iter()
        .filter_map(|a| match a {
            Action::Send { conn, frame_bytes, .. } if *conn == W => Some(decode_one(frame_bytes)),
            _ => None,
        })
        .collect();
    assert_eq!(
        w_frames,
        vec![
            DecodedFrame::AttachServer(AttachServer::PenChanged {
                holder: Some("d".to_string()),
                take_epoch: 1
            }),
            DecodedFrame::AttachServer(AttachServer::PenChanged { holder: None, take_epoch: 1 }),
        ],
        "W must see Some(new driver) then None (the eviction) -- never the reverse, and never a stale \
         Some(evicted driver) resurrected after the None that already superseded it: {w_frames:?}"
    );
}

#[test]
fn v2_watcher_receives_no_pen_or_geometry_events() {
    let mut p = proto();
    let now = t0();
    attach_to_done_at(&mut p, 1, wire::ATTACH_PROTO_V2, now);
    let a = p.frame(1, take_frame("alice"), now);
    let request_id = match a.as_slice() {
        [Action::CommitTake { request_id, .. }] => *request_id,
        other => panic!("expected CommitTake: {other:?}"),
    };
    let a = p.take_committed(1, "alice".to_string(), 1, request_id, now);
    assert!(
        matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::Reply { .. }), .. }]),
        "a v2 watcher must receive nothing beyond its own TakeOk reply: {a:?}"
    );
    assert_eq!(
        decode_one(a[0].send_bytes()),
        DecodedFrame::AttachServer(AttachServer::TakeOk { take_epoch: 1 })
    );
}

#[test]
fn a_v3_watcher_receives_pen_changed_alongside_a_v2_watcher_that_does_not() {
    let mut p = proto();
    let now = t0();
    attach_to_done_at(&mut p, 1, wire::ATTACH_PROTO_V2, now);
    attach_to_done_at(&mut p, 2, wire::ATTACH_PROTO_V3, now);
    attach_to_done_at(&mut p, 3, wire::ATTACH_PROTO_V3, now);

    // conn 3 takes -- conns 1 and 2 are both pure bystanders.
    let a = p.frame(3, take_frame("carol"), now);
    let request_id = match a.as_slice() {
        [Action::CommitTake { request_id, .. }] => *request_id,
        other => panic!("expected CommitTake: {other:?}"),
    };
    let a = p.take_committed(3, "carol".to_string(), 1, request_id, now);

    assert!(
        !a.iter().any(|act| matches!(act, Action::Send { conn: 1, .. })),
        "the v2 watcher must receive nothing: {a:?}"
    );
    let conn2_frame = a.iter().find_map(|act| match act {
        Action::Send { conn: 2, frame_bytes, .. } => Some(decode_one(frame_bytes)),
        _ => None,
    });
    assert_eq!(
        conn2_frame,
        Some(DecodedFrame::AttachServer(AttachServer::PenChanged {
            holder: Some("carol".to_string()),
            take_epoch: 1
        })),
        "the v3 watcher must receive PenChanged: {a:?}"
    );
}
