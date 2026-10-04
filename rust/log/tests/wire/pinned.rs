//! Pinned lane fixtures: committed bytes that every later build must still decode identically.

use super::*;

// -----------------------------------------------------------------------
// supervisor-lane-v1 fixture (ADR 0045 decision 7): mirrors
// `tests/golden.rs:66-110`'s pattern -- an immutable committed file, one
// instance of EVERY top-level `SupervisorRequest`/`SupervisorReply`
// variant. Embedded via `include_bytes!`, not read at runtime: a
// committed fixture is immutable by construction, so this file carries
// no writer function for it -- a wire change is a NEW protocol version,
// proven by a SECOND fixture (a `-v2` file, hand-written or a one-off
// script), never a rewrite of this one. What this proves and what it
// does not: two processes agreeing on `SUPERVISOR_PROTO_V1` agree on
// these BYTES -- it cannot prove semantics, and command bytes feed
// durable journal digests (`wire.rs:1098`), so a lane bump that changes
// a command's encoding is ALSO a `journal::SCHEMA_VERSION` event. The
// NESTED variants this file leaves unpinned (`SupervisorOp::Reset`/
// `Stop`, every `SupervisorOperationState`, every `SupervisorPhase`,
// every `SupervisorRefusedReason`, `StatusOk`'s optional fields both
// present and absent) are covered by the coverage fixture below instead
// (Codex review finding, 2026-09-11: eight top-level variants alone
// leave `Stop`'s own tag, say, free to change without failing anything).
// -----------------------------------------------------------------------

/// Fixed field values, stable declaration order -- one instance of every
/// top-level `SupervisorRequest`/`SupervisorReply` variant.
fn supervisor_lane_v1_frames() -> Vec<Vec<u8>> {
    vec![
        encode_supervisor_request(&SupervisorRequest::Hello {
            proto: SUPERVISOR_PROTO_V1,
            build: "fixture-build".into(),
        })
        .unwrap(),
        encode_supervisor_request(&SupervisorRequest::Command {
            operation_id: "op-1".into(),
            op: SupervisorOp::EndRun {
                reason: "fixture".into(),
                voyage: "01900000-0000-7000-8000-000000000001".into(),
            },
        })
        .unwrap(),
        encode_supervisor_request(&SupervisorRequest::Status).unwrap(),
        encode_supervisor_request(&SupervisorRequest::Query { operation_id: "op-1".into() }).unwrap(),
        encode_supervisor_reply(&SupervisorReply::HelloOk {
            proto: SUPERVISOR_PROTO_V1,
            build: "fixture-build".into(),
            pid: 4242,
            created: 1_756_000_000,
        })
        .unwrap(),
        encode_supervisor_reply(&SupervisorReply::Refused { reason: SupervisorRefusedReason::VersionSkew }).unwrap(),
        encode_supervisor_reply(&SupervisorReply::Operation(SupervisorOperationState::Accepted)).unwrap(),
        encode_supervisor_reply(&SupervisorReply::StatusOk {
            pid: 4242,
            created: 1_756_000_000,
            voyage: Some("01900000-0000-7000-8000-000000000001".into()),
            leg: Some(7),
            phase: SupervisorPhase::Ready,
        })
        .unwrap(),
    ]
}

/// Pins the embedded `supervisor-lane-v1.bin` against a fresh encoding of
/// [`supervisor_lane_v1_frames`] -- see this section's own header doc for
/// what this proves and what it does not.
#[test]
fn supervisor_lane_v1_bytes_are_pinned() {
    let generated: Vec<u8> = supervisor_lane_v1_frames().into_iter().flatten().collect();
    const COMMITTED: &[u8] = include_bytes!("../fixtures/supervisor-lane-v1.bin");
    assert_eq!(
        COMMITTED, generated.as_slice(),
        "supervisor lane v1 wire bytes changed -- this is a NEW PROTOCOL VERSION \
         (ADR 0045 decision 7), not test drift: add a -v2 fixture instead of editing this one"
    );
}

// -----------------------------------------------------------------------
// supervisor-lane-v1-coverage fixture: the SAME immutability contract as
// the fixture above, in a SECOND embedded file -- never an edit to
// `supervisor-lane-v1.bin`, which stays byte-identical. Pins every
// NESTED variant and every optional-field absent/present form the
// top-level fixture leaves uncovered: `SupervisorOp::Reset` (both with
// and without its optional `voyage`) and `Stop`; all seven
// `SupervisorOperationState` variants; every `SupervisorPhase`; every
// `SupervisorRefusedReason`, both as a top-level `Refused` and inside
// `Operation(Refused)`; and `StatusOk`'s `voyage`/`leg` in their absent,
// mixed, and fully-present forms. Proven TWO ways, not just round-
// tripped: the concatenated bytes are pinned against the committed file
// exactly like the fixture above, AND those same committed bytes are
// independently DECODED through a fresh `FrameSplitter` and compared
// frame-for-frame against the expected `DecodedFrame` values -- a
// decoder bug that happens to still produce matching encoder output
// cannot hide behind the byte pin alone.
// -----------------------------------------------------------------------

