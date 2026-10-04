//! Verifier tests: the cross-field matrix, input-fact lattice, stream chains, take epochs and blob refs.

use super::support_tests::store;
use super::*;
use crate::store::envelope::*;
use crate::store::segment::{tests::test_env, Commit};
use serde_json::json;

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
