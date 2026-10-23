//! One process per row (decision D2, pass 6b): the supervisor runs the agent itself, and the agent ends with its
//! supervisor. (A reset's admission right after an `end_run` is `row_management.rs`'s.)

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

/// Watches the agent's death without signaling it. Opened while the agent is alive, before anything is done to its
/// supervisor, so the handle names that process and no later one that reuses its pid: a pidfd on Linux (its parent
/// checked to be the supervisor this test spawned, after it is open), a process handle on Windows. The test never
/// ends the agent: a red leaves it to its dying supervisor and the guard's own cleanup of the capsule it started.
struct AgentWatch {
    #[cfg(target_os = "linux")]
    fd: std::os::fd::OwnedFd,
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

impl AgentWatch {
    /// `supervisor` is the pid of the supervisor this test spawned; the agent must be its own child.
    fn open(agent: u32, supervisor: u32) -> AgentWatch {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd as _;
            let fd = sot_platform::identity::challenge_unix::pidfd_open(agent).expect("open a pidfd for the live agent");
            let stat = std::fs::read_to_string(format!("/proc/{agent}/stat")).expect("the agent is alive after its pidfd opened");
            let parent = stat.rfind(')').and_then(|close| stat[close + 1..].split_whitespace().nth(1).map(str::to_string));
            assert_eq!(parent.as_deref(), Some(supervisor.to_string().as_str()), "the pidfd names the supervisor's own child");
            let _ = fd.as_raw_fd();
            AgentWatch { fd }
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
            use windows_sys::Win32::System::Threading::OpenProcess;
            let _ = supervisor;
            // SAFETY: a plain Win32 call; the handle is closed in `Drop`.
            let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, agent) };
            assert!(!handle.is_null(), "open a handle for the live agent");
            AgentWatch { handle }
        }
    }

    /// Whether the agent has ended within `bound`.
    fn ended_within(&self, bound: Duration) -> bool {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd as _;
            sot_platform::identity::challenge_unix::poll_pidfd_readable(self.fd.as_raw_fd(), bound).expect("poll the agent's pidfd")
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
            use windows_sys::Win32::System::Threading::WaitForSingleObject;
            // SAFETY: a plain Win32 wait on a handle this value owns.
            unsafe { WaitForSingleObject(self.handle, bound.as_millis() as u32) == WAIT_OBJECT_0 }
        }
    }
}

#[cfg(windows)]
impl Drop for AgentWatch {
    fn drop(&mut self) {
        // SAFETY: closes the handle `open` returned, once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
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
/// Windows, delivered through the `Child` handle of the supervisor the test started; the agent is only watched, never
/// signaled.
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
    let watch = AgentWatch::open(agent, guard.id());

    guard.child_mut().kill().expect("kill the supervisor this test started");
    let killed = Instant::now();
    let gone = watch.ended_within(Duration::from_secs(2));
    assert!(gone, "the agent outlived its killed supervisor by {:?}", killed.elapsed());
}