/// One `(encoded bytes, expected decoded frame)` pair per nested variant
/// / optional-field form this fixture exists to cover.
fn supervisor_lane_v1_coverage_frames() -> Vec<(Vec<u8>, DecodedFrame)> {
    let req = |r: SupervisorRequest| {
        let bytes = encode_supervisor_request(&r).unwrap();
        (bytes, DecodedFrame::SupervisorRequest(r))
    };
    let rep = |r: SupervisorReply| {
        let bytes = encode_supervisor_reply(&r).unwrap();
        (bytes, DecodedFrame::SupervisorReply(r))
    };
    vec![
        req(SupervisorRequest::Command {
            operation_id: "op-2".into(),
            op: SupervisorOp::Reset { voyage: Some("01900000-0000-7000-8000-000000000002".into()) },
        }),
        req(SupervisorRequest::Command { operation_id: "op-3".into(), op: SupervisorOp::Reset { voyage: None } }),
        req(SupervisorRequest::Command { operation_id: "op-4".into(), op: SupervisorOp::Stop }),
        rep(SupervisorReply::Operation(SupervisorOperationState::RecordClosed)),
        rep(SupervisorReply::Operation(SupervisorOperationState::RecordVerified)),
        rep(SupervisorReply::Operation(SupervisorOperationState::ResetDone {
            new_voyage: "01900000-0000-7000-8000-000000000003".into(),
        })),
        rep(SupervisorReply::Operation(SupervisorOperationState::Stopping)),
        rep(SupervisorReply::Operation(SupervisorOperationState::Failed { detail: "fixture failure".into() })),
        rep(SupervisorReply::Operation(SupervisorOperationState::Refused {
            reason: SupervisorRefusedReason::StaleVoyage,
        })),
        rep(SupervisorReply::Operation(SupervisorOperationState::Refused {
            reason: SupervisorRefusedReason::IdConflict,
        })),
        rep(SupervisorReply::Operation(SupervisorOperationState::UnknownOperation)),
        rep(SupervisorReply::Refused { reason: SupervisorRefusedReason::StaleVoyage }),
        rep(SupervisorReply::Refused { reason: SupervisorRefusedReason::IdConflict }),
        rep(SupervisorReply::StatusOk {
            pid: 4242,
            created: 1_756_000_000,
            voyage: None,
            leg: None,
            phase: SupervisorPhase::Starting,
        }),
        rep(SupervisorReply::StatusOk {
            pid: 4242,
            created: 1_756_000_000,
            voyage: Some("01900000-0000-7000-8000-000000000001".into()),
            leg: None,
            phase: SupervisorPhase::Ready,
        }),
        rep(SupervisorReply::StatusOk {
            pid: 4242,
            created: 1_756_000_000,
            voyage: Some("01900000-0000-7000-8000-000000000001".into()),
            leg: Some(7),
            phase: SupervisorPhase::Ending,
        }),
        rep(SupervisorReply::StatusOk {
            pid: 4242,
            created: 1_756_000_000,
            voyage: Some("01900000-0000-7000-8000-000000000001".into()),
            leg: Some(7),
            phase: SupervisorPhase::EndedNoRespawn,
        }),
        rep(SupervisorReply::StatusOk {
            pid: 4242,
            created: 1_756_000_000,
            voyage: Some("01900000-0000-7000-8000-000000000001".into()),
            leg: Some(7),
            phase: SupervisorPhase::Terminal,
        }),
    ]
}

/// Pins the embedded `supervisor-lane-v1-coverage.bin` against a fresh
/// encoding of [`supervisor_lane_v1_coverage_frames`], THEN decodes the
/// committed bytes back through a fresh [`FrameSplitter`] and compares
/// the result frame-for-frame against what each entry expects -- see
/// this section's own header doc for why both checks matter.
#[test]
fn supervisor_lane_v1_coverage_bytes_are_pinned_and_decode_back_identically() {
    let pairs = supervisor_lane_v1_coverage_frames();
    let generated: Vec<u8> = pairs.iter().flat_map(|(bytes, _)| bytes.clone()).collect();
    const COMMITTED: &[u8] = include_bytes!("../fixtures/supervisor-lane-v1-coverage.bin");
    assert_eq!(
        COMMITTED, generated.as_slice(),
        "supervisor lane v1 coverage wire bytes changed -- this is a NEW PROTOCOL VERSION \
         (ADR 0045 decision 7), not test drift: add a -v2 fixture instead of editing this one"
    );

    let mut splitter = FrameSplitter::new();
    let (frames, err) = splitter.feed(COMMITTED);
    assert!(err.is_none(), "the committed coverage fixture must decode cleanly: {err:?}");
    let expected: Vec<DecodedFrame> = pairs.into_iter().map(|(_, frame)| frame).collect();
    assert_eq!(frames, expected, "the committed coverage fixture must decode back to exactly these frames");
}

