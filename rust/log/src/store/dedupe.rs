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
        // fallible here -- `serde_json::from_value` into the same shape
        // `store/verify/mod.rs`'s own `FactObj` deserializes (`input: Seq, fact:
        // InputFactKind, intent: Option<Seq>`), so a missing/malformed
        // `fact` object, a missing/non-object `input` seq, or an unknown
        // fact-kind string are ALL a single `Err` instead of three
        // separate silent `continue`s. `InputFactKind` is serde's closed
        // enum (`#[serde(rename_all = "snake_case")]`), so an
        // unrecognized string fails the deserialize itself -- no separate
        // catch-all arm needed anymore.
        #[derive(serde::Deserialize)]
        struct FactObj {
            input: Seq,
            fact: InputFactKind,
            #[serde(default)]
            intent: Option<Seq>,
        }
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
mod tests {
    use super::super::support_tests::{ctrl_env, forwarded_env, input_env, intent_env, lc_take, refused_env};
    use super::super::voyage::VoyageStore;
    use super::*;
    use crate::store::envelope::Class;
    use crate::store::segment::{Commit, RetentionClass};
    /// The dedupe index, from a voyage exercising every legal `idem_key`
    /// chain shape (ADR 0039's exact five, minus `{…,observed}` which
    /// `DedupeState` deliberately folds into `Forwarded` — see its doc),
    /// folded by `open_for_writing`'s OWN existing frame walk. "No second
    /// scan" is asserted by construction, not timing: `walk_segment` is
    /// called only from inside that walk (visible in this module's source,
    /// `open_for_writing` above), computing BOTH the max take epoch and the
    /// dedupe fold in one pass — there is no second call site anywhere in
    /// the crate. Reopening as a SUCCESSOR incarnation (a fresh
    /// `open_for_writing`, not the same store handle) proves the index
    /// survives an epoch boundary — the exact case decision 5 names: "a
    /// successor capsule starting with an empty index would let a
    /// pre-crash `forwarded` key re-forward".
    #[test]
    fn dedupe_index_folds_every_legal_chain_shape_across_a_capsule_restart() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voyd");
        VoyageStore::bootstrap(&root, "voyd", RetentionClass::Discard).unwrap();

        let key_input_only = "1".repeat(32);
        let key_intent_only = "2".repeat(32);
        let key_forwarded = "3".repeat(32);
        let key_refused = "4".repeat(32);

        {
            let mut store = VoyageStore::open_for_writing(&root, "voyd").unwrap();
            let mut w = store.open_segment(0).unwrap();
            // The take preamble every real capsule commits: revoke-first
            // (null holder), then grant -- required for the verifier's
            // take-matrix check (a controller frame's declared take_epoch
            // must match committed state).
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            // {input} only.
            w.append(&input_env(1, 3, &key_input_only), Commit::Immediate).unwrap();
            // {input, intent}.
            w.append(&input_env(1, 4, &key_intent_only), Commit::Immediate).unwrap();
            w.append(&intent_env(1, 5, Seq { epoch: 1, n: 4 }), Commit::Immediate).unwrap();
            // {input, intent, forwarded}.
            w.append(&input_env(1, 6, &key_forwarded), Commit::Immediate).unwrap();
            w.append(&intent_env(1, 7, Seq { epoch: 1, n: 6 }), Commit::Immediate).unwrap();
            w.append(
                &forwarded_env(1, 8, Seq { epoch: 1, n: 6 }, Seq { epoch: 1, n: 7 }),
                Commit::Immediate,
            )
            .unwrap();
            // {input, refused} -- refusal skips intent entirely (ADR 0039's
            // exact chain list has no {input,intent,refused} member).
            w.append(&input_env(1, 9, &key_refused), Commit::Immediate).unwrap();
            w.append(&refused_env(1, 10, Seq { epoch: 1, n: 9 }), Commit::Immediate).unwrap();
            let d = w.seal(None).unwrap();
            store.advance_chain(d);

            crate::store::verify::verify_voyage(&root, "voyd").unwrap();
        }

