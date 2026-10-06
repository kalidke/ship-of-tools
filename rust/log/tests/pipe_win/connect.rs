//! Connect, byte exchange, multiplexing, rival first instance and owner-only descriptor tests.

use super::*;

/// Test 1: one server, one client, bytes both ways (accumulated); a
/// marker-tagged send's `Sent` event fires once its `WriteFile`
/// physically completes.
#[test]
fn server_and_client_exchange_bytes_and_sent_carries_marker() {
    if !run_isolated("connect::server_and_client_exchange_bytes_and_sent_carries_marker") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 4).unwrap();
    let client = connect_voyage_pipe(&id).unwrap();
    let conn_id = expect_accepted(&server, TIMEOUT);

    let outbound = b"hello from client";
    client.write_all(outbound).unwrap();
    let got = accumulate_bytes(&server, conn_id, outbound.len(), TIMEOUT);
    assert_eq!(got, outbound);

    let inbound = b"hello from server";
    server.send(conn_id, inbound.to_vec(), Some(42)).unwrap();
    let mut buf = vec![0u8; inbound.len()];
    let mut got = 0;
    while got < buf.len() {
        got += client.read(&mut buf[got..]).unwrap();
    }
    assert_eq!(buf, inbound);

    match next_event(&server, TIMEOUT) {
        LaneEvent::Sent(cid, marker) => {
            assert_eq!(cid, conn_id);
            assert_eq!(marker, 42);
        }
        other => panic!("expected Sent, got {other:?}"),
    }

    drop(server);
}

/// Test 2: two clients connected to the same voyage pipe are multiplexed
/// by distinct `ConnId`s.
#[test]
fn two_concurrent_clients_multiplexed_by_conn_id() {
    if !run_isolated("connect::two_concurrent_clients_multiplexed_by_conn_id") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 4).unwrap();

    let client_a = connect_voyage_pipe(&id).unwrap();
    let conn_a = expect_accepted(&server, TIMEOUT);
    let client_b = connect_voyage_pipe(&id).unwrap();
    let conn_b = expect_accepted(&server, TIMEOUT);
    assert_ne!(conn_a, conn_b);

    client_a.write_all(b"from A").unwrap();
    client_b.write_all(b"from B").unwrap();

    let mut a_got = Vec::new();
    let mut b_got = Vec::new();
    let deadline = Instant::now() + TIMEOUT;
    while a_got.len() < 6 || b_got.len() < 6 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "timed out: a={a_got:?} b={b_got:?}");
        match next_event(&server, remaining) {
            LaneEvent::Bytes(cid, bytes) if cid == conn_a => a_got.extend(bytes),
            LaneEvent::Bytes(cid, bytes) if cid == conn_b => b_got.extend(bytes),
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(a_got, b"from A");
    assert_eq!(b_got, b"from B");

    drop(server);
}

/// Test 3: squat detection AND continuous name hold. With
/// `max_instances == 1`, several connect/close cycles run in a row; the
/// `FIRST_PIPE_INSTANCE` rival probe (using the SAME `nMaxInstances`,
/// round-3 finding 9) must fail EVERY time, including immediately after
/// each teardown. Only after `PipeServer::drop` does the probe succeed.
#[test]
fn rival_first_instance_create_fails_continuously_then_frees_on_drop() {
    if !run_isolated("connect::rival_first_instance_create_fails_continuously_then_frees_on_drop") {
        return;
    }
    let id = fresh_voyage_id();
    let max_instances = 1;
    let server = PipeServer::bind(&id, max_instances).unwrap();

    for _ in 0..3 {
        assert_squat_check_failed(try_create_first_instance(&id, max_instances).unwrap_err());

        let client = connect_voyage_pipe(&id).unwrap();
        let conn_id = expect_accepted(&server, TIMEOUT);
        server.close(conn_id);
        assert_eq!(
            expect_closed(&server, conn_id, TIMEOUT),
            ClosedReason::Closed
        );
        drop(client);

        assert_squat_check_failed(try_create_first_instance(&id, max_instances).unwrap_err());
    }

    drop(server);
    try_create_first_instance(&id, max_instances)
        .unwrap_or_else(|e| panic!("expected the freed name to bind again: {e}"));
}

