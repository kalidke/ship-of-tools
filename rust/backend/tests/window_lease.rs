#![cfg(any(windows, target_os = "linux"))]
//! The close lifecycle's daemon half, end to end against real daemons:
//! one daemon per state root (the daemon lock, `server::run`'s first step).
//! Every daemon here is a scratch daemon on its own `Env`.

mod support;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::tokio::Stream as _;
use sot_protocol::ops::{op, FeLeaseReq};
use sot_protocol::{codec, Frame};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

use support::{call, connect_and_hello, find_row, poll_until, sotd_exe, try_connect, Conn, Env, TEST_STATE_HOST};

const BOUND: Duration = Duration::from_secs(20);

/// The daemon's info line while another holder has its lock (`server::lock_daemon`).
const WAITING_FOR_LOCK: &str = "waiting for the previous daemon on this computer to finish shutting down";

/// `sot_state_dir()` under `env`'s `XDG_STATE_HOME`/`LOCALAPPDATA`: the
/// state root joined with `sot` (`state_dir.rs`).
fn state_dir(env: &Env) -> PathBuf {
    env.state_root.join("sot")
}

/// A `sotd` on `env`'s own socket, state root, config, comm home and host
/// (the variables `Env::spawn_sotd` sets), owned by the test so its exit
/// can be awaited.
fn sotd_on(env: &Env, extra: &[(&str, &str)]) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(sotd_exe());
    cmd.arg("--socket")
        .arg(&env.socket_path)
        .arg("--project-root")
        .arg(&env.daemon_project_root)
        .env("LOCALAPPDATA", &env.state_root)
        .env("XDG_STATE_HOME", &env.state_root)
        .env("XDG_CONFIG_HOME", &env.config_root)
        .env("SOT_SELF_HOST", TEST_STATE_HOST)
        .env("SOT_RUNTIME_DIR", env._runtime_tmp.path())
        .env("HOME", &env.home_root)
        .env("USERPROFILE", &env.home_root)
        .env("SOT_COMM_HOME", &env.comm_root)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd
}

#[tokio::test]
async fn second_daemon_refuses_live() {
    let env = Env::new("lockl");
    env.spawn_sotd();
    poll_until(|| async { try_connect(&env.socket_path).await }, BOUND, "the first daemon to accept").await;

    let out = tokio::time::timeout(Duration::from_secs(5), sotd_on(&env, &[]).output())
        .await
        .expect("the second daemon did not exit within 5 s")
        .expect("spawn the second sotd");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the second daemon must refuse; stderr: {stderr}");
    assert!(try_connect(&env.socket_path).await.is_some(), "the first daemon must still answer");
    env.kill_daemon_bounded().await;
}

