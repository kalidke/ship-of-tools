//! The Windows half of the start/fire tests: a suspended start cannot be outwaited, and job requests are checked.

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
        let requests = super::super::contain::REQUEST_EVENTS.with(|events| events.borrow().clone());
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
