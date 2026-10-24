//! A row a 0.6 release left is retired before its first 0.7 start: its old supervisor is asked to end the run, then stopped,
//! a person's end is kept, and the 0.6 files move into `.retired-voyages/`.
//!
//! The old supervisor is a stand-in process (this test binary run again on `standin_process`, so it has a pid of its own
//! that the daemon's challenge proves and waits on). It holds the row's fence and its terminal's writer lock and speaks the
//! frozen 0.6 frames on `<runtime dir>/supervisor-<h>.sock`, logging what it receives.

use super::*;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, Command, Stdio};

const V: &str = "01900000-0000-7000-8000-000000000001";

fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = b"SOSV".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    out
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.push(s.len() as u8);
    out.extend_from_slice(s.as_bytes());
}

fn hello_ok(pid: u32, created: u64) -> Vec<u8> {
    let mut body = vec![0x81];
    body.extend_from_slice(&1u32.to_le_bytes());
    put_str(&mut body, "standin");
    body.extend_from_slice(&pid.to_le_bytes());
    body.extend_from_slice(&created.to_le_bytes());
    frame(&body)
}

fn status_ok(pid: u32, created: u64, phase: u8) -> Vec<u8> {
    let mut body = vec![0x84];
    body.extend_from_slice(&pid.to_le_bytes());
    body.extend_from_slice(&created.to_le_bytes());
    body.push(1);
    put_str(&mut body, V);
    body.push(1);
    body.extend_from_slice(&1u64.to_le_bytes());
    body.push(phase);
    frame(&body)
}

fn operation(state: u8) -> Vec<u8> {
    frame(&[0x83, state])
}

fn read_frame(stream: &mut UnixStream) -> Option<Vec<u8>> {
    let mut head = [0u8; 8];
    stream.read_exact(&mut head).ok()?;
    let len = u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).ok()?;
    Some(body)
}

fn string_at(body: &[u8], at: usize) -> (String, usize) {
    let n = usize::from(body[at]);
    (String::from_utf8_lossy(&body[at + 1..at + 1 + n]).into_owned(), at + 1 + n)
}

fn log_line(log: &Path, line: &str) {
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(log).expect("open the stand-in's log");
    writeln!(file, "{line}").expect("write the stand-in's log");
}

/// Waits until `count` files are in `dir`, 60 s at most.
fn barrier_met(dir: &Path, count: usize) -> bool {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0) >= count {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// The stand-in's own process: a test of this file launched again with its parameters in the environment. Under a normal
/// run it is a test that does nothing.
#[test]
fn standin_process() {
    let Ok(state_dir) = std::env::var("SOT_RETIRE_STANDIN_STATE") else { return };
    let state_dir = PathBuf::from(state_dir);
    let sock = PathBuf::from(std::env::var("SOT_RETIRE_STANDIN_SOCK").unwrap());
    let log = PathBuf::from(std::env::var("SOT_RETIRE_STANDIN_LOG").unwrap());
    let barrier = std::env::var("SOT_RETIRE_STANDIN_BARRIER").ok().map(|dir| {
        (PathBuf::from(dir), std::env::var("SOT_RETIRE_STANDIN_COUNT").unwrap().parse::<usize>().unwrap())
    });

    let mut fence = Some(sot_log::supervisor::journal::fence::lock_supervisor(&state_dir).expect("hold the fence"));
    let mut writer = Some(sot_platform::host::lock_writer(&state_dir.join("voyages").join(V).join("writer.lock")).expect("hold the writer lock"));
    let listener = UnixListener::bind(&sock).expect("bind the old supervisor's socket");
    let pid = std::process::id();
    let created = sot_platform::identity::challenge_unix::self_start_ticks().expect("own start time");
    let mut phase = 1u8;
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        while let Some(body) = read_frame(&mut stream) {
            match body[0] {
                0x01 => {
                    log_line(&log, "Hello");
                    stream.write_all(&hello_ok(pid, created)).unwrap();
                }
                0x03 => {
                    log_line(&log, "Status");
                    stream.write_all(&status_ok(pid, created, phase)).unwrap();
                }
                0x02 => {
                    let (op_id, at) = string_at(&body, 1);
                    match body[at] {
                        0x01 => {
                            let (_reason, at) = string_at(&body, at + 1);
                            let (voyage, _) = string_at(&body, at);
                            log_line(&log, &format!("EndRun {voyage} {op_id}"));
                            stream.write_all(&operation(0x01)).unwrap();
                            if let Some((dir, count)) = &barrier {
                                std::fs::write(dir.join(pid.to_string()), b"").unwrap();
                                if !barrier_met(dir, *count) {
                                    log_line(&log, "BARRIER-TIMEOUT");
                                    stream.write_all(&frame(&[0x83, 0x07, 7, b't', b'i', b'm', b'e', b'o', b'u', b't'])).unwrap();
                                    continue;
                                }
                            }
                            writer = None;
                            phase = 3;
                            stream.write_all(&operation(0x03)).unwrap();
                        }
                        0x03 => {
                            log_line(&log, &format!("Stop {op_id}"));
                            stream.write_all(&operation(0x06)).unwrap();
                            // 0.6 exits a stopped supervisor once its connection is gone.
                            let mut rest = Vec::new();
                            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                            let _ = stream.read_to_end(&mut rest);
                            drop(writer.take());
                            drop(fence.take());
                            std::process::exit(0);
                        }
                        other => panic!("an op the stand-in does not know: {other}"),
                    }
                }
                other => panic!("a request the stand-in does not know: {other}"),
            }
        }
    }
}

