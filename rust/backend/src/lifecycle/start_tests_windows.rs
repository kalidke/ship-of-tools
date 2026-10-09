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
        self.dead_within(3000)
    }
    pub(crate) fn dead_within(&self, ms: u32) -> bool {
        // SAFETY: wait on this object's retained handle, rather than a potentially reused PID.
        unsafe { WaitForSingleObject(self.0 as _, ms) == WAIT_OBJECT_0 }
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

/// The role a re-run of this test binary plays for the daemon-death cases: wait for the case to put it in the case's own
/// job, then start a tree through a private signal as the daemon does, say who is in it and wait to be killed. A no-op in
/// an ordinary run.
#[test]
fn contained_tree_role() {
    let Some(dir) = std::env::var_os("SOT_L2_ROLE_DIR").map(std::path::PathBuf::from) else {
        return;
    };
    sot_log::test_isolated::enter("lifecycle::start_tests::windows::contained_tree_role");
    let began = Instant::now();
    while !dir.join("go").exists() {
        assert!(began.elapsed() < Duration::from_secs(60), "no go");
        std::thread::sleep(Duration::from_millis(50));
    }
    let signal: &'static Signal = Box::leak(Box::new(Signal::new()));
    let mut cmd = std::process::Command::new("powershell");
    cmd.args([
        "-NoProfile",
        "-Command",
        &format!(
            "$p = Start-Process -PassThru -NoNewWindow ping -ArgumentList '-n','600','127.0.0.1'; \
             Set-Content -Path '{}' -Value $p.Id; Start-Sleep 600",
            dir.join("grandchild.pid").display()
        ),
    ]);
    let child = signal.spawn_std(&mut cmd).expect("start the tree");
    std::fs::write(dir.join("child.pid"), child.id().to_string()).unwrap();
    std::thread::sleep(Duration::from_secs(600));
    drop(child);
}

/// A daemon role running, inside a job of the case's own: whatever the role leaves alive when it is killed ends when the
/// case drops this, so the case ends only what its own spawn put in its own job.
struct DaemonRole {
    dir: tempfile::TempDir,
    role: std::process::Child,
    entry: sot_log::test_isolated::Entry,
    _job: sot_log::capsule::producer::conpty::AnonymousJob,
}

impl DaemonRole {
    fn start(env: &[(&str, &std::ffi::OsStr)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (mut command, entry) = sot_log::test_isolated::test_command(
            "lifecycle::start_tests::windows::contained_tree_role",
        );
        command
            .env("SOT_L2_ROLE_DIR", dir.path())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        for (name, value) in env {
            command.env(name, value);
        }
        let role = command.spawn().expect("start the role");
        let job = sot_log::capsule::producer::conpty::AnonymousJob::create().unwrap();
        // SAFETY: both handles are live; the job is the case's and the process handle is the role's.
        assert!(
            unsafe {
                windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                    job.raw(),
                    std::os::windows::io::AsRawHandle::as_raw_handle(&role) as _,
                )
            } != 0,
            "put the role in the case's job: {}",
            std::io::Error::last_os_error()
        );
        std::fs::write(dir.path().join("go"), "").unwrap();
        Self {
            dir,
            role,
            entry,
            _job: job,
        }
    }

    /// A pid the role wrote, opened as a retained handle while it lives.
    fn watch(&self, name: &str) -> Watched {
        let began = Instant::now();
        loop {
            if let Some(pid) = std::fs::read_to_string(self.dir.path().join(name))
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return Watched::open(pid);
            }
            assert!(
                began.elapsed() < Duration::from_secs(60),
                "the role never wrote {name}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// End the role, the case's own spawn, and check that its body ran once.
    fn kill(mut self) -> DeadRole {
        self.role.kill().expect("end the role");
        self.role.wait().expect("reap the role");
        self.entry.assert_once(self.role.id());
        DeadRole {
            _dir: self.dir,
            _job: self._job,
        }
    }
}

/// What outlives the role's death: the case's job, which ends what is left when the case drops it.
struct DeadRole {
    _dir: tempfile::TempDir,
    _job: sot_log::capsule::producer::conpty::AnonymousJob,
}

/// The kernel closes a dead daemon's handles, and a contained tree's per-child job closes with them: the daemon (a re-run
/// of this binary that holds the tree through a private signal) is ended by the case, which spawned it, and the tree it
/// started ends with it though nothing in the daemon ran. The tree's processes are watched through handles opened while
/// they lived. Run with `SOT_L2_NO_JOB_ASSIGNMENT=1` in the environment, the role skips the job assignment and this case
/// is red: that is its reversal.
#[test]
fn windows_job_daemon_death() {
    let role = DaemonRole::start(&[]);
    let child = role.watch("child.pid");
    let grandchild = role.watch("grandchild.pid");
    assert!(!child.dead_within(0), "the tree's leader was not running");
    assert!(
        !grandchild.dead_within(0),
        "the tree's descendant was not running"
    );
    let _dead = role.kill();
    let child_ended = child.dead_within(10_000);
    let grandchild_ended = grandchild.dead_within(10_000);
    assert!(child_ended, "the killed daemon's tree leader outlived it");
    assert!(
        grandchild_ended,
        "the killed daemon's tree descendant outlived it"
    );
}

/// The fault control of the case above: with the job assignment skipped, the killed daemon's tree outlives it, so the
/// case above can fail. The case's own job ends the tree afterwards.
#[test]
fn windows_job_daemon_death_needs_the_job_assignment() {
    let role = DaemonRole::start(&[("SOT_L2_NO_JOB_ASSIGNMENT", "1".as_ref())]);
    let child = role.watch("child.pid");
    let grandchild = role.watch("grandchild.pid");
    let _dead = role.kill();
    assert!(
        !child.dead_within(3000) && !grandchild.dead_within(0),
        "the tree ended without its job: the daemon-death case cannot fail"
    );
}

/// The known Windows limit, observed: a child created suspended and not yet assigned to its job when the daemon dies is
/// not ended by anything (the role pauses in the assignment, `SOT_L2_PAUSE_ADOPT`). It never ran; the case's job ends it.
#[test]
fn windows_suspended_interval_is_the_known_limit() {
    let dir = tempfile::tempdir().unwrap();
    let paused = dir.path().join("paused.pid");
    let role = DaemonRole::start(&[("SOT_L2_PAUSE_ADOPT", paused.as_os_str())]);
    let suspended = {
        let began = Instant::now();
        loop {
            if let Some(pid) = std::fs::read_to_string(&paused)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                break Watched::open(pid);
            }
            assert!(began.elapsed() < Duration::from_secs(60), "no pause");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let _dead = role.kill();
    assert!(
        !suspended.dead_within(3000),
        "the suspended child ended with the daemon: the limit no longer holds, so retire it in ADR 0050"
    );
}
