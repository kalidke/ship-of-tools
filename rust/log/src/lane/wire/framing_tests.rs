//! Framing tests: lane binding, caps, unknown tags, trailing bytes, the failed latch.

use super::support_tests::*;
use super::*;

// ---- lane binding ---------------------------------------------------

#[test]
fn mgmt_then_supervisor_is_a_lane_mismatch() {
    let mgmt = encode_mgmt_request(&MgmtRequest::Probe).unwrap();
    let supervisor = wrap(SUPERVISOR_MAGIC, vec![TAG_SV_REQ_STATUS]).unwrap();
    let mut s = FrameSplitter::new();
    feed_ok(&mut s, &mgmt);
    let err = feed_err(&mut s, &supervisor);
    assert_eq!(
        err,
        WireError::LaneMismatch {
            latched: MGMT_MAGIC,
            got: SUPERVISOR_MAGIC
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

// ---- hello negotiation ---------------------------------------------

// ---- chunk arithmetic ------------------------------------------------

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

