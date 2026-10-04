//! The checklist pass: `verify_voyage_mode` walks every segment of a voyage and applies each rule.

use super::*;
use super::lifecycle::{check_input_fact, check_lifecycle_fields, check_producer_spawn, check_take_state};

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
                if !seen.contains_key(&(r.frame.epoch, r.frame.n)) {
                    return Err(Error::Schema(format!(
                        "frame {:?}: {:?} ref to unresolved/later frame {:?}",
                        env.seq, r.kind, r.frame
                    )));
                }
            }
            if let Some(stream) = &env.stream {
                if let Some(prev) = stream.prev {
                    if !seen.contains_key(&(prev.epoch, prev.n)) {
                        return Err(Error::Schema(format!(
                            "frame {:?}: stream.prev to unresolved frame {:?}",
                            env.seq, prev
                        )));
                    }
                }
            }

            let attached_to: Vec<Seq> = env
                .refs
                .iter()
                .filter(|r| r.kind == RefKind::AttachedTo)
                .map(|r| r.frame)
                .collect();

            // Cross-field: Actor.kind=controller <=> controller_id + take_epoch.
            let actor = &env.source.actor;
            let actor_fields_present = actor.controller_id.is_some() || actor.take_epoch.is_some();
            if actor.kind == ActorKind::Controller {
                if actor.controller_id.is_none() || actor.take_epoch.is_none() {
                    return Err(Error::Schema(format!(
                        "frame {:?}: controller actor needs controller_id and take_epoch",
                        env.seq
                    )));
                }
            } else if actor_fields_present {
                return Err(Error::Schema(format!(
                    "frame {:?}: controller_id/take_epoch forbidden for actor kind {:?}",
                    env.seq, actor.kind
                )));
            }
            // take_epoch ordering: a controller-actor frame must carry the
            // currently committed take_epoch.
            if actor.kind == ActorKind::Controller {
                let te = actor.take_epoch.expect("checked above");
                if te != committed_take_epoch {
                    return Err(Error::Schema(format!(
                        "frame {:?}: controller take_epoch {te} != committed {committed_take_epoch}",
                        env.seq
                    )));
                }
            }

            // Cross-field: stream/transformed must carry a resolvable
            // attached_to (resolution itself is the generic ref-loop above).
            if (env.stream.is_some() || env.transformed.is_some()) && attached_to.is_empty() {
                return Err(Error::Schema(format!(
                    "frame {:?}: stream/transformed needs an attached_to",
                    env.seq
                )));
            }
            // Producer frames carry exactly one same-epoch attached_to →
            // their producer_attached frame.
            if env.class == Class::Producer
                && (attached_to.len() != 1 || attached_to[0].epoch != *epoch)
            {
                return Err(Error::Schema(format!(
                    "producer frame {:?} needs exactly one same-epoch attached_to",
                    env.seq
                )));
            }

            // Stream prev-chains: linear, unique head, per (attached_to, cell).
            if let Some(stream) = &env.stream {
                if attached_to.len() != 1 {
                    return Err(Error::Schema(format!(
                        "frame {:?}: a stream frame needs exactly one attached_to",
                        env.seq
                    )));
                }
                let key = ((attached_to[0].epoch, attached_to[0].n), stream.cell.clone());
                match stream_head.get(&key) {
                    None => {
                        if stream.prev.is_some() {
                            return Err(Error::Schema(format!(
                                "frame {:?}: first frame of cell {:?} must not carry prev",
                                env.seq, key.1
                            )));
                        }
                    }
                    Some(&last_of_cell) => {
                        let ok = stream
                            .prev
                            .map(|p| (p.epoch, p.n) == last_of_cell)
                            .unwrap_or(false);
                        if !ok {
                            return Err(Error::Schema(format!(
                                "frame {:?}: stream.prev must chain to the immediate predecessor of cell {:?}",
                                env.seq, key.1
                            )));
                        }
                    }
                }
                stream_head.insert(key, (env.seq.epoch, env.seq.n));
            }

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

            // Codex round-1 Minor 10: `profile_def.id`'s str128 bound —
            // one of the four shared-validator sites. Only checked when
            // present as a string (the inline `profile_def` shape);
            // `{blob: ...}` carries no `id` and is untouched here.
            if env.class == Class::ProducerAttached {
                if let Some(payload) = &env.payload {
                    if let Some(id) = payload.get("profile_def").and_then(|pd| pd.get("id")).and_then(|v| v.as_str()) {
                        validate_str128(id, "profile_def.id")
                            .map_err(|e| Error::Schema(format!("producer_attached {:?}: {e}", env.seq)))?;
                    }
                }
            }

            // control_exchange's cross-field matrix.
            if env.class == Class::ControlExchange {
                if let Some(payload) = &env.payload {
                    let phase = payload
                        .get("phase")
                        .and_then(|p| serde_json::from_value::<ExchangePhase>(p.clone()).ok())
                        .ok_or_else(|| {
                            Error::Schema(format!(
                                "control_exchange {:?}: invalid/missing phase",
                                env.seq
                            ))
                        })?;
                    let has_to = payload.get("to").is_some();
                    let has_scope = payload.get("scope").is_some();
                    let has_target = payload.get("target").is_some();
                    let responds_to = env.refs.iter().filter(|r| r.kind == RefKind::RespondsTo).count();
                    match phase {
                        ExchangePhase::Request => {
                            if !has_to || responds_to != 0 {
                                return Err(Error::Schema(format!(
                                    "control_exchange {:?}: request needs to, forbids responds_to",
                                    env.seq
                                )));
                            }
                        }
                        ExchangePhase::Response => {
                            if responds_to != 1 || has_to || has_scope || has_target {
                                return Err(Error::Schema(format!(
                                    "control_exchange {:?}: response needs exactly one responds_to, forbids to/scope/target",
                                    env.seq
                                )));
                            }
                        }
                        ExchangePhase::Outcome => {
                            if !has_scope || !has_target || responds_to != 0 {
                                return Err(Error::Schema(format!(
                                    "control_exchange {:?}: outcome needs scope and target, forbids responds_to",
                                    env.seq
                                )));
                            }
                        }
                    }
                }
            }

            // turn_close: exactly one non-duplicate_of winner per turn, and
            // caused_by must target a turn_open frame.
            if env.class == Class::TurnClose {
                let caused_by: Vec<Seq> = env
                    .refs
                    .iter()
                    .filter(|r| r.kind == RefKind::CausedBy)
                    .map(|r| r.frame)
                    .collect();
                if caused_by.len() != 1 {
                    return Err(Error::Schema(format!(
                        "turn_close {:?}: needs exactly one caused_by",
                        env.seq
                    )));
                }
                let turn = (caused_by[0].epoch, caused_by[0].n);
                if seen.get(&turn) != Some(&Class::TurnOpen) {
                    return Err(Error::Schema(format!(
                        "turn_close {:?}: caused_by does not target a turn_open frame",
                        env.seq
                    )));
                }
                let dup: Vec<Seq> = env
                    .refs
                    .iter()
                    .filter(|r| r.kind == RefKind::DuplicateOf)
                    .map(|r| r.frame)
                    .collect();
                if dup.len() > 1 {
                    return Err(Error::Schema(format!(
                        "turn_close {:?}: multiple duplicate_of refs",
                        env.seq
                    )));
                }
                match turn_close_winner.get(&turn) {
                    None => {
                        if !dup.is_empty() {
                            return Err(Error::Schema(format!(
                                "turn_close {:?}: first close for its turn carries duplicate_of",
                                env.seq
                            )));
                        }
                        turn_close_winner.insert(turn, (env.seq.epoch, env.seq.n));
                    }
                    Some(&winner) => match dup.first() {
                        None => {
                            return Err(Error::Schema(format!(
                                "turn_close {:?}: turn already closed; needs duplicate_of",
                                env.seq
                            )))
                        }
                        Some(&d) if (d.epoch, d.n) == winner => {}
                        Some(_) => {
                            return Err(Error::Schema(format!(
                                "turn_close {:?}: duplicate_of does not point at the winning close",
                                env.seq
                            )))
                        }
                    },
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
                // Register this input frame's idem_key for the WAL lattice
                // and the reuse-across-different-inputs check.
                if let Some(payload) = &env.payload {
                    let idem_key = payload
                        .get("idem_key")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            Error::Schema(format!("input {:?}: missing idem_key", env.seq))
                        })?
                        .to_string();
                    // Finding 8: a JSON-string check alone lets an
                    // uppercase or malformed key through, which could
                    // verify green yet let the store's OWN dedupe fold
                    // (`voyage::parse_idem_key`, the shared implementation
                    // — one format check, not two) omit the identity from
                    // its index and re-forward it after a crash.
                    if crate::voyage::parse_idem_key(&idem_key).is_none() {
                        return Err(Error::Schema(format!(
                            "input {:?}: idem_key {idem_key:?} is not lowercase hex32",
                            env.seq
                        )));
                    }
                    match idem_owner.get(&idem_key) {
                        Some(owner) if *owner != (env.seq.epoch, env.seq.n) => {
                            return Err(Error::Schema(format!(
                                "input {:?}: idem_key {idem_key} reused from earlier input frame {owner:?}",
                                env.seq
                            )));
                        }
                        Some(_) => {}
                        None => {
                            idem_owner.insert(idem_key.clone(), (env.seq.epoch, env.seq.n));
                            idem_state.insert(idem_key.clone(), FactState::Input);
                        }
                    }
                    input_idem.insert((env.seq.epoch, env.seq.n), idem_key);
                }
            }

            // Blob presence + length: artifact_ref's embedded blob, and any
            // frame's payload_ref (digest shape is checked by
            // `validate_blob_ref`, called from `check_blob_on_disk`).
            if env.class == Class::ArtifactRef {
                if let Some(payload) = &env.payload {
                    let blob: BlobRef = payload
                        .get("blob")
                        .ok_or_else(|| {
                            Error::Schema(format!("artifact_ref {:?}: missing blob", env.seq))
                        })
                        .and_then(|v| {
                            serde_json::from_value(v.clone()).map_err(|e| {
                                Error::Schema(format!(
                                    "artifact_ref {:?}: blob malformed: {e}",
                                    env.seq
                                ))
                            })
                        })?;
                    check_blob_on_disk(root, &blob, env.seq)?;
                }
            }
            // payload_ref is producer-class only — enforced in
            // Envelope::validate(), which runs on BOTH append and segment
            // read, so a spilled control-plane frame (which would carry
            // its cross-field obligations out of this walk's sight) can
            // neither be written nor read. No duplicate check here.
            if let Some(pr) = &env.payload_ref {
                check_blob_on_disk(root, &pr.blob, env.seq)?;
            }

            if env.class == Class::TurnOpen {
                turn_opens.insert((env.seq.epoch, env.seq.n));
            }
            // Producer-payload numbers: integer atoms unless the segment
            // declares sot.producer.json-f64-v1 (ADR 0039 registry). Covers
            // BOTH carriers (review F1): the inline payload AND a
            // payload_ref with encoding json-utf8 — a spilled JSON payload
            // is still JSON and must obey the same atoms. encoding "bytes"
            // is never parsed, so no number rule can apply to it.
            if env.class == Class::Producer && !f64_ok {
                if let Some(payload) = &env.payload {
                    check_integer_numbers(payload).map_err(|what| {
                        Error::Schema(format!(
                            "producer frame {:?}: {what} without sot.producer.json-f64-v1",
                            env.seq
                        ))
                    })?;
                }
                if let Some(pr) = &env.payload_ref {
                    if pr.encoding == crate::envelope::PayloadEncoding::JsonUtf8 {
                        let bytes = read_blob(root, &pr.blob, env.seq)?;
                        let v: serde_json::Value =
                            serde_json::from_slice(&bytes).map_err(|e| Error::Schema(format!(
                                "producer frame {:?}: json-utf8 payload_ref does not parse: {e}",
                                env.seq
                            )))?;
                        check_integer_numbers(&v).map_err(|what| {
                            Error::Schema(format!(
                                "producer frame {:?} (via payload_ref): {what} without sot.producer.json-f64-v1",
                                env.seq
                            ))
                        })?;
                    }
                }
            }
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
