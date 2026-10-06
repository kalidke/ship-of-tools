//! Close tests: validation, close and EOF in both directions, EOF before registration, fd-leak churn, saturation abandoning bytes.

use super::*;

/// One connect -> accept -> server-close -> confirmed-closed -> client
/// drop cycle, used by the churn/leak test. `#[cfg(target_os = "linux")]`:
/// its one caller is the `/proc/self/fd`-based leak test, itself gated
/// the same way (the macOS CI leg still compiles this whole file, so an
/// ungated helper with no non-Linux caller would warn there).
#[cfg(target_os = "linux")]
fn churn_one(server: &SocketServer, test: &str, path: &Path) {
    let client = io_named!(test, "connect", None, UnixStream::connect(path)).unwrap();
    let conn_id = expect_accepted(server, test, "accept", TIMEOUT);
    named!(test, "close", Some(conn_id), server.close(conn_id));
    expect_closed(server, test, "closed", conn_id, TIMEOUT);
    named!(test, "client.drop", None, drop(client));
}
/// Invalid voyage ids and out-of-range connection ceilings are rejected
/// loudly. Provably non-wedging (every case fails before any socket
/// syscall is ever issued), so this test is NOT process-isolated.
#[test]
fn invalid_voyage_ids_and_max_connections_are_rejected_loudly() {
    let test = "close::invalid_voyage_ids_and_max_connections_are_rejected_loudly";
    let _rt = isolated_runtime_dir();
    let bad_ids = [
        "../../../etc/passwd",
        "not-a-uuid",
        "550E8400-E29B-41D4-A716-446655440000",   // uppercase
        "550e8400e29b41d4a716446655440000",       // no hyphens ("simple" form)
        "550e8400-e29b-41d4-a716-44665544000",    // one hex digit short
        "{550e8400-e29b-41d4-a716-446655440000}", // braced GUID form
        "",
    ];
    for bad in bad_ids {
        let err = io_named!(test, "bind", None, SocketServer::bind(bad, 1)).unwrap_err();
        assert!(
            matches!(err, TransportError::InvalidVoyageId(_)),
            "id {bad:?}: got {err}"
        );
        let path_err = voyage_socket_path(bad).unwrap_err();
        assert!(
            matches!(path_err, TransportError::InvalidVoyageId(_)),
            "id {bad:?}: got {path_err}"
        );
    }

    let id = fresh_voyage_id();
    assert!(matches!(
        io_named!(test, "bind", None, SocketServer::bind(&id, 0)).unwrap_err(),
        TransportError::InvalidMaxConnections
    ));
    assert!(matches!(
        io_named!(test, "bind", None, SocketServer::bind(&id, 256)).unwrap_err(),
        TransportError::InvalidMaxConnections
    ));
}

/// `close` on the server side gives the client an ordered EOF; dropping a
/// client gives the server a `Closed(Eof)`.
#[test]
fn server_close_yields_client_eof_and_client_drop_yields_server_closed() {
    let test = "close::server_close_yields_client_eof_and_client_drop_yields_server_closed";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None,
        run_isolated(test)
    ) {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = io_named!(
        test,
        "server.bind",
        "server bound",
        None,
        SocketServer::bind(&id, 2)
    )
    .unwrap();

    let mut client_a = io_named!(
        test,
        "a.connect",
        "connected",
        None,
        UnixStream::connect(&path)
    )
    .unwrap();
    let conn_a = expect_accepted(&server, test, "a.accept", TIMEOUT);
    named!(
        test,
        "a.close",
        "close requested",
        Some(conn_a),
        server.close(conn_a)
    );
    let mut buf = [0u8; 16];
    let n = read_a_eof(test, Some(conn_a), &mut client_a, &mut buf).unwrap();
    assert_eq!(n, 0, "expected ordered EOF after a server-initiated close");
    let a_closed = WaitContext::new(test, "a.closed", "Closed(Closed)", Some(conn_a), TIMEOUT);
    match a_closed.event(&server) {
        LaneEvent::Closed(id, reason) => {
            assert_eq!(id, conn_a);
            assert_eq!(reason, ClosedReason::Closed);
        }
        other => panic!("expected Closed(Closed), got {other:?}"),
    }

    let client_b = io_named!(
        test,
        "b.connect",
        "connected",
        None,
        UnixStream::connect(&path)
    )
    .unwrap();
    let conn_b = expect_accepted(&server, test, "b.accept", TIMEOUT);
    named!(
        test,
        "b.drop",
        "client dropped",
        Some(conn_b),
        drop(client_b)
    );
    let b_closed = WaitContext::new(test, "b.closed", "Closed(Eof)", Some(conn_b), TIMEOUT);
    match b_closed.event(&server) {
        LaneEvent::Closed(id, reason) => {
            assert_eq!(id, conn_b);
            assert_eq!(reason, ClosedReason::Eof);
        }
        other => panic!("expected Closed(Eof), got {other:?}"),
    }
    named!(test, "server.drop", None, drop(server));
}

