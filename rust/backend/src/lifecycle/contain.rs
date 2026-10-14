//! Platform containment, checked termination requests, and exit observations (unreaped on Unix).
//! Successful requests do not prove tree death; escapes remain ADR 0050 residual 7.
//!
//! Unix: each child runs in its own process group, killed with `killpg`, and its leader by pid as well.
//! Windows: each child runs in its own anonymous job (no breakaway),
//! created suspended, assigned, and only then resumed, so no instruction
//! runs outside the job (the ADR 0041 rule `sot_log::capsule::producer::conpty` follows).
//! On Unix a group's number is its leader's pid, retained until reap, so requests
//! run before the owner reaps that leader. On Windows wait/try_wait can obtain
//! direct-child status before the job request; the retained job handle preserves
//! containment identity across that wait. Explicit kill requests termination first.
//! Requests run under the registry lock. [`crate::lifecycle::child_signal::Signal::spawn`]
//! owns the registry; this module holds nothing but the platform calls.
//! On macOS only, a group-request EPERM counts as no live member when a libproc listing, each member's short status record and a second listing show every member ended, exiting, a zombie or in another group, and none added; any other observation returns its step and evidence with the original EPERM as source. No Unix signal or process-group identity query occurs after leader reap.

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
/// `CREATE_SUSPENDED`: std has no getter to add to them, so a caller that needs a flag of its own adds it here. On Unix
/// the child also keeps none of this process's descriptors past its exec but its stdio ([`inherit_none`]).
pub(crate) fn prepare(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        // SAFETY: the hook runs in the child between fork and exec and makes only async-signal-safe calls.
        unsafe { cmd.pre_exec(inherit_none) };
    }
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(
        cmd,
        windows_sys::Win32::System::Threading::CREATE_SUSPENDED,
    );
}

