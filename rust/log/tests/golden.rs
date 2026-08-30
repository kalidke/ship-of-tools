#![cfg(any(target_os = "linux", windows))] // seals a segment -> needs the store's rename arm (ADR 0039 / ADR 0041 windows port)
//! Golden fixture: pins the v1 wire bytes. If this test fails after a code
//! change, the FORMAT changed — that is a versioning event (ADR 0039), not a
//! test to update casually. The fixture doubles as the cross-language
//! conformance input (the Julia reader will consume the same file).

use sot_log::segment::{Commit, HeaderBody, RetentionClass, SegmentReader, SegmentWriter};
use sot_log::{Actor, ActorKind, Class, Derivation, Emitter, Envelope, Seq, Source};
use std::path::PathBuf;

fn fixture_frames() -> Vec<Envelope> {
    let mk = |n: u64, class: Class, payload: serde_json::Value, refs| Envelope {
        seq: Seq { epoch: 1, n },
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
        t_wall_ms: 1_756_000_000_000,
        t_mono_us: n * 1_000,
        stream: None,
        transformed: None,
        refs,
        payload: Some(payload),
        payload_ref: None,
    };
    vec![
        mk(
            1,
            Class::ProducerAttached,
            serde_json::json!({
                "producer_kind": "fixture", "version": "1.0.0",
                "profile_def": {"id": "default", "sha256": "0".repeat(64), "rules": {}}
            }),
            vec![],
        ),
        mk(
            2,
            Class::Lifecycle,
            serde_json::json!({"kind": "producer_ready"}),
            vec![],
        ),
        {
            let mut e = mk(
                3,
                Class::Producer,
                serde_json::json!({"native": {"text": "hello, log"}}),
                vec![sot_log::FrameRef {
                    kind: sot_log::RefKind::AttachedTo,
                    frame: Seq { epoch: 1, n: 1 },
                }],
            );
            e.source.emitter = Emitter::Producer;
            e.source.actor.kind = ActorKind::Producer;
            e.source.derivation = Derivation::Native;
            e
        },
    ]
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden-v1.sotseg")
}

/// Regenerate with: UPDATE_GOLDEN=1 cargo test -p sot-log --test golden
#[test]
fn golden_segment_bytes_are_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let header = HeaderBody {
        version: 1,
        required_features: vec![],
        voyage_id: "01900000-0000-7000-8000-000000000001".into(),
        segment_index: 0,
        epoch: 1,
        prev_seal_digest: None,
        created_wall_ms: 1_756_000_000_000,
        retention_class: Some(RetentionClass::Archive),
    };
    let mut w = SegmentWriter::create(dir.path(), header).unwrap();
    for f in fixture_frames() {
        w.append(&f, Commit::Buffered).unwrap();
    }
    w.commit().unwrap();
    w.seal(None).unwrap();
    let generated = std::fs::read(dir.path().join("00000000-00000000000001.sotseg")).unwrap();

    let path = fixture_path();
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &generated).unwrap();
    }
    let committed = std::fs::read(&path).expect("fixture missing — run with UPDATE_GOLDEN=1 once");
    assert_eq!(
        committed, generated,
        "wire bytes changed — this is a FORMAT change (versioning event), not test drift"
    );

    // And the committed fixture must read + verify on its own.
    let r = SegmentReader::read(&path, true).unwrap();
    r.verify_seal().unwrap();
    assert_eq!(r.frames.len(), 3);
}


/// Second golden: the sot.producer.json-f64-v1 feature + fractional and
/// exponent number spellings. Regenerate: UPDATE_GOLDEN=1.
#[test]
fn golden_f64_segment_bytes_are_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let header = HeaderBody {
        version: 1,
        required_features: vec!["sot.producer.json-f64-v1".into()],
        voyage_id: "01900000-0000-7000-8000-0000000000f6".into(),
        segment_index: 0,
        epoch: 1,
        prev_seal_digest: None,
        created_wall_ms: 1_756_000_000_000,
        retention_class: Some(RetentionClass::Archive),
    };
    let mut w = SegmentWriter::create(dir.path(), header).unwrap();
    let mut frames = fixture_frames();
    frames[2].payload = Some(serde_json::json!({
        "native": {"text": "hello, f64"},
        "total_cost_usd": 0.048731,
        "tiny": 1.5e-8,
        "edge": 9007199254740991u64
    }));
    for f in frames {
        w.append(&f, Commit::Buffered).unwrap();
    }
    w.commit().unwrap();
    w.seal(None).unwrap();
    let generated = std::fs::read(dir.path().join("00000000-00000000000001.sotseg")).unwrap();

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden-f64-v1.sotseg");
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &generated).unwrap();
    }
    let committed = std::fs::read(&path).expect("fixture missing — run with UPDATE_GOLDEN=1 once");
    assert_eq!(committed, generated, "wire bytes changed — format event");
    let r = SegmentReader::read(&path, true).unwrap();
    r.verify_seal().unwrap();
    assert_eq!(r.frames.len(), 3);
}

/// Third golden: `sot.capsule.run-end-requested-v1` (ADR 0041 step 6
/// U1b) — the ordinary fixture content plus the EndRun marker frame
/// (`lifecycle.kind=run_end_requested`, carrying its `reason` verbatim)
/// a step-6 capsule appends last, in a segment that declares the
/// feature. Regenerate: UPDATE_GOLDEN=1.
#[test]
fn golden_run_end_requested_segment_bytes_are_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let header = HeaderBody {
        version: 1,
        required_features: vec!["sot.capsule.run-end-requested-v1".into()],
        voyage_id: "01900000-0000-7000-8000-0000000000e4".into(),
        segment_index: 0,
        epoch: 1,
        prev_seal_digest: None,
        created_wall_ms: 1_756_000_000_000,
        retention_class: Some(RetentionClass::Archive),
    };
    let mut w = SegmentWriter::create(dir.path(), header).unwrap();
    let mut frames = fixture_frames();
    frames.push(Envelope {
        seq: Seq { epoch: 1, n: 4 },
        class: Class::Lifecycle,
        source: Source {
            emitter: Emitter::Capsule,
            actor: Actor {
                kind: ActorKind::Unknown,
                controller_id: None,
                take_epoch: None,
            },
            derivation: Derivation::Synthetic,
        },
        t_wall_ms: 1_756_000_000_004,
        t_mono_us: 4_000,
        stream: None,
        transformed: None,
        refs: vec![],
        payload: Some(serde_json::json!({"kind": "run_end_requested", "reason": "operator quit"})),
        payload_ref: None,
    });
    for f in frames {
        w.append(&f, Commit::Buffered).unwrap();
    }
    w.commit().unwrap();
    w.seal(None).unwrap();
    let generated = std::fs::read(dir.path().join("00000000-00000000000001.sotseg")).unwrap();

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/golden-run-end-requested-v1.sotseg");
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, &generated).unwrap();
    }
    let committed = std::fs::read(&path).expect("fixture missing — run with UPDATE_GOLDEN=1 once");
    assert_eq!(committed, generated, "wire bytes changed — format event");
    let r = SegmentReader::read(&path, true).unwrap();
    r.verify_seal().unwrap();
    assert_eq!(r.frames.len(), 4);
}
