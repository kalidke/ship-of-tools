//! Platform containment, checked termination requests, and exit observations without reaping.
//! Successful requests do not prove tree death; escapes remain ADR 0050 residual 7.
//!
//! Unix: each child runs in its own process group, killed with `killpg`, and its leader by pid as well.
//! Windows: each child runs in its own anonymous job (no breakaway),
//! created suspended, assigned, and only then resumed, so no instruction
//! runs outside the job (the ADR 0041 rule `sot_log::capsule::producer::conpty` follows).
//! A group's number is its leader's pid, and a pid (a zombie's too) is not
//! reused before its reap, so termination is requested only while its leader is
//! unreaped: the owners of [`crate::lifecycle::child_signal::Contained`] and
//! [`crate::lifecycle::child_signal::ContainedStd`] reap the leader
//! after successful requests, never before, and requests run under the
//! registry lock. [`crate::lifecycle::child_signal::Signal::spawn`] owns the registry; this
//! module holds nothing but the platform calls.

/// One child's containment. Explicit termination checks requests; Drop logs failures.
pub(crate) struct Tree {
    terminated: bool,
    #[cfg(unix)]
    pgid: i32,
    #[cfg(windows)]
    job: sot_log::capsule::producer::conpty::AnonymousJob,
}

impl Tree {
    #[cfg(all(test, unix))]
    pub(crate) fn pgid(&self) -> i32 {
        self.pgid
    }
}

/// Make `cmd`'s child start inside its own containment. On Windows this replaces the command's creation flags with
/// `CREATE_SUSPENDED`: std has no getter to add to them, so a caller that needs a flag of its own adds it here.
pub(crate) fn prepare(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(cmd, 0);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(cmd, windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
}

/// Take hold of a child [`prepare`] started, given its pid (and, on
/// Windows, its process handle); `None` means it was reaped already.
pub(crate) fn adopt(
    pid: Option<u32>,
    #[cfg(windows)] process: Option<std::os::windows::io::RawHandle>,
) -> std::io::Result<Tree> {
    #[cfg(all(test, windows))]
    if ADOPT_FAILURE.with(|failure| failure.get()) {
        return Err(std::io::Error::other("injected job assignment failure"));
    }
    #[cfg(unix)]
    {
        let pid = pid.ok_or_else(|| std::io::Error::other("the child was reaped before it was contained"))?;
        Ok(Tree { pgid: pid as i32, terminated: false })
    }
    #[cfg(windows)]
    {
        let pid = pid.ok_or_else(|| std::io::Error::other("the child was reaped before it was contained"))?;
        let handle = process.ok_or_else(|| std::io::Error::other("the child was reaped before it was contained"))?;
        let job = sot_log::capsule::producer::conpty::AnonymousJob::create().map_err(std::io::Error::other)?;
        // SAFETY: both handles are live; the job is ours and the process handle is the child's.
        if unsafe { windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(job.raw(), handle as _) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut tree = Tree { job, terminated: false };
        if let Err(error) = resume_main_thread(pid) {
            return Err(combine(error, tree.terminate()));
        }
        Ok(tree)
    }
}

impl Tree {
    /// Attempt every request while the leader identity is retained. Never retry after a successful release/reap.
    pub(crate) fn terminate(&mut self) -> std::io::Result<()> {
        if self.terminated {
            return Ok(());
        }
        #[cfg(unix)]
        let result = {
            let group = request(self.pgid, true);
            let leader = request(self.pgid, false);
            match (group, leader) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(group), Err(leader)) => Err(std::io::Error::other(format!("group: {group}; leader: {leader}"))),
                (Err(error), _) | (_, Err(error)) => Err(error),
            }
        };
        #[cfg(windows)]
        let result = request_job(&self.job);
        self.terminated = result.is_ok();
        result
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        if let Err(error) = self.terminate() {
            tracing::error!(%error, "contained tree Drop: termination request failed");
        }
    }
}

#[cfg(windows)]
fn request_job(job: &sot_log::capsule::producer::conpty::AnonymousJob) -> std::io::Result<()> {
    let result = job.terminate().map_err(std::io::Error::other);
    #[cfg(test)]
    {
        REQUEST_EVENTS.with(|events| events.borrow_mut().push("job"));
        if REQUEST_FAILURE.with(|failure| failure.get() & 4 != 0) {
            return Err(std::io::Error::other("injected job request failure"));
        }
    }
    result
}

/// Preserve the initial failure together with a failed cleanup, rather than replace either reason.
pub(super) fn combine(error: std::io::Error, cleanup: std::io::Result<()>) -> std::io::Error {
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => std::io::Error::other(format!("{error}; cleanup: {cleanup}")),
    }
}

/// Before Unix adoption, creation already assigned the leader's own process group.
#[cfg(unix)]
pub(super) fn partial(pid: u32) -> Tree {
    Tree { pgid: pid as i32, terminated: false }
}

