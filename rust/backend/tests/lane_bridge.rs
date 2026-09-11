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

mod support;
use support::*;

use sot_log::fe_client_io::{FeAttachClient, InputOutcome};
use sot_log::segment::SegmentReader;
use sot_protocol::lane_client::{DaemonLaneEndpoint, LaneDial};
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
    Duration::from_secs(sot_log::wire::CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD as u64 * 5);

fn wake_flag_for_test() -> (Arc<AtomicBool>, Box<dyn Fn() + Send + 'static>) {
    let woke = Arc::new(AtomicBool::new(false));
    let woke2 = Arc::clone(&woke);
    (woke, Box::new(move || woke2.store(true, Ordering::Relaxed)))
}

/// Creates a `runtime: "capsule"` workspace and polls it to `"ready"`,
/// returning its id and the `tmux_session` `lane.connect`'s own `target`
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
    let target = create_res.payload["tmux_session"].as_str().expect("tmux_session").to_string();
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

/// [`rust/log/tests/fe_client.rs`'s own `sealed_frames`]: every frame
/// across every sealed `.sotseg` under a real supervisor-owned voyage.
fn sealed_frames(state_dir: &Path, voyage: &str) -> Vec<sot_log::envelope::Envelope> {
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
/// durably as `Commit::Immediate` — see `fe_client.rs`'s own doc on why
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

// -----------------------------------------------------------------------
// (i) The attach client reaches a supervisor through a daemon in the
// middle.
// -----------------------------------------------------------------------

#[tokio::test]
async fn fe_client_reaches_a_capsule_row_through_the_daemon() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found next to sotd — build it first (cargo build -p sot-log --bin sot-capsule)");

    let env = Env::new("lb1");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_workspace_id, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb1-workspace").await;

    let relay = Relay::start(env.socket_path.clone()).await;
    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach through the daemon bridge");

    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "died before a checkpoint through the bridge: {}", client.status_line());
        assert!(Instant::now() < deadline, "never checkpointed through the bridge");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    client.send_input(b"echo hi\r");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        client.pump();
        if let Some(outcome) = client.last_input_outcome() {
            assert_eq!(outcome, InputOutcome::Recorded, "the first input through the bridge must record");
            break;
        }
        assert!(!client.is_dead(), "died before its input outcome: {}", client.status_line());
        assert!(Instant::now() < deadline, "input outcome never arrived through the bridge");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(client.recorded_bytes(), 8, "recorded_bytes must equal the sent payload's own length");
    assert!(
        client.notice().is_some_and(|n| n.starts_with("attached to leg")),
        "notice() must name the leg, got {:?}",
        client.notice()
    );

    client.request_quit("lane bridge test quit");
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
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(exited, "quit dispatcher never reached record_verified through the bridge (status={})", client.status_line());

    drop(client);
    env.kill_daemon_bounded().await;
}

// -----------------------------------------------------------------------
// (ii) An old daemon (no lane bridge) is a terminal "no bridge" — never a
// second dial attempt.
// -----------------------------------------------------------------------

#[tokio::test]
async fn an_old_daemon_is_a_terminal_no_bridge() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let dials = Arc::new(AtomicUsize::new(0));
    let dials2 = Arc::clone(&dials);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            dials2.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                // The generic "unknown op" shape a daemon predating the
                // lane bridge answers — matches `sot-protocol`'s own
                // `lane_client.rs` unit test `an_unknown_op_reply_is_no_
                // bridge` exactly.
                let res = Frame::res(1, op::LANE_CONNECT, serde_json::json!({ "error": "unknown op: lane.connect" }));
                let mut line = serde_json::to_vec(&res).unwrap();
                line.push(b'\n');
                let _ = stream.write_all(&line);
                std::thread::sleep(Duration::from_millis(300));
            });
        }
    });

    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        DaemonLaneEndpoint { dial: LaneDial::Tcp(addr), token: None },
        "row-old-daemon".to_string(),
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach starts even against a bridge-less daemon");

    let deadline = Instant::now() + Duration::from_secs(10);
    while !client.is_dead() {
        client.pump();
        assert!(Instant::now() < deadline, "client never went terminal against a no-bridge daemon");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(client.status_line().contains("no bridge"), "status must name the missing bridge, got {:?}", client.status_line());

    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(dials.load(Ordering::SeqCst), 1, "a Refused{{no_bridge}} reply must be terminal — never a second dial");
}

