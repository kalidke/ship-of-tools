#![cfg(any(windows, target_os = "linux"))]
//! Real cross-process integration tests for `sot_log::supervisor`: they spawn the real `sot-capsule` binary
//! (`supervise`) and talk to it as the daemon does, on the row socket: a dial that proves the peer and checks its row
//! number, then `row.status`, `row.end_run`, `row.reset` and `row.stop`. The journal's own crash-durability is proven
//! by `supervisor/journal/`'s unit tests; what these tests add is proof the wiring across a real process boundary is
//! right, on every platform the supervisor runs on.
//!
//! Deterministic by construction: every wait below is a bounded poll for an external, observable fact (a socket
//! answering, a process's own exit code), never a sleep-and-hope.

use serde_json::{json, Value};
use sot_log::SupervisorPhase;
use sot_log::row::{row_socket_path, RowClient};
use sot_log::supervisor::journal;
use sot_platform::host::state_dir::state_dir_hash;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[path = "../support/capsule_guard.rs"]
mod capsule_guard;
use capsule_guard::CapsuleGuard;

/// An interactive shell on its pty stays open until EndRun, on both
/// platforms — the process every "spawn and wait for Ready" test starts.
#[cfg(windows)]
const SHELL: &[&str] = &["cmd.exe"];
#[cfg(target_os = "linux")]
const SHELL: &[&str] = &["/bin/sh"];

/// Points `SOT_RUNTIME_DIR` at a fresh, mode-0700 tempdir under `/tmp`
/// for the lifetime of the returned guard, so this process's own socket
/// paths (and every child `sot-capsule` it spawns, which inherits this
/// env var like any other) stay short and isolated — mirrors
/// `tests/socket_unix/`/`tests/challenge_unix.rs`'s identical helper.
/// A no-op on Windows, which has no such env var or `sun_path` bound.
#[cfg(target_os = "linux")]
struct RuntimeDirGuard {
    _tmp: tempfile::TempDir,
}
#[cfg(target_os = "linux")]
fn isolated_runtime_dir() -> RuntimeDirGuard {
    let tmp = tempfile::Builder::new()
        .prefix("sot-t")
        .tempdir_in("/tmp")
        .expect("tempdir under /tmp");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::env::set_var("SOT_RUNTIME_DIR", tmp.path());
    RuntimeDirGuard { _tmp: tmp }
}
#[cfg(windows)]
struct RuntimeDirGuard;
#[cfg(windows)]
fn isolated_runtime_dir() -> RuntimeDirGuard {
    RuntimeDirGuard
}

/// Real-process tests are SERIALIZED: each spawns a supervisor, a capsule
/// and a shell, and the CI runner (two cores) is the shared resource. Run
/// in parallel they starve each other's admission and readiness polls —
/// the crash-recovery and adopt-after-kill tests timed out on a loaded
/// windows-latest release runner and again on windows-2022 (2026-09-01),
/// while passing every quiet run. Same mechanism as the rig's `RIG_LOCK`.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn capsule_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sot-capsule"))
}

fn poll_until<T>(mut attempt: impl FnMut() -> Option<T>, timeout: Duration, what: &str) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = attempt() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A connection to a row's supervisor: the daemon's own dial, then raw ops. A call that fails is a failed test.
struct Lane(std::cell::RefCell<RowClient>);

impl Lane {
    /// The supervisor's `(pid, created)`, as its hello named them and the dial proved them.
    fn peer(&self) -> (u32, u64) {
        self.0.borrow().peer()
    }

    /// One op, its reply payload within `bound`.
    fn call(&self, op: &str, payload: Value, bound: Duration) -> Value {
        self.0.borrow_mut().call(op, payload, bound).unwrap_or_else(|e| panic!("{op}: {e}")).payload
    }
}

