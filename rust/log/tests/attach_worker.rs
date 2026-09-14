#![cfg(any(windows, target_os = "linux"))]
//! ADR 0046 decision 3 (lane B3a): real cross-process integration tests
//! for `sot_log::attach_worker::AttachWorker` — the transport half
//! extracted out of `fe_client_io::FeAttachClient`. This is a PURE,
//! behavior-preserving extraction (see `attach_worker.rs`'s own top
//! doc): `tests/fe_client.rs`'s existing 11 real-process tests, run
//! unchanged against the new wrapper, are the proof of that. This file
//! adds coverage for the one genuine addition the extraction makes —
//! bounded ingress — plus a real multi-chunk checkpoint transfer, whose
//! WIRE-level chunking (`AttachServer::CheckpointChunk`, still
//! reassembled internally into one `WorkerEvent::Checkpoint` exactly as
//! the pre-extraction worker did) is otherwise never exercised by any
//! existing test: every other real-process fixture in this crate uses a
//! small 80x24 terminal whose checkpoint always fits in one wire chunk.
//!
//! Real supervisor+capsule fixture — `tests/fe_client.rs`'s own pattern
//! (spawn a real `sot-capsule supervise`, poll for lane readiness),
//! since a `TestTransport`-mocked capsule (`tests/capsule.rs`'s own
//! harness) drives the capsule's server-side `Transport` trait, not the
//! client-side `Endpoint` an `AttachWorker` connects through.
//!
//! Gated `target_os = "linux"` for the multi-chunk test specifically
//! (see that test's own doc): checkpoint reassembly is platform-
//! independent code, so proving it once, on Linux, is enough — a
//! Windows fixture that reliably fills a 512x256 screen densely enough
//! to force multiple wire chunks needs its own producer this lane has
//! no reason to build.

use sot_log::attach_worker::{AttachWorker, IngressRefused, WorkerEvent};
use sot_log::client::{Endpoint, PlatformEndpoint};
use sot_log::state_dir::state_dir_hash;
use sot_log::supervisor::{connect_and_challenge_for_test, request_for_test};
use sot_log::wire::{SupervisorOp, SupervisorOperationState, SupervisorPhase, SupervisorReply, SupervisorRequest};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Client = <PlatformEndpoint as Endpoint>::Client;

#[cfg(windows)]
const SHELL: &[&str] = &["cmd.exe"];
#[cfg(target_os = "linux")]
const SHELL: &[&str] = &["/bin/sh"];

// -----------------------------------------------------------------------
// Leaf helpers — duplicated from `tests/fe_client.rs` rather than shared
// (this crate's own convention, stated there: a supervisor-lane CLIENT
// test's helpers are small enough that one copy per file beats a shared
// dependency between two independent test binaries).
// -----------------------------------------------------------------------

#[cfg(target_os = "linux")]
struct RuntimeDirGuard {
    _tmp: tempfile::TempDir,
}
#[cfg(target_os = "linux")]
fn isolated_runtime_dir() -> RuntimeDirGuard {
    let tmp = tempfile::Builder::new().prefix("sot-t").tempdir_in("/tmp").expect("tempdir under /tmp");
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

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn capsule_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sot-capsule"))
}

struct KillGuard(Option<Child>);
impl Drop for KillGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline {
                if matches!(c.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            eprintln!("KillGuard: process did not exit within 30s of kill(); abandoning the wait");
        }
    }
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

fn spawn_supervisor(state_dir: &Path, mode: &str, argv: &[&str]) -> Child {
    let mut cmd = Command::new(capsule_exe());
    cmd.arg("supervise")
        .arg(state_dir)
        .arg(mode)
        .arg("--assume-no-rollback-target")
        .arg("--")
        .args(argv)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    cmd.spawn().expect("spawn sot-capsule supervise")
}

fn spawn_supervisor_sized(state_dir: &Path, mode: &str, cols: u16, rows: u16, argv: &[&str]) -> Child {
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
    cmd.spawn().expect("spawn sot-capsule supervise (sized)")
}

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
    match request_for_test(conn, &SupervisorRequest::Command { operation_id: operation_id.to_string(), op }, Instant::now() + Duration::from_secs(5))
        .expect("command")
    {
        SupervisorReply::Operation(state) => state,
        other => panic!("expected Operation, got {other:?}"),
    }
}

fn query(conn: &Client, operation_id: &str) -> SupervisorOperationState {
    match request_for_test(conn, &SupervisorRequest::Query { operation_id: operation_id.to_string() }, Instant::now() + Duration::from_secs(5)).expect("query") {
        SupervisorReply::Operation(state) => state,
        other => panic!("expected Operation, got {other:?}"),
    }
}

