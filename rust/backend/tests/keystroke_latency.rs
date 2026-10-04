#![cfg(target_os = "linux")]
//! Keystroke latency against a PRIVATE daemon: timing, not CI, so every
//! test is `#[ignore]`. Run with
//! `cargo test -p sot-backend --test keystroke_latency -- --ignored --nocapture --test-threads=1`.
//!
//! Each `k*` test drives the real headless attach client
//! (`FeAttachClient<DaemonLaneEndpoint>`) at a bash capsule row of a daemon
//! the harness started with its own HOME, state root and socket, takes the
//! pen, then 200 times sends one byte and polls every 0.2 ms for two events:
//! `t_ack` (`recorded_bytes` grew: the capsule acknowledged the input) and
//! `t_echo` (the byte is on the client's screen). Between samples it sleeps a
//! seeded-random 0-100 ms. It measures the lane from client to capsule and
//! back; it does not include the frontend's render or present.

mod support;
use support::*;

use sot_log::attach_client::client::FeAttachClient;
use sot_protocol::topology::lane_client::{DaemonLaneEndpoint, LaneDial};
use sot_protocol::{codec, op, Frame, HelloReq};

use std::io::{BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tokio::net::{TcpListener, UnixStream};

const SAMPLES: usize = 200;
const PROBE: u8 = b'Z';

fn screen_count(client: &FeAttachClient<DaemonLaneEndpoint>, byte: u8) -> usize {
    let (rows, cols) = client.screen().size();
    let mut n = 0;
    for r in 0..rows {
        for c in 0..cols {
            if let Some(cell) = client.screen().cell(r, c) {
                n += cell.contents().bytes().filter(|b| *b == byte).count();
            }
        }
    }
    n
}

fn percentiles(label: &str, mut v: Vec<Duration>) {
    v.sort();
    let at = |q: f64| v[(((v.len() - 1) as f64) * q).round() as usize].as_secs_f64() * 1000.0;
    println!("{label}: n={} p50={:.2}ms p95={:.2}ms max={:.2}ms", v.len(), at(0.5), at(0.95), at(1.0));
}

/// Plain loopback TCP in front of the daemon's Unix socket.
async fn tcp_front(unix_path: std::path::PathBuf) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else { break };
            let path = unix_path.clone();
            tokio::spawn(async move {
                let Ok(mut unix) = UnixStream::connect(&path).await else { return };
                let _ = tokio::io::copy_bidirectional(&mut tcp, &mut unix).await;
            });
        }
    });
    addr
}

async fn create_row(env: &Env, conn: &mut Conn, next_id: &mut u64) -> String {
    let req = serde_json::json!({
        "label": "latency",
        "project_root": env.workspace_project_root.to_string_lossy(),
        "runtime": "capsule",
    });
    let res = call(conn, *next_id, op::WORKSPACE_CREATE, req).await;
    *next_id += 1;
    assert!(res.payload.get("error").is_none(), "workspace.create failed: {:?}", res.payload);
    let id = res.payload["workspace_id"].as_str().expect("workspace_id").to_string();
    let target = res.payload["session_name"].as_str().expect("session_name").to_string();
    poll_for_phase(conn, next_id, &id, "ready", Duration::from_secs(90)).await;
    target
}

/// Polls until `done`, pumping the client; returns the instant it was seen.
fn wait_for(client: &mut FeAttachClient<DaemonLaneEndpoint>, what: &str, done: impl Fn(&FeAttachClient<DaemonLaneEndpoint>) -> bool) -> Instant {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        client.pump();
        if done(client) {
            return Instant::now();
        }
        assert!(!client.is_dead(), "client died waiting for {what}: {}", client.status_line());
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_micros(200));
    }
}

