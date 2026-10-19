#![cfg(any(windows, target_os = "linux"))]
//! A daemon whose stdout is a regular file (a launcher's per-start log) writes its log once: into `sotd.log`, which
//! rolls, and not also into that file, which would grow without a bound. Every folder is the harness's own.

mod support;

use std::process::Stdio;
use support::{sotd_command, try_connect, Env, BOUND};

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
    let log = std::fs::read_to_string(env.daemon_log()).expect("sotd.log");
    assert!(log.contains("sotd starting"), "sotd.log holds no start line: {log}");
}

/// `sotd log-path` prints the path the daemon writes, per platform, under the daemon's own environment: after a start
/// that file holds the start line. The launchers name this path when a backend does not come up.
#[tokio::test]
async fn log_path_names_the_file_the_daemon_writes() {
    let env = Env::new("log-path");
    env.spawn_sotd();
    support::poll_until(|| async { try_connect(&env.socket_path).await }, BOUND, "the daemon's socket").await;
    let out = sot_log::test_wait::run_within(
        sotd_command()
            .arg("log-path")
            .env("LOCALAPPDATA", &env.state_root)
            .env("XDG_STATE_HOME", &env.state_root)
            .env("HOME", &env.home_root)
            .env("USERPROFILE", &env.home_root),
        BOUND,
    );
    env.kill_daemon_bounded().await;

    let out = out.ok();
    let printed = std::path::PathBuf::from(out.stdout.trim());
    assert_eq!(printed, env.daemon_log(), "log-path prints a path other than the one the daemon writes");
    let log = std::fs::read_to_string(&printed).expect("the printed path");
    assert!(log.contains("sotd starting"), "the printed path holds no start line: {log}");
}
