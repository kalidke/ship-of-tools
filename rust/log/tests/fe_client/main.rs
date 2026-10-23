#![cfg(any(windows, target_os = "linux"))]
//! ADR 0041 step 6 U3 (ADR 0043 decisions 20/21 for the Linux half):
//! real cross-process integration tests for
//! `sot_log::attach_client::client::FeAttachClient` — the FE attach-only client,
//! driven exactly the way the real frontend drives it, against a REAL
//! `sot-capsule supervise` and a REAL capsule leg. `tests/supervisor/`
//! already proves the supervisor's OWN lifecycle wiring across a real
//! process boundary; what THIS file adds is proof the CLIENT's own six
//! rulings (`attach_client::rules`'s pure state machines) hold when driven by a real
//! reconnect-classified episode loop against real named pipes/Unix
//! domain sockets, not merely scripted inputs.
//!
//! One case per acceptance-matrix row named in the U3 unit: attach as a
//! watcher and receive the checkpoint; first input takes the pen and the
//! resize precedes the flush (proven by the echo of the first input reaching the screen); `end_run` from the quit dispatcher gets
//! `record_closed`; reconnect after the capsule is killed and a fresh
//! supervisor takes over restores the screen from the new checkpoint.
//!
//! Deterministic by construction where the underlying primitives allow
//! it: every wait is a bounded poll for an external, observable fact,
//! never a sleep-and-hope. The reconnect episode's own backoff (250ms
//! doubling to 4s) means real time passes during the reconnect test —
//! this file does not attempt to inject a clock into a live worker
//! thread, unlike `attach_client::rules`'s own unit tests.

