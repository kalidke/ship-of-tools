#![cfg(target_os = "linux")]
//! A second daemon pointed at a live daemon's socket refuses, naming the
//! path, and the first daemon keeps answering. Before this, the second
//! daemon's bind unlinked the live socket, and the first daemon ran on
//! a deleted file that no client could reach until it restarted.

mod support;

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
    let ran = sot_platform::test_wait::run_within(
        support::sotd_command()
            .arg("--socket")
            .arg(&first.socket_path)
            .arg("--project-root")
            .arg(&second.daemon_project_root)
            .env("XDG_STATE_HOME", &second.state_root)
            .env("XDG_CONFIG_HOME", &second.config_root)
            .env("SOT_SELF_HOST", TEST_STATE_HOST)
            .env("SOT_RUNTIME_DIR", second._runtime_tmp.path())
            .env("HOME", &second.home_root)
            .env("SOT_COMM_HOME", &second.comm_root),
        BOUND,
    );
    assert!(!ran.status.success(), "the second daemon must refuse: {ran}");
    let want = format!("another daemon is already listening on {}", first.socket_path.display());
    assert!(ran.stderr.contains(&want), "stderr must name the live socket: {ran}");
    assert!(first.socket_path.exists(), "the live socket file must survive");
    assert!(try_connect(&first.socket_path).await.is_some(), "the first daemon must still answer");
    first.kill_daemon_bounded().await;
}

/// Queue item 21 on the real start path: a successor meets a socket whose listener only completes the connect and
/// never serves it (a copy of a dead predecessor's listener held by a child between its fork and its exec) while no
/// daemon holds the state lock. It is not a live daemon: the successor starts, replaces the path and answers a hello.
#[tokio::test]
async fn a_start_against_a_listener_that_never_replies_binds_the_socket_and_answers() {
    use tokio::io::AsyncWriteExt as _;
    let env = Env::new("succ");
    let stale = std::os::unix::net::UnixListener::bind(&env.socket_path).expect("bind the never-serving listener");
    env.spawn_sotd();
    let hello = sot_protocol::HelloReq::this_process("live-socket-test", "", Some("host-a".to_string())).expect("this process's account");
    let answered = poll_until(
        || async {
            let stream = try_connect(&env.socket_path).await?;
            let mut conn = tokio::io::BufReader::new(stream);
            let frame = sot_protocol::Frame::req(1, sot_protocol::op::HELLO, serde_json::to_value(&hello).unwrap());
            sot_protocol::codec::write_frame(&mut conn, &frame, None).await.ok()?;
            conn.flush().await.ok()?;
            let read = tokio::time::timeout(Duration::from_secs(2), sot_protocol::codec::read_frame(&mut conn)).await;
            read.ok()?.ok().map(|(reply, _)| reply)
        },
        BOUND,
        "the successor to answer a hello on the path a never-serving listener held",
    )
    .await;
    assert!(answered.payload.get("error").is_none(), "hello refused: {:?}", answered.payload);
    drop(stale);
    env.kill_daemon_bounded().await;
}
