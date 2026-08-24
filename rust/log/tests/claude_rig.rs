#![cfg(all(unix, target_os = "linux"))]
//! Conformance rig for the Claude adapter (ADR 0040 §Gates): a scripted
//! FAKE HELPER (bash, speaking helper-protocol 1) drives the real adapter
//! through the turn table, WAL, redaction, terminal, and successor-closure
//! paths. The kill-domain fence is exercised only where cgroup delegation
//! exists (the adapter itself fails closed in production without it); these
//! tests use the explicit test-only unfenced mode — protocol logic, not
//! kill-domain, is under test here. The real-helper + pinned-SDK fixtures
//! ride the helper package's own suite.

use sot_log::claude::{run, ClaudeConfig, Fence, OperatorCmd};
use sot_log::segment::{RetentionClass, SegmentReader};
use sot_log::verify::verify_voyage;
use sot_log::{Class, RefKind};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

fn write_fake_helper(dir: &Path, scenario: &str) -> PathBuf {
    let body = match scenario {
        "happy" => r#"#!/bin/bash
echo '{"ev":"hello","protocol":1,"sdk_version":"fake-0"}'
while IFS= read -r line; do
  case "$line" in
    *'"query_id":1'*)
      echo '{"ev":"msg","body":{"type":"system","subtype":"init","session_id":"s1"}}'
      echo '{"ev":"msg","body":{"type":"assistant","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"tu1","name":"Read","input":{}}]},"session_id":"s1"}}'
      echo '{"ev":"msg","body":{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","content":"ok"}]},"session_id":"s1"}}'
      echo '{"ev":"msg","body":{"type":"result","subtype":"success","total_cost_usd":0.0123,"session_id":"s1"}}'
      echo '{"ev":"turn_end","query_id":1}' ;;
    *'"query_id":2'*)
      echo '{"ev":"msg","body":{"type":"assistant","message":{"content":[{"type":"text","text":"again"}]},"session_id":"s1"}}'
      echo '{"ev":"msg","body":{"type":"result","subtype":"success","session_id":"s1"}}'
      echo '{"ev":"turn_end","query_id":2}' ;;
    *'"op":"shutdown"'*) exit 0 ;;
  esac
done
"#,
        "echo_secret" => r#"#!/bin/bash
echo '{"ev":"hello","protocol":1,"sdk_version":"fake-0"}'
while IFS= read -r line; do
  case "$line" in
    *'"query_id":1'*)
      echo '{"ev":"msg","body":{"type":"system","subtype":"init","session_id":"s1"}}'
      echo '{"ev":"msg","body":{"type":"user","message":{"content":[{"type":"text","text":"CANARY-hunter2-SECRET"}]},"session_id":"s1"}}'
      echo '{"ev":"msg","body":{"type":"result","subtype":"success","session_id":"s1"}}'
      echo '{"ev":"turn_end","query_id":1}' ;;
    *'"op":"shutdown"'*) exit 0 ;;
  esac
done
"#,
        "unknown_type" => r#"#!/bin/bash
echo '{"ev":"hello","protocol":1,"sdk_version":"fake-0"}'
while IFS= read -r line; do
  case "$line" in
    *'"query_id":1'*)
      echo '{"ev":"msg","body":{"type":"martian","payload":"???"}}'
      echo '{"ev":"msg","body":{"type":"result","subtype":"success","session_id":"s1"}}'
      echo '{"ev":"turn_end","query_id":1}' ;;
  esac
done
"#,
        "interrupt" => r#"#!/bin/bash
echo '{"ev":"hello","protocol":1,"sdk_version":"fake-0"}'
while IFS= read -r line; do
  case "$line" in
    *'"query_id":1'*)
      echo '{"ev":"msg","body":{"type":"system","subtype":"init","session_id":"s1"}}' ;;
    *'"op":"interrupt"'*)
      echo '{"ev":"interrupted","id":1,"ok":true,"sdk_return":null,"note":"adapter-derived"}'
      echo '{"ev":"msg","body":{"type":"result","subtype":"error_during_execution","session_id":"s1"}}'
      echo '{"ev":"turn_end","query_id":1}' ;;
    *'"op":"shutdown"'*) exit 0 ;;
  esac
done
"#,
        "bad_version" => r#"#!/bin/bash
echo '{"ev":"hello","protocol":1,"sdk_version":"evil-9"}'
read -r _line
"#,
        "interrupt_no_result" => r#"#!/bin/bash
echo '{"ev":"hello","protocol":1,"sdk_version":"fake-0"}'
while IFS= read -r line; do
  case "$line" in
    *'"query_id":1'*)
      echo '{"ev":"msg","body":{"type":"system","subtype":"init","session_id":"s1"}}' ;;
    *'"op":"interrupt"'*)
      echo '{"ev":"interrupted","id":1,"ok":true,"sdk_return":null,"note":"adapter-derived"}'
      echo '{"ev":"turn_end","query_id":1,"results":0}' ;;
    *'"op":"shutdown"'*) exit 0 ;;
  esac