#[tokio::test]
async fn second_daemon_waits_for_lock() {
    let env = Env::new("lockw");
    std::fs::create_dir_all(state_dir(&env)).expect("create the state dir");
    let lock = sot_log::fence::try_lock_daemon(&state_dir(&env))
        .expect("open the daemon lock")
        .expect("the daemon lock is free");
    let mut daemon = sotd_on(&env, &[("RUST_LOG", "info")])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn sotd");

    // An unfenced start takes about as long as the hold below, so the hold
    // starts once the daemon reports that it is waiting on the lock.
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    let mut lines = tokio::io::BufReader::new(daemon.stdout.take().expect("piped stdout")).lines();
    tokio::spawn(async move {
        let mut seen_tx = Some(seen_tx);
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains(WAITING_FOR_LOCK) {
                if let Some(tx) = seen_tx.take() {
                    let _ = tx.send(());
                }
            }
        }
    });
    tokio::time::timeout(BOUND, seen_rx)
        .await
        .expect("the daemon did not report waiting on the lock within BOUND")
        .expect("the daemon's stdout closed before it reported waiting on the lock");

    let held_until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < held_until {
        assert!(
            try_connect(&env.socket_path).await.is_none(),
            "the daemon bound its socket while another holder had the lock"
        );
        let exited = daemon.try_wait().expect("try_wait");
        assert!(exited.is_none(), "the daemon exited while waiting for the lock: {exited:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    drop(lock);
    poll_until(
        || async { try_connect(&env.socket_path).await },
        Duration::from_secs(5),
        "the daemon to bind after the lock was released",
    )
    .await;
    tokio::time::timeout(BOUND, daemon.kill()).await.expect("kill the daemon within BOUND").expect("kill the daemon");
}

#[tokio::test]
async fn daemon_lock_timeout_exits_1() {
    let env = Env::new("lockt");
    std::fs::create_dir_all(state_dir(&env)).expect("create the state dir");
    let _lock = sot_log::fence::try_lock_daemon(&state_dir(&env))
        .expect("open the daemon lock")
        .expect("the daemon lock is free");

    let out = tokio::time::timeout(BOUND, sotd_on(&env, &[("SOT_TEST_DAEMON_LOCK_WAIT_MS", "1000")]).output())
        .await
        .expect("the daemon did not give up on the lock within BOUND")
        .expect("spawn sotd");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "a lock timeout exits 1; stderr: {stderr}");
    let lock_path = sot_log::fence::daemon_lock_path(&state_dir(&env));
    assert!(stderr.contains(&lock_path.display().to_string()), "the error names the lock path: {stderr}");
    assert!(try_connect(&env.socket_path).await.is_none(), "a daemon that never got the lock never binds");
}

// ---------------------------------------------------------------------
// Leases and the shutdown (A6)
// ---------------------------------------------------------------------

/// How long a test waits for a shutdown that has been decided.
const EXIT_WITHIN: Duration = Duration::from_secs(60);

/// `Env::new` points this process's `SOT_RUNTIME_DIR` at its own dir, and
/// the tests below read rows through it: one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Not a test of its own: the window the tests below spawn
/// (`SOT_TEST_LEASE_CHILD=<socket>`), a no-op otherwise. It leases with
/// its own identity and prints the answer, then obeys stdin lines
/// (`close`, `keep`, `handover`, `half`, `garbage`, `ack <n>`), printing
/// each answer, and holds the lease until stdin closes.
#[tokio::test]
async fn lease_holder_child() {
    let Ok(socket) = std::env::var("SOT_TEST_LEASE_CHILD") else { return };
    let who = sot_log::challenge::self_identity().expect("this process's identity");
    let stream = try_connect(Path::new(&socket)).await.expect("connect to the daemon");
    let (rx, mut tx) = stream.split();
    let mut rx = tokio::io::BufReader::new(rx);
    let req = FeLeaseReq { boot: who.boot, pid: who.pid, created: who.created, token: None };
    let lease = Frame::req(1, op::FE_LEASE, serde_json::to_value(&req).unwrap());
    codec::write_frame(&mut tx, &lease, None).await.expect("write fe.lease");
    let mut line = String::new();
    rx.read_line(&mut line).await.expect("read the lease answer");
    println!("LEASE {}", line.trim());

    let (lines_tx, mut lines) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut id = 2;
    while let Some(cmd) = lines.recv().await {
        let words: Vec<&str> = cmd.split_whitespace().collect();
        let bytes = match words.as_slice() {
            [intent @ ("close" | "keep" | "handover")] => {
                let f = Frame::req(id, op::FE_LEAVING, serde_json::json!({ "intent": intent }));
                format!("{}\n", serde_json::to_string(&f).unwrap())
            }
            ["ack", n] => {
                let n: u32 = n.parse().expect("ack <n>");
                let f = Frame::req(id, op::FE_NOTICE_SEEN, serde_json::json!({ "not_ended": n }));
                format!("{}\n", serde_json::to_string(&f).unwrap())
            }
            ["garbage"] => "garbage\n".to_string(),
            ["half"] => "{\"v\":2,\"id\":".to_string(),
            other => panic!("unknown command {other:?}"),
        };
        id += 1;
        tx.write_all(bytes.as_bytes()).await.expect("write to the lease");
        tx.flush().await.expect("flush the lease");
        if words[0] != "half" {
            line.clear();
            if rx.read_line(&mut line).await.unwrap_or(0) > 0 {
                println!("REPLY {}", line.trim());
            } else {
                println!("REPLY EOF");
            }
        }
    }
}

