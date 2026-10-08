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

    /// What the hook reports when the child is created or adopted: the leader's identity and, when the descendant is ready,
    /// its identity (both opened while they live).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    type Entered = (Watched, Option<Watched>);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    type Entered = ();

    /// The hook that runs right after the child is created (or adopted): it opens the identities the case watches, reports
    /// them and stops until the case releases it, so the shutdown's fire lands in that window.
    fn race_hook(
        adopted: bool,
        hook_ready: std::path::PathBuf,
        hook_entered: mpsc::Sender<Entered>,
        hook_released: Arc<Barrier>,
    ) -> Box<dyn FnMut(u32) + Send> {
        Box::new(move |pid: u32| {
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
        })
    }

    /// Start the child through `signal` on a thread of its own, blocking or async, and end it once the fire has answered.
    fn start_on_a_thread(
        signal: &'static Signal,
        asynchronous: bool,
        adopted: bool,
        program: std::path::PathBuf,
        ready: std::path::PathBuf,
        after_fire: mpsc::Receiver<()>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
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
        })
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
        let hook = race_hook(adopted, ready.clone(), entered, released.clone());
        if adopted {
            *signal.after_adopt.lock().unwrap() = Some(hook);
        } else {
            *signal.after_create.lock().unwrap() = Some(hook);
        }
        let (cleanup, after_fire) = mpsc::channel();
        let worker = start_on_a_thread(signal, asynchronous, adopted, program, ready, after_fire);
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
#[path = "start_tests_windows.rs"]
pub(crate) mod windows;

#[cfg(target_os = "macos")]
#[path = "start_tests_macos.rs"]
mod macos;