done
"#,
        "double_result" => r#"#!/bin/bash
echo '{"ev":"hello","protocol":1,"sdk_version":"fake-0"}'
while IFS= read -r line; do
  case "$line" in
    *'"query_id":1'*)
      echo '{"ev":"msg","body":{"type":"result","subtype":"success","session_id":"s1"}}'
      echo '{"ev":"msg","body":{"type":"result","subtype":"success","session_id":"s1"}}'
      echo '{"ev":"turn_end","query_id":1}' ;;
  esac
done
"#,
        "die_mid_turn" => r#"#!/bin/bash
echo '{"ev":"hello","protocol":1,"sdk_version":"fake-0"}'
while IFS= read -r line; do
  case "$line" in
    *'"query_id":1'*)
      echo '{"ev":"msg","body":{"type":"assistant","message":{"content":[{"type":"text","text":"working"}]},"session_id":"s1"}}'
      exit 7 ;;
  esac
done
"#,
        other => panic!("unknown scenario {other}"),
    };
    let path = dir.join(format!("fake-helper-{scenario}.sh"));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    drop(f);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn config(root: &Path, name: &str, helper: &Path) -> ClaudeConfig {
    ClaudeConfig {
        voyage_root: root.join(name),
        voyage_id: name.into(),
        retention: RetentionClass::Discard,
        helper_argv: vec![helper.to_string_lossy().into_owned()],
        expected_sdk_version: "fake-0".into(),
        fence: Fence::test_unfenced(),
    }
}

fn all_sealed_frames(root: &Path) -> Vec<sot_log::Envelope> {
    let seg_dir = root.join("seg");
    let mut names: Vec<String> = std::fs::read_dir(&seg_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".sotseg"))
        .collect();
    names.sort();
    let mut out = vec![];
    for n in names {
        out.extend(SegmentReader::read(&seg_dir.join(n), true).unwrap().frames);
    }
    out
}

/// Drive run() on a thread; feed commands with settling delays. The rig's
/// scenarios are TIMING-based (fixed settling sleeps), so they serialize
/// through one lock — parallel test scheduling starved the sleeps and
/// produced order-dependent flakes.
fn drive(cfg: ClaudeConfig, cmds: Vec<(u64, OperatorCmd)>) -> sot_log::claude::ClaudeSummary {
    static RIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = RIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || run(cfg, rx).unwrap());
    for (delay_ms, cmd) in cmds {
        std::thread::sleep(Duration::from_millis(delay_ms));
        let _ = tx.send(cmd);
    }
    drop(tx);
    handle.join().unwrap()
}

#[test]
fn happy_two_turns_verify_green() {
    let dir = tempfile::tempdir().unwrap();
    let helper = write_fake_helper(dir.path(), "happy");
    let cfg = config(dir.path(), "v1", &helper);
    let root = cfg.voyage_root.clone();
    let summary = drive(cfg, vec![
        (150, OperatorCmd::Turn("first".into())),
        (300, OperatorCmd::Turn("second".into())),
        (300, OperatorCmd::Shutdown),
    ]);
    assert_eq!(summary.turns, 2, "terminal: {}", summary.terminal_reason);
    assert_eq!(summary.terminal_reason, "shutdown");
    verify_voyage(&root, "v1").unwrap();

    let frames = all_sealed_frames(&root);
    let opens: Vec<_> = frames.iter().filter(|f| f.class == Class::TurnOpen).collect();
    let closes: Vec<_> = frames.iter().filter(|f| f.class == Class::TurnClose).collect();
    assert_eq!(opens.len(), 2);
    assert_eq!(closes.len(), 2);
    // Every open responds_to its input; every close producer_done.
    for o in &opens {
        assert!(o.refs.iter().any(|r| r.kind == RefKind::RespondsTo));
    }
    for c in &closes {
        assert_eq!(c.payload.as_ref().unwrap()["reason"], "producer_done");
    }
    // The tool_result user message was attributed to turn 1 via the index
    // (its caused_by == the first open), not treated as an echo.
    let tool_result_frame = frames
        .iter()
        .find(|f| {
            f.class == Class::Producer
                && f.payload.as_ref().and_then(|p| p.pointer("/message/content/0/tool_use_id")).is_some()
        })
        .expect("tool_result frame present");
    let t1 = opens[0].seq;
    assert!(tool_result_frame.refs.iter().any(|r| r.kind == RefKind::CausedBy && r.frame == t1));
    assert!(tool_result_frame.transformed.is_none(), "tool_result is work product, not echo");
    // Fractional cost survived under the f64 feature.
    let result_frame = frames
        .iter()
        .find(|f| f.class == Class::Producer && f.payload.as_ref().unwrap()["type"] == "result"
              && f.payload.as_ref().unwrap().get("total_cost_usd").is_some())
        .unwrap();
    assert_eq!(result_frame.payload.as_ref().unwrap()["total_cost_usd"], 0.0123);
}

