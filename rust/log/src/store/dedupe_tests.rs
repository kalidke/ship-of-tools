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

/// The fold and the verifier parse an `input_fact`'s `fact` object alike:
/// both accept or both reject it, and a rejection names the same cause.
#[test]
fn fact_object_parses_alike_in_the_fold_and_the_verifier() {
    use crate::store::envelope::{FrameRef, RefKind};
    let input = serde_json::json!({"epoch": 1, "n": 3});
    let cases: Vec<(serde_json::Value, Option<&str>)> = vec![
        (serde_json::json!({"input": input, "fact": "forward_intent"}), None),
        (serde_json::json!({"input": input, "fact": "forward_intent", "extra": 1}), None),
        (serde_json::json!({"input": input, "fact": "bogus"}), Some("fact malformed")),
        (serde_json::json!({"fact": "forward_intent"}), Some("fact malformed")),
        (serde_json::json!("x"), Some("expected struct FactObj")),
    ];
    for (i, (case, want)) in cases.into_iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let id = format!("voyfact{i}");
        let root = dir.path().join(&id);
        VoyageStore::bootstrap(&root, &id, RetentionClass::Discard).unwrap();
        {
            let mut store = VoyageStore::open_for_writing(&root, &id).unwrap();
            let mut w = store.open_segment(0).unwrap();
            w.append(&lc_take(1, 1, 1, None), Commit::Immediate).unwrap();
            w.append(&lc_take(1, 2, 2, Some("ctrl")), Commit::Immediate).unwrap();
            w.append(&input_env(1, 3, &"9".repeat(32)), Commit::Immediate).unwrap();
            let fact = ctrl_env(
                1,
                4,
                Class::Lifecycle,
                serde_json::json!({"kind": "input_fact", "fact": case}),
                vec![FrameRef { kind: RefKind::CausedBy, frame: Seq { epoch: 1, n: 3 } }],
            );
            w.append(&fact, Commit::Immediate).unwrap();
            let d = w.seal(None).unwrap();
            store.advance_chain(d);
        }
        let verified = crate::store::verify::verify_voyage(&root, &id);
        let folded = VoyageStore::open_for_writing(&root, &id).map(|_| ());
        match want {
            None => {
                assert!(verified.is_ok(), "case {i}: verifier: {verified:?}");
                assert!(folded.is_ok(), "case {i}: fold: {folded:?}");
            }
            Some(text) => {
                let v = verified.expect_err("verifier must reject").to_string();
                let f = folded.expect_err("fold must reject").to_string();
                assert!(v.contains(text), "case {i}: verifier said: {v}");
                assert!(f.contains(text), "case {i}: fold said: {f}");
            }
        }
    }
}
