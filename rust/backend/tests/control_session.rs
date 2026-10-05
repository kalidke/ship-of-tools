#![cfg(any(windows, target_os = "linux"))]
//! A control session's replies, pinned through a real `sotd`: the op table's unknown-op answer, the
//! `monitor.*` and `pty.open` arms that answer inline, the four off-loop ops, `workspace.activate`,
//! the evt-frame skip, the `ping` a connection with no hello is refused, the refused hellos (another protocol, one
//! that does not parse), which close their connection and never enter the roster. Every request is read strictly:
//! its reply is the next non-evt frame and no second reply follows it.

mod support;

use serde_json::json;
use sot_protocol::{codec, op, Frame, HelloReq, Kind};
use std::time::Duration;
use support::{connect_and_hello, poll_until, try_connect, Conn, Env, BOUND};

/// How long a request's connection must stay silent after its one reply: a second reply, for the same
/// request, would arrive within it.
const QUIET: Duration = Duration::from_millis(100);

/// A fresh connection whose first frame is `frame`: the one reply, which must carry the frame's id, and then the
/// daemon's end of the connection (the admission, ADR 0049 `## User isolation`: a refused first frame is one reply and
/// a close).
async fn refused_first_frame(env: &Env, frame: Frame) -> serde_json::Value {
    let stream = poll_until(|| async { try_connect(&env.socket_path).await }, BOUND, "a refused connection").await;
    let mut conn = tokio::io::BufReader::new(stream);
    codec::write_frame(&mut conn, &frame, None).await.expect("write the first frame");
    let (reply, _blob) = tokio::time::timeout(BOUND, codec::read_frame(&mut conn))
        .await
        .expect("the refusal did not arrive")
        .expect("read the refusal");
    assert_eq!((reply.id, reply.kind), (frame.id, Kind::Res), "{reply:?}");
    let end = tokio::time::timeout(BOUND, codec::read_frame(&mut conn)).await.expect("the refused connection stayed open");
    assert!(end.is_err(), "a refused first frame is followed by the end of the connection, got {end:?}");
    reply.payload
}

/// Writes one request, reads the next frame that is not an evt, which must carry that request's id, and
/// then requires nothing but evt frames for `QUIET`. (`support::call` skips other ids, so it cannot see
/// a second reply.)
async fn strict(conn: &mut Conn, id: u64, name: &str, payload: serde_json::Value) -> Frame {
    codec::write_frame(conn, &Frame::req(id, name, payload), None).await.expect("write request");
    let reply = loop {
        let (frame, _blob) = tokio::time::timeout(BOUND, codec::read_frame(conn))
            .await
            .unwrap_or_else(|_| panic!("{name} (id {id}) did not reply within {BOUND:?}"))
            .expect("read_frame");
        if frame.kind != Kind::Evt {
            break frame;
        }
    };
    assert_eq!((reply.id, reply.kind), (id, Kind::Res), "the next reply to {name} (id {id}) is not its own: {reply:?}");
    let until = tokio::time::Instant::now() + QUIET;
    while let Ok(read) = tokio::time::timeout_at(until, codec::read_frame(conn)).await {
        let (frame, _blob) = read.expect("read_frame");
        assert_eq!(frame.kind, Kind::Evt, "a second reply after {name}'s (id {id}): {frame:?}");
    }
    reply
}

