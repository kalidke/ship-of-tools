//! `lane.connect` helpers and tests.

use super::*;

// --- ADR 0045 decision 2 (lane B3): `lane.connect`, the daemon-side lane
// bridge --- //

/// The raw bytes of a `lane.connect` request frame — split out of
/// [`lane_connect`] so a caller that needs to prove peek-buffer
/// preservation (Codex review SHOULD-FIX, 2026-09-11: the daemon's own
/// `read_frame` peek must consume EXACTLY this envelope and leave
/// whatever follows untouched for the raw pipe) can concatenate it with
/// the FIRST lane bytes and write both in ONE call, before ever reading
/// a reply.
#[cfg(target_os = "linux")]
fn lane_connect_envelope_bytes(target: &str, lane: &str, voyage_id: Option<&str>) -> Vec<u8> {
    let mut req = serde_json::json!({ "target": target, "lane": lane });
    if let Some(v) = voyage_id {
        req["voyage_id"] = serde_json::json!(v);
    }
    let mut bytes = serde_json::to_vec(&Frame::req(1, op::LANE_CONNECT, req)).expect("serialize lane.connect envelope");
    bytes.push(b'\n');
    bytes
}

/// A FRESH connection to `env.socket_path` whose only frame is
/// `lane.connect` (ADR 0045 decision 2: a dedicated connection, never
/// through `connect_and_hello`'s multiplexed control loop). Returns the
/// still-open connection — a `Conn` so a raw byte read/write afterward
/// (on success) shares the SAME `BufReader` the response frame was read
/// through, never a throwaway second reader that would lose whatever
/// piped bytes it already buffered past the envelope — alongside the
/// parsed response payload.
#[cfg(target_os = "linux")]
async fn lane_connect(env: &Env, target: &str, lane: &str, voyage_id: Option<&str>) -> (Conn, serde_json::Value) {
    lane_connect_with_payload(env, target, lane, voyage_id, &[]).await
}

/// [`lane_connect`]'s own general form: the connect envelope AND
/// `extra_payload` (raw bytes meant for the lane, once piped) are
/// written in ONE `write_all` call, BEFORE this function ever reads
/// anything back — the peek-buffer-preservation proof. Still returns
/// only after the `LaneConnectRes` frame itself has been read (through
/// the SAME `BufReader` the caller goes on to read any piped reply
/// from), exactly like [`lane_connect`].
#[cfg(target_os = "linux")]
async fn lane_connect_with_payload(
    env: &Env,
    target: &str,
    lane: &str,
    voyage_id: Option<&str>,
    extra_payload: &[u8],
) -> (Conn, serde_json::Value) {
    use tokio::io::AsyncWriteExt;
    let stream = poll_until(
        || async { try_connect(&env.socket_path).await },
        BOUND,
        "sotd's local socket to accept a connection",
    )
    .await;
    let mut conn = tokio::io::BufReader::new(stream);
    let mut combined = lane_connect_envelope_bytes(target, lane, voyage_id);
    combined.extend_from_slice(extra_payload);
    conn.write_all(&combined).await.expect("write lane.connect envelope (+ payload) in ONE call");
    let (frame, _blob) = tokio::time::timeout(BOUND, codec::read_frame(&mut conn))
        .await
        .unwrap_or_else(|_| panic!("lane.connect reply did not arrive within {BOUND:?}"))
        .expect("read_frame lane.connect reply");
    (conn, frame.payload)
}

/// A refused `lane.connect` closes the connection (ADR 0045 decision 2:
/// "Either direction closing ends both") — the next read observes
/// ordered EOF, never a hang.
#[cfg(target_os = "linux")]
async fn assert_lane_connect_closes(conn: &mut Conn) {
    use tokio::io::AsyncReadExt;
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(BOUND, conn.read(&mut buf))
        .await
        .expect("read after a lane.connect refusal within BOUND")
        .expect("read after a lane.connect refusal");
    assert_eq!(n, 0, "the connection must close (EOF) after a lane.connect refusal");
}

