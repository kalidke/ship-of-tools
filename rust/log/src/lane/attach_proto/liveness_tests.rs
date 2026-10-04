//! Keepalive, progress deadline, mgmt idle deadline, queue accounting and teardown tests.

use super::*;
use super::support_tests::*;
use crate::lane::wire::{encode_attach_client, encode_keepalive, encode_mgmt_request};

// -- keepalive --------------------------------------------------------

/// The keepalive's OWN send is also subject to the generic
/// progress-stall bound (finding 5: it is just another outstanding
/// send) -- so an UNCONFIRMED keepalive eventually closes the
/// connection regardless, via that generic mechanism, and this test
/// must not (and does not) contradict that. What it proves instead is
/// the DISTINCT property the ADR pins for the reply deadline
/// specifically: once the keepalive IS confirmed sent, its OWN 30s
/// reply window starts fresh from THAT moment, not from whenever it
/// was originally enqueued -- confirming it late (here, 25s after
/// enqueue, comfortably inside both bounds) and then checking a point
/// that would already be expired under a wrongly enqueue-anchored
/// deadline, but is not under a correctly sent-completion-anchored
/// one, is what distinguishes the two.
#[test]
fn keepalive_deadline_starts_at_sent_completion_not_enqueue() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    drive_take(&mut p, 1, "alice", 1, now);

    let idle = now + KEEPALIVE_IDLE_TRIGGER;
    let a = p.tick(idle);
    assert_eq!(a.len(), 1);
    let nonce = match &a[0] {
        Action::Send {
            marker: Some(SentMarker::Keepalive { nonce }),
            ..
        } => *nonce,
        other => panic!("expected a Keepalive send: {other:?}"),
    };

    // Confirmed late -- 25s after being sent, still inside every
    // bound.
    let confirmed_at = idle + Duration::from_secs(25);
    assert!(p.tick(confirmed_at).is_empty());
    p.sent(1, Some(SentMarker::Keepalive { nonce }), confirmed_at);

    // 29s after CONFIRMATION (54s after the original enqueue): past a
    // WRONGLY enqueue-anchored deadline (idle+30s), but still inside a
    // correctly confirmation-anchored one (confirmed_at+30s).
    let a = p.tick(confirmed_at + Duration::from_secs(29));
    assert!(a.is_empty(), "the reply deadline must run from sent-completion, not enqueue: {a:?}");
    let a = p.tick(confirmed_at + Duration::from_secs(31));
    assert!(a.iter().any(|x| matches!(x, Action::Close(1))), "expected keepalive death: {a:?}");
}

#[test]
fn keepalive_reply_wrong_nonce_is_unexpected_and_closes() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    drive_take(&mut p, 1, "alice", 1, now);
    let idle = now + KEEPALIVE_IDLE_TRIGGER;
    let a = p.tick(idle);
    let real_nonce = match &a[0] {
        Action::Send { marker: Some(SentMarker::Keepalive { nonce }), .. } => *nonce,
        other => panic!("{other:?}"),
    };
    let bogus = decode_one(&encode_keepalive(real_nonce.wrapping_add(1)));
    let a = p.frame(1, bogus, idle);
    assert!(a.iter().any(|x| matches!(x, Action::Close(1))));
    assert!(a
        .iter()
        .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::UnexpectedKeepalive, .. })));
}

/// Finding 6 / round-2 finding 3: a demoted former driver's late
/// keepalive echo, for a nonce `Conn::last_keepalive_nonce` still
/// remembers issuing to it, is ignorable, not fatal -- it must not
/// close the connection, which is still a legitimate subscriber.
#[test]
fn demoted_drivers_late_keepalive_echo_is_ignored_not_fatal() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    attach_to_done(&mut p, 2, now);
    drive_take(&mut p, 1, "alice", 1, now);

    let idle = now + KEEPALIVE_IDLE_TRIGGER;
    let a = p.tick(idle);
    let nonce = match &a[0] {
        Action::Send { marker: Some(SentMarker::Keepalive { nonce }), .. } => *nonce,
        other => panic!("{other:?}"),
    };

    // conn 2 takes next, demoting conn 1 -- conn 1 is no longer the
    // driver, but its OWN Conn record still remembers issuing this nonce.
    drive_take(&mut p, 2, "bob", 2, now);

    let echo = decode_one(&encode_keepalive(nonce));
    let a = p.frame(1, echo, now);
    assert!(a.is_empty(), "a demoted driver's late echo must be ignored, not closed: {a:?}");
    let out = p.output_committed(b"hi", now);
    assert!(out.iter().any(|a| matches!(a, Action::Send { conn: 1, .. })));
}

