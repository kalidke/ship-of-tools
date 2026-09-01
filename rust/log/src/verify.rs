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
    validate_blob_ref, validate_str128, ActorKind, BlobRef, Class, ExchangePhase, InputContent,
    InputFactKind, LifecycleKind, RefKind, Seq,
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

/// The three registered required features (ADR 0039 registry, 2026-08-24;
/// third entry 2026-08-30, ADR 0041 step 6 U1b). `json-f64-v1` is enforced
/// bidirectionally (undeclared + fractional = loud, inline or spilled).
/// `cgroup-fence-v1` is likewise bidirectional since the wiring PR fixed
/// the spawn-detail schema: a `producer_spawn` whose `kill_domain` bears
/// authority (scheme "cgroup") must sit in a segment declaring the
/// feature; scheme "none" and an absent `kill_domain` claim no authority;
/// unknown schemes fail closed. `run-end-requested-v1` is bidirectional
/// the same way: a `run_end_requested` frame requires its segment to
/// declare it (below), and a reader built before this constant grew a
/// third entry refuses ANY segment that declares an unknown feature name
/// (the loop just below this one) before it would ever decode the frame
/// — the ADR 0041 "reader lands one release before the writer" property,
/// for free, from the SAME mechanism the two existing entries already
/// use.
pub const REGISTERED_FEATURES: [&str; 3] = [
    "sot.producer.json-f64-v1",
    "sot.capsule.cgroup-fence-v1",
    "sot.capsule.run-end-requested-v1",
];

/// Turn-closure predicate (ADR 0039 §Verifier, ADR 0040).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    /// Zero unmatched turn_opens. The default and the only CERTIFYING mode.
    Complete,
    /// Non-certifying diagnostic: tolerates at most one unmatched open,
    /// and only in the currently open tip's epoch. A verifier cannot prove
    /// a writer is live; only the owning capsule may treat this as health.
    AllowOpenTip,
}

/// Certifying verification (Complete mode).
pub fn verify_voyage(root: &Path, voyage_id: &str) -> Result<()> {
    verify_voyage_mode(root, voyage_id, VerifyMode::Complete)
}

