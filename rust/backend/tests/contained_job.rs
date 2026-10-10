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

/// Whether the row's supervisor fence and its voyage's writer lock can be taken (each is dropped at once), in that order.
fn state_locks(state_dir: &Path, voyage: &str) -> (bool, bool) {
    let fence = sot_log::supervisor::journal::fence::lock_supervisor(state_dir).is_ok();
    let writer =
        sot_log::lock_writer(&state_dir.join("voyages").join(voyage).join("writer.lock")).is_ok();
    (fence, writer)
}

fn state_lock_free(state_dir: &Path, voyage: &str) -> bool {
    state_locks(state_dir, voyage) == (true, true)
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
    // The parent checks that the row is live first: a container ends everything the moment its job does.
    let go = dir.join("go");
    let end = Instant::now() + Duration::from_secs(60);
    while !go.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
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

/// The ready record, if the child wrote one, and how long after the start the client had already exited, if it had.
async fn wait_ready_or_exit(
    dir: &Path,
    client: &mut std::process::Child,
) -> (Option<String>, Option<Duration>) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(120) {
        if let Ok(text) = std::fs::read_to_string(dir.join("ready")) {
            return (Some(text), None);
        }
        if client.try_wait().ok().flatten().is_some() {
            return (
                std::fs::read_to_string(dir.join("ready")).ok(),
                Some(start.elapsed()),
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    (None, None)
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

/// `cgroup.kill` in the job's own group, only when its leaf is the unit this test named: a contained row the unit did
/// not kill (`KillMode=none`, reversal R2) and the product did not end. Never panics.
fn kill_job_group(rel: &str, unit: &str) {
    let leaf = rel.rsplit('/').next().unwrap_or("");
    if leaf == format!("{unit}.service") || leaf == format!("{unit}.scope") {
        let kill = support::row_scope_aim::v2_root().join(rel.trim_start_matches('/')).join("cgroup.kill");
        let _ = std::fs::write(kill, "1");
    }
}

/// Whether a control group is gone or reads `populated 0`.
fn group_empty(rel: &str) -> bool {
    let events = support::row_scope_aim::v2_root()
        .join(rel.trim_start_matches('/'))
        .join("cgroup.events");
    match std::fs::read_to_string(events) {
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        Ok(text) => text.lines().any(|l| l.trim() == "populated 0"),
    }
}

/// Releases the child to kill itself, waits for the job to end, then judges: was the row's fence held before, are its
/// fence and writer lock free and the job's control group empty within 10 s, and did the row live in that group.
async fn judge(
    dir: &Path,
    r: &HashMap<String, String>,
    client: &mut std::process::Child,
) -> (bool, bool, bool, bool) {
    let (state_dir, voyage) = (PathBuf::from(&r["state_dir"]), r["voyage"].clone());
    let held_before = state_locks(&state_dir, &voyage) == (false, false);
    std::fs::write(dir.join("go"), "").expect("release the child");
    // The job's end: the client returns when its unit stops; with the base shape it is the child itself.
    let end = Instant::now() + Duration::from_secs(30);
    while client.try_wait().ok().flatten().is_none() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let end = Instant::now() + Duration::from_secs(10);
    let (mut gone, mut emptied) = (false, false);
    while !(gone && emptied) && Instant::now() < end {
        gone = gone || state_lock_free(&state_dir, &voyage);
        emptied = emptied || group_empty(&r["child_cgroup"]);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    (
        held_before,
        gone,
        r["child_cgroup"] == r["supervisor_cgroup"],
        emptied,
    )
}

/// Whether the row's fence and writer lock are both free within `secs`.
async fn wait_free(state_dir: &Path, voyage: &str, secs: u64) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while !state_lock_free(state_dir, voyage) && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    state_lock_free(state_dir, voyage)
}

/// The lease close on a thread of its own, with a runtime of its own: a panic in it stays in that thread.
fn close_on_thread(sock: PathBuf) {
    let worker = std::thread::Builder::new().spawn(move || {
        if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            rt.block_on(close_by_lease(&sock));
        }
    });
    let _ = worker.map(|w| w.join());
}

/// Ends a row the job left, through the product, then by its scope's control group if it escaped, and reports whether
/// its locks are free at the end. Never panics.
async fn end_leftover_row(r: &HashMap<String, String>) -> bool {
    let (state_dir, voyage, sock) = (
        PathBuf::from(&r["state_dir"]),
        r["voyage"].clone(),
        PathBuf::from(&r["socket"]),
    );
    // A row that left the job's group lives in a scope of its own: the product's end may not reach it.
    let guard = (r["supervisor_cgroup"] != r["child_cgroup"])
        .then(|| {
            std::panic::catch_unwind(|| {
                support::arm_scope_guard(&r["supervisor_cgroup"], &state_dir)
            })
            .ok()
        })
        .flatten();
    if support::try_connect(&sock).await.is_some() {
        close_on_thread(sock);
    } else {
        // The supervisor's lane socket lives in the child's private runtime folder.
        std::env::set_var("SOT_RUNTIME_DIR", &r["runtime"]);
        let (d, v) = (state_dir.clone(), voyage.clone());
        let _ = tokio::task::spawn_blocking(move || {
            let ended =
                sot_log::attach_client::supervisor_client::end_run(&d, &v, "contained_job cleanup");
            let stopped = sot_log::attach_client::supervisor_client::stop(&d);
            eprintln!("cleanup: end_run {ended:?}, stop {stopped:?}");
        })
        .await;
    }
    if wait_free(&state_dir, &voyage, 60).await {
        return true;
    }
    // The scope's cgroup.kill ends whatever the product did not.
    drop(guard);
    wait_free(&state_dir, &voyage, 10).await
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
    support::require_cgroup_v2_root();
    let dir = tempfile::Builder::new()
        .prefix("sotcj-")
        .tempdir_in("/tmp")
        .expect("a scratch folder");
    let unit = format!("sot-test-cj-{}", uuid::Uuid::now_v7().simple());
    let (cmd, entry) = sot_log::test_isolated::test_command(NAME);
    let mut client = spawn_client(dir.path(), &unit, &cmd, &entry);

    let (ready, exited) = wait_ready_or_exit(dir.path(), &mut client).await;
    let record = ready.as_deref().map(parse);
    let (held_before, gone, contained, emptied) = match &record {
        Some(r) => judge(dir.path(), r, &mut client).await,
        None => (true, true, false, true),
    };
    // Cleanup runs before any assert and never panics.
    let _ = client.kill();
    let _ = client.wait();
    let mut freed = match (&record, gone) {
        (Some(r), false) => end_leftover_row(r).await,
        _ => true,
    };
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
    if let (Some(r), false) = (&record, freed) {
        kill_job_group(&r["child_cgroup"], &unit);
        freed = wait_free(Path::new(&r["state_dir"]), &r["voyage"], 10).await;
    }
    let field = |k: &str| record.as_ref().map(|r| r[k].clone()).unwrap_or_default();
    if record.is_none() {
        let out = std::fs::read_to_string(dir.path().join("out"));
        eprintln!("the child's output ({}/out): {out:?}", dir.path().display());
    }
    if freed {
        for k in ["runtime", "tmp"] {
            let _ = std::fs::remove_dir_all(field(k));
        }
        drop(dir);
    } else {
        eprintln!(
            "the row is not confirmed ended: keeping its folders: {} {} {}",
            dir.path().display(),
            field("runtime"),
            field("tmp")
        );
        std::mem::forget(dir);
    }
    let seen = match exited {
        Some(after) => format!("the client had already exited after {after:?}"),
        None => "the client was still running at 120 s".to_string(),
    };
    assert!(
        record.is_some(),
        "the child never wrote its ready record ({seen})"
    );
    assert!(
        held_before,
        "precondition: the supervisor's fence and the voyage's writer lock must be held while the job runs"
    );
    entry.assert_once(field("child_pid").parse().expect("child_pid"));
    let (c, sc) = (field("child_cgroup"), field("supervisor_cgroup"));
    assert!(
        contained && gone && emptied,
        "a killed test job: contained={contained} (the supervisor's control group {sc}, the job's {c}), gone={gone} (its fence or writer lock was still held 10 s after the job ended), emptied={emptied} (the job's control group was still populated)"
    );
}
