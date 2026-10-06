//! Headless proofs through App's actual returned-loop continuation, never resumed().
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use sot_log::test_isolated::{enter, run_isolated, test_command};

const CHILD_BODY: &str = "ui::app::exit_process_tests::owned_child_body";
const CHILD_ROOT: &str = "SOT_TEST_T1_CHILD_ROOT";

fn fixture_app(runtime: Option<tokio::runtime::Runtime>) -> App {
    let cli = crate::cli::Cli {
        dial: vec![], socket: None, token: None, capture: None, scale: 1.0,
        start_mode: None, start_selected: None, auto_expand: false,
        demo_function_methods: None, start_path: None, font_scale: None,
        demo_repl_eval: None, start_maximized: false, start_fullscreen: false,
        start_focus: "nav".to_string(), capture_preview: None, capture_delay_ms: 0,
        capture_cycle: 0, auto_pin: false, start_help: false, start_help_peek: false,
        start_monitor: false, start_scalebar: false, ephemeral: true, no_lease: true,
        demo_sessions: vec![], demo_session_states: vec![], demo_flash: vec![],
        contrast_mode: "bright".to_string(), relaunched: false,
    };
    let (tx, rx) = std::sync::mpsc::channel();
    App::new(rx, runtime, cli, tx, vec![], None, crate::lease::Leases::new(true, vec![]))
}

struct Folder(std::path::PathBuf);
impl Drop for Folder {
    fn drop(&mut self) { std::fs::remove_dir_all(&self.0).expect("remove owned absolute fixture folder"); }
}

struct Witness(Arc<AtomicBool>);
impl Drop for Witness {
    fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); }
}

// A parent-bound wait observes this original direct child, never an EOF or a process-name match.
#[cfg(unix)]
struct OwnedIdentity(u32);
#[cfg(unix)]
impl OwnedIdentity {
    fn new(pid: u32) -> Self { Self(pid) }
    fn exited(&self) -> bool {
        unsafe extern "C" { fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32; }
        let mut status = 0;
        // SAFETY: the recorded pid is our unreaped direct child. WNOHANG is 1 on supported Unix targets.
        let result = unsafe { waitpid(self.0 as i32, &mut status, 1) };
        if result == self.0 as i32 { return true; }
        if result == 0 { return false; }
        let error = std::io::Error::last_os_error();
        assert_eq!(error.raw_os_error(), Some(10), "owned wait failed: {error}"); // ECHILD: Tokio reaped it.
        true
    }
}

#[cfg(windows)]
struct OwnedIdentity(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl OwnedIdentity {
    fn new(pid: u32) -> Self {
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
        // SAFETY: the newly spawned child remains owned while its original process handle is pinned.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        assert!(!handle.is_null(), "pin recorded child process");
        Self(handle)
    }
    fn exited(&self) -> bool {
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        // SAFETY: the retained handle is valid until Drop; zero is a nonblocking wait.
        let result = unsafe { WaitForSingleObject(self.0, 0) };
        assert!(result == 0 || result == 258, "owned process wait failed: {result}");
        result == 0
    }
}
#[cfg(windows)]
impl Drop for OwnedIdentity {
    fn drop(&mut self) {
        // SAFETY: this fixture owns the one retained process handle.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0); }
    }
}

#[test]
fn owned_child_body() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else { return; };
    enter(CHILD_BODY);
    let root = std::path::PathBuf::from(root);
    assert!(root.is_absolute());
    std::fs::write(root.join("ready"), std::process::id().to_string()).unwrap();
    // A fixture failure cannot leave this child indefinitely alive.
    std::thread::sleep(Duration::from_secs(8));
}