/// One 0.6 row: its folder as 0.6.6 left it, and the stand-in that stands for its supervisor.
struct OldRow {
    id: String,
    state_dir: PathBuf,
    log: PathBuf,
    child: Option<Child>,
}

impl Drop for OldRow {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl OldRow {
    /// The registered row and its 0.6 folder, no supervisor yet.
    fn seed(env: &Env, id: &str, slug: &str) -> Self {
        let project = env._tmp.path().join(format!("project-{slug}"));
        std::fs::create_dir_all(&project).unwrap();
        env.seed_capsule_toml(id, slug, &project, "none");
        let state_dir = env.state_root.join("sot").join("workspaces").join(id);
        std::fs::create_dir_all(state_dir.join("voyages").join(V)).unwrap();
        std::fs::create_dir_all(state_dir.join("supervisor-journal")).unwrap();
        std::fs::write(state_dir.join("supervisor.lock"), b"{}").unwrap();
        std::fs::write(state_dir.join("drawer.voyage"), V).unwrap();
        std::fs::write(state_dir.join("voyages").join(V).join("writer.lock"), b"").unwrap();
        std::fs::write(state_dir.join("rollout-evidence.json"), b"{}").unwrap();
        let log = env._tmp.path().join(format!("standin-{slug}.log"));
        OldRow { id: id.to_string(), state_dir, log, child: None }
    }