/// ADR 0043 decision 27: an ABSENT pipe — no instance has EVER been
/// created for this voyage id — fails `connect_voyage_pipe` on the FIRST
/// `CreateFileW` attempt with `ERROR_FILE_NOT_FOUND`, never retried
/// within [`CONNECT_BOUND`] (this module's own "Continuous name hold"
/// doc: an instance is held and recycled once bound, so unavailable past
/// that point can only mean busy — absence is the caller's to poll, not
/// this bound's to spend). Elapsed time is asserted GENEROUSLY (< 1s) —
/// evidence the bound was never consumed, not a tight perf gate.
#[test]
fn connect_fails_fast_when_pipe_absent() {
    if !run_isolated("connect::connect_fails_fast_when_pipe_absent") {
        return;
    }
    let id = fresh_voyage_id(); // nothing ever binds this id

    let started = Instant::now();
    let err = connect_voyage_pipe(&id).unwrap_err();
    let elapsed = started.elapsed();

    // Codex review round finding 9: classification is the real proof; the
    // timing check is a loose sanity bound against `CONNECT_BOUND` itself
    // (the value the OLD retrying behavior would have fully consumed),
    // reported alongside it rather than a tight wall-clock gate a busy
    // runner could occasionally trip.
    eprintln!("connect_fails_fast_when_pipe_absent: elapsed={elapsed:?}");
    assert!(
        matches!(err, TransportError::Io { op, .. } if op == "CreateFileW"),
        "expected a CreateFileW error, got {err}"
    );
    assert!(
        elapsed < CONNECT_BOUND,
        "an absent pipe (ERROR_FILE_NOT_FOUND) must fail on the FIRST attempt, never consume the full {CONNECT_BOUND:?}: took {elapsed:?}"
    );
}