/// A window: `lease_holder_child` in a child process of this test binary.
struct Window {
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    out: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
}

impl Window {
    /// Spawns the window and returns it with its lease answer's payload.
    async fn open(socket: &Path) -> (Self, serde_json::Value) {
        let mut child = tokio::process::Command::new(std::env::current_exe().expect("this test binary"))
            .args(["--exact", "lease_holder_child", "--nocapture", "--test-threads", "1"])
            .env("SOT_TEST_LEASE_CHILD", socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn the window");
        let stdin = child.stdin.take();
        let out = tokio::io::BufReader::new(child.stdout.take().expect("piped stdout")).lines();
        let mut w = Window { child, stdin, out };
        let lease = w.next("LEASE ", BOUND).await;
        (w, lease)
    }

    /// The next line with `prefix`, as the payload of the frame it carries.
    async fn next(&mut self, prefix: &str, within: Duration) -> serde_json::Value {
        let read = async {
            loop {
                let line = self.out.next_line().await.expect("read the window").expect("the window exited");
                // libtest's own "test lease_holder_child ... " may lead the
                // first line.
                if let Some(rest) = line.find(prefix).map(|at| &line[at + prefix.len()..]) {
                    if rest == "EOF" {
                        return serde_json::json!("EOF");
                    }
                    let f: Frame = serde_json::from_str(rest).unwrap_or_else(|e| panic!("not a frame ({e}): {rest}"));
                    return f.payload;
                }
            }
        };
        tokio::time::timeout(within, read).await.unwrap_or_else(|_| panic!("no {prefix:?} line within {within:?}"))
    }

    async fn send(&mut self, cmd: &str) {
        let stdin = self.stdin.as_mut().expect("the window's stdin is open");
        stdin.write_all(format!("{cmd}\n").as_bytes()).await.expect("write to the window");
        stdin.flush().await.expect("flush the window");
    }

    /// `cmd`, then its answer.
    async fn ask(&mut self, cmd: &str, within: Duration) -> serde_json::Value {
        self.send(cmd).await;
        self.next("REPLY ", within).await
    }

    /// Closes stdin: the window drops its lease connection (EOF) and exits.
    async fn eof(&mut self) {
        drop(self.stdin.take());
        let _ = tokio::time::timeout(BOUND, self.child.wait()).await;
    }
}

/// A test-owned daemon whose stdout and stderr go to one log file.
struct Daemon {
    child: tokio::process::Child,
    log: PathBuf,
}

impl Daemon {
    async fn start(env: &Env, extra: &[(&str, &str)]) -> Self {
        let log = env._tmp.path().join(format!("sotd-{}.log", uuid_ish()));
        let file = std::fs::File::create(&log).expect("create the daemon log");
        let child = sotd_on(env, extra)
            .env("RUST_LOG", "info")
            .stdout(Stdio::from(file.try_clone().expect("clone the log")))
            .stderr(Stdio::from(file))
            .spawn()
            .expect("spawn sotd");
        poll_until(|| async { try_connect(&env.socket_path).await }, BOUND, "the daemon to accept").await;
        Daemon { child, log }
    }

    /// Its exit code, if it exits within `within`.
    async fn exit_within(&mut self, within: Duration) -> Option<i32> {
        match tokio::time::timeout(within, self.child.wait()).await {
            Ok(status) => status.expect("wait for the daemon").code(),
            Err(_) => None,
        }
    }

    fn still_up(&mut self) -> bool {
        self.child.try_wait().expect("try_wait the daemon").is_none()
    }