/// On a ready row, `lane: "supervisor"` pipes the real supervisor lane:
/// `Hello` AND `Status` written in ONE write call (the buffered-bytes
/// proof that the daemon never decodes a lane frame after its own reply
/// — it is a raw pipe, not a second parser) come back as `HelloOk` (own
/// pid matching `lane.connect`'s own report) and `StatusOk{phase:
/// Ready}`. Dropping the pipe releases the lane slot without disturbing
/// the row: `workspace.list` still reports "ready" afterward.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn lane_connect_supervisor_pipes_hello_and_status() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("lch");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "lch-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["session_name"].as_str().expect("session_name").to_string();

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;

    // Codex review SHOULD-FIX (2026-09-11): the connect envelope AND the
    // first lane bytes (Hello + Status) are written in ONE call, BEFORE
    // this test ever reads the LaneConnectRes reply -- the real
    // peek-buffer-preservation proof (the daemon's own `read_frame` peek
    // must consume EXACTLY the envelope and hand everything past it,
    // untouched, to the raw pipe; sending it only AFTER reading the
    // reply would never exercise that).
    let mut payload = sot_log::lane::wire::encode_supervisor_request(&sot_log::lane::wire::SupervisorRequest::Hello {
        proto: sot_log::lane::wire::SUPERVISOR_PROTO_V1,
        build: sot_log::identity::exchange::SUPERVISOR_LANE_BUILD_ID.to_string(),
    })
    .expect("encode hello");
    payload.extend(
        sot_log::lane::wire::encode_supervisor_request(&sot_log::lane::wire::SupervisorRequest::Status).expect("encode status"),
    );
    let (mut lane_conn, res) = lane_connect_with_payload(&env, &target, "supervisor", None, &payload).await;
    assert!(res.get("error").is_none(), "lane.connect refused: {res:?}");
    assert_eq!(res["ok"].as_bool(), Some(true), "lane.connect payload: {res:?}");
    let pid = res["pid"].as_u64().expect("pid");
    let created = res["created"].as_u64().expect("created");
    assert!(pid > 0, "pid must be a real process id: {res:?}");

    use tokio::io::AsyncReadExt;
    let mut splitter = sot_log::lane::wire::FrameSplitter::new();
    let mut got_hello: Option<(u32, u64)> = None;
    let mut got_status: Option<sot_log::lane::wire::SupervisorPhase> = None;
    let deadline = Instant::now() + BOUND;
    let mut buf = [0u8; 4096];
    while got_hello.is_none() || got_status.is_none() {
        assert!(Instant::now() < deadline, "timed out waiting for HelloOk+StatusOk over the piped lane");
        let n = tokio::time::timeout(BOUND, lane_conn.read(&mut buf))
            .await
            .expect("read piped bytes within BOUND")
            .expect("read piped bytes");
        assert!(n > 0, "the piped connection EOF'd before HelloOk+StatusOk arrived");
        let (frames, err) = splitter.feed(&buf[..n]);
        assert!(err.is_none(), "wire decode error over the piped supervisor lane: {err:?}");
        for f in frames {
            match f {
                sot_log::lane::wire::DecodedFrame::SupervisorReply(sot_log::lane::wire::SupervisorReply::HelloOk {
                    pid: hp,
                    created: hc,
                    ..
                }) => got_hello = Some((hp, hc)),
                sot_log::lane::wire::DecodedFrame::SupervisorReply(sot_log::lane::wire::SupervisorReply::StatusOk {
                    phase,
                    ..
                }) => got_status = Some(phase),
                other => panic!("unexpected frame over the piped supervisor lane: {other:?}"),
            }
        }
    }
    assert_eq!(
        got_hello,
        Some((pid as u32, created)),
        "the piped HelloOk's own pid+created must match lane.connect's own report"
    );
    assert_eq!(got_status, Some(sot_log::lane::wire::SupervisorPhase::Ready));

    drop(lane_conn);

    // The lane slot was released, not the row itself -- workspace.list
    // still reports "ready" afterward.
    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;

    env.kill_daemon_bounded().await;
}