#[test]
fn operator_echo_is_redacted_and_no_canary_in_voyage_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let helper = write_fake_helper(dir.path(), "echo_secret");
    let cfg = config(dir.path(), "v2", &helper);
    let root = cfg.voyage_root.clone();
    let summary = drive(cfg, vec![
        (150, OperatorCmd::Turn("CANARY-hunter2-SECRET".into())),
        (300, OperatorCmd::Shutdown),
    ]);
    assert_eq!(summary.turns, 1, "terminal: {}", summary.terminal_reason);
    verify_voyage(&root, "v2").unwrap();
    // The echo frame exists, transform-marked, turn-free.
    let frames = all_sealed_frames(&root);
    let echo = frames
        .iter()
        .find(|f| f.class == Class::Producer && f.transformed.is_some()
              && f.payload.as_ref().unwrap()["type"] == "user")
        .expect("redacted echo frame retained");
    assert!(!echo.refs.iter().any(|r| r.kind == RefKind::CausedBy));
    // Canary byte-scan of the ENTIRE voyage tree (capture-off gate).
    fn scan(p: &Path, needle: &[u8]) -> bool {
        for e in std::fs::read_dir(p).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                if scan(&e.path(), needle) {
                    return true;
                }
            } else if std::fs::read(e.path()).unwrap().windows(needle.len()).any(|w| w == needle) {
                return true;
            }
        }
        false
    }
    assert!(!scan(&root, b"hunter2"), "canary leaked into voyage bytes");
}

#[test]
fn unknown_type_drains_then_terminates() {
    let dir = tempfile::tempdir().unwrap();
    let helper = write_fake_helper(dir.path(), "unknown_type");
    let cfg = config(dir.path(), "v3", &helper);
    let root = cfg.voyage_root.clone();
    let summary = drive(cfg, vec![(150, OperatorCmd::Turn("go".into()))]);
    assert!(summary.terminal_reason.contains("unknown producer message type"),
        "got: {}", summary.terminal_reason);
    assert_eq!(summary.turns, 1, "the in-flight turn drains to its result first");
    verify_voyage(&root, "v3").unwrap(); // complete: the turn closed properly
}

#[test]
fn interrupt_three_frame_exchange() {
    let dir = tempfile::tempdir().unwrap();
    let helper = write_fake_helper(dir.path(), "interrupt");
    let cfg = config(dir.path(), "v4", &helper);
    let root = cfg.voyage_root.clone();
    let summary = drive(cfg, vec![
        (150, OperatorCmd::Turn("long".into())),
        (200, OperatorCmd::Interrupt),
        (300, OperatorCmd::Shutdown),
    ]);
    assert_eq!(summary.turns, 1, "terminal: {}", summary.terminal_reason);
    verify_voyage(&root, "v4").unwrap();
    let frames = all_sealed_frames(&root);
    let req = frames.iter().find(|f| f.class == Class::ControlExchange
        && f.payload.as_ref().unwrap()["phase"] == "request").expect("request frame");
    let resp = frames.iter().find(|f| f.class == Class::ControlExchange
        && f.payload.as_ref().unwrap()["phase"] == "response").expect("response frame");
    assert!(resp.refs.iter().any(|r| r.kind == RefKind::RespondsTo && r.frame == req.seq));
    assert_eq!(resp.payload.as_ref().unwrap()["body"]["note"], "adapter-derived");
    // The interrupted turn closed failed (error_during_execution result).
    let close = frames.iter().find(|f| f.class == Class::TurnClose).unwrap();
    assert_eq!(close.payload.as_ref().unwrap()["reason"], "failed");
}