// -----------------------------------------------------------------------
// attach-lane-v3 fixture (ADR 0046 decision 3, lane B3b1): the FIRST
// attach-lane fixture in this crate — v1/v2 have `src/wire.rs`'s own
// inline byte goldens but no committed file, since the wire-level shape
// hasn't needed a cross-process conformance pin until now. An immutable
// committed file, one instance of `hello{v3}`/`hello_ok{v3}` (proving
// the new version negotiates) plus every new `AttachServer` shape and
// both `holder` forms (`Some`/`None`) `wire.rs`'s own colocated golden
// tests already cover individually — this file's job is pinning them
// TOGETHER, concatenated, the same cross-language conformance role
// `tests/golden.rs`'s journal fixtures play. A wire change here is a NEW
// PROTOCOL VERSION (a `-v4` fixture), never an edit to this one; v1/v2
// stay wholly unmoved by this lane.
// -----------------------------------------------------------------------

/// Fixed field values, stable declaration order -- see this section's
/// own header doc for what it covers.
fn attach_lane_v3_frames() -> Vec<Vec<u8>> {
    vec![
        encode_attach_client(&AttachClient::Hello { proto: ATTACH_PROTO_V3 }).unwrap(),
        encode_attach_server(&AttachServer::HelloOk { proto: ATTACH_PROTO_V3 }).unwrap(),
        encode_attach_server(&AttachServer::PenSnapshot {
            holder: Some("alice".to_string()),
            take_epoch: 1,
        })
        .unwrap(),
        encode_attach_server(&AttachServer::PenSnapshot { holder: None, take_epoch: 0 }).unwrap(),
        encode_attach_server(&AttachServer::PenChanged {
            holder: Some("bob".to_string()),
            take_epoch: 2,
        })
        .unwrap(),
        encode_attach_server(&AttachServer::PenChanged { holder: None, take_epoch: 2 }).unwrap(),
        encode_attach_server(&AttachServer::Geometry { cols: 120, rows: 40 }).unwrap(),
    ]
}

/// Pins the embedded `attach-lane-v3.bin` against a fresh encoding of
/// [`attach_lane_v3_frames`], THEN decodes the committed bytes back
/// through a fresh [`FrameSplitter`] and compares frame-for-frame — the
/// same two-sided proof `supervisor_lane_v1_coverage_bytes_are_pinned_
/// and_decode_back_identically` uses, so a decoder bug that happens to
/// still produce matching encoder output cannot hide behind the byte
/// pin alone. Embedded via `include_bytes!`, not read at runtime, per
/// that fixture's own precedent (ADR 0045 decision 7): a committed
/// fixture is immutable by construction, so this file carries no writer
/// for it — a wire change is a NEW protocol version (a `-v4` fixture),
/// never a rewrite of this one (Codex review round: an earlier version
/// of this test could overwrite the fixture it was meant to pin under
/// an env var, including an accidentally-set empty one).
#[test]
fn attach_lane_v3_bytes_are_pinned_and_decode_back_identically() {
    let generated: Vec<u8> = attach_lane_v3_frames().into_iter().flatten().collect();
    const COMMITTED: &[u8] = include_bytes!("../fixtures/attach-lane-v3.bin");
    assert_eq!(
        COMMITTED,
        generated.as_slice(),
        "attach lane v3 wire bytes changed -- this is a NEW PROTOCOL VERSION, not test drift: \
         add a -v4 fixture instead of editing this one"
    );

    let mut splitter = FrameSplitter::new();
    let (frames, err) = splitter.feed(COMMITTED);
    assert!(err.is_none(), "the committed fixture must decode cleanly: {err:?}");
    let expected: Vec<DecodedFrame> = vec![
        DecodedFrame::AttachClient(AttachClient::Hello { proto: ATTACH_PROTO_V3 }),
        DecodedFrame::AttachServer(AttachServer::HelloOk { proto: ATTACH_PROTO_V3 }),
        DecodedFrame::AttachServer(AttachServer::PenSnapshot {
            holder: Some("alice".to_string()),
            take_epoch: 1,
        }),
        DecodedFrame::AttachServer(AttachServer::PenSnapshot { holder: None, take_epoch: 0 }),
        DecodedFrame::AttachServer(AttachServer::PenChanged {
            holder: Some("bob".to_string()),
            take_epoch: 2,
        }),
        DecodedFrame::AttachServer(AttachServer::PenChanged { holder: None, take_epoch: 2 }),
        DecodedFrame::AttachServer(AttachServer::Geometry { cols: 120, rows: 40 }),
    ];
    assert_eq!(frames, expected, "the committed fixture must decode back to exactly these frames");
}
