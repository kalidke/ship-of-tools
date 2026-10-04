//! Tests of the lane dial against a stub daemon: refusal mapping, bad-token and no-bridge replies, timeouts and cancel.

use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

#[test]
fn tcp_client_sets_nodelay() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let client = TcpClient::new(stream).unwrap();
    assert!(client.stream.nodelay().unwrap());
}

/// A `std::net::TcpListener` on loopback plays the daemon — portable,
/// no daemon process needed — for every refusal/uncertainty case
/// `dial` must classify (ADR 0045 decision 4).
fn dial_against<F>(respond: F) -> Result<(u32, u64), TransportError>
where
    F: FnOnce(std::net::TcpStream) + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (conn, _) = listener.accept().unwrap();
        respond(conn);
    });
    let endpoint = DaemonLaneEndpoint { dial: LaneDial::Tcp(addr), token: None };
    let result = endpoint.dial("row-1", "supervisor", None).map(|c| (c.peer.pid, c.peer.created));
    handle.join().unwrap();
    result
}

fn respond_with(mut conn: std::net::TcpStream, payload: serde_json::Value) {
    // Drain the request frame so the client's write doesn't block.
    let mut buf = [0u8; 4096];
    let _ = conn.read(&mut buf);
    let res = crate::Frame::res(1, op::LANE_CONNECT, payload);
    let mut line = serde_json::to_vec(&res).unwrap();
    line.push(b'\n');
    conn.write_all(&line).unwrap();
}

#[test]
fn refusal_codes_map_to_typed_errors() {
    for code in ["unknown_workspace", "not_capsule", "bad_lane", "foreign", "voyage_mismatch"] {
        let code = code.to_string();
        let result = dial_against(move |conn| {
            respond_with(conn, serde_json::json!({ "error": "refused", "code": code }));
        });
        match result {
            Err(TransportError::Refused { code: got, .. }) => assert_ne!(got, "no_bridge"),
            other => panic!("expected Refused, got {other:?}"),
        }
    }
}

/// The bridge's OWN token check (a daemon that DOES speak
/// `lane.connect` but rejected this dial's `token`) stays
/// `unauthenticated` — distinct from the old-daemon case below, which
/// shares the same wire code but a different message.
#[test]
fn a_bridge_daemons_own_bad_token_stays_unauthenticated() {
    let result = dial_against(|conn| {
        respond_with(conn, serde_json::json!({ "error": "bad or missing token", "code": "unauthenticated" }));
    });
    match result {
        Err(TransportError::Refused { code, .. }) => assert_eq!(code, "unauthenticated"),
        other => panic!("expected Refused{{code: unauthenticated}}, got {other:?}"),
    }
}

/// An OLD daemon's ordinary control-loop auth gate answers
/// `lane.connect` with the SAME `unauthenticated` code but its own
/// "send a token-valid hello first" text — this must be recognized
/// as `no_bridge`, not confused with a real bridge's bad-token
/// refusal (ADR 0045 lane B4a Codex review blocker).
#[test]
fn an_old_daemons_control_loop_unauthenticated_is_no_bridge() {
    let result = dial_against(|conn| {
        respond_with(
            conn,
            serde_json::json!({ "error": "authentication required: send a token-valid hello first", "code": "unauthenticated" }),
        );
    });
    match result {
        Err(TransportError::Refused { code, .. }) => assert_eq!(code, "no_bridge"),
        other => panic!("expected Refused{{code: no_bridge}}, got {other:?}"),
    }
}

/// ADR 0045 decision 4: a lane dial over ssh while the host's link is
/// down fails at once with `LinkDown` and starts no child.
#[test]
fn an_ssh_dial_with_a_down_gate_is_link_down_at_once() {
    let gate = crate::ssh_bridge::LinkGate::default();
    gate.set_up(false);
    let recipe = crate::ssh_bridge::SshRecipe::new("hub", None).unwrap();
    let endpoint = DaemonLaneEndpoint { dial: LaneDial::Ssh(recipe, gate), token: None };
    let t0 = Instant::now();
    let result = endpoint.dial("row", "supervisor", None);
    assert!(matches!(result, Err(TransportError::LinkDown)), "got {:?}", result.err());
    assert!(t0.elapsed() < Duration::from_millis(50));
}

