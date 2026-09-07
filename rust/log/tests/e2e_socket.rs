#![cfg(target_os = "linux")]
//! End-to-end test for the Unix socket transport (ADR 0043 "Decisions for
//! LU2" LU2b) — the twin of `tests/e2e_pipe.rs`, over a REAL Unix domain
//! socket instead of a REAL named pipe. `tests/capsule.rs` proves the
//! writer loop and `AttachProto` against a synthetic `TestTransport` (now
//! against `PtyProducer` too, on Linux); this file is the one place both
//! are proven together with a REAL transport:
//! `socket_transport::SocketTransport` wrapping a real
//! `socket_unix::SocketServer`, with real OS clients connecting via
//! `socket_unix::connect_voyage_socket` — a watcher, a driver, and a mgmt
//! connection, all against the SAME running capsule.
//!
//! `target_os = "linux"` (not bare `unix`): `connect_voyage_socket`'s own
//! challenge (`challenge_unix::authenticate_server`) is Linux-only (ADR
//! 0043 decision 8) — other Unix fails closed there, matching
//! `tests/challenge_unix.rs`'s own gate.

use sot_log::capsule::{self, CapsuleConfig, ExitKind};
use sot_log::producer_pty::PtyProducer;
use sot_log::segment::{RetentionClass, SegmentReader};
use sot_log::socket_transport::SocketTransport;
use sot_log::socket_unix::{connect_voyage_socket, SocketClient};
use sot_log::verify::verify_voyage;
use sot_log::wire::{self, Survival};
use sot_log::{Class, Envelope, RefKind};
use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// Serializes every test in this file. Two independent reasons, both
/// already precedented elsewhere in this crate: every test here mutates
/// the shared, process-global `SOT_RUNTIME_DIR` env var (see
/// `isolated_runtime_dir` below, copied from `tests/socket_unix.rs`'s and
/// `tests/challenge_unix.rs`'s identical helper) — unsafe to interleave
/// across threads in the SAME process — and, like `tests/capsule.rs`'s
/// own `SERIAL`, two real-pty-plus-real-socket tests could otherwise
/// starve each other on a loaded CI runner.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Points `SOT_RUNTIME_DIR` at a fresh, mode-0700 tempdir under `/tmp` for
/// the lifetime of the returned guard — mirrors `tests/socket_unix.rs`'s
/// and `tests/challenge_unix.rs`'s identical helper (never the default
/// `$TMPDIR`: same rationale, one copy-paste source of truth). Every test
/// in this file holds `serial()` for its own whole duration before
/// calling this, so no two tests' env mutations can ever interleave.
struct RuntimeDirGuard {
    tmp: tempfile::TempDir,
}
impl RuntimeDirGuard {
    fn path(&self) -> &std::path::Path {
        self.tmp.path()
    }
}
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
    RuntimeDirGuard { tmp }
}

/// A fresh, canonical lowercase-hyphenated UUID — `socket_unix::SocketServer::bind`
/// (reached through `SocketTransport::bind`, called by `run` itself)
/// validates the voyage id as exactly this shape before it will ever
/// create the socket.
fn fresh_voyage_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn config(dir: &std::path::Path, voyage_id: &str, argv: Vec<String>, cols: u16, rows: u16) -> CapsuleConfig {
    CapsuleConfig {
        voyage_root: dir.join(voyage_id),
        voyage_id: voyage_id.to_string(),
        retention: RetentionClass::Discard,
        producer_kind: "test-shell".into(),
        argv,
        cols,
        rows,
        survival: Survival::Normal,
        // Codex round-1 Major 9 (same reasoning as `tests/e2e_pipe.rs`'s
        // identical field): typed evidence, not `None`.
        rollout_evidence: sot_log::rollout::RolloutEvidence::NoRollbackTarget,
        // No supervisor in this end-to-end harness.
        parent_lease: None,
    }
}

/// Encode helpers for the attach lane's client frames and the mgmt lane's
/// requests — identical in shape to `tests/e2e_pipe.rs`'s own `frame`
/// module.
mod frame {
    use super::wire;

