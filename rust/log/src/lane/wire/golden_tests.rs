//! Golden bytes for the mgmt and attach lanes, keepalive, pen and geometry frames.

use super::support_tests::*;
use super::*;

// ---- goldens -------------------------------------------------------
//
// These pin the exact bytes for every frame in both lanes. The mgmt
// lane's shapes are PERMANENTLY PINNED (ADR 0041): these bytes are the
// compatibility record a future build must keep reading, not just a
// snapshot of what this build happens to emit today.

#[test]
fn golden_mgmt_probe() {
    let wire = encode_mgmt_request(&MgmtRequest::Probe).unwrap();
    assert_golden(wire.clone(), &[0x53, 0x4f, 0x4d, 0x30, 0x01, 0x00, 0x00, 0x00, 0x01]);
    let mut s = FrameSplitter::new();
    let decoded = feed_ok(&mut s, &wire);
    assert_eq!(decoded, vec![DecodedFrame::MgmtRequest(MgmtRequest::Probe)]);
}

#[test]
fn golden_mgmt_status() {
    let wire = encode_mgmt_request(&MgmtRequest::Status).unwrap();
    assert_golden(wire.clone(), &[0x53, 0x4f, 0x4d, 0x30, 0x01, 0x00, 0x00, 0x00, 0x02]);
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::MgmtRequest(MgmtRequest::Status)]
    );
}

#[test]
fn golden_mgmt_shutdown() {
    let wire = encode_mgmt_request(&MgmtRequest::Shutdown {
        reason: "bye".to_string(),
    })
    .unwrap();
    assert_golden(
        wire.clone(),
        &[
            0x53, 0x4f, 0x4d, 0x30, // SOM0
            0x05, 0x00, 0x00, 0x00, // len = 5
            0x03, // tag
            0x03, // reason len = 3
            0x62, 0x79, 0x65, // "bye"
        ],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::MgmtRequest(MgmtRequest::Shutdown {
            reason: "bye".to_string()
        })]
    );
}

#[test]
fn golden_mgmt_probe_ok() {
    let wire = encode_mgmt_reply(&MgmtReply::ProbeOk).unwrap();
    assert_golden(wire.clone(), &[0x53, 0x4f, 0x4d, 0x30, 0x01, 0x00, 0x00, 0x00, 0x81]);
    let mut s = FrameSplitter::new();
    assert_eq!(feed_ok(&mut s, &wire), vec![DecodedFrame::MgmtReply(MgmtReply::ProbeOk)]);
}

#[test]
fn golden_mgmt_status_ok() {
    let wire = encode_mgmt_reply(&MgmtReply::StatusOk {
        pid: 1,
        created: 2,
        survival: Survival::Normal,
    })
    .unwrap();
    assert_golden(
        wire.clone(),
        &[
            0x53, 0x4f, 0x4d, 0x30, // SOM0
            0x0e, 0x00, 0x00, 0x00, // len = 14
            0x82, // tag
            0x01, 0x00, 0x00, 0x00, // pid = 1
            0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // created = 2
            0x00, // survival = normal
        ],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::MgmtReply(MgmtReply::StatusOk {
            pid: 1,
            created: 2,
            survival: Survival::Normal
        })]
    );
}

#[test]
fn golden_mgmt_status_ok_degraded() {
    let wire = encode_mgmt_reply(&MgmtReply::StatusOk {
        pid: 0xdead_beef,
        created: 0x0102_0304_0506_0708,
        survival: Survival::Degraded,
    })
    .unwrap();
    assert_golden(
        wire.clone(),
        &[
            0x53, 0x4f, 0x4d, 0x30, 0x0e, 0x00, 0x00, 0x00, 0x82, 0xef, 0xbe, 0xad, 0xde,
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x01,
        ],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::MgmtReply(MgmtReply::StatusOk {
            pid: 0xdead_beef,
            created: 0x0102_0304_0506_0708,
            survival: Survival::Degraded
        })]
    );
}

