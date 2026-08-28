#![cfg(windows)]
//! End-to-end test for the ADR 0041 step-5 pipe transport's deferred half
//! (U3 round 2): a REAL capsule run, driven entirely over a REAL named
//! pipe. `tests/capsule_win.rs` proves the writer loop and `AttachProto`
//! against a synthetic `TestTransport`; `tests/pipe_win.rs` proves the raw
//! transport against a plain echo consumer with no capsule. This file is
//! the one place both are proven together: `pipe_transport::PipeTransport`
//! wrapping a real `pipe_win::PipeServer`, with real OS clients connecting
//! via `pipe_win::connect_voyage_pipe` — a watcher, a driver, and a mgmt
//! connection, all against the SAME running capsule.
//!
//! Heavy, like `tests/capsule_win.rs`'s own tests (a real ConPTY producer,
//! a real writer loop, now real pipe I/O on top) — the same SERIAL lock
//! pattern is reused here for the same reason (one shared lock makes
//! concurrent heavy tests additive rather than adversarial; a separate
//! file is its own binary, but cargo may still run test BINARIES
//! concurrently with this one, and this file may grow more than the one
//! test below over time).

use sot_log::capsule_win::{self, CapsuleWinConfig, ExitKind};
use sot_log::pipe_transport::PipeTransport;
use sot_log::pipe_win::{connect_voyage_pipe, PipeClient};
use sot_log::segment::{RetentionClass, SegmentReader};
use sot_log::verify::verify_voyage;
use sot_log::wire::{self, Survival};
use sot_log::{Class, Envelope};
use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// A fresh, canonical lowercase-hyphenated UUID — `pipe_win::PipeServer::bind`
/// (reached through `PipeTransport::bind`, called by `run` itself) validates
/// the voyage id as exactly this shape before it will ever create the pipe,
/// so (unlike `tests/capsule_win.rs`'s short mnemonic names, which never
/// touch a real pipe) this file's voyage id MUST be one.
fn fresh_voyage_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn config(
    dir: &std::path::Path,
    voyage_id: &str,
    argv: Vec<String>,
    cols: u16,
    rows: u16,
) -> CapsuleWinConfig {
    CapsuleWinConfig {
        voyage_root: dir.join(voyage_id),
        voyage_id: voyage_id.to_string(),
        retention: RetentionClass::Discard,
        producer_kind: "test-shell".into(),
        argv,
        cols,
        rows,
        survival: Survival::Normal,
    }
}

/// Encode helpers for the attach lane's client frames and the mgmt lane's
/// requests — identical in shape to `tests/capsule_win.rs`'s own `frame`
/// module (not shared: a three-function module is exactly this crate's
/// own leaf-helper-duplication convention, see `pipe_win.rs`'s module doc).
mod frame {
    use super::wire;

    pub fn hello() -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Hello {
            proto: wire::ATTACH_PROTO_V1,
        })
        .unwrap()
    }
    pub fn attach(controller_id: &str) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Attach {
            controller_id: controller_id.into(),
        })
        .unwrap()
    }
    pub fn take(controller_id: &str) -> Vec<u8> {
        wire::encode_attach_client(&wire::AttachClient::Take {
            controller_id: controller_id.into(),
        })
        .unwrap()
    }
    pub fn input(
        controller_id: &str,
        take_epoch: u64,
        idem_key: [u8; 16],
        payload: &[u8],
    ) -> Vec<u8> {
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
        wire::encode_mgmt_request(&wire::MgmtRequest::Shutdown {
            reason: reason.into(),
        })
        .unwrap()
    }
}

