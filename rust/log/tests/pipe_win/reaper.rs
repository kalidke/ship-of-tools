//! Independent reaping: a held or panicking worker, a full events channel, a failed recycle or a late registration
//! never stalls another connection's close or the server's shutdown.

use super::*;
use sot_log::lane::test_progress::{Checkpoint, Pause, Role};

/// The short per-connection teardown budget a test gives a server it expects to expire.
const SHORT: Duration = Duration::from_millis(200);
/// How long one expected checkpoint may take; well inside the isolation bound.
const RECORD: Duration = Duration::from_secs(5);

fn at(conn: ConnId, step: &'static str) -> impl Fn(&Checkpoint) -> bool {
    move |r| r.conn == Some(conn) && r.step == step
}

/// Wait for the first checkpoint `wanted` accepts; on expiry fail with the named wait and the progress snapshot.
fn await_progress(
    server: &PipeServer,
    what: &str,
    wanted: impl Fn(&Checkpoint) -> bool,
) -> Checkpoint {
    let deadline = Instant::now() + RECORD;
    loop {
        let snapshot = server.progress_for_test();
        if !snapshot.unavailable {
            if let Some(found) = snapshot.records.iter().find(|r| wanted(r)) {
                return found.clone();
            }
        }
        assert!(
            Instant::now() < deadline,
            "pipe wait {what}: not observed within {RECORD:?}\n{snapshot}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The worker stopped at its hold; the gate is the deterministic proof.
fn reached(server: &PipeServer, what: &str, hold: &Pause) {
    assert!(
        hold.wait_reached(RECORD),
        "pipe wait {what}: the worker never reached its hold\n{}",
        server.progress_for_test()
    );
}

/// Connect one client and consume its `Accepted`.
fn connect(server: &PipeServer, id: &str) -> (sot_log::lane::pipe_win::PipeClient, ConnId) {
    let client = connect_voyage_pipe(id).unwrap();
    let conn = expect_accepted(server, TIMEOUT);
    (client, conn)
}

fn read_exact(client: &sot_log::lane::pipe_win::PipeClient, want: usize) {
    let mut got = 0;
    let mut buf = [0u8; 64];
    while got < want {
        let n = client.read(&mut buf[..want - got]).unwrap();
        assert!(n > 0, "peer closed before {want} bytes");
        got += n;
    }
}

/// Every event until `Closed` has been seen for each of `wanted`.
fn drain_closes(server: &PipeServer, wanted: &[ConnId]) -> Vec<(ConnId, ClosedReason)> {
    let deadline = Instant::now() + TIMEOUT;
    let mut closes: Vec<(ConnId, ClosedReason)> = Vec::new();
    while !wanted.iter().all(|id| closes.iter().any(|(c, _)| c == id)) {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "pipe wait drain.closes: not every Closed arrived"
        );
        if let LaneEvent::Closed(id, reason) = next_event(server, left) {
            closes.push((id, reason));
        }
    }
    closes
}

/// A held worker of A must not delay B's close (red before B2: the reaper blocks on A's join).
fn held_does_not_delay_another_closed(role: Role) {
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();
    let (client_a, a) = connect(&server, &id);
    let (client_b, b) = connect(&server, &id);
    let hold = server.hold_worker_exit_for_test(a, role);
    server.close(a);
    reached(&server, "a.held", &hold);
    drop(client_b);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "pipe wait b.closed: Closed(B,Eof) never arrived while A was held\n{}",
            server.progress_for_test()
        );
        match server.events().recv_timeout(left) {
            Ok(LaneEvent::Closed(c, reason)) if c == b => {
                assert_eq!(reason, ClosedReason::Eof);
                break;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    hold.release();
    let closes = drain_closes(&server, &[a]);
    assert_eq!(
        closes
            .iter()
            .filter(|(c, _)| *c == a)
            .map(|(_, r)| r.clone())
            .collect::<Vec<_>>(),
        vec![ClosedReason::Closed],
        "exactly one Closed(A,Closed)"
    );
    drop(client_a);
}

#[test]
fn held_reader_does_not_delay_another_closed() {
    if !run_isolated("reaper::held_reader_does_not_delay_another_closed") {
        return;
    }
    held_does_not_delay_another_closed(Role::Reader);
}

#[test]
fn held_writer_does_not_delay_another_closed() {
    if !run_isolated("reaper::held_writer_does_not_delay_another_closed") {
        return;
    }
    held_does_not_delay_another_closed(Role::Writer);
}

/// Fill the events channel with exactly 64 real `Sent` markers of `conn`, draining its client bytes and observing
/// every successful enqueue; no 65th event is produced.
fn fill_with_sent(server: &PipeServer, conn: ConnId, client: &sot_log::lane::pipe_win::PipeClient) {
    for marker in 0..64u64 {
        server
            .send(conn, vec![marker as u8; 16], Some(marker))
            .unwrap();
        read_exact(client, 16);
        let want = format!("ok marker={marker}");
        await_progress(server, "b.enqueued", |r| {
            r.conn == Some(conn) && r.step == "sent.enqueue" && r.result == want
        });
    }
}

/// A full events channel blocks A's `Closed`; it must not stop the reaper joining B (red before B2: `b.joined`).
#[test]
fn full_events_do_not_stop_pending_polls() {
    if !run_isolated("reaper::full_events_do_not_stop_pending_polls") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();
    let (client_a, a) = connect(&server, &id);
    let (client_b, b) = connect(&server, &id);
    fill_with_sent(&server, b, &client_b);
    server.close(a);
    await_progress(&server, "a.closed.full", |r| {
        r.conn == Some(a) && r.step == "closed.enqueue" && r.result == "full"
    });
    drop(client_b);
    for step in ["reader.join.end", "writer.join.end"] {
        await_progress(&server, "b.joined", at(b, step));
    }
    let deadline = Instant::now() + TIMEOUT;
    let (mut markers, mut closes) = (Vec::new(), Vec::new());
    while markers.len() < 64 || closes.len() < 2 {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "pipe wait drain: 64 markers and both closes"
        );
        match next_event(&server, left) {
            LaneEvent::Sent(c, marker) => {
                assert_eq!(c, b);
                markers.push(marker);
            }
            LaneEvent::Closed(c, _) => closes.push(c),
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!(markers, (0..64).collect::<Vec<u64>>());
    closes.sort_unstable();
    assert_eq!(closes, vec![a, b], "each close exactly once");
    drop(client_a);
}

/// The pinned recycle-error protocol: A's recycle fails while the channel is full, and the terminal `AcceptError`
/// meets a real `Full`; B's joins must still finish and B's close stay retained (red before B2: `b.joined`).
#[test]
fn recycle_error_does_not_block_other_closes() {
    if !run_isolated("reaper::recycle_error_does_not_block_other_closes") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();
    let (client_a, a) = connect(&server, &id);
    let (client_b, b) = connect(&server, &id);
    let barrier = server.pause_recycle_for_test();
    server.close(a);
    reached(&server, "a.recycle.barrier", &barrier);
    // Drain the events that precede the recycle attempt, A's `Closed` among them if it is already out.
    let mut drained = Vec::new();
    while let Ok(event) = server.events().try_recv() {
        drained.push(event);
    }
    fill_with_sent(&server, b, &client_b);
    server.fail_next_recycle_for_test();
    barrier.release();
    await_progress(&server, "accept_error.full", |r| {
        r.step == "accept_error.enqueue" && r.result == "full"
    });
    drop(client_b);
    for step in ["reader.join.end", "writer.join.end"] {
        await_progress(&server, "b.joined", at(b, step));
    }
    let deadline = Instant::now() + TIMEOUT;
    let (mut markers, mut closes, mut errors) = (Vec::new(), Vec::new(), 0);
    for event in drained {
        if let LaneEvent::Closed(c, _) = event {
            closes.push(c);
        }
    }
    while markers.len() < 64 || closes.len() < 2 || errors < 1 {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "pipe wait drain: markers, closes and the AcceptError"
        );
        match next_event(&server, left) {
            LaneEvent::Sent(c, marker) => {
                assert_eq!(c, b);
                markers.push(marker);
            }
            LaneEvent::Closed(c, _) => closes.push(c),
            LaneEvent::AcceptError(_) => errors += 1,
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!(markers, (0..64).collect::<Vec<u64>>());
    closes.sort_unstable();
    assert_eq!(closes, vec![a, b], "each close exactly once");
    assert_eq!(errors, 1, "exactly one AcceptError");
    drop(client_a);
}

/// A failed recycle retains the dead instance, which keeps the pipe name held. One instance only, so no other
/// instance of the name (B's still-registered one, the acceptor's) can be what refuses the squat probe; closing every
/// registered instance, the retained one included, frees the name, which shows it was the retained one that held it.
#[test]
fn recycle_failure_retains_the_dead_instance() {
    if !run_isolated("reaper::recycle_failure_retains_the_dead_instance") {
        return;
    }
    let id = fresh_voyage_id();
    let mut server = PipeServer::bind(&id, 1).unwrap();
    let (client, a) = connect(&server, &id);
    server.fail_next_recycle_for_test();
    server.close(a);
    await_progress(&server, "recycle.failed", |r| {
        r.step == "recycle.result" && r.result == "false"
    });
    assert_squat_check_failed(try_create_first_instance(&id, 1).unwrap_err());
    drop(client);
    server.disconnect_listener();
    try_create_first_instance(&id, 1)
        .expect("the name stayed held after the server closed every registered instance");
    assert!(server.join_workers(Instant::now() + RECORD));
}

/// A worker held past a normal close's short budget is reported by name and stays owned; the report does not fail the
/// run-end teardown once the worker has finished.
fn expired_worker(role: Role) {
    let id = fresh_voyage_id();
    let mut server = PipeServer::bind(&id, 1).unwrap();
    server.set_teardown_deadline_for_test(SHORT);
    let (client, a) = connect(&server, &id);
    let hold = server.hold_worker_exit_for_test(a, role);
    server.close(a);
    let want = format!(
        "worker={}",
        if role == Role::Reader {
            "reader"
        } else {
            "writer"
        }
    );
    await_progress(&server, "expiry.record", |r| {
        r.conn == Some(a) && r.step == "pending.expired" && r.result == want
    });
    assert!(
        server.events().recv_timeout(SHORT).is_err(),
        "Closed(A) was delivered while a worker was unfinished"
    );
    hold.release();
    let closes = drain_closes(&server, &[a]);
    assert_eq!(closes[0].1, ClosedReason::Closed);
    server.disconnect_listener();
    assert!(
        server.join_workers(Instant::now() + RECORD),
        "a normal close that expired before shutdown failed the run-end teardown"
    );
    drop(client);
}

#[test]
fn expired_reader_is_reported_and_remains_owned() {
    if !run_isolated("reaper::expired_reader_is_reported_and_remains_owned") {
        return;
    }
    expired_worker(Role::Reader);
}

#[test]
fn expired_writer_is_reported_and_remains_owned() {
    if !run_isolated("reaper::expired_writer_is_reported_and_remains_owned") {
        return;
    }
    expired_worker(Role::Writer);
}

/// A pair that expired its normal-close budget earlier and is still unfinished at the shutdown deadline fails the
/// teardown: the reaper cannot end while it owns the pair.
fn expired_then_unfinished_at_shutdown(role: Role) {
    let id = fresh_voyage_id();
    let mut server = PipeServer::bind(&id, 1).unwrap();
    server.set_teardown_deadline_for_test(SHORT);
    let (client, a) = connect(&server, &id);
    let hold = server.hold_worker_exit_for_test(a, role);
    server.close(a);
    let want = format!(
        "worker={}",
        if role == Role::Reader {
            "reader"
        } else {
            "writer"
        }
    );
    await_progress(&server, "expiry.record", |r| {
        r.conn == Some(a) && r.step == "pending.expired" && r.result == want
    });
    server.disconnect_listener();
    assert!(
        !server.join_workers(Instant::now() + Duration::from_millis(300)),
        "shutdown reported success with A still unfinished"
    );
    hold.release();
    await_progress(&server, "a.done", at(a, "pending.done"));
    drop(client);
}

#[test]
fn expired_pair_unfinished_at_shutdown_fails_teardown_reader_held() {
    if !run_isolated("reaper::expired_pair_unfinished_at_shutdown_fails_teardown_reader_held") {
        return;
    }
    expired_then_unfinished_at_shutdown(Role::Reader);
}

#[test]
fn expired_pair_unfinished_at_shutdown_fails_teardown_writer_held() {
    if !run_isolated("reaper::expired_pair_unfinished_at_shutdown_fails_teardown_writer_held") {
        return;
    }
    expired_then_unfinished_at_shutdown(Role::Writer);
}

/// An injected worker panic is reported as a completed panic and closes the connection with an error.
fn panicked_worker(role: Role) {
    let id = fresh_voyage_id();
    let mut server = PipeServer::bind(&id, 1).unwrap();
    let (client, a) = connect(&server, &id);
    server.inject_worker_panic_for_test(a, role);
    server.close(a);
    let closes = drain_closes(&server, &[a]);
    assert_eq!(
        closes[0].1,
        ClosedReason::Error("connection worker panicked".into())
    );
    await_progress(&server, "panic.record", |r| {
        r.conn == Some(a) && r.step == "pending.panicked"
    });
    server.disconnect_listener();
    assert!(
        !server.join_workers(Instant::now() + RECORD),
        "a completed worker panic must latch failed teardown"
    );
    drop(client);
}

#[test]
fn worker_panic_is_reported_reader() {
    if !run_isolated("reaper::worker_panic_is_reported_reader") {
        return;
    }
    panicked_worker(Role::Reader);
}

#[test]
fn worker_panic_is_reported_writer() {
    if !run_isolated("reaper::worker_panic_is_reported_writer") {
        return;
    }
    panicked_worker(Role::Writer);
}

/// A panicking worker with its peer held: only the unfinished peer is named by the expiry record.
fn panic_with_held_peer(panicking: Role, held: Role) {
    let name = |role: Role| {
        if role == Role::Reader {
            "reader"
        } else {
            "writer"
        }
    };
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 1).unwrap();
    server.set_teardown_deadline_for_test(SHORT);
    let (client, a) = connect(&server, &id);
    server.inject_worker_panic_for_test(a, panicking);
    let hold = server.hold_worker_exit_for_test(a, held);
    server.close(a);
    let exit = if panicking == Role::Reader {
        "reader.exit"
    } else {
        "writer.exit"
    };
    await_progress(&server, "peer.exit", |r| {
        r.conn == Some(a) && r.step == exit
    });
    let want = format!("worker={}", name(held));
    await_progress(&server, "expiry.record", |r| {
        r.conn == Some(a) && r.step == "pending.expired" && r.result == want
    });
    hold.release();
    let closes = drain_closes(&server, &[a]);
    assert_eq!(
        closes[0].1,
        ClosedReason::Error("connection worker panicked".into())
    );
    drop(client);
}

#[test]
fn panic_with_held_peer_reports_only_unfinished_expiry_reader_held() {
    if !run_isolated("reaper::panic_with_held_peer_reports_only_unfinished_expiry_reader_held") {
        return;
    }
    panic_with_held_peer(Role::Writer, Role::Reader);
}

#[test]
fn panic_with_held_peer_reports_only_unfinished_expiry_writer_held() {
    if !run_isolated("reaper::panic_with_held_peer_reports_only_unfinished_expiry_writer_held") {
        return;
    }
    panic_with_held_peer(Role::Reader, Role::Writer);
}

/// Shutdown applies one short absolute deadline to a pair that is already pending, and the pair stays owned.
#[test]
fn shutdown_observes_pending_connections() {
    if !run_isolated("reaper::shutdown_observes_pending_connections") {
        return;
    }
    let id = fresh_voyage_id();
    let mut server = PipeServer::bind(&id, 2).unwrap();
    let (client_a, a) = connect(&server, &id);
    let hold = server.hold_worker_exit_for_test(a, Role::Reader);
    server.close(a);
    reached(&server, "a.held", &hold);
    server.disconnect_listener();
    let budget = Duration::from_millis(300);
    let started = Instant::now();
    assert!(
        !server.join_workers(started + budget),
        "shutdown reported success with A still pending"
    );
    assert!(
        started.elapsed() < budget + Duration::from_secs(2),
        "shutdown took another per-pair budget: {:?}",
        started.elapsed()
    );
    hold.release();
    await_progress(&server, "a.joined", at(a, "reader.join.end"));
    drop(client_a);
}

/// Phase one cancels registered pairs but leaves them for the reaper to claim; none is joined by the caller.
fn registered_pairs_through_reaper(held: Role) {
    let id = fresh_voyage_id();
    let mut server = PipeServer::bind(&id, 2).unwrap();
    let (client_a, a) = connect(&server, &id);
    let (client_b, b) = connect(&server, &id);
    let hold = server.hold_worker_exit_for_test(a, held);
    server.disconnect_listener();
    let started = Instant::now();
    assert!(
        !server.join_workers(started + Duration::from_millis(500)),
        "shutdown reported success with A held"
    );
    for conn in [a, b] {
        await_progress(&server, "claim", at(conn, "claim"));
    }
    for step in ["reader.join.end", "writer.join.end"] {
        await_progress(&server, "b.joined", at(b, step));
    }
    await_progress(&server, "a.expired", at(a, "pending.expired"));
    hold.release();
    await_progress(&server, "a.done", at(a, "pending.done"));
    drop((client_a, client_b));
}

#[test]
fn shutdown_routes_registered_pairs_through_reaper_reader_held() {
    if !run_isolated("reaper::shutdown_routes_registered_pairs_through_reaper_reader_held") {
        return;
    }
    registered_pairs_through_reaper(Role::Reader);
}

#[test]
fn shutdown_routes_registered_pairs_through_reaper_writer_held() {
    if !run_isolated("reaper::shutdown_routes_registered_pairs_through_reaper_writer_held") {
        return;
    }
    registered_pairs_through_reaper(Role::Writer);
}

/// A pending teardown keeps its instance charged: with one instance and A pending, no second client connects.
/// Preservation: the parent already recycles only after both joins.
#[test]
fn pending_teardown_stays_inside_connection_capacity() {
    if !run_isolated("reaper::pending_teardown_stays_inside_connection_capacity") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 1).unwrap();
    let (client_a, a) = connect(&server, &id);
    let hold = server.hold_worker_exit_for_test(a, Role::Reader);
    server.close(a);
    reached(&server, "a.held", &hold);
    assert!(
        connect_voyage_pipe(&id).is_err(),
        "a second client connected while a pending teardown held the only instance"
    );
    hold.release();
    drain_closes(&server, &[a]);
    let (client_d, _) = connect(&server, &id);
    drop((client_a, client_d));
}

/// Shutdown beats a registration the acceptor had already started: the pair is aborted and joined by the acceptor,
/// and no `Accepted`, gate-open or registration follows (red before B2: `registration crossed shutdown cutoff`).
#[test]
fn registration_after_shutdown_is_rejected() {
    if !run_isolated("reaper::registration_after_shutdown_is_rejected") {
        return;
    }
    let id = fresh_voyage_id();
    let mut server = PipeServer::bind(&id, 2).unwrap();
    let barrier = server.pause_registration_for_test();
    let client = connect_voyage_pipe(&id).unwrap();
    assert!(
        barrier.wait_reached(RECORD),
        "the acceptor never reached the registration barrier"
    );
    server.disconnect_listener();
    barrier.release();
    let ok = server.join_workers(Instant::now() + RECORD);
    let snapshot = server.progress_for_test();
    let inserted = snapshot
        .records
        .iter()
        .any(|r| r.step == "registration.cutoff" && r.result == "inserted");
    assert!(
        !inserted,
        "registration crossed shutdown cutoff\n{snapshot}"
    );
    assert!(
        ok,
        "registration after shutdown left a worker behind\n{snapshot}"
    );
    assert!(
        snapshot
            .records
            .iter()
            .any(|r| r.step == "registration.cutoff" && r.result == "rejected"),
        "late registration was not rejected\n{snapshot}"
    );
    assert!(
        !snapshot
            .records
            .iter()
            .any(|r| r.step == "accepted.enqueue"),
        "late Accepted\n{snapshot}"
    );
    assert!(server.events().try_recv().is_err());
    drop(client);
}

/// A server thread that panicked fails the teardown for good: a panicked reaper leaves its pending pairs unjoined, and
/// the shutdown that joined it used to see only a finished thread.
fn server_thread_panic(thread: &str) {
    let id = fresh_voyage_id();
    let mut server = PipeServer::bind(&id, 2).unwrap();
    let client = if thread == "reaper" {
        let (client, a) = connect(&server, &id);
        server.inject_reaper_panic_for_test();
        server.close(a);
        client
    } else {
        server.inject_acceptor_panic_for_test();
        connect_voyage_pipe(&id).unwrap()
    };
    await_progress(&server, "panic.injected", |r| r.result == "panic injected");
    server.disconnect_listener();
    assert!(
        !server.join_workers(Instant::now() + RECORD),
        "a panicked {thread} thread reported a clean teardown"
    );
    drop(client);
}

#[test]
fn a_panicked_reaper_fails_the_teardown() {
    if !run_isolated("reaper::a_panicked_reaper_fails_the_teardown") {
        return;
    }
    server_thread_panic("reaper");
}

#[test]
fn a_panicked_acceptor_fails_the_teardown() {
    if !run_isolated("reaper::a_panicked_acceptor_fails_the_teardown") {
        return;
    }
    server_thread_panic("acceptor");
}