/// Bounded poll for the row socket to accept a connection AND answer the hello, the observable fact a real client waits
/// on, never a sleep guessing how long the bind takes.
fn wait_for_lane(h: &str, timeout: Duration) -> Lane {
    let path = row_socket_path(h).expect("the row socket's path fits");
    Lane(std::cell::RefCell::new(poll_until(
        || RowClient::connect(&path, "test", Duration::from_secs(5)).ok(),
        timeout,
        "the row socket to accept and answer the hello",
    )))
}

/// What `row.status` says.
struct Status {
    run: Option<String>,
    phase: SupervisorPhase,
}

fn status(conn: &Lane) -> Status {
    let reply = conn.call("row.status", json!({}), Duration::from_secs(5));
    let text = |key: &str| reply[key].as_str().map(str::to_string);
    let phase = match reply["phase"].as_str() {
        Some("starting") => SupervisorPhase::Starting,
        Some("ready") => SupervisorPhase::Ready,
        Some("ending") => SupervisorPhase::Ending,
        Some("ended_no_respawn") => SupervisorPhase::EndedNoRespawn,
        Some("terminal") => SupervisorPhase::Terminal,
        other => panic!("a status with no known phase: {other:?}"),
    };
    Status { run: text("run"), phase }
}

/// The row socket binds before the agent starts, so answering `status` does not by itself mean the agent is running:
/// poll for the phase itself, the observable fact. Returns the run.
fn wait_for_ready(conn: &Lane, timeout: Duration) -> String {
    poll_until(
        || match status(conn) {
            Status { run: Some(run), phase: SupervisorPhase::Ready } => Some(run),
            _ => None,
        },
        timeout,
        "the agent to reach phase ready",
    )
}

/// Asks for the end of `run` and returns the outcome. Generous bound: the answer comes when the run has ended.
fn end_run(conn: &Lane, run: &str, reason: &str) -> Value {
    conn.call("row.end_run", json!({"run": run, "reason": reason}), Duration::from_secs(40))
}

/// Asserts `end_run`'s own reply is `ended`: it comes once the run has ended, so the supervisor rests in
/// `ended_no_respawn` when the client reads it.
fn end_run_and_expect_ended(conn: &Lane, run: &str, reason: &str) {
    let reply = end_run(conn, run, reason);
    assert_eq!(reply["outcome"], "ended", "end_run's reply arrives when the run has ended: {reply}");
}

/// A reset from a resting supervisor, answered once the end record is removed.
fn reset(conn: &Lane) {
    let reply = conn.call("row.reset", json!({}), Duration::from_secs(40));
    assert_eq!(reply["outcome"], "reset", "{reply}");
}

/// `row.stop`: the supervisor ends its agent and exits once this connection has ended.
fn stop(conn: &Lane) {
    let reply = conn.call("row.stop", json!({}), Duration::from_secs(30));
    assert_eq!(reply["outcome"], "stopping", "{reply}");
}

fn spawn_supervisor(state_dir: &Path, mode: &str, argv: &[&str]) -> CapsuleGuard {
    spawn_supervisor_with_env(state_dir, mode, argv, &[])
}

/// [`spawn_supervisor`] with extra environment for the supervisor, such as a `SOT_TEST_` seam.
fn spawn_supervisor_with_env(state_dir: &Path, mode: &str, argv: &[&str], env: &[(&str, &str)]) -> CapsuleGuard {
    let mut cmd = Command::new(capsule_exe());
    cmd.arg("supervise")
        .arg(state_dir)
        .arg(mode)
        .arg("--assume-no-rollback-target")
        .arg("--")
        .args(argv)
        // `inherit()`, not `piped()`: nothing in this file ever reads
        // the child's stdout/stderr, so a piped handle just accumulates
        // in an OS buffer a chatty child could eventually fill and
        // block on -- and worse, silently swallows every
        // `eprintln!("sot-capsule supervise: ...")` diagnostic (the
        // supervisor's own respawn/flap-bound logging) that CI needs to
        // see when a test times out. Inheriting sends both straight to
        // the test binary's own stdout/stderr, which `cargo test`
        // already captures and only shows on a failing test.
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .envs(env.iter().copied());
    CapsuleGuard::spawn(&mut cmd)
}

