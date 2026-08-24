//! Voyage verifier (ADR 0039 §Verifier). This slice implements the
//! structural core: wrapper/CRC validity (via the reader), seal digests +
//! chain, filename⇔header identity, index continuity, epoch monotonicity
//! (nondecreasing, run-boundary changes), per-epoch `n` contiguity,
//! quiescent-state file counts, structural ref resolution, and
//! capture-before-inline-input.
//!
//! NOT yet enforced here (tracked follow-ups, per the ADR's full list): the
//! complete cross-field matrix, the idem_key chain lattice, stream
//! `prev`-chain linearity, take_epoch ordering, and blob length checks.
//! They land with the capsule that produces those frames; stating the gap
//! beats pretending coverage.

use crate::envelope::{Class, InputContent, LifecycleKind, RefKind, Seq};
use crate::segment::{SegmentIdentity, SegmentReader, SegmentState};
use crate::{Error, Result};
use std::collections::HashSet;
use std::path::Path;

pub fn verify_voyage(root: &Path, voyage_id: &str) -> Result<()> {
    let seg_dir = root.join("seg");
    let mut entries: Vec<(u64, u64, SegmentState)> = Vec::new();
    for entry in std::fs::read_dir(&seg_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(Error::State("non-utf8 segment filename".into()));
        };
        if name == ".tmp" {
            continue;
        }
        let parsed = SegmentIdentity::parse_file_name(name)
            .ok_or_else(|| Error::State(format!("unparseable segment filename {name}")))?;
        entries.push(parsed);
    }
    entries.sort_unstable();

    // Quiescent state: at most one non-sealed file, and only at the tip.
    let non_sealed: Vec<&(u64, u64, SegmentState)> = entries
        .iter()
        .filter(|(_, _, s)| *s != SegmentState::Sealed)
        .collect();
    if non_sealed.len() > 1 {
        return Err(Error::State(format!(
            "{} non-sealed segment files in quiescent state",
            non_sealed.len()
        )));
    }
    if let Some((idx, _, state)) = non_sealed.first() {
        if *state != SegmentState::Open {
            return Err(Error::State(format!(
                "mid-transaction file ({state:?}) present in quiescent state"
            )));
        }
        if entries.iter().any(|(i, _, _)| i > idx) {
            return Err(Error::State("open segment is not the chain tip".into()));
        }
    }

    let mut prev_digest: Option<String> = None;
    let mut prev_epoch: u64 = 0;
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut last_n_by_epoch: std::collections::HashMap<u64, u64> = Default::default();
    let mut capture_enabled = false;

    for (expected_index, (idx, epoch, state)) in entries.iter().enumerate() {
        if *idx != expected_index as u64 {
            return Err(Error::State(format!(
                "segment index gap: expected {expected_index}, found {idx}"
            )));
        }
        if *epoch < prev_epoch {
            return Err(Error::Corrupt {
                offset: 0,
                what: format!("epoch regressed at segment {idx}"),
            });
        }
        prev_epoch = *epoch;

        let id = SegmentIdentity {
            voyage_id: voyage_id.to_string(),
            segment_index: *idx,
            epoch: *epoch,
        };
        let sealed = *state == SegmentState::Sealed;
        let reader = SegmentReader::read(&id.path(&seg_dir, *state), sealed)?;

        // Filename ⇔ header identity.
        if reader.header.segment_index != *idx
            || reader.header.epoch != *epoch
            || reader.header.voyage_id != voyage_id
        {
            return Err(Error::Corrupt {
                offset: 0,
                what: format!("segment {idx}: filename and header identity disagree"),
            });
        }
        if (*idx == 0) != reader.header.retention_class.is_some() {
            return Err(Error::Schema("retention_class is genesis-only".into()));
        }
        if !reader.header.required_features.is_empty() {
            return Err(Error::State(format!(
                "segment {idx} requires features {:?} (none registered in v1)",
                reader.header.required_features
            )));
        }
        // Chain.
        let header_prev = reader.header.prev_seal_digest.as_ref().map(|d| d.value.clone());
        if header_prev != prev_digest {
            return Err(Error::Corrupt {
                offset: 0,
                what: format!("segment {idx} breaks the seal chain"),
            });
        }
        if sealed {
            reader.verify_seal()?;
            prev_digest = reader.seal.as_ref().map(|s| s.digest.value.clone());
        }

        // Frames: epoch match, contiguity, refs resolve earlier, capture rule.
        for env in &reader.frames {
            if env.seq.epoch != *epoch {
                return Err(Error::Schema(format!(
                    "frame {:?} epoch differs from segment epoch {epoch}",
                    env.seq
                )));
            }
            let last = last_n_by_epoch.entry(*epoch).or_insert(0);
            if env.seq.n != *last + 1 {
                return Err(Error::Schema(format!(
                    "epoch {epoch}: non-contiguous n {} after {}",
                    env.seq.n, last
                )));
            }
            *last = env.seq.n;

            for r in &env.refs {
                if !seen.contains(&(r.frame.epoch, r.frame.n)) {
                    return Err(Error::Schema(format!(
                        "frame {:?}: {:?} ref to unresolved/later frame {:?}",
                        env.seq, r.kind, r.frame
                    )));
                }
            }
            if let Some(stream) = &env.stream {
                if let Some(prev) = stream.prev {
                    if !seen.contains(&(prev.epoch, prev.n)) {
                        return Err(Error::Schema(format!(
                            "frame {:?}: stream.prev to unresolved frame {:?}",
                            env.seq, prev
                        )));
                    }
                }
            }
            // Producer frames carry exactly one same-epoch attached_to.
            if env.class == Class::Producer {
                let attached: Vec<&Seq> = env
                    .refs
                    .iter()
                    .filter(|r| r.kind == RefKind::AttachedTo)
                    .map(|r| &r.frame)
                    .collect();
                if attached.len() != 1 || attached[0].epoch != *epoch {
                    return Err(Error::Schema(format!(
                        "producer frame {:?} needs exactly one same-epoch attached_to",
                        env.seq
                    )));
                }
            }
            // Redact-by-default as a wire property.
            if env.class == Class::Lifecycle {
                let kind = env
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("kind"))
                    .and_then(|k| serde_json::from_value::<LifecycleKind>(k.clone()).ok());
                if kind == Some(LifecycleKind::CaptureOptin) {
                    capture_enabled = true;
                }
            }
            if env.class == Class::Input {
                let content = env
                    .payload
                    .as_ref()
                    .and_then(|p| p.get("content"))
                    .and_then(InputContent::from_value);
                match content {
                    Some(InputContent::Redacted) => {}
                    Some(_) if capture_enabled => {}
                    Some(_) => {
                        return Err(Error::Schema(format!(
                            "input {:?} carries bytes before capture_optin",
                            env.seq
                        )))
                    }
                    None => {
                        return Err(Error::Schema(format!(
                            "input {:?} has no valid content",
                            env.seq
                        )))
                    }
                }
            }
            seen.insert((env.seq.epoch, env.seq.n));
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::envelope::*;
    use crate::segment::{tests::test_env, Commit, RetentionClass};
    use crate::voyage::VoyageStore;
    use serde_json::json;

    fn store(dir: &Path, name: &str) -> VoyageStore {
        let root = dir.join(name);
        VoyageStore::bootstrap(&root, name, RetentionClass::Discard).unwrap();
        VoyageStore::open_for_writing(&root, name).unwrap()
    }

    #[test]
    fn inline_input_before_optin_fails_after_optin_passes() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "v");
        let mut w = s.open_segment(0).unwrap();

        let mut input = test_env(1, 1);
        input.class = Class::Input;
        input.payload = Some(json!({
            "idem_key": "0".repeat(32),
            "content": {"inline": "secret"}
        }));
        w.append(&input, Commit::Immediate).unwrap();
        let d = w.seal(None).unwrap();
        s.advance_chain(d);
        let root = dir.path().join("v");
        assert!(verify_voyage(&root, "v").is_err());

        // A fresh voyage with optin FIRST verifies green.
        let mut s2 = store(dir.path(), "v2");
        let mut w2 = s2.open_segment(0).unwrap();
        let mut optin = test_env(1, 1);
        optin.class = Class::Lifecycle;
        optin.payload = Some(json!({"kind": "capture_optin"}));
        w2.append(&optin, Commit::Immediate).unwrap();
        let mut input2 = test_env(1, 2);
        input2.class = Class::Input;
        input2.payload = Some(json!({
            "idem_key": "0".repeat(32),
            "content": {"inline": "ok now"}
        }));
        w2.append(&input2, Commit::Immediate).unwrap();
        w2.seal(None).unwrap();
        verify_voyage(&dir.path().join("v2"), "v2").unwrap();
    }

    #[test]
    fn forward_ref_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "v3");
        let mut w = s.open_segment(0).unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "producer_ready"}));
        e.refs = vec![FrameRef {
            kind: RefKind::CausedBy,
            frame: Seq { epoch: 1, n: 5 }, // later frame
        }];
        w.append(&e, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("v3"), "v3").is_err());
    }

    #[test]
    fn producer_frame_requires_attached_to() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "v4");
        let mut w = s.open_segment(0).unwrap();
        // test_env is class=Producer with no refs — must fail verification.
        w.append(&test_env(1, 1), Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("v4"), "v4").is_err());

        // With a producer_attached + attached_to it verifies.
        let mut s2 = store(dir.path(), "v5");
        let mut w2 = s2.open_segment(0).unwrap();
        let mut att = test_env(1, 1);
        att.class = Class::ProducerAttached;
        att.payload = Some(json!({
            "producer_kind": "julia-repl", "version": "1",
            "profile_def": {"id": "default", "sha256": "0".repeat(64), "rules": {}}
        }));
        w2.append(&att, Commit::Immediate).unwrap();
        let mut prod = test_env(1, 2);
        prod.refs = vec![FrameRef {
            kind: RefKind::AttachedTo,
            frame: Seq { epoch: 1, n: 1 },
        }];
        w2.append(&prod, Commit::Immediate).unwrap();
        w2.seal(None).unwrap();
        verify_voyage(&dir.path().join("v5"), "v5").unwrap();
    }
}
