//! The voyage walk's per-frame rules, in the order check_frame calls them, and WalkState: what the frames before
//! the current one established.

use super::*;
use crate::envelope::Envelope;
use super::lifecycle::check_lifecycle;

/// What the walk has established about the frames before the one it checks.
#[derive(Default)]
pub(super) struct WalkState {
    // Every committed frame's class, keyed by seq — doubles as both the
    // ref-resolution existence set and the class lookup turn_close needs.
    pub(super) seen: HashMap<(u64, u64), Class>,
    pub(super) last_n_by_epoch: HashMap<u64, u64>, // per writer epoch, the last n: n is contiguous
    pub(super) capture_enabled: bool, // capture_optin seen: an input may carry bytes from here on

    // Exactly one non-duplicate_of turn_close per turn: turn_open seq ->
    // the winning close's seq.
    pub(super) turn_close_winner: HashMap<(u64, u64), (u64, u64)>,
    pub(super) turn_opens: HashSet<(u64, u64)>,

    // input_fact chain lattice, all keyed by idem_key.
    pub(super) idem_state: HashMap<String, FactState>,
    pub(super) idem_owner: HashMap<String, (u64, u64)>, // -> input frame seq
    pub(super) input_idem: HashMap<(u64, u64), String>, // input frame seq -> idem_key
    pub(super) intent_owner: HashMap<(u64, u64), String>, // forward_intent seq -> idem_key

    // Stream prev-chains, keyed by (attached_to seq, cell).
    pub(super) stream_head: HashMap<((u64, u64), String), (u64, u64)>,

    // take_epoch ordering.
    pub(super) committed_take_epoch: u64,
    pub(super) take_state_seen_epochs: HashSet<u64>,

    // Codex round-1 Major 7: at most one `run_end_requested` per writer
    // epoch — a marker governs only its own epoch (ADR 0041), and the
    // capsule's own first-commit-wins latch is a promise about ITS
    // process lifetime, not a proof a crafted or corrupted voyage can't
    // carry two. The verifier must refuse what the writer never would.
    pub(super) run_end_seen_epochs: HashSet<u64>,
}

pub(super) fn check_frame(
    root: &Path,
    epoch: &u64,
    f64_ok: bool,
    fence_ok: bool,
    run_end_requested_ok: bool,
    env: &Envelope,
    walk: &mut WalkState,
) -> Result<()> {
    check_frame_position(env, epoch, &mut walk.last_n_by_epoch, &walk.seen)?;

    let attached_to: Vec<Seq> = env
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::AttachedTo)
        .map(|r| r.frame)
        .collect();

    check_actor(env, walk.committed_take_epoch)?;
    check_attachment(env, epoch, &attached_to, &mut walk.stream_head)?;
    check_lifecycle(env, fence_ok, run_end_requested_ok, walk)?;
    check_producer_attached(env)?;
    check_control_exchange(env)?;
    check_turn_close(env, &walk.seen, &mut walk.turn_close_winner)?;
    check_input(env, walk.capture_enabled, &mut walk.idem_owner, &mut walk.idem_state, &mut walk.input_idem)?;
    check_blobs(root, env)?;

    if env.class == Class::TurnOpen {
        walk.turn_opens.insert((env.seq.epoch, env.seq.n));
    }
    check_producer_numbers(root, env, f64_ok)?;
    walk.seen.insert((env.seq.epoch, env.seq.n), env.class);
    Ok(())
}

fn check_frame_position(
    env: &Envelope,
    epoch: &u64,
    last_n_by_epoch: &mut HashMap<u64, u64>,
    seen: &HashMap<(u64, u64), Class>,
) -> Result<()> {
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
    Ok(())
}

fn check_actor(env: &Envelope, committed_take_epoch: u64) -> Result<()> {
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
    Ok(())
}

fn check_attachment(
    env: &Envelope,
    epoch: &u64,
    attached_to: &[Seq],
    stream_head: &mut HashMap<((u64, u64), String), (u64, u64)>,
) -> Result<()> {
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
    Ok(())
}

fn check_producer_attached(env: &Envelope) -> Result<()> {
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
    Ok(())
}

fn check_control_exchange(env: &Envelope) -> Result<()> {
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
    Ok(())
}

fn check_turn_close(
    env: &Envelope,
    seen: &HashMap<(u64, u64), Class>,
    turn_close_winner: &mut HashMap<(u64, u64), (u64, u64)>,
) -> Result<()> {
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
    Ok(())
}

fn check_input(
    env: &Envelope,
    capture_enabled: bool,
    idem_owner: &mut HashMap<String, (u64, u64)>,
    idem_state: &mut HashMap<String, FactState>,
    input_idem: &mut HashMap<(u64, u64), String>,
) -> Result<()> {
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
    Ok(())
}

fn check_blobs(root: &Path, env: &Envelope) -> Result<()> {
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
    Ok(())
}

fn check_producer_numbers(root: &Path, env: &Envelope, f64_ok: bool) -> Result<()> {
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
    Ok(())
}
