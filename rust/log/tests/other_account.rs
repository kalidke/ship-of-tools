//! ADR 0049, User isolation, end to end: a client run as another OS account gets no byte from a `serve_own` listener,
//! while the same client run as this account does (the control that shows the harness works). Needs `sudo -n -u
//! nobody` to work without a password, as it does on the hosted Linux and macOS runners; elsewhere the test says so and
//! skips, but on CI (GITHUB_ACTIONS set) a skip is a failure.

#![cfg(unix)]

use std::process::Stdio;
use tokio::process::Command;

/// Run a client in bash that connects to `port`, prints `M` once it is connected, then waits up to 5 s for one byte and
/// prints it; `as_nobody` runs it as the account `nobody`. `None` when the command could not run at all. So the output
/// is `M` for a client that connected and got nothing, `Ms` for one that was served, and empty for one that never
/// connected.
async fn first_byte(port: u16, as_nobody: bool) -> Option<String> {
    let script = r#"exec 3<>/dev/tcp/127.0.0.1/$0 && { printf M; read -r -t 5 -n 1 b <&3; printf %s "$b"; }"#;
    let mut cmd = if as_nobody {
        let mut c = Command::new("sudo");
        c.args(["-n", "-u", "nobody", "bash", "-c", script]);
        c
    } else {
        let mut c = Command::new("bash");
        c.args(["-c", script]);
        c
    };
    let out = cmd.arg(port.to_string()).stdin(Stdio::null()).stderr(Stdio::null()).output().await.ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A test that cannot run here says so and passes, except on CI, where a silent skip is a failure to be seen.
fn skip(reason: &str) {
    eprintln!("skipped: {reason}");
    assert!(std::env::var_os("GITHUB_ACTIONS").is_none(), "a test skipped on CI: {reason}");
}

#[tokio::test]
async fn a_client_of_another_account_gets_no_byte() {
    let nobody_runs = Command::new("sudo")
        .args(["-n", "-u", "nobody", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success());
    if !nobody_runs {
        return skip("`sudo -n -u nobody` does not work here");
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(sot_log::identity::peer_owner::serve_own(listener, "test", |mut s| async move {
        let _ = tokio::io::AsyncWriteExt::write_all(&mut s, b"served").await;
    }));
    let mine = first_byte(port, false).await;
    if mine.as_deref() != Some("Ms") {
        server.abort();
        return skip(&format!("a client of this account got {mine:?}, so bash's /dev/tcp client does not work here"));
    }
    let theirs = first_byte(port, true).await;
    server.abort();
    // `M` and nothing after it: the client connected (so the test reached the owner check) and no byte came back.
    assert_eq!(theirs.as_deref(), Some("M"), "another account's client was served, or never connected: {theirs:?}");
}