// -----------------------------------------------------------------------
// (iii) A lane-only drop is resumed by the client's own next dial.
// -----------------------------------------------------------------------

#[tokio::test]
async fn a_lane_only_drop_is_resumed_by_the_next_dial() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found — build it first");

    let env = Env::new("lb3");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_workspace_id, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb3-workspace").await;

    let relay = Relay::start(env.socket_path.clone()).await;
    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "died before a checkpoint: {}", client.status_line());
        assert!(Instant::now() < deadline, "never checkpointed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let notice_deadline = Instant::now() + Duration::from_secs(5);
    while client.notice().is_none() {
        client.pump();
        assert!(Instant::now() < notice_deadline, "notice() never named a leg before the drop");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let leg_notice = client.notice().map(|s| s.to_string());

    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    kill_supervisor_only(&env.state_root);

    let resumed_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        client.pump();
        assert!(!client.is_dead(), "a lane-only drop must never go terminal: {}", client.status_line());
        if count_matching_processes(&pattern).unwrap_or(0) >= 1 {
            break;
        }
        assert!(Instant::now() < resumed_deadline, "the daemon never resumed the dropped row");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(count_matching_processes(&pattern).unwrap_or(99), 1, "resume_if_absent must spawn exactly one new supervise process");

    // Functional recovery, not merely a resumed process: a fresh input
    // through the SAME client lands, and the leg identity is unchanged.
    client.send_input(b"echo lb3-resumed\r");
    let input_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        client.pump();
        assert!(!client.is_dead(), "died while resuming: {}", client.status_line());
        if screen_text(&client).contains("lb3-resumed") {
            break;
        }
        assert!(Instant::now() < input_deadline, "resumed client's own input never echoed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(client.notice().map(|s| s.to_string()), leg_notice, "notice() must still name the SAME leg after a lane-only drop");
    assert!(!client.is_dead(), "the client must never have gone terminal across the drop");

    drop(client);
    relay.cut();
    env.kill_daemon_bounded().await;
}

// -----------------------------------------------------------------------
// (iv)+(v) A blackhole is unreachable and retried, never terminal; a cut
// tunnel does not charge the health window.
// -----------------------------------------------------------------------

#[tokio::test]
async fn a_blackhole_is_unreachable_and_retried() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found — build it first");

    let env = Env::new("lb4");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (_workspace_id, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb4-workspace").await;

    let relay = Relay::start(env.socket_path.clone()).await;
    relay.blackhole(true);

    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach starts even against a blackholed relay");

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        client.pump();
        assert!(!client.is_dead(), "must never go terminal while blackholed: {}", client.status_line());
        if client.status_line().contains("daemon unreachable") {
            break;
        }
        assert!(Instant::now() < deadline, "status never reported \"daemon unreachable\" within 3s, got {:?}", client.status_line());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        client.pump();
        assert!(!client.is_dead(), "must never go terminal while blackholed: {}", client.status_line());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    relay.blackhole(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "died after un-blackholing: {}", client.status_line());
        assert!(Instant::now() < deadline, "never checkpointed after un-blackholing");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    relay.cut();
    let cut_deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < cut_deadline {
        client.pump();
        assert!(!client.is_dead(), "must not die during a 30s tunnel cut: {}", client.status_line());
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    relay.resume();

    kill_supervisor_only(&env.state_root);

    let alive_deadline = Instant::now() + Duration::from_secs(100);
    while Instant::now() < alive_deadline {
        client.pump();
        assert!(!client.is_dead(), "the prior 30s outage must not have been charged to the health window: {}", client.status_line());
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    drop(client);
    relay.cut();
    env.kill_daemon_bounded().await;
}

// -----------------------------------------------------------------------
// (v) A daemon outage past the health window keeps retrying, never
// terminal — recovery is the daemon's own next dial, outstanding input
// survives.
// -----------------------------------------------------------------------

#[tokio::test]
async fn a_daemon_outage_past_the_window_keeps_retrying() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found — build it first");

    let env = Env::new("lb5");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (workspace_id, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb5-workspace").await;
    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    let row = find_row(&list_payload, &workspace_id).expect("row listed");
    let state_dir = PathBuf::from(row["state_dir"].as_str().expect("state_dir"));

    let relay = Relay::start(env.socket_path.clone()).await;
    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "died before a checkpoint: {}", client.status_line());
        assert!(Instant::now() < deadline, "never checkpointed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let marker: &[u8] = b"echo lb5pre\r"; // 12 bytes -- a length unique to this send
    client.send_input(marker);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        client.pump();
        if client.last_input_outcome() == Some(InputOutcome::Recorded) {
            break;
        }
        assert!(!client.is_dead(), "died before its pre-cut outcome: {}", client.status_line());
        assert!(Instant::now() < deadline, "pre-cut input never recorded");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    relay.cut();
    let cut_deadline = Instant::now() + Duration::from_secs(130);
    while Instant::now() < cut_deadline {
        client.pump();
        assert!(!client.is_dead(), "a 130s outage must still be retrying, never terminal: {}", client.status_line());
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(client.status_line().contains("unreachable"), "status must show the retry, got {:?}", client.status_line());

    relay.resume();
    let attached_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        client.pump();
        assert!(!client.is_dead(), "died while resuming: {}", client.status_line());
        if screen_text(&client).chars().any(|c| !c.is_whitespace()) {
            break;
        }
        assert!(Instant::now() < attached_deadline, "never resumed after the daemon outage ended");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let marker2: &[u8] = b"echo lb5post\r"; // 13 bytes -- distinct length
    client.send_input(marker2);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        client.pump();
        if screen_text(&client).contains("lb5post") {
            break;
        }
        assert!(!client.is_dead(), "died before the post-resume input echoed: {}", client.status_line());
        assert!(Instant::now() < deadline, "post-resume input never echoed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let voyage = tokio::task::spawn_blocking({
        let d = state_dir.clone();
        move || sot_log::supervisor_client::query_status(&d).expect("query_status").0.voyage
    })
    .await
    .unwrap()
    .expect("a ready row has a voyage");
    let outcome = tokio::task::spawn_blocking({
        let d = state_dir.clone();
        let v = voyage.clone();
        move || sot_log::supervisor_client::end_run(&d, &v, "lane bridge test teardown")
    })
    .await
    .unwrap()
    .expect("end_run");
    assert!(
        matches!(
            outcome,
            sot_log::supervisor_client::EndRunOutcome::RecordVerified | sot_log::supervisor_client::EndRunOutcome::RecordClosed
        ),
        "end_run did not verify: {outcome:?}"
    );

    let frames = sealed_frames(&state_dir, &voyage);
    let recorded_once = frames
        .iter()
        .filter(|f| {
            f.class == sot_log::envelope::Class::Input
                && f.source.actor.controller_id.as_deref() == Some("test-fe")
                && f.payload.as_ref().and_then(|p| p.get("length")?.as_u64()).map(|l| l as usize) == Some(marker.len())
        })
        .count();
    assert_eq!(recorded_once, 1, "the pre-cut input must resolve Recorded exactly once in the sealed record");

    drop(client);
    env.kill_daemon_bounded().await;
}

// -----------------------------------------------------------------------
// (vi) A terminal row is terminal after the health window — every dial
// answers lane_absent, no supervise is ever spawned.
// -----------------------------------------------------------------------

#[tokio::test]
async fn a_terminal_row_is_terminal_after_the_window() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found — build it first");

    let env = Env::new("lb6");
    env.seed_default_capsule_toml("claude");
    let fake_claude_dir = env.seed_fake_unlaunchable_claude();
    env.spawn_sotd_with_prepended_path(&fake_claude_dir);
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;

    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let default_row = list_payload["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .find(|w| w["is_default"].as_bool() == Some(true))
        .cloned()
        .expect("a default workspace row");
    let default_workspace_id = default_row["workspace_id"].as_str().expect("workspace_id").to_string();
    let default_target = default_row["tmux_session"].as_str().expect("tmux_session").to_string();

    let pty_req = serde_json::json!({ "cols": 80, "rows": 24, "user_switch": true, "target": default_target });
    let pty_res = call(&mut conn, next_id, op::PTY_OPEN, pty_req).await;
    next_id += 1;
    assert_eq!(pty_res.payload["code"], "attach_direct", "pty.open payload: {:?}", pty_res.payload);

    let terminal_deadline = Instant::now() + BOUND;
    loop {
        let id = next_id;
        next_id += 1;
        let payload = call(&mut conn, id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
        if let Some(row) = find_row(&payload, &default_workspace_id) {
            if row["phase"].as_str() == Some("terminal") {
                break;
            }
        }
        assert!(Instant::now() < terminal_deadline, "timed out waiting for the unlaunchable-agent row to reach \"terminal\"");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let pattern = build_leg_pgrep_pattern(&sot_capsule_exe(), "supervise", &env.state_root);
    assert!(!any_process_matches(&pattern), "the row is terminal — no supervise process should remain live");

    let relay = Relay::start(env.socket_path.clone()).await;
    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        daemon_lane_endpoint(&relay),
        default_target,
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach starts even against a terminal row");

    let start = Instant::now();
    let mut last_pgrep_check = Instant::now();
    loop {
        client.pump();
        if client.is_dead() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(150), "must reach HealthWindowExpired within 150s of a terminal row, status={}", client.status_line());
        if last_pgrep_check.elapsed() >= Duration::from_secs(10) {
            assert!(!any_process_matches(&pattern), "a terminal row must never be resumed — a supervise process appeared");
            last_pgrep_check = Instant::now();
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let elapsed = start.elapsed();
    assert!(elapsed >= Duration::from_secs(120), "went terminal too early ({elapsed:?}) — the health window must run its full 120s");
    assert!(client.status_line().contains("HealthWindowExpired"), "status must name HealthWindowExpired, got {:?}", client.status_line());
    assert!(!any_process_matches(&pattern), "a terminal row must never be resumed");

    drop(client);
    relay.cut();
    env.kill_daemon_bounded().await;
}

// -----------------------------------------------------------------------
// (vii) A busy pane over a throttled relay converges — recording never
// waits on the slow subscriber.
// -----------------------------------------------------------------------

#[tokio::test]
async fn a_busy_pane_over_a_slow_link_converges() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found — build it first");

    let env = Env::new("lb7");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (workspace_id, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb7-workspace").await;
    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    let row = find_row(&list_payload, &workspace_id).expect("row listed");
    let state_dir = PathBuf::from(row["state_dir"].as_str().expect("state_dir"));

    let relay = Relay::start(env.socket_path.clone()).await;
    relay.throttle(256 * 1024);

    let (_woke, wake) = wake_flag_for_test();
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        "test-fe".to_string(),
        "test-fe".to_string(),
        None,
        wake,
    )
    .expect("attach over a throttled relay");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !client.is_checkpointed() {
        client.pump();
        assert!(!client.is_dead(), "died before a checkpoint over the throttled relay: {}", client.status_line());
        assert!(Instant::now() < deadline, "never checkpointed over the throttled relay");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    client.send_input(b"yes\r");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        client.pump();
        if client.last_input_outcome() == Some(InputOutcome::Recorded) {
            break;
        }
        assert!(!client.is_dead(), "died before \"yes\" recorded: {}", client.status_line());
        assert!(Instant::now() < deadline, "\"yes\" was never recorded");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let voyage = tokio::task::spawn_blocking({
        let d = state_dir.clone();
        move || sot_log::supervisor_client::query_status(&d).expect("query_status").0.voyage
    })
    .await
    .unwrap()
    .expect("a ready row has a voyage");
    let seg_dir = state_dir.join("voyages").join(&voyage).join("seg");
    let initial_bytes = dir_bytes(&seg_dir);

    let mut saw_reconnect = false;
    let mut last_status = client.status_line().to_string();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        client.pump();
        assert!(!client.is_dead(), "must not go terminal under a busy pane over a slow link: {}", client.status_line());
        let cur = client.status_line().to_string();
        if cur != last_status {
            if cur.contains("connecting") || cur.contains("unreachable") || cur.contains("not answering") {
                saw_reconnect = true;
            }
            last_status = cur;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let grown_bytes = dir_bytes(&seg_dir);
    assert!(grown_bytes > initial_bytes, "the record must keep growing under the flood: {initial_bytes} -> {grown_bytes}");
    assert!(saw_reconnect, "a client lagging behind the flood over a throttled link must be dropped and reattach at least once");

    relay.throttle(0);
    client.send_input(&[0x03]);
    let deadline = Instant::now() + CHECKPOINT_TRANSFER_BUDGET;
    loop {
        client.pump();
        if client.last_input_outcome() == Some(InputOutcome::Recorded)
            && !client.status_line().contains("unreachable")
            && !client.status_line().contains("connecting")
        {
            break;
        }
        assert!(!client.is_dead(), "died while converging after the flood: {}", client.status_line());
        assert!(Instant::now() < deadline, "never converged after the flood within the checkpoint transfer budget");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    drop(client);
    relay.cut();
    env.kill_daemon_bounded().await;
}

// -----------------------------------------------------------------------
// (viii) `rust/log/tests/fe_client.rs`'s own pen-contention proof,
// repeated through the bridge: a headless write via the daemon's OWN
// `pty.input` demotes the bridged FE; its next input retakes; watcher
// screen reads never take.
// -----------------------------------------------------------------------

#[tokio::test]
async fn headless_write_while_a_bridged_client_is_driving_demotes_it_without_duplicating_input() {
    let _serial = SERIAL.lock().await;
    assert!(sot_capsule_exe().is_file(), "{CAPSULE_EXE_NAME} not found — build it first");

    let env = Env::new("lb8");
    env.spawn_sotd();
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let (workspace_id, target) = create_ready_capsule_row(&env, &mut conn, &mut next_id, "lb8-workspace").await;
    let list_payload = call(&mut conn, next_id, op::WORKSPACE_LIST, serde_json::json!({})).await.payload;
    next_id += 1;
    let row = find_row(&list_payload, &workspace_id).expect("row listed");
    let state_dir = PathBuf::from(row["state_dir"].as_str().expect("state_dir"));

    let relay = Relay::start(env.socket_path.clone()).await;
    let (_woke, wake) = wake_flag_for_test();
    let mut driver = FeAttachClient::<DaemonLaneEndpoint>::attach(
        daemon_lane_endpoint(&relay),
        target,
        80,
        24,
        "lb8-driver".to_string(),
        "lb8-driver".to_string(),
        None,
        wake,
    )
    .expect("attach the driving client through the bridge");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !driver.is_checkpointed() {
        driver.pump();
        assert!(!driver.is_dead(), "driver died before a checkpoint: {}", driver.status_line());
        assert!(Instant::now() < deadline, "driver never checkpointed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A watcher read BEFORE any write must not take the pen.
    let pre_screen = call(&mut conn, next_id, op::PTY_SCREEN, serde_json::json!({ "workspace_id": workspace_id })).await;
    next_id += 1;
    assert!(pre_screen.payload.get("error").is_none(), "pty.screen before any write failed: {:?}", pre_screen.payload);

    driver.send_input(b"echo A1\r\n");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        driver.pump();
        if screen_text(&driver).contains("A1") {
            break;
        }
        assert!(!driver.is_dead(), "driver died before its first input echoed: {}", driver.status_line());
        assert!(Instant::now() < deadline, "driver's first input never echoed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // A watcher read WHILE the bridged driver holds the pen must not
    // itself take it either.
    let mid_screen = call(&mut conn, next_id, op::PTY_SCREEN, serde_json::json!({ "workspace_id": workspace_id })).await;
    next_id += 1;
    assert!(mid_screen.payload.get("error").is_none(), "pty.screen while driving failed: {:?}", mid_screen.payload);

    // The daemon's OWN headless write (never a second `FeAttachClient`)
    // lands on the same row and must demote the bridged driver.
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    let headless_text = "echo HEADLESSB";
    let input_req = serde_json::json!({
        "workspace_id": workspace_id,
        "data_b64": STANDARD.encode(headless_text.as_bytes()),
        "enter": true,
        "origin": "lb8-headless",
    });
    let input_res = call(&mut conn, next_id, op::PTY_INPUT, input_req).await;
    assert!(input_res.payload.get("error").is_none(), "the daemon's own headless pty.input failed: {:?}", input_res.payload);
    assert_eq!(input_res.payload["ok"], true, "headless pty.input: {:?}", input_res.payload);

    // The driver's own NEXT input: its worker sees refused_stale and
    // retakes automatically (ruling (c)) — invisible to this test except
    // for the eventual echo.
    driver.send_input(b"echo A2RETAKE\r\n");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        driver.pump();
        if screen_text(&driver).contains("A2RETAKE") {
            break;
        }
        assert!(!driver.is_dead(), "driver died before its retake echoed: {}", driver.status_line());
        assert!(Instant::now() < deadline, "driver's post-demotion retake never delivered");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    drop(driver);

    let voyage = tokio::task::spawn_blocking({
        let d = state_dir.clone();
        move || sot_log::supervisor_client::query_status(&d).expect("query_status").0.voyage
    })
    .await
    .unwrap()
    .expect("a ready row has a voyage");
    let outcome = tokio::task::spawn_blocking({
        let d = state_dir.clone();
        let v = voyage.clone();
        move || sot_log::supervisor_client::end_run(&d, &v, "lane bridge test teardown")
    })
    .await
    .unwrap()
    .expect("end_run");
    assert!(
        matches!(
            outcome,
            sot_log::supervisor_client::EndRunOutcome::RecordVerified | sot_log::supervisor_client::EndRunOutcome::RecordClosed
        ),
        "end_run did not verify: {outcome:?}"
    );

    let frames = sealed_frames(&state_dir, &voyage);
    let count_for = |cid: &str| {
        frames.iter().filter(|f| f.class == sot_log::envelope::Class::Input && f.source.actor.controller_id.as_deref() == Some(cid)).count()
    };
    assert_eq!(count_for("lb8-headless"), 1, "the headless write must appear EXACTLY once in the sealed record");
    assert_eq!(count_for("lb8-driver"), 3, "the driver's record: A1 (clean) + A2RETAKE's refused-stale attempt + A2RETAKE's successful retry");
    let refused_stale_count = frames
        .iter()
        .filter(|f| {
            f.class == sot_log::envelope::Class::Lifecycle
                && f.source.actor.controller_id.as_deref() == Some("lb8-driver")
                && f.payload.as_ref().and_then(|p| p.get("fact")?.get("fact")?.as_str()) == Some("refused_stale_epoch")
        })
        .count();
    assert_eq!(refused_stale_count, 1, "exactly ONE of the driver's attempts must be the refused-stale one the demotion causes");

    env.kill_daemon_bounded().await;
}
