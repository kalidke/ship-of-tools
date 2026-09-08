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
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// Reaps a spawned child on every exit path (a panicking assertion
/// included) — identical shape to `tests/supervisor.rs`'s own guard.
/// Codex review round, finding 14: the wait after `kill()` is bounded by
/// the SAME `poll_until` every other wait in this file uses, rather than
/// an unbounded `Child::wait()` — a `Drop` that could itself hang would
/// turn one failing test into a wedged whole binary.
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

/// [`spawn_supervisor`] with an explicit initial pty size — ADR 0042
/// amendment: proves a headless client adopts whatever geometry the
/// capsule actually has, rather than the client's own placeholder.
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

fn wait_for_exit(mut child: Child, timeout: Duration) -> std::process::ExitStatus {
    poll_until(|| child.try_wait().unwrap(), timeout, "the supervisor process to exit")
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

// -----------------------------------------------------------------------
// Ruling: attach as a watcher and receive the checkpoint
// -----------------------------------------------------------------------

#[test]
fn attach_as_watcher_receives_the_checkpoint() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", SHELL);
    let mut guard = KillGuard(Some(child));
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let (woke, wake) = wake_flag();
    let mut client = FeAttachClient::attach(
        state_dir.clone(),
        80,
        24,
        "fe-client-win-test-a".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");

    let banner = poll_screen(&mut client, Duration::from_secs(30), |t| t.trim().chars().any(|c| !c.is_whitespace()));
    assert!(banner, "no checkpoint content ever reached the client's screen (dead={}, status={})", client.is_dead(), client.status_line());
    assert!(!client.is_dead());
    assert!(woke.load(Ordering::Relaxed), "wake() was never called");

    end_run_and_wait_verified(&conn, &voyage);
    let _ = command(&conn, "test-a-stop", SupervisorOp::Stop);
    let child = guard.0.take().unwrap();
    wait_for_exit(child, Duration::from_secs(30));
}

// -----------------------------------------------------------------------
// Ruling: first input takes the pen and the resize precedes the flush
// -----------------------------------------------------------------------

#[test]
fn first_input_takes_the_pen_and_resize_precedes_the_flush() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", SHELL);
    let mut guard = KillGuard(Some(child));
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let (_woke, wake) = wake_flag();
    let mut client = FeAttachClient::attach(
        state_dir.clone(),
        80,
        24,
        "fe-client-win-test-b".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");

    assert!(
        poll_screen(&mut client, Duration::from_secs(30), |t| t.trim().chars().any(|c| !c.is_whitespace())),
        "no checkpoint content ever reached the client's screen"
    );

    // First input while WATCHING: enters the take transaction, sends
    // `take`, and on `take_ok` sends `resize` FIRST, then flushes this
    // exact payload as the ONE `input` frame.
    let marker: &[u8] = b"echo SOT_FE_MARKER\r\n";
    client.send_input(marker);

    let found = poll_screen(&mut client, Duration::from_secs(30), |t| t.contains("SOT_FE_MARKER"));
    assert!(found, "input never reached the shell (dead={}, status={})", client.is_dead(), client.status_line());

    // Ruling (b): resize precedes the flush -- proven at the wire level
    // via the sealed voyage record (the client's own public surface has
    // no other way to observe SEND order). `Class::Input`'s payload
    // REDACTS content, so the match is by exact byte length -- unique in
    // this run since no other command of this length is ever sent.
    drop(client);
    end_run_and_wait_verified(&conn, &voyage);

    let frames = sealed_frames(&state_dir, &voyage);
    let resize_seq = frames
        .iter()
        .find_map(|f| {
            if f.class != sot_log::envelope::Class::ControlExchange {
                return None;
            }
            let p = f.payload.as_ref()?;
            if p.get("phase")?.as_str()? == "request" && p.get("kind_ns")?.as_str()? == "conpty/resize" {
                Some(f.seq.n)
            } else {
                None
            }
        })
        .expect("no resize control_exchange request frame found in the sealed voyage");
    let input_seq = frames
        .iter()
        .find_map(|f| {
            if f.class != sot_log::envelope::Class::Input {
                return None;
            }
            let p = f.payload.as_ref()?;
            if p.get("length")?.as_u64()? == marker.len() as u64 {
                Some(f.seq.n)
            } else {
                None
            }
        })
        .expect("no matching input frame found in the sealed voyage");
    assert!(
        resize_seq < input_seq,
        "resize (seq {resize_seq}) must precede the flushed input (seq {input_seq})"
    );

    let _ = command(&conn, "test-b-stop", SupervisorOp::Stop);
    let child = guard.0.take().unwrap();
    wait_for_exit(child, Duration::from_secs(30));
}

// -----------------------------------------------------------------------
// Ruling: end_run from the quit dispatcher reaches record_verified
// -----------------------------------------------------------------------

/// Codex review round, finding 1 + finding 14: `should_exit()` is now
/// gated on `record_verified`, not merely `record_closed` (the original
/// bug: the dispatcher exited the instant the command's own DEFERRED
/// reply arrived, without ever querying for verification) — so THIS
/// TEST's own `exited` assertion below is itself the client-visible
/// proof of `record_verified`, not just `record_closed`.
#[test]
fn end_run_from_the_quit_dispatcher_reaches_client_visible_record_verified() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", SHELL);
    let mut guard = KillGuard(Some(child));
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (_voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let (_woke, wake) = wake_flag();
    let mut client = FeAttachClient::attach(
        state_dir.clone(),
        80,
        24,
        "fe-client-win-test-c".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");
    assert!(
        poll_screen(&mut client, Duration::from_secs(30), |t| t.trim().chars().any(|c| !c.is_whitespace())),
        "no checkpoint content ever reached the client's screen"
    );

    client.request_quit("integration test quit");

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut exited = false;
    let mut last_msg: Option<String> = None;
    while Instant::now() < deadline {
        client.pump();
        if client.should_exit() {
            exited = true;
            break;
        }
        let msg: Option<String> = client.quit_message().map(|m| m.to_string());
        if msg != last_msg {
            eprintln!("[quit test] quit message: {msg:?}");
            last_msg = msg.clone();
        }
        assert_ne!(
            msg.as_deref(),
            Some("ending the session did not complete \u{2014} outcome unknown"),
            "the quit dispatcher timed out instead of observing record_closed"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    if !exited {
        // Codex review round (evidence from 285ad0d9's real-Windows run):
        // `conn` (opened before the 60s quit-wait loop, never itself used
        // during it) can ALSO have idled out under the supervisor lane's
        // own 5s deadline by now — using it here panicked INSIDE the
        // diagnostic, hiding the real failure. A FRESH connection (ruling
        // 4) is the honest way to ask the authority what it thinks right
        // now; `try_status` (never panics) keeps this path from replacing
        // the real panic message with a connection error instead.
        let diag = wait_for_lane(&h, Duration::from_secs(10));
        panic!(
            "quit dispatcher never reached should_exit (client-visible record_verified) within 60s; \
             quit message: {:?}; authority status (voyage, leg, phase): {:?}",
            client.quit_message(),
            try_status(&diag)
        );
    }

    // Independent corroboration: the supervisor ends up serving
    // ENDED-NO-RESPAWN, exactly what a `record_closed` end_run leaves
    // behind (ADR 0041 Lifecycle: "An ended authority stays
    // serviceable"). A FRESH connection (ruling 4, same reasoning as the
    // diagnostic above) — `conn` has been idle since before the quit was
    // even requested. POLLED, not asserted at once: the lane replies
    // `record_closed` the moment the record is closed (B3, the deferred
    // reply) while the authority is still ENDING — verification
    // (`record_verified`) and the phase transition land after it. First
    // real-Windows run caught exactly that: Ending.
    let conn = wait_for_lane(&h, Duration::from_secs(10));
    let corroborated = {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let (_v, _l, phase) = status(&conn);
            if phase == SupervisorPhase::EndedNoRespawn {
                break true;
            }
            if Instant::now() >= deadline {
                eprintln!("[quit test] last phase before giving up: {phase:?}");
                break false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    assert!(corroborated, "the authority never reached EndedNoRespawn after record_closed");

    let _ = command(&conn, "test-c-stop", SupervisorOp::Stop);
    let child = guard.0.take().unwrap();
    wait_for_exit(child, Duration::from_secs(30));
}

// -----------------------------------------------------------------------
// Ruling: reconnect after the capsule is killed and a fresh supervisor
// takes over restores the screen from the new checkpoint
// -----------------------------------------------------------------------

#[test]
fn reconnect_after_the_capsule_is_killed_restores_the_screen_from_the_new_checkpoint() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", SHELL);
    let mut guard1 = KillGuard(Some(child));
    let conn1 = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn1, Duration::from_secs(90));

    let (_woke, wake) = wake_flag();
    let mut client = FeAttachClient::attach(
        state_dir.clone(),
        80,
        24,
        "fe-client-win-test-d".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");
    assert!(
        poll_screen(&mut client, Duration::from_secs(30), |t| t.trim().chars().any(|c| !c.is_whitespace())),
        "no checkpoint content ever reached the client's screen before the kill"
    );
    // A distinctive marker in the FIRST leg's screen, so the later
    // assertion can tell "still showing the old screen" apart from "a
    // genuinely fresh checkpoint arrived" -- proving RESTORE, not mere
    // silence.
    client.send_input(b"echo SOT_FE_OLD_LEG\r\n");
    assert!(
        poll_screen(&mut client, Duration::from_secs(30), |t| t.contains("SOT_FE_OLD_LEG")),
        "first-leg marker never reached the screen"
    );

    // "The capsule is killed": hard-terminate the capsule process
    // directly (learned via the voyage pipe's own mgmt status, the
    // honest hard-termination fallback the ADR itself names), then kill
    // the now-orphaned supervisor too so what comes next is genuinely a
    // FRESH supervisor process, not the same one respawning its own
    // child.
    let pid = capsule_pid(&voyage);
    taskkill(pid);
    if let Some(mut c) = guard1.0.take() {
        let _ = c.kill();
        let _ = c.wait();
    }

    // A fresh supervisor, `--resume` against the SAME state dir: no live
    // capsule survives to adopt, so it spawns a fresh leg under the SAME
    // (already-published, unchanged) voyage pointer.
    let child2 = spawn_supervisor(&state_dir, "--resume", SHELL);
    let mut guard2 = KillGuard(Some(child2));
    let conn2 = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage2, _leg2) = wait_for_ready(&conn2, Duration::from_secs(90));
    assert_eq!(voyage2, voyage, "a fresh spawn under --resume must keep the SAME voyage pointer");

    // The client's own reconnect episode (ruling d) notices the dropped
    // attach connection, re-reads the pointer, reconnects the
    // supervisor lane, and re-attaches -- restoring the screen from the
    // NEW leg's checkpoint. Bounded generously: real backoff (250ms
    // doubling to 4s) plus a real second capsule spawn both cost real
    // wall time here.
    let fresh = poll_screen(&mut client, Duration::from_secs(120), |t| {
        t.trim().chars().any(|c| !c.is_whitespace()) && !t.contains("SOT_FE_OLD_LEG")
    });
    assert!(
        fresh,
        "the client never restored a fresh checkpoint after reconnect (dead={}, status={})",
        client.is_dead(),
        client.status_line()
    );
    assert!(!client.is_dead());

    drop(client);
    end_run_and_wait_verified(&conn2, &voyage2);
    let _ = command(&conn2, "test-d-stop", SupervisorOp::Stop);
    let child2 = guard2.0.take().unwrap();
    wait_for_exit(child2, Duration::from_secs(30));
}

// -----------------------------------------------------------------------
// ADR 0042 amendment (2026-09-07): the HEADLESS client (`attach_headless`)
// — the daemon's own `pty.input`/`pty.screen` attach, never the frontend's.
// -----------------------------------------------------------------------

/// `type_into`'s own core mechanism, driven directly at the client level
/// (the wrapper this proves lives in `sot-backend`'s `capsule_workspace::
/// headless`, which has no dependency on this crate's test harness): a
/// headless attach adopts the CAPSULE's own geometry (never the
/// placeholder it was constructed with), delivers one input frame, and
/// sends NO resize — proven at the wire level via the sealed voyage
/// record, the same way `first_input_takes_the_pen_and_resize_precedes_
/// the_flush` above proves resize DOES precede the flush for an ordinary
/// client.
#[test]
fn headless_attach_adopts_capsule_geometry_and_types_without_resizing() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    // A capsule sized other than the client's own 24x24 placeholder AND
    // other than the common 80x24 default, so an assertion that the
    // client ends up at (120, 40) cannot pass by coincidence.
    let child = spawn_supervisor_sized(&state_dir, "--start", 120, 40, SHELL);
    let mut guard = KillGuard(Some(child));
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    let mut client = FeAttachClient::<PlatformEndpoint>::attach_headless(state_dir.clone(), "lu6c-headless-test".to_string())
        .expect("attach_headless");

    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "client died before a checkpoint arrived: {}", client.status_line());
        assert!(Instant::now() < deadline, "timed out waiting for the first checkpoint");
        std::thread::sleep(Duration::from_millis(20));
    }
    let (rows, cols) = client.screen().size();
    assert_eq!((cols, rows), (120, 40), "a headless client must adopt the CAPSULE's own geometry");

    let marker: &[u8] = b"echo SOT_LU6C_HEADLESS_MARKER\r";
    let before = client.recorded_bytes();
    client.send_input(marker);

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        client.pump();
        if let Some(outcome) = client.last_input_outcome() {
            assert_eq!(outcome, InputOutcome::Recorded, "headless input must be recorded, not refused/unknown");
            break;
        }
        assert!(!client.is_dead(), "client died before the input was recorded: {}", client.status_line());
        assert!(Instant::now() < deadline, "timed out waiting for InputRecorded");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(client.recorded_bytes() - before, marker.len() as u64, "the WHOLE payload must be recorded, not a partial ack");

    assert!(client.shutdown(Duration::from_secs(5)), "the worker must exit within the shutdown bound");

    end_run_and_wait_verified(&conn, &voyage);

    let frames = sealed_frames(&state_dir, &voyage);
    let resize_from_headless = frames.iter().any(|f| {
        if f.class != sot_log::envelope::Class::ControlExchange {
            return false;
        }
        let Some(p) = f.payload.as_ref() else { return false };
        p.get("phase").and_then(|v| v.as_str()) == Some("request")
            && p.get("kind_ns").and_then(|v| v.as_str()) == Some("conpty/resize")
    });
    assert!(!resize_from_headless, "a headless client must send NO resize, ever — found one in the sealed record");

    let input_frame = frames
        .iter()
        .find(|f| {
            f.class == sot_log::envelope::Class::Input
                && f.source.actor.controller_id.as_deref() == Some("lu6c-headless-test")
        })
        .expect("no input frame attributed to the headless controller_id in the sealed record");
    assert_eq!(
        input_frame.payload.as_ref().and_then(|p| p.get("length")).and_then(|v| v.as_u64()),
        Some(marker.len() as u64),
        "the sealed input frame's length must match the whole payload"
    );

    let _ = command(&conn, "test-headless-a-stop", SupervisorOp::Stop);
    let child = guard.0.take().unwrap();
    wait_for_exit(child, Duration::from_secs(30));
}

/// `screen_of`'s own core mechanism: a WATCHER attach reads the checkpoint
/// and NEVER takes — proven by attaching a SECOND (ordinary) client
/// afterwards and confirming its first input still gets `take_ok`
/// immediately (the pen was free the whole time the headless read ran).
#[test]
fn headless_screen_read_never_takes_the_pen() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", SHELL);
    let mut guard = KillGuard(Some(child));
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    // The headless watcher: attach, wait for the checkpoint, then drop —
    // exactly `screen_of`'s own shape, minus the wrapper.
    {
        let mut watcher = FeAttachClient::<PlatformEndpoint>::attach_headless(state_dir.clone(), "lu6c-headless-watcher".to_string())
            .expect("attach_headless");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !watcher.is_checkpointed() {
            watcher.pump();
            assert!(!watcher.is_dead(), "watcher died before a checkpoint arrived: {}", watcher.status_line());
            assert!(Instant::now() < deadline, "timed out waiting for the watcher's checkpoint");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(watcher.shutdown(Duration::from_secs(5)), "the watcher's worker must exit within the bound");
    }

    // A fresh, ORDINARY client's first input must reach DRIVING via a
    // normal, uncontested take_ok — if the watcher above had taken the
    // pen and never released it, this would stall waiting for `take_ok`.
    let (_woke, wake) = wake_flag();
    let mut client = FeAttachClient::attach(
        state_dir.clone(),
        80,
        24,
        "lu6c-post-watcher-driver".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");
    assert!(
        poll_screen(&mut client, Duration::from_secs(30), |t| t.trim().chars().any(|c| !c.is_whitespace())),
        "no checkpoint content ever reached the second client's screen"
    );
    let marker: &[u8] = b"echo SOT_LU6C_POST_WATCHER\r\n";
    client.send_input(marker);
    let found = poll_screen(&mut client, Duration::from_secs(30), |t| t.contains("SOT_LU6C_POST_WATCHER"));
    assert!(found, "the pen was not free after the headless screen read (dead={}, status={})", client.is_dead(), client.status_line());

    drop(client);
    end_run_and_wait_verified(&conn, &voyage);
    let _ = command(&conn, "test-headless-b-stop", SupervisorOp::Stop);
    let child = guard.0.take().unwrap();
    wait_for_exit(child, Duration::from_secs(30));
}

/// Decision 28 (LU6b, attach convergence): a state dir with NO supervisor
/// ever run has no `drawer.voyage` pointer and no supervisor lane, and the
/// worker invents NO cutoff of its own for that -- the frontend's create
/// path starts this same client before the supervisor has bound its lane,
/// so "nothing there yet" is a wait ("supervisor lane not answering --
/// retrying"), bounded only by the health window. What the headless
/// callers (`type_into`/`screen_of`, each with its own deadline + dead
/// check) rely on instead is proven here: `is_checkpointed()` never flips
/// while the worker is parked in that convergence wait, and a shutdown
/// closes the worker within its bound from inside the wait.
#[test]
fn headless_attach_against_a_pointerless_state_dir_waits_and_shutdown_closes_the_worker() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state-with-no-supervisor-ever-run");
    std::fs::create_dir_all(&state_dir).unwrap();

    let mut client = FeAttachClient::<PlatformEndpoint>::attach_headless(state_dir, "lu6c-headless-deadline-test".to_string())
        .expect("attach_headless (the constructor itself never touches the network)");

    // A spell long enough for several fail-fast connect rounds and their
    // backoffs; the client must still be waiting, not dead, not checkpointed.
    let spell = Instant::now() + Duration::from_secs(2);
    while Instant::now() < spell {
        client.pump();
        assert!(!client.is_dead(), "a pointerless state dir is a wait, never a terminal client (status={})", client.status_line());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!client.is_checkpointed(), "no checkpoint can exist without a supervisor");
    assert!(
        client.status_line().contains("not answering"),
        "the wait must be the designed one (supervisor lane not answering), got: {}",
        client.status_line()
    );
    assert!(
        client.shutdown(Duration::from_secs(5)),
        "the worker must be observed closed within the shutdown bound from inside the convergence wait"
    );
}

/// Contention proof (brief §5(a), sequenced rather than raced — this
/// harness has no way to pin the interleaving at a specific frame
/// boundary, so the two orderings this test CAN force are: attach and
/// establish DRIVER-A first, THEN run the headless write, THEN send
/// driver-A's next keystroke): a headless `type_into` landing on a row an
/// ordinary client is already DRIVING must demote that driver (ADR 0041's
/// own take-epoch lattice — any `take` succeeds and demotes whoever held
/// the pen) — the headless write is delivered, driver-A's NEXT input sees
/// `input_refused_stale` and its own worker retakes AUTOMATICALLY and
/// invisibly to this test (ruling (c)), and the sealed record shows
/// EXACTLY ONE input frame per controller — no duplication, no silently
/// dropped keystroke on either side.
#[test]
fn headless_write_while_a_client_is_driving_demotes_it_without_duplicating_input() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", SHELL);
    let mut guard = KillGuard(Some(child));
    let conn = wait_for_lane(&h, Duration::from_secs(30));
    let (voyage, _leg) = wait_for_ready(&conn, Duration::from_secs(90));

    // 1. Driver A attaches and becomes DRIVING.
    let (_woke, wake) = wake_flag();
    let mut driver_a = FeAttachClient::attach(
        state_dir.clone(),
        80,
        24,
        "lu6c-driver-a".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach driver_a");
    assert!(
        poll_screen(&mut driver_a, Duration::from_secs(30), |t| t.trim().chars().any(|c| !c.is_whitespace())),
        "driver_a never saw a checkpoint"
    );
    driver_a.send_input(b"echo A1\r\n");
    assert!(
        poll_screen(&mut driver_a, Duration::from_secs(30), |t| t.contains("A1")),
        "driver_a's first input never landed (dead={}, status={})", driver_a.is_dead(), driver_a.status_line()
    );

    // 2. A headless write lands on the SAME row while driver_a still holds
    // the pen — must demote driver_a and succeed on its own.
    let mut headless = FeAttachClient::<PlatformEndpoint>::attach_headless(state_dir.clone(), "lu6c-headless-b".to_string())
        .expect("attach_headless");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !headless.is_checkpointed() {
        headless.pump();
        assert!(!headless.is_dead(), "headless died before a checkpoint: {}", headless.status_line());
        assert!(Instant::now() < deadline, "headless never reached a checkpoint");
        std::thread::sleep(Duration::from_millis(20));
    }
    headless.send_input(b"echo HEADLESSB\r");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        headless.pump();
        if let Some(outcome) = headless.last_input_outcome() {
            assert_eq!(outcome, InputOutcome::Recorded, "headless write while driver_a was driving must still be recorded (it demotes, not refuses, the prior driver)");
            break;
        }
        assert!(!headless.is_dead(), "headless died before its outcome: {}", headless.status_line());
        assert!(Instant::now() < deadline, "headless write timed out while driver_a was driving");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(headless.shutdown(Duration::from_secs(5)), "headless worker must close within the bound");

    // 3. driver_a's NEXT input: its own worker sees `input_refused_stale`,
    // re-takes automatically (ruling (c)), and this keystroke still lands
    // — entirely inside fe_client_io, invisible to this test except for
    // the eventual echo.
    driver_a.send_input(b"echo A2RETAKE\r\n");
    assert!(
        poll_screen(&mut driver_a, Duration::from_secs(30), |t| t.contains("A2RETAKE")),
        "driver_a's post-demotion retake never delivered its keystroke (dead={}, status={})", driver_a.is_dead(), driver_a.status_line()
    );

    drop(driver_a);
    end_run_and_wait_verified(&conn, &voyage);

    // 4. The sealed record: exactly ONE input frame per controller_id —
    // the headless write was not duplicated, and driver_a's two inputs
    // (before and after the demotion) were not merged, dropped, or
    // multiplied by the automatic retake.
    let frames = sealed_frames(&state_dir, &voyage);
    let count_for = |cid: &str| {
        frames
            .iter()
            .filter(|f| {
                f.class == sot_log::envelope::Class::Input
                    && f.source.actor.controller_id.as_deref() == Some(cid)
            })
            .count()
    };
    assert_eq!(count_for("lu6c-headless-b"), 1, "the headless write must appear EXACTLY once in the sealed record");
    // `capsule.rs`'s own `run_input_wal` commits a durable `Class::Input`
    // frame on EVERY fresh idem_key BEFORE deciding stale-or-not ("input is
    // durably logged before the producer sees it", ADR 0039) — so
    // driver_a's post-demotion keystroke is not a single logged attempt:
    // its own worker's automatic retake (ruling (c)) mints a NEW idem_key,
    // which is a SECOND durably-logged `Class::Input` frame. Three, not
    // two: A1 (clean) + A2RETAKE's own refused-stale attempt + A2RETAKE's
    // successful retry. What must NEVER happen — a drop (fewer than 3) or
    // an actual double-FORWARD of the same bytes to the pty — is what the
    // `refused_stale_epoch` lifecycle fact below independently confirms:
    // exactly one of these three is refused.
    assert_eq!(
        count_for("lu6c-driver-a"), 3,
        "driver_a's record: A1 (clean) + A2RETAKE's refused-stale attempt + A2RETAKE's successful retry"
    );
    let refused_stale_count = frames
        .iter()
        .filter(|f| {
            f.class == sot_log::envelope::Class::Lifecycle
                && f.source.actor.controller_id.as_deref() == Some("lu6c-driver-a")
                && f.payload.as_ref().and_then(|p| p.get("fact")?.get("fact")?.as_str()) == Some("refused_stale_epoch")
        })
        .count();
    assert_eq!(refused_stale_count, 1, "exactly ONE of driver_a's attempts must be the refused-stale one the demotion causes");

    let _ = command(&conn, "test-headless-c-stop", SupervisorOp::Stop);
    let child = guard.0.take().unwrap();
    wait_for_exit(child, Duration::from_secs(30));
}

#[cfg(target_os = "linux")]
fn poll_until_checkpointed_collecting_statuses(
    client: &mut FeAttachClient,
    timeout: Duration,
) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    let mut statuses: Vec<String> = Vec::new();
    loop {
        client.pump();
        let s = client.status_line().to_string();
        if statuses.last() != Some(&s) {
            statuses.push(s);
        }
        if client.is_checkpointed() {
            return statuses;
        }
        assert!(
            !client.is_dead(),
            "client died before ever checkpointing (status={}, statuses seen={statuses:?})",
            client.status_line()
        );
        assert!(
            Instant::now() < deadline,
            "timed out waiting for a checkpoint (status={}, statuses seen={statuses:?})",
            client.status_line()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[cfg(target_os = "linux")]
fn attach_converges_on_the_supervisors_word() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let started = Instant::now();
    let child = spawn_supervisor(&state_dir, "--start", &["/bin/sh", "-c", "sleep 60"]);
    let mut guard = KillGuard(Some(child));

    let (_woke, wake) = wake_flag();
    let mut client = FeAttachClient::attach(
        state_dir.clone(),
        80,
        24,
        "fe-client-lu6b-a".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");

    let statuses = poll_until_checkpointed_collecting_statuses(&mut client, Duration::from_secs(30));
    let elapsed = started.elapsed();
    println!("LU6b attach_converges_on_the_supervisors_word: spawn->Checkpoint = {elapsed:?}");
    eprintln!("LU6b attach_converges_on_the_supervisors_word: statuses seen = {statuses:?}");

    assert!(
        !statuses.iter().any(|s| s.contains("voyage pipe unreachable")),
        "the OLD dead-pipe-retry status text must never appear; saw {statuses:?}"
    );

    // DEVIATION (reported per the brief's own instruction): "the supervisor
    // lane saw ONE connection from the client" has no observable surface
    // today -- `StatusOk` carries no connection count, and
    // `spawn_supervisor`'s inherited stdout/stderr carries no per-
    // connection accept log line (`run_worker`/`supervisor.rs`'s lane
    // handling logs nothing on accept). Dropped, as the brief's own
    // fallback instructs.
    drop(client);
    let mut child = guard.0.take().unwrap();
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
#[cfg(target_os = "linux")]
fn quit_is_dispatched_before_ready() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    // A slow producer (brief's own suggested shape) -- belt and braces
    // against a loaded runner, though a real measurement (LU6b's own
    // report) found Ready is reached via the LEG's own bind, independent
    // of the producer's behavior (~600ms either way): the real margin
    // this test relies on is `request_quit` being called at time ~0,
    // before the worker thread's first network round trip even starts.
    let child = spawn_supervisor(&state_dir, "--start", &["/bin/sh", "-c", "sleep 3; sleep 60"]);
    let mut guard = KillGuard(Some(child));

    let (_woke, wake) = wake_flag();
    let mut client: FeAttachClient = FeAttachClient::attach(
        state_dir.clone(),
        80,
        24,
        "fe-client-lu6b-b".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");
    client.request_quit("lu6b test: quit before ready");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut quit_seen = false;
    loop {
        client.pump();
        if client.quit_message().is_some() {
            quit_seen = true;
        }
        assert!(
            !client.is_checkpointed(),
            "a Checkpoint must never arrive once a quit was latched before any attach completed"
        );
        if client.should_exit() || quit_seen {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the latched quit to be dispatched (status={}, quit={:?})",
            client.status_line(),
            client.quit_message()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(quit_seen, "expected a QuitMessage to appear before this loop exited");
    assert!(!client.is_checkpointed(), "a quit-before-ready session must never reach a checkpoint");

    // The brief's own acceptance criterion is exactly what the loop above
    // proved: the quit reaches the supervisor (a QuitMessage) before any
    // Checkpoint. What the supervisor DOES with an EndRun requested this
    // early is its own call, not this test's: dispatched before the leg
    // is registered as "currently running" (a real, observed outcome —
    // `Failed { detail: "no leg is currently running" }` — this exact
    // timing produced once already), the worker simply carries on toward
    // the ordinary Ready/Checkpoint path with the quit now a settled,
    // non-blocking `Failed` state (never a hang, never a second EndRun);
    // dispatched slightly later, it can just as legitimately succeed.
    // Either way this test's property already holds, so teardown here is
    // a plain kill rather than negotiating a specific quit outcome.
    drop(client);
    let mut child = guard.0.take().unwrap();
    let _ = child.kill();
    let _ = child.wait();
}

/// Codex review round finding 8: the predecessor version of this test
/// only ever watched 15 of the 120s `HEALTH_WINDOW` -- proving the
/// window does not expire EARLY, but never proving it expires AT ALL.
/// This version runs the window to its real end (a genuinely slow test,
/// deliberately: `HEALTH_WINDOW` is a wall-clock constant, and expiry is
/// exactly the fact worth proving against the real clock, not a
/// shortened stand-in for it) and asserts the client reaches `Terminal`
/// with the `HealthWindowExpired` reason once it does, having stayed
/// alive for the entire slow half.
#[test]
#[cfg(target_os = "linux")]
fn unresponsive_supervisor_expires_the_health_window() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let _server = sot_log::socket_unix::SocketServer::bind_supervisor(&h, 1).expect("bind a bare supervisor socket");

    let (_woke, wake) = wake_flag();
    let mut client: FeAttachClient = FeAttachClient::attach(
        state_dir.clone(),
        80,
        24,
        "fe-client-lu6b-c".to_string(),
        "test-handle".to_string(),
        None,
        wake,
    )
    .expect("attach");

    // Well inside the window: proves it does not expire early, exactly
    // as the predecessor test did.
    let mid_deadline = Instant::now() + Duration::from_secs(15);
    let mut saw_retry_status = false;
    loop {
        client.pump();
        let s = client.status_line().to_string();
        if s.to_lowercase().contains("not answering") {
            saw_retry_status = true;
        }
        assert!(!client.is_checkpointed(), "an unresponsive supervisor lane must never yield a checkpoint");
        assert!(
            !client.is_dead(),
            "must not reach Terminal well inside the 120s health window (status={s})"
        );
        if Instant::now() >= mid_deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        saw_retry_status,
        "expected the health-window retry status to appear at least once; last status = {}",
        client.status_line()
    );
    assert!(!client.is_dead(), "must not be Terminal after only a small slice of the 120s health window");

    // Past the window's own end: a generous margin beyond the constant
    // itself, so the wait is a proof of "it DOES expire," never a tight
    // race against `HEALTH_WINDOW`'s exact edge.
    let expiry_deadline = Instant::now() + sot_log::fe_client::HEALTH_WINDOW + Duration::from_secs(30);
    loop {
        client.pump();
        if client.is_dead() {
            break;
        }
        assert!(
            Instant::now() < expiry_deadline,
            "expected the client to reach Terminal once the health window truly expired (status={})",
            client.status_line()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        client.status_line().to_lowercase().contains("healthwindowexpired"),
        "expected the health-window's OWN expiry reason, got status={}",
        client.status_line()
    );
}
