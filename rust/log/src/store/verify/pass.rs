//! The checklist pass: `verify_voyage_mode` walks every segment of a voyage and applies each rule.

use super::*;
use super::frame::{check_frame, WalkState};

pub fn verify_voyage_mode(root: &Path, voyage_id: &str, mode: VerifyMode) -> Result<()> {
    let seg_dir = root.join("seg");
    let entries = list_segments(&seg_dir)?;
    check_quiescent(&entries)?;

    let mut prev_digest: Option<String> = None;
    let mut prev_epoch: u64 = 0;
    let mut walk = WalkState::default();

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

        let (f64_ok, fence_ok, run_end_requested_ok) = check_segment_header(&reader, idx, epoch, voyage_id)?;
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

        // Frames: epoch match, contiguity, refs resolve earlier, capture
        // rule, cross-field matrix, WAL lattice, stream chains, take
        // ordering, blob presence + length.
        for env in &reader.frames {
            check_frame(root, epoch, f64_ok, fence_ok, run_end_requested_ok, env, &mut walk)?;
        }
    }

    check_turn_closure(&entries, &walk.turn_opens, &walk.turn_close_winner, mode)
}

fn list_segments(seg_dir: &Path) -> Result<Vec<(u64, u64, SegmentState)>> {
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
    Ok(entries)
}

fn check_quiescent(entries: &[(u64, u64, SegmentState)]) -> Result<()> {
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
    Ok(())
}

fn check_segment_header(
    reader: &SegmentReader,
    idx: &u64,
    epoch: &u64,
    voyage_id: &str,
) -> Result<(bool, bool, bool)> {
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
    for feat in &reader.header.required_features {
        if !REGISTERED_FEATURES.contains(&feat.as_str()) {
            return Err(Error::State(format!(
                "segment {idx} requires unknown feature {feat:?}"
            )));
        }
    }
    let f64_ok = reader
        .header
        .required_features
        .iter()
        .any(|f| f == "sot.producer.json-f64-v1");
    let fence_ok = reader
        .header
        .required_features
        .iter()
        .any(|f| f == "sot.capsule.cgroup-fence-v1");
    let run_end_requested_ok = reader
        .header
        .required_features
        .iter()
        .any(|f| f == "sot.capsule.run-end-requested-v1");
    Ok((f64_ok, fence_ok, run_end_requested_ok))
}

fn check_turn_closure(
    entries: &[(u64, u64, SegmentState)],
    turn_opens: &HashSet<(u64, u64)>,
    turn_close_winner: &HashMap<(u64, u64), (u64, u64)>,
    mode: VerifyMode,
) -> Result<()> {
    // Turn closure (ADR 0039 amended / ADR 0040): every open needs a winner.
    let unmatched: Vec<(u64, u64)> = {
        let mut u: Vec<(u64, u64)> = turn_opens
            .iter()
            .filter(|t| !turn_close_winner.contains_key(*t))
            .copied()
            .collect();
        u.sort_unstable();
        u
    };
    match mode {
        VerifyMode::Complete => {
            if let Some(t) = unmatched.first() {
                return Err(Error::State(format!(
                    "turn_open {t:?} has no winning close ({} unmatched; complete mode)",
                    unmatched.len()
                )));
            }
        }
        VerifyMode::AllowOpenTip => {
            let tip_open_epoch = entries
                .last()
                .filter(|(_, _, st)| *st == SegmentState::Open)
                .map(|(_, ep, _)| *ep);
            let tolerable = |t: &(u64, u64)| Some(t.0) == tip_open_epoch;
            if unmatched.len() > 1 || unmatched.first().is_some_and(|t| !tolerable(t)) {
                return Err(Error::State(format!(
                    "{} unmatched turn_open(s) beyond the open tip's allowance: {:?}",
                    unmatched.len(),
                    unmatched
                )));
            }
        }
    }
    Ok(())
}
