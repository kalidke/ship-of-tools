//! Tests of the connection task: backoff, the link gate, a closed local connection.

use super::steady::read_owned;
use super::*;

#[test]
fn a_down_ssh_host_costs_the_hub_at_most_two_logins_a_minute() {
    let count = |dial: &Dial| {
        let mut redial = redial_for(dial);
        let (mut t, mut n) = (std::time::Duration::ZERO, 0u32);
        while t < std::time::Duration::from_secs(3_600) {
            n += 1;
            t += redial.after(std::time::Duration::ZERO);
        }
        n
    };
    let ssh = Dial::Ssh(sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", Some("gamma")).unwrap());
    let n = count(&ssh);
    assert!(n <= 130, "{n} logins per hour on the hub for one down host");
    let n = count(&Dial::Relay(std::path::PathBuf::from(
        "/run/user/1000/sot-host-gamma.sock",
    )));
    assert!(
        n <= 130,
        "{n} relay connects per hour: each one makes the hub log in to the far host"
    );
    let n = count(&Dial::Pipe(std::path::PathBuf::from("/x")));
    assert!(n >= 700, "a local socket keeps its 5 s cap, got {n} probes per hour");
}

/// A daemon that answers every hello and then closes is redialed on the doubling wait, not every 200 ms: an answered
/// hello is not a working connection. The reconnect loop `spawn` runs dials a generated-relay endpoint this test serves
/// (an in-process Unix socket, or a named pipe on Windows) and names, in each `Disconnected` event, the wait it is about
/// to sleep. The first three are 200, 400 and 800 ms: three attempts happened and each wait doubled. No clock decides
/// the outcome; the deadline only fails a loop that stops redialing.
#[test]
fn a_daemon_that_answers_the_hello_and_drops_is_redialed_on_the_doubling_wait() {
    use interprocess::local_socket::{tokio::prelude::*, GenericFilePath, ListenerOptions};
    let _env = crate::net::state::test_env::set_test_env();
    #[cfg(windows)]
    let sock_path = std::path::PathBuf::from(format!(r"\\.\pipe\sot-redial-test-{}", std::process::id()));
    #[cfg(not(windows))]
    let sock_path = {
        use std::os::unix::fs::DirBuilderExt;
        let dir = std::path::PathBuf::from(format!("/tmp/sot-redial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::DirBuilder::new().mode(0o700).create(&dir).expect("private folder");
        dir.join("s.sock")
    };
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        let name = sock_path.to_str().unwrap().to_fs_name::<GenericFilePath>().unwrap();
        let listener = ListenerOptions::new().name(name).create_tokio().expect("bind test endpoint");
        tokio::spawn(async move {
            while let Ok(conn) = listener.accept().await {
                tokio::spawn(async move {
                    let (rx, mut tx) = conn.split();
                    let mut rx = codec::buffered(rx);
                    if let Ok((hello, _)) = codec::read_frame(&mut rx).await {
                        let reply = Frame::res(hello.id, op::HELLO, hello_ok()).with_rev(0);
                        let _ = codec::write_frame(&mut tx, &reply, None).await;
                    }
                    // Both halves drop here: the daemon closes right after its answer.
                });
            }
        });
    });
    let (evt_tx, evt_rx) = std::sync::mpsc::channel();
    let (_out_tx, out_rx) = outgoing_channel();
    let config = TransportConfig { dial: Dial::Relay(sock_path.clone()), token: None };
    spawn(
        &rt,
        "redial-test".to_string(),
        config,
        evt_tx,
        out_rx,
        NoWindow,
        Arc::new(tokio::sync::Notify::new()),
        sot_protocol::topology::ssh_bridge::LinkGate::default(),
        crate::lease::Leases::new(true, Vec::new()),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut waits: Vec<u64> = Vec::new();
    while waits.len() < 3 {
        match evt_rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok((_, IncomingEvt::Disconnected { reason })) => {
                let ms = reason.rsplit_once("retry in ").and_then(|(_, rest)| rest.split_once("ms")).and_then(|(n, _)| n.parse().ok());
                waits.push(ms.unwrap_or_else(|| panic!("no wait named in {reason:?}")));
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    rt.shutdown_timeout(std::time::Duration::from_secs(2));
    #[cfg(not(windows))]
    let _ = std::fs::remove_dir_all(sock_path.parent().unwrap());
    println!("redial: waits {waits:?} ms against a daemon that answers the hello and closes");
    assert_eq!(waits, [200, 400, 800], "the reconnect loop's first three waits: an answered hello restarted the doubling, or the loop stopped redialing");
}

// --- ADR 0045 decision 4: the link gate. ---

#[derive(Clone, Copy)]
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
        let result = run_protocol(
            host.clone(),
            codec::buffered(rx),
            tx,
            None,
            &evt_tx,
            &mut out_rx,
            &NoWindow,
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

/// ADR 0049 `## User isolation`: a daemon's refusal reaches the person whatever its code. The blocking screen shows
/// the daemon's own message, and a version skew its built "update needed" body.
#[tokio::test]
async fn a_refused_hello_is_shown_whatever_its_code() {
    let _env = crate::net::state::test_env::set_test_env();
    for (code, error) in [
        ("os_user_conflict", "host h has said hello to this daemon as more than one OS account"),
        ("identity_missing", "a hello must name its host and the OS account it runs as"),
        ("unauthenticated", "send a hello first"),
        ("protocol_mismatch", "protocol mismatch: update the older side"),
    ] {
        let gate = sot_protocol::topology::ssh_bridge::LinkGate::default();
        let reply = serde_json::json!({ "error": error, "code": code });
        let (session, _daemon, evt_rx, _hold) = run_against_fake_daemon("hello-refused", reply, false, gate).await;
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), session).await.unwrap().unwrap();
        assert!(result.unwrap_err().is::<HelloRefused>(), "{code}");
        let mut shown = None;
        while let Ok((_, evt)) = evt_rx.try_recv() {
            if let IncomingEvt::HelloRefused { message } = evt {
                shown = Some(message);
            }
        }
        let shown = shown.unwrap_or_else(|| panic!("{code}: the refusal never reached the chrome"));
        if code == "protocol_mismatch" {
            assert!(shown.contains("out of date"), "{shown}");
        } else {
            assert_eq!(shown, error, "{code}");
        }
    }
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

    // A named-pipe path on Windows, a socket file elsewhere, each bound by
    // its path (`GenericFilePath`).
    #[cfg(windows)]
    let unique = format!(
        "sot-transport-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    #[cfg(windows)]
    let sock_path = std::path::PathBuf::from(format!(r"\\.\pipe\{unique}"));
    // A private folder of the test's own, so no other account on a shared host reaches the socket, at a short path
    // (macOS's `sun_path` is 104 bytes).
    #[cfg(not(windows))]
    let sock_path = {
        use std::os::unix::fs::DirBuilderExt;
        let dir = std::path::PathBuf::from(format!("/tmp/sot-t-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::DirBuilder::new().mode(0o700).create(&dir).expect("private folder");
        dir.join("s.sock")
    };
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
    #[cfg(not(windows))]
    let _ = std::fs::remove_dir(sock_path.parent().unwrap());
}

/// ADR 0049, User isolation: `connect_pipe` returns within `CONNECT_BOUND` plus 2 s of slack against a socket whose
/// backlog another OS account has filled, and its error is not the account refusal, so the backlog was full.
#[cfg(unix)]
#[tokio::test]
async fn a_full_foreign_backlog_ends_connect_pipe_within_its_bound() {
    if !sot_log::test_isolated::run_isolated(
        "net::transport::tests::a_full_foreign_backlog_ends_connect_pipe_within_its_bound",
    ) {
        return;
    }
    let Some(foreign) = sot_log::test_foreign::ForeignListener::start(true) else {
        return;
    };
    let result = tokio::time::timeout(
        sot_log::lane::transport::CONNECT_BOUND + std::time::Duration::from_secs(2),
        connect_pipe(&foreign.path),
    )
    .await
    .expect("connect_pipe did not end within its bound");
    let err = result.err().expect("connected through a full backlog");
    let text = format!("{err:#}");
    assert!(
        !text.contains("not connecting"),
        "the connect went through, so the backlog was not full: {text}"
    );
    assert_eq!(
        foreign.finish(),
        0,
        "connect_pipe sent another account's listener bytes"
    );
}

/// ADR 0049, User isolation: `connect_pipe` refuses a socket another OS account listens on, and that listener gets no
/// byte.
#[cfg(unix)]
#[tokio::test]
async fn connect_pipe_refuses_a_socket_another_account_listens_on() {
    if !sot_log::test_isolated::run_isolated(
        "net::transport::tests::connect_pipe_refuses_a_socket_another_account_listens_on",
    ) {
        return;
    }
    let Some(foreign) = sot_log::test_foreign::ForeignListener::start(false) else {
        return;
    };
    let refused = match connect_pipe(&foreign.path).await {
        Ok(_) => String::from("connected"),
        Err(e) => format!("{e:#}"),
    };
    assert_eq!(
        foreign.finish(),
        0,
        "connect_pipe sent another account's listener bytes"
    );
    assert!(
        refused.contains("another OS account listens on this socket"),
        "{refused}"
    );
}

/// ADR 0049, User isolation: `connect_pipe` refuses a pipe another account serves (`epmapper`, SYSTEM's).
#[cfg(windows)]
#[tokio::test]
async fn connect_pipe_refuses_a_pipe_another_account_serves() {
    let refused = match connect_pipe(std::path::Path::new(r"\\.\pipe\epmapper")).await {
        Ok(_) => String::from("connected"),
        Err(e) => format!("{e:#}"),
    };
    assert!(refused.contains("not connecting"), "{refused}");
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