#[test]
fn golden_mgmt_shutdown_ok() {
    let wire = encode_mgmt_reply(&MgmtReply::ShutdownOk).unwrap();
    assert_golden(wire.clone(), &[0x53, 0x4f, 0x4d, 0x30, 0x01, 0x00, 0x00, 0x00, 0x83]);
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::MgmtReply(MgmtReply::ShutdownOk)]
    );
}

#[test]
fn golden_attach_hello() {
    let wire = encode_attach_client(&AttachClient::Hello { proto: 1 }).unwrap();
    assert_golden(
        wire.clone(),
        &[0x53, 0x4f, 0x41, 0x30, 0x05, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachClient(AttachClient::Hello { proto: 1 })]
    );
}

#[test]
fn golden_attach_attach() {
    let wire = encode_attach_client(&AttachClient::Attach {
        controller_id: "c1".to_string(),
    })
    .unwrap();
    assert_golden(
        wire.clone(),
        &[0x53, 0x4f, 0x41, 0x30, 0x04, 0x00, 0x00, 0x00, 0x02, 0x02, 0x63, 0x31],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachClient(AttachClient::Attach {
            controller_id: "c1".to_string()
        })]
    );
}

#[test]
fn golden_attach_take() {
    let wire = encode_attach_client(&AttachClient::Take {
        controller_id: "c1".to_string(),
    })
    .unwrap();
    assert_golden(
        wire.clone(),
        &[0x53, 0x4f, 0x41, 0x30, 0x04, 0x00, 0x00, 0x00, 0x03, 0x02, 0x63, 0x31],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachClient(AttachClient::Take {
            controller_id: "c1".to_string()
        })]
    );
}

#[test]
fn golden_attach_input() {
    let idem_key: [u8; 16] = [
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
        0x1e, 0x1f,
    ];
    let wire = encode_attach_client(&AttachClient::Input {
        controller_id: "c1".to_string(),
        take_epoch: 7,
        idem_key,
        payload: b"hi".to_vec(),
    })
    .unwrap();
    assert_golden(
        wire.clone(),
        &[
            0x53, 0x4f, 0x41, 0x30, // SOA0
            0x20, 0x00, 0x00, 0x00, // len = 32
            0x04, // tag
            0x02, 0x63, 0x31, // controller_id "c1"
            0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // take_epoch = 7
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, // idem_key
            0x02, 0x00, // payload len = 2
            0x68, 0x69, // "hi"
        ],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachClient(AttachClient::Input {
            controller_id: "c1".to_string(),
            take_epoch: 7,
            idem_key,
            payload: b"hi".to_vec(),
        })]
    );
}

#[test]
fn golden_attach_resize() {
    let wire = encode_attach_client(&AttachClient::Resize { cols: 80, rows: 24 }).unwrap();
    assert_golden(
        wire.clone(),
        &[0x53, 0x4f, 0x41, 0x30, 0x05, 0x00, 0x00, 0x00, 0x05, 0x50, 0x00, 0x18, 0x00],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachClient(AttachClient::Resize { cols: 80, rows: 24 })]
    );
}

#[test]
fn golden_keepalive() {
    // One direction-neutral frame, not one per side (see the module
    // doc's tag-table note).
    let wire = encode_keepalive(42);
    assert_golden(
        wire.clone(),
        &[
            0x53, 0x4f, 0x41, 0x30, 0x09, 0x00, 0x00, 0x00, 0x06, 0x2a, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(feed_ok(&mut s, &wire), vec![DecodedFrame::Keepalive { nonce: 42 }]);
}

#[test]
fn keepalive_is_byte_identical_whichever_side_sends_it() {
    // The ADR pins ONE echo frame: the server originates it, and the
    // client's reply must be the identical bytes bounced back. There
    // is exactly one encoder, so "the server's frame" and "the
    // client's echo" are, by construction, the same bytes.
    let server_sent = encode_keepalive(0xdead_beef);
    let client_echo = encode_keepalive(0xdead_beef);
    assert_eq!(server_sent, client_echo);

    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &server_sent),
        vec![DecodedFrame::Keepalive { nonce: 0xdead_beef }]
    );
}

