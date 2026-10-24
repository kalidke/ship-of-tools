#![cfg(target_os = "linux")]
//! The daemon's management calls on the row socket (pass 6c-1b1): status, end_run, reset and stop are ops of `row-<h>`,
//! answered by the supervisor beside the viewers' ops. The daemon's client dials only the row number, never the wire
//! integer; an `end_run` is fenced by the run it names.

use serde_json::{json, Value};
use sot_capsule::attach_client::supervisor_client::{self, CancelHandle, WatchStep};
use sot_capsule::SupervisorPhase;
use sot_capsule::row::{socket_path_in, RowClient, RowEvent, ROW_PROTOCOL};
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
    supervisor_running(runtime, state_dir, env, "echo agent-was-here; exec cat")
}

/// [`supervisor`] with the agent's own shell script.
fn supervisor_running(runtime: &Path, state_dir: &Path, env: &[(&str, &str)], script: &str) -> (Supervisor, PathBuf) {
    std::fs::create_dir_all(state_dir).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sot-capsule"));
    cmd.arg("supervise")
        .arg(state_dir)
        .arg("--start")
        .arg("--")
        .args(["/bin/sh", "-c", script])
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
                    let (pid, created) = sot_capsule::row::self_identity().expect("own identity");
                    let payload = match req.op.as_str() {
                        "row.hello" => json!({"pid": pid, "created": created, "phase": "ready", "run": "run-one", "row": row_number}),
                        "row.status" => json!({"phase": "ready", "run": "run-one"}),
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
    assert!(matches!(err, sot_capsule::Error::VersionSkew), "named: {err:?}");
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

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let now = status(&mut manager);
        if now["phase"] == "ready" && now["run"] != first["run"] {
            break;
        }
        assert!(Instant::now() < deadline, "no new run after the reset: {now:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A resting supervisor reports the run whose end it rests on, so the daemon's cell can latch `ended_no_respawn` for that
/// run, release it on a newer one, and tell a stale resting observation from a current one.
#[test]
fn a_resting_supervisor_reports_the_run_it_rests_on() {
    let _serial = serial();
    let runtime = private_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let (_supervisor, path) = supervisor(runtime.path(), &dir.path().join("state"), &[]);
    let mut driver = dial(&path);
    driver.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut driver, "agent-was-here", Duration::from_secs(20));

    let mut manager = dial(&path);
    let live = status(&mut manager);
    let run = live["run"].as_str().expect("a live run has an id").to_string();
    let ended = manager.call("row.end_run", json!({"run": run, "reason": "test"}), Duration::from_secs(30)).expect("end_run answers");
    assert_eq!(ended.payload["outcome"], "ended", "{ended:?}");

    let resting = status(&mut manager);
    assert_eq!(resting["phase"], "ended_no_respawn", "{resting:?}");
    assert_eq!(resting["run"], run.as_str(), "a resting supervisor names the run it rests on: {resting:?}");

    // A reset starts a new run, which the next status names, higher than the one it replaces.
    let reset = manager.call("row.reset", json!({}), Duration::from_secs(30)).expect("reset answers");
    assert_eq!(reset.payload["outcome"], "reset", "{reset:?}");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let now = status(&mut manager);
        if now["phase"] == "ready" && now["run"] != resting["run"] {
            assert!(now["run"].as_str().unwrap() > run.as_str(), "the new run orders after the old: {now:?}");
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
    /// The reset is dropped without being carried out: the supervisor died mid-answer before it did anything.
    reset_dropped: bool,
    /// The status the stand-in gives once ended, as JSON; none answers the unnamed rest of a fresh supervisor.
    after_end: Option<&'static str>,
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
                    let (pid, created) = sot_capsule::row::self_identity().expect("own identity");
                    let payload = match req.op.as_str() {
                        "row.hello" => json!({"pid": pid, "created": created, "phase": "ready", "run": "run-one", "row": ROW_PROTOCOL}),
                        "row.status" => {
                            let s = state.lock().unwrap();
                            if s.reset {
                                json!({"phase": "starting", "run": "run-two"})
                            } else if s.ended {
                                match s.after_end {
                                    Some(text) => serde_json::from_str(text).unwrap(),
                                    None => json!({"phase": "ended_no_respawn"}),
                                }
                            } else {
                                json!({"phase": "ready", "run": "run-one"})
                            }
                        }
                        "row.end_run" => {
                            state.lock().unwrap().ended = true;
                            return;
                        }
                        "row.reset" => {
                            let mut s = state.lock().unwrap();
                            s.reset = !s.reset_dropped;
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

/// What a lost `end_run` reply is settled to when the supervisor's status afterward is `after_end`.
fn lost_end_settled_by(after_end: &'static str) -> supervisor_client::EndRunOutcome {
    let _serial = serial();
    let runtime = private_runtime_dir();
    let (dir, state_dir, stop, row) = lossy_setup(runtime.path(), Lossy { after_end: Some(after_end), ..Lossy::default() });
    let outcome = supervisor_client::end_run(&state_dir, "run-one", "test").expect("the call is answered");
    stop.store(true, std::sync::atomic::Ordering::Release);
    row.join().unwrap();
    drop(dir);
    outcome
}

/// A resting supervisor names the run it rests on, and that run is over: a rest naming the asked run, and one naming
/// another, both read as ended; a terminal supervisor fails the end whatever run it names.
#[test]
fn a_lost_end_reply_is_settled_by_the_run_a_rest_or_a_terminal_status_names() {
    use supervisor_client::EndRunOutcome as O;
    assert!(matches!(lost_end_settled_by(r#"{"phase":"ended_no_respawn","run":"run-one"}"#), O::Ended), "a rest on the asked run");
    assert!(matches!(lost_end_settled_by(r#"{"phase":"ended_no_respawn","run":"run-other"}"#), O::Ended), "a rest on another run");
    assert!(matches!(lost_end_settled_by(r#"{"phase":"terminal","run":"run-one"}"#), O::Failed(_)), "terminal on the asked run");
    assert!(matches!(lost_end_settled_by(r#"{"phase":"terminal","run":"run-other"}"#), O::Failed(_)), "terminal on another run");
}

/// What a lost `reset` reply is settled to: `state` is what the stand-in has done, `after_end` the status it gives while
/// it rests (none: unnamed). A reset it carries out is followed by `starting` on `run-two`.
fn lost_reset_settled_by(state: Lossy) -> sot_capsule::Result<()> {
    let _serial = serial();
    let runtime = private_runtime_dir();
    let (dir, state_dir, stop, row) = lossy_setup(runtime.path(), state);
    let outcome = supervisor_client::reset(&state_dir);
    stop.store(true, std::sync::atomic::Ordering::Release);
    row.join().unwrap();
    drop(dir);
    outcome
}

const REST_ON_ONE: &str = r#"{"phase":"ended_no_respawn","run":"run-one"}"#;
const TERMINAL_ON_ONE: &str = r#"{"phase":"terminal","run":"run-one"}"#;

/// A reset whose reply is lost is settled by the run status then names, against the run it rested on: a run other than
/// that one (here none, or `run-one`) is the reset's, done; a supervisor still resting or terminal on the same run was
/// not reset.
#[test]
fn a_reset_whose_reply_is_lost_is_settled_by_the_run_status_names() {
    let lossy = |after_end, reset_dropped| Lossy { ended: true, reset_dropped, after_end, ..Lossy::default() };
    lost_reset_settled_by(lossy(None, false)).expect("an unnamed rest, then a named run: done");
    lost_reset_settled_by(lossy(None, true)).expect_err("an unnamed rest still resting: not done");
    lost_reset_settled_by(lossy(Some(REST_ON_ONE), false)).expect("a rest on run-one, then run-two: done");
    lost_reset_settled_by(lossy(Some(REST_ON_ONE), true)).expect_err("still resting on run-one: not done");
    lost_reset_settled_by(lossy(Some(TERMINAL_ON_ONE), true)).expect_err("terminal on run-one: not done, a failed publication");
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
    impl sot_capsule::row::Management for Stopper {
        fn status(&self) -> sot_capsule::row::StatusRes {
            sot_capsule::row::StatusRes { phase: "ready".into(), run: None }
        }
        fn end_run(&self, _: &str, _: &str) -> sot_capsule::row::EndRunRes {
            unreachable!()
        }
        fn reset(&self) -> sot_capsule::row::ResetRes {
            unreachable!()
        }
        fn stop(&self, _: sot_capsule::row::ConnId) -> sot_capsule::row::StopRes {
            sot_capsule::row::StopRes { outcome: sot_capsule::row::StopOutcome::Stopping }
        }
        fn conn_gone(&self, _: sot_capsule::row::ConnId) {}
    }
    let run = sot_capsule::row::RowRun::new("r", "ready");
    run.set_management(Box::new(Stopper));
    let server = sot_capsule::row::RowServer::bind(&path, run).expect("bind");
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

/// The supervisor's own exit code, once it has exited.
fn exit_code(supervisor: &mut Supervisor) -> Option<i32> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = supervisor.0.try_wait().unwrap() {
            return status.code();
        }
        assert!(Instant::now() < deadline, "the supervisor never exited");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Reads the watch until `done` says a step is the one wanted; every phase report on the way is returned, in order.
fn watch_until(watch: &mut supervisor_client::Watch, within: Duration, mut done: impl FnMut(&WatchStep) -> bool) -> Vec<WatchStep> {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        let step = watch.next(Duration::from_millis(200));
        let stop = done(&step);
        if !matches!(step, WatchStep::Quiet) {
            seen.push(step);
        }
        if stop {
            return seen;
        }
    }
    panic!("the watch never reached the step wanted within {within:?}; saw {seen:?}");
}

/// A watch on a row that has an agent writing, and the report that says it is ready: the first reply is the status last
/// told, which can still be the start's when the watch is dialed, and `ready` then follows as an event.
fn ready_watch(state_dir: &Path) -> (supervisor_client::Watch, supervisor_client::StatusReport) {
    let mut watch = supervisor_client::watch(state_dir, &CancelHandle::new()).expect("the watch is served");
    let first = watch.first().clone();
    if first.phase == SupervisorPhase::Ready && first.run.is_some() {
        return (watch, first);
    }
    let seen = watch_until(&mut watch, Duration::from_secs(30), |s| matches!(s, WatchStep::Phase(r) if r.phase == SupervisorPhase::Ready));
    let Some(WatchStep::Phase(ready)) = seen.last().cloned() else { unreachable!() };
    (watch, ready)
}

/// A watch's first reply names the run; each change of phase or run after it is one event, in the order the changes
/// happened: an `end_run` brings `ending` then `ended_no_respawn` naming the same run, and a reset brings `starting` on a
/// newer run.
#[test]
fn a_watch_reports_each_phase_change_in_order() {
    let _serial = serial();
    let runtime = private_runtime_dir();
    std::env::set_var("SOT_RUNTIME_DIR", runtime.path());
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let (_supervisor, path) = supervisor(runtime.path(), &state_dir, &[]);
    let mut driver = dial(&path);
    driver.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut driver, "agent-was-here", Duration::from_secs(20));

    let (mut watch, first) = ready_watch(&state_dir);
    let run = first.run.clone().expect("the report names the run");
    assert_eq!(watch.peer(), (first.pid, first.created), "the watch's peer is the supervisor's hello identity");

    let mut manager = dial(&path);
    let ended = manager.call("row.end_run", json!({"run": run, "reason": "test"}), Duration::from_secs(30)).expect("end_run answers");
    assert_eq!(ended.payload["outcome"], "ended", "{ended:?}");
    let seen = watch_until(&mut watch, Duration::from_secs(20), |s| matches!(s, WatchStep::Phase(r) if r.phase == SupervisorPhase::EndedNoRespawn));
    let phases: Vec<_> = seen.iter().map(|s| match s { WatchStep::Phase(r) => (r.phase, r.run.clone()), other => panic!("{other:?}") }).collect();
    assert_eq!(
        phases,
        vec![(SupervisorPhase::Ending, Some(run.clone())), (SupervisorPhase::EndedNoRespawn, Some(run.clone()))],
        "ending, then the rest, both naming the run"
    );

    let reset = manager.call("row.reset", json!({}), Duration::from_secs(30)).expect("reset answers");
    assert_eq!(reset.payload["outcome"], "reset", "{reset:?}");
    let seen = watch_until(&mut watch, Duration::from_secs(30), |s| matches!(s, WatchStep::Phase(r) if r.phase == SupervisorPhase::Ready));
    let WatchStep::Phase(last) = seen.last().unwrap() else { unreachable!() };
    let newer = last.run.clone().expect("the new run is named");
    assert!(newer > run, "the reset's run orders after the old: {newer} after {run}");
    assert!(
        seen.iter().any(|s| matches!(s, WatchStep::Phase(r) if r.phase == SupervisorPhase::Starting && r.run.as_ref() == Some(&newer))),
        "the reset's run is announced starting before it is ready: {seen:?}"
    );
}

/// A controlled end is told: the watch's last frame is `row.exiting` with the exit code, then the stream ends. A stop
/// ends 0; an agent that ends by itself ends 76.
#[test]
fn a_watch_reports_exiting_with_the_code_before_a_controlled_end() {
    let _serial = serial();
    let runtime = private_runtime_dir();
    std::env::set_var("SOT_RUNTIME_DIR", runtime.path());
    let dir = tempfile::tempdir().unwrap();

    // A stop, on another connection: 0.
    let state_dir = dir.path().join("stopped");
    let (mut supervisor, path) = supervisor(runtime.path(), &state_dir, &[]);
    let mut driver = dial(&path);
    driver.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut driver, "agent-was-here", Duration::from_secs(20));
    let mut watch = supervisor_client::watch(&state_dir, &CancelHandle::new()).expect("the watch is served");
    let mut manager = dial(&path);
    let stopped = manager.call("row.stop", json!({}), Duration::from_secs(30)).expect("stop answers");
    assert_eq!(stopped.payload["outcome"], "stopping", "{stopped:?}");
    let seen = watch_until(&mut watch, Duration::from_secs(30), |s| matches!(s, WatchStep::Exiting(_) | WatchStep::Ended));
    assert!(matches!(seen.last(), Some(WatchStep::Exiting(0))), "a stop is told as exit 0: {seen:?}");
    assert!(matches!(watch.next(Duration::from_secs(10)), WatchStep::Ended), "the stream ends after the exit is told");
    assert_eq!(exit_code(&mut supervisor), Some(0), "the code the watch was told is the one the process left with");

    // The agent ends by itself: 76.
    let state_dir = dir.path().join("agent-ends");
    let (mut supervisor, path) = supervisor_running(runtime.path(), &state_dir, &[], "echo agent-was-here; read line; exit 0");
    let mut driver = dial(&path);
    driver.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut driver, "agent-was-here", Duration::from_secs(20));
    let mut watch = supervisor_client::watch(&state_dir, &CancelHandle::new()).expect("the watch is served");
    driver.send_input(b"bye\r").expect("the driver types");
    let seen = watch_until(&mut watch, Duration::from_secs(30), |s| matches!(s, WatchStep::Exiting(_) | WatchStep::Ended));
    assert!(matches!(seen.last(), Some(WatchStep::Exiting(76))), "an agent that ends by itself is told as exit 76: {seen:?}");
    assert!(matches!(watch.next(Duration::from_secs(10)), WatchStep::Ended), "the stream ends after the exit is told");
    assert_eq!(exit_code(&mut supervisor), Some(76), "the code the watch was told is the one the process left with");
}

/// A watch's end waits for the supervisor's process, so a reader that acts on a told exit does not decide while the
/// exiting supervisor still answers or holds its fence: `end` closes the connection first, then waits for the process,
/// and reports whether it ended within the bound. The seam holds the exit two seconds with the row socket bound.
#[test]
fn a_watch_ends_by_waiting_for_the_supervisors_process() {
    let _serial = serial();
    let runtime = private_runtime_dir();
    std::env::set_var("SOT_RUNTIME_DIR", runtime.path());
    let dir = tempfile::tempdir().unwrap();

    for (name, bound, ended) in [("short", Duration::from_millis(300), false), ("long", Duration::from_secs(30), true)] {
        let state_dir = dir.path().join(name);
        let (mut supervisor, path) = supervisor(runtime.path(), &state_dir, &[("SOT_TEST_EXIT_HOLD_MS", "2000")]);
        let mut driver = dial(&path);
        driver.attach(80, 24, true).expect("attach as the driver");
        read_until(&mut driver, "agent-was-here", Duration::from_secs(20));
        let mut watch = supervisor_client::watch(&state_dir, &CancelHandle::new()).expect("the watch is served");
        let mut manager = dial(&path);
        manager.call("row.stop", json!({}), Duration::from_secs(30)).expect("stop answers");
        let seen = watch_until(&mut watch, Duration::from_secs(30), |s| matches!(s, WatchStep::Exiting(_) | WatchStep::Ended));
        assert!(matches!(seen.last(), Some(WatchStep::Exiting(0))), "a stop is told as exit 0: {seen:?}");
        drop((manager, driver));
        let began = Instant::now();
        assert_eq!(watch.end(bound), ended, "{name}: the process had {bound:?} to end while it held its exit for 2 s");
        if ended {
            assert!(began.elapsed() >= Duration::from_secs(1), "the end returned at once, without waiting for the process");
        }
        assert_eq!(exit_code(&mut supervisor), Some(0));
    }
}

/// `Watch::end` gives its place in the cancel handle back before it waits for the process, so the handle does not keep
/// the connection (a Windows pipe stays open until its last owner lets go) through the wait.
#[test]
fn a_watch_end_withdraws_its_publication_before_waiting() {
    let _serial = serial();
    let runtime = private_runtime_dir();
    std::env::set_var("SOT_RUNTIME_DIR", runtime.path());
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let (mut supervisor, path) = supervisor(runtime.path(), &state_dir, &[("SOT_TEST_EXIT_HOLD_MS", "3000")]);
    let mut driver = dial(&path);
    driver.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut driver, "agent-was-here", Duration::from_secs(20));
    let cancel = CancelHandle::new();
    let mut watch = supervisor_client::watch(&state_dir, &cancel).expect("the watch is served");
    assert!(cancel.is_published(), "a live watch is published");
    let mut manager = dial(&path);
    manager.call("row.stop", json!({}), Duration::from_secs(30)).expect("stop answers");
    let seen = watch_until(&mut watch, Duration::from_secs(30), |s| matches!(s, WatchStep::Exiting(_) | WatchStep::Ended));
    assert!(matches!(seen.last(), Some(WatchStep::Exiting(0))), "{seen:?}");
    let ending = std::thread::spawn(move || watch.end(Duration::from_secs(30)));
    std::thread::sleep(Duration::from_millis(700));
    assert!(!ending.is_finished(), "the supervisor holds its exit for 3 s: the wait is still going");
    assert!(!cancel.is_published(), "the handle still owns the connection during the process wait");
    assert!(ending.join().unwrap());
    assert_eq!(exit_code(&mut supervisor), Some(0));
}

/// A cancel that lands between the dial and the publication of the connection is never missed: the watch is refused, and
/// so is every later one on that handle (cancellation is latched), while the row stays served to a fresh handle.
#[test]
fn a_cancelled_watch_never_publishes_a_connection() {
    let _serial = serial();
    let runtime = private_runtime_dir();
    std::env::set_var("SOT_RUNTIME_DIR", runtime.path());
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let (_supervisor, path) = supervisor(runtime.path(), &state_dir, &[]);
    let mut driver = dial(&path);
    driver.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut driver, "agent-was-here", Duration::from_secs(20));

    let cancel = CancelHandle::new();
    let racing = cancel.clone();
    let result = supervisor_client::watch_with_test_hook(&state_dir, &cancel, move || racing.cancel());
    assert!(result.is_err(), "a connect published after a cancel landed inside the race window must still refuse");
    for _ in 0..3 {
        assert!(supervisor_client::watch(&state_dir, &cancel).is_err(), "a cancelled handle refuses every later watch");
    }
    let _fresh = supervisor_client::watch(&state_dir, &CancelHandle::new()).expect("the row still serves a watch");
}

/// Every change is told, in order, whatever the thread that announces it is doing: the announcing thread is held after the
/// mutation of an `end_run` (the seam), so the run's end completes before the first announcement, and the watch must
/// still hear `ending` before `ended_no_respawn`.
#[test]
fn a_transition_is_told_though_the_run_ends_before_its_announcement() {
    let _serial = serial();
    let runtime = private_runtime_dir();
    std::env::set_var("SOT_RUNTIME_DIR", runtime.path());
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let (_supervisor, path) = supervisor(runtime.path(), &state_dir, &[("SOT_TEST_ANNOUNCE_HOLD_MS", "2000")]);
    let mut driver = dial(&path);
    driver.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut driver, "agent-was-here", Duration::from_secs(30));

    let mut watch = supervisor_client::watch(&state_dir, &CancelHandle::new()).expect("the watch is served");
    let mut manager = dial(&path);
    let run = status(&mut manager)["run"].as_str().expect("a live run has an id").to_string();
    // Let the watch catch up with the run's start (each announcement is held), so the end's reports stand alone.
    if watch.first().phase != SupervisorPhase::Ready {
        watch_until(&mut watch, Duration::from_secs(60), |s| matches!(s, WatchStep::Phase(r) if r.phase == SupervisorPhase::Ready));
    }
    let ended = manager.call("row.end_run", json!({"run": run, "reason": "test"}), Duration::from_secs(30)).expect("end_run answers");
    assert_eq!(ended.payload["outcome"], "ended", "{ended:?}");
    let seen = watch_until(&mut watch, Duration::from_secs(60), |s| matches!(s, WatchStep::Phase(r) if r.phase == SupervisorPhase::EndedNoRespawn));
    let phases: Vec<_> = seen.iter().filter_map(|s| match s { WatchStep::Phase(r) => Some(r.phase), _ => None }).collect();
    assert_eq!(phases, vec![SupervisorPhase::Ending, SupervisorPhase::EndedNoRespawn], "{seen:?}");
}

/// Input meant for one run is refused, `run ended`, by a row whose current run is another: a reset replaces the run while
/// the typing connection survives, and what was meant for the run before is never written into the one after.
#[test]
fn a_row_refuses_input_meant_for_another_run() {
    let _serial = serial();
    let runtime = private_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let (_supervisor, path) = supervisor(runtime.path(), &dir.path().join("state"), &[]);
    let mut driver = dial(&path);
    let first = driver.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut driver, "agent-was-here", Duration::from_secs(20));
    assert_eq!(driver.send_input_in(b"for-the-first\r", Some(&first)).expect("the run it was meant for takes it"), 14);
    read_until(&mut driver, "for-the-first", Duration::from_secs(10));

    let mut manager = dial(&path);
    let ended = manager.call("row.end_run", json!({"run": first, "reason": "test"}), Duration::from_secs(30)).expect("end_run answers");
    assert_eq!(ended.payload["outcome"], "ended", "{ended:?}");
    let reset = manager.call("row.reset", json!({}), Duration::from_secs(30)).expect("reset answers");
    assert_eq!(reset.payload["outcome"], "reset", "{reset:?}");
    let deadline = Instant::now() + Duration::from_secs(30);
    let second = loop {
        let now = status(&mut manager);
        if now["phase"] == "ready" && now["run"].as_str().is_some_and(|r| r != first) {
            break now["run"].as_str().unwrap().to_string();
        }
        assert!(Instant::now() < deadline, "no new run after the reset: {now:?}");
        std::thread::sleep(Duration::from_millis(100));
    };
    let refused = driver.send_input_in(b"for-the-old-run\r", Some(&first)).expect_err("another run's input is refused");
    assert!(refused.to_string().contains("run ended"), "{refused}");
    // Input sent for no run keeps today's rule: it goes to the current one.
    driver.send_input_in(b"for-whoever\r", None).expect("input for no run in particular is written");
    driver.send_input_in(b"for-the-new-run\r", Some(&second)).expect("the current run takes input meant for it");
    read_until(&mut driver, "for-the-new-run", Duration::from_secs(10));
}

/// A watch that is dropped gives its slot back: the row holds eight connections, and nine watches in turn all get one.
#[test]
fn a_dropped_watch_gives_its_slot_back() {
    let _serial = serial();
    let runtime = private_runtime_dir();
    std::env::set_var("SOT_RUNTIME_DIR", runtime.path());
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let (_supervisor, path) = supervisor(runtime.path(), &state_dir, &[]);
    let mut driver = dial(&path);
    driver.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut driver, "agent-was-here", Duration::from_secs(20));
    let cancel = CancelHandle::new();
    for turn in 0..9 {
        let watch = supervisor_client::watch(&state_dir, &cancel).unwrap_or_else(|e| panic!("watch {turn} found no slot: {e}"));
        drop(watch);
    }
    let _viewer = dial(&path);
}
