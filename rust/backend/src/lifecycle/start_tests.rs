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
            assert!(
                fd >= 0,
                "cannot retain the ready fixture identity: {}",
                std::io::Error::last_os_error()
            );
            Self(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
        }
        pub(crate) fn dead(&self) -> bool {
            use std::os::fd::AsRawFd;
            let mut fd = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
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
            assert_eq!(
                unsafe { libc::kevent(fd, &event, 1, std::ptr::null_mut(), 0, std::ptr::null()) },
                0
            );
            Self(owned)
        }

        pub(crate) fn dead(&self) -> bool {
            use std::os::fd::AsRawFd;
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            let bound = libc::timespec {
                tv_sec: 3,
                tv_nsec: 0,
            };
            // SAFETY: receive the retained event, never probing or signaling a reusable PID.
            unsafe {
                libc::kevent(
                    self.0.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &mut event,
                    1,
                    &bound,
                ) == 1
            }
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
                while std::fs::read_to_string(&hook_ready)
                    .map_or(true, |text| text.trim().is_empty())
                {
                    assert!(
                        began.elapsed() < Duration::from_secs(10),
                        "the adopted descendant was not ready"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let descendant = if adopted {
                let descendant: u32 = std::fs::read_to_string(&hook_ready)
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
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
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async {
                    let mut cmd = tokio::process::Command::new(if adopted {
                        program.as_os_str()
                    } else {
                        std::ffi::OsStr::new("sleep")
                    });
                    cmd.arg(if adopted {
                        ready.as_os_str()
                    } else {
                        std::ffi::OsStr::new("600")
                    })
                    .kill_on_drop(true);
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
                cmd.arg(if adopted {
                    ready.as_os_str()
                } else {
                    std::ffi::OsStr::new("600")
                });
                if let Ok(mut child) = signal.spawn_std(&mut cmd) {
                    after_fire.recv_timeout(Duration::from_secs(10)).unwrap();
                    child.kill().unwrap();
                }
            }
        });
        let watched = arrived
            .recv_timeout(Duration::from_secs(10))
            .expect("the child was not created");
        let (sent, received) = mpsc::channel();
        let firing = std::thread::spawn(move || {
            fire_result(signal).unwrap();
            let requests =
                super::super::contain::REQUEST_EVENTS.with(|events| events.borrow().clone());
            sent.send(requests).unwrap();
            let _ = cleanup.send(());
        });
        let began = std::time::Instant::now();
        let bypassed = received.recv_timeout(Duration::from_millis(3200)).is_ok();
        std::thread::sleep(Duration::from_millis(3200).saturating_sub(began.elapsed()));
        released.wait();
        worker.join().unwrap();
        firing.join().unwrap();
        let requests = received
            .recv_timeout(Duration::from_secs(1))
            .unwrap_or_default();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let dead = watched.0.dead() && watched.1.as_ref().is_none_or(Watched::dead);
        assert!(
            !bypassed,
            "terminal callback ran before the stalled start registered"
        );
        assert!(
            requests.contains(&"group") && requests.contains(&"leader"),
            "fire skipped the checked requests"
        );
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(dead, "the fixture survived the later OS death observation");
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("600");
        assert!(
            signal.spawn_std(&mut cmd).is_err(),
            "a start after fire created a child"
        );
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
        let error = result
            .expect_err("fire ignored failed termination requests")
            .to_string();
        assert!(
            error.contains("group request failure") && error.contains("leader request failure"),
            "{error}"
        );
    }

    fn wait_error(reap: bool) {
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("600");
        let mut child = signal.spawn_std(&mut cmd).unwrap();
        super::super::contain::REQUEST_FAILURE
            .with(|failure| failure.set(if reap { 0 } else { 3 }));
        super::super::contain::REAP_FAILURE.with(|failure| failure.set(reap));
        let result = child.wait_within(Duration::ZERO);
        super::super::contain::REQUEST_FAILURE.with(|failure| failure.set(0));
        super::super::contain::REAP_FAILURE.with(|failure| failure.set(false));
        child.kill().unwrap();
        let error = result
            .expect_err("timeout claimed successful cleanup after a request/reap error")
            .to_string();
        assert!(
            error.contains(if reap {
                "direct-child reap failure"
            } else {
                "group request failure"
            }),
            "{error}"
        );
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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    struct UnwindFixture {
        dir: tempfile::TempDir,
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    impl UnwindFixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            sot_log::test_exec::write_executable(
                &dir.path().join("unwind-tree"),
                "#!/bin/sh\n/bin/sh -c 'echo $$ > \"$1\"; while [ ! -e \"$2\" ]; do /bin/sleep 0.02; done' sh \"$1\" \"$2\" &\nwait\n",
            );
            Self { dir }
        }

        fn command(&self) -> std::process::Command {
            let mut command = std::process::Command::new(self.dir.path().join("unwind-tree"));
            command
                .arg(self.dir.path().join("ready"))
                .arg(self.dir.path().join("cleanup"));
            command
        }

        fn descendant(pid: u32, ready: &std::path::Path) -> Watched {
            let began = std::time::Instant::now();
            let descendant = loop {
                if let Ok(text) = std::fs::read_to_string(ready) {
                    if let Ok(pid) = text.trim().parse::<u32>() {
                        break pid;
                    }
                }
                assert!(
                    began.elapsed() < Duration::from_secs(10),
                    "the partial descendant was not ready"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            // SAFETY: query the ready fixture while its unreaped direct leader still owns the group identity.
            assert_eq!(
                unsafe { libc::getpgid(descendant as i32) },
                pid as i32,
                "partial descendant escaped its group"
            );
            Watched::open(descendant)
        }

        fn cleanup(&self) {
            std::fs::write(self.dir.path().join("cleanup"), b"done").unwrap();
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    impl Drop for UnwindFixture {
        fn drop(&mut self) {
            self.cleanup();
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_post_create_unwind_cleans_the_partial_std_tree() {
        let fixture = UnwindFixture::new();
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let ready = fixture.dir.path().join("ready");
        let (sent, watched) = mpsc::channel();
        *signal.after_create.lock().unwrap() = Some(Box::new(move |pid| {
            let descendant = UnwindFixture::descendant(pid, &ready);
            sent.send((pid, descendant)).unwrap();
            panic!("injected post-create unwind");
        }));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            signal.spawn_std(&mut fixture.command())
        }));
        let (pid, descendant) = watched.recv_timeout(Duration::from_secs(1)).unwrap();
        let descendant_dead = descendant.dead();
        // SAFETY: WNOWAIT retains this direct child's group identity if production cleanup did not reap it.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        let reaped =
            result != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD);
        if result == 0 {
            // SAFETY: waitid confirmed our direct child is unreaped, so its group identity is still retained.
            unsafe {
                libc::killpg(pid as i32, libc::SIGKILL);
                libc::kill(pid as i32, libc::SIGKILL);
                libc::waitpid(pid as i32, std::ptr::null_mut(), 0);
            }
        }
        fixture.cleanup();
        if !descendant_dead {
            assert!(
                descendant.dead(),
                "the retained descendant fixture cleanup failed"
            );
        }
        fire_result(signal).unwrap();
        assert!(outcome.is_err(), "the hook did not unwind");
        assert!(
            reaped,
            "the post-create unwind left an unregistered partial tree unreaped"
        );
        assert!(
            descendant_dead,
            "the post-create unwind left a partial std descendant alive"
        );
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
        assert!(signal
            .spawn_std(&mut std::process::Command::new(&missing))
            .is_err());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            assert!(signal
                .spawn(&mut tokio::process::Command::new(&missing))
                .is_err());
        });
        fire_result(signal).unwrap();
        assert!(signal
            .spawn_std(&mut std::process::Command::new("sleep"))
            .is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_post_create_unwind_cleans_the_partial_async_tree() {
        let fixture = UnwindFixture::new();
        let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
        let ready = fixture.dir.path().join("ready");
        let (sent, watched) = mpsc::channel();
        *signal.after_create.lock().unwrap() = Some(Box::new(move |pid| {
            let descendant = UnwindFixture::descendant(pid, &ready);
            sent.send((Watched::open(pid), descendant)).unwrap();
            panic!("injected post-create unwind");
        }));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(async {
                let mut command = tokio::process::Command::from(fixture.command());
                signal.spawn(&mut command)
            })
        }));
        let (leader, descendant) = watched.recv_timeout(Duration::from_secs(1)).unwrap();
        let leader_dead = leader.dead();
        let descendant_dead = descendant.dead();
        if !leader_dead {
            leader.cleanup();
            assert!(leader.dead(), "the retained leader fixture cleanup failed");
        }
        fixture.cleanup();
        if !descendant_dead {
            assert!(
                descendant.dead(),
                "the retained descendant fixture cleanup failed"
            );
        }
        fire_result(signal).unwrap();
        assert!(outcome.is_err(), "the hook did not unwind");
        assert!(
            leader_dead,
            "the post-create unwind left a partial async child alive"
        );
        assert!(
            descendant_dead,
            "the post-create unwind left a partial async descendant alive"
        );
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
            assert!(
                !handle.is_null(),
                "cannot retain the fixture identity: {}",
                std::io::Error::last_os_error()
            );
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
        use windows_sys::Win32::System::Threading::{
            OpenThread, ResumeThread, SuspendThread, THREAD_SUSPEND_RESUME,
        };
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
            assert_eq!(
                before, 1,
                "the created child was not suspended before adoption"
            );
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
                    assert!(
                        began.elapsed() < Duration::from_secs(30),
                        "the adopted descendant never became ready"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                };
                Some(Watched::open(pid))
            } else {
                assert_suspended(pid);
                assert!(
                    !hook_ready.exists(),
                    "the suspended child executed before adoption"
                );
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
        let (cleanup, after_requests) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut command = std::process::Command::new("powershell");
            command
                .args(["-NoProfile", "-File"])
                .arg(program)
                .arg(ready);
            if asynchronous {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async {
                    if let Ok(mut child) = signal.spawn(&mut command.into()) {
                        after_requests
                            .recv_timeout(Duration::from_secs(10))
                            .unwrap();
                        child.kill().await.unwrap();
                    }
                });
            } else if let Ok(mut child) = signal.spawn_std(&mut command) {
                after_requests
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
                child.kill().unwrap();
            }
        });
        let (leader, descendant) = arrived
            .recv_timeout(Duration::from_secs(40))
            .expect("the start hook never ran");
        let (sent, received) = mpsc::channel();
        let firing = std::thread::spawn(move || {
            super::super::contain::REQUEST_EVENTS.with(|events| events.borrow_mut().clear());
            fire_result(signal).unwrap();
            let requests =
                super::super::contain::REQUEST_EVENTS.with(|events| events.borrow().clone());
            sent.send(requests).unwrap();
            let _ = cleanup.send(());
        });
        let began = Instant::now();
        let bypassed = received.recv_timeout(Duration::from_millis(3200)).is_ok();
        std::thread::sleep(Duration::from_millis(3200).saturating_sub(began.elapsed()));
        released.wait();
        worker.join().unwrap();
        firing.join().unwrap();
        let requests = received
            .recv_timeout(Duration::from_secs(1))
            .unwrap_or_default();
        let dead = leader.dead() && descendant.as_ref().is_none_or(Watched::dead);
        assert!(
            !bypassed,
            "terminal callback ran before the stalled start registered"
        );
        assert_eq!(requests, ["job"], "fire skipped the checked job request");
        assert!(dead, "the fixture survived the later OS death observation");
        assert!(signal
            .spawn_std(&mut std::process::Command::new("powershell"))
            .is_err());
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
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("job request failure"));
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
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
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
            assert!(outcome
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("job assignment failure"));
        }
        assert!(
            dead,
            "the failed partial start left its suspended process alive"
        );
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