#[test]
fn golden_attach_hello_ok() {
    let wire = encode_attach_server(&AttachServer::HelloOk { proto: 1 }).unwrap();
    assert_golden(
        wire.clone(),
        &[0x53, 0x4f, 0x41, 0x30, 0x05, 0x00, 0x00, 0x00, 0x81, 0x01, 0x00, 0x00, 0x00],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::HelloOk { proto: 1 })]
    );
}

#[test]
fn golden_attach_hello_refused() {
    let wire = encode_attach_server(&AttachServer::HelloRefused { supported: 1 }).unwrap();
    assert_golden(
        wire.clone(),
        &[0x53, 0x4f, 0x41, 0x30, 0x05, 0x00, 0x00, 0x00, 0x82, 0x01, 0x00, 0x00, 0x00],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::HelloRefused { supported: 1 })]
    );
}

#[test]
fn golden_attach_checkpoint_chunk() {
    let wire = encode_attach_server(&AttachServer::CheckpointChunk {
        last: true,
        bytes: b"AB".to_vec(),
    })
    .unwrap();
    assert_golden(
        wire.clone(),
        &[0x53, 0x4f, 0x41, 0x30, 0x04, 0x00, 0x00, 0x00, 0x83, 0x01, 0x41, 0x42],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::CheckpointChunk {
            last: true,
            bytes: b"AB".to_vec()
        })]
    );
}

#[test]
fn golden_attach_refused() {
    let wire = encode_attach_server(&AttachServer::AttachRefused {
        reason: AttachRefusedReason::GroundTimeout,
    })
    .unwrap();
    assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x02, 0x00, 0x00, 0x00, 0x84, 0x00]);
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::AttachRefused {
            reason: AttachRefusedReason::GroundTimeout
        })]
    );
}

#[test]
fn golden_attach_output() {
    let wire = encode_attach_server(&AttachServer::Output {
        bytes: b"hi".to_vec(),
    })
    .unwrap();
    assert_golden(
        wire.clone(),
        &[0x53, 0x4f, 0x41, 0x30, 0x03, 0x00, 0x00, 0x00, 0x85, 0x68, 0x69],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::Output {
            bytes: b"hi".to_vec()
        })]
    );
}

#[test]
fn golden_attach_take_ok() {
    let wire = encode_attach_server(&AttachServer::TakeOk { take_epoch: 9 }).unwrap();
    assert_golden(
        wire.clone(),
        &[
            0x53, 0x4f, 0x41, 0x30, 0x09, 0x00, 0x00, 0x00, 0x86, 0x09, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::TakeOk { take_epoch: 9 })]
    );
}

#[test]
fn golden_take_refused() {
    let wire = encode_attach_server(&AttachServer::TakeRefused {
        reason: TakeRefusedReason::NotAttached,
    })
    .unwrap();
    assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x02, 0x00, 0x00, 0x00, 0x87, 0x00]);
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::TakeRefused {
            reason: TakeRefusedReason::NotAttached
        })]
    );
}

#[test]
fn golden_input_recorded() {
    let wire = encode_attach_server(&AttachServer::InputRecorded).unwrap();
    assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x01, 0x00, 0x00, 0x00, 0x88]);
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::InputRecorded)]
    );
}

#[test]
fn golden_input_refused_stale() {
    let wire = encode_attach_server(&AttachServer::InputRefusedStale).unwrap();
    assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x01, 0x00, 0x00, 0x00, 0x89]);
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::InputRefusedStale)]
    );
}

#[test]
fn golden_input_delivery_unknown() {
    let wire = encode_attach_server(&AttachServer::InputDeliveryUnknown).unwrap();
    assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x01, 0x00, 0x00, 0x00, 0x8a]);
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::InputDeliveryUnknown)]
    );
}

