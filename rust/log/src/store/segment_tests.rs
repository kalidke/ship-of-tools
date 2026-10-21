use super::*;
use crate::store::envelope::*;
use serde_json::json;

pub(crate) fn test_env(epoch: u64, n: u64) -> Envelope {
    Envelope {
        seq: Seq { epoch, n },
        class: Class::Producer,
        source: Source {
            emitter: Emitter::Producer,
            actor: Actor {
                kind: ActorKind::Producer,
                controller_id: None,
                take_epoch: None,
            },
            derivation: Derivation::Native,
        },
        t_wall_ms: 1_756_000_000_000,
        t_mono_us: n * 1000,
        stream: None,
        transformed: None,
        refs: vec![],
        payload: Some(json!({"line": n})),
        payload_ref: None,
    }
}

pub(crate) fn test_header(voyage: &str, index: u64, epoch: u64, prev: Option<Digest>) -> HeaderBody {
    HeaderBody {
        version: 1,
        required_features: vec![],
        voyage_id: voyage.into(),
        segment_index: index,
        epoch,
        prev_seal_digest: prev,
        created_wall_ms: 1_756_000_000_000,
        retention_class: (index == 0).then_some(RetentionClass::Archive),
    }
}

#[test]
fn buffered_append_owes_a_commit_until_a_sync_covers_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = SegmentWriter::create(dir.path(), test_header("voy", 0, 1, None)).unwrap();
    assert!(!w.has_unsynced());
    let synced = w.synced_len();
    w.append(&test_env(1, 1), Commit::Buffered).unwrap();
    assert!(w.has_unsynced());
    assert_eq!(w.synced_len(), synced);
    w.commit().unwrap();
    assert!(!w.has_unsynced());
    w.append(&test_env(1, 2), Commit::Buffered).unwrap();
    assert!(w.has_unsynced());
    w.append(&test_env(1, 3), Commit::Immediate).unwrap();
    assert!(!w.has_unsynced());
}

#[test]
fn filename_roundtrip() {
    let id = SegmentIdentity {
        voyage_id: "v".into(),
        segment_index: 7,
        epoch: 300,
    };
    let name = format!("{}.open", id.file_stem());
    assert_eq!(
        SegmentIdentity::parse_file_name(&name),
        Some((7, 300, SegmentState::Open))
    );
    assert_eq!(SegmentIdentity::parse_file_name("0000000A-0000000000012c.open"), None); // uppercase
    assert_eq!(SegmentIdentity::parse_file_name("x.sotseg"), None);
}

#[test]
fn write_seal_read_verify() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = SegmentWriter::create(dir.path(), test_header("voy", 0, 1, None)).unwrap();
    for n in 1..=3 {
        w.append(&test_env(1, n), Commit::Immediate).unwrap();
    }
    let digest = w.seal(None).unwrap();
    let path = dir.path().join("00000000-00000000000001.sotseg");
    let r = SegmentReader::read(&path, true).unwrap();
    assert_eq!(r.frames.len(), 3);
    r.verify_seal().unwrap();
    assert_eq!(r.seal.as_ref().unwrap().digest.value, digest.value);
    assert_eq!(r.seal.as_ref().unwrap().first_seq, Some(Seq { epoch: 1, n: 1 }));
}

#[test]
fn empty_segment_seals_with_nulls() {
    let dir = tempfile::tempdir().unwrap();
    let w = SegmentWriter::create(dir.path(), test_header("voy", 0, 1, None)).unwrap();
    w.seal(None).unwrap();
    let r = SegmentReader::read(&dir.path().join("00000000-00000000000001.sotseg"), true).unwrap();
    let seal = r.seal.as_ref().unwrap();
    assert_eq!(seal.frame_count, 0);
    assert!(seal.first_seq.is_none() && seal.last_seq.is_none());
    r.verify_seal().unwrap();
}

#[test]
fn non_contiguous_n_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = SegmentWriter::create(dir.path(), test_header("voy", 0, 1, None)).unwrap();
    w.append(&test_env(1, 1), Commit::Immediate).unwrap();
    assert!(w.append(&test_env(1, 3), Commit::Immediate).is_err());
    assert!(w.append(&test_env(2, 2), Commit::Immediate).is_err());
}

#[test]
fn genesis_rules_enforced() {
    let dir = tempfile::tempdir().unwrap();
    // Non-genesis without prev digest: refused.
    let mut h = test_header("voy", 1, 1, None);
    h.retention_class = None;
    assert!(SegmentWriter::create(dir.path(), h).is_err());
    // Genesis with a prev digest: refused.
    let mut h2 = test_header("voy", 0, 1, Some(Digest { algo: "sha256".into(), value: "0".repeat(64) }));
    h2.retention_class = Some(RetentionClass::Discard);
    assert!(SegmentWriter::create(dir.path(), h2).is_err());
}

#[test]
fn tampered_frame_byte_breaks_seal_digest() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = SegmentWriter::create(dir.path(), test_header("voy", 0, 1, None)).unwrap();
    w.append(&test_env(1, 1), Commit::Immediate).unwrap();
    w.seal(None).unwrap();
    let path = dir.path().join("00000000-00000000000001.sotseg");
    let mut bytes = std::fs::read(&path).unwrap();
    // Flip a byte INSIDE the first frame's body and fix that record's
    // CRCs so only the seal-digest chain can catch it.
    let hdr = record::decode_at(&bytes, 0).unwrap().unwrap();
    let f_off = hdr.wire_len;
    let frame = record::decode_at(&bytes, f_off as u64).unwrap().unwrap();
    let body_start = f_off + record::PRELUDE_LEN;
    bytes[body_start] ^= 0x01;
    let new_crc = crc32c::crc32c(&bytes[body_start..body_start + frame.body.len()]);
    bytes[f_off + 14..f_off + 18].copy_from_slice(&new_crc.to_le_bytes());
    std::fs::write(&path, &bytes).unwrap();
    let r = SegmentReader::read(&path, true);
    // Either the frame no longer parses (schema) or the seal digest
    // catches it — silent acceptance is the only failure.
    match r {
        Err(_) => {}
        Ok(r) => assert!(r.verify_seal().is_err()),
    }
}

#[test]
fn post_seal_bytes_are_loud() {
    let dir = tempfile::tempdir().unwrap();
    let w = SegmentWriter::create(dir.path(), test_header("voy", 0, 1, None)).unwrap();
    w.seal(None).unwrap();
    let path = dir.path().join("00000000-00000000000001.sotseg");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(b"garbage");
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        SegmentReader::read(&path, true),
        Err(Error::Corrupt { .. })
    ));
}
