//! Rows' work-state and phase in a real daemon: a row waiting at its agent's permission prompt reads as waiting on its
//! person, and a refreshed row loaded at boot follows its new supervisor.

use super::*;

const RULE_CHARS: usize = 60;

/// A stand-in `claude` that draws, from a cleared screen, a free prompt box, or, while `dialog` exists, a permission
/// dialog with Claude Code's footer (rows' dialog tick reads it as `dialog_of` does).
fn write_dialog_claude(dir: &Path, dialog: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).expect("mkdir stub bin");
    let script = format!(
        "#!/bin/sh\n\
         rule=''; i=0; while [ $i -lt {RULE_CHARS} ]; do rule=\"$rule$(printf '\\342\\224\\200')\"; i=$((i+1)); done\n\
         while :; do\n\
           printf '\\033[2J\\033[H'\n\
           if [ -f '{dialog}' ]; then\n\
             printf '\\342\\227\\217 Bash(rm -rf build)\\n%s\\n Bash command\\n\\n   rm -rf build\\n\\n Do you want to proceed?\\n \\342\\235\\257 1. Yes\\n   2. No\\n\\n Esc to cancel \\302\\267 Tab to amend\\n' \"$rule\"\n\
             while [ -f '{dialog}' ]; do sleep 0.1; done\n\
           else\n\
             printf '%s\\n\\342\\235\\257\\302\\240\\n%s\\n' \"$rule\" \"$rule\"\n\
             while [ ! -f '{dialog}' ]; do sleep 0.1; done\n\
           fi\n\
         done\n",
        dialog = dialog.display(),
    );
    sot_log::test_exec::write_executable(&dir.join("claude"), script);
    dir.to_path_buf()
}

