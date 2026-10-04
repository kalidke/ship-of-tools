#![cfg(any(windows, target_os = "linux"))]
//! ADR 0041 step 6 U3 (ADR 0043 decisions 20/21 for the Linux half):
//! real cross-process integration tests for
//! `sot_log::fe_client_io::FeAttachClient` — the FE attach-only client,
//! driven exactly the way the real frontend drives it, against a REAL
//! `sot-capsule supervise` and a REAL capsule leg. `tests/supervisor.rs`
//! already proves the supervisor's OWN lifecycle wiring across a real
//! process boundary; what THIS file adds is proof the CLIENT's own six
//! rulings (`fe_client`'s pure state machines) hold when driven by a real
//! reconnect-classified episode loop against real named pipes/Unix
//! domain sockets, not merely scripted inputs.
//!
//! One case per acceptance-matrix row named in the U3 unit: attach as a
//! watcher and receive the checkpoint; first input takes the pen and the
//! resize precedes the flush (proven at the wire level, via the sealed
//! voyage record — the client's own black-box surface has no other way to
//! observe SEND order); `end_run` from the quit dispatcher gets
//! `record_closed`; reconnect after the capsule is killed and a fresh
//! supervisor takes over restores the screen from the new checkpoint.
//!
//! Deterministic by construction where the underlying primitives allow
//! it: every wait is a bounded poll for an external, observable fact,
//! never a sleep-and-hope. The reconnect episode's own backoff (250ms
//! doubling to 4s) means real time passes during the reconnect test —
//! this file does not attempt to inject a clock into a live worker
//! thread, unlike `fe_client`'s own unit tests.

use sot_log::client::{Endpoint, PlatformEndpoint};
use sot_log::fe_client_io::{FeAttachClient, InputOutcome};
use sot_log::segment::SegmentReader;
use sot_log::state_dir::state_dir_hash;
use sot_log::supervisor::{connect_and_challenge_for_test, request_for_test};
use sot_log::wire::{
    MgmtReply, MgmtRequest, SupervisorOp, SupervisorOperationState, SupervisorPhase, SupervisorReply,
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
/// `tests/supervisor.rs`'s identical alias for why this replaces
/// `sot_log::pipe_win::PipeClient` (Windows-only, as this whole file used
/// to be).
type Client = <PlatformEndpoint as Endpoint>::Client;

/// An interactive shell on its pty stays open until EndRun, on both
/// platforms — mirrors `tests/supervisor.rs`'s identical `SHELL` const.
#[cfg(windows)]
const SHELL: &[&str] = &["cmd.exe"];
#[cfg(target_os = "linux")]
const SHELL: &[&str] = &["/bin/sh"];

/// Points `SOT_RUNTIME_DIR` at a fresh, mode-0700 tempdir under `/tmp`
/// for the lifetime of the returned guard — mirrors `tests/supervisor.rs`
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
/// mirrors `tests/supervisor.rs`'s identical helper.
#[cfg(windows)]
fn connect_voyage_mgmt(voyage_id: &str) -> Result<Client, sot_log::transport::TransportError> {
    sot_log::pipe_win::connect_voyage_pipe(voyage_id)
}
#[cfg(target_os = "linux")]
fn connect_voyage_mgmt(voyage_id: &str) -> Result<Client, sot_log::transport::TransportError> {
    sot_log::socket_unix::connect_voyage_socket(voyage_id)
}

/// Real-process tests are SERIALIZED (same reason and mechanism as
/// `tests/supervisor.rs`'s `SERIAL`): each spawns a supervisor, a capsule and a
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
    CapsuleGuard::new(cmd.spawn().expect("spawn sot-capsule supervise"), state_dir)
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
    CapsuleGuard::new(cmd.spawn().expect("spawn sot-capsule supervise (sized)"), state_dir)
}

fn wait_for_exit(child: &mut CapsuleGuard, timeout: Duration) -> std::process::ExitStatus {
    poll_until(|| child.child_mut().try_wait().unwrap(), timeout, "the supervisor process to exit")
}

/// Bounded poll for the lane to accept a connection AND answer the
/// challenge — `tests/supervisor.rs`'s own helper of the same name.
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