fn wait_for_exit(child: &mut CapsuleGuard, timeout: Duration) -> std::process::ExitStatus {
    poll_until(|| child.child_mut().try_wait().unwrap(), timeout, "the supervisor process to exit")
}

/// Single-owner reaping (review round 2, F7): a count of THIS
/// SUPERVISOR's own zombie (state `Z`) direct children — never a uid-wide
/// count (review round 2's own finding: a shared CI runner, or even this
/// test binary's OWN other tests running concurrently, can have
/// unrelated zombies under the same uid at any moment, and a uid-wide
/// count also can't be checked while pinning it to "this one
/// supervisor's own leg" specifically). Reads `/proc/<pid>/task/<pid>/children`
/// — the MAIN THREAD's own child list, which for a single-threaded
/// process like this supervisor is every process it has ever fork()'d
/// and not yet reaped, Linux's own portable "list my children" mechanism
/// (no `ptrace`, no `/proc` tree walk) — then each listed child's own
/// `/proc/<c>/stat` field 3 (state), parsed the same way
/// `tests/e2e_socket/`'s own `proc_state` does (fields after the comm's
/// closing paren). Meant to be asserted WHILE the supervisor is still
/// alive (its own `/proc/<pid>` entry, and thus this file, only exists
/// then) — right after a run has ended, before anything stops the
/// authority itself.
#[cfg(target_os = "linux")]
fn zombie_children_of(pid: u32) -> usize {
    let Ok(contents) = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) else {
        return 0;
    };
    contents
        .split_whitespace()
        .filter(|child_pid| {
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{child_pid}/stat")) else {
                return false;
            };
            let Some(close) = stat.rfind(')') else { return false };
            stat[close + 1..].split_whitespace().next() == Some("Z")
        })
        .count()
}

/// The supervisor's own direct children right now, live or dead, via
/// `/proc/<pid>/task/<pid>/children` -- as [`zombie_children_of`], minus
/// its zombie-state filter. Used to find the leg's own pid (the
/// supervisor's ONE child at a time) rather than a name-matching scrape.
#[cfg(target_os = "linux")]
fn direct_children_of(pid: u32) -> Vec<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect()
}

mod lifecycle;
mod argv;
mod authority;
mod one_process;
mod spawn;


/// Names the test process that `a_killed_test_process_leaves_no_capsule_process` kills: set (to its folder) only in
/// the child it starts, whose body is `killed_child_body`.
const KILLED_CHILD: &str = "SOT_TEST_KILLED_CHILD";

/// Each probe takes its lock and drops it before returning: locks conflict between two opens in one process, so
/// a probe that kept its lock would make the next probe of that lock read a live holder.
fn fence_free(state_dir: &Path) -> bool {
    journal::fence::lock_supervisor(state_dir).is_ok()
}

fn free_within(free: impl Fn() -> bool, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    loop {
        if free() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A capsule is gone when the kernel lock it holds, the supervisor fence, is free: released on any death. No pid is
/// read.
fn capsule_gone_within(state_dir: &Path, bound: Duration) -> bool {
    free_within(|| fence_free(state_dir), bound)
}

/// A red's cleanup, run on every path after the kill: ends a supervisor that outlived its test, over its row socket. It never panics, so the assert that follows it still reports.
fn end_what_outlived(state_dir: &Path) {
    if state_dir.exists() && !fence_free(state_dir) {
        let path = row_socket_path(&state_dir_hash(state_dir)).ok();
        if let Some(mut client) = path.and_then(|p| RowClient::connect(&p, "test", Duration::from_secs(5)).ok()) {
            if let Ok(frame) = client.call("row.status", json!({}), Duration::from_secs(5)) {
                if let Some(run) = frame.payload["run"].as_str() {
                    let _ = client.call("row.end_run", json!({"run": run, "reason": "test cleanup"}), Duration::from_secs(40));
                }
            }
            let _ = client.call("row.stop", json!({}), Duration::from_secs(30));
        }
        free_within(|| fence_free(state_dir), Duration::from_secs(60));
    }
}

/// The body that runs in the child of `a_killed_test_process_leaves_no_capsule_process`: a supervisor and its leg on
/// the parent's state folder, an empty `ready` marker, then a wait on stdin that only the parent's kill
/// (or its death) ends. It does not make a runtime folder: it uses the parent's, which the parent removes.
fn killed_child_body(name: &str, dir: &Path) {
    sot_platform::test_isolated::enter(name);
    let state_dir = dir.join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let _capsule = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&state_dir_hash(&state_dir), Duration::from_secs(30));
    wait_for_ready(&conn, Duration::from_secs(90));
    let pending = dir.join("ready.tmp");
    std::fs::write(&pending, b"").unwrap();
    std::fs::rename(&pending, dir.join("ready")).unwrap();
    std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).unwrap();
}

