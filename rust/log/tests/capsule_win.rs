#![cfg(windows)]
//! Integration tests for the Windows capsule runtime (`src/capsule_win.rs`,
//! ADR 0041 step 4). Lives in `tests/` for the same reason `tests/conpty.rs`
//! does: one of these (the flood test) needs `env!("CARGO_BIN_EXE_...")` to
//! find its helper binary, which Cargo only wires up for integration test
//! binaries, and the rest are kept here too for one home and one
//! `cargo test -p sot-log --test capsule_win` filter.
//!
//! The host-handshake byte state machine's own unit tests (`host_handshake.rs`) are
//! pure and run everywhere already; what these tests add is proof the
//! WIRING is correct on a real ConPTY — that a real DA1 answer becomes a
//! well-formed `request`/`response` pair, that a real resize commits a real
//! `outcome`, that a real spawn failure and a real requested kill both seal
//! a verifiable voyage.

use sot_log::capsule_win::{self, CapsuleWinConfig, ControlCmd, ExitKind};
use sot_log::segment::{RetentionClass, SegmentReader};
use sot_log::verify::verify_voyage;
use sot_log::{Class, Envelope, RefKind};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn config(dir: &std::path::Path, name: &str, argv: Vec<String>, cols: u16, rows: u16) -> CapsuleWinConfig {
    CapsuleWinConfig {
        voyage_root: dir.join(name),
        voyage_id: name.to_string(),
        retention: RetentionClass::Discard,
        producer_kind: "test-shell".into(),
        argv,
        echo: false,
        cols,
        rows,
    }
}

/// Every sealed frame across every `.sotseg` in `root/seg`, in segment
/// order — mirrors `capsule.rs`'s own test helper of the same name (not
/// shared: see `capsule_win.rs`'s module doc on duplication).
fn sealed_frames(root: &std::path::Path, voyage: &str) -> Vec<Envelope> {
    let seg_dir = root.join("seg");
    let mut out = Vec::new();
    let mut names: Vec<String> = std::fs::read_dir(&seg_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for n in names {
        if n.ends_with(".sotseg") {
            let r = SegmentReader::read(&seg_dir.join(&n), true).unwrap();
            assert_eq!(r.header.voyage_id, voyage);
            out.extend(r.frames);
        }
    }
    out
}

/// Test-only base64 decoder for `capsule_win.rs`'s encode-only engine —
/// duplicated from `capsule.rs`'s own test helper.
fn decode_b64(s: &str) -> Vec<u8> {
    let val = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => 0,
        }
    };
    let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c) << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    out
}

/// Bounded join: `run` blocks until the run ends, and a bug in the
/// teardown sequence's own ordering is exactly the class of bug that would
/// hang it forever — a test must fail loud within a bounded wait, never
/// hang the suite.
fn wait_for_join<T: Send + 'static>(handle: std::thread::JoinHandle<T>, timeout: Duration) -> Option<T> {
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Some(handle.join().unwrap())
}