async fn measure(env: &Env, tcp: bool) {
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let target = create_row(env, &mut conn, &mut next_id).await;
    let dial = if tcp { LaneDial::Tcp(tcp_front(env.socket_path.clone()).await) } else { LaneDial::Local(env.socket_path.clone()) };
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        DaemonLaneEndpoint { dial, token: None },
        target,
        80,
        24,
        "latency-fe".to_string(),
        "latency-fe".to_string(),
        None,
        Box::new(|| {}),
    )
    .expect("attach");
    wait_for(&mut client, "checkpoint", |c| c.is_checkpointed());
    // The first input takes the pen; settle before measuring.
    client.send_input(&[PROBE]);
    wait_for(&mut client, "first ack", |c| c.recorded_bytes() >= 1);
    std::thread::sleep(Duration::from_millis(500));
    client.pump();

    let mut rng: u64 = 0x2545_F491_4F6C_DD1D;
    let (mut acks, mut echoes) = (Vec::new(), Vec::new());
    for _ in 0..SAMPLES {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        std::thread::sleep(Duration::from_millis(rng % 101));
        client.pump();
        let base = screen_count(&client, PROBE);
        let before = client.recorded_bytes();
        let t0 = Instant::now();
        client.send_input(&[PROBE]);
        let mut t_ack = None;
        let deadline = t0 + Duration::from_secs(10);
        let t_echo = loop {
            client.pump();
            if t_ack.is_none() && client.recorded_bytes() > before {
                t_ack = Some(Instant::now());
            }
            if screen_count(&client, PROBE) > base {
                break Instant::now();
            }
            assert!(!client.is_dead(), "client died: {}", client.status_line());
            assert!(Instant::now() < deadline, "echo never arrived");
            std::thread::sleep(Duration::from_micros(200));
        };
        acks.push(t_ack.unwrap_or(t_echo) - t0);
        echoes.push(t_echo - t0);
        // Keep the line short: kill it (Ctrl-U) and wait for the screen to clear.
        if screen_count(&client, PROBE) >= 60 {
            client.send_input(&[0x15]);
            wait_for(&mut client, "line clear", |c| screen_count(c, PROBE) < 5);
        }
    }
    percentiles("t_ack ", acks);
    percentiles("t_echo", echoes);
    drop(client);
    env.kill_daemon_bounded().await;
}

