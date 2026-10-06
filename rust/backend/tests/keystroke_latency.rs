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

async fn measure(env: &Env) {
    let (mut conn, mut next_id) = connect_and_hello(&env.socket_path).await;
    let target = create_row(env, &mut conn, &mut next_id).await;
    let dial = LaneDial::Local(env.socket_path.clone());
    let mut client = FeAttachClient::<DaemonLaneEndpoint>::attach(
        DaemonLaneEndpoint::new(dial, None),
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
    measure(&env).await;
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

/// Local setup shared by k3b and k5: retain the first accepted hello, never send a duplicate.
async fn first_hello_setup(env: &Env) -> (Conn, u64, Frame) {
    let stream = poll_until(|| async { try_connect(&env.socket_path).await }, BOUND, "private daemon socket").await;
    let mut conn = tokio::io::BufReader::new(stream);
    let reply = call(&mut conn, 1, op::HELLO, hello_frame("latency-test").payload).await;
    assert!(reply.payload.get("error").is_none(), "private hello refused: {:?}", reply.payload);
    assert!(reply.payload["session_id"].is_string(), "private hello carries no session_id");
    (conn, 2, reply)
}

fn hello_frame(client_id: &str) -> Frame {
    let hello = HelloReq::this_process(client_id, "", Some("host-a".to_string())).expect("this process's account");
    Frame::req(1, op::HELLO, serde_json::to_value(hello).unwrap())
}

/// The private runtime symlink and a fail-closed bridge command; its owner stays alive throughout the proof.
fn private_bridge_remote(env: &Env) -> (tempfile::TempDir, String) {
    let rt = tempfile::Builder::new().prefix("sotk-").tempdir_in("/tmp").expect("runtime dir");
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
        sotd_program().display()
    );

    (rt, remote)
}

/// One login plus its private hello. Kill and bounded-reap the owned child on every result.
fn bridge_hello_dial(mut cmd: Command, hello: &Frame) -> Option<(Duration, Frame)> {
    let t0 = Instant::now();
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("spawn");
    let mut stdin = child.stdin.take().unwrap();
    let ok = codec::write_frame_blocking(&mut stdin, hello).is_ok() && stdin.flush().is_ok();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || { let _ = tx.send(codec::read_frame_blocking(&mut out).ok()); });
    let reply = if ok { rx.recv_timeout(Duration::from_secs(10)).ok().flatten() } else { None };
    let elapsed = t0.elapsed();
    drop(stdin);
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().expect("wait for owned login").is_none() {
        assert!(Instant::now() < deadline, "owned login did not reap");
        std::thread::sleep(Duration::from_millis(5));
    }
    while !reader.is_finished() {
        assert!(Instant::now() < deadline, "owned hello reader did not exit");
        std::thread::sleep(Duration::from_millis(5));
    }
    reader.join().unwrap();
    reply.filter(|f| f.payload.get("error").is_none()).map(|f| (elapsed, f))
}