/// `dial_failed` is the daemon's OWN dial/authenticate step failing
/// on the far side — uncertain transport, not a confirmed absence,
/// so it must classify as `Unreachable` (retried, clock cleared),
/// never a generic `Io` a caller's absence-window accounting could
/// charge (ADR 0045 lane B4a Codex review blocker).
#[test]
fn dial_failed_is_unreachable_not_generic_io() {
    let result = dial_against(|conn| {
        respond_with(conn, serde_json::json!({ "error": "connection refused dialing the voyage socket", "code": "dial_failed" }));
    });
    assert!(matches!(result, Err(TransportError::Unreachable(_))), "got {result:?}");
}

#[test]
fn lane_absent_decodes_to_endpoint_absent() {
    let result = dial_against(|conn| {
        respond_with(
            conn,
            serde_json::json!({ "error": "the row is terminal", "code": "lane_absent", "kind": "ConnectionRefused" }),
        );
    });
    match result {
        Err(e @ TransportError::Io { .. }) => assert!(e.is_endpoint_absent()),
        other => panic!("expected an absent Io error, got {other:?}"),
    }
}

#[test]
fn an_unknown_op_reply_is_no_bridge() {
    let result = dial_against(|conn| {
        respond_with(conn, serde_json::json!({ "error": "unknown op: lane.connect" }));
    });
    match result {
        Err(TransportError::Refused { code, .. }) => assert_eq!(code, "no_bridge"),
        other => panic!("expected Refused{{code: no_bridge}}, got {other:?}"),
    }
}

#[test]
fn an_undetermined_reply_is_undetermined() {
    let result = dial_against(|conn| {
        respond_with(conn, serde_json::json!({ "error": "could not authenticate", "code": "undetermined" }));
    });
    assert!(matches!(result, Err(TransportError::Undetermined { via: "bridge", .. })), "got {result:?}");
}

#[test]
fn a_silent_daemon_is_unreachable_within_two_seconds() {
    // Case 1: accepts the connect, then never speaks — the
    // handshake bound. NOT a dropped/unaccepted port: on Windows
    // loopback, a client's own ephemeral port can coincide with a
    // just-freed listener port and complete a TCP *self*-connect,
    // reading back its own request frame instead of seeing a
    // refused connect. A real listener that accepts and stays
    // silent (held open in a thread until this case is done)
    // exercises the same "no daemon answers" outcome without that
    // race.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let (conn, _) = listener.accept().unwrap();
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(10));
        drop(conn);
    });
    let endpoint = DaemonLaneEndpoint { dial: LaneDial::Tcp(addr), token: None };
    let started = Instant::now();
    // `.map(|_| ())`: `DaemonLaneClient` carries no `Debug` impl (a
    // live socket/pipe handle has no useful one), so the success
    // side is dropped before the failure assertion formats `result`.
    let result = endpoint.dial("row-1", "supervisor", None).map(|_| ());
    assert!(matches!(result, Err(TransportError::Unreachable(_))), "got {result:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    let _ = release_tx.send(());
    handle.join().unwrap();

    // Case 2: accepts, then never answers — the handshake bound.
    let started = Instant::now();
    let result = dial_against(|conn| {
        // Hold the connection open, answering nothing, until the
        // client's own read timeout gives up.
        std::thread::sleep(std::time::Duration::from_millis(2500));
        drop(conn);
    });
    assert!(matches!(result, Err(TransportError::Unreachable(_))), "got {result:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
}

/// A cancel mid-read must be what unblocks it, not an eventual peer
/// close racing ahead of the cancel — so the peer stays open for up
/// to 10 s (far past any real cancel latency) while the test asserts
/// the read actually completed in well under 2 s (ADR 0045 lane B4a
/// Codex review SHOULD-FIX: the previous version's peer closed after
/// 500 ms, so the test could pass even if `cancel()` did nothing).
#[test]
fn cancel_unblocks_a_pending_read() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = conn.read(&mut buf);
        let res = crate::Frame::res(1, op::LANE_CONNECT, serde_json::json!({ "ok": true, "pid": 4242u32, "created": 99u64 }));
        let mut line = serde_json::to_vec(&res).unwrap();
        line.push(b'\n');
        conn.write_all(&line).unwrap();
        // Held open until the test says the cancelled read already
        // completed -- see this test's own doc.
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(10));
    });
    let endpoint = DaemonLaneEndpoint { dial: LaneDial::Tcp(addr), token: None };
    let client = endpoint.dial("row-1", "supervisor", None).expect("handshake succeeds");

    let client = std::sync::Arc::new(client);
    let reader = std::sync::Arc::clone(&client);
    let read_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        let started = Instant::now();
        let result = reader.read(&mut buf);
        (result, started.elapsed())
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    client.cancel();
    let (result, elapsed) = read_thread.join().unwrap();
    let _ = release_tx.send(()); // only now may the peer close
    handle.join().unwrap();

    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "cancel() itself must unblock the read (peer stayed open 10s) -- took {elapsed:?}"
    );
    // `shutdown(Both)` on a Tcp/Unix stream unblocks a pending LOCAL
    // read as ordered EOF (`Ok(0)`) — it marks this end fully closed,
    // it does not raise an error the way `PipeClient::cancel`'s own
    // OVERLAPPED cancel does. Either outcome proves cancel() (not
    // the still-open peer) produced the completion.
    match result {
        Ok(0) | Err(TransportError::Cancelled) | Err(TransportError::Io { .. }) => {}
        other => panic!("expected cancel to unblock the read as EOF or an error, got {other:?}"),
    }
}

