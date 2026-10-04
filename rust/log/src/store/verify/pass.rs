//! The checklist pass: `verify_voyage_mode` walks every segment of a voyage and applies each rule.

use super::*;
use super::lifecycle::{check_input_fact, check_lifecycle_fields, check_producer_spawn, check_take_state};
use super::frame::{check_actor, check_attachment, check_blobs, check_control_exchange};
use super::frame::{check_frame_position, check_input, check_producer_attached};
use super::frame::{check_producer_numbers, check_turn_close};

pub fn verify_voyage_mode(root: &Path, voyage_id: &str, mode: VerifyMode) -> Result<()> {
    let seg_dir = root.join("seg");
    let entries = list_segments(&seg_dir)?;
    check_quiescent(&entries)?;

    let mut prev_digest: Option<String> = None;
    let mut prev_epoch: u64 = 0;
    // Every committed frame's class, keyed by seq — doubles as both the
    // ref-resolution existence set and the class lookup turn_close needs.
    let mut seen: HashMap<(u64, u64), Class> = HashMap::new();
    let mut last_n_by_epoch: HashMap<u64, u64> = HashMap::new();
    let mut capture_enabled = false;

    // Exactly one non-duplicate_of turn_close per turn: turn_open seq ->
    // the winning close's seq.
    let mut turn_close_winner: HashMap<(u64, u64), (u64, u64)> = HashMap::new();
    let mut turn_opens: HashSet<(u64, u64)> = HashSet::new();

    // input_fact chain lattice, all keyed by idem_key.
    let mut idem_state: HashMap<String, FactState> = HashMap::new();
    let mut idem_owner: HashMap<String, (u64, u64)> = HashMap::new(); // -> input frame seq
    let mut input_idem: HashMap<(u64, u64), String> = HashMap::new(); // input frame seq -> idem_key
    let mut intent_owner: HashMap<(u64, u64), String> = HashMap::new(); // forward_intent seq -> idem_key

    // Stream prev-chains, keyed by (attached_to seq, cell).
    let mut stream_head: HashMap<((u64, u64), String), (u64, u64)> = HashMap::new();

    // take_epoch ordering.
    let mut committed_take_epoch: u64 = 0;
    let mut take_state_seen_epochs: HashSet<u64> = HashSet::new();

    // Codex round-1 Major 7: at most one `run_end_requested` per writer
    // epoch — a marker governs only its own epoch (ADR 0041), and the
    // capsule's own first-commit-wins latch is a promise about ITS
    // process lifetime, not a proof a crafted or corrupted voyage can't
    // carry two. The verifier must refuse what the writer never would.
    let mut run_end_seen_epochs: HashSet<u64> = HashSet::new();

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
            check_frame_position(env, epoch, &mut last_n_by_epoch, &seen)?;

            let attached_to: Vec<Seq> = env
                .refs
                .iter()
                .filter(|r| r.kind == RefKind::AttachedTo)
                .map(|r| r.frame)
                .collect();

            check_actor(env, committed_take_epoch)?;

            check_attachment(env, epoch, &attached_to, &mut stream_head)?;

            // Redact-by-default as a wire property, plus lifecycle's
            // cross-field matrix and the input_fact / take_state processing.
            if env.class == Class::Lifecycle {
                if let Some(payload) = &env.payload {
                    let kind = payload
                        .get("kind")
                        .and_then(|k| serde_json::from_value::<LifecycleKind>(k.clone()).ok())
                        .ok_or_else(|| {
                            Error::Schema(format!("lifecycle {:?}: invalid/missing kind", env.seq))
                        })?;
                    let has_take = payload.get("take").is_some();
                    let has_fact = payload.get("fact").is_some();
                    check_lifecycle_fields(env, payload, kind, has_take, has_fact, &mut run_end_seen_epochs)?;
                    if kind == LifecycleKind::CaptureOptin {
                        capture_enabled = true;
                    }
                    if kind == LifecycleKind::RunEndRequested && !run_end_requested_ok {
                        // ADR 0039 registry (bidirectional, like
                        // cgroup-fence-v1's locator-must-declare): the
                        // frame is only legal in a segment that declared
                        // the feature at creation.
                        return Err(Error::Schema(format!(
                            "lifecycle {:?}: run_end_requested in a segment that does not declare sot.capsule.run-end-requested-v1",
                            env.seq
                        )));
                    }
                    check_producer_spawn(env, payload, kind, fence_ok)?;
                    committed_take_epoch = check_take_state(env, payload, kind, committed_take_epoch, &mut take_state_seen_epochs)?;
                    check_input_fact(env, payload, kind, &input_idem, &mut idem_state, &mut intent_owner)?;
                }
            }

            check_producer_attached(env)?;

            check_control_exchange(env)?;

            check_turn_close(env, &seen, &mut turn_close_winner)?;

            check_input(env, capture_enabled, &mut idem_owner, &mut idem_state, &mut input_idem)?;

            check_blobs(root, env)?;

            if env.class == Class::TurnOpen {
                turn_opens.insert((env.seq.epoch, env.seq.n));
            }
            check_producer_numbers(root, env, f64_ok)?;
            seen.insert((env.seq.epoch, env.seq.n), env.class);
        }
    }

    check_turn_closure(&entries, &turn_opens, &turn_close_winner, mode)
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
