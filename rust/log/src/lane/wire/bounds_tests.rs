//! Field-bound and edge-completeness tests for the mgmt lane.

use super::support_tests::*;
use super::*;

// ---- bounds -------------------------------------------------------

#[test]
fn shutdown_reason_over_128_bytes_refused_at_encode() {
    let err = encode_mgmt_request(&MgmtRequest::Shutdown {
        reason: "x".repeat(129),
    })
    .unwrap_err();
    assert_eq!(
        err,
        WireError::FieldTooLarge {
            field: "shutdown.reason",
            len: 129,
            max: 128
        }
    );
}

#[test]
fn shutdown_reason_over_128_bytes_refused_at_decode() {
    let mut body = vec![TAG_MGMT_REQ_SHUTDOWN, 129u8];
    body.extend(std::iter::repeat_n(b'x', 129));
    let wire = wrap(MGMT_MAGIC, body).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_err(&mut s, &wire),
        WireError::FieldTooLarge {
            field: "shutdown.reason",
            len: 129,
            max: 128
        }
    );
}

#[test]
fn non_utf8_shutdown_reason_refused_at_decode() {
    let body = vec![TAG_MGMT_REQ_SHUTDOWN, 1u8, 0xff];
    let wire = wrap(MGMT_MAGIC, body).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_err(&mut s, &wire),
        WireError::InvalidUtf8("shutdown.reason")
    );
}

// ---- should-fix 5: golden/edge completeness --------------------------

#[test]
fn shutdown_reason_at_exactly_128_bytes_is_legal_both_ways() {
    let reason = "x".repeat(128);
    let wire = encode_mgmt_request(&MgmtRequest::Shutdown {
        reason: reason.clone(),
    })
    .unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::MgmtRequest(MgmtRequest::Shutdown { reason })]
    );
}

#[test]
fn status_ok_body_one_byte_short_errors() {
    let mut body = vec![TAG_MGMT_REP_STATUS_OK];
    push_u32(&mut body, 1);
    push_u64(&mut body, 2);
    // Missing the trailing survival byte: 13 bytes total, not 14.
    let wire = wrap(MGMT_MAGIC, body).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_err(&mut s, &wire), WireError::Malformed("status_ok.survival"));
}

#[test]
fn status_ok_body_one_byte_long_errors() {
    let mut body = vec![TAG_MGMT_REP_STATUS_OK];
    push_u32(&mut body, 1);
    push_u64(&mut body, 2);
    body.push(0); // survival = normal
    body.push(0xff); // one byte past the defined shape
    let wire = wrap(MGMT_MAGIC, body).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_err(&mut s, &wire), WireError::TrailingBytes("status_ok"));
}

