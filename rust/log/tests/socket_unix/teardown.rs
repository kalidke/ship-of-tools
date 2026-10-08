//! Teardown tests: worker fan-out within the budget, stalled and flooded peers, the join deadline, drops with pending or saturated state.

use super::*;

/// ADR 0043, acceptance matrix "teardown composes": real worker fan-out
/// (several live connections torn down at once, none of them having
/// disconnected on their own) completes well inside the pinned aggregate
/// deadline.
#[test]
fn worst_case_worker_fan_out_completes_well_inside_the_aggregate_budget() {
    let test = "teardown::worst_case_worker_fan_out_completes_well_inside_the_aggregate_budget";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None
    ) {
        return;
    }
    let _rt = isolated_runtime_dir();
    assert!(Duration::from_secs(5) < TEARDOWN_AGGREGATE_DEADLINE);
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let max_connections = 8;
    let mut server =
        io_named!(test, "bind", None, SocketServer::bind(&id, max_connections)).unwrap();

    let mut clients = Vec::new();
    for _ in 0..max_connections {
        let client = io_named!(test, "connect", None, UnixStream::connect(&path)).unwrap();
        expect_accepted(&server, test, "accept", TIMEOUT);
        clients.push(client); // every connection stays LIVE -- worst case
    }

    named!(
        test,
        "listener.disconnect",
        None,
        server.disconnect_listener()
    );
    let started = Instant::now();
    let ok = WaitContext::from_origin(
        test,
        "workers.join",
        "workers complete",
        None,
        started,
        started + Duration::from_secs(5),
    )
    .workers(&mut server);
    assert!(
        ok,
        "real teardown of {max_connections} live connections did not finish within a 5s budget \
         (took at least {:?})",
        started.elapsed()
    );
    named!(test, "clients.drop", None, drop(clients));
    named!(test, "server.drop", None, drop(server));
}

/// ADR 0043, acceptance matrix "teardown composes": a GENUINELY STALLED
/// connection worker (one whose peer never drains, never closes) must not
/// prevent the OTHER connections from being cancelled and torn down, and
/// the AGGREGATE join must still resolve within a small bound.
#[test]
fn stalled_worker_does_not_block_teardown_of_healthy_connections() {
    let test = "teardown::stalled_worker_does_not_block_teardown_of_healthy_connections";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None
    ) {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let max_connections = 4;
    let mut server =
        io_named!(test, "bind", None, SocketServer::bind(&id, max_connections)).unwrap();

    // One connection whose client never reads and never writes again --
    // outbound bytes queued for it will sit until the server side
    // shuts down the fd out from under it. Flood until the outbound
    // budget genuinely reports full, proving the writer thread has real
    // in-flight/backed-up work when teardown begins.
    let stalled_client = io_named!(test, "connect", None, UnixStream::connect(&path)).unwrap();
    let stalled_conn = expect_accepted(&server, test, "accept", TIMEOUT);
    let payload = vec![0xABu8; 65_536];
    let mut saw_full = false;
    let flood = WaitContext::new(test, "send.flood", "QueueFull", Some(stalled_conn), TIMEOUT);
    for _ in 0..128 {
        flood.check(Some(&server));
        match flood.attempt_io(|| server.send(stalled_conn, payload.clone(), None)) {
            Ok(()) => {}
            Err(TransportError::QueueFull(cid)) => {
                assert_eq!(cid, stalled_conn);
                saw_full = true;
                break;
            }
            Err(other) => panic!("unexpected send error: {other}"),
        }
    }
    assert!(
        saw_full,
        "expected the outbound budget to report full against a stalled peer"
    );

    let mut healthy_clients = Vec::new();
    for _ in 0..2 {
        let c = io_named!(test, "connect", None, UnixStream::connect(&path)).unwrap();
        expect_accepted(&server, test, "accept", TIMEOUT);
        healthy_clients.push(c);
    }

    let started = Instant::now();
    // A budget an order of magnitude under the pinned 20s: `shutdown(2)`
    // unsticks the stalled writer promptly (ADR 0043 decision 5), so
    // healthy AND stalled connections alike tear down promptly -- no
    // Windows-style completion-proof scaffolding is needed to prove this.
    named!(
        test,
        "listener.disconnect",
        None,
        server.disconnect_listener()
    );
    let ok = WaitContext::from_origin(
        test,
        "workers.join",
        "workers complete",
        None,
        started,
        started + Duration::from_secs(5),
    )
    .workers(&mut server);
    assert!(
        ok,
        "teardown with one stalled connection among several live ones did not finish within a \
         5s budget (took at least {:?})",
        started.elapsed()
    );
    named!(test, "stalled_client.drop", None, drop(stalled_client));
    named!(test, "healthy_clients.drop", None, drop(healthy_clients));
    named!(test, "server.drop", None, drop(server));
}

