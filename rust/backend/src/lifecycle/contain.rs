//! contain.rs — the platform half of "every child the daemon owns dies with its
//! tree": the process group or job, adopting a child, and the kill. What can
//! leave a tree is ADR 0050 residual 7.
//!
//! Unix: each child runs in its own process group, killed with `killpg`.
//! Windows: each child runs in its own anonymous job (no breakaway),
//! created suspended, assigned, and only then resumed, so no instruction
//! runs outside the job (the ADR 0041 rule `sot_log::capsule::producer::conpty` follows).
//! [`crate::lifecycle::child_signal::Signal::spawn`] owns the registry; this module holds
//! nothing but the platform calls.

/// One child's containment. Dropping it kills everything inside.
pub(crate) struct Tree {
    #[cfg(unix)]
    pgid: i32,
    #[cfg(windows)]
    job: sot_log::capsule::producer::conpty::AnonymousJob,
}

/// Make `cmd`'s child start inside its own containment.
pub(crate) fn prepare(cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
}

/// Take hold of a child [`prepare`] started.
pub(crate) fn adopt(child: &tokio::process::Child) -> std::io::Result<Tree> {
    #[cfg(unix)]
    {
        let pid = child
            .id()
            .ok_or_else(|| std::io::Error::other("the child was reaped before it was contained"))?;
        Ok(Tree { pgid: pid as i32 })
    }
    #[cfg(windows)]
    {
        let pid = child
            .id()
            .ok_or_else(|| std::io::Error::other("the child was reaped before it was contained"))?;
        let handle = child
            .raw_handle()
            .ok_or_else(|| std::io::Error::other("the child was reaped before it was contained"))?;
        let job = sot_log::capsule::producer::conpty::AnonymousJob::create().map_err(std::io::Error::other)?;
        // SAFETY: both handles are live; the job is ours and the process handle is the child's.
        if unsafe { windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(job.raw(), handle as _) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        resume_main_thread(pid)?;
        Ok(Tree { job })
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: a plain signal to the group this daemon created. The result
        // is ignored: ESRCH means the group is already gone.
        unsafe {
            libc::killpg(self.pgid, libc::SIGKILL);
        }
        #[cfg(windows)]
        let _ = self.job.terminate();
    }
}

/// Resume the one thread of a child started `CREATE_SUSPENDED`.
#[cfg(windows)]
fn resume_main_thread(pid: u32) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    // SAFETY: plain Toolhelp and thread calls; every handle opened here is closed here.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut tid = None;
        let mut more = Thread32First(snap, &mut entry);
        while more != 0 {
            if entry.th32OwnerProcessID == pid {
                tid = Some(entry.th32ThreadID);
                break;
            }
            more = Thread32Next(snap, &mut entry);
        }
        CloseHandle(snap);
        let tid = tid.ok_or_else(|| std::io::Error::other("the suspended child has no thread to resume"))?;
        let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, tid);
        if thread.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let resumed = ResumeThread(thread);
        let err = std::io::Error::last_os_error();
        CloseHandle(thread);
        if resumed == u32::MAX {
            return Err(err);
        }
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use crate::lifecycle::child_signal::Signal;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

    fn exits_within(pid: u32, ms: u32) -> bool {
        // SAFETY: plain process-handle calls; the handle is closed here.
        unsafe {
            let h = OpenProcess(SYNCHRONIZE, 0, pid);
            if h.is_null() {
                return true;
            }
            let r = WaitForSingleObject(h, ms);
            CloseHandle(h);
            r == WAIT_OBJECT_0
        }
    }

    /// Start `cmd` through a private signal, read the grandchild's pid from
    /// its first stdout line, fire, and report whether that pid is gone.
    async fn fire_takes_the_grandchild(mut cmd: tokio::process::Command) -> bool {
        let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
        let (mut child, _contained) = sig.spawn(&mut cmd).expect("spawn through the signal");
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let line = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
            .await
            .expect("the grandchild's pid never arrived")
            .expect("read")
            .expect("stdout closed before a pid arrived");
        let pid: u32 = line.trim().parse().unwrap_or_else(|_| panic!("not a pid: {line:?}"));
        sig.fire();
        exits_within(pid, 3000)
    }

    fn ping_tree() -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-Command",
            "$p=Start-Process -PassThru -NoNewWindow ping -ArgumentList '-n','600','127.0.0.1'; $p.Id; Start-Sleep 600",
        ]);
        cmd
    }

    #[tokio::test]
    async fn fire_kills_a_windows_tree() {
        assert!(fire_takes_the_grandchild(ping_tree()).await, "the grandchild survived the shutdown");
    }

    #[test]
    fn contain_job_permits_no_breakaway() {
        use windows_sys::Win32::System::JobObjects::{
            JobObjectExtendedLimitInformation, QueryInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        let job = sot_log::capsule::producer::conpty::AnonymousJob::create().unwrap();
        // SAFETY: a plain query into a zeroed, correctly sized struct.
        let flags = unsafe {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            let ok = QueryInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                &mut info as *mut _ as *mut _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                std::ptr::null_mut(),
            );
            assert!(ok != 0, "QueryInformationJobObject: {}", std::io::Error::last_os_error());
            info.BasicLimitInformation.LimitFlags
        };
        assert_eq!(flags, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, "the containment job permits a breakaway");
    }

    /// Needs julia; started the way the daemon starts every julia.
    #[tokio::test]
    #[ignore = "needs julia"]
    async fn fire_kills_a_julia_detached_grandchild_windows() {
        let (julia, _) = crate::sidecars::julia::resolve_bin().expect("resolve julia");
        let mut cmd = tokio::process::Command::new(julia);
        cmd.args([
            "-e",
            "run(`cmd /c exit 0`); p=run(detach(`ping -n 600 127.0.0.1`);wait=false);println(getpid(p));flush(stdout);sleep(600)",
        ]);
        assert!(fire_takes_the_grandchild(cmd).await, "the julia-detached grandchild survived the shutdown");
    }

    #[tokio::test]
    #[ignore = "needs Git for Windows bash"]
    async fn fire_kills_an_msys_launched_grandchild_windows() {
        let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".to_string());
        let bash = std::path::Path::new(&pf).join(r"Git\usr\bin\bash.exe");
        assert!(bash.exists(), "Git for Windows bash not found");
        let mut cmd = tokio::process::Command::new(bash);
        cmd.args(["-c", "ping -n 600 127.0.0.1 >/dev/null & cat /proc/$!/winpid; exec sleep 600"]);
        assert!(fire_takes_the_grandchild(cmd).await, "the MSYS-launched grandchild survived the shutdown");
    }

    /// The daemon itself inside a job: a nested job must still contain.
    #[tokio::test]
    #[ignore = "puts the test process itself in a job"]
    async fn contain_from_a_jobbed_daemon_windows() {
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let outer = sot_log::capsule::producer::conpty::AnonymousJob::create().unwrap();
        // SAFETY: assigns this process to a job we own; the job is leaked so it never ends the test run.
        let ok = unsafe { AssignProcessToJobObject(outer.raw(), GetCurrentProcess()) };
        assert!(ok != 0, "AssignProcessToJobObject: {}", std::io::Error::last_os_error());
        std::mem::forget(outer);
        assert!(fire_takes_the_grandchild(ping_tree()).await, "the grandchild survived the shutdown");
    }
}
