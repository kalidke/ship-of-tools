use super::*;

fn status_ok_bytes(pid: u32, created: u64) -> Vec<u8> {
    wire::encode_mgmt_reply(&MgmtReply::StatusOk { pid, created, survival: wire::Survival::Normal }).unwrap()
}

#[test]
fn incomplete_on_a_partial_frame() {
    let mut ex = VoyageMgmtExchange::default();
    let full = status_ok_bytes(1, 2);
    assert!(matches!(ex.feed(&full[..full.len() - 1]), ExchangeDecode::Incomplete));
}

#[test]
fn decodes_exactly_one_clean_status_ok() {
    let mut ex = VoyageMgmtExchange::default();
    let full = status_ok_bytes(123, 456);
    match ex.feed(&full) {
        ExchangeDecode::Identity { pid, created } => {
            assert_eq!(pid, 123);
            assert_eq!(created, 456);
        }
        ExchangeDecode::Foreign => panic!("expected Identity, got Foreign"),
        ExchangeDecode::Incomplete => panic!("expected Identity, got Incomplete"),
    }
}

#[test]
fn two_complete_replies_in_one_feed_is_foreign() {
    let mut ex = VoyageMgmtExchange::default();
    let mut bytes = status_ok_bytes(1, 2);
    bytes.extend(status_ok_bytes(1, 2));
    assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
}

#[test]
fn status_ok_followed_by_malformed_bytes_is_foreign_not_proven() {
    // A frame-plus-error in the SAME feed call: the good StatusOk
    // must not mask the trailing corruption (finding 5).
    let mut ex = VoyageMgmtExchange::default();
    let mut bytes = status_ok_bytes(1, 2);
    bytes.extend([0xffu8; 16]); // an unknown magic -- UnknownMagic
    assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
}

#[test]
fn malformed_first_frame_alone_is_foreign() {
    let mut ex = VoyageMgmtExchange::default();
    assert!(matches!(ex.feed(&[0xff; 16]), ExchangeDecode::Foreign));
}

#[test]
fn oversized_first_frame_is_foreign() {
    let mut ex = VoyageMgmtExchange::default();
    // SOM0 magic + a length field announcing far more than MAX_BODY_LEN.
    let mut bytes = b"SOM0".to_vec();
    bytes.extend(u32::MAX.to_le_bytes());
    assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
}

/// Round-2 finding 1: the round-1 version of this test fed the "bad
/// second" bytes to a FRESH exchange, which proves nothing about
/// whether THIS SAME instance would still accept them after already
/// answering once -- Codex reproduced exactly that gap (a second
/// complete `status_ok` fed to the SAME instance in a later `feed`
/// call returned `Identity` again). Rewritten to drive one instance
/// throughout: a good reply, then a second complete reply arriving
/// in a LATER `feed` call, must be `Foreign`.
#[test]
fn good_first_reply_then_a_later_complete_reply_on_the_same_instance_is_foreign() {
    let mut ex = VoyageMgmtExchange::default();
    assert!(matches!(ex.feed(&status_ok_bytes(1, 2)), ExchangeDecode::Identity { pid: 1, created: 2 }));
    // A second, otherwise-perfectly-well-formed reply, fed to the
    // SAME already-answered instance in a separate call.
    assert!(matches!(ex.feed(&status_ok_bytes(3, 4)), ExchangeDecode::Foreign));
}

/// Round-2 finding 1 (the other reproduced leak): a single trailing
/// byte riding along with a valid reply in the SAME `feed` call —
/// not even enough to determine a magic, so the OLD code's
/// `frames.len() == 1` check alone could not distinguish this from
/// "nothing left over" and returned `Identity`.
#[test]
fn status_ok_plus_one_trailing_partial_header_byte_is_foreign() {
    let mut ex = VoyageMgmtExchange::default();
    let mut bytes = status_ok_bytes(1, 2);
    bytes.push(0xAB); // a single byte: not even a full header's worth
    assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
}

