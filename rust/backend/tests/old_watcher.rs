#![cfg(any(windows, target_os = "linux"))]
//! A pre-0.6.6 wake watcher's request, in its library's exact form, gets one thing from this daemon: a
//! `protocol_mismatch` refusal of its hello (literal `"protocol":2`), then the end of the connection with no other
//! byte, so its `pty.input` is never answered. The gate reads `protocol` before any field a later protocol adds or
//! requires. The test does not observe dispatch itself: a refused connection is read for one envelope only
//! (server/conn.rs `handle_connection`).

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

use sot_protocol::{op, Frame, Kind};
use support::{poll_until, try_connect, Env, BOUND};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::time::{timeout_at, Instant};

/// The old watcher's two lines, byte for byte as its library built them: a fresh connection, one `hello`,
/// one `pty.input`, written in one write.
const OLD_WATCHER_BYTES: &str = concat!(
    r#"{"v":1,"id":1,"kind":"req","op":"hello","payload":{"client_id":"sot-comm","last_seen_revision":0,"protocol":2,"app_version":"comm","token":"","host":"old-host","role":"agent","name":"old-watcher"}}"#,
    "\n",
    r#"{"v":1,"id":1,"kind":"req","op":"pty.input","payload":{"workspace_id":"ws-old","data_b64":"W3NvdC1jb21tXSB5b3UgaGF2ZSBtYWls","enter":true}}"#,
    "\n",
);

#[tokio::test]
async fn an_old_watcher_hello_is_refused() {
    let env = Env::new("oldwatch");
    env.spawn_sotd();
    let stream = poll_until(|| async { try_connect(&env.socket_path).await }, BOUND, "the socket").await;
    let mut old = tokio::io::BufReader::new(stream);

    // One deadline for the whole exchange, the fixture's BOUND: a slow runner fails only at the bound.
    let deadline = Instant::now() + BOUND;
    timeout_at(deadline, async {
        old.get_mut().write_all(OLD_WATCHER_BYTES.as_bytes()).await?;
        old.get_mut().flush().await
    })
    .await
    .expect("the watcher's bytes were not written within BOUND")
    .expect("write the watcher's bytes");

    // The first line back is the refusal, one envelope declaring no blob: no broadcast first, nothing else. Every byte
    // after its newline is left to the raw read below.
    let mut line = Vec::new();
    timeout_at(deadline, old.read_until(b'\n', &mut line))
        .await
        .expect("no reply to the old hello within BOUND")
        .expect("read the reply to the old hello");
    let hello: Frame = serde_json::from_slice(line.strip_suffix(b"\n").unwrap_or(&line)).unwrap_or_else(|e| {
        panic!("the reply to the old hello is no frame: {e}: {:?}", String::from_utf8_lossy(&line))
    });
    assert!(
        hello.kind == Kind::Res && hello.id == 1 && hello.op == op::HELLO && hello.payload["code"] == "protocol_mismatch",
        "the old watcher's hello was not refused for its protocol: {hello:?}"
    );
    assert!(hello.payload.get("blob").is_none(), "the refusal declares a blob: {hello:?}");

    // Then the end of the connection and not one byte more, already buffered or not: the transport's EOF, or the
    // platform's disconnect. A byte, or any other error, fails.
    let mut rest = [0u8; 256];
    match timeout_at(deadline, old.read(&mut rest)).await {
        Err(_) => panic!("the connection stayed open after a refused hello"),
        Ok(Ok(0)) => {}
        Ok(Ok(n)) => panic!("a byte after the refusal: {:?}", String::from_utf8_lossy(&rest[..n])),
        Ok(Err(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            ) => {}
        Ok(Err(e)) => panic!("the read after the refusal failed while the connection was open: {e}"),
    }
    env.kill_daemon_bounded().await;
}
