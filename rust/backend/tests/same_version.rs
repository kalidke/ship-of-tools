#![cfg(any(windows, target_os = "linux"))]
//! `sotd same-version` against a real daemon of this build: exit 0 and nothing on stdout, so the launch scripts' ensure
//! keeps it, and with `--json` exit 0 and the one value `"same"` the installer reads. The daemon's own hello reply is what the command reads; a daemon of another version (exit 3 and its
//! version) is `topology::same_version`'s unit test, a listener answering as an older daemon does.

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

use support::{poll_until, sotd_command, try_connect, Env, BOUND, TEST_STATE_HOST};

#[tokio::test]
async fn a_daemon_of_this_build_is_the_same_version() {
    let env = Env::new("samever");
    env.spawn_sotd();
    let _ = poll_until(|| async { try_connect(&env.socket_path).await }, BOUND, "the socket").await;

    let scheme = if cfg!(windows) { "pipe" } else { "unix" };
    let ask = |json: bool| {
        let mut cmd = sotd_command();
        cmd.arg("same-version")
            .arg("--endpoint")
            .arg(format!("{scheme}:{}", env.socket_path.display()))
            .args(json.then_some("--json"))
            .env("SOT_SELF_HOST", TEST_STATE_HOST)
            .env("HOME", &env.home_root)
            .env("USERPROFILE", &env.home_root)
            .env("SOT_COMM_HOME", &env.comm_root);
        sot_platform::test_wait::run_within(&mut cmd, BOUND)
    };
    let (ran, json) = (ask(false), ask(true));
    env.kill_daemon_bounded().await;
    assert_eq!(ran.status.code(), Some(0), "{ran}");
    assert_eq!(ran.stdout, "", "{ran}");
    // The installer's `--json` answer: the same exit, and one JSON value naming the same version.
    assert_eq!(json.status.code(), Some(0), "{json}");
    assert_eq!(json.stdout, "\"same\"\n", "{json}");
}
