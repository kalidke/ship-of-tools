use super::*;
use crate::sidecars::contract_tests::{pin_env, within};
use std::time::Duration;

/// A `julia` stand-in that says it is ready and answers each interrupt with a note holding a byte that is not UTF-8.
const STUB_BYTE: &str = r#"#!/bin/sh
printf '{"v":1,"id":0,"kind":"evt","op":"repl.ready","payload":{}}\n'
while read -r line; do
  id=$(printf '%s' "$line" | sed -e 's/.*"id":\([0-9]*\).*/\1/')
  case "$line" in
    *repl.interrupt*) printf '{"v":1,"id":%s,"kind":"res","op":"repl.interrupt","payload":{"interrupted":true,"note":"a\377b"}}\n' "$id" ;;
  esac
done
"#;

/// A REPL child's line may hold any byte: its answer arrives with U+FFFD for the byte, and the same child answers
/// the next request.
#[tokio::test]
async fn a_repl_line_holding_an_invalid_byte_keeps_the_child() {
    // Removed when dropped, after a failed assertion too.
    let tmp = tempfile::Builder::new().prefix("sot-repl-byte-").tempdir().unwrap();
    let dir = tmp.path();
    let resources = dir.join("resources");
    std::fs::create_dir_all(resources.join("julia").join("repl")).unwrap();
    let root = dir.join("row");
    std::fs::create_dir_all(&root).unwrap();
    let julia = dir.join("julia");
    sot_platform::test_exec::write_executable(&julia, STUB_BYTE);
    let _pin = pin_env(&julia, &resources);

    let (frame_tx, _bus) = tokio::sync::broadcast::channel(256);
    let workspaces = Workspaces::new();
    workspaces.set_repl_frame_tx(frame_tx);
    let session = Session::new();
    let ws = crate::rows::Workspace::from_label("byterow", root, false, "none".into(), String::new(), String::new());
    let id = ws.workspace_id.clone();
    workspaces.insert(ws);
    let row = row_or_reply(&workspaces, Some(&id), 0, op::REPL_EVAL).unwrap_or_else(|_| panic!("setup: the row"));
    handle_repl_eval(1, json!({ "workspace_id": id, "eval_id": 7, "code": "1" }), &session, &workspaces, &Default::default())
        .await
        .expect("setup: the eval starts the child");
    within(Duration::from_secs(30), "the child is ready", || row.repl_state() == "ready").await;

    let mut answers = Vec::new();
    for req in 2..4 {
        let out = handle_repl_interrupt(req, json!({ "workspace_id": id, "eval_ids": [7] }), &session, &workspaces, &Default::default())
            .await
            .expect("handler");
        answers.push(out[0].0.payload.clone());
    }
    let answered = json!({ "interrupted": true, "note": "a\u{fffd}b" });
    assert_eq!(answers, [answered.clone(), answered]);
}

/// A `julia` stand-in: it says it is ready, then logs each request line beside itself and answers it.
const STUB: &str = r#"#!/bin/sh
printf '{"v":1,"id":0,"kind":"evt","op":"repl.ready","payload":{}}\n'
while read -r line; do
  printf '%s\n' "$line" >> "$0.log"
  id=$(printf '%s' "$line" | sed -e 's/.*"id":\([0-9]*\).*/\1/')
  printf '{"v":1,"id":%s,"kind":"res","op":"repl.interrupt","payload":{"interrupted":true}}\n' "$id"
done
"#;

#[tokio::test]
async fn an_interrupt_reaches_the_child_with_the_evals_it_names() {
    // Removed when dropped, after a failed assertion too.
    let tmp = tempfile::Builder::new().prefix("sot-repl-interrupt-").tempdir().unwrap();
    let dir = tmp.path();
    let resources = dir.join("resources");
    std::fs::create_dir_all(resources.join("julia").join("repl")).unwrap();
    let root = dir.join("row");
    std::fs::create_dir_all(&root).unwrap();
    let julia = dir.join("julia");
    sot_platform::test_exec::write_executable(&julia, STUB);
    let _pin = pin_env(&julia, &resources);

    let (frame_tx, _bus) = tokio::sync::broadcast::channel(256);
    let workspaces = Workspaces::new();
    workspaces.set_repl_frame_tx(frame_tx);
    let session = Session::new();
    let ws = crate::rows::Workspace::from_label("introw", root, false, "none".into(), String::new(), String::new());
    let id = ws.workspace_id.clone();
    workspaces.insert(ws);
    let row = row_or_reply(&workspaces, Some(&id), 0, op::REPL_EVAL).unwrap_or_else(|_| panic!("setup: the row"));
    // A window's connection, as the roster makes one; the window redials before its Ctrl+C.
    let clients = crate::clients::Clients::new();
    let window = || clients.register("c", "0.6.6", 1, "fe".into(), Some("h".into()), Some("w".into()), None);
    let conn = window();
    handle_repl_eval(1, json!({ "workspace_id": id, "eval_id": 205, "code": "1" }), &session, &workspaces, &conn.numbers())
        .await
        .expect("setup: the eval starts the child");
    within(Duration::from_secs(30), "the child is ready", || row.repl_state() == "ready").await;
    drop(conn);
    let again = window();

    // The eval is named by the window's own number (205) and an announced run by the daemon's.
    let named = json!({ "workspace_id": id, "eval_ids": [205, 1u64 << 40] });
    let out = handle_repl_interrupt(2, named, &session, &workspaces, &again.numbers()).await.expect("handler");
    assert_eq!(out[0].0.payload, json!({ "interrupted": true }), "the child's answer is the reply");
    let logged: Vec<serde_json::Value> = std::fs::read_to_string(dir.join("julia.log"))
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let eval = logged.iter().find(|v| v["op"] == op::REPL_EVAL).expect("the eval reached the child");
    let number = eval["payload"]["eval_id"].as_u64().expect("the eval's number");
    assert!(number >= 1 << 40, "the child runs the eval under the daemon's number: {eval}");
    let interrupts: Vec<&serde_json::Value> = logged.iter().filter(|v| v["op"] == op::REPL_INTERRUPT).collect();
    assert_eq!(interrupts.len(), 1, "one interrupt reached the child: {logged:?}");
    let reached = json!({ "workspace_id": id, "eval_ids": [number, 1u64 << 40] });
    assert_eq!(interrupts[0]["payload"], reached, "the child is named the eval by the daemon's number");
}