/// In a child between fork and exec: marks every descriptor above stdio close-on-exec, so the child keeps none of this
/// process's past its exec. A descriptor is not always close-on-exec from its birth: on macOS interprocess, mio and std
/// set the flag with a second call after creating a socket or a pipe, and a child started by another thread between the
/// two kept it, the daemon's listener and its accepted connections included. Marked, not closed: std reports a failed
/// exec through a close-on-exec pipe it holds open until then.
#[cfg(unix)]
fn inherit_none() -> std::io::Result<()> {
    // SAFETY: close_range, getdtablesize and fcntl act on this process's own descriptor table; a number not open is
    // EBADF and changes nothing.
    unsafe {
        #[cfg(target_os = "linux")]
        if libc::syscall(
            libc::SYS_close_range,
            3u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        ) == 0
        {
            return Ok(());
        }
        for fd in 3..libc::getdtablesize() {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    Ok(())
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
        let pid = pid
            .ok_or_else(|| std::io::Error::other("the child was reaped before it was contained"))?;
        Ok(Tree {
            pgid: pid as i32,
            terminated: false,
        })
    }
    #[cfg(windows)]
    {
        let pid = pid
            .ok_or_else(|| std::io::Error::other("the child was reaped before it was contained"))?;
        let handle = process
            .ok_or_else(|| std::io::Error::other("the child was reaped before it was contained"))?;
        let job = sot_log::capsule::producer::conpty::AnonymousJob::create()
            .map_err(std::io::Error::other)?;
        assign(&job, pid, handle)?;
        let mut tree = Tree {
            job,
            terminated: false,
        };
        if let Err(error) = resume_main_thread(pid) {
            return Err(combine(error, tree.terminate()));
        }
        Ok(tree)
    }
}

/// Put the suspended child in its job. The tests' daemon role has two faults here (`SOT_L2_PAUSE_ADOPT`, `SOT_L2_NO_JOB_ASSIGNMENT`).
#[cfg(windows)]
#[cfg_attr(not(test), allow(unused_variables))]
fn assign(
    job: &sot_log::capsule::producer::conpty::AnonymousJob,
    pid: u32,
    handle: std::os::windows::io::RawHandle,
) -> std::io::Result<()> {
    #[cfg(test)]
    {
        if let Some(path) = std::env::var_os("SOT_L2_PAUSE_ADOPT") {
            std::fs::write(path, pid.to_string())?;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        }
        if std::env::var_os("SOT_L2_NO_JOB_ASSIGNMENT").is_some() {
            return Ok(());
        }
    }
    // SAFETY: both handles are live; the job is ours and the process handle is the child's.
    if unsafe {
        windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(job.raw(), handle as _)
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
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
                (Err(group), Err(leader)) => Err(std::io::Error::other(format!(
                    "group: {group}; leader: {leader}"
                ))),
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
    Tree {
        pgid: pid as i32,
        terminated: false,
    }
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

/// A group and its unreaped leader are checked independently. On macOS a group request refused with EPERM is done when
/// every member is no longer shown, exiting, a zombie or in another group (`macos::checked_no_live_group`).
#[cfg(unix)]
fn request(pid: i32, group: bool) -> std::io::Result<()> {
    // SAFETY: the caller retains the unreaped leader identity, and this only signals its own group or leader.
    #[cfg(test)]
    REQUEST_EVENTS.with(|events| {
        events
            .borrow_mut()
            .push(if group { "group" } else { "leader" })
    });
    let rc = unsafe {
        if group {
            libc::killpg(pid, libc::SIGKILL)
        } else {
            libc::kill(pid, libc::SIGKILL)
        }
    };
    // Capture errno before observations or test hooks can overwrite it.
    let result = if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    };
    #[cfg(target_os = "macos")]
    let result = match result {
        Err(error) if group && error.raw_os_error() == Some(libc::EPERM) => {
            match macos::checked_no_live_group(pid) {
                Ok(()) => Ok(()),
                Err(observation) => Err(std::io::Error::new(
                    error.kind(),
                    macos::RejectedGroup {
                        request: error,
                        observation,
                    },
                )),
            }
        }
        result => result,
    };
    #[cfg(test)]
    if REQUEST_FAILURE.with(|failure| failure.get() & if group { 1 } else { 2 } != 0) {
        return Err(std::io::Error::other(if group {
            "injected group request failure"
        } else {
            "injected leader request failure"
        }));
    }
    match result {
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
        result => result,
    }
}

#[cfg(target_os = "macos")]
pub(super) mod macos {
    use std::io;

    // sys/proc_info.h in the macOS SDK. libc supplies proc_listpids, proc_pidinfo, __error and SZOMB, not these.
    const PROC_PGRP_ONLY: u32 = 2;
    const PROC_PIDT_SHORTBSDINFO: i32 = 13;
    const PROC_FLAG_INEXIT: u32 = 4;

    /// `struct proc_bsdshortinfo`: unlike `proc_bsdinfo` its query has no same-user check.
    #[repr(C)]
    #[allow(
        dead_code,
        reason = "the SDK layout; the check reads five of its fields"
    )]
    struct ShortBsdInfo {
        pid: u32,
        ppid: u32,
        pgid: u32,
        status: u32,
        comm: [u8; 16],
        flags: u32,
        uid: u32,
        gid: u32,
        ruid: u32,
        rgid: u32,
        svuid: u32,
        svgid: u32,
        rfu: u32,
    }
    const _: () = assert!(std::mem::size_of::<ShortBsdInfo>() == 64);

    #[cfg(test)]
    thread_local! {
        pub(crate) static FAULT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
        pub(crate) static EVENTS: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    pub(super) struct RejectedGroup {
        pub(super) request: io::Error,
        pub(super) observation: io::Error,
    }

    impl std::fmt::Display for RejectedGroup {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "macOS group EPERM: {}; request: {}",
                self.observation, self.request
            )
        }
    }

    impl std::fmt::Debug for RejectedGroup {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            std::fmt::Display::fmt(self, f)
        }
    }

    impl std::error::Error for RejectedGroup {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.request)
        }
    }

    #[derive(Debug)]
    pub(in crate::lifecycle) struct Observation {
        rejection: Option<(&'static str, &'static str)>,
        evidence: Vec<String>,
    }

    impl std::fmt::Display for Observation {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let (step, reason) = self.rejection.unwrap_or(("accepted", "all-ended"));
            write!(
                f,
                "step={step} reason={reason}; {}",
                self.evidence.join("; ")
            )
        }
    }

    impl std::error::Error for Observation {}

    impl Observation {
        fn reject(&mut self, step: &'static str, reason: &'static str) {
            if self.rejection.is_none() {
                self.rejection = Some((step, reason));
            }
        }
    }

    pub(in crate::lifecycle) fn checked_no_live_group(group: i32) -> io::Result<()> {
        let observation = observe(group);
        if observation.rejection.is_some() {
            Err(io::Error::other(observation))
        } else {
            Ok(())
        }
    }

    /// Read-only evidence of a group whose number is the retained, unreaped leader's pid.
    pub(in crate::lifecycle) fn observe(group: i32) -> Observation {
        let mut observation = Observation {
            rejection: None,
            evidence: Vec::new(),
        };
        let Some(first) = members(&mut observation, group, "membership-first") else {
            return observation;
        };
        for &pid in &first {
            member(&mut observation, group, pid);
        }
        if observation.rejection.is_some() {
            return observation;
        }
        // A pid that left the listing was reaped or left the group; only a pid that joined is a rejection.
        if let Some(second) = members(&mut observation, group, "membership-second") {
            if second.iter().any(|pid| !first.contains(pid)) {
                observation.reject("membership-second", "membership-grew");
            }
        }
        observation
    }

    fn members(observation: &mut Observation, group: i32, step: &'static str) -> Option<Vec<i32>> {
        #[cfg(test)]
        EVENTS.with(|events| events.borrow_mut().push("members"));
        let width = std::mem::size_of::<i32>();
        // SAFETY: the SDK sizing call writes nothing; it returns room for every process plus 20.
        let sizing =
            unsafe { libc::proc_listpids(PROC_PGRP_ONLY, group as u32, std::ptr::null_mut(), 0) };
        #[cfg(test)]
        let sizing = match (FAULT.with(|fault| fault.get()), step) {
            (6, "membership-first") => 0,
            _ => sizing,
        };
        if sizing <= 0 {
            let errno = io::Error::last_os_error().raw_os_error();
            observation
                .evidence
                .push(format!("{step} sizing={sizing} errno={errno:?}"));
            observation.reject(step, "sizing-failed");
            return None;
        }
        let bytes = sizing as usize + width;
        let mut pids = vec![0i32; bytes / width];
        // SAFETY: errno is this thread's own; cleared, it tells an empty listing (no errno) from a failed one.
        unsafe { *libc::__error() = 0 };
        // SAFETY: this writable buffer has exactly bytes bytes.
        let count = unsafe {
            libc::proc_listpids(
                PROC_PGRP_ONLY,
                group as u32,
                pids.as_mut_ptr().cast(),
                bytes as i32,
            )
        };
        let errno = (count <= 0)
            .then(|| io::Error::last_os_error().raw_os_error())
            .flatten()
            .filter(|&errno| errno != 0);
        #[cfg(test)]
        let (count, errno) = match (FAULT.with(|fault| fault.get()), step) {
            (1, "membership-first") => (0, None),
            (2, "membership-first") => (bytes as i32, errno),
            (7, "membership-first") => (0, Some(libc::EIO)),
            _ => (count, errno),
        };
        let listed = usize::try_from(count).unwrap_or(0);
        pids.truncate(listed.min(bytes) / width);
        #[cfg(test)]
        if FAULT.with(|fault| fault.get()) == 5 && step == "membership-second" {
            pids.push(1);
        }
        observation.evidence.push(format!(
            "{step} sizing={sizing} count={count} errno={errno:?} pids={pids:?}"
        ));
        if count <= 0 {
            let reason = if errno.is_some() {
                "listing-failed"
            } else {
                "listing-empty"
            };
            observation.reject(step, reason);
            return None;
        }
        if listed >= bytes {
            observation.reject(step, "listing-truncated");
            return None;
        }
        Some(pids)
    }

    /// One listed pid: ended when it is no longer shown, is in another group, is a zombie or is exiting.
    fn member(observation: &mut Observation, group: i32, pid: i32) {
        #[cfg(test)]
        EVENTS.with(|events| events.borrow_mut().push("status"));
        // SAFETY: the buffer is the SDK layout; arg=1 includes zombies. This only reads evidence.
        let mut info: ShortBsdInfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<ShortBsdInfo>() as i32;
        let count = unsafe {
            libc::proc_pidinfo(
                pid,
                PROC_PIDT_SHORTBSDINFO,
                1,
                (&mut info as *mut ShortBsdInfo).cast(),
                size,
            )
        };
        let errno = (count <= 0)
            .then(|| io::Error::last_os_error().raw_os_error())
            .flatten();
        #[cfg(test)]
        let (count, errno) = match FAULT.with(|fault| fault.get()) {
            3 => (size - 1, errno),
            8 => (0, Some(libc::ESRCH)),
            _ => (count, errno),
        };
        // Faults 4 and 8-11 leave one state each: live, not shown, exiting, in another group, a zombie.
        #[cfg(test)]
        match FAULT.with(|fault| fault.get()) {
            4 => {
                info.status = libc::SRUN;
                info.flags = 0;
            }
            9 => {
                info.status = libc::SRUN;
                info.flags = PROC_FLAG_INEXIT;
            }
            10 => {
                info.pgid = group as u32 + 1;
                info.status = libc::SRUN;
                info.flags = 0;
            }
            11 => info.flags = 0,
            _ => {}
        }
        let mut text = format!("member pid={pid} count={count} errno={errno:?}");
        if count == size {
            text.push_str(&format!(
                " group={} status={} flags={:#x} uid={} ruid={}",
                info.pgid, info.status, info.flags, info.uid, info.ruid
            ));
        }
        observation.evidence.push(text);
        if errno == Some(libc::ESRCH) {
            return;
        }
        if count != size {
            observation.reject("member", "query-failed");
            return;
        }
        let ended = info.pgid != group as u32
            || info.status == libc::SZOMB
            || info.flags & PROC_FLAG_INEXIT != 0;
        if !ended {
            observation.reject("member", "live-member");
        }
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

/// Whether `child` has exited; with `block` this waits. Unix retains its unreaped identity;
/// Windows uses wait/try_wait while the caller retains its job handle.
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
        let tid = tid
            .ok_or_else(|| std::io::Error::other("the suspended child has no thread to resume"))?;
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
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

    /// A retained observation handle; a reported PID never becomes a termination target.
    struct Watched(windows_sys::Win32::Foundation::HANDLE);

    impl Watched {
        fn open(pid: u32) -> Result<Self, String> {
            // SAFETY: an observation-only process handle, closed by Watched.
            let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
            if handle.is_null() {
                Err(format!(
                    "process {pid} could not be opened to watch: {}",
                    std::io::Error::last_os_error()
                ))
            } else {
                Ok(Self(handle))
            }
        }

        fn exits_within(&self, ms: u32) -> Result<bool, String> {
            // SAFETY: a wait on the retained observation handle; no process is signalled.
            match unsafe { WaitForSingleObject(self.0, ms) } {
                WAIT_OBJECT_0 => Ok(true),
                WAIT_TIMEOUT => Ok(false),
                WAIT_FAILED => Err(format!(
                    "native process wait failed: {}",
                    std::io::Error::last_os_error()
                )),
                result => Err(format!("unexpected native process wait result: {result}")),
            }
        }
    }

    impl Drop for Watched {
        fn drop(&mut self) {
            // SAFETY: the handle is this value's own.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// One tree fixture's output: stdout and stderr are files in the fixture's own folder, so no reader exists to
    /// block the tree or to supply its death. Readiness is stdout's first complete numeric line (a grandchild that
    /// shares stdout, like `ping`, may write first); a failure carries a bounded excerpt of both files.
    struct TreeFiles {
        dir: tempfile::TempDir,
        leader: Arc<AtomicU32>,
    }

    impl TreeFiles {
        /// Points `cmd`'s standard handles at this fixture's files and records the root that `sig` creates.
        fn new(sig: &'static Signal, cmd: &mut std::process::Command) -> Result<Self, String> {
            let dir = tempfile::tempdir().map_err(|error| format!("fixture folder: {error}"))?;
            let file = |name: &str| {
                std::fs::File::create(dir.path().join(name))
                    .map_err(|error| format!("fixture {name} file: {error}"))
            };
            cmd.stdin(std::process::Stdio::null())
                .stdout(file("stdout")?)
                .stderr(file("stderr")?);
            let leader = Arc::new(AtomicU32::new(0));
            let recorded = leader.clone();
            // The private signal creates exactly one tree, so its hook is never reset.
            *sig.after_create.lock().unwrap() = Some(Box::new(move |pid| {
                recorded.store(pid, Ordering::SeqCst);
            }));
            Ok(Self { dir, leader })
        }

        /// The recorded root, opened while its owner still holds it.
        fn leader(&self) -> Result<Watched, String> {
            Watched::open(self.leader.load(Ordering::SeqCst))
        }

        /// The descendant's PID from stdout's first complete numeric line. The leader is checked before stdout is
        /// read, so a line written before it exited is still found; an exited leader or 30 s without one is an error.
        fn ready(&self, leader: &Watched) -> Result<u32, String> {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                let exited = leader.exits_within(0)?;
                let stdout = std::fs::read(self.dir.path().join("stdout"))
                    .map_err(|error| format!("read fixture stdout: {error}"))?;
                let complete = stdout
                    .iter()
                    .rposition(|&byte| byte == b'\n')
                    .map_or(0, |end| end + 1);
                if let Some(pid) = String::from_utf8_lossy(&stdout[..complete])
                    .lines()
                    .find_map(|line| line.trim().parse().ok())
                {
                    return Ok(pid);
                }
                if exited {
                    return Err("fixture leader exited before descendant readiness".into());
                }
                if Instant::now() >= deadline {
                    return Err("descendant readiness exceeded 30 s".into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        /// The observation, with both files' excerpts appended to an error.
        fn result(&self, observation: Result<(), String>) -> Result<(), String> {
            observation.map_err(|reason| {
                format!(
                    "{reason}\n{}\n{}",
                    self.excerpt("stdout"),
                    self.excerpt("stderr")
                )
            })
        }

        /// A stream's first 4 KiB and last 12 KiB, with the count of bytes left out between them.
        fn excerpt(&self, name: &str) -> String {
            let bytes = match std::fs::read(self.dir.path().join(name)) {
                Ok(bytes) => bytes,
                Err(error) => return format!("{name}: unreadable: {error}"),
            };
            let (head, tail) = if bytes.len() > 16 * 1024 {
                (&bytes[..4 * 1024], &bytes[bytes.len() - 12 * 1024..])
            } else {
                (&bytes[..], &bytes[..0])
            };
            format!(
                "{name}: {} bytes, {} omitted between head and tail\n{}\n{}",
                bytes.len(),
                bytes.len() - head.len() - tail.len(),
                String::from_utf8_lossy(head),
                String::from_utf8_lossy(tail)
            )
        }
    }

    /// Waits for readiness, applies `end` (the one termination under test), then requires the leader and the
    /// descendant to exit within one shared 3 s deadline. An owner moved into `end` drops with it on every path.
    fn tree_ends(
        files: &TreeFiles,
        end: impl FnOnce(&Watched, &Watched) -> Result<(), String>,
    ) -> Result<(), String> {
        let leader = files.leader()?;
        let descendant = Watched::open(files.ready(&leader)?)?;
        end(&leader, &descendant)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        for (label, watched) in [("leader", &leader), ("descendant", &descendant)] {
            let millis = deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u32;
            if !watched.exits_within(millis)? {
                return Err(format!("{label} survived past the shared 3 s deadline"));
            }
        }
        Ok(())
    }

    /// Fire alone supplies the death: the owner outlives the observation.
    fn fire_takes_the_tree(mut cmd: tokio::process::Command) -> Result<(), String> {
        let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
        let files = TreeFiles::new(sig, cmd.as_std_mut())?;
        let owner = sig
            .spawn(&mut cmd)
            .map_err(|error| format!("spawn through the signal: {error}"))?;
        let observation = tree_ends(&files, |_, _| {
            sig.fire().map_err(|error| format!("fire: {error}"))
        });
        drop(owner);
        files.result(observation)
    }

    fn std_drop_takes_the_tree(mut cmd: std::process::Command) -> Result<(), String> {
        let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
        let files = TreeFiles::new(sig, &mut cmd)?;
        let owner = sig
            .spawn_std(&mut cmd)
            .map_err(|error| format!("spawn_std: {error}"))?;
        files.result(tree_ends(&files, move |_, _| {
            drop(owner);
            Ok(())
        }))
    }

    /// Retaining the owner keeps the tree live; dropping it, with no fire, is the only termination.
    fn async_drop_takes_the_tree(mut cmd: tokio::process::Command) -> Result<(), String> {
        let sig: &'static Signal = Box::leak(Box::new(Signal::new()));
        let files = TreeFiles::new(sig, cmd.as_std_mut())?;
        let owner = sig
            .spawn(&mut cmd)
            .map_err(|error| format!("spawn through a private signal: {error}"))?;
        files.result(tree_ends(&files, move |leader, descendant| {
            if leader.exits_within(100)? || descendant.exits_within(0)? {
                return Err("retained-owner control observed tree death".into());
            }
            drop(owner);
            Ok(())
        }))
    }

    fn ping_tree() -> std::process::Command {
        let mut cmd = std::process::Command::new("powershell");
        cmd.args([
            "-NoProfile", "-Command",
            "$p=Start-Process -PassThru -NoNewWindow ping -ArgumentList '-n','600','127.0.0.1'; $p.Id; Start-Sleep 600",
        ]);
        cmd
    }

    #[tokio::test]
    async fn fire_kills_a_windows_tree() {
        fire_takes_the_tree(ping_tree().into()).expect("fire fixture");
    }

    #[test]
    fn a_dropped_std_child_takes_its_windows_tree() {
        std_drop_takes_the_tree(ping_tree()).expect("std Drop fixture");
    }

    #[tokio::test]
    async fn a_dropped_async_child_takes_its_windows_tree() {
        async_drop_takes_the_tree(ping_tree().into()).expect("async Drop fixture");
        println!("WINDOWS async Drop PASS: live leader and descendant exited after owner drop; signal not fired");
    }

    /// 128 KiB on each stream between a prefix and a suffix line, then exit with no numeric line.
    fn pressure_command() -> std::process::Command {
        let mut cmd = std::process::Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-Command",
            concat!(
                "$data='x' * 131072; ",
                "[Console]::Out.WriteLine('STDOUT-PREFIX'); [Console]::Out.Write($data); ",
                "[Console]::Out.WriteLine('STDOUT-SUFFIX'); [Console]::Out.Flush(); ",
                "[Console]::Error.WriteLine('STDERR-PREFIX'); [Console]::Error.Write($data); ",
                "[Console]::Error.WriteLine('STDERR-SUFFIX'); [Console]::Error.Flush(); exit 0"
            ),
        ]);
        cmd
    }

    #[tokio::test]
    async fn all_windows_tree_fixtures_keep_bounded_diagnostics() {
        for mode in ["std", "async", "fire"] {
            let cmd = pressure_command();
            let result = match mode {
                "std" => std_drop_takes_the_tree(cmd),
                "async" => async_drop_takes_the_tree(cmd.into()),
                _ => fire_takes_the_tree(cmd.into()),
            };
            let text = result.expect_err("a fixture that printed no PID became ready");
            let missing: Vec<_> = [
                "fixture leader exited before descendant readiness",
                "STDOUT-PREFIX",
                "STDOUT-SUFFIX",
                "STDERR-PREFIX",
                "STDERR-SUFFIX",
            ]
            .into_iter()
            .filter(|item| !text.contains(item))
            .collect();
            assert!(
                missing.is_empty() && text.len() < 34 * 1024,
                "{mode} fixture: missing {missing:?} or longer than 34 KiB ({} bytes): {text}",
                text.len()
            );
        }
        println!(
            "WINDOWS shared fixture diagnostics PASS: std, async and fire retain both streams"
        );
    }

    #[test]
    fn contain_job_permits_no_breakaway() {
        use windows_sys::Win32::System::JobObjects::{
            JobObjectExtendedLimitInformation, QueryInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
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
            assert!(
                ok != 0,
                "QueryInformationJobObject: {}",
                std::io::Error::last_os_error()
            );
            info.BasicLimitInformation.LimitFlags
        };
        assert_eq!(
            flags, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            "the containment job permits a breakaway"
        );
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
        fire_takes_the_tree(cmd).expect("julia fire fixture");
    }

    #[tokio::test]
    #[ignore = "needs Git for Windows bash"]
    async fn fire_kills_an_msys_launched_grandchild_windows() {
        let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".to_string());
        let bash = std::path::Path::new(&pf).join(r"Git\usr\bin\bash.exe");
        assert!(bash.exists(), "Git for Windows bash not found");
        let mut cmd = tokio::process::Command::new(bash);
        cmd.args([
            "-c",
            "ping -n 600 127.0.0.1 >/dev/null & cat /proc/$!/winpid; exec sleep 600",
        ]);
        fire_takes_the_tree(cmd).expect("MSYS fire fixture");
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
        assert!(
            ok != 0,
            "AssignProcessToJobObject: {}",
            std::io::Error::last_os_error()
        );
        std::mem::forget(outer);
        fire_takes_the_tree(ping_tree().into()).expect("fire fixture");
    }
}