#[test]
fn a_wire_identity_that_differs_from_the_reported_peer_is_foreign() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = conn.read(&mut buf);
        let res = crate::Frame::res(1, op::LANE_CONNECT, serde_json::json!({ "ok": true, "pid": 111u32, "created": 1u64 }));
        let mut line = serde_json::to_vec(&res).unwrap();
        line.push(b'\n');
        conn.write_all(&line).unwrap();
        // The wire hello: read whatever `exchange_identity` sends,
        // answer with anything — `FixedExchange::feed` below ignores
        // the bytes and always decodes pid=222, deliberately NOT the
        // 111 just reported, which is the mismatch this test proves.
        let mut hello_buf = [0u8; 256];
        let _ = conn.read(&mut hello_buf);
        let _ = conn.write_all(b"irrelevant");
    });

    let endpoint = DaemonLaneEndpoint { dial: LaneDial::Tcp(addr), token: None };
    let client = endpoint.dial("row-1", "supervisor", None).expect("handshake succeeds");

    struct FixedExchange;
    impl IdentityExchange for FixedExchange {
        fn encode_request(&self) -> Vec<u8> {
            b"status".to_vec()
        }
        fn feed(&mut self, _bytes: &[u8]) -> sot_log::exchange::ExchangeDecode {
            sot_log::exchange::ExchangeDecode::Identity { pid: 222, created: 1 }
        }
    }
    let mut exchange = FixedExchange;
    let outcome = endpoint.challenge(&client, &mut exchange, Instant::now() + std::time::Duration::from_secs(1));
    handle.join().unwrap();
    assert!(matches!(outcome, ChallengeOutcome::Foreign), "a mismatched pid must never be Proven");
}

/// A portable stand-in for the real `ssh` child: `cat` echoes stdin
/// back on stdout and outlives its parent until killed, exactly the
/// two properties `BridgedClient::cancel()`'s kill→EOF mechanism
/// needs (C3 as amended §2; §7's test table, "Linux + Windows").
#[cfg(unix)]
fn spawn_stub_child() -> std::process::Child {
    std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("`cat` must be on PATH for this test")
}

#[cfg(windows)]
fn spawn_stub_child() -> std::process::Child {
    // `more` with no filename argument reads stdin and copies it to
    // stdout, the same echo shape `cat` gives on Unix — no unix-only
    // tool required. It is `more.com`, and Command looks up only `.exe`
    // without an extension.
    std::process::Command::new("more.com")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("`more.com` must be on PATH for this test")
}

/// `BridgedClient::cancel()`'s own contract: the kill it issues must
/// be what unblocks a read already parked on the child's stdout, not
/// an eventual natural exit racing ahead of it — same property
/// `cancel_unblocks_a_pending_read` proves for `TcpClient` above, one
/// mechanism for both.
#[test]
fn bridged_cancel_unblocks_a_parked_read_via_kill() {
    let client = std::sync::Arc::new(BridgedClient::wrap(spawn_stub_child()).expect("wrap"));
    let reader = std::sync::Arc::clone(&client);
    let read_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        let started = Instant::now();
        let result = reader.read(&mut buf);
        (result, started.elapsed())
    });
    // Give the read a moment to actually park before cancelling —
    // `cat`/`more` never write anything unprompted, so the read has
    // nothing to return until either bytes arrive or the child dies.
    std::thread::sleep(std::time::Duration::from_millis(100));
    client.cancel();
    let (result, elapsed) = read_thread.join().unwrap();
    assert!(elapsed < std::time::Duration::from_secs(2), "cancel() must unblock the read promptly, took {elapsed:?}");
    match result {
        Ok(0) | Err(TransportError::Cancelled) | Err(TransportError::Io { .. }) => {}
        other => panic!("expected cancel to unblock the read as EOF or an error, got {other:?}"),
    }
}
