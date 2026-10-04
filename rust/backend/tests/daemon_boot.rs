#![cfg(any(windows, target_os = "linux"))]
//! The daemon's start-up, end to end: a first boot seeds the default row as the inert anchor,
//! and the comm-registry poll started at boot relays a change of an agent's state as a
//! `workspace.changed` evt. Every folder is the harness's own; the live comm home is never read.

mod support;

use std::time::{Duration, Instant};

use sot_protocol::{codec, op, Kind};
use support::{call, connect_and_hello, Env, BOUND};

/// Sets the one agent's `state` in the harness's registry; written to a temp file and renamed.
fn write_registry(env: &Env, state: &str) {
    let doc = serde_json::json!({ "agents": { "boot-probe": {
        "state": state, "summary": "", "status_at": "", "host": "h",
    } } });
    let tmp = env.comm_root.join("registry.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&doc).expect("encode")).expect("write registry tmp");
    std::fs::rename(&tmp, env.comm_root.join("registry.json")).expect("rename registry");
}

#[tokio::test]
async fn boot_seeds_the_anchor_and_relays_registry_changes() {
    let env = Env::new("boot");
    write_registry(&env, "idle");
    env.spawn_sotd();
    let (mut conn, next_id) = connect_and_hello(&env.socket_path).await;

    let list = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    let defaults: Vec<&serde_json::Value> = list["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .filter(|w| w["is_default"].as_bool() == Some(true))
        .collect();
    assert_eq!(defaults.len(), 1, "exactly one default row: {list}");
    assert_eq!(defaults[0]["agent"], "none", "the default row is the inert anchor: {list}");
    assert_eq!(defaults[0]["runtime"], "capsule", "the default row runs as a capsule: {list}");

    // A reader task owns the connection: `read_frame` is not cancel-safe, so the loop below
    // waits on a channel, never on the read itself.
    let (evt_tx, mut evt_rx) = tokio::sync::mpsc::unbounded_channel();
    let reader = tokio::spawn(async move {
        while let Ok((frame, _blob)) = codec::read_frame(&mut conn).await {
            if evt_tx.send(frame).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + BOUND;
    let mut state = "idle";
    loop {
        assert!(Instant::now() < deadline, "no agent_state workspace.changed evt within {BOUND:?}");
        state = if state == "idle" { "working" } else { "idle" };
        write_registry(&env, state);
        let until = Instant::now() + Duration::from_secs(2);
        let mut seen = false;
        while let Ok(Some(frame)) = tokio::time::timeout_at(until.into(), evt_rx.recv()).await {
            if frame.kind == Kind::Evt
                && frame.op == op::WORKSPACE_CHANGED
                && frame.payload["action"] == "agent_state"
            {
                seen = true;
                break;
            }
        }
        if seen {
            break;
        }
    }
    reader.abort();
    env.kill_daemon_bounded().await;
}
