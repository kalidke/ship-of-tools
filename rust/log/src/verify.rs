//! Voyage verifier (ADR 0039 §Verifier). Implements the full `sot-log
//! verify` checklist: wrapper/CRC validity (via the reader), seal digests +
//! chain, filename⇔header identity, index continuity, epoch monotonicity
//! (nondecreasing, run-boundary changes), per-epoch `n` contiguity,
//! quiescent-state file counts, structural ref resolution,
//! capture-before-inline-input, the cross-field matrix (actor/lifecycle/
//! control_exchange requireds+forbiddens, stream/transformed attached_to,
//! turn_close uniqueness+target), the input_fact chain lattice per
//! idem_key, stream `prev`-chain linearity per (attached_to, cell),
//! take_epoch strict ordering (the null-holder-first-in-epoch rule and
//! controller-actor agreement), and blob presence + length for
//! artifact_ref and payload_ref.
//!
//! Out of scope for this module by design (ADR 0039's "Merge gates for the
//! crate" list, not the `sot-log verify` checklist): cross-language golden
//! fixtures and the crash/fault harness — those exercise the writer and
//! recovery paths, not this reader-side pass.

use crate::envelope::{
    validate_blob_ref, ActorKind, BlobRef, Class, ExchangePhase, InputContent, InputFactKind,
    LifecycleKind, RefKind, Seq,
};
use crate::segment::{SegmentIdentity, SegmentReader, SegmentState};
use crate::{Error, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// `lifecycle.kind=take_state`'s `take` object.
#[derive(Deserialize)]
struct TakeObj {
    take_epoch: u64,
    #[serde(default)]
    holder: Option<String>,
}

/// `lifecycle.kind=input_fact`'s `fact` object.
#[derive(Deserialize)]
struct FactObj {
    input: Seq,
    fact: InputFactKind,
    #[serde(default)]
    intent: Option<Seq>,
}

/// Per-idem_key WAL state (ADR 0039 "Input WAL + dedupe" lattice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FactState {
    Input,
    Intent,
    Forwarded,
    Observed,
    Refused,
}

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
    // Every committed frame's class, keyed by seq — doubles as both the
    // ref-resolution existence set and the class lookup turn_close needs.
    let mut seen: HashMap<(u64, u64), Class> = HashMap::new();
    let mut last_n_by_epoch: HashMap<u64, u64> = HashMap::new();
    let mut capture_enabled = false;

    // Exactly one non-duplicate_of turn_close per turn: turn_open seq ->
    // the winning close's seq.
    let mut turn_close_winner: HashMap<(u64, u64), (u64, u64)> = HashMap::new();

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
                    match kind {
                        LifecycleKind::TakeState => {
                            if !has_take || has_fact {
                                return Err(Error::Schema(format!(
                                    "lifecycle {:?}: take_state needs take, forbids fact",
                                    env.seq
                                )));
                            }
                        }
                        LifecycleKind::InputFact => {
                            if has_take || !has_fact {
                                return Err(Error::Schema(format!(
                                    "lifecycle {:?}: input_fact needs fact, forbids take",
                                    env.seq
                                )));
                            }
                        }
                        _ => {
                            if has_take || has_fact {
                                return Err(Error::Schema(format!(
                                    "lifecycle {:?}: kind {kind:?} forbids take and fact",
                                    env.seq
                                )));
                            }
                        }
                    }
                    if kind == LifecycleKind::CaptureOptin {
                        capture_enabled = true;
                    }
                    if kind == LifecycleKind::TakeState {
                        let take: TakeObj = serde_json::from_value(
                            payload.get("take").expect("checked above").clone(),
                        )
                        .map_err(|e| {
                            Error::Schema(format!("lifecycle {:?}: take malformed: {e}", env.seq))
                        })?;
                        if take.take_epoch <= committed_take_epoch {
                            return Err(Error::Schema(format!(
                                "lifecycle {:?}: take_epoch {} does not strictly increase past {committed_take_epoch}",
                                env.seq, take.take_epoch
                            )));
                        }
                        let writer_epoch = env.seq.epoch;
                        if take_state_seen_epochs.insert(writer_epoch) && take.holder.is_some() {
                            return Err(Error::Schema(format!(
                                "lifecycle {:?}: first take_state in writer epoch {writer_epoch} must have holder=null",
                                env.seq
                            )));
                        }
                        committed_take_epoch = take.take_epoch;
                    }
                    if kind == LifecycleKind::InputFact {
                        let fact: FactObj = serde_json::from_value(
                            payload.get("fact").expect("checked above").clone(),
                        )
                        .map_err(|e| {
                            Error::Schema(format!("lifecycle {:?}: fact malformed: {e}", env.seq))
                        })?;
                        let input_key = (fact.input.epoch, fact.input.n);
                        let idem_key = input_idem.get(&input_key).cloned().ok_or_else(|| {
                            Error::Schema(format!(
                                "lifecycle {:?}: fact.input {:?} is not a committed input frame",
                                env.seq, fact.input
                            ))
                        })?;
                        let chain_state = *idem_state.get(&idem_key).ok_or_else(|| {
                            Error::Schema(format!(
                                "lifecycle {:?}: idem_key {idem_key} has no chain state",
                                env.seq
                            ))
                        })?;
                        let new_state = match fact.fact {
                            InputFactKind::ForwardIntent => {
                                if chain_state != FactState::Input {
                                    return Err(Error::Schema(format!(
                                        "lifecycle {:?}: forward_intent illegal from the current chain state for idem_key {idem_key}",
                                        env.seq
                                    )));
                                }
                                intent_owner.insert((env.seq.epoch, env.seq.n), idem_key.clone());
                                FactState::Intent
                            }
                            InputFactKind::Forwarded => {
                                if chain_state != FactState::Intent {
                                    return Err(Error::Schema(format!(
                                        "lifecycle {:?}: forwarded illegal from the current chain state for idem_key {idem_key}",
                                        env.seq
                                    )));
                                }
                                check_intent_ref(env.seq, fact.fact, fact.intent, &idem_key, &intent_owner)?;
                                FactState::Forwarded
                            }
                            InputFactKind::ProducerObserved => {
                                if chain_state != FactState::Forwarded {
                                    return Err(Error::Schema(format!(
                                        "lifecycle {:?}: producer_observed illegal from the current chain state for idem_key {idem_key}",
                                        env.seq
                                    )));
                                }
                                check_intent_ref(env.seq, fact.fact, fact.intent, &idem_key, &intent_owner)?;
                                FactState::Observed
                            }
                            InputFactKind::RefusedStaleEpoch => {
                                if chain_state != FactState::Input {
                                    return Err(Error::Schema(format!(
                                        "lifecycle {:?}: refused_stale_epoch illegal from the current chain state for idem_key {idem_key}",
                                        env.seq
                                    )));
                                }
                                FactState::Refused
                            }
                        };
                        idem_state.insert(idem_key, new_state);
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
            if let Some(pr) = &env.payload_ref {
                check_blob_on_disk(root, &pr.blob, env.seq)?;
            }

            seen.insert((env.seq.epoch, env.seq.n), env.class);
        }
    }
    Ok(())
}