    fn said(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

fn uuid_ish() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
}

fn held_record(env: &Env) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(state_dir(env).join(sot_protocol::ops::lease::HELD_RECORD_FILE)).ok()?;
    Some(serde_json::from_str(&text).expect("held.json parses"))
}

fn row_toml(env: &Env, slug: &str) -> PathBuf {
    env.app_config_dir().join(format!("workspaces-{TEST_STATE_HOST}")).join(format!("{slug}.toml"))
}

/// A capsule row (no agent) created and waited to `ready`; its state dir.
async fn create_row(env: &Env, conn: &mut Conn, next_id: &mut u64, label: &str) -> (String, PathBuf) {
    let req = serde_json::json!({
        "label": label,
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let res = call(conn, *next_id, op::WORKSPACE_CREATE, req).await;
    *next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    let id = res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let payload = call(conn, *next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        *next_id += 1;
        if let Some(row) = find_row(&payload, &id) {
            if let (Some(sd), Some("ready")) = (row["state_dir"].as_str(), row["phase"].as_str()) {
                return (id, PathBuf::from(sd));
            }
        }
        assert!(Instant::now() < deadline, "the row {label} never reached ready");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The "slow row": its supervisor killed by the pid this test read, then
/// its fence held here, so no end of it can be proven.
#[cfg(target_os = "linux")]
async fn slow_row(state_dir: &Path) -> sot_log::fence::SupervisorLock {
    let dir = state_dir.to_path_buf();
    let (_status, process) = tokio::task::spawn_blocking(move || sot_log::supervisor_client::query_status(&dir))
        .await
        .unwrap()
        .expect("query_status on a ready row");
    // SAFETY: a plain kill of the supervisor this test's daemon spawned.
    unsafe { libc::kill(process.pid() as i32, libc::SIGKILL) };
    drop(process);
    let dir = state_dir.to_path_buf();
    poll_until(|| { let dir = dir.clone(); async move { sot_log::fence::lock_supervisor(&dir).ok() } }, BOUND, "the row's fence").await
}

fn capsules_left(env: &Env) -> bool {
    support::any_process_matches(&env.leg_pgrep_pattern())
}

#[tokio::test]
async fn last_one_out_two_windows() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("lease1");
    let mut daemon = Daemon::start(&env, &[]).await;
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_id, _sd) = create_row(&env, &mut conn, &mut next_id, "last-one").await;
    drop(conn);
    let (mut a, lease_a) = Window::open(&env.socket_path).await;
    let (mut b, lease_b) = Window::open(&env.socket_path).await;
    assert_eq!(lease_a["outcome"], "granted", "{lease_a:?}");
    assert_eq!(lease_b["outcome"], "granted", "{lease_b:?}");

    a.eof().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(daemon.still_up(), "the daemon shut down while a window still held a lease: {}", daemon.said());
    assert!(row_toml(&env, "last-one").exists(), "a row ended while a window still held a lease");

    b.eof().await;
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "the last window's EOF is a close: {}", daemon.said());
    assert!(!env.socket_path.exists(), "the socket outlived the daemon");
    assert!(!row_toml(&env, "last-one").exists(), "the row's toml outlived the close");
    assert!(!capsules_left(&env), "a sot-capsule for this state root outlived the close");
    assert!(held_record(&env).is_none(), "held.json outlived a clean close: {:?}", held_record(&env));
}

#[tokio::test]
async fn lease_end_any_way_departs() {
    let _serial = SERIAL.lock().await;
    for how in ["close", "half", "kill"] {
        let env = Env::new(&format!("dep{how}"));
        let mut daemon = Daemon::start(&env, &[]).await;
        let (mut w, lease) = Window::open(&env.socket_path).await;
        assert_eq!(lease["outcome"], "granted", "{how}: {lease:?}");
        match how {
            "close" => {
                let ack = w.ask("close", EXIT_WITHIN).await;
                assert_eq!(ack["not_ended"], 0, "{how}: {ack:?}");
            }
            "half" => {
                w.send("half").await;
                w.eof().await;
            }
            _ => w.child.kill().await.expect("SIGKILL the window"),
        }
        assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "{how}: the lease's end did not shut down: {}", daemon.said());
    }
}

#[tokio::test]
async fn keep_is_open_ended() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("keep");
    let mut daemon = Daemon::start(&env, &[("SOT_TEST_HANDOVER_BOUND_MS", "1000")]).await;
    let (mut w, _) = Window::open(&env.socket_path).await;
    let ack = w.ask("keep", BOUND).await;
    assert_eq!(ack["not_ended"], 0, "{ack:?}");
    w.eof().await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(daemon.still_up(), "a keep was not open-ended: {}", daemon.said());
    assert!(held_record(&env).is_none(), "a keep left a record: {:?}", held_record(&env));
    let (_w2, lease) = Window::open(&env.socket_path).await;
    assert_eq!(lease["outcome"], "granted", "{lease:?}");
}

