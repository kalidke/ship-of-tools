use super::*;

#[test]
fn roundtrip() {
    let body = br#"{"k":1}"#;
    let wire = encode(RecordKind::Frame, body, None).unwrap();
    assert_eq!(wire.len(), PRELUDE_LEN + body.len());
    let rec = decode_at(&wire, 0).unwrap().unwrap();
    assert_eq!(rec.kind, RecordKind::Frame);
    assert_eq!(rec.body, body);
    assert_eq!(rec.wire_len, wire.len());
    assert!(decode_at(&wire, wire.len() as u64).unwrap().is_none());
}

#[test]
fn prelude_is_18_bytes() {
    // r4-6: 2+1+1+1+1+4+4+4.
    assert_eq!(PRELUDE_LEN, 18);
}

#[test]
fn truncated_prelude_is_a_tear() {
    let wire = encode(RecordKind::Frame, b"{}", None).unwrap();
    for cut in 1..PRELUDE_LEN {
        let err = decode_at(&wire[..cut], 0).unwrap_err();
        assert_eq!(classify_tail(&err), Some(TailClass::TruncatedPrelude));
    }
}

#[test]
fn short_body_is_a_tear() {
    let wire = encode(RecordKind::Frame, b"{\"x\":123}", None).unwrap();
    for cut in PRELUDE_LEN..wire.len() {
        let err = decode_at(&wire[..cut], 0).unwrap_err();
        assert_eq!(classify_tail(&err), Some(TailClass::TornBody));
    }
}

#[test]
fn corrupted_len_with_full_prelude_is_loud_not_a_tear() {
    // r4/r2-18: a complete prelude whose len was corrupted upward must
    // NOT classify as a tear — the prelude CRC catches it.
    let mut wire = encode(RecordKind::Frame, b"{}", None).unwrap();
    wire[6] = 0xFF; // len low byte
    let err = decode_at(&wire, 0).unwrap_err();
    assert!(matches!(classify_tail(&err), Some(TailClass::Loud(_))));
}

#[test]
fn full_body_bad_crc_is_loud() {
    let mut wire = encode(RecordKind::Frame, b"{\"x\":1}", None).unwrap();
    let last = wire.len() - 1;
    wire[last] ^= 0xFF;
    let err = decode_at(&wire, 0).unwrap_err();
    assert!(matches!(classify_tail(&err), Some(TailClass::Loud(_))));
}

#[test]
fn unknown_fields_fail_closed() {
    for (byte, val) in [(2usize, 9u8), (3, 9), (4, 9), (5, 9)] {
        let mut wire = encode(RecordKind::Frame, b"{}", None).unwrap();
        wire[byte] = val;
        // Re-fix prelude CRC so ONLY the field is wrong — fail-closed
        // must trigger on the field itself, not the CRC.
        let crc = crc32c::crc32c(&wire[PRELUDE_CRC_RANGE]);
        wire[10..14].copy_from_slice(&crc.to_le_bytes());
        let err = decode_at(&wire, 0).unwrap_err();
        assert!(
            matches!(classify_tail(&err), Some(TailClass::Loud(_))),
            "byte {byte} val {val} must be loud"
        );
    }
}