    /// Starts the stand-in and waits until it listens.
    fn standin(&mut self, env: &Env, barrier: Option<(&Path, usize)>) {
        let h = sot_platform::host::state_dir::state_dir_hash(&self.state_dir);
        let sock = env._runtime_tmp.path().join(format!("supervisor-{h}.sock"));
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "retire::standin_process", "--nocapture", "--test-threads=1"])
            .env("SOT_RETIRE_STANDIN_STATE", &self.state_dir)
            .env("SOT_RETIRE_STANDIN_SOCK", &sock)
            .env("SOT_RETIRE_STANDIN_LOG", &self.log)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some((dir, count)) = barrier {
            command.env("SOT_RETIRE_STANDIN_BARRIER", dir).env("SOT_RETIRE_STANDIN_COUNT", count.to_string());
        }
        #[allow(clippy::disallowed_methods, reason = "a test's own stand-in process, killed by its handle when the row is dropped")]
        let child = command.spawn().expect("start the stand-in");
        self.child = Some(child);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !sock.exists() {
            assert!(Instant::now() < deadline, "the stand-in never bound {sock:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// What the stand-in received, in order.
    fn seen(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log).unwrap_or_default().lines().map(str::to_string).collect()
    }

    fn retired_dir(&self) -> PathBuf {
        self.state_dir.join(".retired-voyages")
    }

    fn assert_layout_retired(&self) {
        for name in ["voyages", "drawer.voyage", "supervisor-journal", "rollout-evidence.json"] {
            assert!(!self.state_dir.join(name).exists(), "{name} is still in the row's folder");
            assert!(self.retired_dir().join(name).exists(), "{name} is not in .retired-voyages");
        }
    }

    async fn stop_new_supervisor(&self) {
        let dir = self.state_dir.clone();
        let _ = tokio::task::spawn_blocking(move || sot_log::attach_client::supervisor_client::stop(&dir)).await;
    }
}

fn require_capsule() {
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd at {:?} — build it first", sot_capsule_exe());
}

async fn phase_of_row(conn: &mut Conn, next_id: &mut u64, id: &str) -> Option<String> {
    let payload = call(conn, *next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    *next_id += 1;
    find_row(&payload, id).and_then(|row| row["phase"].as_str().map(str::to_string))
}

/// The booting daemon finds a 0.6 supervisor answering with its run ready. It ends the run, stops the supervisor, moves the
/// 0.6 files aside and starts the row under a 0.7 supervisor, which is the only one left.
#[tokio::test]
async fn an_old_supervisor_is_retired_at_boot() {
    let _serial = SERIAL.lock().await;
    require_capsule();
    let env = Env::new("rt1");
    let mut row = OldRow::seed(&env, "ws-old-1", "old_one");
    row.standin(&env, None);

    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    poll_for_phase(&mut conn, &mut next_id, &row.id, "ready", BOUND.max(Duration::from_secs(90))).await;

    let seen = row.seen();
    assert_eq!(seen.len(), 4, "{seen:?}");
    assert_eq!(seen[0], "Hello");
    assert_eq!(seen[1], "Status");
    assert!(seen[2].starts_with(&format!("EndRun {V} sot-retire-")), "{seen:?}");
    assert!(seen[3].starts_with("Stop sot-retire-"), "{seen:?}");
    row.assert_layout_retired();
    assert!(!row.state_dir.join("ended").exists(), "an end the retire asked for is not a person's");
    let status = try_query_status(row.state_dir.clone()).await.expect("a 0.7 supervisor answers on the row socket");
    assert_ne!(Some(status.pid), row.child.as_ref().map(Child::id), "the old supervisor is gone");

    row.stop_new_supervisor().await;
    env.kill_daemon_bounded().await;
}

/// An end the person asked for under 0.6 is kept. Here no supervisor lives: its journal holds a closed `end_run` on the
/// pointer's voyage, so the 0.7 supervisor rests and records the end as requested.
#[tokio::test]
async fn a_0_6_rows_requested_end_is_kept_at_boot() {
    let _serial = SERIAL.lock().await;
    require_capsule();
    let env = Env::new("rt2");
    let row = OldRow::seed(&env, "ws-old-2", "old_two");
    let digest = "ab".repeat(32);
    let journal = row.state_dir.join("supervisor-journal");
    std::fs::write(
        journal.join(format!("{digest}.active")),
        format!(r#"{{"schema_version":1,"operation_id":"sot-backend-end-run-0190","digest":"{digest}","op":{{"op":"end_run","voyage":"{V}","epoch":3}}}}"#),
    )
    .unwrap();
    std::fs::write(journal.join(format!("{digest}.closed")), r#"{"schema_version":1}"#).unwrap();

    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    poll_for_phase(&mut conn, &mut next_id, &row.id, "ended_no_respawn", BOUND.max(Duration::from_secs(90))).await;

    row.assert_layout_retired();
    let ended = sot_log::capsule::read_ended(&row.state_dir).expect("read the ended record").expect("an ended record");
    assert!(ended.requested, "{ended:?}");

    row.stop_new_supervisor().await;
    env.kill_daemon_bounded().await;
}

/// A 0.6 terminal that outlives its supervisor holds its writer lock: no agent is started beside it, the row reads
/// `unreachable` and its files stay where they are. Once the terminal is gone, selecting the row starts it.
#[tokio::test]
async fn a_row_whose_old_terminal_still_runs_is_not_started() {
    let _serial = SERIAL.lock().await;
    require_capsule();
    let env = Env::new("rt3");
    let row = OldRow::seed(&env, "ws-old-3", "old_three");
    let held = sot_platform::host::lock_writer(&row.state_dir.join("voyages").join(V).join("writer.lock")).expect("hold the old terminal's lock");

    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let log = env.daemon_log();
    poll_until(
        || {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            async move { (text.contains("row resolution failed") && text.contains("a 0.6 terminal still runs")).then_some(()) }
        },
        BOUND.max(Duration::from_secs(90)),
        "the resume scan to refuse the row",
    )
    .await;
    assert_eq!(phase_of_row(&mut conn, &mut next_id, &row.id).await.as_deref(), Some("unreachable"));
    let h = sot_platform::host::state_dir::state_dir_hash(&row.state_dir);
    assert!(!env._runtime_tmp.path().join(format!("row-{h}.sock")).exists(), "no supervisor was started");
    assert!(row.state_dir.join("voyages").is_dir() && row.state_dir.join("drawer.voyage").is_file(), "nothing moved");
    assert!(!row.retired_dir().exists());

    drop(held);
    let payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let target = find_row(&payload, &row.id).expect("the row")["session_name"].as_str().expect("session_name").to_string();
    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": target });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);
    poll_for_phase(&mut conn, &mut next_id, &row.id, "ready", BOUND.max(Duration::from_secs(90))).await;
    row.assert_layout_retired();

    row.stop_new_supervisor().await;
    env.kill_daemon_bounded().await;
}

/// More 0.6 rows than the resume scan runs at once are retired together: every stand-in holds its `EndRun` reply until all
/// five have been asked, which waves of four could never reach.
#[tokio::test]
async fn old_rows_are_retired_together_at_boot() {
    let _serial = SERIAL.lock().await;
    require_capsule();
    let env = Env::new("rt9");
    let barrier = env._tmp.path().join("end-barrier");
    std::fs::create_dir_all(&barrier).unwrap();
    let mut rows: Vec<OldRow> = (1..=5).map(|n| OldRow::seed(&env, &format!("ws-old-9{n}"), &format!("old_nine_{n}"))).collect();
    for row in &mut rows {
        row.standin(&env, Some((&barrier, 5)));
    }

    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    for row in &rows {
        poll_for_phase(&mut conn, &mut next_id, &row.id, "ready", Duration::from_secs(150)).await;
    }
    for row in &rows {
        let seen = row.seen();
        assert!(!seen.iter().any(|l| l == "BARRIER-TIMEOUT"), "{}: the fifth end never came while four were held: {seen:?}", row.id);
        assert!(seen.iter().any(|l| l.starts_with("EndRun ")), "{}: {seen:?}", row.id);
        row.assert_layout_retired();
    }

    for row in &rows {
        row.stop_new_supervisor().await;
    }
    env.kill_daemon_bounded().await;
}

/// A close that meets an old row (a Cleanup boot ends rows the same way): `workspace.destroy` reaches `end_run`, which
/// retires the row first, asking the old supervisor to end its run and stopping it, and only then judges the row ended.
/// The folder holds no fence file at boot, so the boot resume leaves the row alone; the stand-in starts afterwards.
#[tokio::test]
async fn a_close_retires_an_old_row_it_meets() {
    let _serial = SERIAL.lock().await;
    require_capsule();
    let env = Env::new("rt5");
    let mut row = OldRow::seed(&env, "ws-old-5", "old_five");
    std::fs::remove_file(row.state_dir.join("supervisor.lock")).unwrap();

    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    assert_eq!(phase_of_row(&mut conn, &mut next_id, &row.id).await.as_deref(), Some("stopped"));
    row.standin(&env, None);

    let destroy = call(&mut conn, next_id, op::WORKSPACE_DESTROY, serde_json::json!({ "workspace_id": row.id })).await;
    assert!(destroy.payload.get("error").is_none(), "workspace.destroy failed: {:?}", destroy.payload);
    let seen = row.seen();
    assert_eq!(seen.len(), 4, "{seen:?}");
    assert!(seen[2].starts_with(&format!("EndRun {V} sot-retire-")), "{seen:?}");
    assert!(seen[3].starts_with("Stop sot-retire-"), "{seen:?}");
    env.kill_daemon_bounded().await;
}
