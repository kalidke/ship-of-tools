#![cfg(target_os = "linux")]
//! The inbox lock (0031 B1) across the processes that take it: the daemon's
//! filer (`comm_inbox::file_frame`, included by path because this crate is a
//! binary) and `sotd comm send` in a process of its own, which appends with
//! the same code when it takes the folder's lock itself. Both are `flock(2)`
//! on the same sidecar, so a mixed run is the proof that two processes take
//! the SAME lock.
//!
//! Two boxes on one shared folder are `comm/tests/interop-066.sh`'s, run by hand on build hosts.

#[path = "../src/comm/mail/inbox.rs"]
#[allow(dead_code)] // the forward's sentence is the binary's alone
mod comm_inbox;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use sot_protocol::op;

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;
use support::{call, connect_and_hello, Env, BOUND};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

use comm_inbox::{create_lock_record, file_frame, lock_identity, machine_id};

/// Every line one JSON object, LF-terminated; returns each line's `msg`.
fn whole_lines(p: &Path) -> Vec<String> {
    let s = std::fs::read_to_string(p).unwrap_or_default();
    assert!(s.is_empty() || s.ends_with('\n'), "a partial tail: {s:?}");
    s.lines()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).expect("one JSON object per line");
            v["msg"].as_str().unwrap_or_default().to_string()
        })
        .collect()
}

fn assert_distinct(msgs: &[String], want: usize) {
    let mut seen = msgs.to_vec();
    seen.sort();
    seen.dedup();
    assert_eq!((msgs.len(), seen.len()), (want, want), "a line was lost or doubled");
}

fn inbox_home() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("inbox")).unwrap();
    d
}

