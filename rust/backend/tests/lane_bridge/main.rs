#![cfg(target_os = "linux")]
//! ADR 0045 lane B4b: the cross-process proofs that an FE attach client
//! reaches a real capsule row THROUGH a real daemon in the middle, over
//! a test-owned Unix-socket relay standing in for the link a remote
//! `DaemonLaneEndpoint` reaches its daemon by (`sot-protocol`'s own
//! `lane_client.rs`, lane B4a), which the tests can cut, blackhole and
//! throttle. Shares `Env` and
//! the wire-protocol round-trip helpers with `capsule_workspaces/main.rs` via
//! `tests/support/mod.rs` (a pure lift there, no behavior change).
//!
//! Decision 11's own words are what each test below proves piece by
//! piece: "a slow or interrupted link is never mistaken for a dead row,
//! and recording never blocks on either."

#[path = "../support/mod.rs"]
mod support;
use support::*;

use sot_log::attach_client::client::{FeAttachClient, InputOutcome};
use sot_topology::lane_client::{DaemonLaneEndpoint, LaneDial};
use sot_protocol::{op, Frame};

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

/// Mirrors `capsule_workspaces/main.rs`'s own `SERIAL`: this file's real
/// `sotd`/`sot-capsule` processes share the same per-process
/// `SOT_RUNTIME_DIR` env var `Env::new` sets, so parallel tests within
/// THIS binary would race it exactly the same way.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// How long the pane test lets a snapshot transfer take over its throttled relay: the 60 s the old attach lane's
/// checkpoint transfer was bounded at (12 chunks of 5 s), kept as the one bound.
const CHECKPOINT_TRANSFER_BUDGET: Duration = Duration::from_secs(60);

/// Creates a `runtime: "capsule"` workspace and polls it to `"ready"`,
/// returning its id and the `session_name` `lane.connect`'s own `target`
/// names — `workspace.create`'s reply already carries it (mirrors
/// `capsule_workspaces/lane_connect.rs`'s own `lane_connect_supervisor_pipes_hello_
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

/// When the row's screen file (`<state_dir>/screen`) was last written, if it has been: a live, growing proxy for "the
/// capsule keeps handling output", without requiring `end_run` first.
fn screen_modified(state_dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(state_dir.join("screen")).and_then(|m| m.modified()).ok()
}

fn screen_text(client: &FeAttachClient) -> String {
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

/// A test-owned Unix-socket relay standing in for the link a remote
/// `DaemonLaneEndpoint` reaches its daemon by: binds a socket in its own
/// temp folder and forwards each accepted connection to the daemon's
/// own `env.socket_path`, using `tokio::io::copy_bidirectional` for the
/// ordinary (unthrottled) case. Four controls simulate the transport
/// failure modes ADR 0045 decisions 4 and 11 classify: [`Relay::cut`] /
/// [`Relay::resume`] (a dead or restored tunnel — the SAME `path` stays
/// valid across both, no rebind), [`Relay::blackhole`] (accepted but
/// silent — a wedged peer), [`Relay::throttle`] (a slow downlink, a
/// simple token bucket paced on the daemon-to-client direction only).
struct Relay {
    path: PathBuf,
    /// Keeps the relay's socket folder alive: a literal `/tmp` base, since `sun_path` is short.
    _dir: tempfile::TempDir,
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
        let dir = tempfile::Builder::new().prefix("sot-relay-").permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700)).tempdir_in("/tmp").expect("relay folder");
        let path = dir.path().join("relay.sock");
        let listener = UnixListener::bind(&path).expect("bind the test relay");
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
                let (front, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                if accept_state.cutting.load(Ordering::SeqCst) {
                    // "Stop accepting": the handshake itself cannot be
                    // refused without unbinding the socket `path` must stay
                    // valid across — closing the connection the instant
                    // it lands is observably the same to a caller
                    // reading/writing it (`TransportError::Unreachable`,
                    // never a wedge).
                    drop(front);
                    continue;
                }
                if accept_state.blackhole.load(Ordering::SeqCst) {
                    let h = tokio::spawn(async move {
                        let mut front = front;
                        let _ = tokio::io::copy(&mut front, &mut tokio::io::sink()).await;
                    });
                    accept_state.pipes.lock().unwrap().push(h);
                    continue;
                }
                let path = accept_state.unix_path.clone();
                let rate_state = Arc::clone(&accept_state);
                let h = tokio::spawn(async move {
                    let Ok(unix) = UnixStream::connect(&path).await else { return };
                    let mut front = front;
                    let mut unix = unix;
                    if rate_state.rate.load(Ordering::SeqCst) == 0 {
                        let _ = tokio::io::copy_bidirectional(&mut front, &mut unix).await;
                        return;
                    }
                    let (mut front_r, mut front_w) = tokio::io::split(front);
                    let (mut unix_r, mut unix_w) = tokio::io::split(unix);
                    let up = tokio::io::copy(&mut front_r, &mut unix_w);
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
                                    if front_w.write_all(&buf[off..n]).await.is_err() {
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
                                if front_w.write_all(&buf[off..off + take]).await.is_err() {
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
        Relay { path, _dir: dir, state, accept_task }
    }

    /// Aborts every currently-piped connection and stops completing new
    /// ones — still bound at the SAME `path`, so `resume()` needs no
    /// rebind and a client retrying against the stable path sees the
    /// SAME outage continue, not a fresh socket.
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
    DaemonLaneEndpoint::new(LaneDial::Local(relay.path.clone()), None)
}

mod diagnostics;
mod dial;
mod outage;
mod pane;