    pub fn hello() -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Hello { proto: wire::ATTACH_PROTO_V2 }).unwrap()
    }
    pub fn attach(controller_id: &str) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Attach { controller_id: controller_id.into() }).unwrap()
    }
    pub fn take(controller_id: &str) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Take { controller_id: controller_id.into() }).unwrap()
    }
    pub fn input(controller_id: &str, take_epoch: u64, idem_key: [u8; 16], payload: &[u8]) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Input {
            controller_id: controller_id.into(),
            take_epoch,
            idem_key,
            payload: payload.to_vec(),
        })
        .unwrap()
    }
    pub fn resize(cols: u16, rows: u16) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Resize { cols, rows }).unwrap()
    }
    pub fn mgmt_probe() -> Vec<u8> {
        wire::encode_mgmt_request(&wire::MgmtRequest::Probe).unwrap()
    }
    pub fn mgmt_status() -> Vec<u8> {
        wire::encode_mgmt_request(&wire::MgmtRequest::Status).unwrap()
    }
    pub fn mgmt_shutdown(reason: &str) -> Vec<u8> {
        wire::encode_mgmt_request(&wire::MgmtRequest::Shutdown { reason: reason.into() }).unwrap()
    }
}

/// Bounded join — see `tests/e2e_pipe.rs`'s identical helper.
fn wait_for_join<T: Send + 'static>(handle: std::thread::JoinHandle<T>, timeout: Duration) -> Option<T> {
    let deadline = Instant::now() + timeout;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Some(handle.join().unwrap())
}