/// Bounded join — see `tests/capsule_win.rs`'s identical helper: `run`
/// blocks until the run ends, and a teardown-ordering bug is exactly the
/// class of bug that would hang it forever.
fn wait_for_join<T: Send + 'static>(
    handle: std::thread::JoinHandle<T>,
    timeout: Duration,
) -> Option<T> {
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
/// order — identical to `tests/capsule_win.rs`'s own helper of the same
/// name.
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

/// A real-pipe frame reader: spawns a background thread reading from a
/// `PipeClient` (via `Arc`, since `PipeClient::read`/`write_all` both take
/// `&self` — the same handle stays usable for WRITING requests on the
/// caller's own thread the whole time), decoding through a
/// `wire::FrameSplitter`, appending every decoded frame to a shared,
/// growing log this struct polls. The real-pipe analog of
/// `tests/capsule_win.rs`'s `FrameWatcher`, needed here (unlike the mgmt
/// lane below, serviced with plain synchronous reads) because the watcher
/// and driver connections must also observe UNSOLICITED frames — live
/// `Output` and the server-originated `Keepalive` — arriving between
/// explicit requests, which a "write then blocking-read-one-frame" model
/// cannot.
struct RealFrames {
    log: Arc<Mutex<Vec<wire::DecodedFrame>>>,
    reader_jh: Option<std::thread::JoinHandle<()>>,
    next_idx: usize,
}

impl RealFrames {
    fn spawn(client: Arc<PipeClient>) -> Self {
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
        Self {
            log,
            reader_jh: Some(jh),
            next_idx: 0,
        }
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

    fn collect_checkpoint(&mut self, label: &'static str, timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        loop {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1));
            let (last, bytes) = self.wait_for(label, remaining, |f| {
                if let wire::DecodedFrame::AttachServer(wire::AttachServer::CheckpointChunk {
                    last,
                    bytes,
                }) = f
                {
                    Some((*last, bytes.clone()))
                } else {
                    None
                }
            });
            out.extend(bytes);
            if last {
                return out;
            }
        }
    }

    /// Joins the background reader thread — bounded, since it only ever
    /// blocks in `PipeClient::read`, which the connection's own EOF (a
    /// server-side close) or the caller's `PipeClient::cancel` unblocks.
    fn join(mut self, timeout: Duration) {
        if let Some(jh) = self.reader_jh.take() {
            let deadline = Instant::now() + timeout;
            while !jh.is_finished() {
                assert!(
                    Instant::now() < deadline,
                    "real-pipe reader thread did not stop within {timeout:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            jh.join().ok();
        }
    }
}

/// The mgmt lane is lockstep with no unsolicited pushes (unlike the attach
/// lane above) — one outstanding request at a time, so a plain synchronous
/// "write, then read until one full reply decodes" is sufficient; no
/// background thread needed.
fn mgmt_roundtrip(
    client: &PipeClient,
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
        let mut buf = [0u8; 4096];
        let n = client.read(&mut buf).unwrap();
        assert!(n > 0, "unexpected EOF waiting for a mgmt reply");
        let (decoded, err) = splitter.feed(&buf[..n]);
        assert_eq!(err, None, "unexpected wire error decoding a mgmt reply");
        pending.extend(decoded);
    }
}

/// The full scenario: a real `--script --linger` producer under a real
/// capsule, its pipe bound for real, driven by three real OS connections —
/// a watcher (checkpoint + live output), a driver (checkpoint, take,
/// input, resize, keepalive), and a mgmt connection (probe, status,
/// shutdown) — ending the run via the mgmt lane's own `shutdown` and
/// verifying the sealed voyage records the input.
#[test]
fn full_pipe_e2e_two_clients_and_mgmt() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let helper = env!("CARGO_BIN_EXE_sot-conpty-helper").to_string();
    // --linger: the producer must stay alive for every step below; the run
    // ends by an explicit mgmt `shutdown`, never by the producer exiting.
    let argv = vec![
        helper,
        "--script".to_string(),
        "1000".to_string(),
        "--linger".to_string(),
    ];
    let voyage_id = fresh_voyage_id();
    let cfg = config(dir.path(), &voyage_id, argv, 80, 25);
    let root = cfg.voyage_root.clone();

    let (mut transport, transport_rx) = PipeTransport::new(8);
    let (_cmd_tx, cmd_rx) = mpsc::channel();
    let handle =
        std::thread::spawn(move || capsule_win::run(cfg, cmd_rx, transport_rx, &mut transport));

    // The pipe is created INSIDE `run` (`Transport::bind` runs right after
    // `open_for_writing` — see `capsule_win.rs`'s own doc at that call
    // site); `connect_voyage_pipe`'s own bounded retry on
    // `ERROR_FILE_NOT_FOUND` absorbs the ordinary race of a client trying
    // to connect before that has happened yet.
    let watcher_client = Arc::new(connect_voyage_pipe(&voyage_id).unwrap());
    let mut watcher = RealFrames::spawn(Arc::clone(&watcher_client));
    watcher_client.write_all(&frame::hello()).unwrap();
    watcher.wait_for("watcher hello_ok", Duration::from_secs(10), |f| {
        matches!(
            f,
            wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })
        )
        .then_some(())
    });
    watcher_client.write_all(&frame::attach("watcher")).unwrap();
    let watcher_checkpoint =
        watcher.collect_checkpoint("watcher checkpoint", Duration::from_secs(10));
    assert!(
        !watcher_checkpoint.is_empty(),
        "expected a non-empty checkpoint transfer"
    );

    // The producer keeps emitting (--script, 1000 repeats) — prove the
    // watcher also receives LIVE post-watermark output, not just the
    // checkpoint.
    let live_bytes = watcher.wait_for("watcher live output", Duration::from_secs(10), |f| {
        if let wire::DecodedFrame::AttachServer(wire::AttachServer::Output { bytes }) = f {
            Some(bytes.clone())
        } else {
            None
        }
    });
    assert!(!live_bytes.is_empty(), "expected non-empty live output");

    // Driver connection: checkpoint, take, input, resize, keepalive.
    let driver_client = Arc::new(connect_voyage_pipe(&voyage_id).unwrap());
    let mut driver = RealFrames::spawn(Arc::clone(&driver_client));
    driver_client.write_all(&frame::hello()).unwrap();
    driver.wait_for("driver hello_ok", Duration::from_secs(10), |f| {
        matches!(
            f,
            wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })
        )
        .then_some(())
    });
    driver_client.write_all(&frame::attach("driver")).unwrap();
    driver.collect_checkpoint("driver checkpoint", Duration::from_secs(10));
    driver_client.write_all(&frame::take("driver")).unwrap();
    let epoch = driver.wait_for("driver take_ok", Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => {
            Some(*take_epoch)
        }
        _ => None,
    });

    // Input over the real pipe — the wire's own redaction rule means the
    // sealed voyage record can only ever be checked for LENGTH, not
    // content (asserted after the run ends, below).
    let idem_key: [u8; 16] = [0x42; 16];
    let payload: &[u8] = b"echo hello-from-e2e\r\n";
    driver_client
        .write_all(&frame::input("driver", epoch, idem_key, payload))
        .unwrap();
    let recorded = driver.wait_for(
        "driver input outcome",
        Duration::from_secs(10),
        |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRecorded) => Some(true),
            wire::DecodedFrame::AttachServer(wire::AttachServer::InputRefusedStale) => Some(false),
            _ => None,
        },
    );
    assert!(recorded, "expected the fresh input to be recorded");

    // Resize (in-budget).
    driver_client.write_all(&frame::resize(100, 40)).unwrap();
    let resize_ok = driver.wait_for(
        "driver resize outcome",
        Duration::from_secs(10),
        |f| match f {
            wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeOk) => Some(true),
            wire::DecodedFrame::AttachServer(wire::AttachServer::ResizeRefused { .. }) => {
                Some(false)
            }
            _ => None,
        },
    );
    assert!(resize_ok, "expected the in-budget resize to succeed");

    // Keepalive: the server originates it after `KEEPALIVE_IDLE_TRIGGER`
    // (30s) of driver inactivity — this connection does nothing else in
    // the meantime, so the wait below is genuinely ~30s, bounded
    // generously past that. `wire.rs`'s own contract: the client must
    // bounce the IDENTICAL bytes back verbatim.
    let nonce = driver.wait_for(
        "driver keepalive from server",
        Duration::from_secs(50),
        |f| {
            if let wire::DecodedFrame::Keepalive { nonce } = f {
                Some(*nonce)
            } else {
                None
            }
        },
    );
    driver_client
        .write_all(&wire::encode_keepalive(nonce))
        .unwrap();

    // Mgmt lane: a THIRD connection — SOM0 probe + status.
    let mgmt_client = connect_voyage_pipe(&voyage_id).unwrap();
    let mut mgmt_splitter = wire::FrameSplitter::new();
    let mut mgmt_pending = VecDeque::new();

    let probe_reply = mgmt_roundtrip(
        &mgmt_client,
        &mut mgmt_splitter,
        &mut mgmt_pending,
        frame::mgmt_probe(),
    );
    assert_eq!(probe_reply, wire::MgmtReply::ProbeOk);

    let status_reply = mgmt_roundtrip(
        &mgmt_client,
        &mut mgmt_splitter,
        &mut mgmt_pending,
        frame::mgmt_status(),
    );
    match status_reply {
        wire::MgmtReply::StatusOk { pid, .. } => {
            // This test runs the capsule IN-PROCESS (`capsule_win::run` on
            // a spawned THREAD of this same test binary, not a separate
            // OS process), so the pid `self_status` reports IS this test
            // process's own — the real cross-process identity check
            // (a supervisor observing a DIFFERENT process's capsule) is
            // step 6/7's row, not provable from here.
            assert_eq!(
                pid,
                std::process::id(),
                "expected status's pid to equal this (in-process) test's own pid"
            );
        }
        other => panic!("expected StatusOk, got {other:?}"),
    }

    // End the run over the mgmt lane. The ack must arrive BEFORE the
    // connection's ordered EOF (ADR 0041: "the shutdown ack is physically
    // written before teardown closes its connection").
    let shutdown_reply = mgmt_roundtrip(
        &mgmt_client,
        &mut mgmt_splitter,
        &mut mgmt_pending,
        frame::mgmt_shutdown("e2e test done"),
    );
    assert_eq!(shutdown_reply, wire::MgmtReply::ShutdownOk);
    let mut eof_buf = [0u8; 16];
    let n = mgmt_client.read(&mut eof_buf).unwrap();
    assert_eq!(
        n, 0,
        "expected ordered EOF on the mgmt connection after its own shutdown ack"
    );

    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(
        summary.exit_kind,
        ExitKind::Requested,
        "expected the mgmt shutdown to end the run as Requested"
    );

    verify_voyage(&root, &voyage_id).unwrap();

    // The sealed voyage must show the input recorded (length only — its
    // content is redacted by design) and a matching `forwarded` fact.
    let frames = sealed_frames(&root, &voyage_id);
    let hex: String = idem_key.iter().map(|b| format!("{b:02x}")).collect();
    let input_frame = frames
        .iter()
        .find(|f| f.class == Class::Input && f.payload.as_ref().unwrap()["idem_key"] == hex)
        .expect("expected exactly one Input frame for this idem_key");
    assert_eq!(
        input_frame.payload.as_ref().unwrap()["length"],
        payload.len()
    );
    let forwarded = frames.iter().any(|f| {
        f.class == Class::Lifecycle
            && f.payload.as_ref().unwrap()["kind"] == "input_fact"
            && f.payload.as_ref().unwrap()["fact"]["fact"] == "forwarded"
    });
    assert!(
        forwarded,
        "expected a forwarded input_fact for the recorded input"
    );

    watcher.join(Duration::from_secs(10));
    driver.join(Duration::from_secs(10));
    drop(mgmt_client);
}