fn end_run_and_wait_verified(conn: &Client, voyage: &str) {
    let op_id = "test-teardown-end-run";
    let reply = command(conn, op_id, SupervisorOp::EndRun { reason: "test teardown".into(), voyage: voyage.to_string() });
    assert_eq!(reply, SupervisorOperationState::RecordClosed);
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

/// Spawns a real [`AttachWorker`] against the supervisor lane named by
/// `h`, wiring its sink into a plain channel this file polls directly —
/// the same shape `fe_client_io::FeAttachClient::attach_inner` builds,
/// minus the parser/UI bookkeeping this file has no need for.
/// `recorded_bytes`/`last_input_outcome` are the worker's own shared
/// observables; this file has no use for them beyond satisfying the
/// constructor, so each test gets a fresh, otherwise-unread pair.
fn spawn_worker(h: String, cols: u16, rows: u16, controller_id: &str, ingress_bound: usize) -> (AttachWorker<PlatformEndpoint>, Receiver<WorkerEvent>) {
    let (tx, rx) = mpsc::channel::<WorkerEvent>();
    let worker = AttachWorker::spawn(
        PlatformEndpoint::default(),
        h,
        cols,
        rows,
        controller_id.to_string(),
        "test-handle".to_string(),
        None,
        false,
        ingress_bound,
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
        Arc::new(Mutex::new(None)),
        move |e| {
            let _ = tx.send(e);
        },
    )
    .expect("spawn attach worker");
    (worker, rx)
}

/// Bounded poll: applies `f` to every event received until it returns
/// `Some`, or panics after `timeout`.
fn recv_until<T>(rx: &Receiver<WorkerEvent>, timeout: Duration, mut f: impl FnMut(&WorkerEvent) -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for the expected worker event");
        match rx.recv_timeout(remaining.min(Duration::from_millis(200))) {
            Ok(e) => {
                if let Some(v) = f(&e) {
                    return v;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("worker event channel disconnected before the expected event arrived"),
        }
    }
}

// -----------------------------------------------------------------------
// Bounded ingress
// -----------------------------------------------------------------------

#[test]
fn an_ingress_overflow_is_refused() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", SHELL);
    let guard = KillGuard(Some(child));
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    // A bound smaller than a single ordinary send: the refusal is
    // synchronous and needs no live connection or timing race to prove
    // ("never queued unbounded" holds from the very first call).
    let (worker, _rx) = spawn_worker(h.clone(), 80, 24, "attach-worker-test-ingress", 4);

    let result = worker.send_input(vec![0u8; 10]);
    assert_eq!(result, Err(IngressRefused), "a send exceeding the ingress bound must be refused, not queued");

    // Empty input is never enqueued and never refused -- it types
    // nothing and must not itself consume any of the bound.
    assert_eq!(worker.send_input(Vec::new()), Ok(()), "empty input must be a silent no-op, not a refusal");

    // The bound is still intact after the refusal above: a send that
    // fits now succeeds, proving the refused 10-byte attempt above left
    // no stale reservation behind.
    assert_eq!(worker.send_input(vec![0u8; 4]), Ok(()), "a send within the bound must still succeed after an earlier refusal");

    drop(worker);
    end_run_and_wait_verified(&conn, &voyage);
    let _ = command(&conn, "test-ingress-stop", SupervisorOp::Stop);
    drop(guard);
}

// -----------------------------------------------------------------------
// A multi-chunk checkpoint transfer reassembles correctly
// -----------------------------------------------------------------------

/// A large, densely-colored screen (near the checkpoint format's own
/// MAX_ROWS x MAX_COLS) so the checkpoint's own worst-case-per-cell cost
/// (content + non-default attrs) comfortably exceeds one wire chunk
/// (`sot_log::wire::MAX_CHECKPOINT_CHUNK_PAYLOAD`, ~1 MiB) — a blank or
/// default-attrs screen would not (a default cell costs a single zero
/// byte in the checkpoint format), so this is the one real-process
/// fixture in the crate whose checkpoint transfer actually spans
/// multiple wire `CheckpointChunk` frames. `AttachWorker` reassembles
/// them internally, exactly as the pre-extraction worker did, and this
/// test only ever sees the resulting single `WorkerEvent::Checkpoint` —
/// proof the reassembly is correct (order, no truncation, no
/// corruption) even though this lane changes nothing about how it
/// works.
#[cfg(target_os = "linux")]
#[test]
fn multi_chunk_checkpoint_reassembles_correctly() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let argv: Vec<&str> = vec![
        "/bin/sh",
        "-c",
        "p=$(printf '%.0sX' $(seq 1 500)); i=0; while [ $i -lt 300 ]; do printf '\\033[48;5;%dm%s\\033[0m\\n' $((i % 256)) \"$p\"; i=$((i+1)); done; sleep 3600",
    ];

    let child = spawn_supervisor_sized(&state_dir, "--start", 512, 256, &argv);
    let guard = KillGuard(Some(child));
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    // Give the shell time to fill the screen and scroll enough to
    // populate the scrollback ring before attaching.
    std::thread::sleep(Duration::from_secs(3));

    let (worker, rx) = spawn_worker(h.clone(), 512, 256, "attach-worker-test-multichunk", 64 * 1024);

    let checkpoint = recv_until(&rx, Duration::from_secs(60), |e| match e {
        WorkerEvent::Checkpoint(bytes) => Some(bytes.clone()),
        WorkerEvent::Terminal(reason) => panic!("worker reached a terminal state before checkpointing: {reason}"),
        _ => None,
    });

    assert!(
        checkpoint.len() > sot_log::wire::MAX_CHECKPOINT_CHUNK_PAYLOAD,
        "expected a checkpoint spanning multiple wire chunks (>{} bytes); got {} bytes -- \
         the fixture's own content may need to be denser",
        sot_log::wire::MAX_CHECKPOINT_CHUNK_PAYLOAD,
        checkpoint.len()
    );

    // The bytes really do decode into a well-formed checkpoint of the
    // spawned size -- proof the wire-level chunks were reassembled in
    // order with nothing dropped or duplicated.
    let mut parser = vt100_ctt::Parser::new(2, 2, 200);
    parser.restore_screen(&checkpoint).expect("a correctly reassembled checkpoint restores cleanly");
    assert_eq!(parser.screen().size(), (256, 512));

    drop(worker);
    end_run_and_wait_verified(&conn, &voyage);
    let _ = command(&conn, "test-multichunk-stop", SupervisorOp::Stop);
    drop(guard);
}
