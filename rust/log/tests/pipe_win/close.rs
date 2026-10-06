//! Voyage-id validation, close/EOF, handle-leak churn, cancel and saturation tests.

use super::*;

/// Test 7: invalid voyage ids and out-of-range instance counts are
/// rejected loudly. Provably non-wedging (every case fails before any
/// Win32 I/O call), so this test is NOT process-isolated.
#[test]
fn invalid_voyage_ids_and_instance_counts_are_rejected_loudly() {
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
        let err = PipeServer::bind(bad, 1).unwrap_err();
        assert!(
            matches!(err, TransportError::InvalidVoyageId(_)),
            "id {bad:?}: got {err}"
        );
        let err2 = connect_voyage_pipe(bad).unwrap_err();
        assert!(
            matches!(err2, TransportError::InvalidVoyageId(_)),
            "id {bad:?}: got {err2}"
        );
    }

    try_create_first_instance("not-a-uuid", 1)
        .unwrap_or_else(|e| panic!("expected a rejected id to create no pipe at all: {e}"));

    let id = fresh_voyage_id();
    assert!(matches!(
        PipeServer::bind(&id, 0).unwrap_err(),
        TransportError::InvalidMaxConnections
    ));
    assert!(matches!(
        PipeServer::bind(&id, 256).unwrap_err(),
        TransportError::InvalidMaxConnections
    ));
}

/// Test 8: `close` on the server side gives the client an ordered EOF;
/// dropping a client gives the server a `Closed(Eof)`.
#[test]
fn server_close_yields_client_eof_and_client_drop_yields_server_closed() {
    if !run_isolated("close::server_close_yields_client_eof_and_client_drop_yields_server_closed") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();

    let client_a = connect_voyage_pipe(&id).unwrap();
    let conn_a = expect_accepted(&server, TIMEOUT);
    server.close(conn_a);
    let mut buf = [0u8; 16];
    let n = client_a.read(&mut buf).unwrap();
    assert_eq!(n, 0, "expected ordered EOF after a server-initiated close");
    assert_eq!(
        expect_closed(&server, conn_a, TIMEOUT),
        ClosedReason::Closed
    );

    let client_b = connect_voyage_pipe(&id).unwrap();
    let conn_b = expect_accepted(&server, TIMEOUT);
    drop(client_b);
    assert_eq!(expect_closed(&server, conn_b, TIMEOUT), ClosedReason::Eof);

    drop(server);
}

/// Test 9 (PRIMARY, deterministic — round-3 finding 9): the client
/// connects, the test waits for `Accepted` (proving registration
/// definitely happened) BEFORE closing, then asserts `Closed(Eof)`. The
/// instant-close race itself is a separate smoke test below.
#[test]
fn eof_before_registration_is_handled_cleanly() {
    if !run_isolated("close::eof_before_registration_is_handled_cleanly") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();

    let client = connect_voyage_pipe(&id).unwrap();
    let conn_id = expect_accepted(&server, TIMEOUT); // synchronize FIRST
    drop(client); // now close, after registration is proven

    assert_eq!(expect_closed(&server, conn_id, TIMEOUT), ClosedReason::Eof);

    let client2 = connect_voyage_pipe(&id).unwrap();
    let _ = expect_accepted(&server, TIMEOUT);
    drop(client2);

    drop(server);
}

/// Test 9b (smoke test, round-3 finding 9): a client that connects and
/// disconnects with NO synchronization at all is a genuine race with
/// `ConnectNamedPipe`'s own completion. Two outcomes are both honest: the
/// accept loop's own connect-error handling registers the connection
/// anyway (see `accept_loop`'s `match connect_result`), producing
/// `Accepted` then `Closed(Eof)`; OR Windows reports a "nobody ever
/// connected" condition (the `ERROR_NO_DATA` family, per Microsoft's
/// `ConnectNamedPipe` documentation) before the accept thread even
/// issues the call, producing NO event for this attempt at all. Neither
/// is a bug — this test only proves the race never wedges anything and
/// never poisons the pipe for the next client.
#[test]
fn eof_before_registration_smoke_test_accepts_either_honest_outcome() {
    if !run_isolated("close::eof_before_registration_smoke_test_accepts_either_honest_outcome") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();

    let client = connect_voyage_pipe(&id).unwrap();
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

    // Whichever happened, the pipe must still be healthy.
    let client2 = connect_voyage_pipe(&id).unwrap();
    let _ = expect_accepted(&server, TIMEOUT);
    drop(client2);

    drop(server);
}

/// Test 10 (round-3 finding 8): sequential connect/close churn must not
/// grow this process's OS handle count without bound. Isolated so
/// `GetProcessHandleCount` is not confounded by other tests running
/// concurrently; the slack is small precisely because isolation removes
/// that confound.
#[test]
fn sequential_connect_close_churn_does_not_leak_handles() {
    if !run_isolated("close::sequential_connect_close_churn_does_not_leak_handles") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 4).unwrap();

    for _ in 0..5 {
        churn_one(&server, &id);
    }

    let before = process_handle_count();
    for _ in 0..50 {
        churn_one(&server, &id);
    }
    let after = process_handle_count();

    assert!(
        after <= before + 6,
        "handle count grew from {before} to {after} across 50 connect/close cycles in isolation -- suspected leak"
    );

    drop(server);
}

