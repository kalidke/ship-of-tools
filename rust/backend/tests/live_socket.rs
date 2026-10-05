#![cfg(target_os = "linux")]
//! A second daemon pointed at a live daemon's socket refuses, naming the
//! path, and the first daemon keeps answering. Before this, the second
//! daemon's bind unlinked the live socket, and the first daemon ran on
//! a deleted file that no client could reach until it restarted.

mod support;

use std::process::Stdio;
use std::time::Duration;

use support::{poll_until, try_connect, Env, TEST_STATE_HOST};

const BOUND: Duration = Duration::from_secs(20);

#[tokio::test]
async fn a_second_daemon_on_a_live_socket_refuses_and_the_first_still_answers() {
    let first = Env::new("live1");
    first.spawn_sotd();
    poll_until(|| async { try_connect(&first.socket_path).await }, BOUND, "the first daemon to accept").await;

    // The second daemon has its own state, config and comm home, as the
    // stray test daemon did: only the socket path is shared.
    let second = Env::new("live2");
    let out = tokio::time::timeout(
        BOUND,
        tokio::process::Command::from(support::sotd_command())
            .arg("--socket")
            .arg(&first.socket_path)
            .arg("--project-root")
            .arg(&second.daemon_project_root)
            .env("XDG_STATE_HOME", &second.state_root)
            .env("XDG_CONFIG_HOME", &second.config_root)
            .env("SOT_SELF_HOST", TEST_STATE_HOST)
            .env("SOT_RUNTIME_DIR", second._runtime_tmp.path())
            .env("HOME", &second.home_root)
            .env("SOT_COMM_HOME", &second.comm_root)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("the second daemon did not exit within BOUND")
    .expect("spawn the second sotd");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the second daemon must refuse; stderr: {stderr}");
    let want = format!("another daemon is already listening on {}", first.socket_path.display());
    assert!(stderr.contains(&want), "stderr must name the live socket: {stderr}");
    assert!(first.socket_path.exists(), "the live socket file must survive");
    assert!(try_connect(&first.socket_path).await.is_some(), "the first daemon must still answer");
    first.kill_daemon_bounded().await;
}