#[cfg(target_os = "macos")]
mod macos {
    use super::unix::Watched;
    use crate::lifecycle::child_signal::{ContainedStd, Signal};
    use crate::lifecycle::contain;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    /// Wait until the fixture process has written its readiness file.
    fn wait_file(path: &std::path::Path) {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while std::fs::read_to_string(path).map_or(true, |s| s.is_empty()) {
            assert!(
                std::time::Instant::now() < deadline,
                "fixture readiness did not arrive: {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn zombie_command() -> (tempfile::TempDir, Command) {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("finished");
        sot_log::test_exec::write_executable(&program, "#!/bin/sh\nexit 17\n");
        (dir, Command::new(program))
    }
    fn exited_eperm(pid: u32) {
        assert!(
            contain::exited_pid(pid, true).unwrap(),
            "leader must remain exited-unreaped"
        );
        // SAFETY: this test still exclusively owns the unreaped leader and its group.
        let result = unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
        let error = std::io::Error::last_os_error();
        assert_eq!(result, -1, "fixture must demonstrate real group EPERM");
        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
        eprintln!("real group request: EPERM (errno=1); retained leader exited-unreaped");
    }
    fn clear_events() {
        contain::REQUEST_EVENTS.with(|events| events.borrow_mut().clear());
        contain::macos::EVENTS.with(|events| events.borrow_mut().clear());
    }
    fn accepted_observations() {
        let events = contain::macos::EVENTS.with(|events| events.borrow().clone());
        eprintln!("no-live observations: {events:?}");
        assert_eq!(
            events,
            [
                "exited-unreaped",
                "members-any-uid",
                "status",
                "members-any-uid",
                "status",
                "exited-unreaped"
            ],
            "zombie-only acceptance bypassed complete membership/status observation"
        );
        assert_eq!(
            contain::REQUEST_EVENTS.with(|events| events.borrow().clone()),
            ["group", "leader"]
        );
    }
    #[test]
    fn zombie_only_group_returns_the_std_status() {
        let (_dir, mut command) = zombie_command();
        let mut child = Box::leak(Box::new(Signal::new()))
            .spawn_std(&mut command)
            .unwrap();
        exited_eperm(child.id());
        clear_events();
        let result = child.wait_within(Duration::from_secs(3));
        let status = result
            .expect("zombie-only std group must return original status")
            .unwrap();
        assert!(
            child.confirmed_reaped() && status.code() == Some(17),
            "original std status and confirmed reap"
        );
        accepted_observations();
    }
    #[tokio::test(flavor = "current_thread")]
    async fn zombie_only_group_returns_the_async_status() {
        let (_dir, command) = zombie_command();
        let signal = Box::leak(Box::new(Signal::new()));
        let mut child = signal.spawn(&mut command.into()).unwrap();
        exited_eperm(signal.held_groups()[0] as u32);
        clear_events();
        let status = child
            .wait()
            .await
            .expect("zombie-only async group must return original status");
        assert_eq!(status.code(), Some(17));
        assert_eq!(child.wait().await.unwrap().code(), Some(17));
        accepted_observations();
    }
    struct LiveTree {
        dir: tempfile::TempDir,
        child: Option<ContainedStd>,
        descendant: Option<Watched>,
    }
    impl LiveTree {
        fn new(root: bool) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (leader, member) = (dir.path().join("leader"), dir.path().join("member"));
            // The member keeps inherited stdin, exits on EOF, and has an independent 30 s ceiling.
            sot_log::test_exec::write_executable(&member, "#!/usr/bin/env python3\nimport os,sys,time,select\nos.setpgid(0,int(sys.argv[1]))\nwith open(sys.argv[2],'w') as f: f.write('%d %d %d'%(os.getpid(),os.getpgrp(),os.geteuid()))\nend=time.monotonic()+30\nwhile time.monotonic()<end:\n if select.select([0],[],[],max(0,end-time.monotonic()))[0]:\n  data=os.read(0,4096)\n  if not data: break\n  with open(sys.argv[2]+'.live','w') as f: f.write('alive')\n");
            sot_log::test_exec::write_executable(&leader, "#!/usr/bin/env python3\nimport os,sys,time,subprocess\nmember,ready,release,cleanup,root=sys.argv[1:]\nargs=[sys.executable,member,str(os.getpid()),ready]\np=subprocess.Popen((['/usr/bin/sudo','-n','--'] if root=='root' else [])+args)\nend=time.monotonic()+20\nwhile not os.path.exists(release) and time.monotonic()<end:\n if p.poll() is not None: raise RuntimeError('owned descendant exited before readiness/release; sudo -n is required')\n time.sleep(.01)\nif os.path.exists(cleanup): p.wait(timeout=35)\nsys.exit(17)\n");
            let signal = Box::leak(Box::new(Signal::new()));
            let mut command = Command::new(leader);
            let paths = ["ready", "release", "cleanup"].map(|name| dir.path().join(name));
            command
                .arg(member)
                .args(paths)
                .arg(if root { "root" } else { "same" })
                .stdin(Stdio::piped());
            let child = signal.spawn_std(&mut command).unwrap();
            let mut tree = Self {
                dir,
                child: Some(child),
                descendant: None,
            };
            wait_file(&tree.dir.path().join("ready"));
            let text = std::fs::read_to_string(tree.dir.path().join("ready")).unwrap();
            let ids: Vec<u32> = text
                .split_whitespace()
                .map(|s| s.parse().unwrap())
                .collect();
            assert_eq!(ids.len(), 3);
            tree.descendant = Some(Watched::open(ids[0]));
            assert_eq!(
                ids[1],
                tree.child.as_ref().unwrap().id(),
                "STOP: sudo changed the retained group"
            );
            assert_eq!(
                unsafe { libc::getpgid(ids[0] as i32) },
                ids[1] as i32,
                "observed live member group"
            );
            assert_eq!(ids[2], if root { 0 } else { unsafe { libc::geteuid() } });
            assert!(
                !root || unsafe { libc::geteuid() } != 0,
                "genuine denial requires different credentials"
            );
            eprintln!("owned live member pgid matches retained leader; cross-uid={root}; stdin-close lifetime armed");
            std::fs::write(tree.dir.path().join("release"), b"exit").unwrap();
            assert!(contain::exited_pid(tree.child.as_ref().unwrap().id(), true).unwrap());
            tree
        }
    }
    impl Drop for LiveTree {
        fn drop(&mut self) {
            let mut child = self.child.take().unwrap();
            drop(child.stdin.take());
            std::fs::write(self.dir.path().join("cleanup"), b"close").unwrap();
            std::fs::write(self.dir.path().join("release"), b"exit").unwrap();
            if let Some(watched) = self.descendant.take() {
                let began = std::time::Instant::now();
                while !watched.dead() {
                    if began.elapsed() > Duration::from_secs(35) {
                        std::mem::forget(child); // Never let product Drop signal the root fixture on a failed EOF cleanup.
                        panic!("owned member did not exit within the stdin-close deadline");
                    }
                }
                eprintln!("owned descendant exit observed after stdin close");
            }
            let began = std::time::Instant::now();
            while let Err(error) = child.wait_within(Duration::from_secs(40)) {
                eprintln!("owned fixture cleanup retry before reap: {error}");
                if began.elapsed() > Duration::from_secs(5) {
                    std::mem::forget(child);
                    panic!("owned fixture cleanup did not reap before its deadline");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    #[test]
    fn an_exited_leaders_live_descendant_still_receives_the_group_signal() {
        let mut tree = LiveTree::new(false);
        let pid = tree.child.as_ref().unwrap().id();
        assert!(
            contain::macos::checked_no_live_group(pid as i32).is_err(),
            "live member accepted as absence"
        );
        clear_events();
        assert_eq!(
            tree.child.as_mut().unwrap().wait().unwrap().code(),
            Some(17)
        );
        assert_eq!(
            contain::REQUEST_EVENTS.with(|events| events.borrow().clone()),
            ["group", "leader"]
        );
        assert!(
            tree.descendant.take().unwrap().dead(),
            "real group request did not end the owned descendant"
        );
    }
    #[test]
    fn a_real_live_group_permission_denial_stays_an_error() {
        use std::io::Write;
        let mut tree = LiveTree::new(true);
        exited_eperm(tree.child.as_ref().unwrap().id());
        let child = tree.child.as_mut().unwrap();
        child.stdin.as_mut().unwrap().write_all(b"probe").unwrap();
        wait_file(&tree.dir.path().join("ready.live"));
        eprintln!("owned root member still live after genuine group EPERM");
        clear_events();
        let result = child.wait();
        assert_eq!(
            contain::REQUEST_EVENTS.with(|events| events.borrow().clone()),
            ["group", "leader"]
        );
        let error = result.expect_err("genuine live-member EPERM was accepted as absence");
        assert_eq!(error.raw_os_error(), Some(libc::EPERM));
    }
    #[test]
    fn a_failed_group_observation_keeps_the_original_eperm() {
        for fault in 1..=12 {
            let (_dir, mut command) = zombie_command();
            let mut child = Box::leak(Box::new(Signal::new()))
                .spawn_std(&mut command)
                .unwrap();
            exited_eperm(child.id());
            clear_events();
            contain::macos::FAULT.with(|value| value.set(fault));
            let result = child.wait();
            contain::macos::FAULT.with(|value| value.set(0));
            child.wait().unwrap();
            let error =
                result.expect_err("failed or ambiguous zombie-group observation was accepted");
            assert_eq!(error.raw_os_error(), Some(libc::EPERM), "fault={fault}");
            let requests = contain::REQUEST_EVENTS.with(|events| events.borrow().clone());
            assert_eq!(requests, ["group", "leader", "group", "leader"]);
            eprintln!(
                "query fault={fault}: original EPERM preserved; independent leader request checked"
            );
        }
    }
    #[test]
    fn injected_group_and_leader_failures_stay_checked() {
        for failure in 1..=3 {
            let (_dir, mut command) = zombie_command();
            let mut child = Box::leak(Box::new(Signal::new()))
                .spawn_std(&mut command)
                .unwrap();
            exited_eperm(child.id());
            clear_events();
            contain::REQUEST_FAILURE.with(|value| value.set(failure));
            let result = child.wait();
            contain::REQUEST_FAILURE.with(|value| value.set(0));
            accepted_observations();
            child.wait().unwrap();
            let error = result
                .expect_err("recognized zombie group suppressed injected request failure")
                .to_string();
            assert!(
                failure & 1 == 0 || error.contains("group request failure"),
                "{error}"
            );
            assert!(
                failure & 2 == 0 || error.contains("leader request failure"),
                "{error}"
            );
        }
    }
}