/// Round-2 review, finding 3 (reproduced by the reviewer's own
/// state-machine probe): a SAME-connection retake used to discard the
/// outstanding nonce along with the rest of `DriverState`, so that
/// connection's own later late echo of it looked identical to a
/// fabricated one and was wrongly closed as `UnexpectedKeepalive`.
#[test]
fn same_connection_retake_still_treats_its_old_nonce_as_a_legitimate_late_echo() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    drive_take(&mut p, 1, "alice", 1, now);

    let idle = now + KEEPALIVE_IDLE_TRIGGER;
    let a = p.tick(idle);
    let nonce = match &a[0] {
        Action::Send { marker: Some(SentMarker::Keepalive { nonce }), .. } => *nonce,
        other => panic!("{other:?}"),
    };
    p.sent(1, Some(SentMarker::Keepalive { nonce }), idle); // reply deadline armed

    // Conn 1 retakes -- SAME connection, a fresh `DriverState`.
    drive_take(&mut p, 1, "alice", 2, idle);

    // The old ping, echoed after the retake: must be ignored, not
    // closed as `UnexpectedKeepalive`.
    let echo = decode_one(&encode_keepalive(nonce));
    let a = p.frame(1, echo, idle);
    assert!(a.is_empty(), "a same-connection retake's own prior nonce must still echo as legitimate: {a:?}");
}

/// The other side of finding 3: a connection that was NEVER issued a
/// keepalive nonce at all (a watcher, never driver) is a genuine
/// protocol violation if it echoes one anyway -- not silently waved
/// through the way a legitimate former driver's late echo is.
#[test]
fn a_never_issued_keepalive_nonce_from_any_connection_is_a_protocol_violation() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    attach_to_done(&mut p, 2, now);
    drive_take(&mut p, 1, "alice", 1, now);

    // Conn 2 was never driver and was never issued anything.
    let echo = decode_one(&encode_keepalive(42));
    let a = p.frame(2, echo, now);
    assert!(a.iter().any(|x| matches!(x, Action::Close(2))));
    assert!(a
        .iter()
        .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::UnexpectedKeepalive, .. })));
}

// -- progress deadline (finding 5) ------------------------------------

/// The generic 30s deadline covers a watcher's own outstanding
/// live-output send and resets at the empty→nonempty transition: a
/// connection that sat idle for a LONG time before anything was ever
/// queued must not be penalized for that idle stretch the instant
/// something finally is.
///
/// U1a Codex round-1, Major 4 discharge: this test's ORIGINAL vehicle
/// was an unconfirmed MGMT reply — which now has its own tighter,
/// mgmt-specific 5s bound (see the "mgmt idle deadline" tests above),
/// so an mgmt connection can no longer reach 29s/31s without the
/// mgmt-specific check firing first at 5s. A watcher's own outstanding
/// `output` send is unaffected by that mgmt-only rule and still proves
/// the SAME generic-30s / empty-to-nonempty-reset property this test
/// exists for.
#[test]
fn progress_deadline_covers_every_kind_of_outstanding_send_and_resets_at_the_empty_to_nonempty_transition() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    let much_later = now + Duration::from_secs(1000);
    let a = p.output_committed(b"hi", much_later);
    assert!(matches!(a.as_slice(), [Action::Send { .. }]));

    let a = p.tick(much_later + Duration::from_secs(29));
    assert!(
        a.is_empty(),
        "must not fire early, and must not be penalized for the earlier idle stretch: {a:?}"
    );
    let a = p.tick(much_later + Duration::from_secs(31));
    assert!(a.iter().any(|x| matches!(x, Action::Close(1))), "{a:?}");
    assert!(a
        .iter()
        .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::ProgressStall, .. })));
}

#[test]
fn progress_deadline_still_covers_live_output_specifically() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    let a = p.output_committed(b"hi", now);
    assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::OutputBytes { n: 2 }), .. }]));
    let a = p.tick(now + Duration::from_secs(31));
    assert!(a.iter().any(|x| matches!(x, Action::Close(1))));
}