/// Round-2 finding 1: once terminal, ANY further byte in a LATER
/// call is Foreign, even bytes that on their own would be
/// `Incomplete` for a fresh exchange (e.g. a lone partial byte).
#[test]
fn any_byte_at_all_after_a_conclusive_answer_is_foreign() {
    let mut ex = VoyageMgmtExchange::default();
    assert!(matches!(ex.feed(&status_ok_bytes(1, 2)), ExchangeDecode::Identity { .. }));
    assert!(matches!(ex.feed(&[0xAB]), ExchangeDecode::Foreign));
}

#[test]
fn encode_request_is_the_pinned_status_wire_bytes() {
    let ex = VoyageMgmtExchange::default();
    assert_eq!(ex.encode_request(), wire::encode_mgmt_request(&MgmtRequest::Status).unwrap());
}

// --- SupervisorLaneExchange ---

// Matches what `SupervisorLaneExchange::new("b")` sends, so the
// "clean" tests below exercise the real mutual check (matching
// proto+build) rather than accidentally skipping it.
fn hello_ok_bytes(pid: u32, created: u64) -> Vec<u8> {
    hello_ok_bytes_with(wire::SUPERVISOR_PROTO_V1, "b", pid, created)
}

fn hello_ok_bytes_with(proto: u32, build: &str, pid: u32, created: u64) -> Vec<u8> {
    wire::encode_supervisor_reply(&SupervisorReply::HelloOk {
        proto,
        build: build.into(),
        pid,
        created,
    })
    .unwrap()
}

#[test]
fn supervisor_encode_request_is_hello_with_this_build_id() {
    let ex = SupervisorLaneExchange::new("my-build");
    assert_eq!(
        ex.encode_request(),
        wire::encode_supervisor_request(&SupervisorRequest::Hello {
            proto: wire::SUPERVISOR_PROTO_V1,
            build: "my-build".into(),
        })
        .unwrap()
    );
}

#[test]
fn supervisor_oversized_build_is_truncated_on_a_char_boundary_not_rejected() {
    // Multi-byte UTF-8 right at the truncation point: naive byte
    // slicing would split a codepoint and panic on `String::truncate`.
    let build: String = "é".repeat(wire::MAX_SUPERVISOR_STRING_LEN); // 2 bytes each
    let ex = SupervisorLaneExchange::new(build);
    let encoded = ex.encode_request(); // must not panic
    assert!(!encoded.is_empty());
}

/// Codex review round 2, finding M4: the ABOVE test's own `é` (2 bytes
/// each) happens to land byte 128 exactly on a boundary (128 is even),
/// which is precisely how the real bug — truncating to 128 BEFORE
/// finding a boundary, which panics if byte 128 lands mid-codepoint —
/// went unnoticed. A 3-byte codepoint repeated enough times to exceed
/// 128 bytes puts byte 128 strictly INSIDE a character (126..129), so
/// this exercises the actual straddle.
#[test]
fn supervisor_oversized_build_with_a_char_straddling_byte_128_does_not_panic() {
    let build: String = "€".repeat(50); // 3 bytes each = 150 bytes; byte 128 splits char 42
    assert!(!build.is_char_boundary(wire::MAX_SUPERVISOR_STRING_LEN), "test setup must actually straddle byte 128");
    let ex = SupervisorLaneExchange::new(build);
    let encoded = ex.encode_request(); // must not panic
    assert!(!encoded.is_empty());
}

#[test]
fn supervisor_decodes_exactly_one_clean_hello_ok() {
    let mut ex = SupervisorLaneExchange::new("b");
    match ex.feed(&hello_ok_bytes(123, 456)) {
        ExchangeDecode::Identity { pid, created } => {
            assert_eq!(pid, 123);
            assert_eq!(created, 456);
        }
        ExchangeDecode::Foreign => panic!("expected Identity, got Foreign"),
        ExchangeDecode::Incomplete => panic!("expected Identity, got Incomplete"),
    }
}

#[test]
fn supervisor_hello_refused_is_foreign_and_specifically_version_skew() {
    // ADR 0030 §8 decision 31c: `Refused { VersionSkew }` is the ONE
    // `Foreign` cause `is_version_skew()` must flag — `phase_of`
    // reports "foreign" only on this, never on a merely wrong/
    // malformed reply (see the sibling test below).
    let mut ex = SupervisorLaneExchange::new("b");
    let bytes = wire::encode_supervisor_reply(&SupervisorReply::Refused {
        reason: wire::SupervisorRefusedReason::VersionSkew,
    })
    .unwrap();
    assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
    assert!(ex.is_version_skew());
}