pub fn verify_voyage_mode(root: &Path, voyage_id: &str, mode: VerifyMode) -> Result<()> {
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
                        LifecycleKind::RunEndRequested => {
                            if has_take || has_fact {
                                return Err(Error::Schema(format!(
                                    "lifecycle {:?}: run_end_requested forbids take and fact",
                                    env.seq
                                )));
                            }
                            let reason = payload
                                .get("reason")
                                .and_then(|r| r.as_str())
                                .ok_or_else(|| {
                                    Error::Schema(format!(
                                        "lifecycle {:?}: run_end_requested needs a string reason",
                                        env.seq
                                    ))
                                })?;
                            validate_str128(reason, "run_end_requested.reason")
                                .map_err(|e| Error::Schema(format!("lifecycle {:?}: {e}", env.seq)))?;
                            // Major 7: at most one marker per writer
                            // epoch -- a second well-formed marker in the
                            // SAME epoch is loud, matching ADR 0039's
                            // amended cross-field matrix and the
                            // first-commit-wins rule it documents.
                            if !run_end_seen_epochs.insert(env.seq.epoch) {
                                return Err(Error::Schema(format!(
                                    "lifecycle {:?}: a second run_end_requested in writer epoch {} \
                                     (first-commit-wins forbids two)",
                                    env.seq, env.seq.epoch
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
                    if kind == LifecycleKind::ProducerSpawn {
                        // Locator-must-declare (ADR 0039 registry): an
                        // authority-bearing kill-domain locator is only
                        // interpretable under `cgroup-fence-v1` — successor
                        // epochs act on it destructively, so an undeclared
                        // one fails closed. Absent kill_domain (the P1 PTY
                        // capsule) and scheme "none" (an explicitly unfenced
                        // rig) claim no authority and need no feature.
                        if let Some(kd) = payload.get("detail").and_then(|d| d.get("kill_domain")) {
                            match kd.get("scheme").and_then(|s| s.as_str()) {
                                Some("none") => {}
                                Some("cgroup") => {
                                    let path_ok = kd
                                        .get("path")
                                        .and_then(|p| p.as_str())
                                        .is_some_and(|p| !p.is_empty());
                                    if !path_ok {
                                        return Err(Error::Schema(format!(
                                            "lifecycle {:?}: cgroup kill_domain needs a non-empty path",
                                            env.seq
                                        )));
                                    }
                                    if !fence_ok {
                                        // Schema, not State: an invalid
                                        // cross-field encoding (like
                                        // undeclared-f64), not an unknown
                                        // feature the reader can't implement.
                                        return Err(Error::Schema(format!(
                                            "lifecycle {:?}: locator-bearing producer_spawn in a segment that does not declare sot.capsule.cgroup-fence-v1",
                                            env.seq
                                        )));
                                    }
                                }
                                other => {
                                    return Err(Error::Schema(format!(
                                        "lifecycle {:?}: unknown kill_domain scheme {other:?} fails closed",
                                        env.seq
                                    )));
                                }
                            }
                        }
                    }
                    if kind == LifecycleKind::TakeState {
                        let take: TakeObj = serde_json::from_value(
                            payload.get("take").expect("checked above").clone(),
                        )
                        .map_err(|e| {
                            Error::Schema(format!("lifecycle {:?}: take malformed: {e}", env.seq))
                        })?;
                        if let Some(holder) = &take.holder {
                            validate_str128(holder, "take.holder")
                                .map_err(|e| Error::Schema(format!("lifecycle {:?}: {e}", env.seq)))?;
                        }
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

/// The READ half of ADR 0041's "Respawn is gated by the typed marker,
/// read from the LATEST LEG AFTER RECONCILIATION" — a small typed
/// accessor for "does this leg's own epoch carry the `run_end_requested`
/// marker", exposed so a later unit's respawn decision has something to
/// call rather than groping the JSON itself. The DECISION (which leg is
/// "latest", what to do about it) is that later unit's job; this
/// function only answers the one question about ONE already-selected
/// epoch.
///
/// Scans every segment file under `seg_dir` whose header names `epoch`
/// exactly, in `.open` or `.sotseg` state — a hard-killed leg's tail
/// segment can remain `.open` and the marker still governs it, per the
/// ADR ("a marker governs only its OWN epoch"). `.recovering`/
/// `.recovering-out` are deliberately NOT read here: those are
/// mid-transaction scratch states reconciliation resolves before this
/// leg's epoch is stable enough to answer "latest" about, matching the
/// ADR's own "after reconciliation" qualifier. Not a certifying pass —
/// no chain, no full cross-field walk; but (Codex round-1 Major 8) it is
/// NOT a bare string grope either: filename identity is cross-checked
/// against the header's OWN claim (the same rule `verify_voyage_mode`
/// enforces), `kind` is decoded through the TYPED closed enum (an
/// unrecognized value is not silently "not a marker" the way a raw
/// string compare would treat it — it is simply not this kind, exactly
/// as the typed decode says), and a CANDIDATE marker frame is verified
/// feature-declared with a well-formed, bounded `reason` before it
/// counts. Every one of those failure shapes — mismatched identity, an
/// authority-changing frame in an undeclaring segment, a malformed
/// reason, or two markers in one epoch — errs LOUD rather than
/// returning `false`: a filename naming epoch E whose header disagrees,
/// or a marker that fails its own shape, must never be silently treated
/// as "no marker", which is exactly the failure mode that could suppress
/// U2's respawn on a genuinely broken record instead of stopping for an
/// operator. A torn or corrupt segment (of any other kind) simply errs
/// too, since only the caller's own already-reconciled leg is ever
/// handed to this function.
pub fn leg_carries_run_end_marker(seg_dir: &Path, voyage_id: &str, epoch: u64) -> Result<bool> {
    let mut found: Option<Seq> = None;
    for entry in std::fs::read_dir(seg_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == ".tmp" {
            continue;
        }
        let Some((idx, seg_epoch, state)) = SegmentIdentity::parse_file_name(name) else {
            continue;
        };
        if seg_epoch != epoch || !matches!(state, SegmentState::Open | SegmentState::Sealed) {
            continue;
        }
        let id = SegmentIdentity {
            voyage_id: voyage_id.to_string(),
            segment_index: idx,
            epoch: seg_epoch,
        };
        let sealed = state == SegmentState::Sealed;
        let reader = SegmentReader::read(&id.path(seg_dir, state), sealed)?;
        if reader.header.segment_index != idx
            || reader.header.epoch != seg_epoch
            || reader.header.voyage_id != voyage_id
        {
            return Err(Error::State(format!(
                "segment {name}: filename and header identity disagree"
            )));
        }
        let feature_ok = reader
            .header
            .required_features
            .iter()
            .any(|f| f == "sot.capsule.run-end-requested-v1");
        for env in &reader.frames {
            if env.class != Class::Lifecycle {
                continue;
            }
            // Codex round-2b Blocker 3 ("the accessor still silently
            // accepts invalid paths"): a missing/unknown lifecycle kind
            // is a HARD SCHEMA ERROR here, exactly as the full verifier
            // treats it -- never a `continue` that lets a corrupt frame
            // slide past as merely "not this kind". `payload` itself is
            // like`Envelope::validate()`'s own payload/payload_ref XOR
            // (already enforced by `SegmentReader::read`, which runs
            // `env.validate()` on every frame) already guarantees a
            // Lifecycle-class frame carries an inline `payload` --
            // checked again here defensively, still loud if it somehow
            // didn't.
            let payload = env.payload.as_ref().ok_or_else(|| {
                Error::State(format!("lifecycle {:?}: missing payload", env.seq))
            })?;
            let kind: LifecycleKind = payload
                .get("kind")
                .and_then(|k| serde_json::from_value::<LifecycleKind>(k.clone()).ok())
                .ok_or_else(|| {
                    Error::State(format!("lifecycle {:?}: invalid/missing kind", env.seq))
                })?;
            if kind != LifecycleKind::RunEndRequested {
                continue;
            }
            // A marker frame carrying the take/fact fields the cross-
            // field matrix forbids for run_end_requested is corrupt in
            // exactly the way the full verifier refuses -- must not be
            // silently counted as a valid marker.
            if payload.get("take").is_some() || payload.get("fact").is_some() {
                return Err(Error::State(format!(
                    "lifecycle {:?}: run_end_requested forbids take and fact",
                    env.seq
                )));
            }
            if !feature_ok {
                return Err(Error::State(format!(
                    "lifecycle {:?}: run_end_requested in a segment that does not declare \
                     sot.capsule.run-end-requested-v1",
                    env.seq
                )));
            }
            let reason = payload.get("reason").and_then(|r| r.as_str()).ok_or_else(|| {
                Error::State(format!(
                    "lifecycle {:?}: run_end_requested missing a string reason",
                    env.seq
                ))
            })?;
            validate_str128(reason, "run_end_requested.reason")
                .map_err(|e| Error::State(format!("lifecycle {:?}: {e}", env.seq)))?;
            if let Some(prior) = found {
                return Err(Error::State(format!(
                    "epoch {epoch} carries two run_end_requested markers ({prior:?} and {:?})",
                    env.seq
                )));
            }
            found = Some(env.seq);
        }
    }
    Ok(found.is_some())
}

/// The READ half of Codex review round 3, N1: "stability must be judged
/// on the PRODUCER's lifetime, never on the capsule process's exit."
/// Scans `seg_dir` for `epoch`'s own `producer_dead` lifecycle frame and
/// returns its `detail.producer_uptime_ms`, an ADDITIVE free-form
/// diagnostic field (like `detail.reason` already is — no registered
/// feature required, no authority changes). Fail-safe direction is
/// `Ok(None)` for every case that must NOT be trusted as a proven
/// stable duration: no `producer_dead` frame found on this epoch at all
/// (a still-open/unsealed leg, or a spawn-failed leg that never reached
/// a real producer), the key absent from an otherwise well-formed
/// frame, or the value present but not a plain non-negative integer.
/// `None` here is the caller's own cue to count the leg UNSTABLE (N1's
/// own ruling) — never to fall back to a wall-clock measurement a slow
/// teardown could inflate arbitrarily, which is the exact bug this
/// exists to close.
///
/// A STRUCTURALLY corrupt segment (mismatched filename/header identity,
/// a malformed lifecycle envelope) still errs loud here, exactly as
/// [`leg_carries_run_end_marker`] does — this accessor is lenient only
/// about the specific diagnostic VALUE it is looking for, never about
/// the store's own integrity; only an already-reconciled leg is ever
/// handed to it.
pub fn leg_producer_uptime_ms(seg_dir: &Path, voyage_id: &str, epoch: u64) -> Result<Option<u64>> {
    for entry in std::fs::read_dir(seg_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == ".tmp" {
            continue;
        }
        let Some((idx, seg_epoch, state)) = SegmentIdentity::parse_file_name(name) else {
            continue;
        };
        if seg_epoch != epoch || !matches!(state, SegmentState::Open | SegmentState::Sealed) {
            continue;
        }
        let id = SegmentIdentity {
            voyage_id: voyage_id.to_string(),
            segment_index: idx,
            epoch: seg_epoch,
        };
        let sealed = state == SegmentState::Sealed;
        let reader = SegmentReader::read(&id.path(seg_dir, state), sealed)?;
        if reader.header.segment_index != idx
            || reader.header.epoch != seg_epoch
            || reader.header.voyage_id != voyage_id
        {
            return Err(Error::State(format!(
                "segment {name}: filename and header identity disagree"
            )));
        }
        for env in &reader.frames {
            if env.class != Class::Lifecycle {
                continue;
            }
            let payload = env.payload.as_ref().ok_or_else(|| {
                Error::State(format!("lifecycle {:?}: missing payload", env.seq))
            })?;
            let kind: LifecycleKind = payload
                .get("kind")
                .and_then(|k| serde_json::from_value::<LifecycleKind>(k.clone()).ok())
                .ok_or_else(|| {
                    Error::State(format!("lifecycle {:?}: invalid/missing kind", env.seq))
                })?;
            if kind != LifecycleKind::ProducerDead {
                continue;
            }
            return Ok(payload
                .get("detail")
                .and_then(|d| d.get("producer_uptime_ms"))
                .and_then(serde_json::Value::as_u64));
        }
    }
    Ok(None)
}

/// Producer payloads without the f64 feature: every number must be an
/// integer with |v| <= 2^53-1 (the §3 atoms), recursively.
fn check_integer_numbers(v: &serde_json::Value) -> std::result::Result<(), String> {
    match v {
        serde_json::Value::Number(n) => {
            let ok = n.as_i64().map(|i| i.unsigned_abs() <= crate::envelope::U53_MAX)
                .or_else(|| n.as_u64().map(|u| u <= crate::envelope::U53_MAX))
                .unwrap_or(false);
            if ok { Ok(()) } else { Err(format!("non-integer or out-of-range number {n}")) }
        }
        serde_json::Value::Array(a) => a.iter().try_for_each(check_integer_numbers),
        serde_json::Value::Object(m) => m.values().try_for_each(check_integer_numbers),
        _ => Ok(()),
    }
}

/// Read a blob's bytes from the CAS (existence/length already verified by
/// `check_blob_on_disk` in the same pass; this re-reads for content checks).
fn read_blob(root: &Path, blob: &BlobRef, seq: Seq) -> Result<Vec<u8>> {
    let path = root
        .join("blobs")
        .join(&blob.algo)
        .join(&blob.digest[0..2])
        .join(&blob.digest);
    std::fs::read(&path).map_err(|e| Error::Schema(format!(
        "frame {seq:?}: payload_ref blob unreadable: {e}"
    )))
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

#[cfg(all(test, any(target_os = "linux", windows)))]
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

    /// Locator-must-declare (ADR 0039 registry): scheme "cgroup" requires
    /// the segment to declare cgroup-fence-v1; "none"/absent claim no
    /// authority; unknown schemes and empty paths fail closed.
    #[test]
    fn locator_must_declare_cgroup_fence() {
        let run = |name: &str, features: Vec<String>, detail: serde_json::Value| {
            let dir = tempfile::tempdir().unwrap();
            let mut s = store(dir.path(), name);
            let mut w = s.open_segment_with_features(0, features).unwrap();
            let mut e = test_env(1, 1);
            e.class = Class::Lifecycle;
            e.payload = Some(json!({"kind": "producer_spawn", "detail": detail}));
            w.append(&e, Commit::Immediate).unwrap();
            w.seal(None).unwrap();
            verify_voyage(&dir.path().join(name), name).map(|_| ())
        };
        let fence = || vec!["sot.capsule.cgroup-fence-v1".to_string()];
        let err = run(
            "l1",
            vec![],
            json!({"kill_domain": {"scheme": "cgroup", "path": "/sys/fs/cgroup/x"}}),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("does not declare"), "got: {err}");
        run(
            "l2",
            fence(),
            json!({"kill_domain": {"scheme": "cgroup", "path": "/sys/fs/cgroup/x"}}),
        )
        .unwrap();
        // No authority claimed — explicitly ("none") or by absence (the P1
        // PTY capsule's spawn detail): no feature needed.
        run("l3", vec![], json!({"kill_domain": {"scheme": "none"}})).unwrap();
        run("l4", vec![], json!({"argv": ["sh"]})).unwrap();
        // Unknown scheme and empty path fail closed even when declared.
        assert!(run("l5", fence(), json!({"kill_domain": {"scheme": "jail"}})).is_err());
        assert!(run("l6", fence(), json!({"kill_domain": {"scheme": "cgroup", "path": ""}})).is_err());
    }

    /// ADR 0041 step 6 U1b: `run_end_requested`'s registered feature,
    /// enforced bidirectionally like `cgroup-fence-v1` above — refused in
    /// a segment that doesn't declare it; a declared, present, string
    /// `reason` (empty legal) verifies green; a missing/non-string reason
    /// or a take/fact alongside it fails closed even when declared.
    #[test]
    fn run_end_requested_needs_its_declared_feature() {
        let run = |name: &str, features: Vec<String>, payload: serde_json::Value| {
            let dir = tempfile::tempdir().unwrap();
            let mut s = store(dir.path(), name);
            let mut w = s.open_segment_with_features(0, features).unwrap();
            let mut e = test_env(1, 1);
            e.class = Class::Lifecycle;
            e.payload = Some(payload);
            w.append(&e, Commit::Immediate).unwrap();
            w.seal(None).unwrap();
            verify_voyage(&dir.path().join(name), name).map(|_| ())
        };
        let feat = || vec!["sot.capsule.run-end-requested-v1".to_string()];
        let err = run("re1", vec![], json!({"kind": "run_end_requested", "reason": "quit"}))
            .unwrap_err();
        assert!(format!("{err}").contains("does not declare"), "got: {err}");
        run("re2", feat(), json!({"kind": "run_end_requested", "reason": "quit"})).unwrap();
        // The wire's shutdown.reason permits empty (require_nonempty=false
        // in wire.rs) — the marker carries it verbatim.
        run("re3", feat(), json!({"kind": "run_end_requested", "reason": ""})).unwrap();
        assert!(run("re4", feat(), json!({"kind": "run_end_requested"})).is_err());
        assert!(run("re5", feat(), json!({"kind": "run_end_requested", "reason": 1})).is_err());
        assert!(run(
            "re6",
            feat(),
            json!({"kind": "run_end_requested", "reason": "quit",
                   "take": {"take_epoch": 1, "holder": null}})
        )
        .is_err());
        // Codex round-1 Minor 10: reason obeys str128 (128 UTF-8 bytes)
        // like every other ADR 0039 str128 site -- exactly at the bound
        // verifies green, one byte over fails closed.
        run("re7", feat(), json!({"kind": "run_end_requested", "reason": "a".repeat(128)})).unwrap();
        assert!(run("re8", feat(), json!({"kind": "run_end_requested", "reason": "a".repeat(129)})).is_err());
    }

    /// Codex round-1 Major 7: at most one `run_end_requested` per writer
    /// epoch -- a second well-formed marker in the SAME epoch, in a
    /// segment that declares the feature, must fail verification even
    /// though each frame is individually well-formed.
    #[test]
    fn verifier_refuses_two_run_end_markers_in_one_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "dup1");
        let mut w = s
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        let mut e1 = test_env(1, 1);
        e1.class = Class::Lifecycle;
        e1.payload = Some(json!({"kind": "run_end_requested", "reason": "first"}));
        w.append(&e1, Commit::Immediate).unwrap();
        let mut e2 = test_env(1, 2);
        e2.class = Class::Lifecycle;
        e2.payload = Some(json!({"kind": "run_end_requested", "reason": "second"}));
        w.append(&e2, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        let err = verify_voyage(&dir.path().join("dup1"), "dup1").unwrap_err();
        assert!(format!("{err}").contains("second run_end_requested"), "got: {err}");
    }

    /// The bidirectional half `run_end_requested_needs_its_declared_feature`
    /// doesn't reach: a segment declaring a feature name this build's
    /// `REGISTERED_FEATURES` doesn't know is refused WHOLESALE, before any
    /// frame inside it is even decoded — the exact mechanism a reader
    /// shipped before an entry existed relies on to refuse a writer's
    /// segment it cannot safely interpret (ADR 0041 "reader lands one
    /// release before the writer"). A fictitious name stands in for "not
    /// yet in this build" since `REGISTERED_FEATURES` is a compile-time
    /// const.
    #[test]
    fn unknown_feature_name_refuses_the_whole_segment() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "unk1");
        let mut w = s
            .open_segment_with_features(0, vec!["sot.capsule.not-yet-registered-v1".to_string()])
            .unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "producer_ready"}));
        w.append(&e, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        let err = verify_voyage(&dir.path().join("unk1"), "unk1").unwrap_err();
        assert!(format!("{err}").contains("unknown feature"), "got: {err}");
    }

    /// `leg_carries_run_end_marker` — the small typed accessor a later
    /// unit's respawn decision reads. Must see the marker on a leg's OWN
    /// epoch whether the segment is still `.open` (a hard-killed leg's
    /// tail) or `.sotseg`, and must not see it on a different epoch or an
    /// ordinary lifecycle frame.
    #[test]
    fn leg_carries_run_end_marker_reads_open_and_sealed_legs() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk1");
        let mut w = s
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "run_end_requested", "reason": "quit"}));
        w.append(&e, Commit::Immediate).unwrap();
        let seg_dir = dir.path().join("mk1").join("seg");
        // Still .open (never sealed yet): the marker must already be
        // readable — a hard-killed leg's tail segment stays .open.
        assert!(leg_carries_run_end_marker(&seg_dir, "mk1", 1).unwrap());
        assert!(!leg_carries_run_end_marker(&seg_dir, "mk1", 2).unwrap());
        w.seal(None).unwrap();
        assert!(leg_carries_run_end_marker(&seg_dir, "mk1", 1).unwrap());
    }

    /// Codex round-1 Major 8: a marker frame present in a segment that
    /// does NOT declare the feature must fail LOUD (`Err`), never
    /// silently `false` -- an authority-changing frame smuggled past its
    /// own registry rule is corrupt, not "no marker".
    #[test]
    fn leg_carries_run_end_marker_errs_on_undeclared_feature() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk3");
        let mut w = s.open_segment(0).unwrap(); // no features declared
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "run_end_requested", "reason": "quit"}));
        w.append(&e, Commit::Immediate).unwrap();
        let seg_dir = dir.path().join("mk3").join("seg");
        let err = leg_carries_run_end_marker(&seg_dir, "mk3", 1).unwrap_err();
        assert!(format!("{err}").contains("does not declare"), "got: {err}");
    }

    /// Codex round-1 Major 8: a malformed reason (missing, or over the
    /// str128 bound) on an otherwise feature-declared marker also errs
    /// loud rather than silently reporting "no marker".
    #[test]
    fn leg_carries_run_end_marker_errs_on_malformed_reason() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk4");
        let mut w = s
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "run_end_requested", "reason": "a".repeat(200)}));
        w.append(&e, Commit::Immediate).unwrap();
        let seg_dir = dir.path().join("mk4").join("seg");
        let err = leg_carries_run_end_marker(&seg_dir, "mk4", 1).unwrap_err();
        assert!(format!("{err}").contains("str128"), "got: {err}");
    }

    /// Codex round-1 Major 8: two well-formed markers in the SAME epoch
    /// (the writer's own first-commit-wins latch is a promise about ITS
    /// process lifetime, not a proof a crafted/corrupted voyage can't
    /// carry two) must err loud, matching the main verifier's own
    /// per-epoch uniqueness rule (Major 7) rather than reporting the
    /// FIRST one found and silently ignoring the second.
    #[test]
    fn leg_carries_run_end_marker_errs_on_duplicate_marker() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk5");
        let mut w = s
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        let mut e1 = test_env(1, 1);
        e1.class = Class::Lifecycle;
        e1.payload = Some(json!({"kind": "run_end_requested", "reason": "first"}));
        w.append(&e1, Commit::Immediate).unwrap();
        let mut e2 = test_env(1, 2);
        e2.class = Class::Lifecycle;
        e2.payload = Some(json!({"kind": "run_end_requested", "reason": "second"}));
        w.append(&e2, Commit::Immediate).unwrap();
        let seg_dir = dir.path().join("mk5").join("seg");
        let err = leg_carries_run_end_marker(&seg_dir, "mk5", 1).unwrap_err();
        assert!(format!("{err}").contains("carries two"), "got: {err}");
    }

    /// Codex round-1 Major 8: the FILENAME'S epoch is not trusted on its
    /// own — a segment renamed to claim a DIFFERENT epoch than its own
    /// header still carries must err loud rather than silently answering
    /// against the wrong epoch's identity.
    #[test]
    fn leg_carries_run_end_marker_errs_on_filename_header_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk6");
        let mut w = s
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "run_end_requested", "reason": "quit"}));
        w.append(&e, Commit::Immediate).unwrap();
        drop(w);
        let seg_dir = dir.path().join("mk6").join("seg");
        // The real (unsealed) file is named for epoch 1; rename it to
        // CLAIM epoch 2 on disk while its header still says 1.
        let real_name = std::fs::read_dir(&seg_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name();
        let renamed = SegmentIdentity {
            voyage_id: "mk6".to_string(),
            segment_index: 0,
            epoch: 2,
        }
        .path(&seg_dir, SegmentState::Open);
        std::fs::rename(seg_dir.join(&real_name), &renamed).unwrap();
        let err = leg_carries_run_end_marker(&seg_dir, "mk6", 2).unwrap_err();
        assert!(format!("{err}").contains("identity disagree"), "got: {err}");
    }

    /// Codex round-2b Blocker 3: a missing/unknown lifecycle `kind` on
    /// ANY lifecycle frame in the scanned epoch must err loud -- exactly
    /// as the full verifier treats it -- never silently `continue` as
    /// "not this kind" the way a bare-string-compare accessor would.
    #[test]
    fn leg_carries_run_end_marker_errs_on_invalid_lifecycle_kind() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk7");
        let mut w = s
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "not_a_real_lifecycle_kind"}));
        w.append(&e, Commit::Immediate).unwrap();
        let seg_dir = dir.path().join("mk7").join("seg");
        let err = leg_carries_run_end_marker(&seg_dir, "mk7", 1).unwrap_err();
        assert!(format!("{err}").contains("invalid/missing kind"), "got: {err}");
    }

    /// Codex round-2b Blocker 3: a `run_end_requested` frame that ALSO
    /// carries `take` or `fact` (forbidden by the cross-field matrix)
    /// must err loud rather than being counted as a valid marker.
    #[test]
    fn leg_carries_run_end_marker_errs_on_marker_with_forbidden_take() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk8");
        let mut w = s
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({
            "kind": "run_end_requested",
            "reason": "quit",
            "take": {"take_epoch": 1, "holder": null}
        }));
        w.append(&e, Commit::Immediate).unwrap();
        let seg_dir = dir.path().join("mk8").join("seg");
        let err = leg_carries_run_end_marker(&seg_dir, "mk8", 1).unwrap_err();
        assert!(format!("{err}").contains("forbids take and fact"), "got: {err}");
    }

    #[test]
    fn leg_without_the_marker_reads_false() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk2");
        let mut w = s.open_segment(0).unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "producer_dead", "detail": {"exit_code": 0}}));
        w.append(&e, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        let seg_dir = dir.path().join("mk2").join("seg");
        assert!(!leg_carries_run_end_marker(&seg_dir, "mk2", 1).unwrap());
    }

    /// Review pin: payload_ref is producer-class only — a spilled
    /// control-plane frame would carry its cross-field obligations (the
    /// take matrix, the WAL lattice, locator-must-declare) out of the
    /// verifier's inline walk. Enforced in Envelope::validate(), which
    /// runs on BOTH append and segment read, so writer and verifier can
    /// never disagree: the frame cannot even be written.
    #[test]
    fn spilled_control_frame_is_refused() {
        use crate::envelope::{PayloadEncoding, PayloadRef};
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "sp2");
        let content =
            br#"{"kind":"producer_spawn","detail":{"kill_domain":{"scheme":"cgroup","path":"/x"}}}"#;
        let digest = s.publish_blob(content).unwrap();
        let mut w = s.open_segment(0).unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = None;
        e.payload_ref = Some(PayloadRef {
            blob: crate::envelope::BlobRef {
                algo: "sha256".into(),
                digest,
                length: content.len() as u64,
                media_type: "application/json".into(),
            },
            encoding: PayloadEncoding::JsonUtf8,
        });
        let err = w.append(&e, Commit::Immediate).unwrap_err();
        assert!(format!("{err}").contains("producer-class only"), "got: {err}");
    }

    #[test]
    fn turn_closure_modes() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "tc1");
        let mut w = s.open_segment(0).unwrap();
        let mut open = test_env(1, 1);
        open.class = Class::TurnOpen;
        open.payload = Some(json!({"admitted_by": "test/rule"}));
        w.append(&open, Commit::Immediate).unwrap();
        // Segment stays OPEN (tip) with an unclosed turn.
        w.commit().unwrap();
        drop(w);
        let root = dir.path().join("tc1");
        // Complete: unmatched open is loud.
        assert!(verify_voyage(&root, "tc1").is_err());
        // AllowOpenTip: tolerated (one unmatched, in the open tip's epoch).
        verify_voyage_mode(&root, "tc1", VerifyMode::AllowOpenTip).unwrap();

        // Now a CLOSED turn in a sealed segment passes complete.
        let dir2 = tempfile::tempdir().unwrap();
        let mut s2 = store(dir2.path(), "tc2");
        let mut w2 = s2.open_segment(0).unwrap();
        let mut o2 = test_env(1, 1);
        o2.class = Class::TurnOpen;
        o2.payload = Some(json!({"admitted_by": "test/rule"}));
        w2.append(&o2, Commit::Immediate).unwrap();
        let mut c2 = test_env(1, 2);
        c2.class = Class::TurnClose;
        c2.payload = Some(json!({"reason": "producer_done"}));
        c2.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: Seq { epoch: 1, n: 1 } }];
        w2.append(&c2, Commit::Immediate).unwrap();
        w2.seal(None).unwrap();
        verify_voyage(&dir2.path().join("tc2"), "tc2").unwrap();
    }

    #[test]
    fn f64_feature_gates_fractional_producer_numbers() {
        use crate::segment::{HeaderBody, RetentionClass, SegmentWriter};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("f1");
        crate::voyage::VoyageStore::bootstrap(&root, "f1", RetentionClass::Discard).unwrap();
        let build = |features: Vec<String>, name: &str, dir: &std::path::Path| {
            let root = dir.join(name);
            crate::voyage::VoyageStore::bootstrap(&root, name, RetentionClass::Discard).ok();
            let header = HeaderBody {
                version: 1,
                required_features: features,
                voyage_id: name.into(),
                segment_index: 0,
                epoch: 1,
                prev_seal_digest: None,
                created_wall_ms: 0,
                retention_class: Some(RetentionClass::Discard),
            };
            let mut w = SegmentWriter::create(&root.join("seg"), header).unwrap();
            let mut att = test_env(1, 1);
            att.class = Class::ProducerAttached;
            att.payload = Some(json!({
                "producer_kind": "t", "version": "1",
                "profile_def": {"id": "d", "sha256": "0".repeat(64), "rules": {}}
            }));
            w.append(&att, Commit::Immediate).unwrap();
            let mut prod = test_env(1, 2);
            prod.refs = vec![FrameRef { kind: RefKind::AttachedTo, frame: Seq { epoch: 1, n: 1 } }];
            prod.payload = Some(json!({"cost": 0.0123, "exp": 1.5e-8, "n": 3}));
            w.append(&prod, Commit::Immediate).unwrap();
            w.seal(None).unwrap();
            root
        };
        // Without the feature: fractional producer numbers are loud.
        let r1 = build(vec![], "f_no", dir.path());
        assert!(verify_voyage(&r1, "f_no").is_err());
        // With the registered feature: green.
        let r2 = build(vec!["sot.producer.json-f64-v1".into()], "f_yes", dir.path());
        verify_voyage(&r2, "f_yes").unwrap();
        // Unknown feature: loud.
        let r3 = build(vec!["sot.future.unknown-v9".into()], "f_unk", dir.path());
        assert!(verify_voyage(&r3, "f_unk").is_err());
    }

    /// Review F1: a producer frame whose JSON payload rides a payload_ref
    /// (spilled) must hit the same f64 gate as an inline payload.
    #[test]
    fn f64_gate_covers_spilled_json_payload_ref() {
        use crate::envelope::{PayloadEncoding, PayloadRef};
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "sp1");
        let content = br#"{"cost": 0.5}"#;
        let digest = s.publish_blob(content).unwrap();
        let mut w = s.open_segment(0).unwrap();
        let mut att = test_env(1, 1);
        att.class = Class::ProducerAttached;
        att.payload = Some(json!({
            "producer_kind": "t", "version": "1",
            "profile_def": {"id": "d", "sha256": "0".repeat(64), "rules": {}}
        }));
        w.append(&att, Commit::Immediate).unwrap();
        let mut prod = test_env(1, 2);
        prod.refs = vec![FrameRef { kind: RefKind::AttachedTo, frame: Seq { epoch: 1, n: 1 } }];
        prod.payload = None;
        prod.payload_ref = Some(PayloadRef {
            blob: crate::envelope::BlobRef {
                algo: "sha256".into(),
                digest,
                length: content.len() as u64,
                media_type: "application/json".into(),
            },
            encoding: PayloadEncoding::JsonUtf8,
        });
        w.append(&prod, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        let err = verify_voyage(&dir.path().join("sp1"), "sp1").unwrap_err();
        assert!(format!("{err}").contains("via payload_ref"), "got: {err}");
    }

    /// Review F2 pin: an unmatched open in a SEALED segment of the SAME
    /// epoch as the open tip is tolerated by allow-open-tip — rotation
    /// within one run is normal, and a live turn may span it (the ADR's
    /// predicate is per-EPOCH, deliberately).
    #[test]
    fn allow_open_tip_tolerates_rotation_spanning_turn() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "rot1");
        let mut w = s.open_segment(0).unwrap();
        let mut open = test_env(1, 1);
        open.class = Class::TurnOpen;
        open.payload = Some(json!({"admitted_by": "test/rule"}));
        w.append(&open, Commit::Immediate).unwrap();
        let d = w.seal(None).unwrap(); // rotation: sealed with the turn open
        s.advance_chain(d);
        let mut w2 = s.open_segment(0).unwrap(); // same epoch, open tip
        let mut lc = test_env(1, 2);
        lc.class = Class::Lifecycle;
        lc.payload = Some(json!({"kind": "producer_ready"}));
        w2.append(&lc, Commit::Immediate).unwrap();
        w2.commit().unwrap();
        drop(w2);
        let root = dir.path().join("rot1");
        assert!(verify_voyage(&root, "rot1").is_err()); // complete: loud
        verify_voyage_mode(&root, "rot1", VerifyMode::AllowOpenTip).unwrap();
    }

    /// Review F4 pin: the integer-atoms rule is WIRE-FORM integrality —
    /// "3.0" is not an integer atom (matches the §3 shortest-decimal rule),
    /// deliberately. A refactor to value-integrality must fail here.
    #[test]
    fn integral_float_wire_form_is_refused_without_f64() {
        assert!(check_integer_numbers(&json!(3.0)).is_err());
        assert!(check_integer_numbers(&json!(3)).is_ok());
        assert!(check_integer_numbers(&json!({"a": [1, {"b": 2}]})).is_ok());
        assert!(check_integer_numbers(&json!({"a": [1, {"b": 2.0}]})).is_err());
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

    /// Finding 8: a `idem_key` that is a JSON string but not lowercase
    /// hex32 (here, uppercase) must fail the verifier too, not only the
    /// store's own dedupe fold (`voyage.rs`'s
    /// `dedupe_fold_rejects_a_malformed_idem_key`) -- both sides share the
    /// SAME format check (`voyage::parse_idem_key`).
    #[test]
    fn input_idem_key_must_be_lowercase_hex32() {
        let dir = tempfile::tempdir().unwrap();
        let key = "A".repeat(32); // uppercase: a string, not lowercase hex32
        let mut s = store(dir.path(), "fact-bad-hex");
        let mut w = s.open_segment(0).unwrap();
        w.append(&input_env(1, 1, &key), Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        assert!(verify_voyage(&dir.path().join("fact-bad-hex"), "fact-bad-hex").is_err());
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
