//! Goldens, bounds and splitter behaviour for the supervisor lane.

use super::support_tests::*;
use super::*;

// ---- ADR 0041 step 6 U2: the supervisor lane -----------------------

#[test]
fn golden_supervisor_hello() {
    let wire = encode_supervisor_request(&SupervisorRequest::Hello {
        proto: SUPERVISOR_PROTO_V1,
        build: "abc".to_string(),
    })
    .unwrap();
    assert_golden(
        wire.clone(),
        &[
            0x53, 0x4f, 0x53, 0x56, // SOSV
            0x09, 0x00, 0x00, 0x00, // len = 9
            0x01, // tag: hello
            0x01, 0x00, 0x00, 0x00, // proto = 1
            0x03, 0x61, 0x62, 0x63, // build = "abc"
        ],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::SupervisorRequest(SupervisorRequest::Hello {
            proto: SUPERVISOR_PROTO_V1,
            build: "abc".to_string(),
        })]
    );
}

#[test]
fn golden_supervisor_hello_ok() {
    let wire = encode_supervisor_reply(&SupervisorReply::HelloOk {
        proto: SUPERVISOR_PROTO_V1,
        build: "abc".to_string(),
        pid: 42,
        created: 7,
    })
    .unwrap();
    assert_golden(
        wire.clone(),
        &[
            0x53, 0x4f, 0x53, 0x56, // SOSV
            0x15, 0x00, 0x00, 0x00, // len = 21
            0x81, // tag: hello_ok
            0x01, 0x00, 0x00, 0x00, // proto = 1
            0x03, 0x61, 0x62, 0x63, // build = "abc"
            0x2a, 0x00, 0x00, 0x00, // pid = 42
            0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // created = 7
        ],
    );
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::SupervisorReply(SupervisorReply::HelloOk {
            proto: SUPERVISOR_PROTO_V1,
            build: "abc".to_string(),
            pid: 42,
            created: 7,
        })]
    );
}

#[test]
fn supervisor_hello_refused_round_trips() {
    for reason in [
        SupervisorRefusedReason::VersionSkew,
        SupervisorRefusedReason::StaleVoyage,
        SupervisorRefusedReason::IdConflict,
    ] {
        let wire = encode_supervisor_reply(&SupervisorReply::Refused { reason }).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::SupervisorReply(SupervisorReply::Refused { reason })]
        );
    }
}

#[test]
fn supervisor_command_end_run_round_trips() {
    let req = SupervisorRequest::Command {
        operation_id: "op-1".to_string(),
        op: SupervisorOp::EndRun {
            reason: "quit".to_string(),
            voyage: "voy-1".to_string(),
        },
    };
    let wire = encode_supervisor_request(&req).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_ok(&mut s, &wire), vec![DecodedFrame::SupervisorRequest(req)]);
}

#[test]
fn supervisor_command_reset_round_trips_with_and_without_voyage() {
    for voyage in [None, Some("voy-1".to_string())] {
        let req = SupervisorRequest::Command {
            operation_id: "op-2".to_string(),
            op: SupervisorOp::Reset { voyage: voyage.clone() },
        };
        let wire = encode_supervisor_request(&req).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(feed_ok(&mut s, &wire), vec![DecodedFrame::SupervisorRequest(req)]);
    }
}

#[test]
fn supervisor_command_stop_round_trips() {
    let req = SupervisorRequest::Command {
        operation_id: "op-3".to_string(),
        op: SupervisorOp::Stop,
    };
    let wire = encode_supervisor_request(&req).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_ok(&mut s, &wire), vec![DecodedFrame::SupervisorRequest(req)]);
}

#[test]
fn supervisor_status_request_round_trips() {
    let wire = encode_supervisor_request(&SupervisorRequest::Status).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::SupervisorRequest(SupervisorRequest::Status)]
    );
}

#[test]
fn supervisor_query_round_trips() {
    let req = SupervisorRequest::Query { operation_id: "op-4".to_string() };
    let wire = encode_supervisor_request(&req).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_ok(&mut s, &wire), vec![DecodedFrame::SupervisorRequest(req)]);
}

#[test]
fn supervisor_status_ok_round_trips_with_and_without_leg() {
    for (voyage, leg) in [(None, None), (Some("voy-1".to_string()), Some(7u64))] {
        let reply = SupervisorReply::StatusOk {
            pid: 1,
            created: 2,
            voyage: voyage.clone(),
            leg,
            phase: SupervisorPhase::Ready,
        };
        let wire = encode_supervisor_reply(&reply).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(feed_ok(&mut s, &wire), vec![DecodedFrame::SupervisorReply(reply)]);
    }
}

