//! Start/fire barriers at creation and at adopted-tree registration.

#[cfg(unix)]
pub(crate) mod unix {
    use crate::lifecycle::child_signal::Signal;
    use std::sync::{mpsc, Arc, Barrier};
    use std::time::Duration;

    #[cfg(target_os = "linux")]
    pub(crate) struct Watched(std::os::fd::OwnedFd);

    #[cfg(target_os = "linux")]
    impl Watched {
        pub(crate) fn open(pid: u32) -> Self {
            use std::os::fd::FromRawFd;
            // SAFETY: open a retained identity of the still-ready fixture; the returned descriptor is owned here.
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
            assert!(fd >= 0, "cannot retain the ready fixture identity: {}", std::io::Error::last_os_error());
            Self(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
        }
        pub(crate) fn dead(&self) -> bool {
            use std::os::fd::AsRawFd;
            let mut fd = libc::pollfd { fd: self.0.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            // SAFETY: poll this object's retained pidfd, which cannot observe a reused PID.
            unsafe { libc::poll(&mut fd, 1, 3000) > 0 && fd.revents & libc::POLLIN != 0 }
        }

        fn cleanup(&self) {
            use std::os::fd::AsRawFd;
            // SAFETY: signal only the retained process identity, never a reused number.
            assert_eq!(
                unsafe {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        self.0.as_raw_fd(),
                        libc::SIGKILL,
                        std::ptr::null::<libc::siginfo_t>(),
                        0,
                    )
                },
                0
            );
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) struct Watched(std::os::fd::OwnedFd);

    #[cfg(target_os = "macos")]
    impl Watched {
        pub(crate) fn open(pid: u32) -> Self {
            use std::os::fd::FromRawFd;
            // SAFETY: retain an exit notification while the owned fixture is still ready and alive.
            let fd = unsafe { libc::kqueue() };
            assert!(fd >= 0, "cannot create a fixture identity watcher");
            let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
            let event = libc::kevent {
                ident: pid as _,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ONESHOT,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            assert_eq!(unsafe { libc::kevent(fd, &event, 1, std::ptr::null_mut(), 0, std::ptr::null()) }, 0);
            Self(owned)
        }

        pub(crate) fn dead(&self) -> bool {
            use std::os::fd::AsRawFd;
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            let bound = libc::timespec { tv_sec: 3, tv_nsec: 0 };
            // SAFETY: receive the retained event, never probing or signaling a reusable PID.
            unsafe { libc::kevent(self.0.as_raw_fd(), std::ptr::null(), 0, &mut event, 1, &bound) == 1 }
        }
    }

    fn start_race(asynchronous: bool, adopted: bool) {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let program = dir.path().join("tree");
        sot_log::test_exec::write_executable(
            &program,
            "#!/bin/sh\n/bin/sh -c 'echo $$ > \"$1\"; exec sleep 600' sh \"$1\" &\nwait\n",
        );
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let (entered, arrived) = mpsc::channel();
        let released = Arc::new(Barrier::new(2));
        let (hook_entered, hook_released) = (entered, released.clone());
        let hook_ready = ready.clone();
        let hook = Box::new(move |pid: u32| {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let leader = Watched::open(pid);
            if adopted {
                let began = std::time::Instant::now();
                while std::fs::read_to_string(&hook_ready).map_or(true, |text| text.trim().is_empty()) {
                    assert!(began.elapsed() < Duration::from_secs(10), "the adopted descendant was not ready");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let descendant = if adopted {
                let descendant: u32 = std::fs::read_to_string(&hook_ready).unwrap().trim().parse().unwrap();
                // SAFETY: a read-only group query while both fixture identities are still alive.
                assert_eq!(
                    unsafe { libc::getpgid(descendant as i32) },
                    pid as i32,
                    "the ready descendant escaped containment"
                );
                Some(Watched::open(descendant))
            } else {
                None
            };
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            hook_entered.send((leader, descendant)).unwrap();
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            hook_entered.send(()).unwrap();
            hook_released.wait();
        });
        if adopted {
            *signal.after_adopt.lock().unwrap() = Some(hook);
        } else {
            *signal.after_create.lock().unwrap() = Some(hook);
        }
        let (cleanup, after_fire) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            if asynchronous {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                runtime.block_on(async {
                    let mut cmd = tokio::process::Command::new(if adopted {
                        program.as_os_str()
                    } else {
                        std::ffi::OsStr::new("sleep")
                    });
                    cmd.arg(if adopted { ready.as_os_str() } else { std::ffi::OsStr::new("600") }).kill_on_drop(true);
                    if let Ok(mut child) = signal.spawn(&mut cmd) {
                        after_fire.recv_timeout(Duration::from_secs(10)).unwrap();
                        child.kill().await.unwrap();
                    }
                });
            } else {
                let mut cmd = std::process::Command::new(if adopted {
                    program.as_os_str()
                } else {
                    std::ffi::OsStr::new("sleep")
                });
                cmd.arg(if adopted { ready.as_os_str() } else { std::ffi::OsStr::new("600") });
                if let Ok(mut child) = signal.spawn_std(&mut cmd) {
                    after_fire.recv_timeout(Duration::from_secs(10)).unwrap();
                    child.kill().unwrap();
                }
            }
        });
        let watched = arrived.recv_timeout(Duration::from_secs(10)).expect("the child was not created");
        let (sent, received) = mpsc::channel();
        let firing = std::thread::spawn(move || {
            fire_result(signal).unwrap();
            let requests = super::super::contain::REQUEST_EVENTS.with(|events| events.borrow().clone());
            sent.send(requests).unwrap();
            let _ = cleanup.send(());
        });
        let began = std::time::Instant::now();
        let bypassed = received.recv_timeout(Duration::from_millis(3200)).is_ok();
        std::thread::sleep(Duration::from_millis(3200).saturating_sub(began.elapsed()));
        released.wait();
        worker.join().unwrap();
        firing.join().unwrap();
        let requests = received.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let dead = watched.0.dead() && watched.1.as_ref().is_none_or(Watched::dead);
        assert!(!bypassed, "terminal callback ran before the stalled start registered");
        assert!(requests.contains(&"group") && requests.contains(&"leader"), "fire skipped the checked requests");
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(dead, "the fixture survived the later OS death observation");
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("600");
        assert!(signal.spawn_std(&mut cmd).is_err(), "a start after fire created a child");
    }

    #[test]
    fn ready_tree_before_registration_cannot_be_outwaited_std() {
        start_race(false, true);
    }

    #[test]
    fn ready_tree_before_registration_cannot_be_outwaited_async() {
        start_race(true, true);
    }

    // Test-only parent adapter: the old fire has no result, and the old production path remains intact.
    fn fire_result(signal: &Signal) -> std::io::Result<()> {
        signal.fire()
    }

    #[test]
    fn fire_checks_group_and_leader_request_failures() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("600");
        let mut child = signal.spawn_std(&mut cmd).unwrap();
        super::super::contain::REQUEST_FAILURE.with(|failure| failure.set(3));
        let result = fire_result(signal);
        super::super::contain::REQUEST_FAILURE.with(|failure| failure.set(0));
        child.kill().unwrap();
        let error = result.expect_err("fire ignored failed termination requests").to_string();
        assert!(error.contains("group request failure") && error.contains("leader request failure"), "{error}");
    }

    fn wait_error(reap: bool) {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("600");
        let mut child = signal.spawn_std(&mut cmd).unwrap();
        super::super::contain::REQUEST_FAILURE.with(|failure| failure.set(if reap { 0 } else { 3 }));
        super::super::contain::REAP_FAILURE.with(|failure| failure.set(reap));
        let result = child.wait_within(Duration::ZERO);
        super::super::contain::REQUEST_FAILURE.with(|failure| failure.set(0));
        super::super::contain::REAP_FAILURE.with(|failure| failure.set(false));
        child.kill().unwrap();
        let error = result.expect_err("timeout claimed successful cleanup after a request/reap error").to_string();
        assert!(error.contains(if reap { "direct-child reap failure" } else { "group request failure" }), "{error}");
    }

    #[test]
    fn wait_within_checks_request_failures() {
        wait_error(false);
    }

    #[test]
    fn wait_within_checks_reap_failures() {
        wait_error(true);
    }

    #[test]
    fn probe_and_cleanup_failures_preserve_both_reasons() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut command = std::process::Command::new("sleep");
        command.arg("600");
        let mut child = signal.spawn_std(&mut command).unwrap();
        super::super::contain::PROBE_FAILURE.with(|failure| failure.set(true));
        super::super::contain::REQUEST_FAILURE.with(|failure| failure.set(3));
        let result = child.wait_within(Duration::ZERO);
        super::super::contain::PROBE_FAILURE.with(|failure| failure.set(false));
        super::super::contain::REQUEST_FAILURE.with(|failure| failure.set(0));
        child.kill().unwrap();
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("exit probe failure")
                && error.contains("group request failure")
                && error.contains("leader request failure"),
            "probe or cleanup reason was discarded: {error}"
        );
    }

