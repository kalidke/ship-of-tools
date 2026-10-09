#![cfg(any(windows, target_os = "linux"))]
//! Real cross-process integration tests for `sot_log::supervisor` (ADR
//! 0041 step 6 U2; ADR 0043 decisions 20/21 for the Linux half) — spawns
//! the REAL `sot-capsule` binary (`supervise`, and the no-supervisor
//! `endrun`/`reset` in-process callers), talking to it exactly the way a
//! real launcher/FE client would: connect the supervisor lane, run the
//! full same-connection challenge, then `hello`/`status`/`command`/
//! `query`. The classifier's own transition table (A1-A5/B0-B9) and the
//! journal's own crash-durability are already proven scripted-only by
//! `classify.rs`'s and `supervisor/journal/`'s own unit tests; what these tests
//! add is proof the WIRING across a real process boundary is correct —
//! on BOTH platforms now, driving a real Linux `sot-capsule supervise`
//! spawning a real `sot-capsule run` over the socket lane, exactly as
//! this file already did for Windows over the pipe lane.
//!
//! Deterministic by construction: every wait below is a BOUNDED POLL for
//! an external, observable fact (a pipe/socket answering, a process's
//! own exit code) — never a sleep-and-hope, and never a lifetime-counter
//! observation of kernel state.

use sot_log::lane::client::{Endpoint, PlatformEndpoint};
use sot_log::supervisor::journal;
use sot_log::host::state_dir::state_dir_hash;
use sot_log::supervisor::{connect_and_challenge_for_test, request_for_test};
use sot_log::lane::wire::{SupervisorOp, SupervisorOperationState, SupervisorPhase, SupervisorReply, SupervisorRequest};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "../support/capsule_guard.rs"]
mod capsule_guard;
use capsule_guard::CapsuleGuard;

/// L1-unix LU3c: the lane's own client type, chosen once — the SAME
/// platform-chosen alias `sot_log::supervisor`'s own production code
/// is generic over, so this test names one type regardless of platform
/// instead of `sot_log::lane::pipe_win::PipeClient` (Windows-only, as this
/// whole file used to be).
type Client = <PlatformEndpoint as Endpoint>::Client;

/// An interactive shell on its pty stays open until EndRun, on both
/// platforms — the process every "spawn and wait for Ready" test starts.
#[cfg(windows)]
const SHELL: &[&str] = &["cmd.exe"];
#[cfg(target_os = "linux")]
const SHELL: &[&str] = &["/bin/sh"];

/// The one scripted producer that exits shortly after reaching Ready —
/// `a_shell_that_dies_shortly_after_ready_trips_the_anti_flap_bound`'s
/// own leg. F5 (Codex review round): ~2s of life, not ~1 -- a loaded
/// runner can miss the ONLY status poll that would ever observe Ready
/// before a ~1s-lived leg self-exits, failing the test while the
/// supervisor correctly reaches its anti-flap terminal state anyway
/// (reproduced). ~2s is wide enough for the poll to observe Ready even
/// under load, far shorter than `STABILITY_INTERVAL` (60s in
/// `supervisor/`), so the leg is still unstable and three of them
/// still trip the anti-flap bound.
#[cfg(windows)]
const SELF_EXITING_PRODUCER: &[&str] = &["cmd.exe", "/d", "/c", "ping -n 3 127.0.0.1 >nul & exit 1"];
#[cfg(target_os = "linux")]
const SELF_EXITING_PRODUCER: &[&str] = &["/bin/sh", "-c", "sleep 2; exit 1"];

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

