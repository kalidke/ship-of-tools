#![cfg(any(windows, target_os = "linux"))]
//! A daemon whose stdout is a regular file (a launcher's per-start log) writes its log once: into `sotd.log`, which
//! rolls, and not also into that file, which would grow without a bound. Every folder is the harness's own.

mod support;

use std::process::Stdio;
use support::{try_connect, Env, BOUND};

#[tokio::test]
async fn a_daemon_whose_stdout_is_a_file_writes_its_log_once() {
    let env = Env::new("stdout-file");
    let out_path = env._tmp.path().join("daemon-stdout.log");
    let out = std::fs::File::create(&out_path).expect("create the stdout file");
    env.spawn_sotd_to(Stdio::from(out));
    support::poll_until(|| async { try_connect(&env.socket_path).await }, BOUND, "the daemon's socket").await;
    env.kill_daemon_bounded().await;

    let stdout = std::fs::read_to_string(&out_path).unwrap_or_default();
    assert!(stdout.is_empty(), "the daemon wrote its log to a stdout that is a file: {stdout}");
    let log = std::fs::read_to_string(env.state_root.join("sot").join("sotd.log")).expect("sotd.log");
    assert!(log.contains("INFO"), "sotd.log holds no start line: {log}");
}