/// Every sealed frame across every `.sotseg` in `root/seg`, in segment
/// order — identical to `tests/e2e_pipe.rs`'s own helper of the same name.
fn sealed_frames(root: &std::path::Path, voyage: &str) -> Vec<Envelope> {
    let seg_dir = root.join("seg");
    let mut out = Vec::new();
    let mut names: Vec<String> = std::fs::read_dir(&seg_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for n in names {
        if n.ends_with(".sotseg") {
            let r = SegmentReader::read(&seg_dir.join(&n), true).unwrap();
            assert_eq!(r.header.voyage_id, voyage);
            out.extend(r.frames);
        }
    }
    out
}

/// A real-socket frame reader — the socket analog of `tests/e2e_pipe.rs`'s
/// `RealFrames`, needed for the same reason: the watcher and driver
/// connections must also observe UNSOLICITED frames (live `Output`,
/// server-originated `Keepalive`) between explicit requests.
struct RealFrames {
    log: Arc<Mutex<Vec<wire::DecodedFrame>>>,
    reader_jh: Option<std::thread::JoinHandle<()>>,
    next_idx: usize,
}

impl RealFrames {
    fn spawn(client: Arc<SocketClient>) -> Self {
        let log = Arc::new(Mutex::new(Vec::new()));
        let reader_log = Arc::clone(&log);
        let jh = std::thread::spawn(move || {
            let mut splitter = wire::FrameSplitter::new();
            let mut buf = [0u8; 65536];
            loop {
                match client.read(&mut buf) {
                    Ok(0) => return, // ordered EOF
                    Ok(n) => {
                        let (decoded, err) = splitter.feed(&buf[..n]);
                        reader_log.lock().unwrap().extend(decoded);
                        if err.is_some() {
                            return;
                        }
                    }
                    Err(_) => return, // cancelled, or the connection died
                }
            }
        });
        Self { log, reader_jh: Some(jh), next_idx: 0 }
    }

    fn wait_for<T>(
        &mut self,
        label: &'static str,
        timeout: Duration,
        mut pred: impl FnMut(&wire::DecodedFrame) -> Option<T>,
    ) -> T {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let log = self.log.lock().unwrap();
                while self.next_idx < log.len() {
                    let f = &log[self.next_idx];
                    self.next_idx += 1;
                    if let Some(v) = pred(f) {
                        return v;
                    }
                }
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for {label}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Collects a checkpoint transfer end-to-end — see
    /// `tests/e2e_pipe.rs`'s identical helper for the full property list
    /// this proves (bounded reassembled length, no live `Output` before
    /// the final chunk, and the bytes actually restore into a valid vt100
    /// screen).
    fn collect_checkpoint(&mut self, label: &'static str, timeout: Duration, cols: u16, rows: u16) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
            let (last, bytes) = self.wait_for(label, remaining, |f| match f {
                wire::DecodedFrame::AttachServer(wire::AttachServer::CheckpointChunk { last, bytes }) => {
                    Some((*last, bytes.clone()))
                }
                wire::DecodedFrame::AttachServer(wire::AttachServer::Output { .. }) => panic!(
                    "{label}: a live Output frame arrived before the checkpoint transfer completed"
                ),
                _ => None,
            });
            out.extend(bytes);
            assert!(
                out.len() <= wire::MAX_CHECKPOINT_LEN,
                "{label}: reassembled checkpoint exceeds MAX_CHECKPOINT_LEN ({} > {})",
                out.len(),
                wire::MAX_CHECKPOINT_LEN
            );
            if last {
                let mut probe = vt100_ctt::Parser::new(rows, cols, 0);
                probe.restore_screen(&out).unwrap_or_else(|e| {
                    panic!("{label}: checkpoint bytes did not restore into a valid screen: {e:?}")
                });
                return out;
            }
        }
    }

    fn join(mut self, timeout: Duration) {
        if let Some(jh) = self.reader_jh.take() {
            let deadline = Instant::now() + timeout;
            while !jh.is_finished() {
                assert!(Instant::now() < deadline, "real-socket reader thread did not stop within {timeout:?}");
                std::thread::sleep(Duration::from_millis(20));
            }
            jh.join().ok();
        }
    }
}

/// One bounded, cancellable `SocketClient::read` — see
/// `tests/e2e_pipe.rs`'s identical `read_bounded` for the full rationale
/// (a missing reply or EOF must fail THIS test with a clear message,
/// never hang the whole CI job).
fn read_bounded(client: &Arc<SocketClient>, label: &'static str, timeout: Duration) -> Vec<u8> {
    let (tx, rx) = mpsc::channel();
    let worker_client = Arc::clone(client);
    let jh = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let result = worker_client.read(&mut buf).map(|n| buf[..n].to_vec());
        let _ = tx.send(result);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(bytes)) => {
            jh.join().ok();
            bytes
        }
        Ok(Err(e)) => {
            jh.join().ok();
            panic!("{label}: read failed: {e:?}");
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            client.cancel();
            let _ = jh.join();
            panic!("{label}: read did not complete within {timeout:?} (cancelled)");
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            jh.join().ok();
            panic!("{label}: worker thread ended without ever returning a result");
        }
    }
}

/// The mgmt lane is lockstep with no unsolicited pushes — see
/// `tests/e2e_pipe.rs`'s identical helper.
fn mgmt_roundtrip(
    client: &Arc<SocketClient>,
    splitter: &mut wire::FrameSplitter,
    pending: &mut VecDeque<wire::DecodedFrame>,
    request: Vec<u8>,
) -> wire::MgmtReply {
    client.write_all(&request).unwrap();
    loop {
        if let Some(f) = pending.pop_front() {
            match f {
                wire::DecodedFrame::MgmtReply(reply) => return reply,
                other => panic!("expected a MgmtReply, got {other:?}"),
            }
        }
        let bytes = read_bounded(client, "mgmt reply", Duration::from_secs(10));
        assert!(!bytes.is_empty(), "unexpected EOF waiting for a mgmt reply");
        let (decoded, err) = splitter.feed(&bytes);
        assert_eq!(err, None, "unexpected wire error decoding a mgmt reply");
        pending.extend(decoded);
    }
}

