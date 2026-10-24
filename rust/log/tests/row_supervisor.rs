#![cfg(target_os = "linux")]
//! The row socket on a real supervisor (pass 6c-1a1): the supervisor binds `row-<h>`, an attach shows
//! what the agent has already written, the agent's output follows in order, and a driver's input reaches the agent.

use sot_log::row::{RowClient, RowEvent};
use sot_platform::host::state_dir::state_dir_hash;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Supervisor(Child);

impl Drop for Supervisor {
    fn drop(&mut self) {
        // The supervisor's end ends the agent: the kernel's parent-death signal follows the thread that forked it.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn read_until(client: &mut RowClient, needle: &str, within: Duration) -> String {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match client.next_event(Duration::from_millis(100)).expect("the row stays up") {
            Some(RowEvent::Output(b)) => seen.extend_from_slice(&b),
            Some(RowEvent::Snapshot { bytes, last }) => {
                if last {
                    let mut p = vt100_ctt::Parser::new(24, 80, 0);
                    p.restore_screen(&bytes).expect("the snapshot restores");
                    seen.extend_from_slice(p.screen().contents().as_bytes());
                }
            }
            _ => {}
        }
        if String::from_utf8_lossy(&seen).contains(needle) {
            return String::from_utf8_lossy(&seen).into_owned();
        }
    }
    panic!("no {needle:?} within {within:?}; saw {:?}", String::from_utf8_lossy(&seen));
}

