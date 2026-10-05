//! ADR 0049, User isolation, end to end: a client run as another OS account gets no byte from a `serve_own` listener,
//! while the same client run as this account does (the control that shows the harness works). Needs `sudo -n -u
//! nobody` to work without a password, as it does on the hosted Linux and macOS runners; elsewhere the test says so and
//! skips.

#![cfg(unix)]

use std::process::Stdio;
use tokio::process::Command;

/// Run a client in bash that connects to `port`, waits up to 5 s for one byte and prints it; `as_nobody` runs it as the
/// account `nobody`. `None` when the command could not run at all.
async fn first_byte(port: u16, as_nobody: bool) -> Option<String> {
    let script = r#"exec 3<>/dev/tcp/127.0.0.1/$0 && read -r -t 5 -n 1 b <&3; printf %s "$b""#;
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
        eprintln!("skipped: `sudo -n -u nobody` does not work here");
        return;
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(sot_log::identity::peer_owner::serve_own(listener, "test", |mut s| async move {
        let _ = tokio::io::AsyncWriteExt::write_all(&mut s, b"served").await;
    }));
    let mine = first_byte(port, false).await;
    if mine.as_deref() != Some("s") {
        eprintln!("skipped: a client of this account got {mine:?}, so bash's /dev/tcp client does not work here");
        server.abort();
        return;
    }
    let theirs = first_byte(port, true).await;
    server.abort();
    assert_eq!(theirs.as_deref(), Some(""), "another account's client was served: {theirs:?}");
}
