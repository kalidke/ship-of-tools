//! Worker fan-out, stalled worker, join deadline, flood and drop-bound teardown tests.

use super::*;

/// ADR 0041 step 6 U1b, acceptance matrix "teardown composes": real
/// worker fan-out (several live connections torn down at once, none of
/// them having disconnected on their own) completes well inside the
/// pinned aggregate deadline — proven against a budget a full order of
/// magnitude smaller than [`TEARDOWN_AGGREGATE_DEADLINE`], which is
/// exactly the margin that constant claims to have.
#[test]
fn worst_case_worker_fan_out_completes_well_inside_the_aggregate_budget() {
    if !run_isolated("worst_case_worker_fan_out_completes_well_inside_the_aggregate_budget") {
        return;
    }
    assert!(Duration::from_secs(5) < TEARDOWN_AGGREGATE_DEADLINE);
    let id = fresh_voyage_id();
    let max_instances = 8;
    let mut server = PipeServer::bind(&id, max_instances).unwrap();

    let mut clients = Vec::new();
    for _ in 0..max_instances {
        let client = connect_voyage_pipe(&id).unwrap();
        expect_accepted(&server, TIMEOUT);
        clients.push(client); // every connection stays LIVE -- worst case
    }

    server.disconnect_listener();
    let started = Instant::now();
    let ok = server.join_workers(started + Duration::from_secs(5));
    assert!(
        ok,
        "real teardown of {max_instances} live connections did not finish within a 5s budget \
         (took at least {:?})",
        started.elapsed()
    );
    drop(clients);
}

/// ADR 0041 step 6 U1b, acceptance matrix "teardown composes" — Codex
/// round-1 Blocker 3 discharge: a GENUINELY STALLED connection worker
/// (one whose peer never drains, never closes, and whose own I/O the
/// server side cannot otherwise unstick) must not prevent the OTHER
/// connections from being cancelled and torn down, and the AGGREGATE
/// join must still resolve (loud on expiry) within a small bound rather
/// than hanging on the one stuck worker. `flooded_never_reading_client_
/// close_completes_within_bound` (below) already proves a single
/// never-reading watcher's OWN close completes bounded; this test proves
/// the WHOLE-SERVER teardown composes the same way when one connection
/// is stalled and several healthy ones are live alongside it.
#[test]
fn stalled_worker_does_not_block_teardown_of_healthy_connections() {
    if !run_isolated("stalled_worker_does_not_block_teardown_of_healthy_connections") {
        return;
    }
    let id = fresh_voyage_id();
    let max_instances = 4;
    let mut server = PipeServer::bind(&id, max_instances).unwrap();

    // One connection whose CLIENT never reads and never writes again --
    // outbound bytes queued for it will sit until the server side closes
    // the handle out from under it. A few healthy connections alongside
    // it, established BEFORE the final pending-at-teardown proof below
    // (Codex round-5 fix 3), so the whole scenario is real by the time
    // that proof runs.
    let stalled_client = connect_voyage_pipe(&id).unwrap();
    let stalled_conn = expect_accepted(&server, TIMEOUT);
    // Codex round-3 test-premise-gap fix: a single 4 KiB send into a pipe
    // configured with 64 KiB buffers never actually stalls -- flood until
    // the outbound budget genuinely reports full (the SAME pattern
    // `flooded_never_reading_client_close_completes_within_bound` uses),
    // proving the writer thread has real in-flight/backed-up work when
    // teardown begins, not an idle connection.
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
        let c = connect_voyage_pipe(&id).unwrap();
        expect_accepted(&server, TIMEOUT);
        healthy_clients.push(c);
    }

    // Codex round-4 finding 3 / round-5 finding 2: `QueueFull` alone only
    // proves the outbound BYTE budget is reserved, and plain
    // `SlotState::Pending` is ALSO set for a synchronously-completed
    // write still awaiting result collection -- neither proves the
    // writer thread has reached a GENUINE `ERROR_IO_PENDING` `WriteFile`.
    // This poll is a best-effort PRE-check deciding WHEN it is worth
    // proceeding to teardown; it is NOT the proof (ignoring a timeout
    // here just means the fused proof below will legitimately fail
    // instead, with a clearer message about what was actually observed).
    let _ = server.conn_write_pending_for_test(stalled_conn, TIMEOUT);

    // Codex round-5 fix 2b/2c/3: fuse the proof with the act. Real
    // Windows CI diagnosis (this round): relying on `close_all`'s
    // `CloseHandle` alone to unstick a write genuinely stalled on full-
    // buffer backpressure was NOT observed to complete within this
    // test's 5s teardown budget -- `disconnect_listener` now issues an
    // explicit `CancelIoEx` per connection FIRST, which is what actually
    // and promptly unsticks it; the assert below reads the TOCTOU-free
    // latch that SAME cancellation recorded, not a stale pre-check.
    server.disconnect_listener();
    assert_eq!(
        server.conn_write_was_genuinely_pending_at_teardown_for_test(stalled_conn),
        Some(true),
        "expected the stalled connection's writer to be GENUINELY pending (ERROR_IO_PENDING) \
         at the exact instant disconnect_listener cancelled it"
    );

    let started = Instant::now();
    // A budget an order of magnitude under the pinned 20s: teardown must
    // not need to wait out the stalled connection's own I/O at all --
    // cancelling then closing its handle (disconnect_listener, above) is
    // what unsticks it, so healthy AND stalled connections alike tear
    // down promptly.
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