/// The test process dying by any means, SIGKILL on Linux and TerminateProcess on Windows, takes its capsule with it:
/// the supervisor and the leg are gone within 5 s. ADR 0041's adoption is unchanged (a leg still outlives a killed
/// supervisor); the guard binds the capsule's life to the test process, not the supervisor's to the leg's.
#[test]
fn a_killed_test_process_leaves_no_capsule_process() {
    const NAME: &str = "a_killed_test_process_leaves_no_capsule_process";
    if let Some(dir) = std::env::var_os(KILLED_CHILD) {
        killed_child_body(NAME, Path::new(&dir));
        return;
    }
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let ready = dir.path().join("ready");
    let log_path = dir.path().join("child.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let (mut command, entry) = sot_platform::test_isolated::test_command(NAME);
    let mut child = command
        .env(KILLED_CHILD, dir.path())
        .stdin(Stdio::piped())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .expect("start the test process to kill");
    let child_log = || std::fs::read_to_string(&log_path).unwrap_or_default();

    let before_kill = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        poll_until(
            || match child.try_wait().unwrap() {
                Some(exit) => panic!("the test process ended ({exit}) before its capsule was ready:\n{}", child_log()),
                None => std::fs::read_to_string(&ready).ok(),
            },
            // The child's own bounds (30 s lane, 90 s Ready) plus its start, so its specific failure is the one reported.
            Duration::from_secs(150),
            "the test process to start its capsule",
        );
        assert!(!fence_free(&state_dir), "precondition: the supervisor holds its fence");
    }));
    let _ = child.kill();
    let reaped = sot_platform::test_isolated::wait_until(&mut child, Instant::now() + Duration::from_secs(5));
    let gone = before_kill.is_ok() && reaped.is_ok() && capsule_gone_within(&state_dir, Duration::from_secs(5));
    end_what_outlived(&state_dir);

    if let Err(unwind) = before_kill {
        std::panic::resume_unwind(unwind);
    }
    let exit = reaped.unwrap_or_else(|e| panic!("{e}"));
    assert!(!exit.success(), "the killed test process reported success: {exit}");
    entry.assert_once(child.id());
    assert!(gone, "the capsule outlived its killed test process (the fence was still held 5 s after the kill)");
}

/// A panicking test must leave no capsule process: not the supervisor, and not the `--survival normal` leg that
/// outlives it by design.
#[test]
fn a_panicking_test_leaves_no_capsule_process() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let child = spawn_supervisor(&state_dir, "--start", SHELL);
    let conn = wait_for_lane(&state_dir_hash(&state_dir), Duration::from_secs(30));
    wait_for_ready(&conn, Duration::from_secs(90));
    drop(conn);

    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _held = child;
        panic!("a test assert fires before its own kill");
    }));
    assert!(unwound.is_err());

    let gone = capsule_gone_within(&state_dir, Duration::from_secs(5));
    end_what_outlived(&state_dir);
    assert!(gone, "the capsule outlived its guard (the fence was still held 5 s after the unwind)");
}
