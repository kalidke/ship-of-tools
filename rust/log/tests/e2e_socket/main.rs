#![cfg(target_os = "linux")]
//! End-to-end test for the Unix socket transport (ADR 0043 "Decisions for
//! LU2" LU2b) — the twin of `tests/e2e_pipe.rs`, over a REAL Unix domain
//! socket instead of a REAL named pipe. `tests/capsule/` proves the
//! writer loop and `AttachProto` against a synthetic `TestTransport` (now
//! against `PtyProducer` too, on Linux); this file is the one place both
//! are proven together with a REAL transport:
//! `platform_transport::PlatformTransport` wrapping a real
//! `socket_unix::SocketServer`, with real OS clients connecting via
//! `socket_unix::connect_voyage_socket` — a watcher, a driver, and a mgmt
//! connection, all against the SAME running capsule.
//!
//! `target_os = "linux"` (not bare `unix`): `connect_voyage_socket`'s own
//! challenge (`challenge_unix::authenticate_server`) is Linux-only (ADR
//! 0043 decision 8) — other Unix fails closed there, matching
//! `tests/challenge_unix.rs`'s own gate.

use sot_log::capsule::{self, CapsuleConfig, ExitKind};
use sot_log::capsule::producer::pty::PtyProducer;
use sot_log::store::segment::{RetentionClass, SegmentReader};
use sot_log::lane::platform_transport::PlatformTransport;
use sot_log::lane::socket_unix::{connect_voyage_socket, SocketClient};
use sot_log::store::verify::verify_voyage;
use sot_log::lane::wire::{self, Survival};
use sot_log::{Class, Envelope, RefKind};
use std::collections::VecDeque;
#[path = "../support/capsule_guard.rs"]
mod capsule_guard;
mod pdeathsig;

use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// Serializes every test in this file. Two independent reasons, both
/// already precedented elsewhere in this crate: every test here mutates
/// the shared, process-global `SOT_RUNTIME_DIR` env var (see
/// `isolated_runtime_dir` below, copied from `tests/socket_unix/`'s and
/// `tests/challenge_unix.rs`'s identical helper) — unsafe to interleave
/// across threads in the SAME process — and, like `tests/capsule/`'s
/// own `SERIAL`, two real-pty-plus-real-socket tests could otherwise
/// starve each other on a loaded CI runner.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Points `SOT_RUNTIME_DIR` at a fresh, mode-0700 tempdir under `/tmp` for
/// the lifetime of the returned guard — mirrors `tests/socket_unix/`'s
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
/// (reached through `PlatformTransport::bind`, called by `run` itself)
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
        rollout_evidence: sot_log::store::rollout::RolloutEvidence::NoRollbackTarget,
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