/// The row's `(agent_state, agent_summary)` from `workspace.list`, polled until `want` holds or `within` passes.
async fn poll_work_state(
    conn: &mut Conn,
    next_id: &mut u64,
    workspace_id: &str,
    within: Duration,
    want: impl Fn(&str, &str) -> bool,
) -> (String, String) {
    let deadline = Instant::now() + within;
    loop {
        let id = *next_id;
        *next_id += 1;
        let payload = call(conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        let row = find_row(&payload, workspace_id).expect("the row is listed");
        let got = (row["agent_state"].as_str().unwrap_or("").to_string(), row["agent_summary"].as_str().unwrap_or("").to_string());
        if want(&got.0, &got.1) || Instant::now() >= deadline {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// A Claude row whose screen shows a permission prompt reads `blocked` with the prompt's question, while its turn runs
/// and at rest, within the dialog tick (5 s plus the sweep); once the prompt closes it reads what its other facts say.
/// Rows' dialog tick writes the fact in the daemon's own process, through work-state's one write.
#[tokio::test]
async fn a_row_at_its_agents_permission_prompt_reads_waiting_on_its_person() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd — build it first");
    let env = Env::new("cwsd");
    let dialog = env._tmp.path().join("dialog");
    let stub = write_dialog_claude(&env._tmp.path().join("stubbin"), &dialog);
    env.spawn_sotd_with_path_and_env(&stub, &[]);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let create = serde_json::json!({
        "label": "cwsd-row",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
        "agent": "claude",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create).await;
    next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    let ws = res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let join = call(&mut conn, next_id, op::AGENT_JOIN, serde_json::json!({ "workspace_id": ws, "handle": "cwsd-h" })).await;
    next_id += 1;
    assert_eq!(join.payload["ok"], true, "agent.join failed: {:?}", join.payload);
    // The session's own entry, as its join and its prompt hook leave it: a turn running.
    let entry = serde_json::json!({"host": TEST_STATE_HOST, "state": "working", "floor": "user", "summary": "", "status_at": "2026-10-21T00:00:00Z"});
    support::write_registry(&env.comm_root, |doc| *doc = serde_json::json!({"agents": {"cwsd-h": entry}}));
    poll_for_phase(&mut conn, &mut next_id, &ws, "ready", BOUND.max(Duration::from_secs(60))).await;
    let got = poll_work_state(&mut conn, &mut next_id, &ws, Duration::from_secs(10), |s, _| s == "working").await;
    assert_eq!(got.0, "working", "setup: the row reads its turn");

    std::fs::write(&dialog, b"").expect("open the dialog");
    let got = poll_work_state(&mut conn, &mut next_id, &ws, Duration::from_secs(20), |s, _| s == "blocked").await;
    assert_eq!(
        got,
        ("blocked".to_string(), "claude dialog: Do you want to proceed?".to_string()),
        "a row at its permission prompt reads waiting on its person, not green"
    );

    std::fs::remove_file(&dialog).expect("close the dialog");
    let got = poll_work_state(&mut conn, &mut next_id, &ws, Duration::from_secs(20), |s, _| s == "working").await;
    assert_eq!(got.0, "working", "the closed prompt leaves the row's turn showing");

    // The turn ends through the session's one write (`sotd work-state stop`, as its Stop hook runs it): the row rests,
    // blue. A dialog that opens now (a background agent's, after the lead's Stop) still reads red, and the rest returns.
    let stop = sot_log::test_wait::run_within(
        std::process::Command::new(sotd_program()).args(["work-state", "stop", "--handle", "cwsd-h"]).env("SOT_COMM_HOME", &env.comm_root),
        BOUND,
    );
    assert!(stop.status.success(), "sotd work-state stop: {stop}");
    let got = poll_work_state(&mut conn, &mut next_id, &ws, Duration::from_secs(10), |s, _| s == "done").await;
    assert_eq!(got.0, "done", "setup: the turn ended");
    std::fs::write(&dialog, b"").expect("open a dialog at rest");
    let got = poll_work_state(&mut conn, &mut next_id, &ws, Duration::from_secs(20), |s, _| s == "blocked").await;
    assert_eq!(got.0, "blocked", "a dialog on a row at rest reads waiting on its person");
    std::fs::remove_file(&dialog).expect("close it");
    let got = poll_work_state(&mut conn, &mut next_id, &ws, Duration::from_secs(20), |s, _| s == "done").await;
    assert_eq!(got.0, "done", "the closed dialog leaves the row at rest");
    env.kill_daemon_bounded().await;
}

/// A row loaded from its toml at boot is observed by its id; a same-label `workspace.create` refreshes it (a new row
/// under the same id) and starts its supervisor. The refreshed row's phase follows that supervisor: once its authority
/// stops, `workspace.list` stops reading `ready` (queue item 58: before the fix, the observer kept feeding the replaced
/// row and the list read the settle's `ready` until the daemon restarted).
#[tokio::test]
async fn a_refreshed_loaded_row_follows_its_new_supervisor() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd — build it first");
    let env = Env::new("cwrf");
    env.seed_capsule_toml("ws-cwrf-row-1", "cwrf-row", &env.workspace_project_root, "none");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let create = serde_json::json!({
        "label": "cwrf-row",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, create).await;
    next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    assert_eq!(res.payload["workspace_id"], "ws-cwrf-row-1", "the refresh keeps the loaded row's id");
    poll_for_phase(&mut conn, &mut next_id, "ws-cwrf-row-1", "ready", BOUND.max(Duration::from_secs(60))).await;
    let state_dir = state_dir_from_list(&mut conn, &mut next_id, "ws-cwrf-row-1").await;

    tokio::task::spawn_blocking(move || sot_log::attach_client::supervisor_client::stop(&state_dir).expect("stop the authority"))
        .await
        .expect("the stop");
    let deadline = Instant::now() + Duration::from_secs(20);
    let phase = loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        let phase = find_row(&payload, "ws-cwrf-row-1").and_then(|r| r["phase"].as_str().map(str::to_string)).unwrap_or_default();
        if phase != "ready" || Instant::now() >= deadline {
            break phase;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_ne!(phase, "ready", "the refreshed row's phase never followed its stopped supervisor");
    env.kill_daemon_bounded().await;
}