/// ADR 0043, acceptance matrix "teardown composes", "total-deadline
/// propagation": the shared deadline is REAL and honored against REAL OS
/// threads. An essentially-zero budget against otherwise-healthy, real
/// connections still returns promptly (never hangs out to the pinned
/// 20s) -- the natural race (some threads may already have finished
/// before the first `is_finished` poll) means this asserts BOUNDED total
/// time, not a specific `true`/`false` outcome.
#[test]
fn join_workers_deadline_is_enforced_against_real_threads_not_merely_computed() {
    let test =
        "teardown::join_workers_deadline_is_enforced_against_real_threads_not_merely_computed";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None
    ) {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let max_connections = 4;
    let mut server =
        io_named!(test, "bind", None, SocketServer::bind(&id, max_connections)).unwrap();

    let mut clients = Vec::new();
    for _ in 0..3 {
        let c = io_named!(test, "connect", None, UnixStream::connect(&path)).unwrap();
        expect_accepted(&server, test, "accept", TIMEOUT);
        clients.push(c);
    }

    named!(
        test,
        "listener.disconnect",
        None,
        server.disconnect_listener()
    );
    let started = Instant::now();
    let _ok = WaitContext::from_origin(
        test,
        "workers.join",
        "workers complete",
        None,
        started,
        started + Duration::from_millis(1),
    )
    .workers(&mut server);
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "join_workers with an essentially-zero budget must return promptly, not silently wait \
         out the full aggregate regardless of outcome (took {elapsed:?})"
    );
    named!(test, "clients.drop", None, drop(clients));
    named!(test, "server.drop", None, drop(server));
}

/// Test: a client that connects and never reads while the server floods
/// it. The outbound BYTE budget eventually reports full once the
/// head-of-line `write` is stuck in the kernel; `close` is
/// fire-and-forget, so the bound under test is how promptly the `Closed`
/// event follows.
#[test]
fn flooded_never_reading_client_close_completes_within_bound() {
    let test = "teardown::flooded_never_reading_client_close_completes_within_bound";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None
    ) {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = io_named!(test, "bind", None, SocketServer::bind(&id, 2)).unwrap();
    let client = io_named!(test, "connect", None, UnixStream::connect(&path)).unwrap(); // deliberately never reads
    let conn_id = expect_accepted(&server, test, "accept", TIMEOUT);

    let payload = vec![0xABu8; 65_536];
    let mut saw_full = false;
    let flood = WaitContext::new(test, "send.flood", "QueueFull", Some(conn_id), TIMEOUT);
    for _ in 0..128 {
        flood.check(Some(&server));
        match flood.attempt_io(|| server.send(conn_id, payload.clone(), None)) {
            Ok(()) => {}
            Err(TransportError::QueueFull(cid)) => {
                assert_eq!(cid, conn_id);
                saw_full = true;
                break;
            }
            Err(other) => panic!("unexpected send error: {other}"),
        }
    }
    assert!(
        saw_full,
        "expected the outbound budget to report full against a non-reading peer"
    );

    named!(test, "close", Some(conn_id), server.close(conn_id));
    assert_eq!(
        expect_closed(&server, test, "closed", conn_id, TIMEOUT),
        ClosedReason::Closed
    );

    named!(test, "server.drop", None, drop(server));
    named!(test, "client.drop", None, drop(client));
}

/// Test: a pending accept with no client ever connecting — server drop
/// must return promptly.
#[test]
fn pending_accept_with_no_client_drops_promptly() {
    let test = "teardown::pending_accept_with_no_client_drops_promptly";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None
    ) {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let server = io_named!(test, "bind", None, SocketServer::bind(&id, 1)).unwrap();
    named!(test, "server.drop", None, drop(server));
}

/// Drop-vs-lifecycle-delivery regression: saturate the events channel and
/// never drain it, then drop the server. `Drop` must still return -- it
/// must not deadlock behind its own `send_lifecycle_event` escape by
/// joining the accept thread before setting `dropping`. Codex review
/// round 2, finding 2: this must exercise a LIFECYCLE send blocked behind
/// the full channel -- `send_lifecycle_event`'s own `dropping` escape
/// hatch is the regression this test names, not merely `deliver_bytes`'s
/// separate (and separately tested) abandon path.
#[test]
fn drop_returns_even_with_a_saturated_events_channel() {
    let test = "teardown::drop_returns_even_with_a_saturated_events_channel";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None
    ) {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = io_named!(test, "bind", None, SocketServer::bind(&id, 2)).unwrap();
    let client = io_named!(test, "connect", None, UnixStream::connect(&path)).unwrap();
    let _conn_id = expect_accepted(&server, test, "accept", TIMEOUT);

    // Never drain events() -- flood until the events channel is OBSERVED
    // genuinely full (a real `TrySendError::Full` the production code
    // itself counted), not assumed from a client-side stall heuristic.
    saturate_via_stalled_writer(&server, test, &client);
    wait_for_probe(
        &server,
        test,
        "probe.1",
        || server.probe_events_full_bytes(),
        Duration::from_secs(10),
        "the events channel to genuinely report Full for a Bytes delivery",
    );

    // Connect a SECOND client so the acceptor's own `Accepted` for it
    // blocks in `send_lifecycle_event`'s retry loop against the SAME full
    // channel -- proving the actual regression under test, not merely
    // that the reader's own (separate) `deliver_bytes` retry stalled.
    let second_client = io_named!(test, "connect", None, UnixStream::connect(&path)).unwrap();
    wait_for_probe(
        &server,
        test,
        "probe.2",
        || server.probe_events_full_lifecycle(),
        Duration::from_secs(10),
        "a lifecycle event (this second connection's own Accepted) to genuinely block \
         against the full channel",
    );

    // Must return even though nobody ever drained events(), and within
    // the SAME aggregate teardown bound every other test in this suite is
    // held to.
    let started = Instant::now();
    named!(test, "server.drop", None, drop(server));
    assert!(
        started.elapsed() < TEARDOWN_AGGREGATE_DEADLINE,
        "Drop did not return within the teardown aggregate deadline (took {:?})",
        started.elapsed()
    );
    named!(test, "client.drop", None, drop(client));
    named!(test, "second_client.drop", None, drop(second_client));
}
