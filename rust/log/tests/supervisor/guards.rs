//! Guards for two of the capsule's guarantees that had no test at the capsule: one authority per state folder, and a row
//! that outlives its viewers. Both pass at the commit that adds them; they are here so a later change cannot break
//! either unseen.

use super::*;

/// A second supervisor started on a state folder whose fence is held exits 70, the contended code, and has bound no row
/// socket: the fence is taken before the socket is bound.
#[test]
fn a_second_supervisor_on_a_held_fence_exits_70_and_binds_nothing() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let held = journal::fence::lock_supervisor(&state_dir).expect("the test takes the fence first");
    let mut second = spawn_supervisor(&state_dir, "--start", SHELL);
    let status = wait_for_exit(&mut second, Duration::from_secs(30));
    assert_eq!(status.code(), Some(70), "a supervisor on a held fence ends contended: {status:?}");
    #[cfg(unix)]
    {
        let socket = row_socket_path(&h).expect("the row socket's path fits");
        assert!(!socket.exists(), "the contended supervisor bound {}", socket.display());
    }
    drop(held);
}

/// The agent's pid, read from the file its shell wrote before it became the agent.
#[cfg(target_os = "linux")]
fn agent_pid(pid_file: &Path) -> u32 {
    poll_until(
        || std::fs::read_to_string(pid_file).ok().and_then(|s| s.trim().parse().ok()),
        Duration::from_secs(30),
        "the agent to record its pid",
    )
}

/// Whether `pid` is a live process, not a zombie.
#[cfg(target_os = "linux")]
fn alive(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return false };
    stat.rfind(')').and_then(|close| stat[close + 1..].split_whitespace().next()) != Some("Z")
}

/// One connection watches, attaches as the driver, ends the run and resets the row: the new run's agent was started
/// from that connection's thread. Then every connection is dropped and their threads end. The supervisor and the new
/// run's agent are unaffected: a fresh connection reads `ready` with the reset's run and the agent is alive. (The
/// agent's parent-death signal is per thread on Linux, so this also proves the agent is not tied to a connection's.)
#[cfg(target_os = "linux")]
#[test]
fn a_supervisor_and_its_agent_outlive_every_connection() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);
    let pid_file = dir.path().join("agent.pid");
    let script = format!("echo $$ > '{}'; exec sleep 300", pid_file.display());
    let mut guard = spawn_supervisor(&state_dir, "--start", &["/bin/sh", "-c", script.as_str()]);

    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let first_run = wait_for_ready(&conn, Duration::from_secs(90));
    let first_agent = agent_pid(&pid_file);
    {
        let mut client = conn.0.borrow_mut();
        client.watch().expect("hold the connection as a watch");
        client.attach(80, 24, true).expect("attach as the driver");
    }
    end_run_and_expect_ended(&conn, &first_run, "guard");
    reset(&conn);
    drop(conn);
    std::thread::sleep(Duration::from_secs(1));

    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let run = wait_for_ready(&conn, Duration::from_secs(90));
    assert_ne!(run, first_run, "the reset began a new run");
    let agent = agent_pid(&pid_file);
    assert_ne!(agent, first_agent, "the reset started a new agent");
    assert!(alive(agent), "the reset's agent outlived every connection that asked for it");
    assert!(guard.child_mut().try_wait().unwrap().is_none(), "the supervisor outlived every connection");

    end_run_and_expect_ended(&conn, &run, "cleanup");
    stop(&conn);
    let _ = wait_for_exit(&mut guard, Duration::from_secs(30));
}