/// ADR 0043 decision 27: the transport's own connect no longer retries an
/// ABSENT endpoint (only a busy one, within `CONNECT_BOUND`) — a caller
/// racing a server's own startup (here, `Transport::bind` running on
/// `capsule::run`'s background thread, a moment after this test spawns
/// it) now owns that readiness wait itself. Polls `connect` every 50ms
/// until it succeeds or `deadline` — a GENEROUS bound, evidence of a
/// genuinely broken startup, never a tight race — expires, at which
/// point the LAST error fails the test loudly. Identical helper in
/// `tests/e2e_pipe.rs` and `tests/pipe_win/` (no shared test module
/// spans Windows-only and Linux-only files).
fn wait_for_endpoint<T, E: std::fmt::Display>(connect: impl Fn() -> Result<T, E>, deadline: Duration) -> T {
    let started = Instant::now();
    loop {
        match connect() {
            Ok(v) => return v,
            Err(e) => {
                if started.elapsed() >= deadline {
                    panic!("endpoint did not become ready within {deadline:?}: {e}");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
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
#[allow(clippy::too_many_lines, reason = "one test scenario: two clients and a mgmt connection over the socket transport")]
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
    // `tests/capsule/`'s own `--flood`-driven job (a dedicated backpressure
    // test), not this end-to-end wiring proof's.
    let argv = vec![helper, "--script".to_string(), "5".to_string(), "--drip".to_string()];
    let voyage_id = fresh_voyage_id();
    let cfg = config(dir.path(), &voyage_id, argv, 80, 25);
    let root = cfg.voyage_root.clone();

    let mut transport = PlatformTransport::new(8);
    let (_cmd_tx, cmd_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || capsule::run::<PtyProducer>(cfg, cmd_rx, &mut transport));

    // The socket is created INSIDE `run` (`Transport::bind` runs right
    // after `open_for_writing` — see `capsule/`'s own doc at that call
    // site). ADR 0043 decision 27: `connect_voyage_socket` no longer
    // retries an absent endpoint (`ENOENT`/`ECONNREFUSED` now fail on the
    // first attempt) — this test owns the ordinary race of connecting
    // before that bind has happened yet via `wait_for_endpoint`.
    let watcher_client = Arc::new(wait_for_endpoint(|| connect_voyage_socket(&voyage_id), Duration::from_secs(30)));
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

/// Switch-latency Phase 1 (c): once a real capsule's producer has gone
/// quiet (`--script 1 --linger` writes one small script block, then sits
/// forever with no further output — no `--drip` nudging the main loop the
/// way `full_socket_e2e_two_clients_and_mgmt`'s own producer does) and the
/// main loop has had nothing to do for well over `GROUP_COMMIT_WINDOW`
/// (50ms) — so it is genuinely parked in its own `output_rx.recv_timeout`
/// tail wait, not mid-tick — a FRESH attach (connect, hello, attach,
/// collect the checkpoint) over the REAL socket transport must complete
/// near-instantly rather than risk paying the OLD worst case (each step
/// landing right after a drain, sitting unnoticed for up to the full
/// window before the loop's own tick would have found it), only reachable
/// at all if `Transport::set_wake`'s callback (`PlatformTransport::bind`,
/// via `SocketServer::set_wake`) actually wakes this loop on real
/// transport activity rather than solely on its own group-commit cadence.
///
/// Codex round on #227 (P2 discharge): a SINGLE sample against a 200ms
/// bound does not reliably fail the OLD code (measured ~91ms across
/// several runs against the pre-fix code — comfortably under 200ms with
/// no fix at all). `TRIALS` independent attaches against a TIGHT bound
/// (50ms) is the mechanism proof instead: the old code's own measured
/// value clusters tightly around 91ms (not mere luck-prone variance), so
/// EVERY trial failing a 50ms bound is what the old code actually does,
/// while an immediate, connection/frame-triggered wake lands every
/// single trial near-instantly.
#[test]
fn an_attach_against_an_idle_capsule_is_not_group_commit_bound() {
    let _serial = serial();
    let _runtime = isolated_runtime_dir();
    let dir = tempfile::tempdir().unwrap();
    let helper = env!("CARGO_BIN_EXE_sot-pty-helper").to_string();
    let argv = vec![helper, "--script".to_string(), "1".to_string(), "--linger".to_string()];
    let voyage_id = fresh_voyage_id();
    let cfg = config(dir.path(), &voyage_id, argv, 80, 25);
    let root = cfg.voyage_root.clone();

    let mut transport = PlatformTransport::new(8);
    let (_cmd_tx, cmd_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || capsule::run::<PtyProducer>(cfg, cmd_rx, &mut transport));

    // Prove the run is genuinely up (and drain its one-shot startup
    // script) with an ORDINARY attach first -- untimed setup, not the
    // measurement.
    let setup_client = Arc::new(wait_for_endpoint(|| connect_voyage_socket(&voyage_id), Duration::from_secs(10)));
    let mut setup = RealFrames::spawn(Arc::clone(&setup_client));
    setup_client.write_all(&frame::hello()).unwrap();
    setup.wait_for("setup hello_ok", Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    setup_client.write_all(&frame::attach("setup")).unwrap();
    setup.collect_checkpoint("setup checkpoint", Duration::from_secs(10), 80, 25);
    // `cancel` (SHUT_RDWR), not `drop`: the run is still very much alive
    // at this point (unlike the OTHER tests in this file, which only
    // drop/join their own connections AFTER the whole run has already
    // been shut down and every connection closed server-side) --
    // `setup`'s own reader thread holds its OWN `Arc` clone of
    // `setup_client`, so dropping this local one alone would never
    // actually close the socket, and the reader thread would block on
    // its own `read` forever.
    setup_client.cancel();
    setup.join(Duration::from_secs(10));

    const TRIALS: usize = 5;
    const IDLE_BEFORE_ATTACH: Duration = Duration::from_millis(300); // > GROUP_COMMIT_WINDOW (50ms)
    const TIGHT_BOUND: Duration = Duration::from_millis(50); // == GROUP_COMMIT_WINDOW
    let mut elapsed_all = Vec::with_capacity(TRIALS);
    for i in 0..TRIALS {
        // Let the main loop go genuinely idle before each trial --
        // comfortably longer than GROUP_COMMIT_WINDOW -- so it is parked
        // in its own tail wait when this trial's attach connects, not
        // mid-tick from the PREVIOUS trial's own teardown.
        std::thread::sleep(IDLE_BEFORE_ATTACH);

        let started = Instant::now();
        let fresh_client = Arc::new(connect_voyage_socket(&voyage_id).unwrap());
        let mut fresh = RealFrames::spawn(Arc::clone(&fresh_client));
        fresh_client.write_all(&frame::hello()).unwrap();
        fresh.wait_for("fresh attach hello_ok", Duration::from_secs(5), |f| {
            matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
        });
        fresh_client.write_all(&frame::attach(&format!("fresh{i}"))).unwrap();
        fresh.collect_checkpoint("fresh attach checkpoint", Duration::from_secs(5), 80, 25);
        elapsed_all.push(started.elapsed());

        // Same reasoning as the setup connection's own cleanup above --
        // `cancel`, not `drop`, actually closes the socket while the run
        // is still alive, and closing it before the NEXT trial's connect
        // keeps each trial an independent, fresh attach.
        fresh_client.cancel();
        fresh.join(Duration::from_secs(10));
    }

    // Clean shutdown over a mgmt connection -- matching
    // `full_socket_e2e_two_clients_and_mgmt`'s own pattern, never just
    // dropping the run thread's handle (which would strand the producer).
    let mgmt_client = Arc::new(connect_voyage_socket(&voyage_id).unwrap());
    let mut mgmt_splitter = wire::FrameSplitter::new();
    let mut mgmt_pending = VecDeque::new();
    let shutdown_reply = mgmt_roundtrip(
        &mgmt_client,
        &mut mgmt_splitter,
        &mut mgmt_pending,
        frame::mgmt_shutdown("idle-attach latency test done"),
    );
    assert_eq!(shutdown_reply, wire::MgmtReply::ShutdownOk);
    let eof = read_bounded(&mgmt_client, "mgmt EOF after shutdown ack", Duration::from_secs(10));
    assert!(eof.is_empty(), "expected ordered EOF on the mgmt connection after its own shutdown ack");

    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested, "expected the mgmt shutdown to end the run as Requested");

    verify_voyage(&root, &voyage_id).unwrap();
    drop(mgmt_client);

    println!(
        "idle-capsule fresh attach over {TRIALS} trials: max={:?}, all={elapsed_all:?}",
        elapsed_all.iter().max().unwrap()
    );
    assert!(
        elapsed_all.iter().all(|e| *e < TIGHT_BOUND),
        "expected every one of {TRIALS} fresh attaches (connect+hello+attach+checkpoint) against an idle \
         capsule to complete in well under {TIGHT_BOUND:?} -- got {elapsed_all:?}. A single attach passing \
         this bound could be luck; ALL {TRIALS} passing is only possible if the main loop wakes on \
         incoming connections/bytes immediately rather than waiting out its own group-commit cadence"
    );
}