#[test]
fn supervisor_wrong_reply_kind_is_foreign_but_not_version_skew() {
    // A well-formed reply of the WRONG kind (here, a premature
    // `StatusOk` — the mgmt lane's own shape, never legal on the
    // supervisor lane's hello step) is exactly the case ADR 0030 §8
    // decision 31c's typed check must NOT classify as "another
    // build": `phase_of` must report "unreachable" for this, not
    // "foreign".
    let mut ex = SupervisorLaneExchange::new("b");
    let bytes = status_ok_bytes(1, 2);
    assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
    assert!(!ex.is_version_skew());
}

#[test]
fn supervisor_corrupt_bytes_are_foreign_but_not_version_skew() {
    // Undecodable bytes (a malformed hello, in the review's own
    // words) must never look like version skew either.
    let mut ex = SupervisorLaneExchange::new("b");
    assert!(matches!(ex.feed(b"not a valid frame at all"), ExchangeDecode::Foreign));
    assert!(!ex.is_version_skew());
}

#[test]
fn supervisor_two_complete_replies_in_one_feed_is_foreign() {
    let mut ex = SupervisorLaneExchange::new("b");
    let mut bytes = hello_ok_bytes(1, 2);
    bytes.extend(hello_ok_bytes(1, 2));
    assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
}

#[test]
fn supervisor_hello_ok_plus_trailing_byte_is_foreign() {
    let mut ex = SupervisorLaneExchange::new("b");
    let mut bytes = hello_ok_bytes(1, 2);
    bytes.push(0xAB);
    assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
}

#[test]
fn supervisor_any_byte_after_a_conclusive_answer_is_foreign() {
    let mut ex = SupervisorLaneExchange::new("b");
    assert!(matches!(ex.feed(&hello_ok_bytes(1, 2)), ExchangeDecode::Identity { .. }));
    assert!(matches!(ex.feed(&[0xAB]), ExchangeDecode::Foreign));
}

#[test]
fn supervisor_incomplete_on_a_partial_frame() {
    let mut ex = SupervisorLaneExchange::new("b");
    let full = hello_ok_bytes(1, 2);
    assert!(matches!(ex.feed(&full[..full.len() - 1]), ExchangeDecode::Incomplete));
}

// The boundary is mutual (Codex review round 1, finding 7): a
// `hello_ok` that doesn't echo THIS client's own proto/build back is
// not proof of anything, even though it decodes cleanly — it could be
// a stale reply to someone else's `hello`, or a server on a different
// build replying without ever checking what it received.

#[test]
fn supervisor_hello_ok_with_a_different_build_id_is_identity_not_foreign() {
    // ADR 0045 decision 7: the gate is the proto integer alone; a
    // different echoed build is fine -- build rides the wire as
    // information only and is never compared.
    let mut ex = SupervisorLaneExchange::new("b");
    let bytes = hello_ok_bytes_with(wire::SUPERVISOR_PROTO_V1, "different-build", 1, 2);
    assert!(matches!(ex.feed(&bytes), ExchangeDecode::Identity { pid: 1, created: 2 }));
}

#[test]
fn supervisor_hello_ok_with_wrong_proto_is_foreign_not_identity() {
    let mut ex = SupervisorLaneExchange::new("b");
    let bytes = hello_ok_bytes_with(wire::SUPERVISOR_PROTO_V1 + 1, "b", 1, 2);
    assert!(matches!(ex.feed(&bytes), ExchangeDecode::Foreign));
}

#[test]
fn supervisor_hello_ok_matching_this_build_id_constant_is_identity() {
    // Not just a fixture string: a `hello_ok` echoing the crate's
    // OWN real build id (what a genuine same-build peer would send)
    // must still be accepted.
    let mut ex = SupervisorLaneExchange::new(SUPERVISOR_LANE_BUILD_ID);
    let bytes = hello_ok_bytes_with(wire::SUPERVISOR_PROTO_V1, SUPERVISOR_LANE_BUILD_ID, 9, 10);
    assert!(matches!(ex.feed(&bytes), ExchangeDecode::Identity { pid: 9, created: 10 }));
}