        // Reopen as a successor incarnation -- the index must be rebuilt
        // from the retained voyage, not start empty.
        let store = VoyageStore::open_for_writing(&root, "voyd").unwrap();
        let key = |s: &str| -> IdemKey { parse_idem_key(s).unwrap() };

        let e = store.dedupe_index[&key(&key_input_only)];
        assert_eq!(e.state, DedupeState::Input);
        assert_eq!(e.input, Seq { epoch: 1, n: 3 });
        assert_eq!(e.intent, None);

        let e = store.dedupe_index[&key(&key_intent_only)];
        assert_eq!(e.state, DedupeState::Intent);
        assert_eq!(e.input, Seq { epoch: 1, n: 4 });
        assert_eq!(e.intent, Some(Seq { epoch: 1, n: 5 }));

        let e = store.dedupe_index[&key(&key_forwarded)];
        assert_eq!(e.state, DedupeState::Forwarded);
        assert_eq!(e.input, Seq { epoch: 1, n: 6 });
        assert_eq!(e.intent, Some(Seq { epoch: 1, n: 7 }));

        let e = store.dedupe_index[&key(&key_refused)];
        assert_eq!(e.state, DedupeState::Refused);
        assert_eq!(e.input, Seq { epoch: 1, n: 9 });
        assert_eq!(e.intent, None);

