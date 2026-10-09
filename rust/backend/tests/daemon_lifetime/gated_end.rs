//! A window's close and a row's destroy while the row's capsule birth is held at the durable parent's gate, after the
//! birth was accepted under the row's fence claim and before the parent opens the gate (`parent_release`). The start
//! holds the row's run-gate permit and its guard until the parent answers, and the fence is claimed, so no end of the row
//! can be proven while the birth is held (ADR 0043 decision 37). A close waits for the birth and then ends the row; a
//! destroy waits for it and then either ends the row or, when the new supervisor is not answering yet, refuses and keeps
//! it, and in neither case is a row lost or a supervisor left without its row. (A close that a start outlasts records the
//! row unended and keeps it: window_lease's `shutdown_bound_is_end_to_end`.) Real `sotd` and `sot-capsule` (the barrier
//! build); the held durable parent and the supervisor it forks are watched, never signalled: the supervisor is ended
//! through the product.

use crate::fixture_owner::Fixture;
use crate::observations::{BarrierDir, Report};
use crate::support::{
    call, connect_and_hello, find_row, handoff, sot_capsule_exe, Conn, Env, BOUND,
};
use crate::SERIAL;
use sot_protocol::ops::{op, FeLeaseReq};
use sot_protocol::{codec, Frame};
use std::time::{Duration, Instant};

/// The phase the birth is held at: accepted and forked, its target gated, the parent about to open the gate.
const PHASE: &str = "parent_release";

/// A daemon with one capsule row whose birth is held at [`PHASE`].
struct Held {
    env: Env,
    barriers: BarrierDir,
    fx: Fixture,
    workspace_id: String,
}