/// A `BlobRef` must resolve to a CAS file of exactly the recorded length.
fn check_blob_on_disk(root: &Path, blob: &BlobRef, seq: Seq) -> Result<()> {
    validate_blob_ref(blob)?;
    let path = root
        .join("blobs")
        .join(&blob.algo)
        .join(&blob.digest[0..2])
        .join(&blob.digest);
    let meta = std::fs::metadata(&path).map_err(|_| Error::Corrupt {
        offset: 0,
        what: format!("frame {seq:?}: blob {} missing on disk", blob.digest),
    })?;
    if meta.len() != blob.length {
        return Err(Error::Corrupt {
            offset: 0,
            what: format!(
                "frame {seq:?}: blob {} length {} != recorded {}",
                blob.digest,
                meta.len(),
                blob.length
            ),
        });
    }
    Ok(())
}

/// A `forwarded`/`producer_observed` fact's `intent` ref must name an
/// earlier `forward_intent` fact frame for the SAME idem_key.
fn check_intent_ref(
    seq: Seq,
    fact_kind: InputFactKind,
    intent: Option<Seq>,
    idem_key: &str,
    intent_owner: &HashMap<(u64, u64), String>,
) -> Result<()> {
    let intent = intent
        .ok_or_else(|| Error::Schema(format!("lifecycle {seq:?}: {fact_kind:?} needs intent")))?;
    if intent_owner.get(&(intent.epoch, intent.n)).map(String::as_str) != Some(idem_key) {
        return Err(Error::Schema(format!(
            "lifecycle {seq:?}: intent {intent:?} does not name this input's forward_intent"
        )));
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

    // --- helpers shared by the cross-field / lattice / stream / take tests ---

    fn attached_anchor(epoch: u64, n: u64) -> Envelope {
        let mut e = test_env(epoch, n);
        e.class = Class::ProducerAttached;
        e.payload = Some(json!({
            "producer_kind": "julia-repl", "version": "1",
            "profile_def": {"id": "default", "sha256": "0".repeat(64), "rules": {}}
        }));
        e
    }

    fn stream_frame(epoch: u64, n: u64, attached: Seq, cell: &str, prev: Option<Seq>) -> Envelope {
        let mut e = test_env(epoch, n);
        e.refs = vec![FrameRef {
            kind: RefKind::AttachedTo,
            frame: attached,
        }];
        e.stream = Some(Stream {
            cell: cell.into(),
            mode: StreamMode::Replace,
            complete: false,
            prev,
        });
        e
    }

    fn input_env(epoch: u64, n: u64, idem_key: &str) -> Envelope {
        let mut e = test_env(epoch, n);
        e.class = Class::Input;
        e.payload = Some(json!({"idem_key": idem_key, "content": "redacted"}));
        e
    }

    fn fact_env(epoch: u64, n: u64, input: Seq, fact: &str, intent: Option<Seq>) -> Envelope {
        let mut e = test_env(epoch, n);
        e.class = Class::Lifecycle;
        let mut fact_obj = json!({"input": {"epoch": input.epoch, "n": input.n}, "fact": fact});
        if let Some(i) = intent {
            fact_obj["intent"] = json!({"epoch": i.epoch, "n": i.n});
        }
        e.payload = Some(json!({"kind": "input_fact", "fact": fact_obj}));
        e
    }

    // --- Rule 1: cross-field matrix ---

    #[test]
    fn cross_field_actor_controller_requires_id_and_take_epoch() {
        let dir = tempfile::tempdir().unwrap();

        // Violation: controller kind but missing controller_id/take_epoch.
        let mut s = store(dir.path(), "actor-bad");
        let mut w = s.open_segment(0).unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "producer_ready"}));
        e.source.actor.kind = ActorKind::Controller;
        w.append(&e, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("actor-bad"), "actor-bad").is_err());

        // Violation: non-controller actor carrying controller_id (forbidden).
        let mut s2 = store(dir.path(), "actor-bad2");
        let mut w2 = s2.open_segment(0).unwrap();
        let mut e2 = test_env(1, 1);
        e2.class = Class::Lifecycle;
        e2.payload = Some(json!({"kind": "producer_ready"}));
        e2.source.actor.controller_id = Some("c1".into());
        w2.append(&e2, Commit::Immediate).unwrap();
        w2.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("actor-bad2"), "actor-bad2").is_err());

        // Green: controller actor with both fields, take_epoch matching the
        // implicit initial committed take_epoch (0).
        let mut s3 = store(dir.path(), "actor-ok");
        let mut w3 = s3.open_segment(0).unwrap();
        let mut e3 = test_env(1, 1);
        e3.class = Class::Lifecycle;
        e3.payload = Some(json!({"kind": "producer_ready"}));
        e3.source.actor.kind = ActorKind::Controller;
        e3.source.actor.controller_id = Some("c1".into());
        e3.source.actor.take_epoch = Some(0);
        w3.append(&e3, Commit::Immediate).unwrap();
        w3.seal(None).unwrap();
        verify_voyage(&dir.path().join("actor-ok"), "actor-ok").unwrap();
    }

    #[test]
    fn cross_field_lifecycle_kind_requires_matching_object() {
        let dir = tempfile::tempdir().unwrap();

        // Violation: take_state without a take object.
        let mut s = store(dir.path(), "lc-bad1");
        let mut w = s.open_segment(0).unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "take_state"}));
        w.append(&e, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("lc-bad1"), "lc-bad1").is_err());

        // Violation: input_fact without a fact object.
        let mut s2 = store(dir.path(), "lc-bad2");
        let mut w2 = s2.open_segment(0).unwrap();
        let mut e2 = test_env(1, 1);
        e2.class = Class::Lifecycle;
        e2.payload = Some(json!({"kind": "input_fact"}));
        w2.append(&e2, Commit::Immediate).unwrap();
        w2.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("lc-bad2"), "lc-bad2").is_err());

        // Green: take_state WITH a proper take object.
        let mut s3 = store(dir.path(), "lc-ok");
        let mut w3 = s3.open_segment(0).unwrap();
        let mut e3 = test_env(1, 1);
        e3.class = Class::Lifecycle;
        e3.payload = Some(json!({"kind": "take_state", "take": {"take_epoch": 1, "holder": null}}));
        w3.append(&e3, Commit::Immediate).unwrap();
        w3.seal(None).unwrap();
        verify_voyage(&dir.path().join("lc-ok"), "lc-ok").unwrap();
    }

    #[test]
    fn cross_field_control_exchange_phase() {
        let dir = tempfile::tempdir().unwrap();

        // Violation: request phase missing `to`.
        let mut s = store(dir.path(), "ce-bad1");
        let mut w = s.open_segment(0).unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::ControlExchange;
        e.payload = Some(json!({"phase": "request", "kind_ns": "sot.take.request"}));
        w.append(&e, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("ce-bad1"), "ce-bad1").is_err());

        // Green: request with `to`, then a response with exactly one
        // responds_to and none of to/scope/target.
        let mut s2 = store(dir.path(), "ce-ok");
        let mut w2 = s2.open_segment(0).unwrap();
        let mut req = test_env(1, 1);
        req.class = Class::ControlExchange;
        req.payload = Some(json!({
            "phase": "request", "kind_ns": "sot.take.request",
            "to": {"kind": "producer"}
        }));
        w2.append(&req, Commit::Immediate).unwrap();
        let mut resp = test_env(1, 2);
        resp.class = Class::ControlExchange;
        resp.payload = Some(json!({"phase": "response", "kind_ns": "sot.take.request"}));
        resp.refs = vec![FrameRef {
            kind: RefKind::RespondsTo,
            frame: Seq { epoch: 1, n: 1 },
        }];
        w2.append(&resp, Commit::Immediate).unwrap();
        w2.seal(None).unwrap();
        verify_voyage(&dir.path().join("ce-ok"), "ce-ok").unwrap();

        // Violation: response phase carrying a forbidden `to`.
        let mut s3 = store(dir.path(), "ce-bad2");
        let mut w3 = s3.open_segment(0).unwrap();
        let mut req3 = test_env(1, 1);
        req3.class = Class::ControlExchange;
        req3.payload = Some(json!({
            "phase": "request", "kind_ns": "sot.take.request", "to": {"kind": "producer"}
        }));
        w3.append(&req3, Commit::Immediate).unwrap();
        let mut resp3 = test_env(1, 2);
        resp3.class = Class::ControlExchange;
        resp3.payload = Some(json!({
            "phase": "response", "kind_ns": "sot.take.request", "to": {"kind": "producer"}
        }));
        resp3.refs = vec![FrameRef {
            kind: RefKind::RespondsTo,
            frame: Seq { epoch: 1, n: 1 },
        }];
        w3.append(&resp3, Commit::Immediate).unwrap();
        w3.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("ce-bad2"), "ce-bad2").is_err());
    }

    #[test]
    fn cross_field_turn_close_uniqueness_and_target() {
        let dir = tempfile::tempdir().unwrap();

        // Green: one turn_open + one turn_close (no duplicate_of).
        let mut s = store(dir.path(), "tc-ok");
        let mut w = s.open_segment(0).unwrap();
        let mut open = test_env(1, 1);
        open.class = Class::TurnOpen;
        open.payload = Some(json!({"admitted_by": "user"}));
        w.append(&open, Commit::Immediate).unwrap();
        let mut close = test_env(1, 2);
        close.class = Class::TurnClose;
        close.payload = Some(json!({"reason": "producer_done"}));
        close.refs = vec![FrameRef {
            kind: RefKind::CausedBy,
            frame: Seq { epoch: 1, n: 1 },
        }];
        w.append(&close, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        verify_voyage(&dir.path().join("tc-ok"), "tc-ok").unwrap();

        // Violation: a second non-duplicate close for the same turn.
        let mut s2 = store(dir.path(), "tc-bad1");
        let mut w2 = s2.open_segment(0).unwrap();
        let mut open2 = test_env(1, 1);
        open2.class = Class::TurnOpen;
        open2.payload = Some(json!({"admitted_by": "user"}));
        w2.append(&open2, Commit::Immediate).unwrap();
        let mut close2a = test_env(1, 2);
        close2a.class = Class::TurnClose;
        close2a.payload = Some(json!({"reason": "producer_done"}));
        close2a.refs = vec![FrameRef {
            kind: RefKind::CausedBy,
            frame: Seq { epoch: 1, n: 1 },
        }];
        w2.append(&close2a, Commit::Immediate).unwrap();
        let mut close2b = test_env(1, 3);
        close2b.class = Class::TurnClose;
        close2b.payload = Some(json!({"reason": "failed"}));
        close2b.refs = vec![FrameRef {
            kind: RefKind::CausedBy,
            frame: Seq { epoch: 1, n: 1 },
        }];
        w2.append(&close2b, Commit::Immediate).unwrap();
        w2.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("tc-bad1"), "tc-bad1").is_err());

        // Green: a second close carrying duplicate_of -> the winning close.
        let mut s3 = store(dir.path(), "tc-ok2");
        let mut w3 = s3.open_segment(0).unwrap();
        let mut open3 = test_env(1, 1);
        open3.class = Class::TurnOpen;
        open3.payload = Some(json!({"admitted_by": "user"}));
        w3.append(&open3, Commit::Immediate).unwrap();
        let mut close3a = test_env(1, 2);
        close3a.class = Class::TurnClose;
        close3a.payload = Some(json!({"reason": "producer_done"}));
        close3a.refs = vec![FrameRef {
            kind: RefKind::CausedBy,
            frame: Seq { epoch: 1, n: 1 },
        }];
        w3.append(&close3a, Commit::Immediate).unwrap();
        let mut close3b = test_env(1, 3);
        close3b.class = Class::TurnClose;
        close3b.payload = Some(json!({"reason": "synthesized_death"}));
        close3b.refs = vec![
            FrameRef {
                kind: RefKind::CausedBy,
                frame: Seq { epoch: 1, n: 1 },
            },
            FrameRef {
                kind: RefKind::DuplicateOf,
                frame: Seq { epoch: 1, n: 2 },
            },
        ];
        w3.append(&close3b, Commit::Immediate).unwrap();
        w3.seal(None).unwrap();
        verify_voyage(&dir.path().join("tc-ok2"), "tc-ok2").unwrap();

        // Violation: turn_close's caused_by targets a non-turn_open frame.
        let mut s4 = store(dir.path(), "tc-bad2");
        let mut w4 = s4.open_segment(0).unwrap();
        let mut notopen = test_env(1, 1);
        notopen.class = Class::Lifecycle;
        notopen.payload = Some(json!({"kind": "producer_ready"}));
        w4.append(&notopen, Commit::Immediate).unwrap();
        let mut close4 = test_env(1, 2);
        close4.class = Class::TurnClose;
        close4.payload = Some(json!({"reason": "producer_done"}));
        close4.refs = vec![FrameRef {
            kind: RefKind::CausedBy,
            frame: Seq { epoch: 1, n: 1 },
        }];
        w4.append(&close4, Commit::Immediate).unwrap();
        w4.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("tc-bad2"), "tc-bad2").is_err());
    }

    // --- Rule 2: input_fact chain lattice ---

    #[test]
    fn input_fact_lattice_happy_paths() {
        let dir = tempfile::tempdir().unwrap();
        let key = "1".repeat(32);

        // Full chain: input -> forward_intent -> forwarded -> producer_observed.
        let mut s = store(dir.path(), "fact-ok-full");
        let mut w = s.open_segment(0).unwrap();
        w.append(&input_env(1, 1, &key), Commit::Immediate).unwrap();
        w.append(
            &fact_env(1, 2, Seq { epoch: 1, n: 1 }, "forward_intent", None),
            Commit::Immediate,
        )
        .unwrap();
        w.append(
            &fact_env(1, 3, Seq { epoch: 1, n: 1 }, "forwarded", Some(Seq { epoch: 1, n: 2 })),
            Commit::Immediate,
        )
        .unwrap();
        w.append(
            &fact_env(1, 4, Seq { epoch: 1, n: 1 }, "producer_observed", Some(Seq { epoch: 1, n: 2 })),
            Commit::Immediate,
        )
        .unwrap();
        w.seal(None).unwrap();
        verify_voyage(&dir.path().join("fact-ok-full"), "fact-ok-full").unwrap();

        // Refused branch: input -> refused_stale_epoch.
        let mut s2 = store(dir.path(), "fact-ok-refused");
        let mut w2 = s2.open_segment(0).unwrap();
        w2.append(&input_env(1, 1, &key), Commit::Immediate).unwrap();
        w2.append(
            &fact_env(1, 2, Seq { epoch: 1, n: 1 }, "refused_stale_epoch", None),
            Commit::Immediate,
        )
        .unwrap();
        w2.seal(None).unwrap();
        verify_voyage(&dir.path().join("fact-ok-refused"), "fact-ok-refused").unwrap();
    }

    #[test]
    fn input_fact_lattice_illegal_transition_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let key = "2".repeat(32);
        let mut s = store(dir.path(), "fact-bad-transition");
        let mut w = s.open_segment(0).unwrap();
        w.append(&input_env(1, 1, &key), Commit::Immediate).unwrap();
        // forwarded without a prior forward_intent: illegal.
        w.append(
            &fact_env(1, 2, Seq { epoch: 1, n: 1 }, "forwarded", Some(Seq { epoch: 1, n: 1 })),
            Commit::Immediate,
        )
        .unwrap();
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("fact-bad-transition"), "fact-bad-transition").is_err());
    }

    #[test]
    fn input_fact_idem_key_reuse_across_inputs_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let key = "3".repeat(32);
        let mut s = store(dir.path(), "fact-bad-reuse");
        let mut w = s.open_segment(0).unwrap();
        w.append(&input_env(1, 1, &key), Commit::Immediate).unwrap();
        w.append(&input_env(1, 2, &key), Commit::Immediate).unwrap(); // reused key
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("fact-bad-reuse"), "fact-bad-reuse").is_err());
    }

    #[test]
    fn input_fact_intent_must_name_this_inputs_forward_intent() {
        let dir = tempfile::tempdir().unwrap();
        let key_a = "4".repeat(32);
        let key_b = "5".repeat(32);
        let mut s = store(dir.path(), "fact-bad-intent");
        let mut w = s.open_segment(0).unwrap();
        w.append(&input_env(1, 1, &key_a), Commit::Immediate).unwrap(); // n=1
        w.append(&input_env(1, 2, &key_b), Commit::Immediate).unwrap(); // n=2
        w.append(
            &fact_env(1, 3, Seq { epoch: 1, n: 1 }, "forward_intent", None),
            Commit::Immediate,
        )
        .unwrap(); // n=3, key_a's intent
        w.append(
            &fact_env(1, 4, Seq { epoch: 1, n: 2 }, "forward_intent", None),
            Commit::Immediate,
        )
        .unwrap(); // n=4, key_b's intent
                   // forwarded for key_a but naming key_b's forward_intent (n=4): illegal.
        w.append(
            &fact_env(1, 5, Seq { epoch: 1, n: 1 }, "forwarded", Some(Seq { epoch: 1, n: 4 })),
            Commit::Immediate,
        )
        .unwrap();
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("fact-bad-intent"), "fact-bad-intent").is_err());
    }

    // --- Rule 3: stream prev-chains ---

    #[test]
    fn stream_prev_chain_linear_and_unique_head() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "stream-ok");
        let mut w = s.open_segment(0).unwrap();
        w.append(&attached_anchor(1, 1), Commit::Immediate).unwrap();
        let anchor = Seq { epoch: 1, n: 1 };
        w.append(&stream_frame(1, 2, anchor, "cellA", None), Commit::Immediate).unwrap();
        w.append(
            &stream_frame(1, 3, anchor, "cellA", Some(Seq { epoch: 1, n: 2 })),
            Commit::Immediate,
        )
        .unwrap();
        w.append(
            &stream_frame(1, 4, anchor, "cellA", Some(Seq { epoch: 1, n: 3 })),
            Commit::Immediate,
        )
        .unwrap();
        // A second, independent cell under the same attachment: its own head.
        w.append(&stream_frame(1, 5, anchor, "cellB", None), Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        verify_voyage(&dir.path().join("stream-ok"), "stream-ok").unwrap();
    }

    #[test]
    fn stream_prev_chain_violations() {
        let dir = tempfile::tempdir().unwrap();

        // Violation: stream frame without any attached_to.
        let mut s = store(dir.path(), "stream-bad-noattach");
        let mut w = s.open_segment(0).unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "producer_ready"}));
        e.stream = Some(Stream {
            cell: "c".into(),
            mode: StreamMode::Replace,
            complete: false,
            prev: None,
        });
        w.append(&e, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("stream-bad-noattach"), "stream-bad-noattach").is_err());

        // Violation: first frame of a cell carries a prev.
        let mut s2 = store(dir.path(), "stream-bad-firstprev");
        let mut w2 = s2.open_segment(0).unwrap();
        w2.append(&attached_anchor(1, 1), Commit::Immediate).unwrap();
        let anchor = Seq { epoch: 1, n: 1 };
        w2.append(
            &stream_frame(1, 2, anchor, "cellA", Some(Seq { epoch: 1, n: 1 })),
            Commit::Immediate,
        )
        .unwrap();
        w2.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("stream-bad-firstprev"), "stream-bad-firstprev").is_err());

        // Violation: second frame's prev doesn't point at the immediate predecessor.
        let mut s3 = store(dir.path(), "stream-bad-skip");
        let mut w3 = s3.open_segment(0).unwrap();
        w3.append(&attached_anchor(1, 1), Commit::Immediate).unwrap();
        let anchor3 = Seq { epoch: 1, n: 1 };
        w3.append(&stream_frame(1, 2, anchor3, "cellA", None), Commit::Immediate).unwrap();
        w3.append(&stream_frame(1, 3, anchor3, "cellA", Some(anchor3)), Commit::Immediate)
            .unwrap(); // points at the anchor, not n=2
        w3.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("stream-bad-skip"), "stream-bad-skip").is_err());
    }

    // --- Rule 4: take_epoch ordering ---

    #[test]
    fn take_epoch_first_in_writer_epoch_requires_null_holder() {
        let dir = tempfile::tempdir().unwrap();

        // Violation: first take_state in epoch 1 has a non-null holder.
        let mut s = store(dir.path(), "take-bad-holder");
        let mut w = s.open_segment(0).unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "take_state", "take": {"take_epoch": 1, "holder": "someone"}}));
        w.append(&e, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("take-bad-holder"), "take-bad-holder").is_err());

        // Green: first take_state has holder=null.
        let mut s2 = store(dir.path(), "take-ok-holder");
        let mut w2 = s2.open_segment(0).unwrap();
        let mut e2 = test_env(1, 1);
        e2.class = Class::Lifecycle;
        e2.payload = Some(json!({"kind": "take_state", "take": {"take_epoch": 1, "holder": null}}));
        w2.append(&e2, Commit::Immediate).unwrap();
        w2.seal(None).unwrap();
        verify_voyage(&dir.path().join("take-ok-holder"), "take-ok-holder").unwrap();
    }

    #[test]
    fn take_epoch_must_strictly_increase() {
        let dir = tempfile::tempdir().unwrap();

        // Violation: non-increasing (equal) take_epoch.
        let mut s = store(dir.path(), "take-bad-order");
        let mut w = s.open_segment(0).unwrap();
        let mut e1 = test_env(1, 1);
        e1.class = Class::Lifecycle;
        e1.payload = Some(json!({"kind": "take_state", "take": {"take_epoch": 2, "holder": null}}));
        w.append(&e1, Commit::Immediate).unwrap();
        let mut e2 = test_env(1, 2);
        e2.class = Class::Lifecycle;
        e2.payload = Some(json!({"kind": "take_state", "take": {"take_epoch": 2, "holder": "c1"}}));
        w.append(&e2, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("take-bad-order"), "take-bad-order").is_err());

        // Green: strictly increasing across two take_state frames.
        let mut s2 = store(dir.path(), "take-ok-order");
        let mut w2 = s2.open_segment(0).unwrap();
        let mut f1 = test_env(1, 1);
        f1.class = Class::Lifecycle;
        f1.payload = Some(json!({"kind": "take_state", "take": {"take_epoch": 2, "holder": null}}));
        w2.append(&f1, Commit::Immediate).unwrap();
        let mut f2 = test_env(1, 2);
        f2.class = Class::Lifecycle;
        f2.payload = Some(json!({"kind": "take_state", "take": {"take_epoch": 3, "holder": "c1"}}));
        w2.append(&f2, Commit::Immediate).unwrap();
        w2.seal(None).unwrap();
        verify_voyage(&dir.path().join("take-ok-order"), "take-ok-order").unwrap();
    }

    #[test]
    fn controller_frame_take_epoch_must_match_committed() {
        let dir = tempfile::tempdir().unwrap();

        // Violation: controller frame's take_epoch doesn't match the
        // committed one (still 0, none granted yet).
        let mut s = store(dir.path(), "ctrl-bad-epoch");
        let mut w = s.open_segment(0).unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "producer_ready"}));
        e.source.actor.kind = ActorKind::Controller;
        e.source.actor.controller_id = Some("c1".into());
        e.source.actor.take_epoch = Some(1);
        w.append(&e, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("ctrl-bad-epoch"), "ctrl-bad-epoch").is_err());

        // Green: after a take_state grants epoch 1, a controller frame at
        // take_epoch=1 is legal.
        let mut s2 = store(dir.path(), "ctrl-ok-epoch");
        let mut w2 = s2.open_segment(0).unwrap();
        let mut take = test_env(1, 1);
        take.class = Class::Lifecycle;
        take.payload = Some(json!({"kind": "take_state", "take": {"take_epoch": 1, "holder": null}}));
        w2.append(&take, Commit::Immediate).unwrap();
        let mut ctrl = test_env(1, 2);
        ctrl.class = Class::Lifecycle;
        ctrl.payload = Some(json!({"kind": "producer_ready"}));
        ctrl.source.actor.kind = ActorKind::Controller;
        ctrl.source.actor.controller_id = Some("c1".into());
        ctrl.source.actor.take_epoch = Some(1);
        w2.append(&ctrl, Commit::Immediate).unwrap();
        w2.seal(None).unwrap();
        verify_voyage(&dir.path().join("ctrl-ok-epoch"), "ctrl-ok-epoch").unwrap();
    }

    // --- Rule 5: blob presence + length ---

    #[test]
    fn artifact_ref_blob_presence_and_length() {
        let dir = tempfile::tempdir().unwrap();

        // Violation: wrong recorded length.
        let mut s = store(dir.path(), "blob-artifact-badlen");
        let digest = s.publish_blob(b"hello world").unwrap();
        let mut w = s.open_segment(0).unwrap();
        let mut bad = test_env(1, 1);
        bad.class = Class::ArtifactRef;
        bad.payload = Some(json!({
            "blob": {"algo": "sha256", "digest": digest, "length": 999, "media_type": "text/plain"}
        }));
        w.append(&bad, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("blob-artifact-badlen"), "blob-artifact-badlen").is_err());

        // Green: correct length.
        let mut s2 = store(dir.path(), "blob-artifact-ok");
        let digest2 = s2.publish_blob(b"hello world").unwrap();
        let mut w2 = s2.open_segment(0).unwrap();
        let mut good = test_env(1, 1);
        good.class = Class::ArtifactRef;
        good.payload = Some(json!({
            "blob": {"algo": "sha256", "digest": digest2, "length": 11, "media_type": "text/plain"}
        }));
        w2.append(&good, Commit::Immediate).unwrap();
        w2.seal(None).unwrap();
        verify_voyage(&dir.path().join("blob-artifact-ok"), "blob-artifact-ok").unwrap();

        // Violation: digest that was never published (missing on disk).
        let mut s3 = store(dir.path(), "blob-artifact-missing");
        let mut w3 = s3.open_segment(0).unwrap();
        let mut missing = test_env(1, 1);
        missing.class = Class::ArtifactRef;
        missing.payload = Some(json!({
            "blob": {"algo": "sha256", "digest": "ab".repeat(32), "length": 1, "media_type": "text/plain"}
        }));
        w3.append(&missing, Commit::Immediate).unwrap();
        w3.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("blob-artifact-missing"), "blob-artifact-missing").is_err());
    }

    #[test]
    fn payload_ref_blob_length_checked() {
        const BYTES: &[u8] = b"oversized payload bytes";
        let dir = tempfile::tempdir().unwrap();

        // Violation: wrong recorded length on a payload_ref.
        let mut s = store(dir.path(), "blob-payloadref-bad");
        let digest = s.publish_blob(BYTES).unwrap();
        let mut w = s.open_segment(0).unwrap();
        w.append(&attached_anchor(1, 1), Commit::Immediate).unwrap();
        let mut bad = test_env(1, 2);
        bad.refs = vec![FrameRef {
            kind: RefKind::AttachedTo,
            frame: Seq { epoch: 1, n: 1 },
        }];
        bad.payload = None;
        bad.payload_ref = Some(PayloadRef {
            blob: BlobRef {
                algo: "sha256".into(),
                digest,
                length: BYTES.len() as u64 + 1,
                media_type: "application/octet-stream".into(),
            },
            encoding: PayloadEncoding::Bytes,
        });
        w.append(&bad, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("blob-payloadref-bad"), "blob-payloadref-bad").is_err());

        // Green: correct length.
        let mut s2 = store(dir.path(), "blob-payloadref-ok");
        let digest2 = s2.publish_blob(BYTES).unwrap();
        let mut w2 = s2.open_segment(0).unwrap();
        w2.append(&attached_anchor(1, 1), Commit::Immediate).unwrap();
        let mut good = test_env(1, 2);
        good.refs = vec![FrameRef {
            kind: RefKind::AttachedTo,
            frame: Seq { epoch: 1, n: 1 },
        }];
        good.payload = None;
        good.payload_ref = Some(PayloadRef {
            blob: BlobRef {
                algo: "sha256".into(),
                digest: digest2,
                length: BYTES.len() as u64,
                media_type: "application/octet-stream".into(),
            },
            encoding: PayloadEncoding::Bytes,
        });
        w2.append(&good, Commit::Immediate).unwrap();
        w2.seal(None).unwrap();
        verify_voyage(&dir.path().join("blob-payloadref-ok"), "blob-payloadref-ok").unwrap();
    }
}