#[tokio::test]
#[allow(clippy::too_many_lines, reason = "one test table: every control.session reply checked against its pinned shape")]
async fn control_session_replies_are_pinned() {
    let env = Env::new("ctl");
    env.spawn_sotd();
    let (mut conn, mut id) = connect_and_hello(&env.socket_path).await;
    let mut next = || {
        id += 1;
        id - 1
    };

    // 1. A frame of kind evt is skipped without a reply; a ping before any hello is refused and closes.
    codec::write_frame(&mut conn, &Frame::evt("probe.evt", json!({})), None).await.expect("write evt");
    let ping_id = next();
    let reply = strict(&mut conn, ping_id, op::PING, json!({})).await;
    assert_eq!((reply.op.as_str(), reply.kind), (op::PING, Kind::Res));
    assert_eq!(reply.payload, json!({"ok": true}));
    let refusal = refused_first_frame(&env, Frame::req(1, op::PING, json!({}))).await;
    assert_eq!(refusal["code"], "unauthenticated", "a request before any hello is refused: {refusal:?}");

    // 2. An op nobody owns.
    let reply = strict(&mut conn, next(), "no.such.op", json!({})).await;
    assert_eq!(reply.op, "no.such.op");
    assert_eq!(reply.payload, json!({"error": "unknown op: no.such.op"}));

    // 3. monitor.*
    let reply = strict(&mut conn, next(), op::MONITOR_SUBSCRIBE, json!({})).await;
    assert_eq!(reply.payload["interval_s"], json!(1.0), "{:?}", reply.payload);
    assert!(reply.payload["hosts"].is_array(), "{:?}", reply.payload);
    let reply = strict(&mut conn, next(), op::MONITOR_HISTORY, json!({"window_s": 60})).await;
    assert!(reply.payload["hosts"].is_array(), "{:?}", reply.payload);
    let reply = strict(&mut conn, next(), op::MONITOR_HISTORY, json!({"window_s": "x"})).await;
    assert_eq!(reply.payload["code"], "handler_error", "{:?}", reply.payload);
    assert!(
        reply.payload["error"].as_str().is_some_and(|e| e.starts_with("monitor.history payload")),
        "{:?}",
        reply.payload
    );
    let reply = strict(&mut conn, next(), op::MONITOR_UNSUBSCRIBE, json!({})).await;
    assert_eq!(reply.payload, json!({}));

    // 4. pty.open's refusals.
    let reply = strict(&mut conn, next(), op::PTY_OPEN, json!({"rows": 24})).await;
    assert_eq!(reply.payload["code"], "bad_request", "{:?}", reply.payload);
    assert!(
        reply.payload["error"].as_str().is_some_and(|e| e.starts_with("pty.open payload: ")),
        "{:?}",
        reply.payload
    );
    let reply = strict(&mut conn, next(), op::PTY_OPEN, json!({"cols": 80, "rows": 24, "target": "a|b"})).await;
    assert_eq!(
        reply.payload,
        json!({"error": "invalid target \"a|b\" (want 1-64 chars of [A-Za-z0-9._-])", "code": "bad_target"})
    );
    let reply = strict(&mut conn, next(), op::PTY_OPEN, json!({"cols": 80, "rows": 24, "target": "nosuch"})).await;
    assert_eq!(reply.payload, json!({"error": "no workspace owns session \"nosuch\"", "code": "no_workspace"}));

    // 5. The four off-loop ops, on an unknown row and on the default row.
    let list = strict(&mut conn, next(), op::WORKSPACE_LIST, json!({})).await.payload;
    let default_id = list["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .find(|w| w["is_default"].as_bool() == Some(true))
        .and_then(|w| w["workspace_id"].as_str())
        .expect("the default row")
        .to_string();
    let ops = [
        (op::PREVIEW_GET, "preview.get payload: missing field `node_id`"),
        (op::IMAGE_CROP, "image.crop payload: missing field `node_id`"),
        (op::KERNEL_REQUEST, "kernel.request payload: missing field `kernel_op`"),
        (op::CONCEPT_READ, "concept.read payload: missing field `target`"),
    ];
    for (name, default_row_error) in ops {
        let reply = strict(&mut conn, next(), name, json!({"workspace_id": "ws-nosuch"})).await;
        assert_eq!(reply.op, name);
        assert_eq!(
            reply.payload,
            json!({"error": "unknown workspace: Some(\"ws-nosuch\")", "code": "unknown_workspace"}),
            "{name}"
        );
        let reply = strict(&mut conn, next(), name, json!({"workspace_id": default_id})).await;
        assert_eq!(reply.op, name);
        assert_eq!(
            reply.payload,
            json!({"error": default_row_error, "code": "handler_error"}),
            "{name} on the default row"
        );
    }

    // 6. workspace.activate on an unknown row.
    let reply = strict(&mut conn, next(), op::WORKSPACE_ACTIVATE, json!({"workspace_id": "ws-nosuch"})).await;
    assert_eq!(reply.payload, json!({}));

    // 7. A hello on another protocol is refused, closes its connection and never enters the roster.
    let hello = HelloReq {
        protocol: sot_protocol::PROTOCOL_VERSION + 1,
        ..HelloReq::this_process("mismatch-probe", "", Some("host-a".to_string())).expect("this process's account")
    };
    let refusal = refused_first_frame(&env, Frame::req(1, op::HELLO, serde_json::to_value(&hello).unwrap())).await;
    assert_eq!(refusal["code"], "protocol_mismatch", "{refusal:?}");
    let reply = strict(&mut conn, next(), op::VERSION_QUERY, json!({})).await;
    let ids: Vec<&str> = reply.payload["clients"]
        .as_array()
        .expect("clients array")
        .iter()
        .filter_map(|c| c["client_id"].as_str())
        .collect();
    assert!(ids.contains(&"capsule-workspaces-test"), "the helloed connection is listed: {ids:?}");
    assert!(!ids.contains(&"mismatch-probe"), "a refused hello never enters the roster: {ids:?}");

    // 8. A hello whose payload does not parse is `unauthenticated` and closes its connection, and never enters the
    // roster; a valid hello on a new connection then does.
    let listed = |reply: &Frame| -> Vec<String> {
        reply.payload["clients"]
            .as_array()
            .expect("clients array")
            .iter()
            .filter_map(|c| c["client_id"].as_str().map(str::to_string))
            .collect()
    };
    let before = listed(&strict(&mut conn, next(), op::VERSION_QUERY, json!({})).await);
    let refusal = refused_first_frame(&env, Frame::req(1, op::HELLO, json!({"client_id": 5}))).await;
    assert_eq!(refusal["code"], "unauthenticated", "{refusal:?}");
    assert!(
        refusal["error"].as_str().is_some_and(|e| e.starts_with("the hello payload does not parse")),
        "{refusal:?}"
    );
    let after_bad = listed(&strict(&mut conn, next(), op::VERSION_QUERY, json!({})).await);
    assert_eq!(after_bad, before, "an unparsable hello never enters the roster");
    let stream = poll_until(|| async { try_connect(&env.socket_path).await }, BOUND, "a fourth connection").await;
    let mut good = tokio::io::BufReader::new(stream);
    let hello = HelloReq::this_process("mw18-after-bad", "", Some("host-a".to_string())).expect("this process's account");
    let reply = strict(&mut good, 1, op::HELLO, serde_json::to_value(&hello).unwrap()).await;
    assert!(reply.payload.get("code").is_none(), "a valid hello is answered: {:?}", reply.payload);
    assert!(reply.payload["session_id"].is_string(), "{:?}", reply.payload);
    let after_good = listed(&strict(&mut conn, next(), op::VERSION_QUERY, json!({})).await);
    assert!(after_good.iter().any(|i| i == "mw18-after-bad"), "the valid hello is listed: {after_good:?}");
    drop(good);
    env.kill_daemon_bounded().await;
}
