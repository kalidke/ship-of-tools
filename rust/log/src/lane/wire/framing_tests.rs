//! Framing tests: lane binding, caps, unknown tags, trailing bytes, negotiation, chunk arithmetic, the failed latch.

use super::support_tests::*;
use super::*;

// ---- lane binding ---------------------------------------------------

#[test]
fn attach_then_mgmt_is_a_lane_mismatch() {
    let attach = encode_attach_client(&AttachClient::Hello { proto: 1 }).unwrap();
    let mgmt = encode_mgmt_request(&MgmtRequest::Probe).unwrap();
    let mut s = FrameSplitter::new();
    feed_ok(&mut s, &attach);
    let err = feed_err(&mut s, &mgmt);
    assert_eq!(
        err,
        WireError::LaneMismatch {
            latched: ATTACH_MAGIC,
            got: MGMT_MAGIC
        }
    );
}

#[test]
fn mgmt_then_attach_is_a_lane_mismatch() {
    let mgmt = encode_mgmt_request(&MgmtRequest::Probe).unwrap();
    let attach = encode_attach_client(&AttachClient::Hello { proto: 1 }).unwrap();
    let mut s = FrameSplitter::new();
    feed_ok(&mut s, &mgmt);
    let err = feed_err(&mut s, &attach);
    assert_eq!(
        err,
        WireError::LaneMismatch {
            latched: MGMT_MAGIC,
            got: ATTACH_MAGIC
        }
    );
}

#[test]
fn unknown_magic_errors() {
    let mut bogus = Vec::new();
    bogus.extend_from_slice(b"XXXX");
    push_u32(&mut bogus, 0);
    let mut s = FrameSplitter::new();
    assert_eq!(feed_err(&mut s, &bogus), WireError::UnknownMagic(*b"XXXX"));
}

// ---- cap --------------------------------------------------------------

#[test]
fn header_announcing_over_cap_len_errors_without_the_body() {
    let mut header_only = Vec::new();
    header_only.extend_from_slice(&MGMT_MAGIC);
    push_u32(&mut header_only, (MAX_BODY_LEN as u32) + 1);
    // No body bytes at all follow -- if this errored by trying to
    // gather that many bytes first it would just carry (return Ok
    // with no frames) instead of erroring.
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_err(&mut s, &header_only),
        WireError::BodyTooLarge((MAX_BODY_LEN as u32) + 1)
    );
}

#[test]
fn header_at_exactly_the_cap_is_not_an_error() {
    let mut header_only = Vec::new();
    header_only.extend_from_slice(&MGMT_MAGIC);
    push_u32(&mut header_only, MAX_BODY_LEN as u32);
    let mut s = FrameSplitter::new();
    // Not enough body bytes yet -- carry, not an error.
    assert_eq!(feed_ok(&mut s, &header_only), Vec::new());
}

// ---- unknown tag / trailing bytes / unknown reason -----------------

#[test]
fn unknown_mgmt_tag_errors() {
    let wire = wrap(MGMT_MAGIC, vec![0x99]).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_err(&mut s, &wire), WireError::UnknownTag(0x99));
}

#[test]
fn unknown_attach_tag_errors() {
    let wire = wrap(ATTACH_MAGIC, vec![0x50]).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_err(&mut s, &wire), WireError::UnknownTag(0x50));
}

#[test]
fn trailing_bytes_after_a_fixed_body_errors() {
    let wire = wrap(MGMT_MAGIC, vec![TAG_MGMT_REQ_PROBE, 0xff]).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_err(&mut s, &wire), WireError::TrailingBytes("probe"));
}

#[test]
fn unknown_survival_value_errors() {
    let mut body = vec![TAG_MGMT_REP_STATUS_OK];
    push_u32(&mut body, 1);
    push_u64(&mut body, 1);
    body.push(2); // neither 0 (normal) nor 1 (degraded)
    let wire = wrap(MGMT_MAGIC, body).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_err(&mut s, &wire),
        WireError::UnknownEnumValue {
            field: "status_ok.survival",
            value: 2
        }
    );
}

#[test]
fn unknown_attach_refused_reason_errors() {
    let wire = wrap(ATTACH_MAGIC, vec![TAG_ATTACH_REP_ATTACH_REFUSED, 7]).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_err(&mut s, &wire),
        WireError::UnknownEnumValue {
            field: "attach_refused.reason",
            value: 7
        }
    );
}

#[test]
fn checkpoint_chunk_last_flag_must_be_0_or_1() {
    let wire = wrap(ATTACH_MAGIC, vec![TAG_ATTACH_REP_CHECKPOINT_CHUNK, 5]).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_err(&mut s, &wire),
        WireError::UnknownEnumValue {
            field: "checkpoint_chunk.last",
            value: 5
        }
    );
}

// ---- hello negotiation ---------------------------------------------

#[test]
fn negotiate_accepts_v1() {
    assert_eq!(negotiate(ATTACH_PROTO_V1), Negotiated::Accepted(ATTACH_PROTO_V1));
}

/// Codex round on #194: v2 landed alongside the scrollback ring, and
/// `negotiate` echoes back exactly what a client asked for -- never
/// silently upgrading v1 to v2, and accepting v2 as its own version,
/// not the older one.
#[test]
fn negotiate_accepts_v2() {
    assert_eq!(
        negotiate(ATTACH_PROTO_V2),
        Negotiated::Accepted(ATTACH_PROTO_V2)
    );
}