        assert_eq!(store.dedupe_index.len(), 4, "no extra/spurious entries");
    }

    /// Finding 8: the fold FAILS CLOSED on retained history it cannot
    /// trust, rather than silently degrading the index. Four ways a
    /// segment can be untrustworthy, each its own test below: a duplicate
    /// `idem_key` across two `input` frames (previously last-writer-wins);
    /// an `input_fact` naming a `Seq` that was never a committed `input`
    /// frame (previously silently skipped); a fact-kind illegal from its
    /// idem_key's current lattice state, mirroring `store/verify/`'s own
    /// `FactState` machine (previously accepted unconditionally); and a
    /// malformed (non-lowercase-hex32) `idem_key` (previously silently
    /// skipped, which could verify green yet omit an identity from the
    /// index and re-forward it).
    #[test]
    fn dedupe_fold_rejects_a_duplicate_idem_key_across_two_input_frames() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voydup");
        VoyageStore::bootstrap(&root, "voydup", RetentionClass::Discard).unwrap();
        let dup_key = "5".repeat(32);
        {
            let mut store = VoyageStore::open_for_writing(&root, "voydup").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            w.append(&input_env(1, 3, &dup_key), Commit::Immediate).unwrap();
            w.append(&input_env(1, 4, &dup_key), Commit::Immediate).unwrap(); // SAME key, second input
            w.seal(None).unwrap();
        }
        let Err(err) = VoyageStore::open_for_writing(&root, "voydup") else { panic!("expected open_for_writing to fail") };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");
    }

    #[test]
    fn dedupe_fold_rejects_a_fact_naming_an_unresolvable_input() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voybadref");
        VoyageStore::bootstrap(&root, "voybadref", RetentionClass::Discard).unwrap();
        {
            let mut store = VoyageStore::open_for_writing(&root, "voybadref").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            // A forward_intent fact naming a Seq that was never an input
            // frame.
            w.append(&intent_env(1, 3, Seq { epoch: 1, n: 99 }), Commit::Immediate).unwrap();
            w.seal(None).unwrap();
        }
        let Err(err) = VoyageStore::open_for_writing(&root, "voybadref") else { panic!("expected open_for_writing to fail") };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");
    }

    #[test]
    fn dedupe_fold_rejects_an_illegal_state_transition() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voyillegal");
        VoyageStore::bootstrap(&root, "voyillegal", RetentionClass::Discard).unwrap();
        let key = "6".repeat(32);
        {
            let mut store = VoyageStore::open_for_writing(&root, "voyillegal").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            w.append(&input_env(1, 3, &key), Commit::Immediate).unwrap();
            // `forwarded` directly from the `Input` state, skipping
            // `forward_intent` entirely -- illegal (mirrors store/verify/'s
            // own FactState lattice).
            let input_seq = Seq { epoch: 1, n: 3 };
            w.append(&forwarded_env(1, 4, input_seq, input_seq), Commit::Immediate).unwrap();
            w.seal(None).unwrap();
        }
        let Err(err) = VoyageStore::open_for_writing(&root, "voyillegal") else { panic!("expected open_for_writing to fail") };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");
    }

    #[test]
    fn dedupe_fold_rejects_a_malformed_idem_key() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voybadkey");
        VoyageStore::bootstrap(&root, "voybadkey", RetentionClass::Discard).unwrap();
        {
            let mut store = VoyageStore::open_for_writing(&root, "voybadkey").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            // Uppercase hex: structurally a 32-char string, not lowercase
            // hex32.
            w.append(&input_env(1, 3, &"A".repeat(32)), Commit::Immediate).unwrap();
            w.seal(None).unwrap();
        }
        let Err(err) = VoyageStore::open_for_writing(&root, "voybadkey") else { panic!("expected open_for_writing to fail") };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");
    }

    /// Round-2 review, finding 6: an `input` frame whose `idem_key` field
    /// is entirely ABSENT (not merely malformed) must fail closed too --
    /// previously a silent `continue` that dropped the frame out of the
    /// index without a trace.
    #[test]
    fn dedupe_fold_rejects_an_input_frame_missing_idem_key_entirely() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voynokey");
        VoyageStore::bootstrap(&root, "voynokey", RetentionClass::Discard).unwrap();
        {
            let mut store = VoyageStore::open_for_writing(&root, "voynokey").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            let no_key = ctrl_env(1, 3, Class::Input, serde_json::json!({"content": "redacted", "length": 3}), vec![]);
            w.append(&no_key, Commit::Immediate).unwrap();
            w.seal(None).unwrap();
        }
        let Err(err) = VoyageStore::open_for_writing(&root, "voynokey") else { panic!("expected open_for_writing to fail") };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");
    }

    /// Round-2 review, finding 6: an `input_fact` lifecycle frame with no
    /// `fact` object at all must fail closed -- previously a silent
    /// `continue`.
    #[test]
    fn dedupe_fold_rejects_an_input_fact_missing_its_fact_object() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voynofact");
        VoyageStore::bootstrap(&root, "voynofact", RetentionClass::Discard).unwrap();
        {
            let mut store = VoyageStore::open_for_writing(&root, "voynofact").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            let key = "7".repeat(32);
            w.append(&input_env(1, 3, &key), Commit::Immediate).unwrap();
            let no_fact = ctrl_env(1, 4, Class::Lifecycle, serde_json::json!({"kind": "input_fact"}), vec![]);
            w.append(&no_fact, Commit::Immediate).unwrap();
            w.seal(None).unwrap();
        }
        let Err(err) = VoyageStore::open_for_writing(&root, "voynofact") else { panic!("expected open_for_writing to fail") };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");
    }

    /// Round-2 review, finding 6 (last clause): a `forwarded` fact whose
    /// `intent` does not name the SAME `forward_intent` frame this
    /// idem_key's own chain recorded must fail closed -- previously
    /// unchecked entirely (the fold never even read `intent`).
    #[test]
    fn dedupe_fold_rejects_a_forwarded_intent_that_does_not_match_the_recorded_intent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("voybadintent");
        VoyageStore::bootstrap(&root, "voybadintent", RetentionClass::Discard).unwrap();
        {
            let mut store = VoyageStore::open_for_writing(&root, "voybadintent").unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            let key = "8".repeat(32);
            let input_seq = Seq { epoch: 1, n: 3 };
            w.append(&input_env(1, 3, &key), Commit::Immediate).unwrap();
            w.append(&intent_env(1, 4, input_seq), Commit::Immediate).unwrap(); // the REAL forward_intent is seq (1,4)
            // `forwarded.intent` claims the INPUT frame's own seq instead
            // of the real forward_intent frame's seq -- wrong reference.
            w.append(&forwarded_env(1, 5, input_seq, input_seq), Commit::Immediate).unwrap();
            w.seal(None).unwrap();
        }
        let Err(err) = VoyageStore::open_for_writing(&root, "voybadintent") else { panic!("expected open_for_writing to fail") };
        assert!(matches!(err, Error::Schema(_)), "expected a Schema error, got: {err}");
    }
}