/// The voyage mgmt lane's own unchallenged connect, per platform — the
/// one call `pipe_gone`-style checks need
/// (`endrun_and_reset_without_a_running_supervisor` and
/// `a_crashed_supervisor_s_end_run_is_recovered_and_queryable_by_a_fresh_one`
/// use it via [`sot_log::lane::transport::TransportError::is_endpoint_absent`]
/// rather than a hand-rolled `NotFound` match).
#[cfg(windows)]
fn connect_voyage_mgmt(voyage_id: &str) -> Result<Client, sot_log::lane::transport::TransportError> {
    sot_log::lane::pipe_win::connect_voyage_pipe(voyage_id)
}
#[cfg(target_os = "linux")]
fn connect_voyage_mgmt(voyage_id: &str) -> Result<Client, sot_log::lane::transport::TransportError> {
    sot_log::lane::socket_unix::connect_voyage_socket(voyage_id)
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

/// Bounded poll for the lane to accept a connection AND answer the
/// challenge — the observable fact a real client waits on, never a
/// sleep guessing how long `bind_supervisor` takes.
fn wait_for_client(h: &str, timeout: Duration) -> Client {
    poll_until(
        || connect_and_challenge_for_test(h).ok().map(|(conn, _process)| conn),
        timeout,
        "the supervisor lane to accept and answer the challenge",
    )
}

/// As [`wait_for_client`], held as the [`Lane`] the request helpers take.
fn wait_for_lane(h: &str, timeout: Duration) -> Lane {
    Lane::new(h, wait_for_client(h, timeout))
}

/// A supervisor-lane connection the request helpers may replace. The supervisor closes a connection whose client has
/// sent nothing for `LANE_IDLE_DEADLINE`, so a host that stalls this test that long between two requests would cost
/// the test its connection. Every helper request may be asked twice: `status` and `query` only read, and a command
/// carries its operation id, which the journal answers again rather than running twice (ADR 0041). So a request that
/// fails reconnects once and asks again, as the production client does (`attach_client/worker/quit.rs`); after a
/// reconnect a command's reply is the journal's state for that id. A test of a raw connection's own behaviour uses
/// `request_for_test` on [`Lane::client`].
struct Lane {
    h: String,
    conn: std::cell::RefCell<Client>,
}

impl Lane {
    fn new(h: &str, conn: Client) -> Lane {
        Lane {
            h: h.to_string(),
            conn: std::cell::RefCell::new(conn),
        }
    }

    /// The connection now held, for a test of a raw connection's own behaviour.
    fn client(&self) -> std::cell::Ref<'_, Client> {
        self.conn.borrow()
    }

    /// One reply to `request`, each attempt bounded by `budget`; a failed first attempt reconnects once.
    fn request(
        &self,
        what: &str,
        request: &SupervisorRequest,
        budget: Duration,
    ) -> SupervisorReply {
        let first = match request_for_test(&self.conn.borrow(), request, Instant::now() + budget) {
            Ok(reply) => return reply,
            Err(e) => e,
        };
        eprintln!("{what}: {first}; reconnecting once");
        *self.conn.borrow_mut() = wait_for_client(&self.h, Duration::from_secs(10));
        request_for_test(&self.conn.borrow(), request, Instant::now() + budget)
            .unwrap_or_else(|second| panic!("{what}: {first}, and after one reconnect: {second}"))
    }
}

fn status(conn: &Lane) -> (Option<String>, Option<u64>, SupervisorPhase) {
    match conn.request("status", &SupervisorRequest::Status, Duration::from_secs(5)) {
        SupervisorReply::StatusOk {
            voyage, leg, phase, ..
        } => (voyage, leg, phase),
        other => panic!("expected StatusOk, got {other:?}"),
    }
}

/// As [`status`], but never panics — `Err`'s own text names what went
/// wrong. Used only where a connection MAY legitimately have stopped
/// answering (a diagnostic best-effort, or a poll loop that itself
/// decides what a failure means).
fn try_status(conn: &Client) -> Result<(Option<String>, Option<u64>, SupervisorPhase), String> {
    match request_for_test(conn, &SupervisorRequest::Status, Instant::now() + Duration::from_secs(5)) {
        Ok(SupervisorReply::StatusOk { voyage, leg, phase, .. }) => Ok((voyage, leg, phase)),
        Ok(other) => Err(format!("expected StatusOk, got {other:?}")),
        Err(e) => Err(format!("{e}")),
    }
}

/// The lane binds BEFORE any adopt or spawn (ADR 0041), so answering
/// `status` does not by itself mean a leg is READY yet — poll for the
/// phase itself, the observable fact, rather than assuming spawn
/// finished the instant the lane became reachable.
fn wait_for_ready(conn: &Lane, timeout: Duration) -> (String, u64) {
    poll_until(
        || match status(conn) {
            (Some(voyage), Some(leg), SupervisorPhase::Ready) => Some((voyage, leg)),
            _ => None,
        },
        timeout,
        "the leg to reach phase Ready",
    )
}

fn command(conn: &Lane, operation_id: &str, op: SupervisorOp) -> SupervisorOperationState {
    match conn.request(
        "command",
        &SupervisorRequest::Command {
            operation_id: operation_id.to_string(),
            op,
        },
        // Generous: an EndRun's own reply is DEFERRED to record_closed
        // (B3) -- the mgmt-lane exchange plus the leg writing its own
        // marker, both real OS work on a background thread, not a bound
        // this crate itself pins tighter than "well within the ADR's own
        // per-op budgets stacked together". The supervisor keeps the
        // connection it owes that reply however long it takes
        // (`authority/lane.rs` `service_lane`).
        Duration::from_secs(30),
    ) {
        SupervisorReply::Operation(state) => state,
        other => panic!("expected Operation, got {other:?}"),
    }
}

