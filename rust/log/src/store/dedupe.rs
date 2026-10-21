//! The input-WAL dedupe index folded from the retained voyage at open (ADR 0041 decision 5).

use crate::store::envelope::{InputFactKind, Seq};
use crate::store::segment::SegmentReader;
use crate::{Error, Result};
use std::collections::HashMap;

/// A 16-byte `idem_key`, parsed once from its `hex32` wire/JSON form (ADR
/// 0041 decision 5: "Parsed keys, not Strings") and used as the dedupe
/// index's key type from then on.
pub type IdemKey = [u8; 16];

/// One `idem_key`'s position in the ADR 0039 "Input WAL + dedupe" lattice,
/// as folded from the retained voyage. Four states, not the verifier's
/// five (`FactState` in `store/verify/mod.rs` also tracks `Observed`): a
/// `producer_observed` fact — an adapter-only extension raw-terminal
/// capsules never emit (ADR 0041: "raw-terminal chains end at `forwarded`")
/// — folds into `Forwarded` here too, because both answer a duplicate
/// `idem_key` identically (`input_recorded`); this index exists to decide
/// that wire reply, not to record whether echo-confirmation ever happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupeState {
    Input,
    Intent,
    Forwarded,
    Refused,
}

/// One `idem_key`'s dedupe record. `input` is the ORIGINAL input frame's
/// seq — a `{input}`-only chain's retry-fold (ADR 0039: "chain = {input} =>
/// a same-key retry MUST re-attempt, new intent, same input identity")
/// needs it, not a freshly-minted one. `intent` is set once a
/// `forward_intent` fact has been folded in, so a later `forwarded`/
/// `refused_stale_epoch` fact (or a live re-attempt) can reference it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DedupeEntry {
    pub input: Seq,
    pub state: DedupeState,
    pub intent: Option<Seq>,
}