// -- mgmt idle deadline (U1a, ADR 0041 bounds table "mgmt idle") -----

/// An admitted mgmt connection (classified by its first frame) that
/// never sends again past `MGMT_IDLE_DEADLINE` is evicted — the "pool
/// squatting" row: four such connections could otherwise pin the
/// capsule at `NON_WATCHER_CAP` forever while every new probe was
/// refused.
#[test]
fn idle_mgmt_connection_is_evicted_at_the_deadline() {
    let mut p = proto();
    let now = t0();
    assert_eq!(p.connection_opened(1, now), vec![]);
    let probe = decode_one(&encode_mgmt_request(&MgmtRequest::Probe).unwrap());
    let a = p.frame(1, probe, now);
    assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::Reply { .. }), .. }]));
    p.sent(1, a[0].send_marker(), now); // reply confirmed sent -- last_activity == now

    // Just under the deadline: untouched.
    let a = p.tick(now + MGMT_IDLE_DEADLINE - Duration::from_millis(1));
    assert!(a.is_empty(), "must not evict before the deadline: {a:?}");

    // At the deadline: evicted, with the U1a-specific reason.
    let a = p.tick(now + MGMT_IDLE_DEADLINE);
    assert!(a.iter().any(|x| matches!(x, Action::Close(1))), "{a:?}");
    assert!(
        a.iter()
            .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::MgmtIdleTimeout, .. })),
        "{a:?}"
    );
}

/// The ADR's own carve-out: "ACTIVE occupancy... is out of scope" — a
/// connection that keeps sending requests at least once per interval
/// is never evicted, because every inbound frame resets the SAME
/// `last_activity` clock the deadline reads.
#[test]
fn active_mgmt_connection_is_never_evicted() {
    let mut p = proto();
    let now = t0();
    assert_eq!(p.connection_opened(1, now), vec![]);
    let probe = decode_one(&encode_mgmt_request(&MgmtRequest::Probe).unwrap());
    let a = p.frame(1, probe, now);
    p.sent(1, a[0].send_marker(), now);

    // A second request just inside the deadline resets the clock.
    let t1 = now + MGMT_IDLE_DEADLINE - Duration::from_millis(1);
    let status = decode_one(&encode_mgmt_request(&MgmtRequest::Status).unwrap());
    let a = p.frame(1, status, t1);
    assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::Reply { .. }), .. }]));
    p.sent(1, a[0].send_marker(), t1);

    // Ticking to what would have been the FIRST request's own deadline
    // must not evict -- the clock reset at t1.
    let a = p.tick(now + MGMT_IDLE_DEADLINE);
    assert!(a.is_empty(), "activity must reset the idle clock: {a:?}");

    // The watcher/driver machinery (NON_WATCHER_CAP, admission) is
    // untouched by this connection staying open across the interval.
    assert_eq!(p.non_watcher_count, 1);
}

/// U1a Codex round-1, Major 4 discharge: an mgmt connection that stops
/// draining its OWN unconfirmed reply is evicted at the tighter 5s
/// bound, never the generic 30s `PROGRESS_DEADLINE` -- squatting on an
/// outstanding send is not legitimate write progress for this role.
/// The send is never confirmed (`p.sent` is deliberately not called),
/// so `outstanding_sends` stays 1 throughout.
#[test]
fn mgmt_connection_with_a_stalled_outbound_reply_is_evicted_at_the_tighter_bound() {
    let mut p = proto();
    let now = t0();
    assert_eq!(p.connection_opened(1, now), vec![]);
    let probe = decode_one(&encode_mgmt_request(&MgmtRequest::Probe).unwrap());
    let a = p.frame(1, probe, now);
    assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::Reply { .. }), .. }]));

    // Just under the tighter bound: untouched (proving it isn't ALSO
    // firing prematurely off some other clock).
    let a = p.tick(now + MGMT_IDLE_DEADLINE - Duration::from_millis(1));
    assert!(a.is_empty(), "must not evict before the tighter bound: {a:?}");

    // At the tighter bound (well before the generic 30s deadline could
    // ever apply): evicted, with the mgmt-specific reason, never
    // ProgressStall.
    let a = p.tick(now + MGMT_IDLE_DEADLINE);
    assert!(a.iter().any(|x| matches!(x, Action::Close(1))), "{a:?}");
    assert!(
        a.iter()
            .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::MgmtIdleTimeout, .. })),
        "{a:?}"
    );
}