fn fsync_per_op(dir: &Path) -> f64 {
    let f = dir.join("fsync-probe");
    let t0 = Instant::now();
    let st = Command::new("dd")
        .args(["if=/dev/zero", "bs=512", "count=200", "oflag=dsync"])
        .arg(format!("of={}", f.display()))
        .stderr(Stdio::null())
        .status()
        .expect("dd");
    assert!(st.success());
    let per = t0.elapsed().as_secs_f64() * 1000.0 / 200.0;
    let _ = std::fs::remove_file(&f);
    per
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn k1_loopback_unix() {
    let env = Env::new("k1");
    env.spawn_sotd();
    println!("k1 fsync per op on the state root: {:.3}ms", fsync_per_op(&env.state_root));
    println!("k1_loopback_unix");
    measure(&env, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn k2_loopback_unix_tmpfs() {
    // The daemon refuses a capsule row on a tmpfs state root (decision 23,
    // `state_root_unqualified`), so only the fsync cost can be taken here.
    let env = Env::new_with_state_root_on_tmpfs("k2");
    println!("k2 fsync per op on the tmpfs state root: {:.3}ms", fsync_per_op(&env.state_root));
    println!("k2_loopback_unix_tmpfs: no row can be created on tmpfs, so no keystroke samples");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn k_tcp_loopback() {
    let env = Env::new("ktcp");
    env.spawn_sotd();
    println!("k_tcp_loopback");
    measure(&env, true).await;
}

/// The bridge resolves its socket as `$XDG_RUNTIME_DIR/sot/sessions/sot.sock`,
/// which the harness daemon (`--socket <path>`) does not bind; a symlink in a
/// private temp runtime dir points that name at the private daemon's socket.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn k3b_ssh_cold_dial() {
    let env = Env::new("k3b");
    env.spawn_sotd();
    let (mut conn, next_id) = connect_and_hello(&env.socket_path).await;

    let rt = tempfile::Builder::new().prefix("sotk3b-").tempdir_in("/tmp").expect("runtime dir");
    let sessions = rt.path().join("sot").join("sessions");
    std::fs::create_dir_all(&sessions).expect("mkdir");
    {
        use std::os::unix::fs::PermissionsExt;
        for d in [rt.path().to_path_buf(), rt.path().join("sot"), sessions.clone()] {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        }
    }
    std::os::unix::fs::symlink(&env.socket_path, sessions.join("sot.sock")).expect("symlink");
    assert!(sot_log::host::state_dir::is_private_dir(rt.path()), "temp runtime dir is not private");
    assert!(sot_log::host::state_dir::is_private_dir(&rt.path().join("sot")), "temp sot dir is not private");
    // `test -S` fails closed: where this temp dir does not exist (another /tmp behind ssh), the
    // bridge never starts, so it can never fall back to the live runtime dir's socket.
    let remote = format!(
        "test -S {} && env XDG_RUNTIME_DIR={} {} stdio-bridge",
        sessions.join("sot.sock").display(),
        rt.path().display(),
        sotd_exe().display()
    );

    let hello = || {
        let h = HelloReq {
            client_id: "k3b".to_string(),
            session_id: None,
            last_seen_revision: 0,
            token: None,
            protocol: sot_protocol::PROTOCOL_VERSION,
            app_version: sot_protocol::app_version(),
            host: None,
            role: String::new(),
            instance: None,
            name: None,
        };
        Frame::req(1, op::HELLO, serde_json::to_value(&h).unwrap())
    };
    // The private daemon's session id; the local run's reply must carry the same one.
    let private_sid = call(&mut conn, next_id, op::HELLO, hello().payload).await.payload["session_id"].clone();
    assert!(private_sid.is_string(), "private daemon hello carries no session_id");
    // One dial: spawn `cmd`, send hello, wait up to 10 s for the first reply frame. The
    // child is always killed and reaped.
    let dial = |mut cmd: Command| -> Option<(Duration, Frame)> {
        let t0 = Instant::now();
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("spawn");
        let mut stdin = child.stdin.take().unwrap();
        let ok = codec::write_frame_blocking(&mut stdin, &hello()).is_ok() && stdin.flush().is_ok();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let reply = if ok {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(codec::read_frame_blocking(&mut out).ok());
            });
            rx.recv_timeout(Duration::from_secs(10)).ok().flatten()
        } else {
            None
        };
        let t = t0.elapsed();
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        reply.filter(|f| f.payload.get("error").is_none()).map(|f| (t, f))
    };

    let mut local = Command::new("sh");
    local.arg("-c").arg(&remote);
    let local_reply = dial(local);
    if let Some((_, f)) = &local_reply {
        assert_eq!(f.payload["session_id"], private_sid, "the local run reached a daemon other than the private one");
    }
    let local_ok = local_reply.map(|(t, _)| t);
    println!("k3b local run of the remote command reaches the private daemon (session id matched): {}", local_ok.is_some());
    let ssh_ok = Command::new("ssh").args(["-T", "-o", "BatchMode=yes", "localhost", "true"]).stdin(Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
    println!("k3b ssh localhost without a password: {ssh_ok}");

    let mut times = Vec::new();
    for _ in 0..5 {
        if local_ok.is_some() && ssh_ok {
            let mut c = Command::new("ssh");
            c.args(["-T", "-o", "BatchMode=yes", "localhost", &remote]);
            let (t, f) = dial(c).expect("ssh bridge dial");
            assert_eq!(f.payload["session_id"], private_sid, "an ssh dial reached a daemon other than the private one");
            times.push(t);
        } else {
            let t0 = Instant::now();
            let _ = Command::new("ssh").args(["-T", "-o", "BatchMode=yes", "localhost", "true"]).stdin(Stdio::null()).status();
            times.push(t0.elapsed());
        }
    }
    println!("k3b measured: {}", if local_ok.is_some() && ssh_ok { "ssh + bridge hello" } else { "ssh true only" });
    percentiles("k3b dial", times);
    env.kill_daemon_bounded().await;
}