fn query(conn: &Lane, operation_id: &str) -> SupervisorOperationState {
    match conn.request(
        "query",
        &SupervisorRequest::Query {
            operation_id: operation_id.to_string(),
        },
        Duration::from_secs(5),
    ) {
        SupervisorReply::Operation(state) => state,
        other => panic!("expected Operation, got {other:?}"),
    }
}

/// Poll `query` past both pre-terminal milestones to whatever terminal
/// state follows. Callers that already PROVED `record_closed` via the
/// `end_run` command's own deferred reply (B3) use this only for the
/// remaining `record_closed -> record_verified`/`failed` step.
fn poll_to_terminal(
    conn: &Lane,
    operation_id: &str,
    timeout: Duration,
) -> SupervisorOperationState {
    poll_until(
        || match query(conn, operation_id) {
            SupervisorOperationState::Accepted | SupervisorOperationState::RecordClosed => None,
            other => Some(other),
        },
        timeout,
        "the operation to reach a terminal state",
    )
}

/// Submits `end_run` and asserts its OWN reply is `record_closed` (B3:
/// "the lane reply for an accepted EndRun is sent AT record_closed, not
/// at admission") — proving the FIRST half of the "record_closed then
/// record_verified" sequence directly, rather than silently accepting
/// either as `poll_to_terminal` alone would (Codex review round 2: "the
/// workflow's own description is unproved... never requires observing
/// record_closed").
fn end_run_and_expect_record_closed(conn: &Lane, operation_id: &str, reason: &str, voyage: String) {
    let reply = command(
        conn,
        operation_id,
        SupervisorOp::EndRun {
            reason: reason.into(),
            voyage,
        },
    );
    assert_eq!(
        reply,
        SupervisorOperationState::RecordClosed,
        "end_run's own command reply must arrive AT record_closed (ADR 0041:592)"
    );
}

/// Blocks on `conn.read` in a scoped thread so an EOF (or any other
/// outcome) can be awaited with a bounded timeout — `Client` is
/// `Sync` by design (its own doc: a second thread may `cancel` a
/// blocking call in flight), and a read still blocked at the timeout is
/// cancelled before the scope joins it. Used to verify a connection
/// the supervisor is EXPECTED to close actually does.
fn expect_connection_closes(conn: &Client, timeout: Duration) {
    std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel();
        scope.spawn(move || {
            let mut buf = [0u8; 16];
            let _ = tx.send(conn.read(&mut buf));
        });
        match rx.recv_timeout(timeout) {
            Ok(Ok(0)) => {} // ordered EOF -- the connection closed, exactly as claimed
            Ok(other) => panic!("expected the connection to close (EOF), got {other:?}"),
            Err(_) => {
                conn.cancel();
                panic!("the connection never closed within {timeout:?}");
            }
        }
    });
}