/// The original a.eof path, also driven against a peer kept open by diagnostics.
#[track_caller]
pub(super) fn read_a_eof(
    test: &str,
    conn: Option<ConnId>,
    client_a: &mut UnixStream,
    buf: &mut [u8],
) -> std::io::Result<usize> {
    io_named!(test, "a.eof", "zero bytes", conn, client_a.read(buf))
}

/// PRIMARY, deterministic: the client connects, the test waits for
/// `Accepted` (proving registration definitely happened) BEFORE closing,
/// then asserts `Closed(Eof)`. The instant-close race itself is a
/// separate smoke test below.
#[test]
fn eof_before_registration_is_handled_cleanly() {
    let test = "close::eof_before_registration_is_handled_cleanly";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None,
        run_isolated(test)
    ) {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = io_named!(test, "bind", None, SocketServer::bind(&id, 2)).unwrap();

    let client = io_named!(test, "connect", None, UnixStream::connect(&path)).unwrap();
    let conn_id = expect_accepted(&server, test, "accept", TIMEOUT); // synchronize FIRST
    named!(test, "client.drop", None, drop(client)); // now close, after registration is proven

    assert_eq!(
        expect_closed(&server, test, "closed", conn_id, TIMEOUT),
        ClosedReason::Eof
    );

    let client2 = io_named!(test, "connect", None, UnixStream::connect(&path)).unwrap();
    let _ = expect_accepted(&server, test, "accept", TIMEOUT);
    named!(test, "client2.drop", None, drop(client2));

    named!(test, "server.drop", None, drop(server));
}

/// Smoke test: a client that connects and disconnects with NO
/// synchronization at all. Ported defensively (accepting either honest
/// outcome, matching `tests/pipe_win/`'s own version) even though a
/// Unix listen backlog makes the accept side considerably more
/// deterministic than a named pipe's `ConnectNamedPipe` — this only
/// proves the race never wedges anything and never poisons the socket for
/// the next client.
#[test]
fn eof_before_registration_smoke_test_accepts_either_honest_outcome() {
    let test = "close::eof_before_registration_smoke_test_accepts_either_honest_outcome";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None,
        run_isolated(test)
    ) {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = io_named!(test, "bind", None, SocketServer::bind(&id, 2)).unwrap();

    let client = io_named!(test, "connect", None, UnixStream::connect(&path)).unwrap();
    named!(test, "client.drop", None, drop(client)); // no synchronization -- this IS the race under test

    match WaitContext::new(
        test,
        "smoke.accept",
        "Accepted or timeout",
        None,
        Duration::from_secs(2),
    )
    .receive(&server, Duration::from_secs(2))
    {
        Ok(LaneEvent::Accepted(conn_id)) => {
            assert_eq!(
                expect_closed(&server, test, "closed", conn_id, TIMEOUT),
                ClosedReason::Eof
            );
        }
        Err(_timed_out) => {
            // The other honest outcome: no event at all for this attempt.
        }
        Ok(other) => panic!("unexpected event: {other:?}"),
    }

    // Whichever happened, the socket must still be healthy.
    let client2 = io_named!(test, "connect", None, UnixStream::connect(&path)).unwrap();
    let _ = expect_accepted(&server, test, "accept", TIMEOUT);
    named!(test, "client2.drop", None, drop(client2));

    named!(test, "server.drop", None, drop(server));
}

