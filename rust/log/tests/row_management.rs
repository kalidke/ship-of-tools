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

/// A stop is answered before the supervisor exits: the reply is read off the row socket, not inferred from the exit,
/// and the daemon's `stop` returns once it has seen both.
#[test]
fn a_stop_is_answered_before_the_supervisor_exits() {
    let _serial = serial();
    let runtime = private_runtime_dir();
    std::env::set_var("SOT_RUNTIME_DIR", runtime.path());
    let dir = tempfile::tempdir().unwrap();
    let (mut raw, path) = supervisor(runtime.path(), &dir.path().join("state"), &[]);
    let mut manager = dial(&path);
    let reply = manager.call("row.stop", json!({}), Duration::from_secs(30)).expect("the stop's reply is read before the exit");
    assert_eq!(reply.payload["outcome"], "stopping", "{reply:?}");
    drop(manager);
    let deadline = Instant::now() + Duration::from_secs(30);
    let code = loop {
        if let Some(status) = raw.0.try_wait().unwrap() {
            break status.code();
        }
        assert!(Instant::now() < deadline, "the supervisor never exited after its stop");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(code, Some(0), "a stop ends the supervisor clean");

    // The daemon's own call, on a second supervisor: it returns Ok once the reply and the exit are both seen.
    let state_dir = dir.path().join("state2");
    let (mut second, path) = supervisor(runtime.path(), &state_dir, &[]);
    drop(dial(&path));
    supervisor_client::stop(&state_dir).expect("the daemon's stop sees the reply and the exit");
    assert!(second.0.try_wait().unwrap().is_some(), "the supervisor had exited when stop returned");
}

/// What a row that loses replies has done so far.
#[derive(Default)]
struct Lossy {
    ended: bool,
    reset: bool,
}

/// A stand-in row that carries out `row.end_run` and `row.reset` and then drops the connection with no reply, as a
/// supervisor that died mid-answer or a connection that broke would, and says what happened when status is read.
fn lossy_row(path: &Path, state: std::sync::Arc<std::sync::Mutex<Lossy>>, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) -> std::thread::JoinHandle<()> {
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
            let state = state.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut out = stream;
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    let req: sot_protocol::Frame = serde_json::from_str(line.trim_end()).expect("a request frame");
                    line.clear();
                    let (pid, created) = sot_log::row::self_identity().expect("own identity");
                    let payload = match req.op.as_str() {
                        "row.hello" => json!({"pid": pid, "created": created, "phase": "ready", "run": "run-one", "row": ROW_PROTOCOL}),
                        "row.status" => {
                            let s = state.lock().unwrap();
                            if s.reset {
                                json!({"phase": "starting", "run": "run-two", "voyage": "voyage-two"})
                            } else if s.ended {
                                json!({"phase": "ended_no_respawn", "voyage": "voyage-one"})
                            } else {
                                json!({"phase": "ready", "run": "run-one", "voyage": "voyage-one"})
                            }
                        }
                        "row.end_run" => {
                            state.lock().unwrap().ended = true;
                            return;
                        }
                        "row.reset" => {
                            state.lock().unwrap().reset = true;
                            return;
                        }
                        _ => json!({"error": "no such op", "code": "unknown_op"}),
                    };
                    let mut text = serde_json::to_vec(&sot_protocol::Frame::res(req.id, &req.op, payload)).unwrap();
                    text.push(b'\n');
                    if out.write_all(&text).is_err() {
                        return;
                    }
                }
            });
        }
    })
}

fn lossy_setup(runtime: &Path, state: Lossy) -> (tempfile::TempDir, PathBuf, std::sync::Arc<std::sync::atomic::AtomicBool>, std::thread::JoinHandle<()>) {
    std::thread::sleep(Duration::from_millis(50));
    std::env::set_var("SOT_RUNTIME_DIR", runtime);
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let path = socket_path_in(runtime, &state_dir_hash(&state_dir)).expect("the path fits");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let row = lossy_row(&path, std::sync::Arc::new(std::sync::Mutex::new(state)), stop.clone());
    (dir, state_dir, stop, row)
}

/// An end whose reply is lost after the run ended is settled by reading status from the same supervisor: the run is
/// gone, so it ended.
#[test]
fn an_end_run_whose_reply_is_lost_after_the_run_ended_reports_ended() {
    let _serial = serial();
    let runtime = private_runtime_dir();
    let (dir, state_dir, stop, row) = lossy_setup(runtime.path(), Lossy::default());
    let outcome = supervisor_client::end_run(&state_dir, "run-one", "test").expect("the call is answered");
    assert!(matches!(outcome, supervisor_client::EndRunOutcome::Ended), "a lost reply over an ended run: {outcome:?}");
    stop.store(true, std::sync::atomic::Ordering::Release);
    row.join().unwrap();
    drop(dir);
}

/// A reset whose reply is lost after the new voyage was published reports the reset and its voyage.
#[test]
fn a_reset_whose_reply_is_lost_after_publication_reports_reset() {
    let _serial = serial();
    let runtime = private_runtime_dir();
    let (dir, state_dir, stop, row) = lossy_setup(runtime.path(), Lossy { ended: true, reset: false });
    let voyage = supervisor_client::reset(&state_dir).expect("the reset is found published");
    assert_eq!(voyage, "voyage-two");
    stop.store(true, std::sync::atomic::Ordering::Release);
    row.join().unwrap();
    drop(dir);
}

/// A peer that sends `row.stop` and never reads its reply cannot hold the supervisor's exit: shutdown ends its
/// connection whatever the peer does.
#[test]
fn a_peer_that_never_reads_its_stop_reply_cannot_hold_the_shutdown() {
    use std::io::Write;
    let _serial = serial();
    let runtime = private_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path_in(runtime.path(), "0123456789abcdef").expect("the path fits");
    struct Stopper;
    impl sot_log::row::Management for Stopper {
        fn status(&self) -> sot_log::row::StatusRes {
            sot_log::row::StatusRes { phase: "ready".into(), run: None, voyage: None }
        }
        fn end_run(&self, _: &str, _: &str) -> sot_log::row::EndRunRes {
            unreachable!()
        }
        fn reset(&self) -> sot_log::row::ResetRes {
            unreachable!()
        }
        fn stop(&self, _: sot_log::row::ConnId) -> sot_log::row::StopRes {
            sot_log::row::StopRes { outcome: sot_log::row::StopOutcome::Stopping }
        }
        fn conn_gone(&self, _: sot_log::row::ConnId) {}
    }
    let run = sot_log::row::RowRun::new("r", "ready");
    run.set_management(Box::new(Stopper));
    let server = sot_log::row::RowServer::bind(&path, run).expect("bind");
    let mut peer = std::os::unix::net::UnixStream::connect(&path).expect("connect");
    for (id, op, payload) in [(1, "row.hello", json!({"build": "t", "row": ROW_PROTOCOL})), (2, "row.stop", json!({}))] {
        let mut text = serde_json::to_vec(&sot_protocol::Frame::req(id, op, payload)).unwrap();
        text.push(b'\n');
        peer.write_all(&text).unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        server.shutdown();
        let _ = done.send(());
    });
    assert!(ended.recv_timeout(Duration::from_secs(10)).is_ok(), "the shutdown never ended with a peer that does not read");
    drop((peer, dir));
}