fn spawn_supervisor(state_dir: &Path, mode: &str, argv: &[&str]) -> CapsuleGuard {
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
        .stderr(Stdio::inherit());
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

/// As [`wait_for_exit`], but on timeout makes ONE best-effort attempt to
/// reconnect and report whatever `status` still claims, so a future CI
/// failure NAMES the stuck lifecycle state instead of just timing out
/// (per the coordinator's own round-4 addendum on the flap test).
///
/// Takes `&mut Child` (Codex review round 3, N13), never an owned
/// `Child` — an earlier version moved the child out of its own
/// `CapsuleGuard` before calling this, so a `panic!` on timeout unwound
/// past a bare `Child` with no guard left watching it: `Child`'s own
/// `Drop` does not kill anything, only closes the handle, so the
/// timed-out supervisor process leaked, orphaned, past every test that
/// hit this exact path. Borrowing keeps the CALLER's `CapsuleGuard` in
/// possession of the child throughout, so its `Drop` still runs
/// (kill + wait) as the panic unwinds through it.
fn wait_for_exit_with_diagnostics(child: &mut Child, h: &str, timeout: Duration) -> std::process::ExitStatus {
    let started = Instant::now();
    let deadline = started + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let diagnostic = connect_and_challenge_for_test(h).ok().and_then(|(conn, _)| try_status(&conn).ok());
            panic!(
                "timed out after {:?} waiting for the supervisor process to exit; last reachable \
                 status (voyage, leg, phase): {diagnostic:?}",
                started.elapsed()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
mod lifecycle;
mod authority;
mod spawn;


/// Names the test process that `a_killed_test_process_leaves_no_capsule_process` kills: set (to its folder) only in
/// the child it starts, whose body is `killed_child_body`.
const KILLED_CHILD: &str = "SOT_TEST_KILLED_CHILD";

fn recorded_voyage(state_dir: &Path) -> Option<String> {
    match journal::pointer::validate(state_dir) {
        journal::pointer::PointerState::Valid(voyage) => Some(voyage),
        _ => None,
    }
}

fn writer_lock_path(state_dir: &Path, voyage: &str) -> PathBuf {
    sot_log::supervisor::voyage_root_path(state_dir, voyage).join(sot_log::store::voyage::WRITER_LOCK)
}

/// Each probe takes its lock and drops it before returning: locks conflict between two opens in one process, so
/// a probe that kept its lock would make the next probe of that lock read a live holder.
fn fence_free(state_dir: &Path) -> bool {
    journal::fence::lock_supervisor(state_dir).is_ok()
}

fn writer_free(lock: &Path) -> bool {
    sot_log::lock_writer(lock).is_ok()
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

/// A capsule is gone when neither kernel lock it holds is held: the supervisor fence and its voyage's writer lock,
/// both released on any death. No pid is read.
fn capsule_gone_within(state_dir: &Path, bound: Duration) -> bool {
    free_within(
        || fence_free(state_dir) && recorded_voyage(state_dir).map_or(true, |v| writer_free(&writer_lock_path(state_dir, &v))),
        bound,
    )
}

/// A red's cleanup, run on every path after the kill: ends a supervisor and a leg that outlived their test, over
/// their own lanes. It never panics, so the assert that follows it still reports.
fn end_what_outlived(state_dir: &Path) {
    use sot_log::lane::wire::{encode_mgmt_request, MgmtRequest};
    let voyage = recorded_voyage(state_dir);
    if state_dir.exists() && !fence_free(state_dir) {
        if let Ok((conn, _)) = connect_and_challenge_for_test(&state_dir_hash(state_dir)) {
            let ask = |operation_id: &str, op: SupervisorOp| {
                let command = SupervisorRequest::Command { operation_id: operation_id.to_string(), op };
                let _ = request_for_test(&conn, &command, Instant::now() + Duration::from_secs(30));
            };
            if let Some(voyage) = &voyage {
                ask("cleanup-end", SupervisorOp::EndRun { reason: "test cleanup".into(), voyage: voyage.clone() });
            }
            ask("cleanup-stop", SupervisorOp::Stop);
        }
        free_within(|| fence_free(state_dir), Duration::from_secs(60));
    }
    if let Some(voyage) = voyage {
        let lock = writer_lock_path(state_dir, &voyage);
        if !writer_free(&lock) {
            if let (Ok(mgmt), Ok(shutdown)) =
                (connect_voyage_mgmt(&voyage), encode_mgmt_request(&MgmtRequest::Shutdown { reason: "test cleanup".into() }))
            {
                let _ = mgmt.write_all(&shutdown);
            }
            free_within(|| writer_free(&lock), Duration::from_secs(30));
        }
    }
}

/// The body that runs in the child of `a_killed_test_process_leaves_no_capsule_process`: a supervisor and its leg on
/// the parent's state folder, an empty `ready` marker, then a wait on stdin that only the parent's kill
/// (or its death) ends. It does not make a runtime folder: it uses the parent's, which the parent removes.
fn killed_child_body(name: &str, dir: &Path) {
    sot_log::test_isolated::enter(name);
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
    let (mut command, entry) = sot_log::test_isolated::test_command(NAME);
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
        let voyage = recorded_voyage(&state_dir).expect("precondition: the supervisor recorded a voyage");
        assert!(!writer_free(&writer_lock_path(&state_dir, &voyage)), "precondition: the leg holds the writer lock");
    }));
    let _ = child.kill();
    let reaped = sot_log::test_isolated::wait_until(&mut child, Instant::now() + Duration::from_secs(5));
    let gone = before_kill.is_ok() && reaped.is_ok() && capsule_gone_within(&state_dir, Duration::from_secs(5));
    end_what_outlived(&state_dir);

    if let Err(unwind) = before_kill {
        std::panic::resume_unwind(unwind);
    }
    let exit = reaped.unwrap_or_else(|e| panic!("{e}"));
    assert!(!exit.success(), "the killed test process reported success: {exit}");
    entry.assert_once(child.id());
    assert!(gone, "the capsule outlived its killed test process (the fence or the writer lock was still held 5 s after the kill)");
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
    assert!(gone, "the capsule outlived its guard (the fence or the writer lock was still held 5 s after the unwind)");
}
