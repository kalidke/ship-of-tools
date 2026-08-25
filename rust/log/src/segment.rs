//! Segment files: `<index:hex8>-<epoch:hex14>.{open|recovering|recovering-out|sotseg}`
//! Record order: `header frame* seal?`. Sealing is a filename fact committed
//! by RENAME_NOREPLACE; the seal digest chains segments (ADR 0039).

use crate::envelope::{Digest, Envelope, Seq, U53_MAX};
use crate::fsutil;
use crate::record::{self, RecordKind};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const SEAL_DOMAIN: &[u8] = b"sotseg1.seal\x00";
/// Body-CRC prelude offset within a record's wire bytes.
const BODY_CRC_RANGE: std::ops::Range<usize> = 14..18;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionClass {
    Archive,
    Discard,
    Distill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SegmentState {
    Open,
    Recovering,
    RecoveringOut,
    Sealed,
}

impl SegmentState {
    pub fn ext(self) -> &'static str {
        match self {
            SegmentState::Open => "open",
            SegmentState::Recovering => "recovering",
            SegmentState::RecoveringOut => "recovering-out",
            SegmentState::Sealed => "sotseg",
        }
    }
    pub fn from_ext(ext: &str) -> Option<Self> {
        match ext {
            "open" => Some(SegmentState::Open),
            "recovering" => Some(SegmentState::Recovering),
            "recovering-out" => Some(SegmentState::RecoveringOut),
            "sotseg" => Some(SegmentState::Sealed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentIdentity {
    pub voyage_id: String,
    pub segment_index: u64,
    pub epoch: u64,
}

impl SegmentIdentity {
    pub fn file_stem(&self) -> String {
        format!("{:08x}-{:014x}", self.segment_index, self.epoch)
    }
    pub fn path(&self, seg_dir: &Path, state: SegmentState) -> PathBuf {
        seg_dir.join(format!("{}.{}", self.file_stem(), state.ext()))
    }
    /// Parse `<hex8>-<hex14>.<state>`; filename and header must later agree.
    pub fn parse_file_name(name: &str) -> Option<(u64, u64, SegmentState)> {
        let (stem, ext) = name.split_once('.')?;
        let state = SegmentState::from_ext(ext)?;
        let (idx, ep) = stem.split_once('-')?;
        if idx.len() != 8 || ep.len() != 14 {
            return None;
        }
        let lower_hex = |s: &str| s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !lower_hex(idx) || !lower_hex(ep) {
            return None;
        }
        Some((
            u64::from_str_radix(idx, 16).ok()?,
            u64::from_str_radix(ep, 16).ok()?,
            state,
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderBody {
    pub version: u32,
    pub required_features: Vec<String>,
    pub voyage_id: String,
    pub segment_index: u64,
    pub epoch: u64,
    pub prev_seal_digest: Option<Digest>,
    pub created_wall_ms: i64,
    /// Genesis-only (segment_index 0): immutable voyage policy, stated once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_class: Option<RetentionClass>,
}

/// Seal body. Field order matters: `digest` is LAST, which is what makes the
/// length-preserving preimage substitution locatable (its value is the final
/// `"value":"<hex64>"` in the body bytes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealBody {
    pub frame_count: u64,
    pub first_seq: Option<Seq>,
    pub last_seq: Option<Seq>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovered_by_epoch: Option<u64>,
    pub digest: Digest,
}

const DIGEST_PLACEHOLDER: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// Locate the byte range of the seal digest's 64 hex chars: the LAST
/// `"value":"` in the body (the digest is the last field by construction).
fn digest_value_range(seal_body: &[u8]) -> Result<std::ops::Range<usize>> {
    let needle = b"\"value\":\"";
    let hay = seal_body;
    let mut pos = None;
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            pos = Some(i + needle.len());
        }
        i += 1;
    }
    let start = pos.ok_or_else(|| Error::Schema("seal body has no digest value".into()))?;
    if start + 64 > hay.len() {
        return Err(Error::Schema("seal digest value truncated".into()));
    }
    Ok(start..start + 64)
}

/// Compute the seal digest preimage contribution of a seal RECORD's wire
/// bytes: body-CRC prelude field zeroed, digest value zeroed. Both
/// substitutions are in place and length-preserving (ADR 0039).
fn seal_record_preimage(seal_wire: &[u8]) -> Result<Vec<u8>> {
    let mut w = seal_wire.to_vec();
    w[BODY_CRC_RANGE].fill(0);
    let body_off = record::PRELUDE_LEN;
    let range = digest_value_range(&w[body_off..])?;
    w[body_off + range.start..body_off + range.end].fill(b'0');
    Ok(w)
}

/// Commit policy for one append. State-bearing frames are Immediate per the
/// ADR's durability invariants; opaque producer output may buffer behind the
/// capsule-publication watermark (caller flushes via `commit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commit {
    Immediate,
    Buffered,
}

/// Everything a seal record needs to know. `recovery` present iff bytes were
/// actually discarded (the all-or-none group; a clean successor-seal carries
/// none — ADR 0039 lifecycle §4 as revised after review).
pub(crate) struct SealMeta {
    pub frame_count: u64,
    pub first_seq: Option<Seq>,
    pub last_seq: Option<Seq>,
    pub recovery: Option<RecoveryMeta>,
}

/// Build the seal record for a segment whose preceding bytes have already
/// been fed to `hasher` (SEAL_DOMAIN ‖ header record ‖ frame records — as
/// WIRE BYTES, never re-serialized). Returns (seal record wire bytes, digest).
pub(crate) fn build_seal_record(mut hasher: Sha256, meta: &SealMeta) -> Result<(Vec<u8>, Digest)> {
    let placeholder = SealBody {
        frame_count: meta.frame_count,
        first_seq: meta.first_seq,
        last_seq: meta.last_seq,
        recovered: meta.recovery.as_ref().map(|_| true),
        truncated_bytes: meta.recovery.as_ref().map(|r| r.truncated_bytes),
        truncation_reason: meta.recovery.as_ref().map(|r| r.reason.clone()),
        recovered_by_epoch: meta.recovery.as_ref().map(|r| r.by_epoch),
        digest: Digest {
            algo: "sha256".into(),
            value: DIGEST_PLACEHOLDER.into(),
        },
    };
    let placeholder_body = serde_json::to_vec(&placeholder)?;
    // Preimage seal record: 64-zero digest in the body; zeroed body-CRC via
    // the encode override (both length-preserving, ADR encoding atoms).
    let preimage_wire = record::encode(RecordKind::Seal, &placeholder_body, Some(0))?;
    hasher.update(&preimage_wire);
    let digest_hex = hex(&hasher.finalize());
    // Real record: identical body bytes with the 64 chars replaced in place.
    let mut body = placeholder_body;
    let range = digest_value_range(&body)?;
    body[range].copy_from_slice(digest_hex.as_bytes());
    let wire = record::encode(RecordKind::Seal, &body, None)?;
    Ok((
        wire,
        Digest {
            algo: "sha256".into(),
            value: digest_hex,
        },
    ))
}

pub struct SegmentWriter {
    file: File,
    seg_dir: PathBuf,
    identity: SegmentIdentity,
    hasher: Sha256, // domain ‖ header ‖ frames, updated per record write
    frame_count: u64,
    first_seq: Option<Seq>,
    last_seq: Option<Seq>,
    sealed: bool,
}

impl SegmentWriter {
    /// O_EXCL-create `.open`, write the header record, fsync file + dir —
    /// only after this may frames append or acks fire (ADR 0039 §lifecycle 2).
    pub fn create(seg_dir: &Path, header: HeaderBody) -> Result<Self> {
        if header.segment_index > U53_MAX || header.epoch > U53_MAX {
            return Err(Error::Schema("segment identity exceeds u53".into()));
        }
        if (header.segment_index == 0) != header.prev_seal_digest.is_none() {
            return Err(Error::Schema(
                "prev_seal_digest is null iff genesis (index 0)".into(),
            ));
        }
        if (header.segment_index == 0) != header.retention_class.is_some() {
            return Err(Error::Schema("retention_class is genesis-only".into()));
        }
        let identity = SegmentIdentity {
            voyage_id: header.voyage_id.clone(),
            segment_index: header.segment_index,
            epoch: header.epoch,
        };
        let path = identity.path(seg_dir, SegmentState::Open);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let body = serde_json::to_vec(&header)?;
        let wire = record::encode(RecordKind::Header, &body, None)?;
        let mut hasher = Sha256::new();
        hasher.update(SEAL_DOMAIN);
        hasher.update(&wire);
        file.write_all(&wire)?;
        file.sync_all()?;
        fsutil::fsync_dir(seg_dir)?;
        Ok(Self {
            file,
            seg_dir: seg_dir.to_path_buf(),
            identity,
            hasher,
            frame_count: 0,
            first_seq: None,
            last_seq: None,
            sealed: false,
        })
    }

    pub fn identity(&self) -> &SegmentIdentity {
        &self.identity
    }

    /// Append one frame. `n` must be the successor of the last (contiguity
    /// is an identity rule, not a verifier afterthought); the frame's epoch
    /// must equal the segment's.
    pub fn append(&mut self, env: &Envelope, commit: Commit) -> Result<()> {
        if self.sealed {
            return Err(Error::State("segment already sealed".into()));
        }
        env.validate()?;
        if env.seq.epoch != self.identity.epoch {
            return Err(Error::Schema("frame epoch != segment epoch".into()));
        }
        if let Some(last) = self.last_seq {
            if env.seq.n != last.n + 1 {
                return Err(Error::Schema(format!(
                    "non-contiguous n: {} after {}",
                    env.seq.n, last.n
                )));
            }
        }
        let body = serde_json::to_vec(env)?;
        let wire = record::encode(RecordKind::Frame, &body, None)?;
        self.file.write_all(&wire)?;
        if commit == Commit::Immediate {
            self.file.sync_all()?;
        }
        self.hasher.update(&wire);
        self.frame_count += 1;
        if self.first_seq.is_none() {
            self.first_seq = Some(env.seq);
        }
        self.last_seq = Some(env.seq);
        Ok(())
    }

    /// The visibility watermark: nothing buffered may be published to any
    /// watcher before this returns.
    pub fn commit(&mut self) -> Result<()> {
        self.file.sync_all()?;
        Ok(())
    }

    /// Seal + publish: append seal record → fsync → RENAME_NOREPLACE to
    /// `.sotseg` → fsync dir. Returns the seal digest for chaining.
    pub fn seal(mut self, recovery: Option<RecoveryMeta>) -> Result<Digest> {
        if self.sealed {
            return Err(Error::State("segment already sealed".into()));
        }
        let meta = SealMeta {
            frame_count: self.frame_count,
            first_seq: self.first_seq,
            last_seq: self.last_seq,
            recovery,
        };
        let (wire, digest) = build_seal_record(self.hasher.clone(), &meta)?;
        self.file.write_all(&wire)?;
        self.file.sync_all()?;
        let from = self.identity.path(&self.seg_dir, SegmentState::Open);
        let to = self.identity.path(&self.seg_dir, SegmentState::Sealed);
        fsutil::rename_noreplace(&from, &to)?;
        fsutil::fsync_dir(&self.seg_dir)?;
        self.sealed = true;
        Ok(digest)
    }
}

#[derive(Debug, Clone)]
pub struct RecoveryMeta {
    pub truncated_bytes: u64,
    pub reason: String,
    pub by_epoch: u64,
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// A fully parsed segment. `tail` is set only for unsealed files whose final
/// record was a provable tear (the reader NEVER truncates — recovery does).
pub struct SegmentReader {
    pub header: HeaderBody,
    pub frames: Vec<Envelope>,
    pub seal: Option<SealBody>,
    /// (offset, bytes_dropped_if_truncated_here)
    pub tail_tear: Option<(u64, u64)>,
    /// Raw wire bytes, kept for digest verification.
    bytes: Vec<u8>,
    /// Offsets: header record end, seal record start (if sealed).
    seal_start: Option<u64>,
}

impl SegmentReader {
    /// Read + structurally validate one segment file. `strict_sealed`
    /// = true applies `.sotseg` strictness: every defect loud, seal present,
    /// seal ends exactly at EOF.
    pub fn read(path: &Path, strict_sealed: bool) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let mut offset: u64 = 0;
        let mut header: Option<HeaderBody> = None;
        let mut frames: Vec<Envelope> = Vec::new();
        let mut seal: Option<SealBody> = None;
        let mut seal_start: Option<u64> = None;
        let mut tail_tear: Option<(u64, u64)> = None;

        loop {
            match record::decode_at(&bytes, offset) {
                Ok(None) => break,
                Ok(Some(rec)) => {
                    if seal.is_some() {
                        // The writer never appends after sealing: post-seal
                        // bytes are loud corruption in EVERY state.
                        return Err(Error::Corrupt {
                            offset,
                            what: "record after seal".into(),
                        });
                    }
                    match rec.kind {
                        RecordKind::Header => {
                            if header.is_some() {
                                return Err(Error::Corrupt {
                                    offset,
                                    what: "second header record".into(),
                                });
                            }
                            header = Some(serde_json::from_slice(&rec.body)?);
                        }
                        RecordKind::Frame => {
                            if header.is_none() {
                                return Err(Error::Corrupt {
                                    offset,
                                    what: "frame before header".into(),
                                });
                            }
                            let env: Envelope = serde_json::from_slice(&rec.body)?;
                            env.validate()?;
                            frames.push(env);
                        }
                        RecordKind::Seal => {
                            if header.is_none() {
                                return Err(Error::Corrupt {
                                    offset,
                                    what: "seal before header".into(),
                                });
                            }
                            seal_start = Some(offset);
                            seal = Some(serde_json::from_slice(&rec.body)?);
                        }
                    }
                    offset += rec.wire_len as u64;
                }
                Err(e @ Error::TornTail { .. }) => {
                    if strict_sealed || seal.is_some() {
                        // Tears exist only in unsealed files with no seal yet.
                        return Err(Error::Corrupt {
                            offset,
                            what: format!("tear-shaped defect where none is permitted: {e}"),
                        });
                    }
                    tail_tear = Some((offset, bytes.len() as u64 - offset));
                    break;
                }
                Err(e) => return Err(e),
            }
        }

        let header = header.ok_or_else(|| Error::Corrupt {
            offset: 0,
            what: "missing header record".into(),
        })?;
        if strict_sealed && seal.is_none() {
            return Err(Error::Corrupt {
                offset,
                what: "sealed segment without a seal".into(),
            });
        }
        Ok(Self {
            header,
            frames,
            seal,
            tail_tear,
            bytes,
            seal_start,
        })
    }

    /// True iff the file carries a valid seal ending exactly at EOF (the
    /// publish-as-is reconciliation case for `.open`).
    pub fn seal_at_eof(&self) -> bool {
        self.seal.is_some() && self.tail_tear.is_none()
    }

    /// Byte length of the valid prefix (through the last complete record
    /// before any tear).
    pub fn valid_prefix_len(&self) -> u64 {
        match self.tail_tear {
            Some((offset, _)) => offset,
            None => self.bytes.len() as u64,
        }
    }

    pub fn raw_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Recompute the seal digest from the wire bytes and compare (ADR 0039
    /// preimage). Also checks metadata consistency (count/first/last).
    pub fn verify_seal(&self) -> Result<()> {
        let seal = self.seal.as_ref().ok_or_else(|| Error::State("no seal".into()))?;
        let seal_start = self.seal_start.unwrap() as usize;
        let mut h = Sha256::new();
        h.update(SEAL_DOMAIN);
        h.update(&self.bytes[..seal_start]);
        h.update(seal_record_preimage(&self.bytes[seal_start..])?);
        let computed = hex(&h.finalize());
        if seal.digest.algo != "sha256" || seal.digest.value != computed {
            return Err(Error::Corrupt {
                offset: seal_start as u64,
                what: "seal digest mismatch".into(),
            });
        }
        if seal.frame_count != self.frames.len() as u64
            || seal.first_seq != self.frames.first().map(|f| f.seq)
            || seal.last_seq != self.frames.last().map(|f| f.seq)
        {
            return Err(Error::Corrupt {
                offset: seal_start as u64,
                what: "seal metadata disagrees with frames".into(),
            });
        }
        Ok(())
    }
}

// The STORE (not the codec) is Linux-only in v1: publication needs an
// atomic no-clobber rename, and `rename_noreplace` fails closed off
// Linux (ADR 0039). These tests therefore run where the store runs;
// Windows joins with P3, macOS when it gets a renamex_np arm. The
// pure-codec tests in record.rs/envelope.rs stay on every platform.
#[cfg(all(test, target_os = "linux"))]
pub(crate) mod tests {
    use super::*;
    use crate::envelope::*;
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
}