/// A holder of `inbox/<h>.lock`: takes the lock, then runs `body`. `exec` so
/// the child's pid IS the lock holder — a grandchild that inherited the fd
/// would keep the lock past the kill. The holder writes the ready file itself,
/// with a builtin: a child such as `touch` would still hold the lock when the
/// test, seeing the file, kills the holder.
fn holder(inbox: &Path, h: &str, body: &str) -> Child {
    let ready = inbox.join(format!("{h}.ready"));
    let child = Command::new("bash")
        .arg("-c")
        .arg(format!(
            r#"exec 9>> "$1/$2.lock"; flock 9; exec 8>> "$1/$2.jsonl"; : > "$3"; {body}"#
        ))
        .args(["_", inbox.to_str().unwrap(), h, ready.to_str().unwrap()])
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while !ready.exists() {
        assert!(t0.elapsed() < Duration::from_secs(5), "the holder never took the lock");
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

/// Whether process `pid` is stopped: its state, `T`, is the first field after the paren that closes its command name.
fn stopped(pid: u32) -> bool {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    let state = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next());
    state == Some("T")
}

/// `sotd comm send @<to> <text>` from a session `from` outside any row, whose self file and entry are in `home`'s
/// comm folder `<home>/comm`, run in `project`; no daemon, no topology.
fn sotd_send(home: &Path, project: &Path, from: &str, to: &str, text: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_sotd"));
    c.args(["comm", "send", &format!("@{to}"), text])
        .current_dir(project)
        .env("SOT_COMM_HOME", home.join("comm"))
        .env("SOT_COMM_SELF_FILE", home.join(format!("self-{from}.txt")))
        .env("SOT_SELF_HOST", "box1")
        .env("SOT_SOCKET", home.join("no-daemon.sock"))
        .env("SOT_HOSTS", home.join("no-hosts.toml"))
        .env("SOT_INBOX_LOCK_WAIT_SECS", "60")
        .env_remove("SOT_WORKSPACE_ID")
        .env_remove("SOT_COMM_NAME");
    c
}

/// A comm folder under `home` whose inbox lock record names this machine's own lock manager (so `sotd comm send`
/// appends itself), a live recipient `to`, and a joined sender `from` rooted at `project`.
fn sending_home(home: &Path, project: &Path, from: &str, to: &str) {
    let comm = home.join("comm");
    std::fs::create_dir_all(comm.join("inbox")).unwrap();
    for d in ["self", "read"] {
        std::fs::create_dir_all(comm.join(d)).unwrap();
    }
    let id = lock_identity(&comm.join("inbox"));
    assert_ne!(id, "none", "this test needs a folder whose lock manager is provable");
    create_lock_record(&comm, &id, machine_id().as_deref()).unwrap();
    let root = std::fs::canonicalize(project).unwrap().to_string_lossy().into_owned();
    // A stamp no clock reaches is fresh to every reader: the entries are live for the whole run.
    let now = "2999-01-01T00:00:00Z";
    let reg = serde_json::json!({"protocol_version": 1, "agents": {
        from: {"host": "box1", "workspace_id": "", "root": root, "last_seen": now},
        to: {"host": "box1", "workspace_id": "", "root": "/elsewhere", "last_seen": now},
    }});
    std::fs::write(comm.join("registry.json"), reg.to_string()).unwrap();
    std::fs::write(home.join(format!("self-{from}.txt")), format!("{from}\nrepo=r\nroot={root}\n")).unwrap();
}

// T3 (mixed) — the filer in this process and `sotd comm send` in processes of their own on one inbox at once: 400
// whole lines, none lost or doubled. It proves exclusion, not the wait: a loaded runner can stall one append's sync
// past the 2 s default, so both arms wait 60 s; the frozen-holder tests own the expiry.
#[test]
fn the_filer_and_sotd_comm_send_take_the_same_lock() {
    let d = inbox_home();
    let project = d.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    sending_home(d.path(), &project, "sh", "t3");
    let inbox = d.path().join("comm/inbox");
    let id = lock_identity(&inbox);
    let senders = {
        let (home, project) = (d.path().to_path_buf(), project.clone());
        std::thread::spawn(move || {
            for i in 0..200 {
                let ran = sot_log::test_wait::run_within(&mut sotd_send(&home, &project, "sh", "t3", &format!("sh-{i}")), BOUND);
                assert!(ran.status.success() && ran.stdout.contains("filed -> @t3"), "{ran}");
            }
        })
    };
    for i in 0..200 {
        file_frame(&inbox, "rust", "t3", false, &format!("rust-{i}"), "t", Duration::from_secs(60), &id).unwrap();
    }
    senders.join().unwrap();
    assert_distinct(&whole_lines(&inbox.join("t3.jsonl")), 400);
}

// T12 (Rust arm) — a killed holder costs nothing: the OS released the lock.
#[test]
fn a_killed_holder_frees_the_lock_at_once() {
    let d = inbox_home();
    let inbox = d.path().join("inbox");
    let mut h = holder(&inbox, "k", "exec sleep 60");
    h.kill().unwrap();
    h.wait().unwrap();
    let lock = std::fs::OpenOptions::new().read(true).append(true).create(true).open(inbox.join("k.lock")).unwrap();
    lock.try_lock().expect("the killed holder's lock is still held");
    drop(lock);
    file_frame(&inbox, "rust", "k", false, "after", "t", Duration::from_secs(10), "local t").unwrap();
    assert_eq!(whole_lines(&inbox.join("k.jsonl")), ["after"]);
}

// T12 (Rust arm) — a frozen holder: the filer waits, fails, appends nothing;
// the holder resumes into a whole line; the next append files.
#[test]
fn a_frozen_holder_makes_the_filer_wait_then_fail() {
    let d = inbox_home();
    let inbox = d.path().join("inbox");
    let mut h = holder(
        &inbox,
        "f",
        r#"printf '%s' '{"from":"holder",' >&8; kill -STOP $$; printf '%s\n' '"msg":"resumed"}' >&8"#,
    );
    // The ready file comes before the body: a CONT sent before the holder's own STOP does nothing, and the
    // holder would then stay stopped for good. So wait for the STOP first, as the shell arm does.
    let t0 = Instant::now();
    while !stopped(h.id()) {
        if t0.elapsed() >= Duration::from_secs(5) {
            h.kill().unwrap();
            panic!("the holder never stopped itself");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let t0 = Instant::now();
    let e = file_frame(&inbox, "rust", "f", false, "frozen", "t", Duration::from_secs(1), "local t").unwrap_err();
    assert!(t0.elapsed() >= Duration::from_secs(1));
    assert_eq!(e, "the inbox lock for @f was held for 1s — nothing was appended");
    sot_log::test_wait::run_within(Command::new("kill").args(["-CONT", &h.id().to_string()]), BOUND).ok();
    sot_log::test_isolated::wait_within(&mut h, BOUND);
    assert_eq!(whole_lines(&inbox.join("f.jsonl")), ["resumed"]);
    file_frame(&inbox, "rust", "f", false, "after", "t", Duration::from_secs(1), "local t").unwrap();
    assert_eq!(whole_lines(&inbox.join("f.jsonl")), ["resumed", "after"]);
}

// S2 (Rust arm) — a write the file-size limit cuts off mid-line: the filer
// answers the append failed and the file is byte-identical. The filer runs
// in a child (this binary, the ignored case below) under `ulimit -f 1`
// (1024 bytes) with SIGXFSZ ignored, so the write returns EFBIG after a
// partial write instead of killing it. The fsync itself is read, not tested.
#[test]
fn a_write_cut_short_by_the_file_size_limit_leaves_the_file_byte_identical() {
    let d = inbox_home();
    let inbox = d.path().join("inbox");
    let before = format!("{{\"msg\":\"{}\"}}\n", "x".repeat(1000 - 11));
    assert_eq!(before.len(), 1000);
    std::fs::write(inbox.join("z.jsonl"), &before).unwrap();
    let child = Command::new("bash")
        .arg("-c")
        .arg(r#"ulimit -f 1; trap '' XFSZ; exec "$@""#)
        .args(["_", std::env::current_exe().unwrap().to_str().unwrap()])
        .args(["--exact", "fsize_child_files_one", "--ignored", "--nocapture", "--test-threads=1"])
        .env("SOT_TEST_INBOX_DIR", &inbox)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Drained while it runs and waited on within a bound; its stderr reaches this test's own output.
    let (status, stdout, _) =
        sot_log::test_isolated::drain(child).wait_within(sot_log::test_isolated::ISOLATION_TIMEOUT);
    assert!(stdout.contains("FAILED the append failed: "), "{status}: {stdout}");
    assert_eq!(std::fs::read_to_string(inbox.join("z.jsonl")).unwrap(), before);
}

#[test]
#[ignore = "run by a_write_cut_short_by_the_file_size_limit_leaves_the_file_byte_identical"]
fn fsize_child_files_one() {
    let dir = PathBuf::from(std::env::var_os("SOT_TEST_INBOX_DIR").expect("SOT_TEST_INBOX_DIR"));
    match file_frame(&dir, "rust", "z", false, &"y".repeat(200), "t", Duration::from_secs(1), "local t") {
        Ok(()) => println!("filed"),
        Err(e) => println!("FAILED {e}"),
    }
}

/// Stages the scripts and runs `comm-join.sh` for row `ws` as that row's session, on the raw host `raw` under the declared host `declared`.
fn join_in_row(env: &Env, ws: &str, handle: &str, raw: &str, declared: &str) {
    let bin = env._tmp.path().join("bin");
    sot_log::test_wait::run_within(
        Command::new("bash")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../comm/tests/stage-bin.sh"
            ))
            .arg(&bin),
        BOUND,
    )
    .ok();
    // Beneath a stand-in for the row's capsule (the process walk reads only the command line), as the session is.
    sot_log::test_wait::run_within(
        Command::new("bash")
            .args(["-c", "exec -a sot-capsule bash -c 'shift; \"$@\"; exit $?' _ \"/in-row/state/workspaces/$0/voyages/v0\" timeout 60 bash \"$1\" --name \"$2\""])
            .arg(ws)
            .arg(bin.join("comm-join.sh"))
            .arg(handle)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            // The wrapper runs this build's sotd, never one an install put on PATH.
            .env("SOTD_BIN", env!("CARGO_BIN_EXE_sotd"))
            .env("HOME", &env.home_root)
            .env("SOT_COMM_HOME", &env.comm_root)
            .env("SOT_COMM_TEST_HOST", raw)
            .env("SOT_SELF_HOST", declared)
            .env("SOT_SOCKET", &env.socket_path)
            .env("SOT_WORKSPACE_ID", ws)
            .env("XDG_RUNTIME_DIR", env._runtime_tmp.path())
            .env("TMPDIR", env._tmp.path())
            .current_dir(&env.workspace_project_root),
        BOUND,
    )
    .ok();

}

fn registry_of(env: &Env) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(env.comm_root.join("registry.json")).unwrap()).unwrap()
}

