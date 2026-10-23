#![cfg(target_os = "linux")]
//! The daemon's management calls on the row socket (pass 6c-1b1): status, end_run, reset and stop are ops of `row-<h>`,
//! answered by the supervisor beside the viewers' ops. The daemon's client dials only the row number, never the wire
//! integer; an `end_run` is fenced by the run it names.

use serde_json::{json, Value};
use sot_log::attach_client::supervisor_client;
use sot_log::lane::wire::SupervisorPhase;
use sot_log::row::{socket_path_in, RowClient, RowEvent, ROW_PROTOCOL};
use sot_platform::host::state_dir::state_dir_hash;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BOUND: Duration = Duration::from_secs(5);

/// One test at a time: the daemon's client reads the runtime folder from this process's environment.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn private_runtime_dir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let runtime = tempfile::Builder::new().prefix("sot-r").tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    runtime
}

struct Supervisor(Child);

impl Drop for Supervisor {
    fn drop(&mut self) {
        // The supervisor's end ends the agent: the kernel's parent-death signal follows the thread that forked it.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A real supervisor on a fresh state folder, with an agent that writes and then echoes what it is typed. Returns it
/// with the row socket's path.
fn supervisor(runtime: &Path, state_dir: &Path, env: &[(&str, &str)]) -> (Supervisor, PathBuf) {
    std::fs::create_dir_all(state_dir).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sot-capsule"));
    cmd.arg("supervise")
        .arg(state_dir)
        .arg("--start")
        .arg("--assume-no-rollback-target")
        .arg("--")
        .args(["/bin/sh", "-c", "echo agent-was-here; exec cat"])
        .env("SOT_RUNTIME_DIR", runtime)
        .envs(env.iter().copied())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let supervisor = Supervisor(cmd.spawn().expect("start a supervisor"));
    let path = socket_path_in(runtime, &state_dir_hash(state_dir)).expect("the path fits");
    (supervisor, path)
}

fn dial(path: &Path) -> RowClient {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match RowClient::connect(path, "test", BOUND) {
            Ok(c) => return c,
            Err(e) => assert!(Instant::now() < deadline, "the row socket never answered: {e}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn read_until(client: &mut RowClient, needle: &str, within: Duration) {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match client.next_event(Duration::from_millis(100)).expect("the row stays up") {
            Some(RowEvent::Output(b)) => seen.extend_from_slice(&b),
            Some(RowEvent::Snapshot { bytes, last }) if last => {
                let mut p = vt100_ctt::Parser::new(24, 80, 0);
                p.restore_screen(&bytes).expect("the snapshot restores");
                seen.extend_from_slice(p.screen().contents().as_bytes());
            }
            _ => {}
        }
        if String::from_utf8_lossy(&seen).contains(needle) {
            return;
        }
    }
    panic!("no {needle:?} within {within:?}; saw {:?}", String::from_utf8_lossy(&seen));
}

fn status(client: &mut RowClient) -> Value {
    client.call("row.status", json!({}), BOUND).expect("row.status is answered").payload
}

/// A stand-in row on `path` that writes every frame at `v` and declares `row_number` in its hello. It answers the hello
/// with this process's own identity and `row.status` with `ready`, for every connection that comes, until `stop`.
fn stand_in_row(path: &Path, row_number: u32, v: u32, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) -> std::thread::JoinHandle<()> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::Ordering;
    let listener = UnixListener::bind(path).expect("bind the stand-in");
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            let Ok((stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            stream.set_nonblocking(false).unwrap();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut out = stream;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    let req: sot_protocol::Frame = serde_json::from_str(line.trim_end()).expect("a request frame");
                    line.clear();
                    let (pid, created) = sot_log::row::self_identity().expect("own identity");
                    let payload = match req.op.as_str() {
                        "row.hello" => json!({"pid": pid, "created": created, "phase": "ready", "run": "run-one", "row": row_number}),
                        "row.status" => json!({"phase": "ready", "run": "run-one", "voyage": "voyage-one"}),
                        _ => json!({"error": "no such op", "code": "unknown_op"}),
                    };
                    let mut res = sot_protocol::Frame::res(req.id, &req.op, payload);
                    res.v = v;
                    let mut text = serde_json::to_vec(&res).unwrap();
                    text.push(b'\n');
                    if out.write_all(&text).is_err() {
                        return;
                    }
                }
            });
        }
    })
}