#[tokio::test]
async fn relaunch_handover_expires_then_shuts_down() {
    let _serial = SERIAL.lock().await;
    for in_time in [false, true] {
        let env = Env::new(if in_time { "hand1" } else { "hand0" });
        let mut daemon = Daemon::start(&env, &[("SOT_TEST_HANDOVER_BOUND_MS", "3000")]).await;
        let (mut w, _) = Window::open(&env.socket_path).await;
        let ack = w.ask("handover", BOUND).await;
        assert_eq!(ack["not_ended"], 0, "{ack:?}");
        w.eof().await;
        assert!(held_record(&env).is_some_and(|r| r["handover_until_ms"].is_u64()), "the handover is not recorded");
        if in_time {
            let (_next, lease) = Window::open(&env.socket_path).await;
            assert_eq!(lease["outcome"], "granted", "{lease:?}");
            assert_eq!(daemon.exit_within(Duration::from_secs(6)).await, None, "a lease in time did not keep the daemon: {}", daemon.said());
        } else {
            assert_eq!(daemon.exit_within(Duration::from_secs(20)).await, Some(0), "an expired handover did not shut down: {}", daemon.said());
        }
    }
}

#[tokio::test]
async fn lane_connection_close_does_not_shut_down() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("lanes");
    let mut daemon = Daemon::start(&env, &[]).await;
    // No lease yet: a capture's data connection, a lane login and a
    // redial come and go.
    let (conn, _) = connect_and_hello(&env.socket_path).await;
    drop(conn);
    let mut lane = tokio::io::BufReader::new(try_connect(&env.socket_path).await.expect("connect"));
    let f = Frame::req(1, op::LANE_CONNECT, serde_json::json!({ "target": "no-such-row", "lane": "supervisor" }));
    codec::write_frame(&mut lane, &f, None).await.expect("write lane.connect");
    let _ = tokio::time::timeout(Duration::from_secs(5), codec::read_frame(&mut lane)).await;
    drop(lane);
    drop(try_connect(&env.socket_path).await.expect("redial"));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(daemon.still_up(), "a non-lease connection's end shut the daemon down: {}", daemon.said());

    // With a lease held, the same comings and goings never depart it.
    let (mut w, _) = Window::open(&env.socket_path).await;
    let (conn, _) = connect_and_hello(&env.socket_path).await;
    drop(conn);
    drop(try_connect(&env.socket_path).await.expect("redial"));
    let ack = w.ask("garbage", BOUND).await;
    assert!(ack.get("error").is_some(), "a malformed lease line is answered with an error: {ack:?}");
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(daemon.still_up(), "a non-lease connection's end shut the daemon down: {}", daemon.said());
    w.eof().await;
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "the lease was no longer held: {}", daemon.said());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn refused_end_reaches_the_window() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("refused");
    let mut daemon = Daemon::start(&env, &[("SOT_TEST_SHUTDOWN_BOUND_MS", "15000")]).await;
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_id, state_dir) = create_row(&env, &mut conn, &mut next_id, "refused").await;
    drop(conn);
    let fence = slow_row(&state_dir).await;
    let (mut w, _) = Window::open(&env.socket_path).await;
    let ack = w.ask("close", EXIT_WITHIN).await;
    assert_eq!(ack["not_ended"], 1, "the close ack carries the refused end: {ack:?}");
    let rec = held_record(&env).expect("the record carries the count");
    assert_eq!(rec["not_ended"], 1, "{rec:?}");
    assert_eq!(rec["closing"], false, "{rec:?}");
    assert!(row_toml(&env, "refused").exists(), "a row that was not ended lost its registration");
    let ack = w.ask("ack 1", BOUND).await;
    assert!(ack.get("error").is_none(), "{ack:?}");
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "{}", daemon.said());
    drop(fence);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn create_during_shutdown_refused() {
    let _serial = SERIAL.lock().await;
    let env = Env::new("createsd");
    let mut daemon = Daemon::start(&env, &[("SOT_TEST_SHUTDOWN_BOUND_MS", "15000")]).await;
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_id, state_dir) = create_row(&env, &mut conn, &mut next_id, "slow").await;
    let fence = slow_row(&state_dir).await;
    let (mut w, _) = Window::open(&env.socket_path).await;
    w.send("close").await;
    // The slow row holds the shutdown in step 3; a create on a connection
    // accepted before it is refused.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let req = serde_json::json!({
        "label": "too-late",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, req).await;
    assert!(res.payload.get("error").is_some(), "a create during the shutdown was not refused: {:?}", res.payload);
    assert!(try_connect(&env.socket_path).await.is_none(), "the dying daemon still accepted a connection");
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "{}", daemon.said());
    assert!(!row_toml(&env, "too-late").exists(), "the refused create left a row");
    drop(fence);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn shutdown_ends_a_child_that_left_the_agents_process_group() {
    let _serial = SERIAL.lock().await;
    use support::{arm_scope_guard, assert_scope_empties, cgroup_rel, user_manager_available_for_test};
    if let Err(e) = user_manager_available_for_test() {
        if std::env::var("SOT_TEST_REQUIRE_USER_MANAGER").as_deref() == Ok("1") {
            panic!("SOT_TEST_REQUIRE_USER_MANAGER=1 but no user manager is reachable: {e}");
        }
        eprintln!("SKIPPED: no user manager: {e}");
        return;
    }
    let env = Env::new("lesc");
    let (dir, pidfile) = env.seed_fake_claude_with_escapee();
    let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap_or_default());
    let mut daemon = Daemon::start(&env, &[("PATH", &path)]).await;
    let (mut w, _) = Window::open(&env.socket_path).await;
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let req = serde_json::json!({
        "label": "esc-workspace",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
        "agent": "claude",
    });
    let res = call(&mut conn, next_id, op::WORKSPACE_CREATE, req).await;
    next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    let id = res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let deadline = Instant::now() + Duration::from_secs(90);
    let state_dir = loop {
        let payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        next_id += 1;
        if let Some(row) = find_row(&payload, &id) {
            if let (Some(sd), Some("ready")) = (row["state_dir"].as_str(), row["phase"].as_str()) {
                break PathBuf::from(sd);
            }
        }
        assert!(Instant::now() < deadline, "the claude row never reached ready");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let dir = state_dir.clone();
    let (_status, process) = tokio::task::spawn_blocking(move || sot_log::supervisor_client::query_status(&dir))
        .await
        .unwrap()
        .expect("query_status on a ready row");
    let scope = cgroup_rel(process.pid());
    drop(process);
    let _guard = arm_scope_guard(&scope, &state_dir);
    let deadline = Instant::now() + Duration::from_secs(10);
    let escapee: u32 = loop {
        if let Some(e) = std::fs::read_to_string(&pidfile).ok().and_then(|t| t.trim().parse().ok()) {
            break e;
        }
        assert!(Instant::now() < deadline, "the escapee never wrote {pidfile:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(cgroup_rel(escapee), scope, "the escapee is not in the row's scope");
    drop(conn);

    let ack = w.ask("close", EXIT_WITHIN).await;
    assert_eq!(ack["not_ended"], 0, "{ack:?}");
    assert_eq!(daemon.exit_within(EXIT_WITHIN).await, Some(0), "{}", daemon.said());
    assert_scope_empties(&scope, Duration::from_secs(5)).await;
}
