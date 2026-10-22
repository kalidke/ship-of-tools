//! One process per row (decision D2, pass 6b): the supervisor runs the agent itself, the agent ends with its
//! supervisor, and a reset is admitted the moment an `end_run` has answered.

use super::*;

/// The agent: records its own pid in `pid_file`, then stays alive until the test or its parent ends it. On Unix the
/// shell ignores SIGHUP, so the terminal's hangup cannot end it, and `exec`s into `sleep`, so the recorded pid is the
/// process the supervisor started: only the kernel's parent-death signal (Linux) can end it when the supervisor dies.
fn agent_argv(pid_file: &Path) -> Vec<String> {
    #[cfg(unix)]
    {
        vec!["/bin/sh".into(), "-c".into(), format!("trap '' HUP; echo $$ > '{}'; exec sleep 600", pid_file.display())]
    }
    #[cfg(windows)]
    {
        vec![
            "powershell.exe".into(),
            "-NoProfile".into(),
            "-Command".into(),
            format!("[IO.File]::WriteAllText('{}', [string]$PID); Start-Sleep 600", pid_file.display()),
        ]
    }
}

fn read_pid(pid_file: &Path) -> u32 {
    poll_until(
        || std::fs::read_to_string(pid_file).ok().and_then(|s| s.trim().parse().ok()),
        Duration::from_secs(30),
        "the agent to record its pid",
    )
}

/// The agent is gone: no such process, or one that is only a zombie awaiting a reaper.
#[cfg(target_os = "linux")]
fn process_gone(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat.rfind(')').is_some_and(|close| stat[close + 1..].split_whitespace().next() == Some("Z")),
    }
}

#[cfg(windows)]
fn process_gone(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: plain Win32 calls on a handle this function opens and closes.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return true;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code != STILL_ACTIVE
    }
}

/// Ends an agent the test's own supervisor started, after a red left it running. Only ever a pid the agent itself
/// recorded.
fn end_agent(pid: u32) {
    #[cfg(unix)]
    // SAFETY: SIGKILL to the agent this test started through its own supervisor, a plain syscall.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    // SAFETY: plain Win32 calls on a handle this block opens and closes; it ends the process by that handle.
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

/// The agent is the supervisor's own child: the supervisor's direct children are the agent and nothing named
/// `sot-capsule`, so no `sot-capsule run` stands between them.
#[cfg(target_os = "linux")]
#[test]
fn one_process_per_row() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let pid_file = dir.path().join("agent.pid");
    let argv = agent_argv(&pid_file);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let guard = spawn_supervisor(&state_dir, "--start", &argv);
    let agent = read_pid(&pid_file);
    let conn = wait_for_lane(&state_dir_hash(&state_dir), Duration::from_secs(30));
    wait_for_ready(&conn, Duration::from_secs(90));

    let children = direct_children_of(guard.id());
    let names: Vec<String> = children
        .iter()
        .map(|c| std::fs::read_to_string(format!("/proc/{c}/comm")).unwrap_or_default().trim().to_string())
        .collect();
    let parent_of_agent = std::fs::read_to_string(format!("/proc/{agent}/stat"))
        .ok()
        .and_then(|stat| stat.rfind(')').map(|close| stat[close + 1..].split_whitespace().nth(1).map(str::to_string)))
        .flatten();
    end_agent(agent);
    assert_eq!(
        parent_of_agent.as_deref(),
        Some(guard.id().to_string().as_str()),
        "the agent's parent is the supervisor; the supervisor's children are {children:?} ({names:?})"
    );
    assert!(
        names.iter().all(|n| n != "sot-capsule"),
        "no `sot-capsule run` stands between the supervisor and the agent: children {children:?} ({names:?})"
    );
}

/// The agent ends within two seconds of its supervisor being killed outright: SIGKILL on Unix, TerminateProcess on
/// Windows, delivered by the test to the supervisor it started.
#[test]
fn the_agent_dies_with_its_supervisor() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let pid_file = dir.path().join("agent.pid");
    let argv = agent_argv(&pid_file);
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let mut guard = spawn_supervisor(&state_dir, "--start", &argv);
    let agent = read_pid(&pid_file);
    let conn = wait_for_lane(&state_dir_hash(&state_dir), Duration::from_secs(30));
    wait_for_ready(&conn, Duration::from_secs(90));
    drop(conn);

    guard.child_mut().kill().expect("kill the supervisor this test started");
    let killed = Instant::now();
    let gone = free_within(|| process_gone(agent), Duration::from_secs(2));
    let took = killed.elapsed();
    if !gone {
        end_agent(agent);
    }
    assert!(gone, "the agent {agent} outlived its killed supervisor by {took:?}");
}

/// A reset is admitted the instant an `end_run` has answered: the answer means the run has ended, so the next
/// command never meets an end still in progress.
#[test]
fn a_reset_after_end_run_is_admitted() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let mut guard = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&state_dir_hash(&state_dir), Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let ended = command(&conn, "end-1", SupervisorOp::EndRun { reason: "test".into(), voyage: voyage.clone() });
    assert!(
        matches!(ended, SupervisorOperationState::RecordClosed | SupervisorOperationState::RecordVerified),
        "end_run answered: {ended:?}"
    );
    let reset = command(&conn, "reset-1", SupervisorOp::Reset { voyage: Some(voyage.clone()) });
    assert_eq!(reset, SupervisorOperationState::Accepted, "the reset right after the end_run's answer is admitted");
    let done = poll_to_terminal(&conn, "reset-1", Duration::from_secs(30));
    let SupervisorOperationState::ResetDone { new_voyage } = done else {
        panic!("the reset completed: {done:?}");
    };
    assert_ne!(new_voyage, voyage, "the reset minted a new voyage");
    let (now, _leg) = wait_for_ready(&conn, Duration::from_secs(90));
    assert_eq!(now, new_voyage, "a fresh agent runs under the new voyage");

    let stop = command(&conn, "stop-1", SupervisorOp::Stop);
    assert_eq!(stop, SupervisorOperationState::Stopping);
    wait_for_exit(&mut guard, Duration::from_secs(30));
}
