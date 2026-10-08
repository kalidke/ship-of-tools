//! Where the durable parent stands in the process tree of a real daemon with a real capsule row: its parent is not the
//! launched process nor any process the daemon started, so a capsule it forks is never inside what the daemon's end
//! drains. The tree is read, not changed: every pid below comes from the product's own lane (the supervisor) or from
//! `/proc` links read upward, and none of the parents found by walking up is signalled or recorded for cleanup.

use crate::support::{call, connect_and_hello, find_row, poll_until, Env, BOUND};
use sot_protocol::op;
use std::path::PathBuf;

use crate::SERIAL;

/// A process's parent, from `/proc/<pid>/stat` (the field after the command name and the state letter).
fn parent_of(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// The chain of parents above `pid`, nearest first, bounded.
fn ancestors(pid: i32) -> Vec<i32> {
    let mut chain = Vec::new();
    let mut at = pid;
    while let Some(parent) = parent_of(at).filter(|p| *p > 1 && chain.len() < 32) {
        chain.push(parent);
        at = parent;
    }
    chain
}

#[tokio::test]
async fn the_durable_parent_descends_from_nothing_the_daemon_started() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("dpar");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let created = call(
        &mut conn,
        next_id,
        op::WORKSPACE_CREATE,
        serde_json::json!({ "label": "tree", "project_root": env.workspace_project_root.to_string_lossy(), "runtime": "capsule" }),
    )
    .await
    .payload;
    next_id += 1;
    assert!(
        created.get("error").is_none(),
        "workspace.create failed: {created:?}"
    );
    let workspace_id = created["workspace_id"]
        .as_str()
        .expect("workspace_id")
        .to_string();
    let listed = call(
        &mut conn,
        next_id,
        op::WORKSPACE_LIST,
        serde_json::json!({}),
    )
    .await
    .payload;
    let state_dir = PathBuf::from(
        find_row(&listed, &workspace_id)
            .and_then(|r| r["state_dir"].as_str().map(str::to_owned))
            .expect("state dir"),
    );
    // The supervisor the product reports over its lane; its parent is the durable parent (observed, never signalled).
    let supervisor = poll_until(
        || {
            let dir = state_dir.clone();
            async move {
                tokio::task::spawn_blocking(move || {
                    sot_log::attach_client::supervisor_client::query_status(&dir)
                        .ok()
                        .map(|(report, _)| report.pid as i32)
                })
                .await
                .unwrap()
            }
        },
        BOUND,
        "the supervisor to answer",
    )
    .await;
    let launched = env.daemon.borrow().as_ref().expect("the daemon").id() as i32;
    let durable = parent_of(supervisor).expect("the supervisor's parent");
    assert!(
        durable > 1 && durable != launched,
        "the supervisor's parent is the daemon or init: {durable}"
    );
    let above = ancestors(durable);
    assert!(
        !above.contains(&launched),
        "the durable parent {durable} descends from the launched daemon {launched}: {above:?}"
    );
    assert_ne!(
        parent_of(durable),
        Some(launched),
        "the durable parent was started as the daemon's child"
    );
    // Its own session: the daemon's terminal and group are not its.
    // SAFETY: plain reads of two processes' session ids; the pids are only observed.
    let (daemon_sid, durable_sid) = unsafe { (libc::getsid(launched), libc::getsid(durable)) };
    assert_ne!(
        daemon_sid, durable_sid,
        "the durable parent shares the daemon's session"
    );
    env.kill_daemon_bounded().await;
}
