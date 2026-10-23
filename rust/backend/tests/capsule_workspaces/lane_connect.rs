//! `lane.connect` helpers and tests.

use super::*;

// --- ADR 0045 decision 2 (lane B3): `lane.connect`, the daemon-side lane
// bridge --- //

/// A FRESH connection to `env.socket_path` that hands off to `lane.connect` (ADR 0045 decision 2: a dedicated
/// connection, never through `connect_and_hello`'s multiplexed control loop): a handoff hello and the request in one
/// write. Returns the still-open connection — a `Conn` so a raw byte read/write afterward (on success) shares the
/// SAME `BufReader` the response frame was read through, never a throwaway second reader that would lose whatever
/// piped bytes it already buffered past the envelope — alongside the parsed response payload.
#[cfg(target_os = "linux")]
async fn lane_connect(env: &Env, target: &str) -> (Conn, serde_json::Value) {
    lane_connect_with_payload(env, target, &[]).await
}

/// [`lane_connect`]'s own general form: the hello, the connect envelope AND `extra_payload` (raw bytes meant for the
/// lane, once piped) are written in ONE `write_all` call, BEFORE this function ever reads anything back — the
/// peek-buffer-preservation proof (Codex review SHOULD-FIX, 2026-09-11: the daemon's own `read_frame` must consume
/// EXACTLY the hello and this envelope and leave whatever follows untouched for the raw pipe). Still returns only
/// after the `LaneConnectRes` frame itself has been read (through the SAME `BufReader` the caller goes on to read
/// any piped reply from), exactly like [`lane_connect`].
#[cfg(target_os = "linux")]
async fn lane_connect_with_payload(env: &Env, target: &str, extra_payload: &[u8]) -> (Conn, serde_json::Value) {
    let req = serde_json::json!({ "target": target });
    let mut conn = handoff_with(&env.socket_path, &Frame::req(1, op::LANE_CONNECT, req), extra_payload).await;
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

/// A frame as the row socket reads it: the envelope's line, then its blob if it declares one.
fn row_frame(id: u64, op: &str, payload: serde_json::Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(&Frame::req(id, op, payload)).expect("a frame serializes");
    line.push(b'\n');
    line
}

/// On a ready row, `lane.connect` pipes the row's one socket: `row.hello` AND `row.attach` written in ONE write call
/// (the buffered-bytes proof that the daemon never decodes a row frame after its own reply — it is a raw pipe, not a
/// second parser) come back as the hello's reply (own pid and creation matching `lane.connect`'s own report, phase
/// `ready`), the attach's reply and the screen's snapshot. Dropping the pipe releases the connection without
/// disturbing the row: `workspace.list` still reports "ready" afterward.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn lane_connect_pipes_the_row_hello_and_attach() {
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

    // The connect envelope AND the first row frames are written in ONE call, BEFORE this test ever reads the
    // LaneConnectRes reply: the daemon's own `read_frame` must consume EXACTLY the envelope and hand everything past it,
    // untouched, to the raw pipe.
    let mut payload = row_frame(1, "row.hello", serde_json::json!({"build": "test", "row": sot_log::row::ROW_PROTOCOL}));
    payload.extend(row_frame(2, "row.attach", serde_json::json!({"cols": 80, "rows": 24, "driver": false})));
    let (mut lane_conn, res) = lane_connect_with_payload(&env, &target, &payload).await;
    assert!(res.get("error").is_none(), "lane.connect refused: {res:?}");
    assert_eq!(res["ok"].as_bool(), Some(true), "lane.connect payload: {res:?}");
    let pid = res["pid"].as_u64().expect("pid");
    let created = res["created"].as_u64().expect("created");
    assert!(pid > 0, "pid must be a real process id: {res:?}");

    let (hello, _) = tokio::time::timeout(BOUND, codec::read_frame(&mut lane_conn))
        .await
        .expect("the hello's reply within BOUND")
        .expect("read the hello's reply");
    assert_eq!(hello.op, "row.hello", "{hello:?}");
    assert_eq!(
        (hello.payload["pid"].as_u64(), hello.payload["created"].as_u64()),
        (Some(pid), Some(created)),
        "the piped hello's own pid+created must match lane.connect's own report"
    );
    assert_eq!(hello.payload["phase"].as_str(), Some("ready"), "{hello:?}");
    let (attach, _) = tokio::time::timeout(BOUND, codec::read_frame(&mut lane_conn))
        .await
        .expect("the attach's reply within BOUND")
        .expect("read the attach's reply");
    assert_eq!(attach.op, "row.attach", "{attach:?}");
    assert!(attach.payload["run"].as_str().is_some_and(|r| !r.is_empty()), "the attach names the run: {attach:?}");
    let (snapshot, blob) = tokio::time::timeout(BOUND, codec::read_frame(&mut lane_conn))
        .await
        .expect("the snapshot within BOUND")
        .expect("read the snapshot");
    assert_eq!(snapshot.op, "row.snapshot", "{snapshot:?}");
    assert!(blob.is_some_and(|b| !b.is_empty()), "the snapshot carries the screen");

    drop(lane_conn);

    // The connection was released, not the row itself -- workspace.list still reports "ready" afterward.
    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND).await;

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
    let (mut first_conn, res) = lane_connect(&env, &target).await;
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
                let (conn, res) = lane_connect(env, target).await;
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
/// `lane.connect` on a row whose authority died resumes it in
/// place rather than answering absent. TWO CONCURRENT initial dials
/// (Codex review SHOULD-FIX, 2026-09-11: sequential connects plus one
/// one process count afterward cannot prove absence of a transient extra
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

    let state_root = env.state_root.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let max_seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sampler = {
        let stop = stop.clone();
        let max_seen = max_seen.clone();
        let state_root = state_root.clone();
        tokio::spawn(async move {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let state_root = state_root.clone();
                let n = tokio::task::spawn_blocking(move || try_own_supervisor_count(&state_root).expect("the capsule process listing"))
                    .await
                    .expect("the supervisor sampler's blocking count");
                max_seen.fetch_max(n, std::sync::atomic::Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
    };

    let ((first_conn, res_a), (second_conn, res_b)) = tokio::join!(
        lane_connect(&env, &target),
        lane_connect(&env, &target),
    );
    assert!(res_a.get("error").is_none(), "lane.connect must resume the stopped row rather than refuse it: {res_a:?}");
    assert_eq!(res_a["ok"].as_bool(), Some(true), "first concurrent lane.connect payload: {res_a:?}");
    assert!(res_b.get("error").is_none(), "lane.connect must resume the stopped row rather than refuse it: {res_b:?}");
    assert_eq!(res_b["ok"].as_bool(), Some(true), "second concurrent lane.connect payload: {res_b:?}");

    poll_for_phase(&mut conn, &mut next_id, &workspace_id, "ready", BOUND.max(Duration::from_secs(90))).await;

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    sampler.await.expect("the supervisor sampler");
    assert!(
        max_seen.load(std::sync::atomic::Ordering::Relaxed) <= 1,
        "more than one supervise process matched at some sampled instant across the two concurrent initial dials"
    );

    drop(first_conn);
    drop(second_conn);
    env.kill_daemon_bounded().await;
}

/// Every `lane.connect` refusal code this daemon can answer, each closing
/// the connection: an unknown `target` (`unknown_workspace`) and a TERMINAL
/// row's own socket (`lane_absent` with `kind` present, and — the row already
/// has no live authority to begin with — no process spawned by the refusal).
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
    let (mut s, res) = lane_connect(&env, "sot-be-no-such-row").await;
    assert_eq!(res["code"].as_str(), Some("unknown_workspace"), "{res:?}");
    assert_lane_connect_closes(&mut s).await;

    env.kill_daemon_bounded().await;

    // A TERMINAL row, in its own fresh daemon (the default row's own
    // toml must carry `agent = "claude"` BEFORE boot -- incompatible
    // with the plain tmux default row exercised above). Mirrors
    // `capsule_row_with_an_unlaunchable_agent_reaches_terminal_and_is_destroyable`.
    let env2 = Env::new("lcft");
    env2.seed_default_capsule_toml("claude");
    let fake_claude_dir = env2.seed_fake_unlaunchable_claude();
    env2.spawn_sotd_with_path_and_env(&fake_claude_dir, &[SHORT_BACKOFF]);
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

    // The daemon watchdog's restart backoffs (shortened by `SHORT_BACKOFF`), its settles and the suite's own bound: the longest the one
    // restart budget can take to end the row, whichever capsule runs it.
    let terminal_deadline = Instant::now() + BUDGET_WAIT;
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

    // The anti-flap authority exits within a couple hundred ms of its own
    // last spawn attempt (`capsule_row_with_an_unlaunchable_agent_...`'s
    // own doc); `workspace.list`'s "terminal" is a POINTER read, so it
    // can be observed a hair before the OS finishes reaping the just-
    // exited process -- poll rather than a single point-in-time listing.
    let left = poll_until_no_capsule_pids(&env2.state_root, BOUND);
    assert!(left.is_empty(), "a terminal row must settle to no live capsule process: {left:?}");

    let (mut s, res) = lane_connect(&env2, &default_target2).await;
    assert_eq!(res["code"].as_str(), Some("lane_absent"), "{res:?}");
    assert!(res.get("kind").and_then(|v| v.as_str()).is_some(), "lane_absent must carry a kind field: {res:?}");
    assert_lane_connect_closes(&mut s).await;

    assert_eq!(
        own_supervisor_pids(&env2.state_root).len(),
        0,
        "a terminal row's own lane.connect refusal must never spawn a supervise process"
    );

    env2.kill_daemon_bounded().await;
}

/// `lane.connect` pipes the socket of the row it names, never another's: two ready rows, and the pid the daemon reports
/// for each is that row's own supervisor, which differs.
#[tokio::test]
#[cfg(target_os = "linux")]
async fn lane_connect_pipes_the_named_rows_own_socket() {
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

    // Every `workspace.create` needs its OWN project root (the daemon refuses a duplicate one).
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
    let (target_b, state_dir_b) = create_ready(&mut conn, &mut next_id, &env, "lcvm-b").await;
    let pid_of = |dir: PathBuf| async move {
        tokio::task::spawn_blocking(move || sot_log::attach_client::supervisor_client::query_status(&dir).expect("query_status on a ready row").0.pid)
            .await
            .unwrap()
    };
    let (pid_a, pid_b) = (pid_of(state_dir_a).await, pid_of(state_dir_b).await);
    assert_ne!(pid_a, pid_b, "two rows have two supervisors");

    let (_a, res_a) = lane_connect(&env, &target_a).await;
    assert_eq!(res_a["pid"].as_u64(), Some(u64::from(pid_a)), "A's connect reaches A's own socket: {res_a:?}");
    let (_b, res_b) = lane_connect(&env, &target_b).await;
    assert_eq!(res_b["pid"].as_u64(), Some(u64::from(pid_b)), "B's connect reaches B's own socket: {res_b:?}");

    env.kill_daemon_bounded().await;
}
