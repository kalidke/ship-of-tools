#![cfg(target_os = "linux")]
//! A test job killed from outside leaves no row. The parent runs this binary's own child as the main process of a
//! test container (`scripts/tests/in-container.sh`); the child starts a real daemon and a ready capsule row and then
//! kills itself with SIGKILL, so no `Drop` runs. When the container's job ends, the row's supervisor and the leg must
//! be gone and must have lived in the job's own control group. Both facts are judged by locks and control groups,
//! never by pids a test read from `ps`.

mod support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use sot_protocol::ops::{FeLeaseReq, FeLeaseRes};
use sot_protocol::{codec, op, Frame};
use support::{
    call, cgroup_rel, connect_and_hello, find_row, handoff, poll_for_phase, poll_until,
    user_manager_available_for_test, Env, BOUND,
};

const NAME: &str = "a_killed_test_job_leaves_no_row";
const CHILD: &str = "SOT_TEST_CONTAINED_CHILD";

fn state_lock_free(state_dir: &Path, voyage: &str) -> bool {
    let fence = sot_log::supervisor::journal::fence::lock_supervisor(state_dir).is_ok();
    let writer =
        sot_log::lock_writer(&state_dir.join("voyages").join(voyage).join("writer.lock")).is_ok();
    fence && writer
}