/// Parses a lowercase `hex32` `idem_key` into its 16 raw bytes. Shared by
/// the fold (below, the "writer" side) and `store/verify/` (finding 8: "add
/// lowercase-hex32 idem_key enforcement to BOTH the writer validation and
/// the verifier" — one format check, not two independently-drifting ones).
/// `pub(crate)` rather than a second copy: a malformed key is now a FOLD
/// ERROR (see `walk_segment`), not a silently-skipped frame.
pub(crate) fn parse_idem_key(s: &str) -> Option<IdemKey> {
    if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// `lifecycle.kind=input_fact`'s `fact` object, parsed by the fold and the verifier.
#[derive(serde::Deserialize)]
pub(super) struct FactObj {
    pub(super) input: Seq,
    pub(super) fact: InputFactKind,
    #[serde(default)]
    pub(super) intent: Option<Seq>,
}

/// Walks one segment's frames EXACTLY ONCE (finding 9: the previous version
/// traversed `reader.frames` a second time for this, even though there was
/// only ever one disk read/decode; `open_for_writing`'s max-take-epoch
/// tracking now folds into this SAME loop instead of its own), computing
/// the max committed `take_state.take_epoch` AND folding the ADR 0041
/// decision-5 input-WAL dedupe index together. `by_seq` is a
/// WALK-TIME-ONLY accessory (an `input_fact`'s `fact.input` names its input
/// frame by `Seq`, never by `idem_key` — the wire schema has no other way
/// to resolve it) threaded across every segment alongside `index`, then
/// dropped: a capsule's own LIVE updates after open always already hold the
/// idem_key of whatever they just wrote, so they never need it.
///
/// FAILS CLOSED (finding 8): a duplicate `idem_key` across two `input`
/// frames, an `input_fact` naming an unresolvable input, a fact-kind
/// illegal from its idem_key's CURRENT lattice state (mirroring
/// `store/verify/mod.rs`'s own `FactState` machine exactly — `forward_intent` only
/// from `Input`, `forwarded`/`refused_stale_epoch` only from... see below),
/// an unrecognized fact-kind string, or a malformed (non-lowercase-hex32)
/// `idem_key` are all errors, not silently-skipped or last-writer-wins
/// frames. This index is read by a LIVE writer to decide whether to
/// re-forward a duplicate input — silently mis-indexing it here is not a
/// defect an optional, separate `sot-log verify` run can be trusted to
/// catch first.
#[allow(clippy::too_many_lines, reason = "walks one retained segment to rebuild the dedupe index; predates the 100-line limit")]
pub(super) fn walk_segment(
    index: &mut HashMap<IdemKey, DedupeEntry>,
    by_seq: &mut HashMap<Seq, IdemKey>,
    reader: &SegmentReader,
) -> Result<u64> {
    let mut max_take = 0u64;
    for f in &reader.frames {
        let Some(p) = f.payload.as_ref() else { continue };

        if p.get("kind").and_then(|v| v.as_str()) == Some("take_state") {
            if let Some(te) = p.get("take").and_then(|t| t.get("take_epoch")).and_then(|v| v.as_u64()) {
                max_take = max_take.max(te);
            }
        }

        if f.class == crate::store::envelope::Class::Input {
            let key_str = p.get("idem_key").and_then(|v| v.as_str()).ok_or_else(|| {
                Error::Schema(format!("frame {:?}: idem_key is missing or not a string", f.seq))
            })?;
            let key = parse_idem_key(key_str).ok_or_else(|| {
                Error::Schema(format!("frame {:?}: idem_key {key_str:?} is not lowercase hex32", f.seq))
            })?;
            if index.contains_key(&key) {
                return Err(Error::Schema(format!(
                    "frame {:?}: idem_key {key_str:?} reused from an earlier input frame",
                    f.seq
                )));
            }
            by_seq.insert(f.seq, key);
            index.insert(
                key,
                DedupeEntry {
                    input: f.seq,
                    state: DedupeState::Input,
                    intent: None,
                },
            );
            continue;
        }

        if f.class != crate::store::envelope::Class::Lifecycle
            || p.get("kind").and_then(|v| v.as_str()) != Some("input_fact")
        {
            continue;
        }
        // Round-2 review, finding 6: the fold goes fully TYPED and
        // fallible here -- `serde_json::from_value` into the one `FactObj`
        // the fold and the verifier both parse (`input: Seq, fact:
        // InputFactKind, intent: Option<Seq>`), so a missing/malformed
        // `fact` object, a missing/non-object `input` seq, or an unknown
        // fact-kind string are ALL a single `Err` instead of three
        // separate silent `continue`s. `InputFactKind` is serde's closed
        // enum (`#[serde(rename_all = "snake_case")]`), so an
        // unrecognized string fails the deserialize itself -- no separate
        // catch-all arm needed anymore.
        let fact_value = p.get("fact").ok_or_else(|| {
            Error::Schema(format!("frame {:?}: input_fact is missing its fact object", f.seq))
        })?;
        let fact: FactObj = serde_json::from_value(fact_value.clone())
            .map_err(|e| Error::Schema(format!("frame {:?}: fact malformed: {e}", f.seq)))?;
        let key = *by_seq.get(&fact.input).ok_or_else(|| {
            Error::Schema(format!(
                "frame {:?}: input_fact names {:?}, which is not a committed input frame",
                f.seq, fact.input
            ))
        })?;
        let entry = index
            .get_mut(&key)
            .expect("by_seq and index are populated together, in the same branch above");
        match fact.fact {
            InputFactKind::ForwardIntent => {
                if entry.state != DedupeState::Input {
                    return Err(Error::Schema(format!(
                        "frame {:?}: forward_intent illegal from the current chain state for idem_key {key:02x?}",
                        f.seq
                    )));
                }
                // The `forward_intent` fact carries no separate `intent`
                // field (ADR 0039: only `forwarded`/`producer_observed`
                // do, naming a PRIOR frame) -- THIS frame's own seq IS the
                // intent record.
                entry.state = DedupeState::Intent;
                entry.intent = Some(f.seq);
            }
            InputFactKind::Forwarded => {
                if entry.state != DedupeState::Intent {
                    return Err(Error::Schema(format!(
                        "frame {:?}: forwarded illegal from the current chain state for idem_key {key:02x?}",
                        f.seq
                    )));
                }
                // Round-2 review, finding 6: `forwarded.intent` must name
                // the SAME `forward_intent` frame this idem_key's chain
                // actually recorded -- `entry.intent` is exactly that
                // (set, `Some`, the moment `ForwardIntent` fired above; a
                // mismatch or a missing `intent` field are equally wrong).
                if fact.intent != entry.intent {
                    return Err(Error::Schema(format!(
                        "frame {:?}: forwarded.intent {:?} does not match the recorded forward_intent {:?} for idem_key {key:02x?}",
                        f.seq, fact.intent, entry.intent
                    )));
                }
                entry.state = DedupeState::Forwarded;
            }
            InputFactKind::ProducerObserved => {
                if entry.state != DedupeState::Forwarded {
                    return Err(Error::Schema(format!(
                        "frame {:?}: producer_observed illegal from the current chain state for idem_key {key:02x?}",
                        f.seq
                    )));
                }
                if fact.intent != entry.intent {
                    return Err(Error::Schema(format!(
                        "frame {:?}: producer_observed.intent {:?} does not match the recorded forward_intent {:?} for idem_key {key:02x?}",
                        f.seq, fact.intent, entry.intent
                    )));
                }
                // Already the terminal `Forwarded` bucket (see
                // `DedupeState`'s doc) -- no state change.
            }
            InputFactKind::RefusedStaleEpoch => {
                if entry.state != DedupeState::Input {
                    return Err(Error::Schema(format!(
                        "frame {:?}: refused_stale_epoch illegal from the current chain state for idem_key {key:02x?}",
                        f.seq
                    )));
                }
                entry.state = DedupeState::Refused;
            }
        }
    }
    Ok(max_take)
}

#[cfg(test)]
#[cfg(any(target_os = "linux", windows))]
#[path = "dedupe_tests.rs"]
mod tests;
