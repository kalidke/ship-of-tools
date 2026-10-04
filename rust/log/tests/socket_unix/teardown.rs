//! Teardown tests: worker fan-out within the budget, stalled and flooded peers, the join deadline, drops with pending or saturated state.

use super::*;

/// ADR 0043, acceptance matrix "teardown composes": real worker fan-out
/// (several live connections torn down at once, none of them having
/// disconnected on their own) completes well inside the pinned aggregate
/// deadline.
#[test]
fn worst_case_worker_fan_out_completes_well_inside_the_aggregate_budget() {
    if !run_isolated("teardown::worst_case_worker_fan_out_completes_well_inside_the_aggregate_budget") {
        return;
    }
    let _rt = isolated_runtime_dir();
    assert!(Duration::from_secs(5) < TEARDOWN_AGGREGATE_DEADLINE);
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let max_connections = 8;
    let mut server = SocketServer::bind(&id, max_connections).unwrap();

    let mut clients = Vec::new();
    for _ in 0..max_connections {
        let client = UnixStream::connect(&path).unwrap();
        expect_accepted(&server, TIMEOUT);
        clients.push(client); // every connection stays LIVE -- worst case
    }

    server.disconnect_listener();
    let started = Instant::now();
    let ok = server.join_workers(started + Duration::from_secs(5));
    assert!(
        ok,
        "real teardown of {max_connections} live connections did not finish within a 5s budget \
         (took at least {:?})",
        started.elapsed()
    );
    drop(clients);
}

/// ADR 0043, acceptance matrix "teardown composes": a GENUINELY STALLED
/// connection worker (one whose peer never drains, never closes) must not
/// prevent the OTHER connections from being cancelled and torn down, and
/// the AGGREGATE join must still resolve within a small bound.
#[test]
fn stalled_worker_does_not_block_teardown_of_healthy_connections() {
    if !run_isolated("teardown::stalled_worker_does_not_block_teardown_of_healthy_connections") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let max_connections = 4;
    let mut server = SocketServer::bind(&id, max_connections).unwrap();

    // One connection whose client never reads and never writes again --
    // outbound bytes queued for it will sit until the server side
    // shuts down the fd out from under it. Flood until the outbound
    // budget genuinely reports full, proving the writer thread has real
    // in-flight/backed-up work when teardown begins.
    let stalled_client = UnixStream::connect(&path).unwrap();
    let stalled_conn = expect_accepted(&server, TIMEOUT);
    let payload = vec![0xABu8; 65_536];
    let mut saw_full = false;
    for _ in 0..128 {
        match server.send(stalled_conn, payload.clone(), None) {
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
        let c = UnixStream::connect(&path).unwrap();
        expect_accepted(&server, TIMEOUT);
        healthy_clients.push(c);
    }

    let started = Instant::now();
    // A budget an order of magnitude under the pinned 20s: `shutdown(2)`
    // unsticks the stalled writer promptly (ADR 0043 decision 5), so
    // healthy AND stalled connections alike tear down promptly -- no
    // Windows-style completion-proof scaffolding is needed to prove this.
    server.disconnect_listener();
    let ok = server.join_workers(started + Duration::from_secs(5));
    assert!(
        ok,
        "teardown with one stalled connection among several live ones did not finish within a \
         5s budget (took at least {:?})",
        started.elapsed()
    );
    drop(stalled_client);
    drop(healthy_clients);
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
    if !run_isolated(
        "teardown::join_workers_deadline_is_enforced_against_real_threads_not_merely_computed",
    ) {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let max_connections = 4;
    let mut server = SocketServer::bind(&id, max_connections).unwrap();

    let mut clients = Vec::new();
    for _ in 0..3 {
        let c = UnixStream::connect(&path).unwrap();
        expect_accepted(&server, TIMEOUT);
        clients.push(c);
    }

    server.disconnect_listener();
    let started = Instant::now();
    let _ok = server.join_workers(started + Duration::from_millis(1));
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "join_workers with an essentially-zero budget must return promptly, not silently wait \
         out the full aggregate regardless of outcome (took {elapsed:?})"
    );
    drop(clients);
}

/// Test: a client that connects and never reads while the server floods
/// it. The outbound BYTE budget eventually reports full once the
/// head-of-line `write` is stuck in the kernel; `close` is
/// fire-and-forget, so the bound under test is how promptly the `Closed`
/// event follows.
#[test]
fn flooded_never_reading_client_close_completes_within_bound() {
    if !run_isolated("teardown::flooded_never_reading_client_close_completes_within_bound") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = UnixStream::connect(&path).unwrap(); // deliberately never reads
    let conn_id = expect_accepted(&server, TIMEOUT);

    let payload = vec![0xABu8; 65_536];
    let mut saw_full = false;
    for _ in 0..128 {
        match server.send(conn_id, payload.clone(), None) {
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

    server.close(conn_id);
    assert_eq!(
        expect_closed(&server, conn_id, TIMEOUT),
        ClosedReason::Closed
    );

    drop(server);
    drop(client);
}

/// Test: a pending accept with no client ever connecting — server drop
/// must return promptly.
#[test]
fn pending_accept_with_no_client_drops_promptly() {
    if !run_isolated("teardown::pending_accept_with_no_client_drops_promptly") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let server = SocketServer::bind(&id, 1).unwrap();
    drop(server);
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
    if !run_isolated("teardown::drop_returns_even_with_a_saturated_events_channel") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = UnixStream::connect(&path).unwrap();
    let _conn_id = expect_accepted(&server, TIMEOUT);

    // Never drain events() -- flood until the events channel is OBSERVED
    // genuinely full (a real `TrySendError::Full` the production code
    // itself counted), not assumed from a client-side stall heuristic.
    saturate_via_stalled_writer(&client);
    wait_for_probe(
        || server.probe_events_full_bytes(),
        Duration::from_secs(10),
        "the events channel to genuinely report Full for a Bytes delivery",
    );

    // Connect a SECOND client so the acceptor's own `Accepted` for it
    // blocks in `send_lifecycle_event`'s retry loop against the SAME full
    // channel -- proving the actual regression under test, not merely
    // that the reader's own (separate) `deliver_bytes` retry stalled.
    let second_client = UnixStream::connect(&path).unwrap();
    wait_for_probe(
        || server.probe_events_full_lifecycle(),
        Duration::from_secs(10),
        "a lifecycle event (this second connection's own Accepted) to genuinely block \
         against the full channel",
    );

    // Must return even though nobody ever drained events(), and within
    // the SAME aggregate teardown bound every other test in this suite is
    // held to.
    let started = Instant::now();
    drop(server);
    assert!(
        started.elapsed() < TEARDOWN_AGGREGATE_DEADLINE,
        "Drop did not return within the teardown aggregate deadline (took {:?})",
        started.elapsed()
    );
    drop(client);
    drop(second_client);
}
