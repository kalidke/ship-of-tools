//! Guards for two of the capsule's guarantees that had no test at the capsule: one authority per state folder, and a row
//! that outlives its viewers. Both pass at the commit that adds them; they are here so a later change cannot break
//! either unseen.

use super::*;
#[cfg(target_os = "linux")]
use sot_capsule::row::MAX_CONNECTIONS;

/// A second supervisor started on a state folder whose fence a live supervisor holds exits 70, the contended code, and
/// disturbs nothing of the first: the first's row socket is the same file afterwards (the same inode, so it was not
/// removed and bound again) and answers a connection opened before the contender started and one opened after. The
/// fence is taken before the socket is bound, so a contender that bound and cleaned up would fail this. The inode is a
/// Unix witness; on Windows the guard proves only that the first supervisor keeps answering on both connections.
#[test]
fn a_second_supervisor_on_a_held_fence_exits_70_and_leaves_the_first_serving_its_socket() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let mut first = spawn_supervisor(&state_dir, "--start", SHELL);
    let before = wait_for_lane(&h, Duration::from_secs(30));
    let run = wait_for_ready(&before, Duration::from_secs(90));
    #[cfg(unix)]
    let inode = || {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(row_socket_path(&h).expect("the row socket's path fits")).expect("the row socket exists").ino()
    };
    #[cfg(unix)]
    let socket = inode();

    let mut second = spawn_supervisor(&state_dir, "--start", SHELL);
    let status_of_second = wait_for_exit(&mut second, Duration::from_secs(30));
    assert_eq!(status_of_second.code(), Some(70), "a supervisor on a held fence ends contended: {status_of_second:?}");

    #[cfg(unix)]
    assert_eq!(inode(), socket, "the first supervisor's row socket was replaced");
    assert_eq!(status(&before).run.as_deref(), Some(run.as_str()), "the first supervisor still answers the earlier connection");
    let after = wait_for_lane(&h, Duration::from_secs(30));
    assert_eq!(status(&after).run.as_deref(), Some(run.as_str()), "the first supervisor answers a new connection");

    end_run_and_expect_ended(&after, &run, "cleanup");
    stop(&after);
    let _ = wait_for_exit(&mut first, Duration::from_secs(30));
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

/// Whether every one of the row's connection slots is free: `MAX_CONNECTIONS` connections are all admitted at once.
/// A slot is released only when both of its connection's threads have ended, so this is true only when no earlier
/// connection's thread is left.
#[cfg(target_os = "linux")]
fn every_slot_is_free(h: &str) -> bool {
    let path = row_socket_path(h).expect("the row socket's path fits");
    let held: Vec<_> = (0..MAX_CONNECTIONS).map(|_| RowClient::connect(&path, "test", Duration::from_secs(5))).collect();
    held.iter().all(Result::is_ok)
}

/// Whether `pid` is a live process, not a zombie.
#[cfg(target_os = "linux")]
fn alive(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return false };
    stat.rfind(')').and_then(|close| stat[close + 1..].split_whitespace().next()) != Some("Z")
}

/// One connection watches, attaches as the driver, ends the run and resets the row: the new run's agent was started
/// from that connection's thread. Then every connection is dropped, and the row admits its full eight connections again,
/// which it does only once the dropped connection's threads have ended. The supervisor and the new
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
    poll_until(|| every_slot_is_free(&h).then_some(()), Duration::from_secs(15), "every connection slot to be free again");

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
