//! `sot-fault-writer <voyage-root> <voyage-id>`
//!
//! Minimal CROSS-PLATFORM voyage writer for the portable fault sweep
//! (`tests/fault_terminate.rs`, ADR 0041 build-order step 2). Opens the
//! voyage for writing, seals any survivor tip, opens a fresh segment, and
//! then writes an effectively-endless stream of `Class::Producer` frames —
//! it never seals. The parent test kills this process (`Child::kill()`,
//! i.e. TerminateProcess on Windows / SIGKILL on unix) at a random moment,
//! so the store's crash-recovery path is what ends every run, never a clean
//! exit.
//!
//! Frame shapes mirror `tests/golden.rs`'s `fixture_frames()` exactly —
//! those are proven writer- and verifier-legal.

use sot_log::segment::Commit;
use sot_log::voyage::VoyageStore;
use sot_log::{Actor, ActorKind, Class, Derivation, Emitter, Envelope, FrameRef, RefKind, Seq, Source};
use std::time::{SystemTime, UNIX_EPOCH};

fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Exit codes are part of the test contract: std's `Child::kill()` on
/// Windows is `TerminateProcess(handle, 1)`, and a `main() -> Result` error
/// ALSO exits 1 — indistinguishable. So organic failures exit 3, a clean
/// end is unrepresentable (`Never`), and exit code 1 on Windows therefore
/// proves TerminateProcess, not an organic death, ended the writer.
fn main() {
    match run() {
        Err(e) => {
            eprintln!("sot-fault-writer: {e}");
            std::process::exit(3);
        }
        Ok(never) => match never {},
    }
}

enum Never {}

fn run() -> sot_log::Result<Never> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = std::path::PathBuf::from(&args[0]);
    let voyage_id = args[1].clone();

    let mut store = VoyageStore::open_for_writing(&root, &voyage_id)?;
    store.seal_survivor()?;
    let epoch = store.epoch;
    let mut w = store.open_segment(wall_ms())?;

    let mk = |n: u64, class: Class, payload: serde_json::Value, refs: Vec<FrameRef>| Envelope {
        seq: Seq { epoch, n },
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
        t_wall_ms: wall_ms(),
        t_mono_us: n * 1_000,
        stream: None,
        transformed: None,
        refs,
        payload: Some(payload),
        payload_ref: None,
    };

    // Preamble: ProducerAttached then producer_ready — identical shape to
    // golden.rs's fixture_frames() frames 1-2.
    let attached = mk(
        1,
        Class::ProducerAttached,
        serde_json::json!({
            "producer_kind": "fixture", "version": "1.0.0",
            "profile_def": {"id": "default", "sha256": "0".repeat(64), "rules": {}}
        }),
        vec![],
    );
    let attached_seq = attached.seq;
    w.append(&attached, Commit::Buffered)?;
    let ready = mk(
        2,
        Class::Lifecycle,
        serde_json::json!({"kind": "producer_ready"}),
        vec![],
    );
    w.append(&ready, Commit::Buffered)?;

    // Endless producer stream (no sleeps — a tight loop) so the parent's
    // kill always lands mid-write. Never sealed: the kill is what ends us.
    let mut n = 3u64;
    loop {
        // Vary the record size (0..~4.7KB of filler). Each append writes
        // its record whole via `write_all` (Commit::Buffered defers only
        // the fsync), so a mid-record tear needs the kill to land inside a
        // SHORT-write retry — larger, irregular records widen that window
        // slightly, but tears stay RARE by nature (observed ~1 in dozens of
        // rounds): the true production source of torn tails is
        // power-loss/storage reordering, which no process-kill harness
        // simulates. Deterministic tear coverage is reconcile_matrix row 4;
        // the sweep's tear count is reported, never asserted.
        let filler = "x".repeat(((n * 131) % 4703) as usize);
        let mut e = mk(
            n,
            Class::Producer,
            serde_json::json!({"native": {"text": format!("payload-line-{n}-{filler}")}}),
            vec![FrameRef {
                kind: RefKind::AttachedTo,
                frame: attached_seq,
            }],
        );
        e.source.emitter = Emitter::Producer;
        e.source.actor.kind = ActorKind::Producer;
        e.source.derivation = Derivation::Native;
        w.append(&e, Commit::Buffered)?;
        if n % 25 == 0 {
            w.commit()?;
        }
        n += 1;
    }
}
