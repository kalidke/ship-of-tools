//! Golden bytes for the mgmt lane.

use super::support_tests::*;
use super::*;

// ---- goldens -------------------------------------------------------
//
// These pin the exact bytes for every frame in the lane. The mgmt
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
