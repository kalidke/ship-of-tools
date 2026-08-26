//! The record wrapper (ADR 0039 §Record wrapper).
//!
//! 18-byte prelude: magic(2) version(1) kind(1) codec(1) reserved(1)
//! len(4 LE) prelude_crc32c(4 LE) body_crc32c(4 LE), then `len` body bytes.
//! The prelude CRC validates `len` independently of the body — that is what
//! makes a torn tail *provable* rather than assumed.

use crate::{Error, Result};

pub const MAGIC: [u8; 2] = [0xA9, 0x5F];
pub const WRAPPER_VERSION: u8 = 1;
pub const CODEC_JSON: u8 = 1;
pub const PRELUDE_LEN: usize = 18;
pub const RECORD_MAX_BODY: u32 = 16 * 1024 * 1024;

/// Byte range the prelude CRC covers: [version..len] = bytes 2..10.
const PRELUDE_CRC_RANGE: std::ops::Range<usize> = 2..10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    Header,
    Frame,
    Seal,
}

impl RecordKind {
    pub fn to_byte(self) -> u8 {
        match self {
            RecordKind::Header => 1,
            RecordKind::Frame => 2,
            RecordKind::Seal => 3,
        }
    }
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(RecordKind::Header),
            2 => Some(RecordKind::Frame),
            3 => Some(RecordKind::Seal),
            _ => None,
        }
    }
}

/// Encode one record. `body_crc_override` supports the seal-digest preimage
/// (ADR 0039 encoding atoms: the preimage substitutes 0x00000000 for the seal
/// record's body CRC, length-preserving).
pub fn encode(kind: RecordKind, body: &[u8], body_crc_override: Option<u32>) -> Result<Vec<u8>> {
    if body.len() as u64 > RECORD_MAX_BODY as u64 {
        return Err(Error::Schema(format!(
            "record body {} bytes exceeds cap {}",
            body.len(),
            RECORD_MAX_BODY
        )));
    }
    let mut out = Vec::with_capacity(PRELUDE_LEN + body.len());
    out.extend_from_slice(&MAGIC);
    out.push(WRAPPER_VERSION);
    out.push(kind.to_byte());
    out.push(CODEC_JSON);
    out.push(0); // reserved
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    let prelude_crc = crc32c::crc32c(&out[PRELUDE_CRC_RANGE]);
    out.extend_from_slice(&prelude_crc.to_le_bytes());
    let body_crc = body_crc_override.unwrap_or_else(|| crc32c::crc32c(body));
    out.extend_from_slice(&body_crc.to_le_bytes());
    out.extend_from_slice(body);
    Ok(out)
}

/// One decoded record plus where it sat.
#[derive(Debug)]
pub struct Record {
    pub kind: RecordKind,
    pub body: Vec<u8>,
    pub offset: u64,
    /// Total wire length (prelude + body).
    pub wire_len: usize,
}

/// Tail classification for the FINAL bytes of an unsealed file (ADR 0039
/// tail rule). `Loud` carries the reason; callers must never truncate it.
#[derive(Debug, PartialEq, Eq)]
pub enum TailClass {
    /// Case (a): fewer than 18 prelude bytes remain.
    TruncatedPrelude,
    /// Case (b): valid prelude, fewer than `len` body bytes remain.
    TornBody,
    /// Case (c): anything else — complete prelude failing any check, or a
    /// complete body with a bad body CRC.
    Loud(String),
}

/// Decode the record starting at `offset` in `buf`.
///
/// - `Ok(Some(record))` — a complete, CRC-valid record.
/// - `Ok(None)` — `offset == buf.len()` (clean end).
/// - `Err(Error::TornTail)` — provably incomplete FINAL record (only when
///   the remaining bytes end the buffer).
/// - `Err(Error::Corrupt)` — anything else (loud).
pub fn decode_at(buf: &[u8], offset: u64) -> Result<Option<Record>> {
    let off = offset as usize;
    if off == buf.len() {
        return Ok(None);
    }
    let rest = &buf[off..];
    if rest.len() < PRELUDE_LEN {
        return Err(Error::TornTail {
            offset,
            what: "truncated prelude",
        });
    }
    let prelude = &rest[..PRELUDE_LEN];
    let stored_prelude_crc = u32::from_le_bytes(prelude[10..14].try_into().unwrap());
    let computed_prelude_crc = crc32c::crc32c(&prelude[PRELUDE_CRC_RANGE]);

    // A prelude integrity failure is NOT a tear: the 18 bytes are all
    // present, so this is bit rot or garbage — loud (r4-6/r3-3).
    let loud = |what: String| Error::Corrupt { offset, what };
    if prelude[0..2] != MAGIC {
        return Err(loud("bad magic".into()));
    }
    if prelude[2] != WRAPPER_VERSION {
        return Err(loud(format!("unknown wrapper version {}", prelude[2])));
    }
    let kind = RecordKind::from_byte(prelude[3])
        .ok_or_else(|| loud(format!("unknown record kind {}", prelude[3])))?;
    if prelude[4] != CODEC_JSON {
        return Err(loud(format!("unknown codec id {}", prelude[4])));
    }
    if prelude[5] != 0 {
        return Err(loud(format!("nonzero reserved byte {}", prelude[5])));
    }
    if stored_prelude_crc != computed_prelude_crc {
        return Err(loud("prelude crc mismatch".into()));
    }
    let len = u32::from_le_bytes(prelude[6..10].try_into().unwrap());
    if len > RECORD_MAX_BODY {
        return Err(loud(format!("len {} exceeds cap", len)));
    }
    let body_start = PRELUDE_LEN;
    let body_end = body_start + len as usize;
    if rest.len() < body_end {
        // Valid prelude, short body, at EOF: the one true tear.
        return Err(Error::TornTail {
            offset,
            what: "torn body",
        });
    }
    let body = &rest[body_start..body_end];
    let stored_body_crc = u32::from_le_bytes(prelude[14..18].try_into().unwrap());
    if crc32c::crc32c(body) != stored_body_crc {
        return Err(loud("body crc mismatch".into()));
    }
    Ok(Some(Record {
        kind,
        body: body.to_vec(),
        offset,
        wire_len: body_end,
    }))
}

/// Classify the tail defect at `offset` (which `decode_at` reported). Only
/// meaningful for the final record of an unsealed file.
pub fn classify_tail(err: &Error) -> Option<TailClass> {
    match err {
        Error::TornTail { what, .. } if *what == "truncated prelude" => {
            Some(TailClass::TruncatedPrelude)
        }
        Error::TornTail { .. } => Some(TailClass::TornBody),
        Error::Corrupt { what, .. } => Some(TailClass::Loud(what.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
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
}