/// New coverage (round-3 finding 9): a `PipeClient::read` blocked on one
/// thread is unblocked by `cancel()` called from another, returning
/// `TransportError::Cancelled`.
#[test]
fn client_read_cancel_unblocks_from_another_thread() {
    if !run_isolated("close::client_read_cancel_unblocks_from_another_thread") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();
    let client = Arc::new(connect_voyage_pipe(&id).unwrap());
    let _conn_id = expect_accepted(&server, TIMEOUT);

    let reader_client = Arc::clone(&client);
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        reader_client.read(&mut buf) // blocks -- the server never sends anything
    });

    std::thread::sleep(Duration::from_millis(300)); // let the read actually become Pending
    client.cancel();

    let result = reader.join().unwrap();
    assert!(
        matches!(result, Err(TransportError::Cancelled)),
        "expected Cancelled, got {result:?}"
    );

    drop(server);
}

/// New coverage (round-3 finding 9): a `PipeClient::write_all` blocked on
/// one thread (the pipe's kernel buffer saturated because nobody drains
/// it) is unblocked by `cancel()` called from another.
#[test]
fn client_write_cancel_unblocks_from_another_thread() {
    if !run_isolated("close::client_write_cancel_unblocks_from_another_thread") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();
    let client = Arc::new(connect_voyage_pipe(&id).unwrap());
    let _conn_id = expect_accepted(&server, TIMEOUT);
    // Deliberately never drain `server.events()` from here on -- that is
    // what eventually stalls the server's reader (see `deliver_bytes`)
    // and lets the raw pipe buffer fill up behind it, giving the
    // client's own `write_all` something real to block on.

    let writer_client = Arc::clone(&client);
    let writer = std::thread::spawn(move || {
        let payload = vec![0xCDu8; 65_536];
        loop {
            match writer_client.write_all(&payload) {
                Ok(()) => {}
                Err(e) => return e,
            }
        }
    });

    std::thread::sleep(Duration::from_secs(2)); // let the flood saturate the events channel + pipe buffer
    client.cancel();

    let result = writer.join().unwrap();
    assert!(
        matches!(result, TransportError::Cancelled),
        "expected Cancelled, got {result:?}"
    );

    drop(server);
}

/// New coverage (round-3 finding 1): once the events channel saturates
/// and stays that way past the `Bytes` abandon bound, the reader force-
/// closes the connection and a `Closed` is GUARANTEED to eventually
/// appear in the backlog once drained — never a silent stream gap.
#[test]
fn event_channel_saturation_abandons_bytes_and_guarantees_closed() {
    if !run_isolated("close::event_channel_saturation_abandons_bytes_and_guarantees_closed") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();
    let client = connect_voyage_pipe(&id).unwrap();
    let conn_id = expect_accepted(&server, TIMEOUT);

    let flooder = std::thread::spawn(move || {
        let payload = vec![0xEFu8; 65_536];
        loop {
            if client.write_all(&payload).is_err() {
                return; // expected once the connection is torn down under it
            }
        }
    });

    // Let the reader saturate the events channel and hit its abandon
    // bound WITHOUT this test draining anything -- that stall is exactly
    // what proves the guarantee (lane/pipe_win/'s own BYTES_ABANDON_AFTER is
    // 5s; wait well past it).
    std::thread::sleep(Duration::from_secs(8));

    let mut saw_closed = false;
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        match server.events().recv_timeout(Duration::from_secs(1)) {
            Ok(LaneEvent::Closed(cid, _)) if cid == conn_id => {
                saw_closed = true;
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert!(
        saw_closed,
        "expected a guaranteed Closed after Bytes abandonment"
    );

    let _ = flooder.join();
    drop(server);
}

/// New coverage (round-3 finding 2/9): a SECOND concurrent same-direction
/// `PipeClient::read` returns `TransportError::ConcurrentSubmit` rather than
/// racing the first caller's `OVERLAPPED`.
#[test]
fn concurrent_same_direction_client_read_returns_distinct_error() {
    if !run_isolated("close::concurrent_same_direction_client_read_returns_distinct_error") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();
    let client = Arc::new(connect_voyage_pipe(&id).unwrap());
    let _conn_id = expect_accepted(&server, TIMEOUT);

    let a = Arc::clone(&client);
    let reader_a = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        a.read(&mut buf) // blocks -- nobody ever sends
    });

    std::thread::sleep(Duration::from_millis(300)); // let A's read actually become Pending

    let mut buf_b = [0u8; 16];
    let result_b = client.read(&mut buf_b);
    assert!(
        matches!(result_b, Err(TransportError::ConcurrentSubmit)),
        "expected ConcurrentSubmit, got {result_b:?}"
    );

    client.cancel();
    let result_a = reader_a.join().unwrap();
    assert!(
        matches!(result_a, Err(TransportError::Cancelled)),
        "expected Cancelled, got {result_a:?}"
    );

    drop(server);
}

// Not exercised (round-3 finding 9's explicit "don't invent a seam"
// guidance): `thread::Builder::spawn` failure injection (no seam exists
// to force it deterministically) and `LaneEvent::AcceptError`
// (every path to it is a genuine OS resource exhaustion this test suite
// has no deterministic way to trigger).
