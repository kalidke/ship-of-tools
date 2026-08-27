#![cfg(windows)]
//! Integration tests for the Windows capsule runtime (`src/capsule_win.rs`,
//! ADR 0041 step 4). Lives in `tests/` for the same reason `tests/conpty.rs`
//! does: one of these (the flood test) needs `env!("CARGO_BIN_EXE_...")` to
//! find its helper binary, which Cargo only wires up for integration test
//! binaries, and the rest are kept here too for one home and one
//! `cargo test -p sot-log --test capsule_win` filter.
//!
//! The host-handshake byte state machine's own unit tests
//! (`host_handshake.rs`) are pure and run everywhere already; what these
//! tests add is proof the WIRING is correct on a real ConPTY — that a real
//! DA1 answer becomes a well-formed, exactly-once `request`/`response`/
//! `outcome` triple, that a real resize commits a real `outcome` and calls
//! `ResizePseudoConsole` exactly when it should, and that a real spawn
//! failure and a real requested kill both seal a verifiable voyage with
//! `producer_dead` as the last frame in it.
//!
//! Discharge round (Codex review, finding 7): the previous version's flood
//! test asserted byte-count equality across a lossy transform boundary
//! (`hOutput` is conhost's own rendered VT stream, not raw child stdout)
//! and ran `run` on the test's own thread, so a teardown deadlock consumed
//! the whole CI job's timeout instead of failing locally; the handshake
//! test checked membership, not a bijection; the resize test could pass
//! even if every outcome targeted the same request or `ResizePseudoConsole`
//! were never actually gated. All four are fixed below.

use sot_log::capsule_win::{self, CapsuleWinConfig, Command, ExitKind};
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
/// hang the suite (or, worse, the whole CI job's own timeout — review
/// finding on the flood test specifically).
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

/// `producer_dead` must be the LAST frame ever appended to the last
/// segment before it was sealed — the order the verifier itself does not
/// enforce (review finding). Returns its `detail` payload for further
/// assertions.
fn assert_producer_dead_is_last(frames: &[Envelope]) -> serde_json::Value {
    let last = frames.last().expect("no frames at all");
    assert_eq!(last.class, Class::Lifecycle, "producer_dead is not the last frame in the segment");
    let payload = last.payload.as_ref().unwrap();
    assert_eq!(payload["kind"], "producer_dead", "last frame is not producer_dead: {payload:?}");
    payload["detail"].clone()
}

/// Test 1: E2E. `cmd.exe /d /c echo <marker>` runs to completion (a
/// natural producer exit — no `Kill` ever sent); the resulting voyage
/// verifies, carries the marker in its producer frames, never carries a
/// turn frame (raw terminal), and ends with `producer_dead`. The
/// host-handshake exchange is a BIJECTION, not membership (review
/// finding): at most one request, matched by exactly one response and
/// exactly one outcome, linked correctly — but tolerating zero, since DA1
/// presence is host/build-version-dependent (same reasoning as
/// `tests/conpty.rs`'s own DA1-presence finding).
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
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["exit_code"], 0);

    let phase_is = |f: &&Envelope, kind_ns: &str, phase: &str| {
        f.class == Class::ControlExchange
            && f.payload.as_ref().unwrap()["kind_ns"] == kind_ns
            && f.payload.as_ref().unwrap()["phase"] == phase
    };
    let hh_reqs: Vec<&Envelope> =
        frames.iter().filter(|f| phase_is(f, "conpty/host-handshake", "request")).collect();
    let hh_resps: Vec<&Envelope> =
        frames.iter().filter(|f| phase_is(f, "conpty/host-handshake", "response")).collect();
    let hh_outcomes: Vec<&Envelope> =
        frames.iter().filter(|f| phase_is(f, "conpty/host-handshake", "outcome")).collect();
    eprintln!(
        "capsule_win e2e finding: host-handshake requests={}, responses={}, outcomes={}",
        hh_reqs.len(),
        hh_resps.len(),
        hh_outcomes.len()
    );
    assert!(hh_reqs.len() <= 1, "ADR 0041's model answers the handshake at most once per run");
    assert_eq!(hh_reqs.len(), hh_resps.len(), "bijection: every request has exactly one response");
    assert_eq!(hh_resps.len(), hh_outcomes.len(), "bijection: every response has exactly one outcome");
    assert_eq!(summary.handshake_answered, !hh_reqs.is_empty());
    if let Some(&req) = hh_reqs.first() {
        let resp = hh_resps[0];
        let outcome = hh_outcomes[0];
        let responds_to = resp
            .refs
            .iter()
            .find(|r| r.kind == RefKind::RespondsTo)
            .map(|r| r.frame)
            .expect("host-handshake response missing responds_to");
        assert_eq!(responds_to, req.seq, "response must respond to ITS OWN request");
        let target = outcome.payload.as_ref().unwrap()["target"].as_str().unwrap().to_string();
        assert_eq!(target, format!("{}:{}", req.seq.epoch, req.seq.n), "outcome must target ITS OWN request");
        assert_eq!(outcome.payload.as_ref().unwrap()["body"]["disposition"], "ok");
    }
}