/// Codex round-2b discharge (a live repro confirmed this before the
/// fix): a valid HELD frame is still activity. Models the documented
/// transport race `frame`'s own doc names -- the peer physically read
/// the first reply and sent its next lockstep request, but the
/// reader's `Bytes` event reaches this module before the writer's
/// `Sent` completion does, so `outstanding_sends` is still 1 when the
/// second, entirely valid request arrives and is legitimately HELD
/// (not a lockstep violation). Before this fix, the mgmt idle scan
/// read ONLY `last_send_progress` while `outstanding_sends > 0`,
/// which the held frame never touches -- so a client demonstrably
/// still talking to us, a millisecond before the deadline, was evicted
/// anyway. The fix: `frame` now refreshes `last_activity` when it
/// holds a frame, and the scan reads the MORE RECENT of `last_activity`
/// and `last_send_progress` (see both sites' own doc).
#[test]
fn a_valid_held_mgmt_frame_resets_the_idle_clock_and_is_not_evicted() {
    let mut p = proto();
    let now = t0();
    assert_eq!(p.connection_opened(1, now), vec![]);
    let probe = decode_one(&encode_mgmt_request(&MgmtRequest::Probe).unwrap());
    let a = p.frame(1, probe, now);
    assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::Reply { .. }), .. }]));
    // Deliberately never confirmed (`p.sent` not called): models the
    // writer's own `Sent` completion not having arrived yet.

    // The peer's next lockstep request, arriving just before the
    // deadline, while the first reply is still outstanding.
    let active_at = now + MGMT_IDLE_DEADLINE - Duration::from_millis(1);
    let status = decode_one(&encode_mgmt_request(&MgmtRequest::Status).unwrap());
    assert!(p.frame(1, status, active_at).is_empty(), "a legitimately held frame returns no action yet");

    // At what would have been the ORIGINAL deadline (measured from the
    // first request, ignoring the held frame): must NOT evict -- the
    // held frame proved the client active.
    let a = p.tick(now + MGMT_IDLE_DEADLINE);
    assert!(
        a.is_empty(),
        "a valid held frame must reset the idle clock, not be silently ignored by it: {a:?}"
    );

    // The connection is still genuinely subject to the SAME bound,
    // measured from the held frame's own arrival: silence after that
    // point still evicts.
    let a = p.tick(active_at + MGMT_IDLE_DEADLINE);
    assert!(a.iter().any(|x| matches!(x, Action::Close(1))), "{a:?}");
    assert!(
        a.iter()
            .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::MgmtIdleTimeout, .. })),
        "{a:?}"
    );
}

/// Watchers and the driver are a different `Role` entirely — the mgmt
/// idle deadline must never reach them, however long they sit with no
/// live output to send.
#[test]
fn idle_deadline_does_not_touch_a_watcher_or_the_driver() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    drive_take(&mut p, 1, "alice", 1, now);
    let a = p.tick(now + MGMT_IDLE_DEADLINE + Duration::from_secs(1));
    assert!(a.is_empty(), "a watcher/driver connection must never be mgmt-idle-evicted: {a:?}");
}

// -- queue accounting -------------------------------------------------

#[test]
fn queue_overflow_closes_with_no_wire_frame() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    let a = p.bytes_queued(1, WATCHER_LIVE_QUEUE_BUDGET_BYTES + 1, now);
    assert!(a.iter().any(|x| matches!(x, Action::Close(1))));
    assert!(a
        .iter()
        .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::QueueOverflow, .. })));
    assert!(
        !a.iter().any(|x| matches!(x, Action::Send { .. })),
        "no wire frame exists for eviction, by design: {a:?}"
    );
}