#[test]
fn a_supervisor_serves_its_row_socket_and_the_agent_hears_a_driver() {
    let runtime = tempfile::Builder::new().prefix("sot-r").tempdir_in("/tmp").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sot-capsule"));
    cmd.arg("supervise")
        .arg(&state_dir)
        .arg("--start")
        .arg("--")
        .args(["/bin/sh", "-c", "echo agent-was-here; read line; echo \"agent-read:$line:done\"; exec cat"])
        .env("SOT_RUNTIME_DIR", runtime.path())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let _supervisor = Supervisor(cmd.spawn().expect("start a supervisor"));
    // The row socket's path under the runtime folder this supervisor was given.
    let path = {
        let h = state_dir_hash(&state_dir);
        sot_log::row::socket_path_in(runtime.path(), &h).expect("the path fits")
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut client = loop {
        match RowClient::connect(&path, "test", Duration::from_secs(5)) {
            Ok(c) => break c,
            Err(e) => assert!(Instant::now() < deadline, "the row socket never answered: {e}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(!client.run().is_empty(), "the hello names the run");
    client.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut client, "agent-was-here", Duration::from_secs(20));
    client.send_input(b"typed-by-the-driver\r").expect("the driver types");
    // The terminal echoes what was typed; only the agent's own line proves it read it.
    read_until(&mut client, "agent-read:typed-by-the-driver:done", Duration::from_secs(10));
}

/// The runtime folder of a running supervisor holds its row socket and no attach lane: nothing dials one since the row
/// socket became every viewer's way in, so none is bound (pass 6c-1a3).
#[test]
fn no_attach_lane_is_bound() {
    let runtime = tempfile::Builder::new().prefix("sot-r").tempdir_in("/tmp").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sot-capsule"));
    cmd.arg("supervise")
        .arg(&state_dir)
        .arg("--start")
        .arg("--")
        .args(["/bin/sh", "-c", "echo agent-was-here; exec cat"])
        .env("SOT_RUNTIME_DIR", runtime.path())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let _supervisor = Supervisor(cmd.spawn().expect("start a supervisor"));
    let h = state_dir_hash(&state_dir);
    let path = sot_log::row::socket_path_in(runtime.path(), &h).expect("the path fits");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut client = loop {
        match RowClient::connect(&path, "test", Duration::from_secs(5)) {
            Ok(c) => break c,
            Err(e) => assert!(Instant::now() < deadline, "the row socket never answered: {e}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    client.attach(80, 24, true).expect("attach as the driver");
    // The agent has written, so its run has started: whatever the run binds is bound.
    read_until(&mut client, "agent-was-here", Duration::from_secs(20));
    let mut names: Vec<String> = std::fs::read_dir(runtime.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(names.contains(&format!("row-{h}.sock")), "the row socket is bound: {names:?}");
    let attach_lanes: Vec<&String> = names.iter().filter(|n| n.starts_with("voyage-")).collect();
    assert!(attach_lanes.is_empty(), "no attach lane is bound, but found {attach_lanes:?}");
}

/// Starts a supervisor on a fresh state folder with `SOT_TEST_ROW_LISTENER_DIES` set to `dies`, and the row socket's path.
fn supervisor_with_dying_listener(dies: &str) -> (Supervisor, std::path::PathBuf, tempfile::TempDir, tempfile::TempDir) {
    let runtime = tempfile::Builder::new().prefix("sot-r").tempdir_in("/tmp").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sot-capsule"));
    cmd.arg("supervise")
        .arg(&state_dir)
        .arg("--start")
        .arg("--")
        .args(["/bin/sh", "-c", "echo agent-was-here; exec cat"])
        .env("SOT_RUNTIME_DIR", runtime.path())
        .env("SOT_TEST_ROW_LISTENER_DIES", dies)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let supervisor = Supervisor(cmd.spawn().expect("start a supervisor"));
    let path = sot_log::row::socket_path_in(runtime.path(), &state_dir_hash(&state_dir)).expect("the path fits");
    (supervisor, path, runtime, dir)
}

/// A row listener that dies is bound again once: a viewer that retries gets in and sees the agent.
#[test]
fn a_dead_row_listener_is_bound_again_once() {
    let (_supervisor, path, _runtime, _dir) = supervisor_with_dying_listener("first");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut client = loop {
        match RowClient::connect(&path, "test", Duration::from_secs(5)) {
            Ok(c) => break c,
            Err(e) => assert!(Instant::now() < deadline, "the row socket never came back: {e}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    client.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut client, "agent-was-here", Duration::from_secs(20));
}

/// A row listener that dies again, or cannot be bound, does not leave the supervisor running unreachable: it ends the
/// run unrequested and exits 75, so rows' watchdog decides through its one restart budget.
#[test]
fn a_row_listener_that_keeps_dying_ends_the_supervisor_with_75() {
    let (mut supervisor, _path, _runtime, _dir) = supervisor_with_dying_listener("always");
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = supervisor.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the supervisor kept running with no row socket");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(status.code(), Some(75), "a row that cannot be reached is a row that did not start");
}

/// The runtime folder of a running supervisor holds its row socket and nothing else: the daemon's management calls ride
/// the row socket, so no supervisor lane is bound (pass 6c-1b1).
#[test]
fn no_supervisor_lane_is_bound() {
    let (_supervisor, path, runtime, _dir) = supervisor_with_dying_listener("never");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut client = loop {
        match RowClient::connect(&path, "test", Duration::from_secs(5)) {
            Ok(c) => break c,
            Err(e) => assert!(Instant::now() < deadline, "the row socket never answered: {e}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    client.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut client, "agent-was-here", Duration::from_secs(20));
    let mut names: Vec<String> = std::fs::read_dir(runtime.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let h = path.file_name().unwrap().to_string_lossy().trim_start_matches("row-").trim_end_matches(".sock").to_string();
    assert!(names.contains(&format!("row-{h}.sock")), "the row socket is bound: {names:?}");
    let lanes: Vec<&String> = names.iter().filter(|n| n.starts_with("supervisor-") || n.starts_with("voyage-")).collect();
    assert!(lanes.is_empty(), "no lane is bound beside the row socket, but found {lanes:?}");
}

/// The thread whose name is `name` in the process `pid`, and its voluntary context switches so far.
fn voluntary_switches(pid: u32, name: &str) -> u64 {
    for entry in std::fs::read_dir(format!("/proc/{pid}/task")).unwrap() {
        let task = entry.unwrap().path();
        if std::fs::read_to_string(task.join("comm")).unwrap_or_default().trim() != name {
            continue;
        }
        let status = std::fs::read_to_string(task.join("status")).unwrap_or_default();
        let line = status.lines().find(|l| l.starts_with("voluntary_ctxt_switches:")).expect("the thread reports its switches");
        return line.split_whitespace().nth(1).unwrap().parse().unwrap();
    }
    panic!("the supervisor has no thread named {name:?}");
}

/// The thread that watches the row listener waits for an event: a thread blocked on a channel gains no voluntary
/// context switches, where a poll gains ten a second. A listener that dies is still bound again, and a dial after the
/// death is served at once, with no status that reads `terminal` in between.
#[test]
fn the_row_listeners_watch_waits_on_an_event() {
    let (supervisor, path, _runtime, _dir) = supervisor_with_dying_listener("never");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut client = loop {
        match RowClient::connect(&path, "test", Duration::from_secs(5)) {
            Ok(c) => break c,
            Err(e) => assert!(Instant::now() < deadline, "the row socket never answered: {e}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    client.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut client, "agent-was-here", Duration::from_secs(20));
    let pid = supervisor.0.id();
    let before = voluntary_switches(pid, "row-watch");
    std::thread::sleep(Duration::from_secs(1));
    let after = voluntary_switches(pid, "row-watch");
    assert_eq!(after, before, "the watch woke {} times in a second with nothing to watch", after - before);
    drop(client);

    let (_supervisor, path, _runtime, _dir) = supervisor_with_dying_listener("first");
    let started = Instant::now();
    let mut served = None;
    while started.elapsed() < Duration::from_secs(30) && served.is_none() {
        if let Ok(mut c) = RowClient::connect(&path, "test", Duration::from_secs(5)) {
            let status = c.call("row.status", serde_json::json!({}), Duration::from_secs(5)).expect("a served dial answers a status");
            assert_ne!(status.payload["phase"], "terminal", "the rebound listener's row is not terminal");
            served = Some(started.elapsed());
        } else {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let served = served.expect("the rebound listener served a dial");
    assert!(served < Duration::from_secs(5), "the dial after the death was served after {served:?}");
}

/// A listener lost while the supervisor rests (its run ended, nothing left to ask of it) cannot be bound again: the
/// supervisor ends with 75 and frees its fence, so rows' budget decides. Nothing else could stop it, since the row
/// socket is its only way in.
#[test]
fn a_listener_lost_while_the_supervisor_rests_ends_it_with_75() {
    let trigger_dir = tempfile::tempdir().unwrap();
    let trigger = trigger_dir.path().join("lost");
    let runtime = tempfile::Builder::new().prefix("sot-r").tempdir_in("/tmp").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sot-capsule"));
    cmd.arg("supervise")
        .arg(&state_dir)
        .arg("--start")
        .arg("--")
        .args(["/bin/sh", "-c", "echo agent-was-here; exec cat"])
        .env("SOT_RUNTIME_DIR", runtime.path())
        .env("SOT_TEST_ROW_LISTENER_DIES_ON", &trigger)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut supervisor = Supervisor(cmd.spawn().expect("start a supervisor"));
    let path = sot_log::row::socket_path_in(runtime.path(), &state_dir_hash(&state_dir)).expect("the path fits");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut client = loop {
        match RowClient::connect(&path, "test", Duration::from_secs(5)) {
            Ok(c) => break c,
            Err(e) => assert!(Instant::now() < deadline, "the row socket never answered: {e}"),
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    client.attach(80, 24, true).expect("attach as the driver");
    read_until(&mut client, "agent-was-here", Duration::from_secs(20));
    let run = client.call("row.status", serde_json::json!({}), Duration::from_secs(5)).expect("status").payload["run"].as_str().unwrap().to_string();
    let ended = client.call("row.end_run", serde_json::json!({"run": run, "reason": "test"}), Duration::from_secs(40)).expect("end_run");
    assert_eq!(ended.payload["outcome"], "ended", "{ended:?}");
    drop(client);

    std::fs::write(&trigger, b"").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = supervisor.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "a resting supervisor with no listener kept its fence");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(status.code(), Some(75), "a row nobody can reach is a row that did not start");
}