/// ADR 0041 step 6 U1b, acceptance matrix "teardown composes" — Codex
/// round-1 Blocker 3 discharge, "total-deadline propagation": the shared
/// deadline is REAL and honored against REAL OS threads, not merely a
/// number `join_within`'s own pure unit tests exercise against fully
/// controlled fake threads. `disconnect_listener`'s own redesign makes a
/// worker un-unstickable by ORDINARY means hard to construct (closing
/// every handle is specifically what unsticks them) — so this proves
/// deadline ENFORCEMENT itself is real and workload-independent: an
/// essentially-zero budget against otherwise-healthy, real connections
/// still returns promptly (never hangs out to the pinned 20s), and the
/// natural race (some threads may still finish before the first
/// `is_finished` poll) means this asserts BOUNDED total time, not a
/// specific `true`/`false` outcome — either is legitimate, a HANG is not.
#[test]
fn join_workers_deadline_is_enforced_against_real_threads_not_merely_computed() {
    if !run_isolated("join_workers_deadline_is_enforced_against_real_threads_not_merely_computed") {
        return;
    }
    let id = fresh_voyage_id();
    let max_instances = 4;
    let mut server = PipeServer::bind(&id, max_instances).unwrap();

    let mut clients = Vec::new();
    for _ in 0..3 {
        let c = connect_voyage_pipe(&id).unwrap();
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

/// Test 5: a client that connects and never reads while the server floods
/// it. The outbound BYTE budget eventually reports full once the
/// head-of-line `WriteFile` is stuck in the kernel; `close` is
/// fire-and-forget, so the bound under test is how promptly the `Closed`
/// event follows.
#[test]
fn flooded_never_reading_client_close_completes_within_bound() {
    if !run_isolated("flooded_never_reading_client_close_completes_within_bound") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();
    let client = connect_voyage_pipe(&id).unwrap(); // deliberately never reads
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

/// Test 6: a pending accept with no client ever connecting — server drop
/// must return promptly.
#[test]
fn pending_accept_with_no_client_drops_promptly() {
    if !run_isolated("pending_accept_with_no_client_drops_promptly") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 1).unwrap();
    drop(server);
}

/// New coverage (drop-vs-lifecycle-delivery regression): saturate the
/// events channel and never drain it, then drop the server. `Drop` must
/// still return -- it MUST NOT deadlock behind its own
/// `send_lifecycle_event` escape by joining the accept thread before
/// setting `dropping` (the accept thread's own `Accepted`/`AcceptError`
/// publishes can be stuck retrying against the very saturation this test
/// creates).
#[test]
fn drop_returns_even_with_a_saturated_events_channel() {
    if !run_isolated("drop_returns_even_with_a_saturated_events_channel") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();

    // Churn connections without ever draining events() -- each churn
    // queues at least Accepted + Closed(Eof), so a generous number of
    // attempts guarantees the channel fills well past its capacity, at
    // which point the accept thread itself is blocked delivering an
    // Accepted through send_lifecycle_event's retry loop.
    for _ in 0..200 {
        match connect_voyage_pipe(&id) {
            Ok(client) => drop(client),
            Err(_) => break, // the accept side is now saturated/stalled -- as intended
        }
    }

    // Must return even though nobody ever drained events().
    drop(server);
}