/// `lane: "voyage"` (with the id from a real `status` reply) pipes the
/// attach lane: `AttachClient::Hello{proto: ATTACH_PROTO_V2}` comes back
/// `AttachServer::HelloOk{proto: 2}`.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn lane_connect_voyage_pipes_the_attach_hello() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("lcv");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "lcv-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["session_name"].as_str().expect("session_name").to_string();

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;
    let state_dir = state_dir_from_list(&mut conn, &mut next_id, &workspace_id).await;

    let voyage_id = tokio::task::spawn_blocking({
        let dir = state_dir.clone();
        move || sot_log::attach_client::supervisor_client::query_status(&dir).expect("query_status on the ready row").0.voyage
    })
    .await
    .unwrap()
    .expect("a ready capsule has a voyage");

    let (mut lane_conn, res) = lane_connect(&env, &target, "voyage", Some(&voyage_id)).await;
    assert!(res.get("error").is_none(), "lane.connect refused: {res:?}");
    assert_eq!(res["ok"].as_bool(), Some(true), "lane.connect payload: {res:?}");

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let hello = sot_log::lane::wire::encode_attach_client(&sot_log::lane::wire::AttachClient::Hello {
        proto: sot_log::lane::wire::ATTACH_PROTO_V2,
    })
    .expect("encode attach hello");
    lane_conn.write_all(&hello).await.expect("write attach hello");

    let mut splitter = sot_log::lane::wire::FrameSplitter::new();
    let mut buf = [0u8; 4096];
    let deadline = Instant::now() + BOUND;
    let proto = loop {
        assert!(Instant::now() < deadline, "timed out waiting for the attach lane's own HelloOk");
        let n = tokio::time::timeout(BOUND, lane_conn.read(&mut buf))
            .await
            .expect("read piped bytes within BOUND")
            .expect("read piped bytes");
        assert!(n > 0, "the piped connection EOF'd before HelloOk arrived");
        let (frames, err) = splitter.feed(&buf[..n]);
        assert!(err.is_none(), "wire decode error over the piped voyage lane: {err:?}");
        if let Some(f) = frames.into_iter().next() {
            match f {
                sot_log::lane::wire::DecodedFrame::AttachServer(sot_log::lane::wire::AttachServer::HelloOk { proto }) => break proto,
                other => panic!("unexpected frame over the piped voyage lane: {other:?}"),
            }
        }
    };
    assert_eq!(proto, sot_log::lane::wire::ATTACH_PROTO_V2);

    drop(lane_conn);
    env.kill_daemon_bounded().await;
}

/// (a) The client HALF-closes its own write side — never a full drop —
/// on one pipe. ADR 0045 decision 2's own "either direction closing ends
/// both": `pipe_bidirectional` tears down the WHOLE pipe (both
/// directions) the instant EITHER copy direction completes, so the
/// client's OWN read side then also observes a bounded EOF — the
/// directly observable proof, from the client's own vantage point, that
/// a client-initiated half-close reaches the upstream lane and the
/// daemon closes back. A fresh `lane.connect` against the SAME row
/// afterward still succeeds — the lane concurrency slot was released,
/// not leaked. (b) `workspace.destroy` while a DIFFERENT pipe is open
/// ends the row out from under it: that pipe's own client-side read
/// observes a bounded EOF too — daemon-initiated closure, the opposite
/// direction from (a)'s client-initiated one; together these are the
/// bounded-EOF proof at both ends, both directions.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn lane_connect_closes_when_either_side_closes() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("lcc");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let create_req = serde_json::json!({
        "label": "lcc-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["session_name"].as_str().expect("session_name").to_string();

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;

    // (a) HALF-close the client's own write side (never a full drop) --
    // the daemon must still tear down BOTH directions, so THIS
    // connection's own read side observes a bounded EOF back.
    let (mut first_conn, res) = lane_connect(&env, &target, "supervisor", None).await;
    assert_eq!(res["ok"].as_bool(), Some(true), "first lane.connect payload: {res:?}");
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        first_conn.shutdown().await.expect("half-close the client's own write side");
        let mut buf = [0u8; 1];
        let n = tokio::time::timeout(BOUND, first_conn.read(&mut buf))
            .await
            .expect("read after a client half-close within BOUND")
            .expect("read after a client half-close");
        assert_eq!(
            n, 0,
            "a client half-close must still produce a bounded EOF back -- the daemon tears down BOTH directions once either one closes"
        );
    }
    drop(first_conn);
    let (second_conn, res) = poll_until(
        || {
            let env = &env;
            let target = &target;
            async move {
                let (conn, res) = lane_connect(env, target, "supervisor", None).await;
                (res["ok"].as_bool() == Some(true)).then_some((conn, res))
            }
        },
        BOUND,
        "a second lane.connect to succeed after the first client dropped",
    )
    .await;
    assert_eq!(res["ok"].as_bool(), Some(true), "second lane.connect payload: {res:?}");

    // (b) workspace.destroy while a pipe is still open -> the client's
    // own next read observes EOF within a bound.
    let destroy_req = serde_json::json!({ "workspace_id": workspace_id });
    // `next_id` has no further use on this connection (mirrors the
    // create/list/destroy test's own convention) — no further increment.
    let destroy_res = call(&mut conn, next_id, op::WORKSPACE_DESTROY, destroy_req).await;
    assert!(destroy_res.payload.get("error").is_none(), "workspace.destroy failed: {:?}", destroy_res.payload);

    let mut second_conn = second_conn;
    use tokio::io::AsyncReadExt;
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(30), second_conn.read(&mut buf))
        .await
        .expect("read after workspace.destroy within 30s")
        .expect("read after workspace.destroy");
    assert_eq!(n, 0, "the piped connection must EOF once workspace.destroy ends the row out from under it");

    env.kill_daemon_bounded().await;
}

