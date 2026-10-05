//! Tests of the connection task: backoff, the link gate, a closed local connection.

use super::*;

#[test]
fn a_down_ssh_host_costs_the_hub_at_most_two_logins_a_minute() {
    let count = |dial: &Dial| {
        let (mut t, mut b, mut n) = (0u64, 200u64, 0u32);
        while t < 3_600_000 {
            n += 1;
            t += b;
            b = next_backoff_ms(b, dial);
        }
        n
    };
    let ssh = Dial::Ssh(sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", Some("gamma")).unwrap());
    let n = count(&ssh);
    assert!(n <= 130, "{n} logins per hour on the hub for one down host");
    let n = count(&Dial::Pipe(std::path::PathBuf::from("/x")));
    assert!(n >= 700, "a local socket keeps its 5 s cap, got {n} probes per hour");
}

// --- ADR 0045 decision 4: the link gate. ---

struct NoWindow;
impl Redraw for NoWindow {
    fn request_redraw(&self) {}
}

/// A fake daemon on one end of an in-memory stream: answers the hello
/// with `hello_reply`, then (when `answer_preamble`) answers the
/// tree.root and preview.get preamble with an `{error}` payload, and
/// holds the stream open until `hold` is dropped.
async fn run_against_fake_daemon(
    host: &str,
    hello_reply: serde_json::Value,
    answer_preamble: bool,
    gate: sot_protocol::topology::ssh_bridge::LinkGate,
) -> (
    tokio::task::JoinHandle<Result<()>>,
    tokio::task::JoinHandle<()>,
    std::sync::mpsc::Receiver<(HostKey, IncomingEvt)>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (near, far) = tokio::io::duplex(1 << 16);
    let (hold_tx, hold_rx) = tokio::sync::oneshot::channel::<()>();
    let daemon = tokio::spawn(async move {
        let (rx, mut tx) = tokio::io::split(far);
        let mut rx = codec::buffered(rx);
        let (hello, _) = codec::read_frame(&mut rx).await.unwrap();
        codec::write_frame(&mut tx, &Frame::res(hello.id, op::HELLO, hello_reply).with_rev(0), None).await.unwrap();
        if answer_preamble {
            for _ in 0..2 {
                let (req, _) = codec::read_frame(&mut rx).await.unwrap();
                let err = serde_json::json!({ "error": "cannot read the default directory", "code": "io" });
                codec::write_frame(&mut tx, &Frame::res(req.id, &req.op, err).with_rev(0), None).await.unwrap();
            }
        }
        let _ = hold_rx.await;
    });
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let host = host.to_string();
    let session = tokio::spawn(async move {
        let (rx, tx) = tokio::io::split(near);
        let (_out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut backoff_ms = 200;
        let result = run_protocol(
            host.clone(),
            codec::buffered(rx),
            tx,
            None,
            &evt_tx,
            &mut out_rx,
            &NoWindow,
            &mut backoff_ms,
            ResolvedDial::Local,
            Some(&gate),
        )
        .await;
        result
    });
    (session, daemon, evt_rx, hold_tx)
}

fn hello_ok() -> serde_json::Value {
    serde_json::json!({ "session_id": "sess-1", "revision": 0, "snapshot_pending": false })
}

#[tokio::test]
async fn the_gate_is_up_after_the_hello_reply_and_down_when_the_session_ends() {
    let _env = crate::net::state::test_env::set_test_env();
    let gate = sot_protocol::topology::ssh_bridge::LinkGate::default();
    gate.set_up(false);
    let (session, daemon, evt_rx, hold) =
        run_against_fake_daemon("gate-test-up-down", hello_ok(), true, gate.clone()).await;
    let t0 = std::time::Instant::now();
    while evt_rx.try_recv().map_or(true, |(_, e)| !matches!(e, IncomingEvt::Connected { .. })) {
        assert!(t0.elapsed() < std::time::Duration::from_secs(5), "no Connected event");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(gate.is_up(), "a hello reply proves the link");
    drop(hold);
    daemon.await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), session).await.unwrap().unwrap();
    assert!(result.is_err(), "the daemon closing ends the session");
    assert!(!gate.is_up(), "the gate is down by the time run_protocol has returned");
}

#[tokio::test]
async fn a_protocol_mismatch_reply_leaves_the_gate_up() {
    let _env = crate::net::state::test_env::set_test_env();
    let gate = sot_protocol::topology::ssh_bridge::LinkGate::default();
    gate.set_up(false);
    let reply = serde_json::json!({ "error": "protocol skew", "code": "protocol_mismatch" });
    let (session, _daemon, _evt_rx, _hold) =
        run_against_fake_daemon("gate-test-mismatch", reply, false, gate.clone()).await;
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), session).await.unwrap().unwrap();
    assert!(result.unwrap_err().is::<HelloRefused>());
    assert!(gate.is_up(), "a refusal is a reply: the link is up");
}