fn private_ssh_command(remote: &str) -> Command {
    let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("localhost", None).unwrap();
    let gate = sot_protocol::topology::ssh_bridge::LinkGate::default();
    let command = gate.command(&recipe).expect("up gate");
    let mut args: Vec<_> = command.get_args().map(|arg| arg.to_owned()).collect();
    args.pop().expect("remote command");
    let mut ssh = Command::new(command.get_program());
    ssh.args(args).arg(remote);
    ssh
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn k3b_ssh_cold_dial() {
    let env = Env::new("k3b");
    env.spawn_sotd();
    let (_conn, _next_id, first_hello) = first_hello_setup(&env).await;
    let private_sid = &first_hello.payload["session_id"];
    let (_runtime, remote) = private_bridge_remote(&env);
    let mut local = Command::new("sh");
    local.arg("-c").arg(&remote);
    let (_, reply) = bridge_hello_dial(local, &hello_frame("k3b")).expect("local private bridge hello");
    assert_eq!(&reply.payload["session_id"], private_sid, "local bridge reached another daemon");
    println!("k3b local run of the remote command reaches the private daemon (session id matched): true");
    let mut times = Vec::new();
    for _ in 0..5 {
        let (time, reply) = bridge_hello_dial(private_ssh_command(&remote), &hello_frame("k3b")).expect("k3b needs passwordless local ssh to its private daemon");
        assert_eq!(&reply.payload["session_id"], private_sid, "ssh bridge reached another daemon");
        times.push(time);
    }
    println!("k3b measured: ssh + bridge hello");
    percentiles("k3b dial", times);
    env.kill_daemon_bounded().await;
}

fn private_ssh_endpoint(remote: &str, dir: &Path) -> DaemonLaneEndpoint {
    let wrapper = dir.join("private-ssh");
    let script = format!("#!/usr/bin/env python3\nimport os, sys\nos.execvp('ssh', ['ssh'] + sys.argv[1:-1] + [{remote:?}])\n");
    sot_log::test_exec::write_executable(&wrapper, script);
    let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("localhost", None).unwrap();
    DaemonLaneEndpoint::new(LaneDial::Ssh(recipe, Default::default()), None).with_test_ssh_spawner(std::sync::Arc::new(move |recipe, gate| {
        let command = gate.command(recipe)?;
        Command::new(&wrapper).args(command.get_args())
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().map_err(sot_protocol::topology::ssh_bridge::SpawnError::Io)
    }))
}

/// Headless request-to-checkpoint interval; T3 owns the frontend request-to-pane interval.
fn timed_attach(endpoint: DaemonLaneEndpoint, target: &str) -> Duration {
    let t0 = Instant::now();
    let mut client = FeAttachClient::attach(endpoint, target.to_string(), 80, 24, "latency-fe".to_string(), "latency-fe".to_string(), None, Box::new(|| {})).expect("attach");
    let elapsed = wait_for(&mut client, "checkpoint", |client| client.is_checkpointed()) - t0;
    client.shutdown(Duration::from_secs(5));
    elapsed
}

fn p50(times: &[Duration]) -> f64 {
    let mut sorted = times.to_vec();
    sorted.sort();
    sorted[(sorted.len() - 1) / 2].as_secs_f64() * 1000.0
}

/// Five cold endpoints, interleaved local/ssh rounds; ratio uses p50, ceiling checks every total.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn k5_cold_switch_over_ssh() {
    let env = Env::new("k5");
    env.spawn_sotd();
    let (mut conn, mut next_id, first_hello) = first_hello_setup(&env).await;
    let private_sid = &first_hello.payload["session_id"];
    let target = create_row(&env, &mut conn, &mut next_id).await;
    let (_runtime, remote) = private_bridge_remote(&env);
    let wrapper_dir = tempfile::tempdir().unwrap();
    let mut local = Command::new("sh");
    local.arg("-c").arg(&remote);
    let (_, reply) = bridge_hello_dial(local, &hello_frame("k5")).expect("local private bridge hello");
    assert_eq!(&reply.payload["session_id"], private_sid, "local bridge reached another daemon");
    let (mut logins, mut locals, mut ssh_times, mut excess) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for round in 0..5 {
        let (login, reply) = bridge_hello_dial(private_ssh_command(&remote), &hello_frame("k5")).expect("k5 needs passwordless local ssh to its private daemon");
        assert_eq!(&reply.payload["session_id"], private_sid, "ssh bridge reached another daemon");
        let local = || timed_attach(DaemonLaneEndpoint::new(LaneDial::Local(env.socket_path.clone()), None), &target);
        let ssh = || timed_attach(private_ssh_endpoint(&remote, wrapper_dir.path()), &target);
        let (local, ssh) = if round % 2 == 0 { let local = local(); (local, ssh()) } else { let ssh = ssh(); (local(), ssh) };
        println!("k5 round {round} L={:.1}ms local total={:.1}ms ssh total={:.1}ms", login.as_secs_f64() * 1000.0, local.as_secs_f64() * 1000.0, ssh.as_secs_f64() * 1000.0);
        logins.push(login); locals.push(local); ssh_times.push(ssh);
        excess.push((ssh.as_secs_f64() - local.as_secs_f64()) * 1000.0);
    }
    excess.sort_by(f64::total_cmp);
    let excess_p50 = excess[(excess.len() - 1) / 2];
    let login_p50 = p50(&logins);
    println!("k5 L p50={login_p50:.1}ms");
    println!("k5 local p50={:.1}ms", p50(&locals));
    println!("k5 ssh p50={:.1}ms", p50(&ssh_times));
    println!("k5 excess p50={excess_p50:.1}ms ratio={:.2}", excess_p50 / login_p50);
    let pass = excess_p50 <= 1.5 * login_p50;
    println!("k5 {}", if pass { "PASS" } else { "FAIL" });
    env.kill_daemon_bounded().await;
    assert!(pass, "cold switch over ssh costs {excess_p50:.1}ms over a local dial, more than 1.5 x one login ({login_p50:.1}ms)");
    for total in locals.iter().chain(&ssh_times) {
        assert!(*total <= sot_log::lane::transport::CONNECT_BOUND, "headless request-to-checkpoint switch missed CONNECT_BOUND: {total:?}");
    }
}
