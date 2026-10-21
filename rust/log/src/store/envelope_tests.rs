use super::*;
use serde_json::json;

fn minimal(class: Class) -> Envelope {
    Envelope {
        seq: Seq { epoch: 1, n: 1 },
        class,
        source: Source {
            emitter: Emitter::Capsule,
            actor: Actor {
                kind: ActorKind::Unknown,
                controller_id: None,
                take_epoch: None,
            },
            derivation: Derivation::Synthetic,
        },
        t_wall_ms: 0,
        t_mono_us: 0,
        stream: None,
        transformed: None,
        refs: vec![],
        payload: Some(json!({})),
        payload_ref: None,
    }
}

#[test]
fn payload_xor_enforced() {
    let mut e = minimal(Class::Producer);
    e.payload = None;
    assert!(e.validate().is_err());
    e.payload = Some(json!({}));
    e.payload_ref = Some(PayloadRef {
        blob: BlobRef {
            algo: "sha256".into(),
            digest: "0".repeat(64),
            length: 1,
            media_type: "application/json".into(),
        },
        encoding: PayloadEncoding::Bytes,
    });
    assert!(e.validate().is_err());
}

#[test]
fn unknown_closed_enum_value_fails_closed() {
    let mut v = serde_json::to_value(minimal(Class::Producer)).unwrap();
    v["class"] = json!("brand_new_class");
    assert!(serde_json::from_value::<Envelope>(v).is_err());
}

#[test]
fn unknown_object_members_are_ignorable() {
    let mut v = serde_json::to_value(minimal(Class::Producer)).unwrap();
    v["totally_new_field"] = json!({"x": 1});
    assert!(serde_json::from_value::<Envelope>(v).is_ok());
}

#[test]
fn append_complete_is_illegal() {
    let mut e = minimal(Class::Producer);
    e.stream = Some(Stream {
        cell: "c".into(),
        mode: StreamMode::Append,
        complete: true,
        prev: None,
    });
    assert!(e.validate().is_err());
}

#[test]
fn n_zero_rejected() {
    let mut e = minimal(Class::Producer);
    e.seq.n = 0;
    assert!(e.validate().is_err());
}