#[tokio::test]
async fn a_tree_root_error_reply_does_not_end_the_session() {
    let _env = crate::net::state::test_env::set_test_env();
    let gate = sot_protocol::topology::ssh_bridge::LinkGate::default();
    let (session, _daemon, _evt_rx, _hold) =
        run_against_fake_daemon("gate-test-tree-root", hello_ok(), true, gate.clone()).await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert!(!session.is_finished(), "the session must still be connected after 1 s");
    assert!(gate.is_up());
    session.abort();
    let _ = session.await;
}





// --- Field incident 2026-09-08: a peer closing the LOCAL connection
// must surface as an error, not a silent hang. ---

/// End-to-end regression: when the PEER closes its end of the local
/// connection, the transport's read path must surface that as an `Err`
/// promptly — never hang — so
/// `run_protocol`'s `read?` (see the steady-state loop's read arm)
/// propagates it and `spawn`'s reconnect loop ("transport task ended;
/// reconnecting") takes over. Drives a REAL `interprocess` local-socket
/// listener/stream pair — the exact `connect_pipe`/`read_owned`
/// functions `run_protocol` itself calls (a Unix domain socket on this
/// platform, a Windows named pipe there, same code path) — not a mock.
#[tokio::test]
async fn a_closed_local_connection_surfaces_as_an_error_not_a_silent_hang() {
    use interprocess::local_socket::{tokio::prelude::*, GenericFilePath, ListenerOptions};

    let unique = format!(
        "sot-transport-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    // A named-pipe path on Windows, a socket file elsewhere -- both go
    // through `GenericFilePath`, exactly the route `connect_pipe` takes.
    #[cfg(windows)]
    let sock_path = std::path::PathBuf::from(format!(r"\\.\pipe\{unique}"));
    #[cfg(not(windows))]
    let sock_path = std::env::temp_dir().join(format!("{unique}.sock"));
    let _ = std::fs::remove_file(&sock_path);
    let name = sock_path
        .to_str()
        .unwrap()
        .to_fs_name::<GenericFilePath>()
        .unwrap();
    let listener = ListenerOptions::new()
        .name(name)
        .create_tokio()
        .expect("bind test socket");

    // Server: accept once, answer the hello handshake (so this exercises
    // a connection that was genuinely live, not merely refused), then
    // DROP the connection — standing in for the daemon closing its end
    // ("frame write exceeded 10s; dropping connection (peer not
    // draining)") in the field incident.
    let server = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        let (rx, mut tx) = conn.split();
        let mut rx = codec::buffered(rx);
        let (hello, _) = codec::read_frame(&mut rx).await.expect("read hello");
        let hello_res = serde_json::json!({
            "session_id": "sess-1",
            "revision": 0,
            "snapshot_pending": false,
        });
        codec::write_frame(
            &mut tx,
            &Frame::res(hello.id, op::HELLO, hello_res).with_rev(0),
            None,
        )
        .await
        .expect("write hello res");
        // Connection drops here (both halves go out of scope) — the
        // simulated server-side close.
    });

    let stream = connect_pipe(&sock_path).await.expect("client connect");
    let (client_rx, mut client_tx) = stream.split();
    let mut client_rx = codec::buffered(client_rx);
    codec::write_frame(
        &mut client_tx,
        &Frame::req(
            1,
            op::HELLO,
            serde_json::to_value(HelloReq {
                client_id: "test-client".into(),
                session_id: None,
                last_seen_revision: 0,
                token: None,
                protocol: sot_protocol::PROTOCOL_VERSION,
                app_version: sot_protocol::app_version(),
                host: None,
                role: String::new(),
                instance: None,
                name: None,
                os_user: None,
            })
            .unwrap(),
        ),
        None,
    )
    .await
    .expect("write hello req");
    let (hello_frame, _) = codec::read_frame(&mut client_rx).await.expect("read hello res");
    assert_eq!(hello_frame.id, 1);
    server.await.expect("server task must not panic");

    // The peer has now closed. `read_owned` is EXACTLY what the
    // steady-state select! loop polls (see `run_protocol`) — its next
    // completion must be an `Err` (EOF), bounded by a short timeout so
    // this test itself proves "promptly", not just "eventually".
    let (_rx_back, result) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        read_owned(client_rx),
    )
    .await
    .expect("the read must complete promptly once the peer has closed, not hang");

    assert!(
        result.is_err(),
        "a closed peer connection must surface as an Err, not hang forever"
    );

    let _ = std::fs::remove_file(&sock_path);
}

#[tokio::test]
async fn stderr_drain_keeps_the_last_non_empty_line() {
    async fn drained(input: &'static [u8]) -> Option<String> {
        let cell = spawn_stderr_drain(input);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while Arc::strong_count(&cell) != 1 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the drain task ends at end of input");
        let line = cell.lock().unwrap().clone();
        line
    }
    assert_eq!(drained(b"first\n\n  \nsecond\n \t \n").await.as_deref(), Some("second"));
    assert_eq!(drained(b"\n  \n").await, None);
}