/// The host a session and the daemon agree on (C2): a box whose declared host (`SOT_SELF_HOST`) differs from its raw
/// `hostname -s` still joins under the daemon's host. The registry row, the unpinned self slot, the unread clear on
/// activation and the destroy prune all key on that one fact; a raw-host registry row is invisible to the daemon.
#[tokio::test]
async fn a_join_under_a_distinct_declared_host_binds_clears_and_is_pruned() {
    const DECLARED: &str = "Declared-Box";
    const RAW: &str = "rawbox";
    const HANDLE: &str = "hostjoin";
    let _serial = SERIAL.lock().await;
    assert!(
        support::sot_capsule_exe().is_file(),
        "{} not found next to sotd; build it first",
        support::CAPSULE_EXE_NAME
    );
    let env = Env::new("cfhost");
    let stub = env._tmp.path().join("stubbin");
    std::fs::create_dir_all(&stub).unwrap();
    sot_log::test_exec::write_executable(&stub.join("claude"), "#!/bin/sh\nexec sleep 600\n");
    env.spawn_sotd_with_path_and_env(&stub, &[("SOT_SELF_HOST", DECLARED)]);
    let (mut conn, mut id) = connect_and_hello(&env.socket_path).await;
    let create = serde_json::json!({
        "label": "host-row", "project_root": env.workspace_project_root.to_string_lossy(), "runtime": "capsule", "agent": "claude",
    });
    let res = call(&mut conn, id, op::WORKSPACE_CREATE, create).await;
    id += 1;
    assert!(
        res.payload.get("error").is_none(),
        "workspace.create failed: {:?}",
        res.payload
    );
    let ws = res.payload["workspace_id"]
        .as_str()
        .expect("workspace_id")
        .to_string();

    join_in_row(&env, &ws, HANDLE, RAW, DECLARED);

    let registry = || registry_of(&env);
    assert_eq!(
        registry()["agents"][HANDLE]["host"],
        DECLARED,
        "the registry row carries the daemon's declared host"
    );
    let slot = env
        .comm_root
        .join("self")
        .join(format!("{DECLARED}__{ws}.txt"));
    assert!(
        slot.is_file(),
        "the unpinned self slot is keyed by the declared host: {:?}",
        std::fs::read_dir(env.comm_root.join("self"))
            .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
    );
    assert!(
        !env.comm_root
            .join("self")
            .join(format!("{RAW}__{ws}.txt"))
            .exists(),
        "no raw-host slot"
    );

    support::write_registry(&env.comm_root, |reg| {
        reg["agents"][HANDLE]["done"] = serde_json::json!(true);
        reg["agents"][HANDLE]["state"] = serde_json::json!("done");
    });
    let act = call(
        &mut conn,
        id,
        op::WORKSPACE_ACTIVATE,
        serde_json::json!({ "workspace_id": ws, "read": true }),
    )
    .await;
    id += 1;
    assert!(
        act.payload.get("error").is_none(),
        "activate: {:?}",
        act.payload
    );
    support::poll_until(
        || async {
            registry()["agents"][HANDLE]
                .get("done")
                .is_none()
                .then_some(())
        },
        BOUND,
        "the daemon to clear the joined handle's unread on activation",
    )
    .await;

    let destroy = call(
        &mut conn,
        id,
        op::WORKSPACE_DESTROY,
        serde_json::json!({ "workspace_id": ws }),
    )
    .await;
    assert!(
        destroy.payload.get("error").is_none(),
        "destroy: {:?}",
        destroy.payload
    );
    assert!(
        registry()["agents"].get(HANDLE).is_none(),
        "destroy prunes the joined handle: {}",
        registry()
    );
    env.kill_daemon_bounded().await;
}