fn child_fixture(runtime: &tokio::runtime::Runtime) -> (tokio::process::Child, OwnedIdentity, Folder) {
    let root = std::env::temp_dir().join(format!("sot-t1-child-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    assert!(root.is_absolute());
    std::fs::create_dir(&root).unwrap();
    let folder = Folder(root);
    let (mut command, entry) = test_command(CHILD_BODY);
    command.env(CHILD_ROOT, &folder.0).stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let child = runtime.block_on(async { command.spawn().expect("start recorded fixture child") });
    let pid = child.id().unwrap();
    let identity = OwnedIdentity::new(pid);
    let end = Instant::now() + Duration::from_secs(5);
    while !folder.0.join("ready").exists() && Instant::now() < end { std::thread::sleep(Duration::from_millis(5)); }
    assert_eq!(std::fs::read_to_string(folder.0.join("ready")).unwrap(), pid.to_string(), "original child readiness");
    entry.assert_once(pid);
    assert!(!identity.exited(), "workload child already exited");
    println!("T1 fixture observed: original child entered once and remained alive");
    (child, identity, folder)
}

fn runtime_case(error: bool, held_worker: bool) {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let (child, identity, _folder) = child_fixture(&runtime);
    let dropped = Arc::new(AtomicBool::new(false));
    let witness = Witness(dropped.clone());
    let (started, ready) = std::sync::mpsc::channel();
    runtime.spawn(async move {
        let (_child, _witness) = (child, witness);
        started.send(()).unwrap();
        loop { tokio::task::yield_now().await; }
    });
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    let (started, ready) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let (finished, done) = std::sync::mpsc::channel();
    let block = move || {
        started.send(()).unwrap();
        let _ = gate.recv_timeout(Duration::from_millis(2400));
        finished.send(()).unwrap();
    };
    if held_worker { runtime.spawn(async move { block(); }); }
    else { runtime.spawn_blocking(block); }
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    println!(
        "T1 fixture observed: yielding task and {} ready",
        if held_worker {
            "held worker"
        } else {
            "blocked pool"
        }
    );
    let mut app = fixture_app(Some(runtime));
    assert!(app.state.is_none(), "headless fixture cannot resume a window");
    let start = Instant::now();
    let result = app.run_with(|_| if error { Err(winit::error::EventLoopError::ExitFailure(23)) } else { Ok(()) });
    let taken = app.rt.is_none();
    if taken { app.shutdown_transport(); } // A repeated finalization takes nothing.
    drop(app);
    let elapsed = start.elapsed();
    let delayed = !dropped.load(Ordering::SeqCst);
    let still_alive = !identity.exited();
    let _ = release.send(());
    done.recv_timeout(Duration::from_secs(5)).unwrap();
    let end = Instant::now() + Duration::from_secs(2);
    while (!dropped.load(Ordering::SeqCst) || !identity.exited()) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(dropped.load(Ordering::SeqCst), "yielding task did not drop its child ownership");
    assert!(identity.exited(), "original owned child survived runtime cleanup");
    println!("T1 fixture observed: yielding ownership dropped and original child exited");
    assert_eq!(result.is_err(), error, "returned-loop result changed");
    if error { assert!(matches!(result, Err(winit::error::EventLoopError::ExitFailure(23))), "error identity changed"); }
    assert!(taken, "error return bypassed runtime finalization");
    assert!(elapsed < Duration::from_secs(2), "runtime finalization exceeded two seconds: {elapsed:?}");
    if held_worker {
        assert!(delayed && still_alive, "held worker must expose delayed task destruction before release");
        println!("runtime finalizer: held worker cleanup delayed until release");
    } else {
        assert!(!delayed && !still_alive, "yielding child cleanup was not observed before gate release");
        println!("runtime finalizer: yielding task ownership dropped and original child exited");
    }
    println!("runtime finalizer: {} returned within two seconds", if error { "Err" } else { "Ok" });
}

#[test]
fn blocked_pool_ok_cleans_yielding_child() {
    if run_isolated("ui::app::exit_process_tests::blocked_pool_ok_cleans_yielding_child") {
        println!(
            "T1 body entered: ui::app::exit_process_tests::blocked_pool_ok_cleans_yielding_child"
        );
    runtime_case(false, false);
        println!("T1 assertion passed: runtime finalization exceeded two seconds:");
     }
}
#[test]
fn blocked_pool_error_cleans_yielding_child() {
    if run_isolated("ui::app::exit_process_tests::blocked_pool_error_cleans_yielding_child") { runtime_case(true, false); }
}
#[test]
fn held_worker_exposes_delayed_child_destruction() {
    if run_isolated("ui::app::exit_process_tests::held_worker_exposes_delayed_child_destruction") {
        println!("T1 body entered: ui::app::exit_process_tests::held_worker_exposes_delayed_child_destruction");
    runtime_case(false, true);
        println!("T1 assertion passed: runtime finalization exceeded two seconds:");
     }
}
#[test]
fn absent_runtime_and_repeated_finalization_preserve_results() {
    for error in [false, true] {
        let mut app = fixture_app(None);
        let result = app.run_with(|_| if error { Err(winit::error::EventLoopError::ExitFailure(23)) } else { Ok(()) });
        app.shutdown_transport();
        app.shutdown_transport();
        assert_eq!(result.is_err(), error);
        assert!(app.rt.is_none());
    }
}