/// A stopped row's dial IS the recovery trigger (ADR 0045 decision 2):
/// `lane: "supervisor"` on a row whose authority died resumes it in
/// place rather than answering absent. TWO CONCURRENT initial dials
/// (Codex review SHOULD-FIX, 2026-09-11: sequential connects plus one
/// `pgrep` snapshot afterward cannot prove absence of a transient extra
/// spawn while the race is still live) both succeed, and a background
/// sampler running continuously across the WHOLE recovery window proves
/// at most one `supervise` process ever matched at any sampled instant —
/// the resume, never a second racing authority.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn lane_connect_resumes_a_stopped_row() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("lcr2");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let (workspace_id, _state_dir_path) =
        create_ready_workspace_then_stop_its_supervisor(&env, &mut conn, &mut next_id, "lcr2-workspace").await;

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let row = find_row(&list_payload, &workspace_id).expect("the row is still registered");
    let target = row["session_name"].as_str().expect("session_name").to_string();

    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let max_seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sampler = {
        let stop = stop.clone();
        let max_seen = max_seen.clone();
        let pattern = pattern.clone();
        tokio::spawn(async move {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let pattern = pattern.clone();
                let n = tokio::task::spawn_blocking(move || count_matching_processes(&pattern).unwrap_or(0))
                    .await
                    .unwrap_or(0);
                max_seen.fetch_max(n, std::sync::atomic::Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    };

    let ((first_conn, res_a), (second_conn, res_b)) = tokio::join!(
        lane_connect(&env, &target, "supervisor", None),
        lane_connect(&env, &target, "supervisor", None),
    );
    assert!(res_a.get("error").is_none(), "lane.connect must resume the stopped row rather than refuse it: {res_a:?}");
    assert_eq!(res_a["ok"].as_bool(), Some(true), "first concurrent lane.connect payload: {res_a:?}");
    assert!(res_b.get("error").is_none(), "lane.connect must resume the stopped row rather than refuse it: {res_b:?}");
    assert_eq!(res_b["ok"].as_bool(), Some(true), "second concurrent lane.connect payload: {res_b:?}");

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND.max(Duration::from_secs(90))).await;

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = sampler.await;
    assert!(
        max_seen.load(std::sync::atomic::Ordering::Relaxed) <= 1,
        "more than one supervise process matched at some sampled instant across the two concurrent initial dials"
    );

    drop(first_conn);
    drop(second_conn);
    env.kill_daemon_bounded().await;
}

/// Every `lane.connect` refusal code this daemon can answer, each closing
/// the connection: an unknown `target` (`unknown_workspace`), the tmux
/// default row (`not_capsule`), `lane: "voyage"` with no `voyage_id`
/// (`bad_lane`), a bogus `voyage_id` on a ready row (`lane_absent`, the
/// supervisor owns leg respawn so this is never resumed), and a TERMINAL
/// row's own supervisor lane (`lane_absent` with `kind` present, and —
/// the row already has no live authority to begin with — no process
/// spawned by the refusal).
#[tokio::test]
#[cfg(target_os = "linux")]
async fn lane_connect_refusals() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("lcf");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    // Unknown target.
    let (mut s, res) = lane_connect(&env, "sot-be-no-such-row", "supervisor", None).await;
    assert_eq!(res["code"].as_str(), Some("unknown_workspace"), "{res:?}");
    assert_lane_connect_closes(&mut s).await;

    // A ready capsule row, shared by the two voyage-lane sub-cases below.
    let create_req = serde_json::json!({
        "label": "lcf-ready",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create_req).await;
    next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let ready_workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let ready_target = create_res.payload["session_name"].as_str().expect("session_name").to_string();
    poll_for_phase(&mut conn, &mut next_id, &ready_workspace_id, "ready", BOUND).await;

    // voyage lane, no voyage_id.
    let (mut s, res) = lane_connect(&env, &ready_target, "voyage", None).await;
    assert_eq!(res["code"].as_str(), Some("bad_lane"), "{res:?}");
    assert_lane_connect_closes(&mut s).await;

    // voyage lane, a bogus (but well-formed) id on an otherwise-ready row
    // -- the row's own `drawer.voyage` pointer IS valid, just for a
    // DIFFERENT voyage, so this is `voyage_mismatch` (the ownership
    // check, Codex review BLOCKER 2026-09-11), never a dial attempt at
    // all -- `lane_connect_refuses_a_voyage_id_the_target_row_does_not_own`
    // covers the two-row form of this same check.
    let (mut s, res) = lane_connect(&env, &ready_target, "voyage", Some("00000000-0000-0000-0000-000000000000")).await;
    assert_eq!(res["code"].as_str(), Some("voyage_mismatch"), "{res:?}");
    assert_lane_connect_closes(&mut s).await;

    env.kill_daemon_bounded().await;

    // A TERMINAL row, in its own fresh daemon (the default row's own
    // toml must carry `agent = "claude"` BEFORE boot -- incompatible
    // with the plain tmux default row exercised above). Mirrors
    // `capsule_row_with_an_unlaunchable_agent_reaches_terminal_and_is_destroyable`.
    let env2 = Env::new("lcft");
    env2.seed_default_capsule_toml("claude");
    let fake_claude_dir = env2.seed_fake_unlaunchable_claude();
    env2.spawn_sotd_with_prepended_path(&fake_claude_dir);
    let (mut conn2, mut next_id2) = connect_and_hello(&env2.socket_path).await;

    let list_payload = call(&mut conn2, next_id2, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id2 += 1;
    let default_row2 = list_payload["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .find(|w| w["is_default"].as_bool() == Some(true))
        .cloned()
        .expect("a default workspace row");
    let default_workspace_id2 = default_row2["workspace_id"].as_str().expect("workspace_id").to_string();
    let default_target2 = default_row2["session_name"].as_str().expect("session_name").to_string();

    let pty_req = serde_json::json!({
        "cols": 80, "rows": 24, "user_switch": true, "target": default_target2,
    });
    let pty_res = call(&mut conn2, next_id2, op::PTY_OPEN, pty_req).await;
    next_id2 += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);

    let terminal_deadline = Instant::now() + BOUND;
    loop {
        let id = next_id2;
        next_id2 += 1;
        let payload = call(&mut conn2, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &default_workspace_id2) {
            if row["phase"].as_str() == Some("terminal") {
                break;
            }
        }
        assert!(
            Instant::now() < terminal_deadline,
            "timed out waiting for the unlaunchable-agent capsule row to reach phase \"terminal\""
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env2.state_root);
    // The anti-flap authority exits within a couple hundred ms of its own
    // last spawn attempt (`capsule_row_with_an_unlaunchable_agent_...`'s
    // own doc); `workspace.list`'s "terminal" is a POINTER read, so it
    // can be observed a hair before the OS finishes reaping the just-
    // exited process -- poll rather than a single point-in-time pgrep.
    assert!(
        poll_until_no_process_matches(&pattern, BOUND),
        "a terminal row must settle to no live supervise process"
    );

    let (mut s, res) = lane_connect(&env2, &default_target2, "supervisor", None).await;
    assert_eq!(res["code"].as_str(), Some("lane_absent"), "{res:?}");
    assert!(res.get("kind").and_then(|v| v.as_str()).is_some(), "lane_absent must carry a kind field: {res:?}");
    assert_lane_connect_closes(&mut s).await;

    assert_eq!(
        count_matching_processes(&pattern).expect("pgrep"),
        0,
        "a terminal row's own lane.connect refusal must never spawn a supervise process"
    );

    env2.kill_daemon_bounded().await;
}

/// Codex review BLOCKER (2026-09-11): a `voyage_id` names a socket by id
/// ALONE — `Endpoint::connect_voyage_unchallenged`'s own `lane` argument
/// is the daemon-lane endpoint's namespace, ignored by both platform
/// endpoints — so nothing about the dial itself ties a voyage to the row
/// that owns it. Two ready rows, A and B: `{target: A, voyage_id: B's
/// voyage}` must refuse `voyage_mismatch` (checked against A's own
/// `drawer.voyage` pointer BEFORE any dial — never piping B's voyage
/// through A's row), closing the connection; `{target: A, voyage_id: A's
/// own voyage}` must still succeed, proving the check is a real
/// comparison and not an unconditional refusal.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn lane_connect_refuses_a_voyage_id_the_target_row_does_not_own() {
    let _serial = SERIAL.lock().await;
    assert!(
        sot_capsule_exe().is_file(),
        "{CAPSULE_EXE_NAME} not found next to sotd[.exe] at {:?} — build it first \
         (cargo build -p sot-log --bin sot-capsule) into the SAME target dir \
         this test's own sotd[.exe] was built into",
        sot_capsule_exe()
    );

    let env = Env::new("lcvm");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    // Every `workspace.create` needs its OWN project root (the daemon
    // refuses a duplicate one) -- a fresh directory per row, under this
    // env's own private tempdir, mirrors this file's own
    // `tmux_project_root` precedent.
    async fn create_ready(conn: &mut Conn, next_id: &mut u64, env: &Env, label: &str) -> (String, PathBuf) {
        let project_root = env._tmp.path().join(label);
        std::fs::create_dir_all(&project_root).expect("mkdir project_root");
        let create_req = serde_json::json!({
            "label": label,
            "project_root": project_root.to_string_lossy(),
            "runtime": "capsule",
        });
        let create_res = call(conn, *next_id, op::WORKSPACE_CREATE, create_req).await;
        *next_id += 1;
        assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
        let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
        let target = create_res.payload["session_name"].as_str().expect("session_name").to_string();
        poll_for_phase(conn, next_id, &workspace_id, "ready", BOUND).await;
        (target, state_dir_from_list(conn, next_id, &workspace_id).await)
    }

    let (target_a, state_dir_a) = create_ready(&mut conn, &mut next_id, &env, "lcvm-a").await;
    // Only B's voyage id is needed (never B's own target) -- the whole
    // point is dialing it THROUGH A.
    let (_target_b, state_dir_b) = create_ready(&mut conn, &mut next_id, &env, "lcvm-b").await;

    let voyage_a = tokio::task::spawn_blocking({
        let dir = state_dir_a.clone();
        move || sot_log::attach_client::supervisor_client::query_status(&dir).expect("query_status on row A").0.voyage
    })
    .await
    .unwrap()
    .expect("row A has a voyage");
    let voyage_b = tokio::task::spawn_blocking({
        let dir = state_dir_b.clone();
        move || sot_log::attach_client::supervisor_client::query_status(&dir).expect("query_status on row B").0.voyage
    })
    .await
    .unwrap()
    .expect("row B has a voyage");
    assert_ne!(voyage_a, voyage_b, "two freshly created rows must never share a voyage id");

    // A's target with B's voyage id -- must never pipe B's voyage
    // through A's row.
    let (mut s, res) = lane_connect(&env, &target_a, "voyage", Some(&voyage_b)).await;
    assert_eq!(res["code"].as_str(), Some("voyage_mismatch"), "{res:?}");
    assert_lane_connect_closes(&mut s).await;

    // A's target with A's OWN voyage id -- the check is a real
    // comparison, not an unconditional refusal.
    let (_s, res) = lane_connect(&env, &target_a, "voyage", Some(&voyage_a)).await;
    assert!(res.get("error").is_none(), "A's own voyage id against A's own target must succeed: {res:?}");
    assert_eq!(res["ok"].as_bool(), Some(true), "{res:?}");

    env.kill_daemon_bounded().await;
}