/// Sequential connect/close churn must not grow this process's OS fd
/// count without bound. Isolated so `/proc/self/fd` is not confounded by
/// other tests running concurrently.
#[test]
#[cfg(target_os = "linux")]
fn sequential_connect_close_churn_does_not_leak_fds() {
    let test = "close::sequential_connect_close_churn_does_not_leak_fds";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None,
        run_isolated(test)
    ) {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = io_named!(test, "bind", None, SocketServer::bind(&id, 4)).unwrap();

    for _ in 0..5 {
        churn_one(&server, test, &path);
    }

    let before = open_fd_count();
    for _ in 0..50 {
        churn_one(&server, test, &path);
    }
    let after = open_fd_count();

    assert!(
        after <= before + 6,
        "fd count grew from {before} to {after} across 50 connect/close cycles in isolation -- \
         suspected leak"
    );

    named!(test, "server.drop", None, drop(server));
}

#[cfg(target_os = "linux")]
fn open_fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("read /proc/self/fd")
        .count()
}

/// New coverage (round-3 finding 1's Unix analogue): once the events
/// channel saturates and stays that way past the `Bytes` abandon bound,
/// the reader force-closes the connection and a `Closed` is GUARANTEED to
/// eventually appear in the backlog once drained — never a silent stream
/// gap.
#[test]
fn event_channel_saturation_abandons_bytes_and_guarantees_closed() {
    let test = "close::event_channel_saturation_abandons_bytes_and_guarantees_closed";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None,
        run_isolated(test)
    ) {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = io_named!(test, "bind", None, SocketServer::bind(&id, 2)).unwrap();
    let client = io_named!(test, "connect", None, UnixStream::connect(&path)).unwrap();
    let conn_id = expect_accepted(&server, test, "accept", TIMEOUT);

    // OBSERVE the stall (Codex review round 2), rather than assuming it
    // from a fixed sleep: flood until the reader's own delivery retry is
    // genuinely stuck against the never-drained events channel.
    saturate_via_stalled_writer(&server, test, &client);
    wait_for_probe(
        &server,
        test,
        "probe.1",
        || server.probe_events_full_bytes(),
        Duration::from_secs(10),
        "the events channel to genuinely report Full for a Bytes delivery",
    );

    // WAIT for the reader's own retry loop to have genuinely given up and
    // force-closed the connection -- `probe_bytes_abandoned` is the
    // production code's own count of that, never a fixed sleep guessing
    // at `BYTES_ABANDON_AFTER` (crate-private, 5s -- not importable from
    // this integration-test crate) plus a margin.
    wait_for_probe(
        &server,
        test,
        "probe.2",
        || server.probe_bytes_abandoned(),
        Duration::from_secs(10),
        "deliver_bytes to genuinely abandon this connection's Bytes delivery",
    );

    // Drain everything queued; the LAST event must be the guaranteed
    // Closed this abandonment produces -- every Bytes event this
    // connection will ever produce was necessarily enqueued BEFORE the
    // reader gave up (nothing more is ever sent for it afterward), so in
    // FIFO delivery order Closed is the true tail, not merely "observed
    // at some point".
    let mut last: Option<LaneEvent> = None;
    let drain_wait = WaitContext::new(
        test,
        "events.drain",
        "Closed(_,Error(abandoned)) tail",
        Some(conn_id),
        TIMEOUT,
    );
    let deadline = drain_wait.deadline;
    while Instant::now() < deadline {
        match drain_wait.receive(&server, Duration::from_secs(1)) {
            Ok(evt) => last = Some(evt),
            Err(_) => break,
        }
    }
    drain_wait.record("ok");
    match last {
        Some(LaneEvent::Closed(cid, ClosedReason::Error(msg))) => {
            assert_eq!(cid, conn_id, "Closed for the wrong connection");
            assert!(
                msg.contains("abandoned"),
                "expected an abandonment message, got {msg:?}"
            );
        }
        other => panic!(
            "expected the drained tail to be Closed(_, Error(msg containing \"abandoned\")), \
             got {other:?}"
        ),
    }

    named!(test, "client.drop", None, drop(client));
    named!(test, "server.drop", None, drop(server));
}