/// Test 1: E2E. `cmd.exe /d /c echo <marker>` runs to completion (a
/// natural producer exit — no `Kill` ever sent); the resulting voyage
/// verifies, carries the marker in its producer frames, and never carries
/// a turn frame (raw terminal). DSR presence is LOGGED, not asserted (host/
/// build-version-dependent — same reasoning as `tests/conpty.rs`'s own
/// DA1-presence finding) — but IF a request/response pair shows up, its
/// shape must be correct.
#[test]
fn e2e_records_and_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let marker = "SOT_CAPSULE_WIN_E2E_9f31";
    let argv = vec!["cmd.exe".to_string(), "/d".to_string(), "/c".to_string(), format!("echo {marker}")];
    let cfg = config(dir.path(), "e2e1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let summary = capsule_win::run(cfg, rx).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    assert_eq!(summary.exit_code, Some(0));
    assert_eq!(summary.segments_sealed, 1);
    verify_voyage(&root, "e2e1").unwrap();

    let frames = sealed_frames(&root, "e2e1");
    let mut all = Vec::new();
    for f in &frames {
        if f.class == Class::Producer {
            let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
            all.extend(decode_b64(b64));
        }
    }
    let text = String::from_utf8_lossy(&all);
    assert!(text.contains(marker), "got: {text:?}");
    assert!(frames.iter().all(|f| f.class != Class::TurnOpen && f.class != Class::TurnClose));
    let dead = frames
        .iter()
        .find(|f| f.class == Class::Lifecycle && f.payload.as_ref().unwrap()["kind"] == "producer_dead")
        .unwrap();
    assert_eq!(dead.payload.as_ref().unwrap()["detail"]["exit_code"], 0);

    let hh_reqs: Vec<&Envelope> = frames
        .iter()
        .filter(|f| {
            f.class == Class::ControlExchange
                && f.payload.as_ref().unwrap()["kind_ns"] == "conpty/host-handshake"
                && f.payload.as_ref().unwrap()["phase"] == "request"
        })
        .collect();
    let hh_resps: Vec<&Envelope> = frames
        .iter()
        .filter(|f| {
            f.class == Class::ControlExchange
                && f.payload.as_ref().unwrap()["kind_ns"] == "conpty/host-handshake"
                && f.payload.as_ref().unwrap()["phase"] == "response"
        })
        .collect();
    eprintln!(
        "capsule_win e2e finding: host-handshake requests={}, responses={}",
        hh_reqs.len(),
        hh_resps.len()
    );
    assert_eq!(hh_reqs.len(), hh_resps.len(), "every host-handshake request must have exactly one response");
    for resp in &hh_resps {
        let target = resp
            .refs
            .iter()
            .find(|r| r.kind == RefKind::RespondsTo)
            .map(|r| r.frame)
            .expect("dsr response missing responds_to");
        assert!(
            hh_reqs.iter().any(|r| r.seq == target),
            "dsr response responds_to an unknown request seq"
        );
    }
}

/// Test 2: spawn failure (a nonexistent executable) is compensated, not
/// escaped unsealed (the Linux capsule's own known gap, deliberately not
/// inherited here).
#[test]
fn spawn_failure_is_compensated() {
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["Z:\\sot_capsule_win_test_no_such_exe_9f31.exe".to_string()];
    let cfg = config(dir.path(), "fail1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let summary = capsule_win::run(cfg, rx).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::SpawnFailed);
    assert_eq!(summary.exit_code, None);
    assert_eq!(summary.segments_sealed, 1);
    verify_voyage(&root, "fail1").unwrap();

    let frames = sealed_frames(&root, "fail1");
    let dead = frames
        .iter()
        .find(|f| f.class == Class::Lifecycle && f.payload.as_ref().unwrap()["kind"] == "producer_dead")
        .unwrap();
    assert_eq!(dead.payload.as_ref().unwrap()["detail"]["spawn_failed"], true);
    assert!(dead.payload.as_ref().unwrap()["detail"]["exit_code"].is_null());
}

/// Test 2b: an out-of-budget INITIAL geometry is treated the same way —
/// "Initial geometry is validated by the same rule" a resize is (ADR 0041).
#[test]
fn spawn_failure_from_out_of_budget_initial_geometry() {
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["cmd.exe".to_string(), "/d".to_string(), "/c".to_string(), "exit 0".to_string()];
    let cfg = config(dir.path(), "fail2", argv, 1, 25); // cols=1 < the 2-column floor
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let summary = capsule_win::run(cfg, rx).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::SpawnFailed);
    verify_voyage(&root, "fail2").unwrap();
}