/// The report of arrival `ticket` at `phase`, waited for without holding the case's runtime, which the create, the close
/// and the destroy run on.
async fn reached(barriers: &BarrierDir, phase: &str, ticket: usize) -> Report {
    let began = Instant::now();
    while !barriers.has_reached(phase, ticket) {
        assert!(
            began.elapsed() < BOUND,
            "nothing reached the {phase} barrier within {BOUND:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    barriers.reached(phase, ticket, BOUND)
}

/// One request on a connection of its own, its answer's payload, or `None` when the daemon ended first. Never panics.
async fn request(
    socket: std::path::PathBuf,
    op_name: &'static str,
    payload: serde_json::Value,
) -> Option<serde_json::Value> {
    let (mut conn, id) = connect_and_hello(&socket).await;
    codec::write_frame(&mut conn, &Frame::req(id, op_name, payload), None)
        .await
        .ok()?;
    let (answer, _) = codec::read_frame(&mut conn).await.ok()?;
    Some(answer.payload)
}

async fn hold_a_birth(tag: &str, case: &str, bound_ms: &str) -> Held {
    assert!(
        sot_capsule_exe().is_file(),
        "{:?} missing: build sot-capsule with --features native-barrier into this target first",
        sot_capsule_exe()
    );
    let env = Env::new(tag);
    let barriers = BarrierDir::new(env._tmp.path());
    barriers.hold(PHASE, 1);
    let dir = barriers.path().to_string_lossy().into_owned();
    let mut fx = Fixture::new(case);
    env.spawn_sotd_with_env(&[
        ("SOT_TEST_BARRIER_DIR", &dir),
        ("SOT_TEST_SHUTDOWN_BOUND_MS", bound_ms),
    ]);
    let project = env.workspace_project_root.to_string_lossy().into_owned();
    // The create does not answer while the birth is held; its task lives as long as the case.
    tokio::spawn(request(
        env.socket_path.clone(),
        op::WORKSPACE_CREATE,
        serde_json::json!({ "label": "held-row", "project_root": project, "runtime": "capsule" }),
    ));
    let parent = reached(&barriers, PHASE, 0).await;
    fx.watch(
        parent.pid,
        Some(parent.created),
        "the durable parent, held at the gate",
    )
    .expect("an identity for the held parent");
    let (mut conn, next_id) = connect_and_hello(&env.socket_path).await;
    let listed = call(
        &mut conn,
        next_id,
        op::WORKSPACE_LIST,
        serde_json::json!({}),
    )
    .await
    .payload;
    let workspace_id = listed["workspaces"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["label"] == "held-row"))
        .and_then(|row| row["workspace_id"].as_str())
        .unwrap_or_else(|| panic!("the held row is not listed: {listed:?}"))
        .to_string();
    Held {
        env,
        barriers,
        fx,
        workspace_id,
    }
}

impl Held {
    /// Open the gate; the supervisor the parent forked reaches its first act, and the fixture watches it.
    async fn release(&mut self) -> usize {
        self.barriers.open(PHASE, 0);
        let supervisor = reached(&self.barriers, "pre_fence", 0).await;
        self.fx
            .watch(
                supervisor.pid,
                Some(supervisor.created),
                "the supervisor of the held birth",
            )
            .expect("an identity for the supervisor")
    }

    /// The daemon's exit status within `bound`, `None` if it is still running.
    async fn daemon_status(&self, bound: Duration) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + bound;
        loop {
            if let Some(status) = self
                .env
                .daemon
                .borrow_mut()
                .as_mut()
                .and_then(|d| d.try_wait().ok().flatten())
            {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The row's registration file.
    fn registration(&self) -> std::path::PathBuf {
        self.env
            .app_config_dir()
            .join(format!("workspaces-{}", crate::support::TEST_STATE_HOST))
            .join("held-row.toml")
    }

    /// The row's phase as the daemon lists it, `gone` once it is not listed.
    async fn phase(&self) -> String {
        let (mut conn, next_id) = connect_and_hello(&self.env.socket_path).await;
        let listed = call(
            &mut conn,
            next_id,
            op::WORKSPACE_LIST,
            serde_json::json!({}),
        )
        .await
        .payload;
        find_row(&listed, &self.workspace_id).map_or("gone".into(), |row| {
            row["phase"].as_str().unwrap_or("").to_string()
        })
    }
}

/// A lease from this process on the daemon at `socket`, then the window's close: its answer (`not_ended`, among others).
async fn close(socket: std::path::PathBuf) -> serde_json::Value {
    let me = sot_log::identity::challenge::self_identity().expect("this process's identity");
    let lease = FeLeaseReq {
        boot: me.boot,
        pid: me.pid,
        created: me.created,
        token: None,
    };
    let mut conn: Conn = handoff(
        &socket,
        &Frame::req(1, op::FE_LEASE, serde_json::to_value(&lease).unwrap()),
    )
    .await;
    let (granted, _) = codec::read_frame(&mut conn).await.expect("the lease reply");
    assert_eq!(
        granted.payload["outcome"], "granted",
        "{:?}",
        granted.payload
    );
    codec::write_frame(
        &mut conn,
        &Frame::req(2, op::FE_LEAVING, serde_json::json!({ "intent": "close" })),
        None,
    )
    .await
    .expect("send the close");
    let (answer, _) = codec::read_frame(&mut conn)
        .await
        .expect("read the close's answer");
    answer.payload
}

#[tokio::test]
async fn a_close_waits_for_a_birth_held_at_the_gate_and_ends_its_row() {
    let _serial = SERIAL.lock().await;
    let mut held = hold_a_birth("gclose", "close_with_a_birth_held_at_the_gate", "25000").await;
    let closing = tokio::spawn(close(held.env.socket_path.clone()));
    // The close is in while the birth is held: it waits for the start's permit.
    tokio::time::sleep(Duration::from_secs(3)).await;
    held.fx.save("close_waited", !closing.is_finished());
    let supervisor = held.release().await;
    let answer = tokio::time::timeout(Duration::from_secs(60), closing).await;
    let not_ended = answer.map_or("no answer within 60 s".into(), |a| {
        a.map_or("the close task failed".into(), |a| {
            a["not_ended"].to_string()
        })
    });
    held.fx.save("close_not_ended", not_ended);
    let status = held
        .daemon_status(Duration::from_secs(30))
        .await
        .and_then(|s| s.code());
    held.fx.save("close_status", format!("{status:?}"));
    held.fx.save(
        "supervisor_ended",
        held.fx.identity(supervisor).exited(Duration::from_secs(10)),
    );
    held.fx
        .save("registration_kept", held.registration().exists());
    let cleanup = held.fx.cleanup();
    held.env.daemon.borrow_mut().take();
    assert!(cleanup.complete(), "{cleanup:?}");
    let saved = |key: &str| held.fx.saved(key).unwrap_or("not recorded").to_string();
    assert_eq!(
        saved("close_waited"),
        "true",
        "the close answered while the birth was held"
    );
    assert_eq!(
        saved("close_not_ended"),
        "0",
        "the close did not end the row it waited for"
    );
    assert_eq!(saved("close_status"), "Some(0)", "the close's exit");
    assert_eq!(
        saved("supervisor_ended"),
        "true",
        "the held birth's supervisor outlived the close"
    );
    assert_eq!(
        saved("registration_kept"),
        "false",
        "the ended row's registration is still on disk"
    );
}

#[tokio::test]
async fn a_destroy_during_a_held_birth_loses_no_row_and_leaves_no_supervisor() {
    let _serial = SERIAL.lock().await;
    let mut held = hold_a_birth("gdestroy", "destroy_with_a_birth_held_at_the_gate", "15000").await;
    let payload = serde_json::json!({ "workspace_id": held.workspace_id });
    let destroying = tokio::spawn(request(
        held.env.socket_path.clone(),
        op::WORKSPACE_DESTROY,
        payload.clone(),
    ));
    // The destroy is in while the birth is held: it waits for the row's guard.
    tokio::time::sleep(Duration::from_secs(1)).await;
    held.fx.save("destroy_waited", !destroying.is_finished());
    let supervisor = held.release().await;
    let first = tokio::time::timeout(Duration::from_secs(60), destroying)
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    let refused = first.as_ref().is_none_or(|a| a.get("error").is_some());
    held.fx.save("first_answer", format!("{first:?}"));
    // Whatever the first answer, the row agrees with it: a refusal keeps the row and its live supervisor, and once the
    // row is ready a second destroy ends it; an ended row has no supervisor left.
    let listed = held.phase().await != "gone";
    let settle = if refused {
        Duration::ZERO
    } else {
        Duration::from_secs(10)
    };
    let alive = !held.fx.identity(supervisor).exited(settle);
    held.fx.save(
        "after_first",
        format!(
            "refused {refused} listed {listed} on disk {} supervisor alive {alive}",
            held.registration().exists()
        ),
    );
    if refused {
        let began = Instant::now();
        while held.phase().await != "ready" && began.elapsed() < Duration::from_secs(60) {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let second = request(held.env.socket_path.clone(), op::WORKSPACE_DESTROY, payload).await;
        held.fx.save("second_answer", format!("{second:?}"));
    }
    held.fx.save(
        "supervisor_ended",
        held.fx.identity(supervisor).exited(Duration::from_secs(10)),
    );
    held.fx.save(
        "row_gone",
        held.phase().await == "gone" && !held.registration().exists(),
    );
    let answer = close(held.env.socket_path.clone()).await;
    held.fx
        .save("close_not_ended", answer["not_ended"].to_string());
    let cleanup = held.fx.cleanup();
    held.env.kill_daemon_bounded().await;
    assert!(cleanup.complete(), "{cleanup:?}");
    let saved = |key: &str| held.fx.saved(key).unwrap_or("not recorded").to_string();
    assert_eq!(
        saved("destroy_waited"),
        "true",
        "the destroy answered while the birth was held"
    );
    let after_first = saved("after_first");
    assert!(
        after_first == "refused true listed true on disk true supervisor alive true"
            || after_first == "refused false listed false on disk false supervisor alive false",
        "the first destroy's answer and the row disagree: {after_first} ({})",
        saved("first_answer")
    );
    assert!(
        !saved("second_answer").contains("error"),
        "the second destroy failed: {}",
        saved("second_answer")
    );
    assert_eq!(
        saved("supervisor_ended"),
        "true",
        "the destroyed row's supervisor outlived the destroy"
    );
    assert_eq!(
        saved("row_gone"),
        "true",
        "the destroyed row is still listed or on disk"
    );
    assert_eq!(
        saved("close_not_ended"),
        "0",
        "a close after the destroy found a row it could not end"
    );
}