/// As [`status`], but never panics — `Err`'s own text names what went
/// wrong. Mirrors `tests/supervisor.rs`'s own helper of the same
/// name (this crate's leaf-helper-duplication convention). Used only
/// where a connection MAY legitimately be gone (a diagnostic path that
/// must not itself panic and hide the real failure).
fn try_status(conn: &Client) -> Result<(Option<String>, Option<u64>, SupervisorPhase), String> {
    match request_for_test(conn, &SupervisorRequest::Status, Instant::now() + Duration::from_secs(5)) {
        Ok(SupervisorReply::StatusOk { voyage, leg, phase, .. }) => Ok((voyage, leg, phase)),
        Ok(other) => Err(format!("expected StatusOk, got {other:?}")),
        Err(e) => Err(format!("{e}")),
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

/// One bounded, cancellable `Client::read` — Codex review round,
/// finding 14: mirrors `tests/e2e_pipe.rs`'s own `read_bounded` (the
/// `sot_log::deadline` module this pattern is built on is crate-private,
/// unreachable from an external `tests/*.rs` binary, so this file keeps
/// its own copy of the idiom rather than the machinery). Spawns a worker
/// thread that owns the actual blocking read; `Client::cancel`,
/// called from THIS thread, unblocks it from another thread.
fn read_bounded(conn: &Arc<Client>, label: &'static str, timeout: Duration) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    let worker_conn = Arc::clone(conn);
    let jh = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let n = worker_conn.read(&mut buf).unwrap_or(0);
        let _ = tx.send(buf[..n].to_vec());
    });
    match rx.recv_timeout(timeout) {
        Ok(bytes) => {
            let _ = jh.join();
            bytes
        }
        Err(_) => {
            conn.cancel();
            let _ = jh.join();
            panic!("timed out waiting for {label}");
        }
    }
}

/// The capsule's OWN pid, read off the voyage pipe's mgmt sub-lane
/// (`probe`/`status`/`shutdown` — the step-5 lane, distinct from the
/// supervisor lane above). A throwaway connection: the mgmt lane accepts
/// unrelated probe/status connections freely alongside an already-attached
/// watcher (this test's own `FeAttachClient`), per step 5's design.
fn capsule_pid(voyage: &str) -> u32 {
    let conn = Arc::new(connect_voyage_mgmt(voyage).expect("connect voyage mgmt lane for status"));
    let bytes = sot_log::wire::encode_mgmt_request(&MgmtRequest::Status).unwrap();
    conn.write_all(&bytes).unwrap();
    let mut splitter = sot_log::wire::FrameSplitter::new();
    loop {
        let chunk = read_bounded(&conn, "mgmt status_ok", Duration::from_secs(10));
        assert!(!chunk.is_empty(), "unexpected EOF waiting for mgmt status_ok");
        let (frames, err) = splitter.feed(&chunk);
        assert_eq!(err, None, "unexpected wire error decoding mgmt status_ok");
        for f in frames {
            if let sot_log::wire::DecodedFrame::MgmtReply(MgmtReply::StatusOk { pid, .. }) = f {
                return pid;
            }
        }
    }
}

/// Forcefully terminates a process by pid — the honest hard-termination
/// fallback this test uses to simulate "the capsule is killed" (ADR 0041:
/// "the honest fallback is hard termination"), via the same OS tool/call
/// a real operator would reach for. Windows: `taskkill.exe /T` also kills
/// any child tree, matching the capsule's own containment job semantics
/// (nothing should be left dangling for the test's own cleanup to trip
/// over) -- Linux has no job object to mirror that with here (the
/// capsule's own kill DOMAIN is the process group, `killpg`, which this
/// test does not need: `SIGKILL` on the leader alone is exactly "the
/// capsule is killed", the scenario this simulates).
#[cfg(windows)]
fn taskkill(pid: u32) {
    let out = Command::new("taskkill.exe")
        .args(["/PID", &pid.to_string(), "/F", "/T"])
        .output()
        .expect("run taskkill.exe");
    assert!(out.status.success(), "taskkill failed for pid {pid}: {out:?}");
}
#[cfg(target_os = "linux")]
fn taskkill(pid: u32) {
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    assert_eq!(rc, 0, "kill(SIGKILL) failed for pid {pid}: {}", std::io::Error::last_os_error());
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

/// Every sealed frame across every `.sotseg` under a real
/// supervisor-owned voyage — `state_dir/voyages/<voyage>/seg`
/// (`supervisor::voyage_root_path`'s own convention; not the bespoke
/// per-test root `tests/e2e_pipe.rs`'s own harness uses, since THIS file
/// goes through the real supervisor rather than configuring
/// `capsule::CapsuleConfig` directly).
fn sealed_frames(state_dir: &Path, voyage: &str) -> Vec<sot_log::envelope::Envelope> {
    let seg_dir = state_dir.join("voyages").join(voyage).join("seg");
    let mut out = Vec::new();
    let mut names: Vec<String> = std::fs::read_dir(&seg_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for n in names {
        if n.ends_with(".sotseg") {
            let r = SegmentReader::read(&seg_dir.join(&n), true).unwrap();
            out.extend(r.frames);
        }
    }
    out
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

/// Ends the run cleanly (mirrors `tests/supervisor.rs`'s own
/// `end_run_and_expect_record_closed` + `poll_to_terminal` composition)
/// so the voyage is durably SEALED before `sealed_frames` reads it — an
/// in-progress segment's frames are written with `Commit::Immediate` but
/// this file only ever reads the same way every other test in this crate
/// does: after a clean shutdown.
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

mod pane;
mod headless;
mod supervisor_word;
