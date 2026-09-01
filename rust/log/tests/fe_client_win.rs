#![cfg(windows)]
//! ADR 0041 step 6 U3: real cross-process integration tests for
//! `sot_log::fe_client_win::FeAttachClient` — the FE attach-only client,
//! driven exactly the way the real frontend drives it, against a REAL
//! `sot-capsule supervise` and a REAL capsule leg. `tests/supervisor_win.rs`
//! already proves the supervisor's OWN lifecycle wiring across a real
//! process boundary; what THIS file adds is proof the CLIENT's own six
//! rulings (`fe_client`'s pure state machines) hold when driven by a real
//! reconnect-classified episode loop against real named pipes, not merely
//! scripted inputs.
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

use sot_log::fe_client_win::FeAttachClient;
use sot_log::pipe_win::{connect_voyage_pipe, PipeClient};
use sot_log::segment::SegmentReader;
use sot_log::supervisor::{connect_and_challenge_for_test, request_for_test, state_dir_hash};
use sot_log::wire::{
    MgmtReply, MgmtRequest, SupervisorOp, SupervisorOperationState, SupervisorPhase, SupervisorReply,
    SupervisorRequest,
};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn capsule_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sot-capsule"))
}

/// Reaps a spawned child on every exit path (a panicking assertion
/// included) — identical shape to `tests/supervisor_win.rs`'s own guard.
struct KillGuard(Option<Child>);
impl Drop for KillGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
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

fn wait_for_exit(mut child: Child, timeout: Duration) -> std::process::ExitStatus {
    poll_until(|| child.try_wait().unwrap(), timeout, "the supervisor process to exit")
}

/// Bounded poll for the lane to accept a connection AND answer the
/// challenge — `tests/supervisor_win.rs`'s own helper of the same name.
fn wait_for_lane(h: &str, timeout: Duration) -> PipeClient {
    poll_until(
        || connect_and_challenge_for_test(h).ok().map(|(conn, _process)| conn),
        timeout,
        "the supervisor lane to accept and answer the challenge",
    )
}

fn status(conn: &PipeClient) -> (Option<String>, Option<u64>, SupervisorPhase) {
    match request_for_test(conn, &SupervisorRequest::Status, Instant::now() + Duration::from_secs(5)).expect("status") {
        SupervisorReply::StatusOk { voyage, leg, phase, .. } => (voyage, leg, phase),
        other => panic!("expected StatusOk, got {other:?}"),
    }
}

fn wait_for_ready(conn: &PipeClient, timeout: Duration) -> (String, u64) {
    poll_until(
        || match status(conn) {
            (Some(voyage), Some(leg), SupervisorPhase::Ready) => Some((voyage, leg)),
            _ => None,
        },
        timeout,
        "the leg to reach phase Ready",
    )
}

fn command(conn: &PipeClient, operation_id: &str, op: SupervisorOp) -> SupervisorOperationState {
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

/// The capsule's OWN pid, read off the voyage pipe's mgmt sub-lane
/// (`probe`/`status`/`shutdown` — the step-5 lane, distinct from the
/// supervisor lane above). A throwaway connection: the mgmt lane accepts
/// unrelated probe/status connections freely alongside an already-attached
/// watcher (this test's own `FeAttachClient`), per step 5's design.
fn capsule_pid(voyage: &str) -> u32 {
    let conn = connect_voyage_pipe(voyage).expect("connect voyage pipe for mgmt status");
    let bytes = sot_log::wire::encode_mgmt_request(&MgmtRequest::Status).unwrap();
    conn.write_all(&bytes).unwrap();
    let mut splitter = sot_log::wire::FrameSplitter::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = conn.read(&mut buf).expect("read mgmt status_ok");
        assert!(n > 0, "unexpected EOF waiting for mgmt status_ok");
        let (frames, err) = splitter.feed(&buf[..n]);
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
/// "the honest fallback is hard termination"), via the same OS tool a real
/// operator would reach for. `/T` also kills any child tree, matching the
/// capsule's own containment job semantics (nothing should be left
/// dangling for the test's own cleanup to trip over).
fn taskkill(pid: u32) {
    let out = Command::new("taskkill.exe")
        .args(["/PID", &pid.to_string(), "/F", "/T"])
        .output()
        .expect("run taskkill.exe");
    assert!(out.status.success(), "taskkill failed for pid {pid}: {out:?}");
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
/// `CapsuleWinConfig` directly).
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

fn query(conn: &PipeClient, operation_id: &str) -> SupervisorOperationState {
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

/// Ends the run cleanly (mirrors `tests/supervisor_win.rs`'s own
/// `end_run_and_expect_record_closed` + `poll_to_terminal` composition)
/// so the voyage is durably SEALED before `sealed_frames` reads it — an
/// in-progress segment's frames are written with `Commit::Immediate` but
/// this file only ever reads the same way every other test in this crate
/// does: after a clean shutdown.
fn end_run_and_wait_verified(conn: &PipeClient, voyage: &str) {
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
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]);
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
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]);
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
// Ruling: end_run from the quit dispatcher gets record_closed
// -----------------------------------------------------------------------

#[test]
fn end_run_from_the_quit_dispatcher_gets_record_closed() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]);
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
    while Instant::now() < deadline {
        client.pump();
        if client.should_exit() {
            exited = true;
            break;
        }
        assert_ne!(
            client.quit_message(),
            Some("ending the session did not complete \u{2014} outcome unknown"),
            "the quit dispatcher timed out instead of observing record_closed"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(exited, "quit dispatcher never reached should_exit (record_closed) within 60s");

    // Independent corroboration, over a SEPARATE connection: the
    // supervisor itself is now serving ENDED-NO-RESPAWN, exactly what a
    // `record_closed` end_run leaves behind (ADR 0041 Lifecycle: "An
    // ended authority stays serviceable").
    let (_v, _l, phase) = status(&conn);
    assert_eq!(phase, SupervisorPhase::EndedNoRespawn);

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
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let h = state_dir_hash(&state_dir);

    let child = spawn_supervisor(&state_dir, "--start", &["cmd.exe"]);
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
    let child2 = spawn_supervisor(&state_dir, "--resume", &["cmd.exe"]);
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