#[test]
fn golden_resize_ok() {
    let wire = encode_attach_server(&AttachServer::ResizeOk).unwrap();
    assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x01, 0x00, 0x00, 0x00, 0x8b]);
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::ResizeOk)]
    );
}

#[test]
fn golden_resize_refused() {
    let wire = encode_attach_server(&AttachServer::ResizeRefused {
        reason: ResizeRefusedReason::OutOfBudget,
    })
    .unwrap();
    assert_golden(wire.clone(), &[0x53, 0x4f, 0x41, 0x30, 0x02, 0x00, 0x00, 0x00, 0x8c, 0x00]);
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::ResizeRefused {
            reason: ResizeRefusedReason::OutOfBudget
        })]
    );
}

// ---- ADR 0046 decision 3 (lane B3b1): owner-emitted pen/geometry ----

#[test]
fn golden_pen_snapshot_with_holder() {
    let wire = encode_attach_server(&AttachServer::PenSnapshot {
        holder: Some("bob".to_string()),
        take_epoch: 9,
    })
    .unwrap();
    assert_golden(
        wire.clone(),
        &[
            0x53, 0x4f, 0x41, 0x30, 0x0e, 0x00, 0x00, 0x00, 0x8d, 0x01, 0x03, 0x62, 0x6f,
            0x62, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::PenSnapshot {
            holder: Some("bob".to_string()),
            take_epoch: 9
        })]
    );
}

#[test]
fn golden_pen_snapshot_no_holder() {
    let wire = encode_attach_server(&AttachServer::PenSnapshot { holder: None, take_epoch: 0 }).unwrap();
    assert_golden(
        wire.clone(),
        &[0x53, 0x4f, 0x41, 0x30, 0x0a, 0x00, 0x00, 0x00, 0x8d, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::PenSnapshot { holder: None, take_epoch: 0 })]
    );
}

#[test]
fn golden_pen_changed_with_holder() {
    let wire = encode_attach_server(&AttachServer::PenChanged {
        holder: Some("bob".to_string()),
        take_epoch: 9,
    })
    .unwrap();
    assert_golden(
        wire.clone(),
        &[
            0x53, 0x4f, 0x41, 0x30, 0x0e, 0x00, 0x00, 0x00, 0x8e, 0x01, 0x03, 0x62, 0x6f,
            0x62, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::PenChanged {
            holder: Some("bob".to_string()),
            take_epoch: 9
        })]
    );
}

#[test]
fn golden_pen_changed_no_holder() {
    let wire = encode_attach_server(&AttachServer::PenChanged { holder: None, take_epoch: 9 }).unwrap();
    assert_golden(
        wire.clone(),
        &[0x53, 0x4f, 0x41, 0x30, 0x0a, 0x00, 0x00, 0x00, 0x8e, 0x00, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::PenChanged { holder: None, take_epoch: 9 })]
    );
}

#[test]
fn golden_geometry() {
    let wire = encode_attach_server(&AttachServer::Geometry { cols: 120, rows: 40 }).unwrap();
    assert_golden(
        wire.clone(),
        &[0x53, 0x4f, 0x41, 0x30, 0x05, 0x00, 0x00, 0x00, 0x8f, 0x78, 0x00, 0x28, 0x00],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::AttachServer(AttachServer::Geometry { cols: 120, rows: 40 })]
    );
}

/// A holder longer than [`MAX_CONTROLLER_ID_LEN`] is refused at encode
/// time -- the same bound `controller_id` itself is held to
/// (`push_holder` reuses it: "a holder IS a controller_id").
#[test]
fn pen_changed_holder_over_bound_is_refused() {
    let too_long = "a".repeat(MAX_CONTROLLER_ID_LEN + 1);
    let err = encode_attach_server(&AttachServer::PenChanged { holder: Some(too_long), take_epoch: 1 }).unwrap_err();
    assert!(matches!(err, WireError::FieldTooLarge { field: "holder", .. }));
}