/// The full scenario: a real `sot-pty-helper --script --drip` producer
/// under a real capsule, its socket bound for real, driven by three real
/// OS connections — a watcher (checkpoint + live output), a driver
/// (checkpoint, take, input, resize), and a mgmt connection (probe,
/// status, shutdown) — ending the run via the mgmt lane's own `shutdown`
/// and verifying the sealed voyage records the input. The twin of
/// `tests/e2e_pipe.rs`'s `full_pipe_e2e_two_clients_and_mgmt`.
#[test]
fn full_socket_e2e_two_clients_and_mgmt() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let helper = env!("CARGO_BIN_EXE_sot-pty-helper").to_string();
    // --drip, not --linger: the producer must stay alive for every step
    // below (the run ends by an explicit mgmt `shutdown`, never by the
    // producer exiting) AND must keep emitting real live output -- see
    // `tests/e2e_pipe.rs`'s identical doc for the full rationale.
    //
    // DEVIATION (reported per the brief's own instruction): the brief's
    // literal `--script 1000 --drip` was tried first and timed out this
    // test's own "watcher live output" wait -- `SCRIPT_BLOCK` is 59
    // bytes, `script()` paces one byte per 1ms, so 1000 repeats is ~59
    // REAL wall-clock seconds before `--drip` even starts, which this
    // file's own 10s per-step bound (and the brief's own "run three
    // times" verification budget) cannot comfortably absorb. `5` (the
    // exact value `tests/e2e_pipe.rs`'s own twin test already uses)
    // proves the IDENTICAL property -- a watcher receives live output
    // after its checkpoint, on a REAL producer over a REAL transport --
    // with no property lost: raw output VOLUME under budget is already
    // `tests/capsule.rs`'s own `--flood`-driven job (a dedicated backpressure
    // test), not this end-to-end wiring proof's.
    let argv = vec![helper, "--script".to_string(), "5".to_string(), "--drip".to_string()];
    let voyage_id = fresh_voyage_id();
    let cfg = config(dir.path(), &voyage_id, argv, 80, 25);
    let root = cfg.voyage_root.clone();

    let mut transport = SocketTransport::new(8);
    let (_cmd_tx, cmd_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || capsule::run::<PtyProducer>(cfg, cmd_rx, &mut transport));

    // The socket is created INSIDE `run` (`Transport::bind` runs right
    // after `open_for_writing` — see `capsule.rs`'s own doc at that call
    // site); `connect_voyage_socket`'s own bounded retry on
    // `ENOENT`/`ECONNREFUSED` absorbs the ordinary race of a client trying
    // to connect before that has happened yet.
    let watcher_client = Arc::new(connect_voyage_socket(&voyage_id).unwrap());
    let mut watcher = RealFrames::spawn(Arc::clone(&watcher_client));
    watcher_client.write_all(&frame::hello()).unwrap();
    watcher.wait_for("watcher hello_ok", Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    watcher_client.write_all(&frame::attach("watcher")).unwrap();
    watcher.collect_checkpoint("watcher checkpoint", Duration::from_secs(10), 80, 25);

    // The producer keeps emitting (drip, every ~200ms, indefinitely) --
    // prove the watcher also receives LIVE post-watermark output, not
    // just the checkpoint.
    let live_bytes = watcher.wait_for("watcher live output", Duration::from_secs(10), |f| {
        if let wire::DecodedFrame::AttachServer(wire::AttachServer::Output { bytes }) = f {
            Some(bytes.clone())
        } else {
            None
        }
    });
    assert!(!live_bytes.is_empty(), "expected non-empty live output");

    // Driver connection: checkpoint, take, input, resize.
    let driver_client = Arc::new(connect_voyage_socket(&voyage_id).unwrap());
    let mut driver = RealFrames::spawn(Arc::clone(&driver_client));
    driver_client.write_all(&frame::hello()).unwrap();
    driver.wait_for("driver hello_ok", Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    driver_client.write_all(&frame::attach("driver")).unwrap();
    driver.collect_checkpoint("driver checkpoint", Duration::from_secs(10), 80, 25);
    driver_client.write_all(&frame::take("driver")).unwrap();
    let epoch = driver.wait_for("driver take_ok", Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
        _ => None,
    });

    // Input over the real socket -- the wire's own redaction rule means
    // the sealed voyage record can only ever be checked for LENGTH, not
    // content (asserted after the run ends, below).
    let idem_key: [u8; 16] = [0x42; 16];
    let payload: &[u8] = b"echo hello-from-e2e\n";
    driver_client.write_all(&frame::input("driver", epoch, idem_key, payload)).unwrap();
    let recorded = driver.wait_for("driver input outcome", Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale) => Some(false),
        _ => None,
    });
    assert!(recorded, "expected the fresh input to be recorded");

    // Resize (in-budget).
    driver_client.write_all(&frame::resize(100, 40)).unwrap();
    let resize_ok = driver.wait_for("driver resize outcome", Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
        wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
        _ => None,
    });
    assert!(resize_ok, "expected the in-budget resize to succeed");

    // Finding parity with `tests/e2e_pipe.rs`'s own round-2 review fix: a
    // second, independent resize proves the driver connection is still
    // alive and answering lockstep requests after everything above.
    driver_client.write_all(&frame::resize(80, 25)).unwrap();
    let second_resize_ok =
        driver.wait_for("driver second resize outcome (liveness proof)", Duration::from_secs(10), |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
            wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => Some(false),
            _ => None,
        });
    assert!(second_resize_ok, "expected the driver connection to still be alive and answer a second resize");

    // Mgmt lane: a THIRD connection -- probe + status.
    let mgmt_client = Arc::new(connect_voyage_socket(&voyage_id).unwrap());
    let mut mgmt_splitter = wire::FrameSplitter::new();
    let mut mgmt_pending = VecDeque::new();

    let probe_reply = mgmt_roundtrip(&mgmt_client, &mut mgmt_splitter, &mut mgmt_pending, frame::mgmt_probe());
    assert_eq!(probe_reply, wire::MgmtReply::ProbeOk);

    let status_reply = mgmt_roundtrip(&mgmt_client, &mut mgmt_splitter, &mut mgmt_pending, frame::mgmt_status());
    match status_reply {
        wire::MgmtReply::StatusOk { pid, .. } => {
            // This test runs the capsule IN-PROCESS (`capsule::run` on a
            // spawned THREAD of this same test binary), so the pid
            // `self_status` reports IS this test process's own.
            assert_eq!(pid, std::process::id(), "expected status's pid to equal this (in-process) test's own pid");
        }
        other => panic!("expected StatusOk, got {other:?}"),
    }

    // End the run over the mgmt lane. The ack must arrive BEFORE the
    // connection's ordered EOF.
    let shutdown_reply =
        mgmt_roundtrip(&mgmt_client, &mut mgmt_splitter, &mut mgmt_pending, frame::mgmt_shutdown("e2e test done"));
    assert_eq!(shutdown_reply, wire::MgmtReply::ShutdownOk);
    let eof = read_bounded(&mgmt_client, "mgmt EOF after shutdown ack", Duration::from_secs(10));
    assert!(eof.is_empty(), "expected ordered EOF on the mgmt connection after its own shutdown ack");

    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested, "expected the mgmt shutdown to end the run as Requested");

    verify_voyage(&root, &voyage_id).unwrap();

    // The sealed voyage must show the input recorded (length only -- its
    // content is redacted by design) EXACTLY once, and a `forwarded` fact
    // correlated to THAT exact input's own sequence via `CausedBy`.
    let frames = sealed_frames(&root, &voyage_id);
    let hex: String = idem_key.iter().map(|b| format!("{b:02x}")).collect();
    let input_frames: Vec<&Envelope> = frames
        .iter()
        .filter(|f| f.class == Class::Input && f.payload.as_ref().unwrap()["idem_key"] == hex)
        .collect();
    assert_eq!(input_frames.len(), 1, "expected EXACTLY ONE Input frame for this idem_key, found {}", input_frames.len());
    let input_frame = input_frames[0];
    assert_eq!(input_frame.payload.as_ref().unwrap()["length"], payload.len());
    let input_seq = input_frame.seq;
    let forwarded = frames.iter().any(|f| {
        f.class == Class::Lifecycle
            && f.payload.as_ref().unwrap()["kind"] == "input_fact"
            && f.payload.as_ref().unwrap()["fact"]["fact"] == "forwarded"
            && f.refs.iter().any(|r| r.kind == RefKind::CausedBy && r.frame == input_seq)
    });
    assert!(forwarded, "expected a forwarded input_fact CAUSED BY (correlated to) this exact input's own sequence");

    watcher.join(Duration::from_secs(10));
    driver.join(Duration::from_secs(10));
    drop(mgmt_client);
}

