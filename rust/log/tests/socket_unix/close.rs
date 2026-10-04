//! Close tests: validation, close and EOF in both directions, EOF before registration, fd-leak churn, saturation abandoning bytes.

use super::*;

/// One connect -> accept -> server-close -> confirmed-closed -> client
/// drop cycle, used by the churn/leak test. `#[cfg(target_os = "linux")]`:
/// its one caller is the `/proc/self/fd`-based leak test, itself gated
/// the same way (the macOS CI leg still compiles this whole file, so an
/// ungated helper with no non-Linux caller would warn there).
#[cfg(target_os = "linux")]
fn churn_one(server: &SocketServer, path: &Path) {
    let client = UnixStream::connect(path).unwrap();
    let conn_id = expect_accepted(server, TIMEOUT);
    server.close(conn_id);
    expect_closed(server, conn_id, TIMEOUT);
    drop(client);
}
/// Invalid voyage ids and out-of-range connection ceilings are rejected
/// loudly. Provably non-wedging (every case fails before any socket
/// syscall is ever issued), so this test is NOT process-isolated.
#[test]
fn invalid_voyage_ids_and_max_connections_are_rejected_loudly() {
    let _rt = isolated_runtime_dir();
    let bad_ids = [
        "../../../etc/passwd",
        "not-a-uuid",
        "550E8400-E29B-41D4-A716-446655440000", // uppercase
        "550e8400e29b41d4a716446655440000",     // no hyphens ("simple" form)
        "550e8400-e29b-41d4-a716-44665544000",  // one hex digit short
        "{550e8400-e29b-41d4-a716-446655440000}", // braced GUID form
        "",
    ];
    for bad in bad_ids {
        let err = SocketServer::bind(bad, 1).unwrap_err();
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
        SocketServer::bind(&id, 0).unwrap_err(),
        TransportError::InvalidMaxConnections
    ));
    assert!(matches!(
        SocketServer::bind(&id, 256).unwrap_err(),
        TransportError::InvalidMaxConnections
    ));
}

/// `close` on the server side gives the client an ordered EOF; dropping a
/// client gives the server a `Closed(Eof)`.
#[test]
fn server_close_yields_client_eof_and_client_drop_yields_server_closed() {
    if !run_isolated("close::server_close_yields_client_eof_and_client_drop_yields_server_closed") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();

    let mut client_a = UnixStream::connect(&path).unwrap();
    let conn_a = expect_accepted(&server, TIMEOUT);
    server.close(conn_a);
    let mut buf = [0u8; 16];
    let n = client_a.read(&mut buf).unwrap();
    assert_eq!(n, 0, "expected ordered EOF after a server-initiated close");
    assert_eq!(
        expect_closed(&server, conn_a, TIMEOUT),
        ClosedReason::Closed
    );

    let client_b = UnixStream::connect(&path).unwrap();
    let conn_b = expect_accepted(&server, TIMEOUT);
    drop(client_b);
    assert_eq!(expect_closed(&server, conn_b, TIMEOUT), ClosedReason::Eof);

    drop(server);
}

/// PRIMARY, deterministic: the client connects, the test waits for
/// `Accepted` (proving registration definitely happened) BEFORE closing,
/// then asserts `Closed(Eof)`. The instant-close race itself is a
/// separate smoke test below.
#[test]
fn eof_before_registration_is_handled_cleanly() {
    if !run_isolated("close::eof_before_registration_is_handled_cleanly") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();

    let client = UnixStream::connect(&path).unwrap();
    let conn_id = expect_accepted(&server, TIMEOUT); // synchronize FIRST
    drop(client); // now close, after registration is proven

    assert_eq!(expect_closed(&server, conn_id, TIMEOUT), ClosedReason::Eof);

    let client2 = UnixStream::connect(&path).unwrap();
    let _ = expect_accepted(&server, TIMEOUT);
    drop(client2);

    drop(server);
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
    if !run_isolated("close::eof_before_registration_smoke_test_accepts_either_honest_outcome") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();

    let client = UnixStream::connect(&path).unwrap();
    drop(client); // no synchronization -- this IS the race under test

    match server.events().recv_timeout(Duration::from_secs(2)) {
        Ok(LaneEvent::Accepted(conn_id)) => {
            assert_eq!(expect_closed(&server, conn_id, TIMEOUT), ClosedReason::Eof);
        }
        Err(_timed_out) => {
            // The other honest outcome: no event at all for this attempt.
        }
        Ok(other) => panic!("unexpected event: {other:?}"),
    }

    // Whichever happened, the socket must still be healthy.
    let client2 = UnixStream::connect(&path).unwrap();
    let _ = expect_accepted(&server, TIMEOUT);
    drop(client2);

    drop(server);
}

/// Sequential connect/close churn must not grow this process's OS fd
/// count without bound. Isolated so `/proc/self/fd` is not confounded by
/// other tests running concurrently.
#[test]
#[cfg(target_os = "linux")]
fn sequential_connect_close_churn_does_not_leak_fds() {
    if !run_isolated("close::sequential_connect_close_churn_does_not_leak_fds") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 4).unwrap();

    for _ in 0..5 {
        churn_one(&server, &path);
    }

    let before = open_fd_count();
    for _ in 0..50 {
        churn_one(&server, &path);
    }
    let after = open_fd_count();

    assert!(
        after <= before + 6,
        "fd count grew from {before} to {after} across 50 connect/close cycles in isolation -- \
         suspected leak"
    );

    drop(server);
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
    if !run_isolated("close::event_channel_saturation_abandons_bytes_and_guarantees_closed") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = UnixStream::connect(&path).unwrap();
    let conn_id = expect_accepted(&server, TIMEOUT);

    // OBSERVE the stall (Codex review round 2), rather than assuming it
    // from a fixed sleep: flood until the reader's own delivery retry is
    // genuinely stuck against the never-drained events channel.
    saturate_via_stalled_writer(&client);
    wait_for_probe(
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
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        match server.events().recv_timeout(Duration::from_secs(1)) {
            Ok(evt) => last = Some(evt),
            Err(_) => break,
        }
    }
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

    drop(client);
    drop(server);
}