    #[test]
    fn a_post_create_unwind_cleans_the_partial_std_tree() {
        let dir = tempfile::tempdir().unwrap();
        let leader = dir.path().join("leader");
        let ready = dir.path().join("ready");
        let program = dir.path().join("unwind-tree");
        sot_log::test_exec::write_executable(
            &program,
            "#!/bin/sh\necho $$ > \"$1\"\n/bin/sh -c 'echo $$ > \"$1\"; exec sleep 600' sh \"$2\" &\nwait\n",
        );
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let hook_ready = ready.clone();
        *signal.after_create.lock().unwrap() = Some(Box::new(move |_| {
            let began = std::time::Instant::now();
            while std::fs::read_to_string(&hook_ready).map_or(true, |text| text.trim().is_empty()) {
                assert!(began.elapsed() < Duration::from_secs(10), "the partial descendant was not ready");
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("injected post-create unwind");
        }));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut command = std::process::Command::new(program);
            command.arg(&leader).arg(&ready);
            signal.spawn_std(&mut command)
        }));
        assert!(outcome.is_err(), "the hook did not unwind");
        let pid: libc::pid_t = std::fs::read_to_string(leader).unwrap().trim().parse().unwrap();
        // SAFETY: this is our unreaped direct child; WNOWAIT retains its group identity until cleanup finishes.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOWAIT | libc::WNOHANG)
        };
        let reaped = result != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD);
        if result == 0 {
            // SAFETY: waitid confirmed that this direct child has not been reaped; its group identity remains ours.
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, std::ptr::null_mut(), 0);
            }
        }
        fire_result(signal).unwrap();
        assert!(reaped, "the post-create unwind left an unregistered partial tree unreaped");
    }

    #[test]
    fn suspended_start_cannot_be_outwaited_std() {
        start_race(false, false);
    }

    #[test]
    fn suspended_start_cannot_be_outwaited_async() {
        start_race(true, false);
    }

    #[test]
    fn spawn_errors_release_the_start_lock() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing-program");
        assert!(signal.spawn_std(&mut std::process::Command::new(&missing)).is_err());
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            assert!(signal.spawn(&mut tokio::process::Command::new(&missing)).is_err());
        });
        fire_result(signal).unwrap();
        assert!(signal.spawn_std(&mut std::process::Command::new("sleep")).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_post_create_unwind_cleans_the_partial_async_tree() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let (sent, watched) = mpsc::channel();
        *signal.after_create.lock().unwrap() = Some(Box::new(move |pid| {
            sent.send(Watched::open(pid)).unwrap();
            panic!("injected post-create unwind");
        }));
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(async {
                let mut command = tokio::process::Command::new("sleep");
                command.arg("600");
                signal.spawn(&mut command)
            })
        }));
        let watched = watched.recv_timeout(Duration::from_secs(1)).unwrap();
        let dead = watched.dead();
        if !dead {
            watched.cleanup();
            assert!(watched.dead(), "the retained fixture cleanup failed");
        }
        fire_result(signal).unwrap();
        assert!(outcome.is_err(), "the hook did not unwind");
        assert!(dead, "the post-create unwind left a partial async child alive");
    }
}