/// Test 2: spawn failure (a nonexistent executable) is compensated, not
/// escaped unsealed (the Linux capsule's own known gap, deliberately not
/// inherited here), and `producer_dead` is still the last frame recorded.
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
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["spawn_failed"], true);
    assert!(dead["exit_code"].is_null());
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
    let frames = sealed_frames(&root, "fail2");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["spawn_failed"], true);
}

/// Test 3: a requested kill (`Command::Kill`) tears down a still-running
/// producer through the ONE orchestrator and still seals a verifiable
/// voyage, with a real (job-imposed) exit code recorded as the last frame.
#[test]
fn requested_kill_tears_down_and_seals() {
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["cmd.exe".to_string()]; // bare interactive shell — stays open until killed
    let cfg = config(dir.path(), "kill1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || capsule_win::run(cfg, rx));
    std::thread::sleep(Duration::from_millis(500));
    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    assert!(summary.exit_code.is_some());
    verify_voyage(&root, "kill1").unwrap();

    let frames = sealed_frames(&root, "kill1");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["exit_code"], summary.exit_code.unwrap());
}

/// Test 4: resize is an ordered request+outcome exchange (no response
/// phase — ADR 0041), rejecting out-of-budget requests rather than
/// clamping them, with the outcome's `target` naming ITS OWN request (not
/// just some real request — review finding), and `ResizePseudoConsole`
/// actually invoked exactly once (the in-budget request only — review
/// finding: the disposition string alone doesn't prove the OS call was
/// really gated).
#[test]
fn resize_ordered_exchange_commits_and_rejects() {
    let dir = tempfile::tempdir().unwrap();
    let argv = vec!["cmd.exe".to_string()];
    let cfg = config(dir.path(), "resize1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || capsule_win::run(cfg, rx));
    std::thread::sleep(Duration::from_millis(500));
    tx.send(Command::Resize { cols: 100, rows: 40 }).unwrap(); // in budget
    std::thread::sleep(Duration::from_millis(300));
    tx.send(Command::Resize { cols: 9999, rows: 40 }).unwrap(); // > 512 cols
    std::thread::sleep(Duration::from_millis(300));
    tx.send(Command::Resize { cols: 40, rows: 1 }).unwrap(); // < 2 rows
    std::thread::sleep(Duration::from_millis(300));
    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
    assert_eq!(summary.resize_os_calls, 1, "expected exactly one ResizePseudoConsole call (the valid request only)");
    verify_voyage(&root, "resize1").unwrap();

    let frames = sealed_frames(&root, "resize1");
    let phase_is = |f: &&Envelope, phase: &str| {
        f.class == Class::ControlExchange
            && f.payload.as_ref().unwrap()["kind_ns"] == "conpty/resize"
            && f.payload.as_ref().unwrap()["phase"] == phase
    };
    let requests: Vec<&Envelope> = frames.iter().filter(|f| phase_is(f, "request")).collect();
    let outcomes: Vec<&Envelope> = frames.iter().filter(|f| phase_is(f, "outcome")).collect();
    assert_eq!(requests.len(), 3, "expected 3 resize requests, got {}", requests.len());
    assert_eq!(outcomes.len(), 3, "expected 3 resize outcomes, got {}", outcomes.len());
    assert_eq!(outcomes[0].payload.as_ref().unwrap()["body"]["disposition"], "ok");
    assert_eq!(outcomes[1].payload.as_ref().unwrap()["body"]["disposition"], "failed");
    assert_eq!(outcomes[2].payload.as_ref().unwrap()["body"]["disposition"], "failed");

    // Each outcome must target its OWN request (by emission order, since
    // request[i] and outcome[i] commit as one uninterrupted pair) — not
    // just "some" real request, which the previous version's `.any(...)`
    // would have let a misattribution bug slip through undetected.
    for (req, outcome) in requests.iter().zip(outcomes.iter()) {
        let target = outcome.payload.as_ref().unwrap()["target"].as_str().unwrap().to_string();
        let expected = format!("{}:{}", req.seq.epoch, req.seq.n);
        assert_eq!(target, expected, "outcome does not target its own request");
    }
}

/// Test 5: backpressure. A flood producer emits well beyond the 8 MiB
/// output budget; the run must engage the budget's backpressure (proven by
/// its own high-water/blocked-count seam, not a byte-count comparison
/// across the wrong transform boundary — review finding) without
/// deadlocking, and seal a verifiable voyage. Run on a background thread
/// with a LOCAL bounded wait: a teardown regression here is exactly a
/// deadlock, and this test must fail loud within its own bound rather than
/// consume the whole CI job's timeout.
#[test]
fn flood_engages_backpressure_without_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let helper = env!("CARGO_BIN_EXE_sot-conpty-helper").to_string();
    let total: usize = 20 * 1024 * 1024; // > the 8 MiB producer-channel budget
    let argv = vec![helper, "--flood".to_string(), total.to_string()];
    let cfg = config(dir.path(), "flood1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let start = Instant::now();
    let handle = std::thread::spawn(move || capsule_win::run(cfg, rx));
    let summary = wait_for_join(handle, Duration::from_secs(60))
        .expect("run did not return within the local deadline (deadlock?)")
        .unwrap();
    eprintln!("capsule_win flood finding: {total} bytes in {:?}", start.elapsed());
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    assert_eq!(summary.exit_code, Some(0));
    verify_voyage(&root, "flood1").unwrap();

    // The right side of the transform boundary (review finding): hOutput
    // is conhost's own rendered VT stream, not a byte-for-byte copy of
    // what the child wrote to its own stdout — startup sequences and
    // line-wrap/scroll handling can legitimately change the total length,
    // so exact equality against `total` proves nothing. And whether the
    // budget ever actually BLOCKED during the flood is not this test's to
    // assert: engagement depends on conhost's burst pacing on the runner,
    // which nothing here controls — a runner-image change turned exactly
    // that assertion red on unchanged code (identical throughput, green
    // then red hours apart). The blocking property is proven
    // deterministically by OutputBudget's own unit tests in capsule_win.rs;
    // what this flood proves is what only e2e CAN prove: 20 MiB through a
    // real conhost with the budget's bookkeeping live, no deadlock, a
    // sealed verify-green voyage, and the bytes captured.
    assert!(summary.output_high_water_bytes > 0, "output budget never recorded any outstanding bytes");

    let frames = sealed_frames(&root, "flood1");
    let mut total_decoded = 0usize;
    for f in &frames {
        if f.class == Class::Producer {
            let b64 = f.payload.as_ref().unwrap()["bytes_b64"].as_str().unwrap();
            total_decoded += decode_b64(b64).len();
        }
    }
    assert!(
        total_decoded > total / 2,
        "captured far less output than the flood emitted: {total_decoded} of {total}"
    );
}

/// Test 6: a high-bit (NTSTATUS-shaped) exit code is preserved raw and
/// unsigned all the way through `ExitSummary` AND the sealed
/// `producer_dead` frame's JSON — the review finding that a `u32`-to-`i32`
/// cast anywhere in this path would turn it negative for no reason. Same
/// value `tests/conpty.rs` pins at the primitives layer; this proves the
/// capsule runtime doesn't reintroduce the cast above it.
#[test]
fn exit_code_high_bit_status_preserved_through_producer_dead() {
    let dir = tempfile::tempdir().unwrap();
    let argv =
        vec!["cmd.exe".to_string(), "/d".to_string(), "/c".to_string(), "exit -1073741819".to_string()];
    let cfg = config(dir.path(), "exitcode1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let summary = capsule_win::run(cfg, rx).unwrap();
    assert_eq!(summary.exit_code, Some(0xC000_0005));
    verify_voyage(&root, "exitcode1").unwrap();
    let frames = sealed_frames(&root, "exitcode1");
    let dead = assert_producer_dead_is_last(&frames);
    assert_eq!(dead["exit_code"], 0xC000_0005u32);
}
