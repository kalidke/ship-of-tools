//! Field-bound and edge-completeness tests for the mgmt and attach lanes.

use super::support_tests::*;
use super::*;

// ---- bounds -------------------------------------------------------

#[test]
fn controller_id_over_128_bytes_refused_at_encode() {
    let controller_id = "a".repeat(129);
    let err = encode_attach_client(&AttachClient::Attach { controller_id }).unwrap_err();
    assert_eq!(
        err,
        WireError::FieldTooLarge {
            field: "controller_id",
            len: 129,
            max: 128
        }
    );
}

#[test]
fn controller_id_over_128_bytes_refused_at_decode() {
    // Hand-built: a 129-byte controller_id, which no `encode_*`
    // helper here will ever produce, but a hostile or buggy peer
    // could send.
    let mut body = vec![TAG_ATTACH_REQ_ATTACH, 129u8];
    body.extend(std::iter::repeat_n(b'a', 129));
    let wire = wrap(ATTACH_MAGIC, body).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_err(&mut s, &wire),
        WireError::FieldTooLarge {
            field: "controller_id",
            len: 129,
            max: 128
        }
    );
}

#[test]
fn input_payload_over_8192_bytes_refused_at_encode() {
    let err = encode_attach_client(&AttachClient::Input {
        controller_id: "c".to_string(),
        take_epoch: 0,
        idem_key: [0; 16],
        payload: vec![0u8; 8193],
    })
    .unwrap_err();
    assert_eq!(
        err,
        WireError::FieldTooLarge {
            field: "input.payload",
            len: 8193,
            max: 8192
        }
    );
}

#[test]
fn input_payload_over_8192_bytes_refused_at_decode() {
    let mut body = vec![TAG_ATTACH_REQ_INPUT, 1u8, b'c'];
    push_u64(&mut body, 0);
    body.extend_from_slice(&[0u8; 16]);
    push_u16(&mut body, 8193);
    body.extend(std::iter::repeat_n(0u8, 8193));
    let wire = wrap(ATTACH_MAGIC, body).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_err(&mut s, &wire),
        WireError::FieldTooLarge {
            field: "input.payload",
            len: 8193,
            max: 8192
        }
    );
}

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
fn non_utf8_controller_id_refused_at_decode() {
    let body = vec![TAG_ATTACH_REQ_ATTACH, 1u8, 0xff];
    let wire = wrap(ATTACH_MAGIC, body).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_err(&mut s, &wire),
        WireError::InvalidUtf8("controller_id")
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
fn golden_attach_refused_subscriber_cap() {
    let wire = encode_attach_server(&AttachServer::AttachRefused {
        reason: AttachRefusedReason::SubscriberCap,
    })
    .unwrap();
    assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x02, 0x00, 0x00, 0x00, 0x84, 0x01]);
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::AttachRefused {
            reason: AttachRefusedReason::SubscriberCap
        })]
    );
}

#[test]
fn golden_take_refused_checkpoint_in_flight() {
    let wire = encode_attach_server(&AttachServer::TakeRefused {
        reason: TakeRefusedReason::CheckpointInFlight,
    })
    .unwrap();
    assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x02, 0x00, 0x00, 0x00, 0x87, 0x01]);
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::TakeRefused {
            reason: TakeRefusedReason::CheckpointInFlight
        })]
    );
}

#[test]
fn golden_resize_refused_not_driver() {
    let wire = encode_attach_server(&AttachServer::ResizeRefused {
        reason: ResizeRefusedReason::NotDriver,
    })
    .unwrap();
    assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x02, 0x00, 0x00, 0x00, 0x8c, 0x01]);
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::ResizeRefused {
            reason: ResizeRefusedReason::NotDriver
        })]
    );
}

#[test]
fn golden_attach_checkpoint_chunk_not_last() {
    let wire = encode_attach_server(&AttachServer::CheckpointChunk {
        last: false,
        bytes: b"AB".to_vec(),
    })
    .unwrap();
    assert_golden(
        wire.clone(),
        &[0x53, 0x4f, 0x41, 0x30, 0x04, 0x00, 0x00, 0x00, 0x83, 0x00, 0x41, 0x42],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::CheckpointChunk {
            last: false,
            bytes: b"AB".to_vec()
        })]
    );
}