/// Test 3: a requested kill (`ControlCmd::Kill`) tears down a still-running
/// producer through the ONE orchestrator and still seals a verifiable
/// voyage, with a real (job-imposed) exit code recorded.
#[test]
fn requested_kill_tears_down_and_seals() {
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["cmd.exe".to_string()]; // bare interactive shell — stays open until killed
    let cfg = config(dir.path(), "kill1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || capsule_win::run(cfg, rx));
    std::thread::sleep(Duration::from_millis(500));
    tx.send(ControlCmd::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    assert!(summary.exit_code.is_some());
    verify_voyage(&root, "kill1").unwrap();
}

/// Test 4: resize is an ordered request+outcome exchange (no response
/// phase — ADR 0041), rejecting out-of-budget requests rather than
/// clamping them, with the outcome's `target` naming the request it
/// resolves.
#[test]
fn resize_ordered_exchange_commits_and_rejects() {
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["cmd.exe".to_string()];
    let cfg = config(dir.path(), "resize1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || capsule_win::run(cfg, rx));
    std::thread::sleep(Duration::from_millis(500));
    tx.send(ControlCmd::Resize { cols: 100, rows: 40 }).unwrap(); // in budget
    std::thread::sleep(Duration::from_millis(300));
    tx.send(ControlCmd::Resize { cols: 9999, rows: 40 }).unwrap(); // > 512 cols
    std::thread::sleep(Duration::from_millis(300));
    tx.send(ControlCmd::Resize { cols: 40, rows: 1 }).unwrap(); // < 2 rows
    std::thread::sleep(Duration::from_millis(300));
    tx.send(ControlCmd::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    verify_voyage(&root, "resize1").unwrap();

    let frames = sealed_frames(&root, "resize1");
    let is_resize_outcome = |f: &&Envelope| {
        f.class == Class::ControlExchange
            && f.payload.as_ref().unwrap()["kind_ns"] == "conpty/resize"
            && f.payload.as_ref().unwrap()["phase"] == "outcome"
    };
    let outcomes: Vec<&Envelope> = frames.iter().filter(is_resize_outcome).collect();
    assert_eq!(outcomes.len(), 3, "expected 3 resize outcomes, got {}", outcomes.len());
    assert_eq!(outcomes[0].payload.as_ref().unwrap()["body"]["disposition"], "ok");
    assert_eq!(outcomes[1].payload.as_ref().unwrap()["body"]["disposition"], "failed");
    assert_eq!(outcomes[2].payload.as_ref().unwrap()["body"]["disposition"], "failed");

    let requests: Vec<&Envelope> = frames
        .iter()
        .filter(|f| {
            f.class == Class::ControlExchange
                && f.payload.as_ref().unwrap()["kind_ns"] == "conpty/resize"
                && f.payload.as_ref().unwrap()["phase"] == "request"
        })
        .collect();
    assert_eq!(requests.len(), 3);
    for outcome in &outcomes {
        let target = outcome.payload.as_ref().unwrap()["target"].as_str().unwrap().to_string();
        assert!(
            requests.iter().any(|r| format!("{}:{}", r.seq.epoch, r.seq.n) == target),
            "outcome target {target} does not name a real request seq"
        );
    }
}

/// Test 5: backpressure. A flood producer emits well beyond the 8 MiB
/// output budget; the run must still capture every byte exactly once (no
/// loss, no duplication — the byte-accounting proof the budget's condvar
/// gating exists to make safe) and seal a verifiable voyage.
#[test]
fn flood_survives_backpressure_without_loss() {
    let dir = tempfile::tempdir().unwrap();
    let helper = env!("CARGO_BIN_EXE_sot-conpty-helper").to_string();
    let total: usize = 20 * 1024 * 1024; // > the 8 MiB producer-channel budget
    let argv = vec![helper, "--flood".to_string(), total.to_string()];
    let cfg = config(dir.path(), "flood1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let start = Instant::now();
    let summary = capsule_win::run(cfg, rx).unwrap();
    eprintln!("capsule_win flood finding: {total} bytes in {:?}", start.elapsed());
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    assert_eq!(summary.exit_code, Some(0));
    verify_voyage(&root, "flood1").unwrap();

    let frames = sealed_frames(&root, "flood1");
    let mut total_decoded = 0usize;
    for f in &frames {
        if f.class == Class::Producer {
            let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
            total_decoded += decode_b64(b64).len();
        }
    }
    assert_eq!(total_decoded, total, "byte count mismatch: the flood was lost or duplicated");
}
