//! A spawned `sot-capsule` that no test, panicking or killed, can leave behind.
//!
//! A `--survival normal` `run` leg outlives its supervisor by design (ADR 0041's adoption), so ending the
//! supervisor is not enough, and a `Drop` never runs in a test process that is killed. The capsule therefore runs
//! in something the kernel ends with the TEST PROCESS:
//! - Unix: the capsule joins a process group led by a watcher, a shell that reads its stdin to EOF and then
//!   SIGKILLs its own group. Its stdin is a pipe whose write end the guard holds; the kernel closes that end when
//!   the test process dies by any means, so the watcher kills the group, itself last. A leg inherits the group
//!   (nothing in `rust/log/src` sets another), so it ends with the test process, though not with its supervisor;
//!   the producer has its own session and dies with its leg through `PR_SET_PDEATHSIG`. `Drop` closes the same
//!   end and reaps the watcher. The group id cannot be reused before the kill: the watcher is reaped only after
//!   it. No binary that includes this file builds on macOS, so nothing is claimed for it there.
//! - Windows: the supervise child is put in a kill-on-close job right after it starts, and its legs, which
//!   inherit the job, end when the test process's last handle to it closes.
//! "Ends" means signalled: `Drop` returns once the kill is sent, so a leg may still hold its writer lock for a few
//! milliseconds; a test that restarts on the same state folder waits on `writer.lock`. A leg adopted by a second
//! guard's supervisor stays in the FIRST guard's group (or job), so it ends when the first guard drops.
//! Included by each test file with `#[path = "support/capsule_guard.rs"] mod capsule_guard;`.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub struct CapsuleGuard {
    child: Option<Child>,
    /// Unix: the process-group leader that SIGKILLs the capsule's group once its lifeline closes; see the module doc.
    #[cfg(unix)]
    watcher: Option<watcher::Watcher>,
    /// Windows: the job the supervisor and everything it starts run in; the
    /// kernel ends them all when this handle closes, after the explicit kill
    /// and wait in `drop`. `Some` in every guard `spawn` returns.
    #[cfg(windows)]
    _job: Option<job::KillOnClose>,
}

#[cfg(unix)]
mod watcher {
    use std::io::PipeWriter;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    /// The `sh` that kills its own process group once `lifeline`'s write end is closed.
    pub struct Watcher {
        pub child: Child,
        /// Held until the guard drops or the test process dies; closing it is the kill order.
        pub lifeline: PipeWriter,
    }

    impl Watcher {
        pub fn start() -> Watcher {
            let (read, lifeline) = std::io::pipe().expect("make the watcher's lifeline pipe");
            let child = Command::new("/bin/sh")
                .args(["-c", "read _; kill -9 0"])
                .process_group(0)
                .stdin(read)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start the capsule watcher");
            Watcher { child, lifeline }
        }
    }
}

/// A job object that kills everything in it when its last handle closes.
#[cfg(windows)]
mod job {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct KillOnClose(OwnedHandle);

    impl KillOnClose {
        /// A new job holding `child`, or the OS error that refused it.
        pub fn holding(child: &std::process::Child) -> std::io::Result<KillOnClose> {
            // SAFETY: plain Win32 calls on handles this function creates or borrows.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                let job = KillOnClose(OwnedHandle::from_raw_handle(handle as RawHandle));
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let set = SetInformationJobObject(
                    job.0.as_raw_handle() as HANDLE,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if set == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if AssignProcessToJobObject(job.0.as_raw_handle() as HANDLE, child.as_raw_handle() as HANDLE) == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(job)
            }
        }
    }
}

impl CapsuleGuard {
    /// Starts `command` (a `sot-capsule` invocation) under the guard. Panics when the capsule cannot be bound to
    /// the test process's life; the unwind drops the guard, which ends the supervisor and, through the group or the
    /// job, any leg already started, so no guard whose capsule could outlive its test is ever returned. Two windows,
    /// both before any leg exists, are not covered: on Windows between the process start and its assignment to the
    /// job, on Unix when the watcher is found dead (only the supervisor is then ended).
    pub fn spawn(command: &mut Command) -> Self {
        let mut guard = Self {
            child: None,
            #[cfg(unix)]
            watcher: None,
            #[cfg(windows)]
            _job: None,
        };
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let watcher = watcher::Watcher::start();
            command.process_group(watcher.child.id() as i32);
            guard.watcher = Some(watcher);
        }
        let started = command.spawn();
        guard.child = Some(started.unwrap_or_else(|e| panic!("CapsuleGuard could not start {:?}: {e}", command.get_program())));
        #[cfg(unix)]
        {
            // A watcher that is already gone leaves the group unwatched without a sign.
            let alive = matches!(guard.watcher.as_mut().map(|w| w.child.try_wait()), Some(Ok(None)));
            assert!(alive, "CapsuleGuard: the watcher of the capsule's process group ended before the capsule started");
        }
        #[cfg(windows)]
        {
            match job::KillOnClose::holding(guard.child.as_ref().expect("capsule child still held")) {
                Ok(job) => guard._job = Some(job),
                Err(e) => panic!("CapsuleGuard could not put the capsule in a kill-on-close job: {e}"),
            }
        }
        guard
    }

    #[allow(dead_code)]
    pub fn id(&self) -> u32 {
        self.child.as_ref().expect("capsule child still held").id()
    }

    #[allow(dead_code)]
    pub fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("capsule child still held")
    }
}

/// Waits for `child` to exit, bounded: a process stuck in uninterruptible sleep must not hang the binary. `None`
/// when it did not exit in time or could not be waited on.
fn reap_within_bound(child: &mut Child) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => return None,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

impl Drop for CapsuleGuard {
    fn drop(&mut self) {
        // Never panic here: a panic during unwinding aborts the process.
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = reap_within_bound(&mut c);
        }
        #[cfg(unix)]
        if let Some(watcher) = self.watcher.take() {
            use std::os::unix::process::ExitStatusExt;
            // The watcher kills the group, legs included, once the write end closes; reaping it after is what
            // keeps the group id from being reused before that kill.
            let watcher::Watcher { mut child, lifeline } = watcher;
            drop(lifeline);
            // It ends only through its own SIGKILL of the group; any other end means the group was not killed.
            match reap_within_bound(&mut child) {
                Some(status) if status.signal() == Some(9) => {}
                other => eprintln!("CapsuleGuard: the capsule's watcher ended with {other:?}, not by its own SIGKILL; its process group may have survived"),
            }
        }
    }
}
