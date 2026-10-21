use super::*;

#[test]
fn voyage_root_path_is_scoped_under_a_voyages_subdir() {
    let dir = tempfile::tempdir().unwrap();
    let root = voyage_root_path(dir.path(), "abc");
    assert_eq!(root, dir.path().join("voyages").join("abc"));
}
#[test]
fn digest_of_is_stable_and_distinguishes_ops() {
    let a = SupervisorOp::Stop;
    let b = SupervisorOp::Stop;
    let c = SupervisorOp::EndRun {
        reason: "r".into(),
        voyage: "v".into(),
    };
    assert_eq!(digest_of(&a).unwrap(), digest_of(&b).unwrap());
    assert_ne!(digest_of(&a).unwrap(), digest_of(&c).unwrap());
}

#[test]
fn bounded_detail_truncates_on_a_char_boundary() {
    let long: String = "é".repeat(wire::MAX_SUPERVISOR_STRING_LEN); // 2 bytes each
    let truncated = bounded_detail(long);
    assert!(truncated.len() <= wire::MAX_SUPERVISOR_STRING_LEN);
    assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
}

/// `String::truncate` panics if
/// the cut point splits a codepoint. A 3-byte codepoint repeated
/// enough times to exceed 128 bytes puts byte 128 strictly INSIDE a
/// character, unlike the `é` (2 bytes) case above where byte 128
/// happens to land on a boundary regardless.
#[test]
fn bounded_detail_does_not_panic_when_a_char_straddles_byte_128() {
    let long: String = "€".repeat(50); // 3 bytes each = 150 bytes
    assert!(
        !long.is_char_boundary(wire::MAX_SUPERVISOR_STRING_LEN),
        "test setup must actually straddle byte 128"
    );
    let truncated = bounded_detail(long);
    assert!(truncated.len() <= wire::MAX_SUPERVISOR_STRING_LEN);
    assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
}