#[test]
fn supervisor_status_ok_every_phase_round_trips() {
    for phase in [
        SupervisorPhase::Starting,
        SupervisorPhase::Ready,
        SupervisorPhase::Ending,
        SupervisorPhase::EndedNoRespawn,
        SupervisorPhase::Terminal,
    ] {
        let reply = SupervisorReply::StatusOk { pid: 1, created: 2, voyage: None, leg: None, phase };
        let wire = encode_supervisor_reply(&reply).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(feed_ok(&mut s, &wire), vec![DecodedFrame::SupervisorReply(reply)]);
    }
}

#[test]
fn supervisor_operation_state_every_variant_round_trips() {
    let states = [
        SupervisorOperationState::Accepted,
        SupervisorOperationState::RecordClosed,
        SupervisorOperationState::RecordVerified,
        SupervisorOperationState::ResetDone { new_voyage: "voy-2".to_string() },
        SupervisorOperationState::Stopping,
        SupervisorOperationState::Failed { detail: "record_append".to_string() },
        SupervisorOperationState::Refused { reason: SupervisorRefusedReason::StaleVoyage },
        SupervisorOperationState::UnknownOperation,
    ];
    for state in states {
        let reply = SupervisorReply::Operation(state.clone());
        let wire = encode_supervisor_reply(&reply).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(feed_ok(&mut s, &wire), vec![DecodedFrame::SupervisorReply(reply)]);
    }
}

#[test]
fn supervisor_string_fields_over_bound_refused_at_encode() {
    let too_long = "a".repeat(MAX_SUPERVISOR_STRING_LEN + 1);
    assert!(matches!(
        encode_supervisor_request(&SupervisorRequest::Hello { proto: 1, build: too_long.clone() }),
        Err(WireError::FieldTooLarge { field: "hello.build", .. })
    ));
    assert!(matches!(
        encode_supervisor_request(&SupervisorRequest::Command {
            operation_id: too_long.clone(),
            op: SupervisorOp::Stop,
        }),
        Err(WireError::FieldTooLarge { field: "command.operation_id", .. })
    ));
}

#[test]
fn operation_id_at_exactly_the_64_byte_bound_is_legal_and_one_over_is_refused() {
    let at_bound = "a".repeat(MAX_OPERATION_ID_LEN);
    let over_bound = "a".repeat(MAX_OPERATION_ID_LEN + 1);
    let wire = encode_supervisor_request(&SupervisorRequest::Query { operation_id: at_bound.clone() }).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(
        feed_ok(&mut s, &wire),
        vec![DecodedFrame::SupervisorRequest(SupervisorRequest::Query { operation_id: at_bound })]
    );
    assert!(matches!(
        encode_supervisor_request(&SupervisorRequest::Query { operation_id: over_bound }),
        Err(WireError::FieldTooLarge { field: "query.operation_id", .. })
    ));
}

/// ADR 0041 step 6 U2, Codex review finding 6: `operation_id` is
/// interpolated directly into a Windows filesystem path with no
/// further sanitization downstream, so the wire itself must refuse
/// separators, `..`, and anything outside `[A-Za-z0-9._-]` at DECODE
/// time -- the journal must never see an unvalidated id. Every
/// rejected id here is well-formed enough to pass `bounded_string`'s
/// own length/emptiness checks first, so this exercises the
/// CHARSET/reserved-name check specifically, not length.
#[test]
fn illegal_operation_ids_are_refused_at_decode_not_merely_at_the_journal() {
    for illegal in ["..", ".", "../escape", "a/b", r"a\b", "a b", "a:b", "", ] {
        if illegal.is_empty() {
            // Empty is refused by `FieldEmpty`, a separate, already-
            // covered check -- skip it here to keep this loop's own
            // assertion about `InvalidOperationId` precise.
            continue;
        }
        let wire = wrap(SUPERVISOR_MAGIC, {
            let mut body = vec![TAG_SV_REQ_QUERY];
            push_bounded_string(&mut body, illegal, MAX_OPERATION_ID_LEN, "query.operation_id", true).unwrap();
            body
        })
        .unwrap();
        let mut s = FrameSplitter::new();
        let err = feed_err(&mut s, &wire);
        assert!(
            matches!(&err, WireError::InvalidOperationId { field: "query.operation_id", value } if value == illegal),
            "expected InvalidOperationId for {illegal:?}, got {err:?}"
        );
    }
}