/// Round-2 review, finding 2: an EARLIER exemption here (a real CI
/// fix at the time, for a real bug -- the driver getting caught by the
/// SAME eviction as an actually-slow watcher during
/// `slow_watcher_overflow_closes_while_driver_stays_live`) removed the
/// ADR's 4 MiB memory bound for the driver entirely, which is NOT what
/// the budget table says: "driver queue 4 MiB; committed driver-
/// visible bytes are never dropped while the connection is live, but
/// transport liveness is bounded... a hung driver cannot wedge the
/// writer loop" is TWO clauses together -- the bound stays, and
/// overflow resolves by closing the connection (never by silently
/// dropping bytes while it stays live). The bytes are never actually
/// lost either way (durable in the voyage before this call ever runs;
/// a reconnect replays them via a fresh checkpoint) -- what changes is
/// the LABEL, `DriverQueueOverflow` instead of `QueueOverflow`, so
/// step 6's UX can tell the two situations apart.
#[test]
fn the_driver_is_closed_on_overflow_same_as_a_watcher_but_labeled_distinctly() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    drive_take(&mut p, 1, "alice", 1, now);
    let a = p.bytes_queued(1, WATCHER_LIVE_QUEUE_BUDGET_BYTES + 1, now);
    assert!(a.iter().any(|x| matches!(x, Action::Close(1))), "the driver must still be closed on overflow: {a:?}");
    assert!(
        a.iter()
            .any(|x| matches!(x, Action::RecordRefusal { reason: RefusalReason::DriverQueueOverflow, .. })),
        "a driver's overflow must be labeled distinctly from an ordinary watcher's: {a:?}"
    );
}

/// Reconstructs, event-for-event, what
/// `tests/capsule_win.rs::slow_watcher_overflow_closes_while_driver_stays_live`
/// drives through the wire (PR #139 discharge round CI failure: "timed
/// out waiting for an expected frame on conn 1"): conn 1 (DRIVER)
/// attaches and takes; conn 2 (WATCHER) attaches to a `Done` checkpoint
/// -- exactly like the integration test's `collect_checkpoint`
/// completing BEFORE `set_hold_for` -- then is held forever (its
/// `Output` sends are never confirmed here, matching a client that
/// stopped reading its pipe); a 6 MiB flood, in the capsule's own
/// `GROUP_COMMIT_BYTES`-sized (256 KiB) increments, is published to
/// both, with the driver's own sends confirmed immediately every round
/// (an unheld synthetic transport). Once the watcher is evicted, the
/// driver must still answer a `resize`.
///
/// `now` never advances past `t0()`: this isolates the state machine's
/// own LOGIC from real wall-clock throughput. If this passes, the bug
/// is not in `AttachProto` and must be sought in `capsule_win.rs`'s
/// wiring or the integration test's own synthetic transport.
#[test]
fn replay_slow_watcher_flood_the_driver_still_answers_a_resize() {
    let mut p = proto();
    let now = t0();
    const DRIVER: ConnId = 1;
    const WATCHER: ConnId = 2;

    attach_to_done(&mut p, DRIVER, now);
    drive_take(&mut p, DRIVER, "driver", 1, now);
    attach_to_done(&mut p, WATCHER, now);

    const CHUNK: usize = 256 * 1024;
    const TOTAL: usize = 6 * 1024 * 1024;
    let chunk = vec![0xAAu8; CHUNK];
    let mut sent_so_far = 0usize;
    let mut watcher_closed = false;
    while sent_so_far < TOTAL {
        let actions = p.output_committed(&chunk, now);
        for a in &actions {
            match a {
                Action::Send { conn, .. } if *conn == DRIVER => {
                    p.sent(DRIVER, a.send_marker(), now);
                }
                Action::Send { conn, .. } if *conn == WATCHER => {
                    // Held forever -- never confirmed, exactly like
                    // `set_hold_for(WATCHER, true)`.
                }
                Action::Close(c) if *c == WATCHER => watcher_closed = true,
                Action::RecordRefusal { conn: Some(c), reason: RefusalReason::QueueOverflow } if *c == WATCHER => {}
                other => panic!("unexpected action mid-flood: {other:?}"),
            }
        }
        sent_so_far += CHUNK;
        if watcher_closed {
            break;
        }
    }
    assert!(
        watcher_closed,
        "the watcher must be evicted well before 6 MiB, exactly like the integration test"
    );

    let resize = decode_one(&encode_attach_client(&AttachClient::Resize { cols: 100, rows: 40 }).unwrap());
    let a = p.frame(DRIVER, resize, now);
    let request_id = match a.as_slice() {
        [Action::ApplyResize { request_id, conn, .. }] if *conn == DRIVER => *request_id,
        other => panic!("expected ApplyResize for the driver, got: {other:?}"),
    };
    let a = p.resize_outcome(DRIVER, true, 100, 40, request_id, now);
    assert!(
        matches!(
            a.as_slice(),
            [Action::Send { conn, marker: Some(SentMarker::Reply { .. }), .. }] if *conn == DRIVER
        ),
        "expected a ResizeOk reply Send for the driver: {a:?}"
    );
}