/// ADR 0046 decision 3, lane B3b1: v3 lands alongside the owner-
/// emitted pen/geometry events, with NO checkpoint-format
/// consequence (unlike v1→v2) — `negotiate` still echoes back
/// exactly what was asked, accepting v3 as its own version.
#[test]
fn negotiate_accepts_v3() {
    assert_eq!(negotiate(ATTACH_PROTO_V3), Negotiated::Accepted(ATTACH_PROTO_V3));
}

#[test]
fn negotiate_refuses_anything_else() {
    assert_eq!(
        negotiate(4),
        Negotiated::Refused { supported: ATTACH_PROTO_V3 }
    );
    assert_eq!(
        negotiate(0),
        Negotiated::Refused {
            supported: ATTACH_PROTO_V3
        }
    );
}

// ---- chunk arithmetic ------------------------------------------------

#[test]
fn greedy_chunking_of_the_max_checkpoint_uses_the_computed_count() {
    assert_eq!(MAX_CHECKPOINT_CHUNK_PAYLOAD, 1_048_574);
    assert_eq!(CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD, 12);

    let mut remaining = MAX_CHECKPOINT_LEN;
    let mut splitter = FrameSplitter::new();
    let mut recovered_len = 0usize;
    let mut chunk_count = 0usize;
    while remaining > 0 {
        let take = remaining.min(MAX_CHECKPOINT_CHUNK_PAYLOAD);
        let is_last = take == remaining;
        let bytes = vec![0xabu8; take];
        let frame = AttachServer::CheckpointChunk {
            last: is_last,
            bytes,
        };
        let wire = encode_attach_server(&frame).expect("within the frame cap");
        assert!(
            wire.len() <= HEADER_LEN + MAX_BODY_LEN,
            "checkpoint_chunk frame exceeds the outer 1 MiB cap"
        );
        let decoded = feed_ok(&mut splitter, &wire);
        assert_eq!(decoded.len(), 1);
        match &decoded[0] {
            DecodedFrame::AttachServer(AttachServer::CheckpointChunk { last, bytes }) => {
                assert_eq!(*last, is_last);
                recovered_len += bytes.len();
            }
            other => panic!("expected a checkpoint_chunk, got {other:?}"),
        }
        remaining -= take;
        chunk_count += 1;
    }
    assert_eq!(chunk_count, CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD);
    assert_eq!(recovered_len, MAX_CHECKPOINT_LEN);
}

#[test]
fn more_chunks_than_the_max_payload_count_decode_fine() {
    // CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD is what a GREEDY encoder
    // produces at the largest possible checkpoint -- it is NOT a
    // protocol ceiling. A sender using smaller chunks may legally
    // emit more of them; the wire never counts or caps
    // `checkpoint_chunk` frames, only bytes-per-frame.
    let mut splitter = FrameSplitter::new();
    let total_chunks = CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD + 5;
    for i in 0..total_chunks {
        let wire = encode_attach_server(&AttachServer::CheckpointChunk {
            last: i + 1 == total_chunks,
            bytes: vec![0x11u8; 4],
        })
        .unwrap();
        assert_eq!(feed_ok(&mut splitter, &wire).len(), 1);
    }
}

// ---- should-fix 3: error semantics + the failed-state latch --------

#[test]
fn feeding_after_failure_ignores_bytes_frees_memory_and_repeats_the_error() {
    let bad = wrap(MGMT_MAGIC, vec![0x99]).unwrap(); // unknown tag
    let mut s = FrameSplitter::new();
    let first_err = feed_err(&mut s, &bad);
    assert_eq!(first_err, WireError::UnknownTag(0x99));
    assert!(s.buf.is_empty(), "buffer must be dropped once failed");

    // A large blob of unrelated garbage must not grow memory or
    // change the answer.
    let garbage = vec![0xffu8; 5 * MAX_BODY_LEN];
    let (frames, err) = s.feed(&garbage);
    assert!(frames.is_empty());
    assert_eq!(err, Some(first_err.clone()));
    assert!(s.buf.is_empty(), "a failed splitter must never retain fed bytes");

    // And again -- the answer must not change call to call.
    let (frames2, err2) = s.feed(&[1, 2, 3]);
    assert!(frames2.is_empty());
    assert_eq!(err2, Some(first_err));
}

#[test]
fn frames_decoded_before_a_same_call_error_are_still_returned() {
    // Two valid probe frames followed by one with an unknown tag, all
    // fed in a SINGLE `feed` call: the first two must come back
    // alongside the error, not be dropped by it.
    let mut bytes = Vec::new();
    bytes.extend(encode_mgmt_request(&MgmtRequest::Probe).unwrap());
    bytes.extend(encode_mgmt_request(&MgmtRequest::Status).unwrap());
    bytes.extend(wrap(MGMT_MAGIC, vec![0x99]).unwrap());

    let mut s = FrameSplitter::new();
    let (frames, err) = s.feed(&bytes);
    assert_eq!(
        frames,
        vec![
            DecodedFrame::MgmtRequest(MgmtRequest::Probe),
            DecodedFrame::MgmtRequest(MgmtRequest::Status),
        ]
    );
    assert_eq!(err, Some(WireError::UnknownTag(0x99)));
}

/// The pinned literal above must never drift from the fork's proven
/// bound. The fork is a windows-gated dependency of this crate, so the
/// cross-check runs on the windows CI legs — which is where the number
/// is ever consumed.
#[cfg(windows)]
#[test]
fn pinned_checkpoint_len_matches_the_fork() {
    assert_eq!(MAX_CHECKPOINT_LEN, vt100_ctt::MAX_CHECKPOINT_LEN);
}

