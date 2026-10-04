//! Frame builders shared by the store's voyage and dedupe tests.

use crate::envelope::{Actor, ActorKind, Class, Derivation, Emitter, Envelope, FrameRef, RefKind, Seq, Source};
use crate::segment::tests::test_env;

/// A conforming standalone frame (lifecycle needs no attached_to).
pub(super) fn lc(epoch: u64, n: u64) -> crate::envelope::Envelope {
    let mut e = test_env(epoch, n);
    e.class = Class::Lifecycle;
    e.payload = Some(serde_json::json!({"kind": "producer_ready"}));
    e
}

/// A controller-actor frame (`Actor.kind=controller` requires
/// `controller_id`+`take_epoch` — ADR 0039's cross-field matrix).
pub(super) fn ctrl_env(epoch: u64, n: u64, class: Class, payload: serde_json::Value, refs: Vec<FrameRef>) -> Envelope {
    Envelope {
        seq: Seq { epoch, n },
        class,
        source: Source {
            emitter: Emitter::Capsule,
            actor: Actor {
                kind: ActorKind::Controller,
                controller_id: Some("ctrl".into()),
                take_epoch: Some(2),
            },
            derivation: Derivation::Synthetic,
        },
        t_wall_ms: 1_756_000_000_000,
        t_mono_us: n * 1000,
        stream: None,
        transformed: None,
        refs,
        payload: Some(payload),
        payload_ref: None,
    }
}

/// A `take_state` lifecycle frame (revoke-first / grant), the preamble
/// every real capsule commits before any producer-bound action.
pub(super) fn lc_take(epoch: u64, n: u64, take_epoch: u64, holder: Option<&str>) -> Envelope {
    let mut e = lc(epoch, n);
    e.payload = Some(serde_json::json!({"kind": "take_state", "take": {"take_epoch": take_epoch, "holder": holder}}));
    e
}

pub(super) fn input_env(epoch: u64, n: u64, idem_key: &str) -> Envelope {
    ctrl_env(
        epoch,
        n,
        Class::Input,
        serde_json::json!({"idem_key": idem_key, "content": "redacted", "length": 3}),
        vec![],
    )
}

pub(super) fn intent_env(epoch: u64, n: u64, input: Seq) -> Envelope {
    ctrl_env(
        epoch,
        n,
        Class::Lifecycle,
        serde_json::json!({"kind": "input_fact",
            "fact": {"input": {"epoch": input.epoch, "n": input.n}, "fact": "forward_intent"}}),
        vec![FrameRef { kind: RefKind::CausedBy, frame: input }],
    )
}

pub(super) fn forwarded_env(epoch: u64, n: u64, input: Seq, intent: Seq) -> Envelope {
    ctrl_env(
        epoch,
        n,
        Class::Lifecycle,
        serde_json::json!({"kind": "input_fact",
            "fact": {"input": {"epoch": input.epoch, "n": input.n}, "fact": "forwarded",
                     "intent": {"epoch": intent.epoch, "n": intent.n}}}),
        vec![FrameRef { kind: RefKind::CausedBy, frame: input }],
    )
}

pub(super) fn refused_env(epoch: u64, n: u64, input: Seq) -> Envelope {
    ctrl_env(
        epoch,
        n,
        Class::Lifecycle,
        serde_json::json!({"kind": "input_fact",
            "fact": {"input": {"epoch": input.epoch, "n": input.n}, "fact": "refused_stale_epoch"}}),
        vec![FrameRef { kind: RefKind::CausedBy, frame: input }],
    )
}
