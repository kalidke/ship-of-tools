#![cfg(any(windows, target_os = "linux"))]
//! A pre-0.6.6 wake watcher that outlives the install types nothing into a row: its hello (literal
//! `"protocol":2`) is refused with `protocol_mismatch`, the connection closes, and its `pty.input` is never
//! dispatched. The gate reads `protocol` before any field a later protocol adds or requires.

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

use serde_json::json;
use sot_protocol::{codec, op, Frame, Kind};
use std::time::Duration;
use support::{call, connect_and_hello, poll_until, try_connect, Env};
use tokio::io::AsyncWriteExt;

/// The old watcher's two lines, byte for byte as its library built them: a fresh connection, one `hello`,
/// one `pty.input`, written in one write.
const OLD_WATCHER_BYTES: &str = concat!(
    r#"{"v":1,"id":1,"kind":"req","op":"hello","payload":{"client_id":"sot-comm","last_seen_revision":0,"protocol":2,"app_version":"comm","token":"","host":"old-host","role":"agent","name":"old-watcher"}}"#,
    "\n",
    r#"{"v":1,"id":1,"kind":"req","op":"pty.input","payload":{"workspace_id":"ws-old","data_b64":"W3NvdC1jb21tXSB5b3UgaGF2ZSBtYWls","enter":true}}"#,
    "\n",
);

/// One frame that is not a broadcast, or the reason there is none, within `bound`.
async fn next_frame(conn: &mut support::Conn, bound: Duration) -> Result<Frame, String> {
    let body = async {
        loop {
            match codec::read_frame(conn).await {
                Ok((f, _)) if f.kind == Kind::Evt => continue,
                Ok((f, _)) => return Ok(f),
                Err(e) => return Err(format!("closed: {e}")),
            }
        }
    };
    tokio::time::timeout(bound, body).await.unwrap_or_else(|_| Err("timeout".to_string()))
}

#[tokio::test]
async fn an_old_watcher_hello_is_refused() {
    let env = Env::new("oldwatch");
    env.spawn_sotd();

    // Control: an admitted connection gets its `pty.input` answered, so silence below means "not dispatched".
    let (mut witness, id) = connect_and_hello(&env.socket_path).await;
    let payload = json!({"workspace_id": "ws-old", "data_b64": "W3NvdC1jb21tXSB5b3UgaGF2ZSBtYWls", "enter": true});
    let answered = call(&mut witness, id, op::PTY_INPUT, payload).await;
    assert_eq!(answered.kind, Kind::Res, "control: an admitted pty.input is answered: {:?}", answered.payload);

    // The old watcher: a second connection, both lines in one write.
    let stream = poll_until(|| async { try_connect(&env.socket_path).await }, support::BOUND, "the socket").await;
    let mut old = tokio::io::BufReader::new(stream);
    old.get_mut().write_all(OLD_WATCHER_BYTES.as_bytes()).await.expect("write the watcher's bytes");
    old.get_mut().flush().await.expect("flush");

    let bound = Duration::from_secs(5);
    let hello = next_frame(&mut old, bound).await.unwrap_or_else(|e| panic!("no reply to the old hello: {e}"));
    assert!(
        hello.kind == Kind::Res && hello.id == 1 && hello.op == op::HELLO && hello.payload["code"] == "protocol_mismatch",
        "the old watcher's hello was accepted: {:?}",
        hello.payload
    );

    // Nothing more comes back: the connection closes and the `pty.input` is not answered.
    match next_frame(&mut old, bound).await {
        Ok(f) => panic!("answered after a refused hello: {f:?}"),
        Err(e) if e == "timeout" => panic!("the connection stayed open after a refused hello"),
        Err(_) => {}
    }
    env.kill_daemon_bounded().await;
}
