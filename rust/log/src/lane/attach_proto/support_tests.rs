//! Shared helpers for the attach protocol's tests: frame builders and drivers.

use super::*;
use crate::lane::wire::{encode_attach_client, FrameSplitter};

pub(super) fn t0() -> Instant {
    Instant::now()
}

pub(super) fn proto() -> AttachProto {
    AttachProto::new(MgmtStatus {
        pid: 4242,
        created: 0x0123_4567_89ab_cdef,
        survival: Survival::Normal,
    })
}

pub(super) fn decode_one(bytes: &[u8]) -> DecodedFrame {
    let mut s = FrameSplitter::new();
    let (frames, err) = s.feed(bytes);
    assert_eq!(err, None);
    assert_eq!(frames.len(), 1);
    frames.into_iter().next().unwrap()
}

pub(super) fn hello_frame() -> DecodedFrame {
    hello_frame_at(wire::ATTACH_PROTO_V1)
}

pub(super) fn hello_frame_at(proto: u32) -> DecodedFrame {
    decode_one(&encode_attach_client(&AttachClient::Hello { proto }).unwrap())
}

pub(super) fn attach_frame(controller_id: &str) -> DecodedFrame {
    decode_one(
        &encode_attach_client(&AttachClient::Attach {
            controller_id: controller_id.to_string(),
        })
        .unwrap(),
    )
}

pub(super) fn take_frame(controller_id: &str) -> DecodedFrame {
    decode_one(
        &encode_attach_client(&AttachClient::Take {
            controller_id: controller_id.to_string(),
        })
        .unwrap(),
    )
}

impl Action {
    pub(super) fn send_marker(&self) -> Option<SentMarker> {
        match self {
            Action::Send { marker, .. } => marker.clone(),
            _ => panic!("not a Send: {self:?}"),
        }
    }
    pub(super) fn send_bytes(&self) -> &[u8] {
        match self {
            Action::Send { frame_bytes, .. } => frame_bytes,
            _ => panic!("not a Send: {self:?}"),
        }
    }
}

/// Drives one checkpoint transfer to completion, one chunk at a time
/// (finding 10: `checkpoint_ready`/`sent` must never emit more than one
/// `Send` per step) — the common setup every take/input/resize/
/// keepalive/budget test needs once a watcher must be fully `Done`.
pub(super) fn drive_checkpoint_to_done(p: &mut AttachProto, conn: ConnId, checkpoint_bytes: Vec<u8>, now: Instant) {
    let mut actions = p.checkpoint_ready(conn, checkpoint_bytes, now);
    loop {
        assert_eq!(actions.len(), 1, "expected exactly one Send per checkpoint step: {actions:?}");
        let marker = actions[0].send_marker();
        let is_last = matches!(marker, Some(SentMarker::CheckpointChunk { is_last: true, .. }));
        actions = p.sent(conn, marker, now);
        if is_last {
            break;
        }
    }
}

/// Drives one connection all the way to a `Done` watcher: connection_opened
/// -> hello -> attach -> ground_reached -> a streamed one-chunk checkpoint.
pub(super) fn attach_to_done(p: &mut AttachProto, conn: ConnId, now: Instant) {
    attach_to_done_at(p, conn, wire::ATTACH_PROTO_V1, now);
}

/// As `attach_to_done`, negotiating `proto` explicitly (ADR 0046
/// decision 3, lane B3b1's own v3 tests) — the returned actions
/// through `sent`'s final chunk are DISCARDED here (unlike
/// `attach_to_done`'s v1 callers, a v3 attach's final-chunk actions
/// carry the new `PenSnapshot`, which most callers of this v3 helper
/// want to assert on themselves rather than have silently dropped by
/// a shared helper); use [`drive_checkpoint_to_done_capturing_final`]
/// directly instead of this helper when that assertion matters.
pub(super) fn attach_to_done_at(p: &mut AttachProto, conn: ConnId, proto: u32, now: Instant) {
    assert_eq!(p.connection_opened(conn, now), vec![]);
    let a = p.frame(conn, hello_frame_at(proto), now);
    assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::Reply { .. }), .. }]));
    p.sent(conn, a[0].send_marker(), now);
    let a = p.frame(conn, attach_frame("ctrl"), now);
    assert!(a.is_empty(), "attach should pend, not reply immediately: {a:?}");
    let a = p.ground_reached(now);
    assert!(matches!(a.as_slice(), [Action::BeginCheckpoint { conn: c }] if *c == conn));
    drive_checkpoint_to_done(p, conn, vec![0xAB], now);
}

/// As `drive_checkpoint_to_done`, but returns the FINAL chunk's own
/// completion actions instead of discarding them — what a v3 test
/// asserting on `PenSnapshot`/a drained queue needs to see.
pub(super) fn drive_checkpoint_to_done_capturing_final(
    p: &mut AttachProto,
    conn: ConnId,
    checkpoint_bytes: Vec<u8>,
    now: Instant,
) -> Vec<Action> {
    let mut actions = p.checkpoint_ready(conn, checkpoint_bytes, now);
    loop {
        assert_eq!(actions.len(), 1, "expected exactly one Send per checkpoint step: {actions:?}");
        let marker = actions[0].send_marker();
        let is_last = matches!(marker, Some(SentMarker::CheckpointChunk { is_last: true, .. }));
        let next = p.sent(conn, marker, now);
        if is_last {
            return next;
        }
        actions = next;
    }
}

/// Drives `take` to completion: frame -> CommitTake -> take_committed
/// (the loop's own fsync is not modeled here; this module never needs
/// it) -> the TakeOk reply's own sent-completion.
pub(super) fn drive_take(p: &mut AttachProto, conn: ConnId, controller_id: &str, epoch: u64, now: Instant) {
    let a = p.frame(conn, take_frame(controller_id), now);
    let request_id = match a.as_slice() {
        [Action::CommitTake { request_id, .. }] => *request_id,
        other => panic!("expected CommitTake: {other:?}"),
    };
    let a = p.take_committed(conn, controller_id.to_string(), epoch, request_id, now);
    assert!(matches!(a.as_slice(), [Action::Send { marker: Some(SentMarker::Reply { .. }), .. }]));
    p.sent(conn, a[0].send_marker(), now);
}