#[test]
fn legal_operation_ids_cover_the_whole_allowed_charset() {
    for legal in ["op-1", "op_2", "op.3", "ABC123", "a", "UUID-like-9f8e7d6c"] {
        let wire = encode_supervisor_request(&SupervisorRequest::Query { operation_id: legal.to_string() }).unwrap();
        let mut s = FrameSplitter::new();
        assert_eq!(
            feed_ok(&mut s, &wire),
            vec![DecodedFrame::SupervisorRequest(SupervisorRequest::Query { operation_id: legal.to_string() })]
        );
    }
}

/// The digest input is the SAME canonical bytes `command`'s own wire
/// body carries `op` in — never `format!("{op:?}")` (Codex review
/// finding 6: "neither a digest nor a stable durable encoding across
/// builds").
#[test]
fn canonical_supervisor_op_bytes_is_stable_and_distinguishes_ops() {
    let a = SupervisorOp::EndRun { reason: "r".into(), voyage: "v".into() };
    let b = SupervisorOp::EndRun { reason: "r".into(), voyage: "v".into() };
    let c = SupervisorOp::EndRun { reason: "different".into(), voyage: "v".into() };
    let d = SupervisorOp::Stop;
    assert_eq!(canonical_supervisor_op_bytes(&a).unwrap(), canonical_supervisor_op_bytes(&b).unwrap());
    assert_ne!(canonical_supervisor_op_bytes(&a).unwrap(), canonical_supervisor_op_bytes(&c).unwrap());
    assert_ne!(canonical_supervisor_op_bytes(&a).unwrap(), canonical_supervisor_op_bytes(&d).unwrap());
}

#[test]
fn supervisor_operation_id_and_voyage_must_not_be_empty() {
    assert!(matches!(
        encode_supervisor_request(&SupervisorRequest::Command {
            operation_id: String::new(),
            op: SupervisorOp::Stop,
        }),
        Err(WireError::FieldEmpty("command.operation_id"))
    ));
    assert!(matches!(
        encode_supervisor_request(&SupervisorRequest::Command {
            operation_id: "op".to_string(),
            op: SupervisorOp::EndRun { reason: String::new(), voyage: String::new() },
        }),
        Err(WireError::FieldEmpty("command.end_run.voyage"))
    ));
}

#[test]
fn unknown_supervisor_tag_errors() {
    let wire = wrap(SUPERVISOR_MAGIC, vec![0x77]).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_err(&mut s, &wire), WireError::UnknownTag(0x77));
}

#[test]
fn unknown_supervisor_op_tag_errors() {
    let mut body = Vec::new();
    body.push(TAG_SV_REQ_COMMAND);
    push_bounded_string(&mut body, "op", MAX_SUPERVISOR_STRING_LEN, "operation_id", true).unwrap();
    body.push(0x77); // unrecognized op tag
    let wire = wrap(SUPERVISOR_MAGIC, body).unwrap();
    let mut s = FrameSplitter::new();
    assert_eq!(feed_err(&mut s, &wire), WireError::UnknownTag(0x77));
}

#[test]
fn mgmt_then_supervisor_is_a_lane_mismatch() {
    let mgmt = encode_mgmt_request(&MgmtRequest::Probe).unwrap();
    let sv = encode_supervisor_request(&SupervisorRequest::Status).unwrap();
    let mut s = FrameSplitter::new();
    feed_ok(&mut s, &mgmt);
    let err = feed_err(&mut s, &sv);
    assert_eq!(err, WireError::LaneMismatch { latched: MGMT_MAGIC, got: SUPERVISOR_MAGIC });
}

#[test]
fn supervisor_frame_split_across_multiple_feed_calls() {
    let wire = encode_supervisor_request(&SupervisorRequest::Query {
        operation_id: "op-split".to_string(),
    })
    .unwrap();
    let mut s = FrameSplitter::new();
    for i in 0..wire.len() {
        let (frames, err) = s.feed(&wire[i..i + 1]);
        assert_eq!(err, None);
        if i + 1 < wire.len() {
            assert!(frames.is_empty());
        } else {
            assert_eq!(
                frames,
                vec![DecodedFrame::SupervisorRequest(SupervisorRequest::Query {
                    operation_id: "op-split".to_string()
                })]
            );
        }
    }
}
