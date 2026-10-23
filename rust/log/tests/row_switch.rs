#![cfg(target_os = "linux")]
//! The row socket as the only way in (pass 6c-1a2), on a real supervisor: a row socket that cannot be bound ends the
//! supervisor terminal before any agent starts, and a paste the agent has not yet read does not stop the loop that
//! drains the agent's output.

use sot_log::row::{RowClient, RowEvent};
use sot_platform::host::state_dir::state_dir_hash;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Supervisor(Child);

impl Drop for Supervisor {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn private_runtime_dir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let runtime = tempfile::Builder::new().prefix("sot-r").tempdir_in("/tmp").unwrap();
    std::fs::set_permissions(runtime.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    runtime
}

fn supervise(state_dir: &std::path::Path, runtime: &std::path::Path, agent: &[&str]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_sot-capsule"))
        .arg("supervise")
        .arg(state_dir)
        .arg("--start")
        .arg("--assume-no-rollback-target")
        .arg("--")
        .args(agent)
        .env("SOT_RUNTIME_DIR", runtime)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start a supervisor")
}

#[test]
fn a_row_socket_that_cannot_be_bound_ends_the_supervisor_terminal() {
    let runtime = private_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    // A folder where the socket belongs: bind fails and nothing may be served in its place.
    let path = sot_log::row::socket_path_in(runtime.path(), &state_dir_hash(&state_dir)).expect("the path fits");
    std::fs::create_dir(&path).unwrap();
    let marker = dir.path().join("agent-started");
    let script = format!("touch {}; exec cat", marker.display());
    let mut supervisor = supervise(&state_dir, runtime.path(), &["/bin/sh", "-c", &script]);
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = supervisor.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = supervisor.kill();
            let _ = supervisor.wait();
            panic!("the supervisor served on without its row socket");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(sot_log::supervisor::EXIT_TERMINAL), "an unbindable row socket is terminal");
    assert!(!marker.exists(), "no agent starts for a row nobody can reach");
}

#[test]
fn a_paste_the_agent_has_not_read_does_not_stop_its_output() {
    let runtime = private_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    // The agent writes more than the run's output queue holds (8 MiB) before it reads a byte, then reports how much it was given.
    let script = "stty raw -echo; head -c 12000000 /dev/zero | tr '\\0' x; n=$(head -c 200000 | wc -c); echo; echo \"AGENT-READ:$n:END\"; exec cat";
    let _supervisor = Supervisor(supervise(&state_dir, runtime.path(), &["/bin/sh", "-c", script]));
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
    // The paste is larger than the terminal's input buffer; its answer may be late, never wrong.
    let paste = vec![b'p'; 200_000];
    match client.send_input(&paste) {
        Ok(n) => assert_eq!(n, paste.len()),
        Err(sot_log::row::RowError::DeliveryUnknown | sot_log::row::RowError::Timeout(_)) => {}
        Err(e) => panic!("the paste was refused: {e}"),
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = String::new();
    while Instant::now() < deadline && !seen.contains("AGENT-READ:200000:END") {
        match client.next_event(Duration::from_millis(100)) {
            Ok(Some(RowEvent::Output(b))) => {
                seen.push_str(&String::from_utf8_lossy(&b));
                if seen.len() > 4096 {
                    seen.drain(..seen.len() - 2048);
                }
            }
            Ok(_) => {}
            Err(e) => panic!("the row dropped the viewer: {e}"),
        }
    }
    assert!(seen.contains("AGENT-READ:200000:END"), "the agent was given the whole paste and its output kept flowing");
}