#[test]
fn helper_death_mid_turn_closes_synthesized_and_successor_run_is_green() {
    let dir = tempfile::tempdir().unwrap();
    let helper = write_fake_helper(dir.path(), "die_mid_turn");
    let cfg = config(dir.path(), "v5", &helper);
    let root = cfg.voyage_root.clone();
    let summary = drive(cfg, vec![(150, OperatorCmd::Turn("doomed".into()))]);
    assert!(summary.terminal_reason.contains("helper died mid-turn"),
        "got: {}", summary.terminal_reason);
    verify_voyage(&root, "v5").unwrap(); // the adapter closed the turn at death
    let frames = all_sealed_frames(&root);
    let close = frames.iter().find(|f| f.class == Class::TurnClose).unwrap();
    assert_eq!(close.payload.as_ref().unwrap()["reason"], "synthesized_death");

    // Second incarnation on the SAME voyage, happy scenario: opens cleanly,
    // runs a turn, verify stays green across the epochs.
    let helper2 = write_fake_helper(dir.path(), "happy");
    let cfg2 = ClaudeConfig {
        voyage_root: root.clone(),
        voyage_id: "v5".into(),
        retention: RetentionClass::Discard,
        helper_argv: vec![helper2.to_string_lossy().into_owned()],
        expected_sdk_version: "fake-0".into(),
        fence: Fence::test_unfenced(),
    };
    let s2 = drive(cfg2, vec![
        (150, OperatorCmd::Turn("recovered".into())),
        (300, OperatorCmd::Shutdown),
    ]);
    assert_eq!(s2.turns, 1, "terminal: {}", s2.terminal_reason);
    verify_voyage(&root, "v5").unwrap();
}

#[test]
fn sdk_version_mismatch_is_terminal_attestation() {
    let dir = tempfile::tempdir().unwrap();
    let helper = write_fake_helper(dir.path(), "bad_version");
    let cfg = config(dir.path(), "v6", &helper);
    let root = cfg.voyage_root.clone();
    let summary = drive(cfg, vec![(200, OperatorCmd::Shutdown)]);
    assert!(summary.terminal_reason.contains("sdk version mismatch"),
        "got: {}", summary.terminal_reason);
    verify_voyage(&root, "v6").unwrap();
}

#[test]
fn interrupted_query_without_result_closes_interrupted_with_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let helper = write_fake_helper(dir.path(), "interrupt_no_result");
    let cfg = config(dir.path(), "v7", &helper);
    let root = cfg.voyage_root.clone();
    let summary = drive(cfg, vec![
        (150, OperatorCmd::Turn("long".into())),
        (200, OperatorCmd::Interrupt),
        (300, OperatorCmd::Shutdown),
    ]);
    assert_eq!(summary.turns, 1, "terminal: {}", summary.terminal_reason);
    verify_voyage(&root, "v7").unwrap();
    let frames = all_sealed_frames(&root);
    let close = frames.iter().find(|f| f.class == Class::TurnClose).unwrap();
    assert_eq!(close.payload.as_ref().unwrap()["reason"], "interrupted");
    let outcome = frames.iter().find(|f| f.class == Class::ControlExchange
        && f.payload.as_ref().unwrap()["phase"] == "outcome").expect("outcome frame");
    assert_eq!(outcome.payload.as_ref().unwrap()["scope"], "turn");
    assert!(outcome.payload.as_ref().unwrap()["target"].as_str().unwrap().contains(':'));
    assert!(!outcome.refs.iter().any(|r| r.kind == RefKind::RespondsTo));
}

#[test]
fn double_result_is_terminal_and_log_stays_verify_green() {
    let dir = tempfile::tempdir().unwrap();
    let helper = write_fake_helper(dir.path(), "double_result");
    let cfg = config(dir.path(), "v8", &helper);
    let root = cfg.voyage_root.clone();
    let summary = drive(cfg, vec![(150, OperatorCmd::Turn("go".into()))]);
    assert!(summary.terminal_reason.contains("second result"),
        "got: {}", summary.terminal_reason);
    // The invariant under attack (review B3): a hostile helper must never
    // make the capsule write a log that fails verify.
    verify_voyage(&root, "v8").unwrap();
}

#[test]
fn refused_turn_is_recorded_as_bare_input() {
    let dir = tempfile::tempdir().unwrap();
    // interrupt_no_result holds the first turn OPEN until interrupted, so
    // the second Turn is deterministically refused (no timing race).
    let helper = write_fake_helper(dir.path(), "interrupt_no_result");
    let cfg = config(dir.path(), "v9", &helper);
    let root = cfg.voyage_root.clone();
    let summary = drive(cfg, vec![
        (150, OperatorCmd::Turn("first".into())),
        (150, OperatorCmd::Turn("refused-while-busy".into())),
        (150, OperatorCmd::Interrupt),
        (300, OperatorCmd::Shutdown),
    ]);
    assert_eq!(summary.turns, 1, "terminal: {}", summary.terminal_reason);
    assert_eq!(summary.refused_turns, 1);
    verify_voyage(&root, "v9").unwrap();
    // Two input frames exist; exactly one has a forward_intent behind it.
    let frames = all_sealed_frames(&root);
    let inputs = frames.iter().filter(|f| f.class == Class::Input).count();
    let intents = frames.iter().filter(|f| f.class == Class::Lifecycle
        && f.payload.as_ref().unwrap().pointer("/fact/fact") == Some(&serde_json::json!("forward_intent"))).count();
    assert_eq!(inputs, 2);
    assert_eq!(intents, 1);
}
