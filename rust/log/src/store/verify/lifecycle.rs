//! The lifecycle-class frame rules: per-kind fields, the kill-domain locator, take_epoch order and the input_fact
//! chain lattice.

use super::*;
use crate::store::envelope::Envelope;
use super::frame::WalkState;

pub(super) fn check_lifecycle(
    env: &Envelope,
    fence_ok: bool,
    run_end_requested_ok: bool,
    walk: &mut WalkState,
) -> Result<()> {
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
            check_lifecycle_fields(env, payload, kind, has_take, has_fact, &mut walk.run_end_seen_epochs)?;
            if kind == LifecycleKind::CaptureOptin {
                walk.capture_enabled = true;
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
            walk.committed_take_epoch =
                check_take_state(env, payload, kind, walk.committed_take_epoch, &mut walk.take_state_seen_epochs)?;
            check_input_fact(env, payload, kind, &walk.input_idem, &mut walk.idem_state, &mut walk.intent_owner)?;
        }
    }
    Ok(())
}

fn check_lifecycle_fields(
    env: &Envelope,
    payload: &serde_json::Value,
    kind: LifecycleKind,
    has_take: bool,
    has_fact: bool,
    run_end_seen_epochs: &mut HashSet<u64>,
) -> Result<()> {
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
    Ok(())
}

fn check_producer_spawn(env: &Envelope, payload: &serde_json::Value, kind: LifecycleKind, fence_ok: bool) -> Result<()> {
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
    Ok(())
}

fn check_take_state(
    env: &Envelope,
    payload: &serde_json::Value,
    kind: LifecycleKind,
    mut committed_take_epoch: u64,
    take_state_seen_epochs: &mut HashSet<u64>,
) -> Result<u64> {
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
    Ok(committed_take_epoch)
}

fn check_input_fact(
    env: &Envelope,
    payload: &serde_json::Value,
    kind: LifecycleKind,
    input_idem: &HashMap<(u64, u64), String>,
    idem_state: &mut HashMap<String, FactState>,
    intent_owner: &mut HashMap<(u64, u64), String>,
) -> Result<()> {
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
    Ok(())
}