/// The row's own state folder and its supervisor's report, once ready.
async fn ready_row(env: &Env) -> (PathBuf, String, u32) {
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let req = serde_json::json!({
        "label": "cj-row",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
        "agent": "none",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, req).await;
    next_id += 1;
    assert!(
        res.payload.get("error").is_none(),
        "workspace.create failed: {:?}",
        res.payload
    );
    let id = res.payload["workspace_id"]
        .as_str()
        .expect("workspace_id")
        .to_string();
    poll_for_phase(&mut conn, &mut next_id, &id, "ready", BOUND).await;
    let list = call(
        &mut conn,
        next_id,
        op::WORKSPACE_LIST,
        serde_json::json!({}),
    )
    .await;
    let row = find_row(&list.payload, &id).expect("the new row is listed");
    let state_dir = PathBuf::from(row["state_dir"].as_str().expect("state_dir of a ready row"));
    let (voyage, pid) = poll_until(
        || {
            let dir = state_dir.clone();
            async move {
                let (report, process) = tokio::task::spawn_blocking(move || {
                    sot_log::attach_client::supervisor_client::query_status(&dir).ok()
                })
                .await
                .ok()??;
                Some((report.voyage?, process.pid()))
            }
        },
        BOUND,
        "the supervisor's status with a voyage",
    )
    .await;
    (state_dir, voyage, pid)
}

async fn child(dir: PathBuf) {
    sot_log::test_isolated::enter(NAME);
    let env = Env::new("cj");
    env.spawn_sotd();
    let (state_dir, voyage, peer_pid) = ready_row(&env).await;
    let me = std::process::id();
    let record = format!(
        "state_dir={}\nvoyage={voyage}\nsocket={}\nruntime={}\ntmp={}\nchild_pid={me}\nchild_cgroup={}\nsupervisor_cgroup={}\n",
        state_dir.display(),
        env.socket_path.display(),
        env._runtime_tmp.path().display(),
        env._tmp.path().display(),
        cgroup_rel(me),
        cgroup_rel(peer_pid),
    );
    let part = dir.join("ready.part");
    std::fs::write(&part, record).expect("write the ready record");
    std::fs::rename(&part, dir.join("ready")).expect("publish the ready record");
    // The test process ends as a killed one: no Drop runs.
    unsafe { libc::raise(libc::SIGKILL) };
    std::thread::sleep(Duration::from_secs(60));
}

/// A lease from this process, then the window's close: the daemon ends its rows and exits.
async fn close_by_lease(socket: &Path) {
    let me = sot_log::identity::challenge::self_identity().expect("this process's identity");
    let req = FeLeaseReq {
        boot: me.boot,
        pid: me.pid,
        created: me.created,
        token: None,
    };
    let mut conn = handoff(
        socket,
        &Frame::req(1, op::FE_LEASE, serde_json::to_value(&req).unwrap()),
    )
    .await;
    let (reply, _) = codec::read_frame(&mut conn).await.expect("the lease reply");
    let _: FeLeaseRes = serde_json::from_value(reply.payload).expect("a lease reply");
    let _ = codec::write_frame(
        &mut conn,
        &Frame::req(2, op::FE_LEAVING, serde_json::json!({ "intent": "close" })),
        None,
    )
    .await;
}

fn parse(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

async fn wait_ready_or_exit(dir: &Path, client: &mut std::process::Child) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(dir.join("ready")) {
            return Some(text);
        }
        if client.try_wait().ok().flatten().is_some() {
            return std::fs::read_to_string(dir.join("ready")).ok();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    None
}

fn spawn_client(
    dir: &Path,
    unit: &str,
    cmd: &std::process::Command,
    entry: &sot_log::test_isolated::Entry,
) -> std::process::Child {
    let (entered_name, entered_path) = entry.environment_assignment();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/tests/in-container.sh");
    let out = std::fs::File::create(dir.join("out")).expect("the job's output file");
    let mut entered = std::ffi::OsString::from(entered_name);
    entered.push("=");
    entered.push(entered_path);
    std::process::Command::new("bash")
        .arg(script)
        .arg(unit)
        .arg("--")
        .arg("/usr/bin/env")
        .arg(format!("{CHILD}={}", dir.display()))
        .arg(format!("TMPDIR={}", dir.display()))
        .arg(entered)
        .arg(cmd.get_program())
        .args(cmd.get_args())
        .stdin(Stdio::null())
        .stdout(out.try_clone().expect("clone the output file"))
        .stderr(out)
        .spawn()
        .expect("spawn the container client")
}

/// Waits for the job to end, then judges the row: is it gone within 10 s, and did it live in the job's control group.
async fn judge(r: &HashMap<String, String>, client: &mut std::process::Child) -> (bool, bool) {
    let (state_dir, voyage) = (PathBuf::from(&r["state_dir"]), r["voyage"].clone());
    let held_before = !sot_log::supervisor::journal::fence::lock_supervisor(&state_dir).is_ok();
    assert!(
        held_before,
        "precondition: the supervisor's fence must be held while the job runs"
    );
    // The job's end: the client returns when its unit stops; with the base shape it is the child itself.
    let end = Instant::now() + Duration::from_secs(30);
    while client.try_wait().ok().flatten().is_none() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let end = Instant::now() + Duration::from_secs(10);
    let gone = loop {
        if state_lock_free(&state_dir, &voyage) {
            break true;
        }
        if Instant::now() >= end {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    (gone, r["child_cgroup"] == r["supervisor_cgroup"])
}

/// Ends a row the job left, through the product, and waits for its locks.
async fn end_leftover_row(r: &HashMap<String, String>) {
    let (state_dir, voyage, sock) = (
        PathBuf::from(&r["state_dir"]),
        r["voyage"].clone(),
        PathBuf::from(&r["socket"]),
    );
    if support::try_connect(&sock).await.is_some() {
        close_by_lease(&sock).await;
    } else {
        let (d, v) = (state_dir.clone(), voyage.clone());
        let _ = tokio::task::spawn_blocking(move || {
            let _ =
                sot_log::attach_client::supervisor_client::end_run(&d, &v, "contained_job cleanup");
            let _ = sot_log::attach_client::supervisor_client::stop(&d);
        })
        .await;
    }
    let end = Instant::now() + Duration::from_secs(60);
    while !state_lock_free(&state_dir, &voyage) && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn a_killed_test_job_leaves_no_row() {
    if let Some(dir) = std::env::var_os(CHILD) {
        return child(PathBuf::from(dir)).await;
    }
    if let Err(e) = user_manager_available_for_test() {
        if std::env::var("SOT_TEST_REQUIRE_USER_MANAGER").as_deref() == Ok("1") {
            panic!("SOT_TEST_REQUIRE_USER_MANAGER=1 but no user manager is reachable: {e}");
        }
        eprintln!("SKIPPED: no user manager: {e}");
        return;
    }
    let dir = tempfile::Builder::new()
        .prefix("sotcj-")
        .tempdir_in("/tmp")
        .expect("a scratch folder");
    let unit = format!("sot-test-cj-{}", uuid::Uuid::now_v7().simple());
    let (cmd, entry) = sot_log::test_isolated::test_command(NAME);
    let mut client = spawn_client(dir.path(), &unit, &cmd, &entry);

    let ready = wait_ready_or_exit(dir.path(), &mut client).await;
    let record = ready.as_deref().map(parse);
    let (gone, contained) = match &record {
        Some(r) => judge(r, &mut client).await,
        None => (true, false),
    };
    // Cleanup runs before any assert and never panics.
    let _ = client.kill();
    let _ = client.wait();
    if let (false, Some(r)) = (gone, &record) {
        end_leftover_row(r).await;
    }
    let _ = std::process::Command::new("systemctl")
        .args([
            "--user",
            "stop",
            &format!("{unit}.service"),
            &format!("{unit}.scope"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let field = |k: &str| record.as_ref().map(|r| r[k].clone()).unwrap_or_default();
    for k in ["runtime", "tmp"] {
        let _ = std::fs::remove_dir_all(field(k));
    }
    drop(dir);
    assert!(
        record.is_some(),
        "the child never wrote its ready record within 120 s"
    );
    entry.assert_once(field("child_pid").parse().expect("child_pid"));
    let (c, s) = (field("child_cgroup"), field("supervisor_cgroup"));
    assert!(
        contained && gone,
        "a killed test job: contained={contained} (the supervisor's control group {s}, the job's {c}), gone={gone} (its fence or writer lock was still held 10 s after the job ended)"
    );
}