/// The daemon's client trusts the row number and never the wire integer: a row that writes an older `v` is read, and a
/// row that declares another row number is `VersionSkew`, which rows reads as `foreign`.
#[test]
fn a_daemon_dial_checks_the_row_number_not_the_wire_integer() {
    let _serial = serial();
    // The challenge refuses a peer that started within the clock tick of the connect.
    std::thread::sleep(Duration::from_millis(50));
    let runtime = private_runtime_dir();
    std::env::set_var("SOT_RUNTIME_DIR", runtime.path());
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let path = socket_path_in(runtime.path(), &state_dir_hash(&state_dir)).expect("the path fits");

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let row = stand_in_row(&path, ROW_PROTOCOL, sot_protocol::PROTOCOL_VERSION - 1, stop.clone());
    let (report, _process) = supervisor_client::query_status(&state_dir).expect("the current row number is read whatever v says");
    assert_eq!(report.phase, SupervisorPhase::Ready);
    stop.store(true, std::sync::atomic::Ordering::Release);
    row.join().unwrap();
    std::fs::remove_file(&path).unwrap();

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let row = stand_in_row(&path, ROW_PROTOCOL + 1, sot_protocol::PROTOCOL_VERSION + 1, stop.clone());
    let err = supervisor_client::query_status(&state_dir).err().expect("another row number is refused");
    assert!(matches!(err, sot_log::Error::VersionSkew), "named: {err:?}");
    stop.store(true, std::sync::atomic::Ordering::Release);
    row.join().unwrap();
}

/// An `end_run` is fenced by the run it names: another run's id is refused `stale_run` and changes nothing.
#[test]
fn an_end_run_naming_another_run_is_refused_with_no_change() {
    let _serial = serial();
    let runtime = private_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let (_supervisor, path) = supervisor(runtime.path(), &dir.path().join("state"), &[]);
    let mut driver = dial(&path);
    driver.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut driver, "agent-was-here", Duration::from_secs(20));

    let mut manager = dial(&path);
    let before = status(&mut manager);
    let other = uuid::Uuid::now_v7().to_string();
    let reply = manager
        .call("row.end_run", json!({"run": other, "reason": "test"}), Duration::from_secs(30))
        .expect("an end_run naming another run is answered, not faulted");
    assert_eq!(reply.payload["outcome"], "stale_run", "{reply:?}");
    let after = status(&mut manager);
    assert_eq!(after["phase"], before["phase"], "nothing changed");
    assert_eq!(after["run"], before["run"], "nothing changed");
    driver.send_input(b"still-here\r").expect("the driver still types");
    read_until(&mut driver, "still-here", Duration::from_secs(10));
}

/// A reset is admitted the instant an `end_run` has answered: the answer means the run has ended.
#[test]
fn a_reset_after_end_run_is_admitted() {
    let _serial = serial();
    let runtime = private_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let (_supervisor, path) = supervisor(runtime.path(), &dir.path().join("state"), &[]);
    let mut driver = dial(&path);
    driver.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut driver, "agent-was-here", Duration::from_secs(20));

    let mut manager = dial(&path);
    let first = status(&mut manager);
    let run = first["run"].as_str().expect("a live run has an id").to_string();
    let ended = manager.call("row.end_run", json!({"run": run, "reason": "test"}), Duration::from_secs(30)).expect("end_run answers");
    assert_eq!(ended.payload["outcome"], "ended", "{ended:?}");
    let reset = manager.call("row.reset", json!({}), Duration::from_secs(30)).expect("reset answers");
    assert_eq!(reset.payload["outcome"], "reset", "{reset:?}");
    let new_voyage = reset.payload["voyage"].as_str().expect("the reset names its new voyage").to_string();
    assert_ne!(Some(new_voyage.as_str()), first["voyage"].as_str(), "the reset minted a new voyage");

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let now = status(&mut manager);
        if now["phase"] == "ready" && now["run"] != first["run"] {
            assert_eq!(now["voyage"], new_voyage.as_str(), "a fresh agent runs under the new voyage");
            break;
        }
        assert!(Instant::now() < deadline, "no new run after the reset: {now:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}