/// The direct children of `pid`, via `/proc/<pid>/task/<pid>/children`
/// (Linux 3.5+) — the capsule's fork of its producer happens on its own
/// main thread, before any of its own extra threads exist (the reader
/// thread starts only AFTER a successful spawn — see `capsule.rs`'s own
/// `run`), so at fork time this thread's own tid still equals the
/// process's own pid, making this exactly the producer's own pid with no
/// name-matching needed (a shared CI runner may have unrelated `sleep`
/// processes of its own).
fn read_direct_children(pid: u32) -> Vec<u32> {
    let path = format!("/proc/{pid}/task/{pid}/children");
    std::fs::read_to_string(path).unwrap_or_default().split_whitespace().filter_map(|s| s.parse().ok()).collect()
}

/// ADR 0043 decision 14: `PR_SET_PDEATHSIG(SIGKILL)` must fire on the
/// death of the SPAWNING THREAD by ANY means, including a hard `SIGKILL`
/// of the whole capsule process — not merely an orderly exit. Spawns a
/// real `sot-capsule run` binary with a `sleep 600` producer, polls for
/// the producer's own child pid to appear (see `read_direct_children`'s
/// own doc — `Transport::bind` actually runs BEFORE `Producer::spawn` in
/// `capsule::run`'s own ordering, the opposite of what an earlier version
/// of this test assumed from the socket's own appearance alone), SIGKILLs
/// the capsule with no graceful EndRun at all, and asserts the producer's
/// whole process GROUP is gone within a bounded wait.
#[test]
fn pdeathsig_kills_the_producer_when_the_capsule_dies_hard() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let voyage_id = fresh_voyage_id();
    let voyage_root = dir.path().join(&voyage_id);

    let capsule_exe = env!("CARGO_BIN_EXE_sot-capsule");
    let mut child = std::process::Command::new(capsule_exe)
        .arg("run")
        .arg(&voyage_root)
        .arg(&voyage_id)
        .arg("--assume-no-rollback-target")
        .arg("--")
        .arg("sleep")
        .arg("600")
        .env("SOT_RUNTIME_DIR", _runtime.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn sot-capsule");
    let capsule_pid = child.id();

    // Poll directly for the producer's own child pid to appear -- the
    // actual precondition this test needs (not merely "the capsule got
    // far enough to bind its socket", which happens EARLIER and proves
    // nothing about whether `Producer::spawn` has run yet).
    let deadline = Instant::now() + Duration::from_secs(10);
    let producer_pid = loop {
        if let Some(pid) = read_direct_children(capsule_pid).into_iter().next() {
            break pid;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the capsule never spawned a producer child within 10s");
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    // Hard-kill the capsule itself -- SIGKILL, no graceful EndRun at all.
    child.kill().expect("SIGKILL the capsule");
    let _ = child.wait();

    // `PR_SET_PDEATHSIG(SIGKILL)` fires on the death of the SPAWNING
    // THREAD (the capsule's own main thread, just killed above) -- the
    // producer's whole process GROUP must be gone within a bounded wait,
    // with no supervisor or teardown code involved at all.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let rc = unsafe { libc::killpg(producer_pid as libc::pid_t, 0) };
        if rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the producer's process group was still alive 5s after the capsule was SIGKILLed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