/// ADR 0043 decision 27: a BUSY pipe — every instance already claimed —
/// is a DIFFERENT case from an absent one, and stays retried within
/// [`CONNECT_BOUND`] exactly as before. Proven the same way
/// `rival_first_instance_create_fails_continuously_then_frees_on_drop`
/// proves continuous name hold: `max_instances = 1`, hold one client
/// connected (the only instance is now claimed), start a second connect
/// on its own thread, assert it is STILL PENDING after 300ms (busy, not
/// failed), release the first client (the instance recycles per this
/// module's own "Continuous name hold" doc), then assert the second
/// connect succeeds once the recycled instance is available again.
#[test]
fn connect_retries_within_the_bound_when_busy_then_succeeds_once_freed() {
    if !run_isolated("connect::connect_retries_within_the_bound_when_busy_then_succeeds_once_freed") {
        return;
    }
    let id = fresh_voyage_id();
    let max_instances = 1;
    let server = PipeServer::bind(&id, max_instances).unwrap();

    let first_client = connect_voyage_pipe(&id).unwrap();
    let first_conn = expect_accepted(&server, TIMEOUT);

    // Codex review round finding 9: synchronize on the thread actually
    // having STARTED before relying on any sleep at all -- a raw
    // `sleep(300ms)` with no such signal cannot tell "genuinely still
    // retrying" apart from "never got scheduled yet" on a busy runner.
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let id_for_thread = id.clone();
    let second = std::thread::spawn(move || {
        let _ = started_tx.send(());
        let started = Instant::now();
        let client = connect_voyage_pipe(&id_for_thread).expect("expected the busy retry to eventually succeed");
        (client, started.elapsed())
    });
    started_rx
        .recv_timeout(TIMEOUT)
        .expect("expected the second connect thread to signal it has started");

    // A generous grace period AFTER that signal -- long enough for at
    // least one real busy-retry round trip, short enough that the actual
    // join deadline below remains the meaningful bound.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !second.is_finished(),
        "expected the second connect to still be retrying against a busy pipe 300ms after it started"
    );

    server.close(first_conn);
    assert_eq!(expect_closed(&server, first_conn, TIMEOUT), ClosedReason::Closed);
    drop(first_client);

    let join_deadline = Instant::now() + CONNECT_BOUND + Duration::from_secs(5);
    while !second.is_finished() {
        assert!(
            Instant::now() < join_deadline,
            "expected the second connect thread to finish once the instance was freed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let (_second_client, elapsed) = second.join().unwrap();
    eprintln!("connect_retries_within_the_bound_when_busy_then_succeeds_once_freed: elapsed={elapsed:?}");
    assert!(
        elapsed < CONNECT_BOUND,
        "expected the busy retry to succeed comfortably inside {CONNECT_BOUND:?}, took {elapsed:?}"
    );
    drop(server);
}

/// ADR 0041 step 6 U1b, Lifecycle "the pipe NAME disappears before any
/// blocking join" — Codex round-2b Blocker 1 discharge: `disconnect_listener`
/// ALONE — never `join_workers`, never `Drop` — must free the name for a
/// `FIRST_PIPE_INSTANCE` rival probe the INSTANT it returns, WHILE A
/// CONNECTION IS STILL LIVE, not only after it has already closed. The
/// client's own handle is left open throughout (never dropped before the
/// probe): the SERVER-side instance `disconnect_listener` closes is what a
/// squat probe actually checks for, so a still-open client handle must not
/// keep the name held. NO POLL LOOP inside `disconnect_listener` itself:
/// it closes the live connection's handle synchronously (round 1) and the
/// pending accept's own listening instance handle synchronously too
/// (round 2b — cancelling alone only REQUESTS cancellation, asynchronously;
/// closing the handle directly is what actually makes the instance stop
/// existing) — a poll THERE would silently tolerate exactly the residual
/// delay this round's fix exists to remove.
///
/// Codex round-3 test-premise-gap fix: `max_instances = 2` means a SECOND
/// instance becomes the accept loop's own pending `ConnectNamedPipe` the
/// moment the first connection is accepted, but the accept loop reaching
/// that point is a RACE against this test thread — the previous version
/// of this test called `disconnect_listener` right after `expect_accepted`
/// with no guarantee that race had resolved, so it exercised live-
/// connection closure but did NOT deterministically prove a pending
/// accept handle existed at teardown too (the actual defect this test
/// exists to catch — Codex round-3 finding 1).
///
/// Codex round-4 finding 3: `AcceptState::current` alone is populated
/// BEFORE `ConnectNamedPipe` is ever issued, so polling it cannot prove
/// submission happened at all — and a separate "poll, then call
/// `disconnect_listener`" pair leaves a TOCTOU gap between the two calls.
///
/// Codex round-5 fix 2a/2b/2c: plain `SlotState::Pending` is ALSO set for
/// a synchronously-completed op still awaiting result collection, so
/// even polling THAT is not proof of a genuine `ERROR_IO_PENDING`
/// submission — and merely being in one function does not itself close
/// a TOCTOU between a pre-check and a later act.
/// `assert_accept_parked_then_disconnect_listener_for_test` polls the
/// accept slot's OWN genuine-async-pending signal (set only when `issue`
/// actually returns `ERROR_IO_PENDING`) purely to decide WHEN to call
/// `disconnect_listener`, then returns the TOCTOU-free LATCH that
/// `disconnect_listener`'s own synchronized cancellation records at the
/// exact instant it cancels — the proof and the act share one critical
/// section, so nothing can go stale in between.
#[test]
fn disconnect_listener_frees_the_name_even_with_a_live_connection() {
    if !run_isolated("connect::disconnect_listener_frees_the_name_even_with_a_live_connection") {
        return;
    }
    let id = fresh_voyage_id();
    let max_instances = 2;
    let mut server = PipeServer::bind(&id, max_instances).unwrap();

    // A LIVE connection: never closed by either side before the probe.
    let client = connect_voyage_pipe(&id).unwrap();
    expect_accepted(&server, TIMEOUT);

    assert!(
        server.assert_accept_parked_then_disconnect_listener_for_test(TIMEOUT),
        "expected a second pending accept instance (max_instances=2) to be genuinely parked \
         (ConnectNamedPipe issued) before teardown"
    );

    try_create_first_instance(&id, max_instances)
        .unwrap_or_else(|e| panic!("expected the name to be immediately winnable: {e}"));
    drop(client);
    drop(server);
}

/// Test 4: the pipe's own security descriptor, queried on a LIVE HANDLE
/// via `GetSecurityInfo` — protected, owner-only full access, NO `OI`/`CI`
/// inheritance flags.
#[test]
fn pipe_descriptor_is_protected_owner_only_with_no_container_inherit_flags() {
    if !run_isolated("connect::pipe_descriptor_is_protected_owner_only_with_no_container_inherit_flags") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 1).unwrap();

    let handle = open_pipe_handle(&id);
    let sid = current_user_sid_string();
    let expected = canonical_sddl(&format!("D:P(A;;FA;;;{sid})"));
    let actual = security_descriptor_sddl(handle);
    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };

    assert_eq!(actual, expected);
    assert!(
        !actual.contains("OICI"),
        "pipe descriptor must carry no OI/CI flags: {actual}"
    );

    drop(server);
}
