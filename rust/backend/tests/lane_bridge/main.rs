#![cfg(target_os = "linux")]
//! ADR 0045 lane B4b: the cross-process proofs that an FE attach client
//! reaches a real capsule row THROUGH a real daemon in the middle, over
//! a test-owned TCP -> Unix relay standing in for the loopback tunnel a
//! remote `DaemonLaneEndpoint::LaneDial::Tcp` dials in production
//! (`sot-protocol`'s own `lane_client.rs`, lane B4a). Shares `Env` and
//! the wire-protocol round-trip helpers with `capsule_workspaces.rs` via
//! `tests/support/mod.rs` (a pure lift there, no behavior change).
//!
//! Decision 11's own words are what each test below proves piece by
//! piece: "a slow or interrupted link is never mistaken for a dead row,
//! and recording never blocks on either."

#[path = "../support/mod.rs"]
mod support;
use support::*;

use sot_log::attach_client::client::{FeAttachClient, InputOutcome};
use sot_log::store::segment::SegmentReader;
use sot_protocol::topology::lane_client::{DaemonLaneEndpoint, LaneDial};
use sot_protocol::{op, Frame};

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixStream};

/// Mirrors `capsule_workspaces.rs`'s own `SERIAL`: this file's real
/// `sotd`/`sot-capsule` processes share the same per-process
/// `SOT_RUNTIME_DIR` env var `Env::new` sets, so parallel tests within
/// THIS binary would race it exactly the same way.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `fe_client_io::CHECKPOINT_TRANSFER_BUDGET` is private (12 checkpoint
/// chunks x its own private 5s per-frame `STATUS_BUDGET`) — reconstructed
/// here from the one piece that IS `pub`, `wire::CHECKPOINT_CHUNKS_AT_
/// MAX_PAYLOAD`, rather than a second, drifting magic number.
const CHECKPOINT_TRANSFER_BUDGET: Duration =
    Duration::from_secs(sot_log::lane::wire::CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD as u64 * 5);

fn wake_flag_for_test() -> (Arc<AtomicBool>, Box<dyn Fn() + Send + 'static>) {
    let woke = Arc::new(AtomicBool::new(false));
    let woke2 = Arc::clone(&woke);
    (woke, Box::new(move || woke2.store(true, Ordering::Relaxed)))
}

/// Creates a `runtime: "capsule"` workspace and polls it to `"ready"`,
/// returning its id and the `session_name` `lane.connect`'s own `target`
/// names — `workspace.create`'s reply already carries it (mirrors
/// `capsule_workspaces.rs`'s own `lane_connect_supervisor_pipes_hello_
/// and_status` fixture).
async fn create_ready_capsule_row(env: &Env, conn: &mut Conn, next_id: &mut u64, label: &str) -> (String, String) {
    let create_req = serde_json::json!({
        "label": label,
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let create_res = call(conn, *next_id, op::WORKSPACE_CREATE, create_req).await;
    *next_id += 1;
    assert!(create_res.payload.get("error").is_none(), "workspace.create failed: {:?}", create_res.payload);
    let workspace_id = create_res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = create_res.payload["session_name"].as_str().expect("session_name").to_string();
    poll_for_phase(conn, next_id, &workspace_id, "ready", BOUND.max(Duration::from_secs(90))).await;
    (workspace_id, target)
}

/// [`capsule_workspaces.rs`'s own `kill_supervisor_only`], reproduced
/// here (not moved — only `Env` and the wire helpers were): SIGKILL every
/// process matching this env's own anchored `supervise` pattern, then
/// poll it gone. Simulates the authority crashing outright, never a
/// graceful `stop`.
fn kill_supervisor_only(state_root: &Path) {
    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", state_root);
    let _ = Command::new("pkill")
        .arg("-9")
        .arg("-f")
        .arg(&pattern)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    assert!(poll_until_no_process_matches(&pattern, BOUND), "a supervisor process still matches {pattern:?} after SIGKILL");
}

/// [`capsule_workspaces.rs`'s own `count_matching_processes`] twin —
/// needed here to prove "exactly one new supervise process," not merely
/// "at least one."
fn count_matching_processes(pattern: &str) -> std::io::Result<usize> {
    let output = Command::new("pgrep").arg("-f").arg(pattern).stdin(Stdio::null()).stderr(Stdio::null()).output()?;
    Ok(String::from_utf8_lossy(&output.stdout).lines().filter(|l| !l.trim().is_empty()).count())
}

/// [`rust/log/tests/fe_client/`'s own `sealed_frames`]: every frame
/// across every sealed `.sotseg` under a real supervisor-owned voyage.
fn sealed_frames(state_dir: &Path, voyage: &str) -> Vec<sot_log::store::envelope::Envelope> {
    let seg_dir = state_dir.join("voyages").join(voyage).join("seg");
    let mut names: Vec<String> =
        std::fs::read_dir(&seg_dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    let mut out = Vec::new();
    for n in names {
        if n.ends_with(".sotseg") {
            out.extend(SegmentReader::read(&seg_dir.join(&n), true).unwrap().frames);
        }
    }
    out
}

/// Sum of file sizes under `dir` — a live, growing proxy for "the record
/// keeps committing," without requiring `end_run` first (frames commit
/// durably as `Commit::Immediate` — see `rust/log/tests/fe_client/`'s own doc on why
/// that makes a mid-flight read honest).
fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir).map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0)
}

fn screen_text(client: &FeAttachClient<DaemonLaneEndpoint>) -> String {
    let (rows, cols) = client.screen().size();
    let mut text = String::new();
    for r in 0..rows {
        for c in 0..cols {
            if let Some(cell) = client.screen().cell(r, c) {
                text.push_str(cell.contents());
            }
        }
        text.push('\n');
    }
    text
}