#[cfg(test)]
thread_local! {
    #[cfg(windows)]
    pub(super) static ADOPT_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static REQUEST_FAILURE: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    pub(crate) static PROBE_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static REQUEST_EVENTS: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
    pub(crate) static REAP_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A group and its unreaped leader are checked independently; ESRCH alone means already absent.
#[cfg(unix)]
fn request(pid: i32, group: bool) -> std::io::Result<()> {
    // SAFETY: the caller retains the unreaped leader identity, and this only signals its own group or leader.
    #[cfg(test)]
    REQUEST_EVENTS.with(|events| events.borrow_mut().push(if group { "group" } else { "leader" }));
    let rc = unsafe {
        if group {
            libc::killpg(pid, libc::SIGKILL)
        } else {
            libc::kill(pid, libc::SIGKILL)
        }
    };
    #[cfg(test)]
    if REQUEST_FAILURE.with(|failure| failure.get() & if group { 1 } else { 2 } != 0) {
        return Err(std::io::Error::other(if group {
            "injected group request failure"
        } else {
            "injected leader request failure"
        }));
    }
    if rc == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

pub(super) fn reap(child: &mut std::process::Child) -> std::io::Result<std::process::ExitStatus> {
    let status = child.wait();
    #[cfg(test)]
    if REAP_FAILURE.with(|failure| failure.get()) {
        return Err(std::io::Error::other("injected direct-child reap failure"));
    }
    status
}

/// Whether `pid` has exited, seen without reaping it (`WNOWAIT`), so its
/// number stays its own until its owner reaps it. With `block` this waits
/// for the exit.
#[cfg(unix)]
pub(super) fn exited_pid(pid: u32, block: bool) -> std::io::Result<bool> {
    #[cfg(test)]
    if PROBE_FAILURE.with(|failure| failure.get()) {
        return Err(std::io::Error::other("injected exit probe failure"));
    }
    let options = libc::WEXITED | libc::WNOWAIT | if block { 0 } else { libc::WNOHANG };
    loop {
        // SAFETY: a zeroed siginfo_t is a valid out-parameter; waitid only writes it.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: a plain wait on one pid with WNOWAIT, which reaps nothing.
        let rc = unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, options) };
        if rc == 0 {
            // si_signo is set by every successful waitid; zero means nothing had exited.
            return Ok(info.si_signo == libc::SIGCHLD);
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Whether `child` has exited, without freeing its pid on Unix; with `block`
/// this waits for the exit.
pub(super) fn exited(child: &mut std::process::Child, block: bool) -> std::io::Result<bool> {
    #[cfg(all(test, windows))]
    if PROBE_FAILURE.with(|failure| failure.get()) {
        return Err(std::io::Error::other("injected exit probe failure"));
    }
    #[cfg(unix)]
    {
        exited_pid(child.id(), block)
    }
    #[cfg(windows)]
    {
        if block {
            child.wait().map(|_| true)
        } else {
            child.try_wait().map(|status| status.is_some())
        }
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

    /// A process opened while it is alive, so a later wait is on that process and not on a number.
    struct Watched(windows_sys::Win32::Foundation::HANDLE);

    impl Watched {
        fn open(pid: u32) -> Self {
            // SAFETY: a plain open of a process by pid for SYNCHRONIZE; the handle is closed on drop.
            let h = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
            assert!(!h.is_null(), "process {pid} could not be opened to watch: {}", std::io::Error::last_os_error());
            Watched(h)
        }

        fn exits_within(&self, ms: u32) -> bool {
            // SAFETY: a wait on a handle this value owns.
            unsafe { WaitForSingleObject(self.0, ms) == WAIT_OBJECT_0 }
        }
    }

    impl Drop for Watched {
        fn drop(&mut self) {
            // SAFETY: the handle is this value's own.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Start `cmd` through a private signal, read the grandchild's pid from
    /// its stdout (the first line that is a number: a grandchild that shares
    /// the pipe, like `ping`, may print first), fire, and report whether that
    /// pid is gone.
    async fn fire_takes_the_grandchild(mut cmd: tokio::process::Command) -> bool {
        let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
        let mut contained = sig.spawn(&mut cmd).expect("spawn through the signal");
        let mut lines = BufReader::new(contained.stdout.take().unwrap()).lines();
        let pid: u32 = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let line = lines.next_line().await.expect("read").expect("stdout closed before a pid arrived");
                if let Ok(pid) = line.trim().parse() {
                    return pid;
                }
            }
        })
        .await
        .expect("the grandchild's pid never arrived");
        let watched = Watched::open(pid);
        sig.fire().expect("fire");
        watched.exits_within(3000)
    }

    fn ping_tree() -> std::process::Command {
        let mut cmd = std::process::Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-Command",
            "$p=Start-Process -PassThru -NoNewWindow ping -ArgumentList '-n','600','127.0.0.1'; $p.Id; Start-Sleep 600",
        ]);
        cmd
    }

    #[tokio::test]
    async fn fire_kills_a_windows_tree() {
        assert!(fire_takes_the_grandchild(ping_tree().into()).await, "the grandchild survived the shutdown");
    }

    /// A blocking caller that drops its `ContainedStd` takes the tree.
    #[test]
    fn a_dropped_std_child_takes_its_windows_tree() {
        use std::io::BufRead;
        let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = ping_tree();
        cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
        let mut child = sig.spawn_std(&mut cmd).expect("spawn_std");
        let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
        let pid: u32 = loop {
            let line = lines.next().expect("stdout closed before a pid arrived").expect("read");
            if let Ok(pid) = line.trim().parse() {
                break pid;
            }
        };
        let watched = Watched::open(pid);
        drop(child);
        assert!(watched.exits_within(3000), "the dropped child's descendant survived");
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
        assert!(fire_takes_the_grandchild(ping_tree().into()).await, "the grandchild survived the shutdown");
    }
}