#[test]
fn controller_id_at_exactly_128_bytes_is_legal_both_ways() {
    let controller_id = "a".repeat(128);
    let wire = encode_attach_client(&AttachClient::Attach {
        controller_id: controller_id.clone(),
    })
    .unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachClient(AttachClient::Attach { controller_id })]
    );
}

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
fn input_payload_at_exactly_8192_bytes_is_legal_both_ways() {
    let payload = vec![0xabu8; 8192];
    let wire = encode_attach_client(&AttachClient::Input {
        controller_id: "c".to_string(),
        take_epoch: 1,
        idem_key: [7u8; 16],
        payload: payload.clone(),
    })
    .unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachClient(AttachClient::Input {
            controller_id: "c".to_string(),
            take_epoch: 1,
            idem_key: [7u8; 16],
            payload,
        })]
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

#[test]
fn take_ok_body_one_byte_short_errors() {
    let mut body = vec![TAG_ATTACH_REP_TAKE_OK];
    body.extend_from_slice(&9u64.to_le_bytes()[..7]); // 7 of take_epoch's 8 bytes
    let wire = wrap(ATTACH_MAGIC, body).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_err(&mut s, &wire), WireError::Malformed("take_ok.take_epoch"));
}

#[test]
fn take_ok_body_one_byte_long_errors() {
    let mut body = vec![TAG_ATTACH_REP_TAKE_OK];
    push_u64(&mut body, 9);
    body.push(0xff);
    let wire = wrap(ATTACH_MAGIC, body).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_err(&mut s, &wire), WireError::TrailingBytes("take_ok"));
}

#[test]
fn resize_body_one_byte_short_errors() {
    let mut body = vec![TAG_ATTACH_REQ_RESIZE];
    push_u16(&mut body, 80);
    body.push(24); // only 1 of rows' 2 bytes
    let wire = wrap(ATTACH_MAGIC, body).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_err(&mut s, &wire), WireError::Malformed("resize.rows"));
}

#[test]
fn resize_body_one_byte_long_errors() {
    let mut body = vec![TAG_ATTACH_REQ_RESIZE];
    push_u16(&mut body, 80);
    push_u16(&mut body, 24);
    body.push(0xff);
    let wire = wrap(ATTACH_MAGIC, body).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_err(&mut s, &wire), WireError::TrailingBytes("resize"));
}

#[test]
fn empty_controller_id_rejected_at_encode() {
    let err = encode_attach_client(&AttachClient::Attach {
        controller_id: String::new(),
    })
    .unwrap_err();
    assert_eq!(err, WireError::FieldEmpty("controller_id"));
}

#[test]
fn empty_controller_id_rejected_at_decode() {
    let body = vec![TAG_ATTACH_REQ_ATTACH, 0u8]; // len = 0
    let wire = wrap(ATTACH_MAGIC, body).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_err(&mut s, &wire), WireError::FieldEmpty("controller_id"));
}

#[test]
fn empty_input_payload_is_legal() {
    // Deliberate, unlike controller_id: a payload field's "nothing
    // this round" is a real, meaningful state, not a malformed
    // identity (see "Field minimums" in the module doc).
    let wire = encode_attach_client(&AttachClient::Input {
        controller_id: "c".to_string(),
        take_epoch: 0,
        idem_key: [0; 16],
        payload: Vec::new(),
    })
    .unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachClient(AttachClient::Input {
            controller_id: "c".to_string(),
            take_epoch: 0,
            idem_key: [0; 16],
            payload: Vec::new(),
        })]
    );
}

#[test]
fn empty_output_bytes_is_legal() {
    // Deliberate, same reasoning as the input payload above.
    let wire = encode_attach_server(&AttachServer::Output { bytes: Vec::new() }).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::Output { bytes: Vec::new() })]
    );
}

#[test]
fn empty_non_final_checkpoint_chunk_is_legal() {
    // Deliberate: a zero-byte non-final chunk is unusual but not
    // malformed -- the wire does not encode "why" a writer produced
    // one, and total-byte bounding is the consumer's job (see
    // "Checkpoint chunk arithmetic" in the module doc).
    let wire = encode_attach_server(&AttachServer::CheckpointChunk {
        last: false,
        bytes: Vec::new(),
    })
    .unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::CheckpointChunk {
            last: false,
            bytes: Vec::new()
        })]
    );
}