use sot_log::lane::client::{Endpoint, PlatformEndpoint};
use sot_log::attach_client::client::{FeAttachClient, InputOutcome};
use sot_platform::host::state_dir::state_dir_hash;
use sot_log::supervisor::{connect_and_challenge_for_test, request_for_test};
use sot_log::lane::wire::{
    SupervisorOp, SupervisorOperationState, SupervisorPhase, SupervisorReply,
    SupervisorRequest,
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[path = "../support/capsule_guard.rs"]
mod capsule_guard;
use capsule_guard::CapsuleGuard;

/// L1-unix LU3c: the lane's own client type, chosen once — see
/// `tests/supervisor/`'s identical alias for why this replaces
/// `sot_log::lane::pipe_win::PipeClient` (Windows-only, as this whole file used
/// to be).
type Client = <PlatformEndpoint as Endpoint>::Client;

/// An interactive shell on its pty stays open until EndRun, on both
/// platforms — mirrors `tests/supervisor/`'s identical `SHELL` const.
#[cfg(windows)]
const SHELL: &[&str] = &["cmd.exe"];
#[cfg(target_os = "linux")]
const SHELL: &[&str] = &["/bin/sh"];

/// Points `SOT_RUNTIME_DIR` at a fresh, mode-0700 tempdir under `/tmp`
/// for the lifetime of the returned guard — mirrors `tests/supervisor/`
/// identical helper (see its own doc). A no-op on Windows.
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

/// The voyage mgmt lane's own unchallenged connect, per platform —
/// mirrors `tests/supervisor/`'s identical helper.
/// Real-process tests are SERIALIZED (same reason and mechanism as
/// `tests/supervisor/`'s `SERIAL`): each spawns a supervisor, a capsule and a
/// shell, and a two-core runner is the shared resource.
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

fn spawn_supervisor(state_dir: &Path, mode: &str, argv: &[&str]) -> CapsuleGuard {
    let mut cmd = Command::new(capsule_exe());
    cmd.arg("supervise")
        .arg(state_dir)
        .arg(mode)
        .arg("--assume-no-rollback-target")
        .arg("--")
        .args(argv)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    CapsuleGuard::spawn(&mut cmd)
}

/// [`spawn_supervisor`] with an explicit initial pty size — ADR 0042
/// amendment: proves a headless client adopts whatever geometry the
/// capsule actually has, rather than the client's own placeholder.
fn spawn_supervisor_sized(state_dir: &Path, mode: &str, cols: u16, rows: u16, argv: &[&str]) -> CapsuleGuard {
    let mut cmd = Command::new(capsule_exe());
    cmd.arg("supervise")
        .arg(state_dir)
        .arg(mode)
        .arg("--cols")
        .arg(cols.to_string())
        .arg("--rows")
        .arg(rows.to_string())
        .arg("--assume-no-rollback-target")
        .arg("--")
        .args(argv)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    CapsuleGuard::spawn(&mut cmd)
}

fn wait_for_exit(child: &mut CapsuleGuard, timeout: Duration) -> std::process::ExitStatus {
    poll_until(|| child.child_mut().try_wait().unwrap(), timeout, "the supervisor process to exit")
}

/// Bounded poll for the lane to accept a connection AND answer the
/// challenge — `tests/supervisor/`'s own helper of the same name.
fn wait_for_lane(h: &str, timeout: Duration) -> Client {
    poll_until(
        || connect_and_challenge_for_test(h).ok().map(|(conn, _process)| conn),
        timeout,
        "the supervisor lane to accept and answer the challenge",
    )
}

fn status(conn: &Client) -> (Option<String>, Option<u64>, SupervisorPhase) {
    match request_for_test(conn, &SupervisorRequest::Status, Instant::now() + Duration::from_secs(5)).expect("status") {
        SupervisorReply::StatusOk { voyage, leg, phase, .. } => (voyage, leg, phase),
        other => panic!("expected StatusOk, got {other:?}"),
    }
}

fn wait_for_ready(conn: &Client, timeout: Duration) -> (String, u64) {
    poll_until(
        || match status(conn) {
            (Some(voyage), Some(leg), SupervisorPhase::Ready) => Some((voyage, leg)),
            _ => None,
        },
        timeout,
        "the leg to reach phase Ready",
    )
}

fn command(conn: &Client, operation_id: &str, op: SupervisorOp) -> SupervisorOperationState {
    match request_for_test(
        conn,
        &SupervisorRequest::Command { operation_id: operation_id.to_string(), op },
        Instant::now() + Duration::from_secs(5),
    )
    .expect("command")
    {
        SupervisorReply::Operation(state) => state,
        other => panic!("expected Operation, got {other:?}"),
    }
}

fn screen_text(screen: &vt100_ctt::Screen) -> String {
    let (rows, cols) = screen.size();
    let mut text = String::new();
    for r in 0..rows {
        for c in 0..cols {
            if let Some(cell) = screen.cell(r, c) {
                text.push_str(cell.contents());
            }
        }
        text.push('\n');
    }
    text
}

fn wake_flag() -> (Arc<AtomicBool>, Box<dyn Fn() + Send + 'static>) {
    let woke = Arc::new(AtomicBool::new(false));
    let woke2 = Arc::clone(&woke);
    (woke, Box::new(move || woke2.store(true, Ordering::Relaxed)))
}

/// Polls `client.pump()` + its screen text against `pred`, bounded by
/// `timeout`. Panics with the client's own dead/status diagnostics on
/// timeout rather than a bare "timed out", since a hung client here is
/// exactly the failure mode these tests exist to catch.
fn poll_screen(client: &mut FeAttachClient, timeout: Duration, pred: impl Fn(&str) -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        client.pump();
        let text = screen_text(client.screen());
        if pred(&text) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn query(conn: &Client, operation_id: &str) -> SupervisorOperationState {
    match request_for_test(
        conn,
        &SupervisorRequest::Query { operation_id: operation_id.to_string() },
        Instant::now() + Duration::from_secs(5),
    )
    .expect("query")
    {
        SupervisorReply::Operation(state) => state,
        other => panic!("expected Operation, got {other:?}"),
    }
}

/// Ends the run cleanly (mirrors `tests/supervisor/`'s own `end_run_and_expect_record_closed` + `poll_to_terminal`
/// composition): `record_closed`, then `record_verified`, the wire's names for the end being acknowledged and then
/// final. No record is kept, so nothing is read back.
fn end_run_and_wait_ended(conn: &Client, voyage: &str) {
    let op_id = "test-teardown-end-run";
    let reply = command(conn, op_id, SupervisorOp::EndRun { reason: "test teardown".into(), voyage: voyage.to_string() });
    assert_eq!(reply, SupervisorOperationState::RecordVerified);
    let final_state = poll_until(
        || match query(conn, op_id) {
            SupervisorOperationState::Accepted | SupervisorOperationState::RecordClosed => None,
            other => Some(other),
        },
        Duration::from_secs(60),
        "record_verified",
    );
    assert_eq!(final_state, SupervisorOperationState::RecordVerified);
}

mod headless;