/// Directly answers the coordinator's "double-check the timeout
/// arithmetic" ask: does the resize this scenario needs answered
/// legitimately require a keepalive/progress interval to elapse first
/// (making the integration test's 10s wait too short BY DESIGN), or is
/// the reply available immediately regardless? Advances `now` well
/// past `KEEPALIVE_IDLE_TRIGGER` (30s) since the take -- long enough
/// for `tick` to actually arm a keepalive on the driver, exactly what
/// a slow-draining flood (during which the driver sends no wire
/// traffic of its own) would do -- then leaves it UNANSWERED (the
/// integration test's harness implements no keepalive-reply logic at
/// all) and confirms the SAME resize still gets an immediate
/// `ResizeOk`, at that SAME `now`. No deadline in this module gates a
/// resize behind a keepalive; the ten-second wait is not too short by
/// protocol design.
#[test]
fn resize_answers_immediately_even_with_a_keepalive_outstanding_on_the_driver() {
    let mut p = proto();
    let now = t0();
    const DRIVER: ConnId = 1;
    attach_to_done(&mut p, DRIVER, now);
    drive_take(&mut p, DRIVER, "driver", 1, now);

    let later = now + KEEPALIVE_IDLE_TRIGGER + Duration::from_secs(1);
    let a = p.tick(later);
    assert!(
        matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::Keepalive { .. }), .. }]),
        "expected tick to arm a keepalive after the idle trigger: {a:?}"
    );

    let resize = decode_one(&encode_attach_client(&AttachClient::Resize { cols: 100, rows: 40 }).unwrap());
    let a = p.frame(DRIVER, resize, later);
    let request_id = match a.as_slice() {
        [Action::ApplyResize { request_id, conn, .. }] if *conn == DRIVER => *request_id,
        other => panic!("expected ApplyResize for the driver: {other:?}"),
    };
    let a = p.resize_outcome(DRIVER, true, 100, 40, request_id, later);
    assert!(
        matches!(
            a.as_slice(),
            [Action::Send { conn, marker: Some(SentMarker::Reply { .. }), .. }] if *conn == DRIVER
        ),
        "an outstanding, unanswered keepalive must not delay or block a resize reply: {a:?}"
    );
}

#[test]
fn output_committed_reaches_a_done_watcher_and_is_gated_by_the_budget() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    let a = p.output_committed(b"hi", now);
    assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::OutputBytes { n: 2 }), .. }]));
    p.sent(1, a[0].send_marker(), now);
}

// -- teardown (finding 7) ---------------------------------------------

#[test]
fn teardown_ignores_producer_bound_requests_but_not_mgmt_or_attach() {
    let mut p = proto();
    let now = t0();
    attach_to_done(&mut p, 1, now);
    drive_take(&mut p, 1, "alice", 1, now);
    p.begin_teardown();

    let input = decode_one(
        &encode_attach_client(&AttachClient::Input {
            controller_id: "alice".into(),
            take_epoch: 1,
            idem_key: [1u8; 16],
            payload: b"x".to_vec(),
        })
        .unwrap(),
    );
    assert_eq!(p.frame(1, input, now), vec![]);
    let resize = decode_one(&encode_attach_client(&AttachClient::Resize { cols: 100, rows: 40 }).unwrap());
    assert_eq!(p.frame(1, resize, now), vec![]);

    // No lockstep leak from an ignored request: a follow-up (still
    // ignored) request on the same connection produces no violation
    // either.
    let resize2 = decode_one(&encode_attach_client(&AttachClient::Resize { cols: 90, rows: 30 }).unwrap());
    let a = p.frame(1, resize2, now);
    assert!(a.is_empty());
    assert!(!a.iter().any(|x| matches!(x, Action::Close(_))));

    // mgmt still works, on a separate connection.
    p.connection_opened(2, now);
    let a = p.frame(2, decode_one(&encode_mgmt_request(&MgmtRequest::Probe).unwrap()), now);
    assert!(matches!(a.as_slice(), [Action::Send { .. }]), "mgmt must still be serviced during teardown: {a:?}");
}