/// A test-owned TCP -> Unix relay standing in for the loopback tunnel a
/// remote `DaemonLaneEndpoint::LaneDial::Tcp` dials in production: binds
/// `127.0.0.1:0` and forwards each accepted connection to the daemon's
/// own `env.socket_path`, using `tokio::io::copy_bidirectional` for the
/// ordinary (unthrottled) case. Four controls simulate the transport
/// failure modes ADR 0045 decisions 4 and 11 classify: [`Relay::cut`] /
/// [`Relay::resume`] (a dead or restored tunnel — the SAME `addr` stays
/// valid across both, no rebind), [`Relay::blackhole`] (accepted but
/// silent — a wedged peer), [`Relay::throttle`] (a slow downlink, a
/// simple token bucket paced on the daemon-to-client direction only).
struct Relay {
    addr: SocketAddr,
    state: Arc<RelayState>,
    accept_task: tokio::task::JoinHandle<()>,
}

struct RelayState {
    unix_path: PathBuf,
    cutting: AtomicBool,
    blackhole: AtomicBool,
    rate: AtomicU64,
    pipes: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl Relay {
    async fn start(unix_path: PathBuf) -> Relay {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the test relay");
        let addr = listener.local_addr().expect("relay local_addr");
        let state = Arc::new(RelayState {
            unix_path,
            cutting: AtomicBool::new(false),
            blackhole: AtomicBool::new(false),
            rate: AtomicU64::new(0),
            pipes: Mutex::new(Vec::new()),
        });
        let accept_state = Arc::clone(&state);
        let accept_task = tokio::spawn(async move {
            loop {
                let (tcp, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                if accept_state.cutting.load(Ordering::SeqCst) {
                    // "Stop accepting": the handshake itself cannot be
                    // refused without unbinding the port `addr` must stay
                    // valid across — closing the connection the instant
                    // it lands is observably the same to a caller
                    // reading/writing it (`TransportError::Unreachable`,
                    // never a wedge).
                    drop(tcp);
                    continue;
                }
                if accept_state.blackhole.load(Ordering::SeqCst) {
                    let h = tokio::spawn(async move {
                        let mut tcp = tcp;
                        let _ = tokio::io::copy(&mut tcp, &mut tokio::io::sink()).await;
                    });
                    accept_state.pipes.lock().unwrap().push(h);
                    continue;
                }
                let path = accept_state.unix_path.clone();
                let rate_state = Arc::clone(&accept_state);
                let h = tokio::spawn(async move {
                    let Ok(unix) = UnixStream::connect(&path).await else { return };
                    let mut tcp = tcp;
                    let mut unix = unix;
                    if rate_state.rate.load(Ordering::SeqCst) == 0 {
                        let _ = tokio::io::copy_bidirectional(&mut tcp, &mut unix).await;
                        return;
                    }
                    let (mut tcp_r, mut tcp_w) = tokio::io::split(tcp);
                    let (mut unix_r, mut unix_w) = tokio::io::split(unix);
                    let up = tokio::io::copy(&mut tcp_r, &mut unix_w);
                    let down = async {
                        let mut buf = [0u8; 4096];
                        let mut tokens: f64 = 0.0;
                        let mut last = tokio::time::Instant::now();
                        loop {
                            let n = match unix_r.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => n,
                            };
                            let mut off = 0;
                            while off < n {
                                let rate = rate_state.rate.load(Ordering::SeqCst);
                                if rate == 0 {
                                    if tcp_w.write_all(&buf[off..n]).await.is_err() {
                                        return;
                                    }
                                    off = n;
                                    continue;
                                }
                                let now = tokio::time::Instant::now();
                                tokens = (tokens + now.duration_since(last).as_secs_f64() * rate as f64).min(rate as f64);
                                last = now;
                                if tokens < 1.0 {
                                    tokio::time::sleep(Duration::from_millis(20)).await;
                                    continue;
                                }
                                let take = ((n - off) as f64).min(tokens) as usize;
                                if tcp_w.write_all(&buf[off..off + take]).await.is_err() {
                                    return;
                                }
                                tokens -= take as f64;
                                off += take;
                            }
                        }
                    };
                    let _ = tokio::join!(up, down);
                });
                accept_state.pipes.lock().unwrap().push(h);
            }
        });
        Relay { addr, state, accept_task }
    }

    /// Aborts every currently-piped connection and stops completing new
    /// ones — still bound at the SAME `addr`, so `resume()` needs no
    /// rebind and a client retrying against the stable address sees the
    /// SAME outage continue, not a fresh port.
    fn cut(&self) {
        self.state.cutting.store(true, Ordering::SeqCst);
        for h in self.state.pipes.lock().unwrap().drain(..) {
            h.abort();
        }
    }

    fn resume(&self) {
        self.state.cutting.store(false, Ordering::SeqCst);
    }

    fn blackhole(&self, on: bool) {
        self.state.blackhole.store(on, Ordering::SeqCst);
    }

    /// 0 = unthrottled.
    fn throttle(&self, bytes_per_sec: u64) {
        self.state.rate.store(bytes_per_sec, Ordering::SeqCst);
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.accept_task.abort();
        for h in self.state.pipes.lock().unwrap().drain(..) {
            h.abort();
        }
    }
}

fn daemon_lane_endpoint(relay: &Relay) -> DaemonLaneEndpoint {
    DaemonLaneEndpoint { dial: LaneDial::Tcp(relay.addr), token: None }
}

mod dial;
mod outage;
mod pane;