#[cfg(windows)]
pub(crate) mod windows {
    use crate::lifecycle::child_signal::Signal;
    use std::sync::{mpsc, Arc, Barrier};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

    pub(crate) struct Watched(usize);

    impl Watched {
        pub(crate) fn open(pid: u32) -> Self {
            // SAFETY: the fixture is alive and owned; this retained handle is closed by Drop.
            let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
            assert!(!handle.is_null(), "cannot retain the fixture identity: {}", std::io::Error::last_os_error());
            Self(handle as usize)
        }
        pub(crate) fn dead(&self) -> bool {
            // SAFETY: wait on this object's retained handle, rather than a potentially reused PID.
            unsafe { WaitForSingleObject(self.0 as _, 3000) == WAIT_OBJECT_0 }
        }
    }

    impl Drop for Watched {
        fn drop(&mut self) {
            // SAFETY: close this object's own process handle.
            unsafe { CloseHandle(self.0 as _) };
        }
    }

    fn fire_result(signal: &Signal) -> std::io::Result<()> {
        signal.fire()
    }

    fn assert_suspended(pid: u32) {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
        };
        use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, SuspendThread, THREAD_SUSPEND_RESUME};
        // SAFETY: inspect the owned child's thread by temporarily incrementing and restoring its suspend count.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            assert_ne!(snapshot, INVALID_HANDLE_VALUE);
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            let mut found = Thread32First(snapshot, &mut entry);
            while found != 0 && entry.th32OwnerProcessID != pid {
                found = Thread32Next(snapshot, &mut entry);
            }
            CloseHandle(snapshot);
            assert_ne!(found, 0, "the suspended child has no thread");
            let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
            assert!(!thread.is_null());
            let before = SuspendThread(thread);
            let restored = ResumeThread(thread);
            CloseHandle(thread);
            assert_eq!(before, 1, "the created child was not suspended before adoption");
            assert_eq!(restored, 2, "the thread's suspension was not restored");
        }
    }

    fn start_race(asynchronous: bool, adopted: bool) {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let program = dir.path().join("tree.ps1");
        sot_log::test_exec::write_executable(&program,
            "param($ready)\n$p=Start-Process -PassThru powershell -ArgumentList '-NoProfile','-Command','Start-Sleep 600'\n[IO.File]::WriteAllText($ready,[string]$p.Id)\nStart-Sleep 600\n");
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let (entered, arrived) = mpsc::channel();
        let released = Arc::new(Barrier::new(2));
        let (hook_released, hook_ready) = (released.clone(), ready.clone());
        let hook = Box::new(move |pid: u32| {
            let leader = Watched::open(pid);
            let descendant = if adopted {
                let began = Instant::now();
                let pid = loop {
                    if let Ok(text) = std::fs::read_to_string(&hook_ready) {
                        if let Ok(pid) = text.trim().parse() {
                            break pid;
                        }
                    }
                    assert!(began.elapsed() < Duration::from_secs(30), "the adopted descendant never became ready");
                    std::thread::sleep(Duration::from_millis(10));
                };
                Some(Watched::open(pid))
            } else {
                assert_suspended(pid);
                assert!(!hook_ready.exists(), "the suspended child executed before adoption");
                None
            };
            entered.send((leader, descendant)).unwrap();
            hook_released.wait();
        });
        if adopted {
            *signal.after_adopt.lock().unwrap() = Some(hook);
        } else {
            *signal.after_create.lock().unwrap() = Some(hook);
        }
        let worker = std::thread::spawn(move || {
            let mut command = std::process::Command::new("powershell");
            command.args(["-NoProfile", "-File"]).arg(program).arg(ready);
            if asynchronous {
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                runtime.block_on(async {
                    if let Ok(mut child) = signal.spawn(&mut command.into()) {
                        child.kill().await.unwrap();
                    }
                });
            } else if let Ok(mut child) = signal.spawn_std(&mut command) {
                child.kill().unwrap();
            }
        });
        let (leader, descendant) = arrived.recv_timeout(Duration::from_secs(40)).expect("the start hook never ran");
        let (sent, received) = mpsc::channel();
        let firing = std::thread::spawn(move || {
            fire_result(signal).unwrap();
            sent.send(()).unwrap();
        });
        let began = Instant::now();
        let bypassed = received.recv_timeout(Duration::from_millis(3200)).is_ok();
        std::thread::sleep(Duration::from_millis(3200).saturating_sub(began.elapsed()));
        released.wait();
        worker.join().unwrap();
        firing.join().unwrap();
        let dead = leader.dead() && descendant.as_ref().is_none_or(Watched::dead);
        assert!(!bypassed, "terminal callback ran before the stalled start registered");
        assert!(dead, "the fixture survived the later OS death observation");
        assert!(signal.spawn_std(&mut std::process::Command::new("powershell")).is_err());
    }

    #[test]
    fn fire_checks_job_request_failures() {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut command = std::process::Command::new("powershell");
        command.args(["-NoProfile", "-Command", "Start-Sleep 600"]);
        let mut child = signal.spawn_std(&mut command).unwrap();
        super::super::contain::REQUEST_FAILURE.with(|failure| failure.set(4));
        let result = fire_result(signal);
        super::super::contain::REQUEST_FAILURE.with(|failure| failure.set(0));
        child.kill().unwrap();
        assert!(result.unwrap_err().to_string().contains("job request failure"));
    }

    #[test]
    fn suspended_start_cannot_be_outwaited_std() {
        start_race(false, false);
    }
    #[test]
    fn suspended_start_cannot_be_outwaited_async() {
        start_race(true, false);
    }
    #[test]
    fn ready_tree_before_registration_cannot_be_outwaited_std() {
        start_race(false, true);
    }
    #[test]
    fn ready_tree_before_registration_cannot_be_outwaited_async() {
        start_race(true, true);
    }

    fn partial_start_cleanup(asynchronous: bool, unwind: bool) {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let (sent, watched) = mpsc::channel();
        *signal.after_create.lock().unwrap() = Some(Box::new(move |pid| {
            assert_suspended(pid);
            sent.send(Watched::open(pid)).unwrap();
            if unwind {
                panic!("injected post-create unwind");
            }
        }));
        super::super::contain::ADOPT_FAILURE.with(|failure| failure.set(!unwind));
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut command = std::process::Command::new("powershell");
            command.args(["-NoProfile", "-Command", "Start-Sleep 600"]);
            if asynchronous {
                runtime.block_on(async { signal.spawn(&mut command.into()).map(|_| ()) })
            } else {
                signal.spawn_std(&mut command).map(|_| ())
            }
        }));
        super::super::contain::ADOPT_FAILURE.with(|failure| failure.set(false));
        let dead = watched.recv_timeout(Duration::from_secs(1)).unwrap().dead();
        fire_result(signal).unwrap();
        if unwind {
            assert!(outcome.is_err(), "the hook did not unwind");
        } else {
            assert!(outcome.unwrap().unwrap_err().to_string().contains("job assignment failure"));
        }
        assert!(dead, "the failed partial start left its suspended process alive");
    }

    #[test]
    fn adoption_failure_cleans_std_and_async_suspended_children() {
        partial_start_cleanup(false, false);
        partial_start_cleanup(true, false);
    }

    #[test]
    fn unwind_cleans_std_and_async_suspended_children() {
        partial_start_cleanup(false, true);
        partial_start_cleanup(true, true);
    }
}
