#![cfg(any(windows, target_os = "linux"))]
//! The close lifecycle's daemon half, end to end against real daemons:
//! one daemon per state root (the daemon lock, `server::run`'s first step).
//! Every daemon here is a scratch daemon on its own `Env`.

#[path = "../support/mod.rs"]
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

// ---------------------------------------------------------------------
// Leases and the shutdown (A6)
// ---------------------------------------------------------------------

/// How long a test waits for a shutdown that has been decided.
const EXIT_WITHIN: Duration = Duration::from_secs(60);

/// `Env::new` points this process's `SOT_RUNTIME_DIR` at its own dir, and
/// every test reads rows through it: each takes this before `Env::new`.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Not a test of its own: the window the tests below spawn
/// (`SOT_TEST_LEASE_CHILD=<socket>`), a no-op otherwise. It leases with
/// its own identity and prints the answer, then obeys stdin lines
/// (`close`, `keep`, `handover`, `half`, `garbage`, `ack <n>`, `raw <line>`,
/// `overcap`), printing each answer, and holds the lease until stdin closes.
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
        let bytes = if let Some(text) = cmd.strip_prefix("raw ") {
            format!("{text}\n")
        } else if cmd == "overcap" {
            format!("{}\n", "x".repeat(70_000))
        } else {
            match words.as_slice() {
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
            }
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
    create_row_at(conn, next_id, label, &env.workspace_project_root).await
}

/// `create_row` on `root`: a second row needs a project root of its own.
async fn create_row_at(conn: &mut Conn, next_id: &mut u64, label: &str, root: &Path) -> (String, PathBuf) {
    let req = serde_json::json!({
        "label": label,
        "project_root": root.to_string_lossy(),
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

/// Seconds into the UTC day of `log`'s last `needle` line: a phase boundary on the daemon's clock.
#[cfg(target_os = "linux")]
fn stamped(log: &str, needle: &str) -> f64 {
    let line = log.lines().rev().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("no {needle:?} line:\n{log}"));
    let t = line.find('T').expect("an RFC 3339 time");
    let num = |s: &str| s.parse::<f64>().expect("a time field");
    num(&line[t + 1..t + 3]) * 3600.0 + num(&line[t + 4..t + 6]) * 60.0 + num(line[t + 7..].split('Z').next().unwrap())
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

#[cfg(target_os = "linux")]
fn capsules_left(env: &Env) -> bool {
    support::any_process_matches(&env.leg_pgrep_pattern())
}

mod lease;
mod lock;
