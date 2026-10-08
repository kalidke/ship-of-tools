//! Independent reaping: a held or panicking worker, a full events channel or a late registration never stalls
//! another connection's close or the server's shutdown, and every `shutdown(2)` caller reports its own result.

use super::*;
use sot_log::lane::test_progress::{Checkpoint, Pause, Role};
use sot_log::test_isolated::{enter, ISOLATION_TIMEOUT};

/// The short per-connection teardown budget a test gives a server it expects to expire.
pub(super) const SHORT: Duration = Duration::from_millis(200);
/// How long one expected record may take; well inside the isolation bound.
pub(super) const RECORD: Duration = Duration::from_secs(5);

macro_rules! isolated {
    ($test:expr) => {
        if !named!(
            $test,
            "child.wait",
            "isolated body and bounded completion",
            None
        ) {
            return;
        }
    };
}

pub(super) use isolated;

pub(super) fn child_role(test: &str) -> bool {
    if std::env::var("SOT_TEST_SOCKET_ROLE").as_deref() == Ok(test) {
        enter(test);
        true
    } else {
        false
    }
}

pub(super) fn role_name(role: Role) -> &'static str {
    match role {
        Role::Reader => "reader",
        Role::Writer => "writer",
    }
}

/// One server in a fresh runtime folder; declared before any hold, so every hold is released before it drops.
pub(super) struct Fixture {
    _rt: RuntimeDirGuard,
    pub(super) server: SocketServer,
    pub(super) path: std::path::PathBuf,
}

pub(super) fn fixture(test: &str, max_connections: u32) -> Fixture {
    let rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = io_named!(
        test,
        "server.bind",
        "server bound",
        None,
        SocketServer::bind(&id, max_connections)
    )
    .unwrap();
    Fixture {
        _rt: rt,
        server,
        path,
    }
}

/// Connect one client and consume its `Accepted`.
pub(super) fn connect(f: &Fixture, test: &str, label: &str) -> (UnixStream, ConnId) {
    let client = io_named!(
        test,
        &format!("{label}.connect"),
        "connected",
        None,
        UnixStream::connect(&f.path)
    )
    .unwrap();
    let conn = expect_accepted(&f.server, test, &format!("{label}.accept"), TIMEOUT);
    (client, conn)
}

pub(super) fn at(conn: ConnId, step: &'static str) -> impl Fn(&Checkpoint) -> bool {
    move |r| r.conn == Some(conn) && r.step == step
}

/// Wait, under one named context, for the first checkpoint `wanted` accepts; a snapshot the recorder could not give is
/// retried, and the wait reports its own progress snapshot when it expires.
pub(super) fn await_progress(
    server: &SocketServer,
    test: &str,
    step: &str,
    expected: &str,
    conn: Option<ConnId>,
    wanted: impl Fn(&Checkpoint) -> bool,
) -> Checkpoint {
    let wait = WaitContext::new(test, step, expected, conn, RECORD);
    loop {
        wait.check(Some(server));
        let snapshot = server.progress_for_test();
        if !snapshot.unavailable {
            if let Some(found) = snapshot.records.iter().find(|r| wanted(r)) {
                wait.complete("ok", None, Some(server));
                return found.clone();
            }
        }
        wait.pause(Duration::from_millis(5));
    }
}

/// The worker stopped at its hold: the gate is the deterministic proof (a reader gets there only after the reaper's
/// `shutdown(2)`, a writer only after the reaper released its sender), not a lossy checkpoint.
pub(super) fn reached(
    server: &SocketServer,
    test: &str,
    step: &str,
    expected: &str,
    conn: ConnId,
    hold: &Pause,
) {
    let wait = WaitContext::new(test, step, expected, Some(conn), RECORD);
    if hold.wait_reached(RECORD) {
        wait.complete("ok", None, Some(server));
    } else {
        wait.fail("timeout", "the worker never reached its hold", Some(server));
    }
}

/// How many checkpoints match, or none when the snapshot is unavailable.
pub(super) fn count(server: &SocketServer, wanted: impl Fn(&Checkpoint) -> bool) -> usize {
    let snapshot = server.progress_for_test();
    snapshot.records.iter().filter(|r| wanted(r)).count()
}

pub(super) fn read_exact_named(
    server: &SocketServer,
    test: &str,
    step: &str,
    conn: ConnId,
    client: &mut UnixStream,
    want: usize,
) {
    let wait = WaitContext::new(test, step, "bytes read", Some(conn), TIMEOUT);
    let mut got = 0;
    let mut buf = [0u8; 64];
    while got < want {
        let n = wait
            .read(Some(server), client, &mut buf[..want - got])
            .unwrap();
        assert!(n > 0, "peer closed before {want} bytes");
        got += n;
    }
}

/// Every event until `Closed` has been seen for each of `wanted`, with how often each connection closed.
pub(super) fn drain_closes(
    server: &SocketServer,
    test: &str,
    wanted: &[ConnId],
) -> Vec<(ConnId, ClosedReason)> {
    let wait = WaitContext::new(test, "drain.closes", "every Closed", None, TIMEOUT);
    let mut closes = Vec::new();
    while !wanted
        .iter()
        .all(|id| closes.iter().any(|(c, _): &(ConnId, ClosedReason)| c == id))
    {
        if let LaneEvent::Closed(id, reason) = wait.next(server) {
            closes.push((id, reason));
        }
    }
    wait.complete("ok", None, Some(server));
    closes
}

/// A held worker of A must not delay B's close (red before B2: the reaper blocks on A's join).
fn held_does_not_delay_another_closed(test: &str, role: Role) {
    let f = fixture(test, 2);
    let (client_a, a) = connect(&f, test, "a");
    let (client_b, b) = connect(&f, test, "b");
    let hold = f.server.hold_worker_exit_for_test(a, role);
    named!(test, "a.close", Some(a), f.server.close(a));
    reached(
        &f.server,
        test,
        "a.held",
        "A worker held after the reaper's dequeue",
        a,
        &hold,
    );
    named!(test, "b.drop", Some(b), drop(client_b));
    let b_closed = WaitContext::new(test, "b.closed", "Closed(B,Eof)", Some(b), TIMEOUT);
    match b_closed.until(
        &f.server,
        |event| matches!(event, LaneEvent::Closed(id, _) if *id == b),
    ) {
        LaneEvent::Closed(_, reason) => assert_eq!(reason, ClosedReason::Eof),
        other => panic!("expected Closed(B), got {other:?}"),
    }
    named!(test, "a.release", Some(a), hold.release());
    let closes = drain_closes(&f.server, test, &[a]);
    assert_eq!(
        closes
            .iter()
            .filter(|(id, _)| *id == a)
            .map(|(_, reason)| reason.clone())
            .collect::<Vec<_>>(),
        vec![ClosedReason::Closed],
        "exactly one Closed(A,Closed)"
    );
    named!(test, "a.client.drop", Some(a), drop(client_a));
    named!(test, "server.drop", None, drop(f));
}

#[test]
fn held_reader_does_not_delay_another_closed() {
    let test = "reaper::held_reader_does_not_delay_another_closed";
    isolated!(test);
    held_does_not_delay_another_closed(test, Role::Reader);
}

#[test]
fn held_writer_does_not_delay_another_closed() {
    let test = "reaper::held_writer_does_not_delay_another_closed";
    isolated!(test);
    held_does_not_delay_another_closed(test, Role::Writer);
}

/// A full events channel blocks A's `Closed`; it must not stop the reaper joining B (red before B2: `b.joined`).
#[test]
fn full_events_do_not_stop_pending_polls() {
    let test = "reaper::full_events_do_not_stop_pending_polls";
    isolated!(test);
    let f = fixture(test, 2);
    let (client_a, a) = connect(&f, test, "a");
    let (mut client_b, b) = connect(&f, test, "b");
    for marker in 0..64u64 {
        io_named!(
            test,
            "b.send",
            "queued",
            Some(b),
            f.server.send(b, vec![marker as u8; 16], Some(marker))
        )
        .unwrap();
        read_exact_named(&f.server, test, "b.drain", b, &mut client_b, 16);
        let want = format!("ok marker={marker}");
        await_progress(
            &f.server,
            test,
            "b.enqueued",
            "Sent enqueued",
            Some(b),
            |r| r.conn == Some(b) && r.step == "sent.enqueue" && r.result == want,
        );
    }
    named!(test, "a.close", Some(a), f.server.close(a));
    await_progress(
        &f.server,
        test,
        "a.closed.full",
        "Closed(A) met a full channel",
        Some(a),
        |r| r.conn == Some(a) && r.step == "closed.enqueue" && r.result == "full",
    );
    named!(test, "b.drop", Some(b), drop(client_b));
    for step in ["reader.join.end", "writer.join.end"] {
        await_progress(
            &f.server,
            test,
            "b.joined",
            "B worker joined while A's close is blocked",
            Some(b),
            at(b, step),
        );
    }
    let wait = WaitContext::new(test, "drain", "64 markers and both closes", None, TIMEOUT);
    let (mut markers, mut closes) = (Vec::new(), Vec::new());
    while markers.len() < 64 || closes.len() < 2 {
        match wait.next(&f.server) {
            LaneEvent::Sent(id, marker) => {
                assert_eq!(id, b);
                markers.push(marker);
            }
            LaneEvent::Closed(id, _) => closes.push(id),
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!(markers, (0..64).collect::<Vec<u64>>());
    closes.sort_unstable();
    assert_eq!(closes, vec![a, b], "each close exactly once");
    named!(test, "a.client.drop", Some(a), drop(client_a));
    named!(test, "server.drop", None, drop(f));
}

/// A worker held past its short budget is reported by name and stays owned; the failure outlives the release.
fn expired_worker(test: &str, role: Role) {
    if child_role(test) {
        let mut f = fixture(test, 1);
        f.server.set_teardown_deadline_for_test(SHORT);
        let (client, a) = connect(&f, test, "a");
        assert_eq!(a, 0, "the first connection is conn=0");
        let hold = f.server.hold_worker_exit_for_test(a, role);
        named!(test, "a.close", Some(a), f.server.close(a));
        let want = format!("worker={}", role_name(role));
        await_progress(
            &f.server,
            test,
            "expiry.record",
            "expiry reported for the held worker",
            Some(a),
            |r| r.conn == Some(a) && r.step == "pending.expired" && r.result == want,
        );
        assert!(
            matches!(
                f.server.events().recv_timeout(SHORT),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "Closed(A) was delivered while a worker was unfinished"
        );
        named!(test, "a.release", Some(a), hold.release());
        let closes = drain_closes(&f.server, test, &[a]);
        assert_eq!(closes[0].1, ClosedReason::Closed);
        named!(
            test,
            "listener.disconnect",
            None,
            f.server.disconnect_listener()
        );
        let ok = WaitContext::new(
            test,
            "workers.join",
            "permanent failed teardown",
            None,
            RECORD,
        )
        .workers(&mut f.server);
        assert!(!ok, "expiry latch lost after the worker was released");
        named!(test, "client.drop", None, drop(client));
        named!(test, "server.drop", None, drop(f));
        return;
    }
    let captured = capture(test, ISOLATION_TIMEOUT, None);
    let record = format!(
        "sot-sock: connection teardown failed conn=0 worker={} elapsed_ms=",
        role_name(role)
    );
    let line = captured
        .text
        .lines()
        .find(|line| line.contains(&record))
        .unwrap_or_else(|| panic!("expiry record missing: {}", captured.text));
    assert!(line.ends_with("reason=deadline-expired; unfinished workers remain owned"));
    assert!(captured
        .text
        .contains("sot-sock: server teardown failed; see worker panic and deadline records"));
}

#[test]
fn expired_reader_is_reported_and_remains_owned() {
    expired_worker(
        "reaper::expired_reader_is_reported_and_remains_owned",
        Role::Reader,
    );
}

#[test]
fn expired_writer_is_reported_and_remains_owned() {
    expired_worker(
        "reaper::expired_writer_is_reported_and_remains_owned",
        Role::Writer,
    );
}

/// An injected worker panic is reported as a completed panic and closes the connection with an error.
fn panicked_worker(test: &str, role: Role) {
    if child_role(test) {
        let mut f = fixture(test, 1);
        let (client, a) = connect(&f, test, "a");
        f.server.inject_worker_panic_for_test(a, role);
        named!(test, "a.close", Some(a), f.server.close(a));
        let wait = WaitContext::new(
            test,
            "a.closed",
            "Closed(Error(panicked))",
            Some(a),
            TIMEOUT,
        );
        match wait.until(
            &f.server,
            |event| matches!(event, LaneEvent::Closed(id, _) if *id == a),
        ) {
            LaneEvent::Closed(_, reason) => assert_eq!(
                reason,
                ClosedReason::Error("connection worker panicked".into())
            ),
            other => panic!("expected Closed(A), got {other:?}"),
        }
        named!(
            test,
            "listener.disconnect",
            None,
            f.server.disconnect_listener()
        );
        let ok = WaitContext::new(
            test,
            "workers.join",
            "permanent failed teardown",
            None,
            RECORD,
        )
        .workers(&mut f.server);
        assert!(!ok, "a completed worker panic must latch failed teardown");
        named!(test, "client.drop", None, drop(client));
        named!(test, "server.drop", None, drop(f));
        return;
    }
    let captured = capture(test, ISOLATION_TIMEOUT, None);
    let record = format!(
        "sot-sock: connection teardown failed conn=0 worker={} elapsed_ms=",
        role_name(role)
    );
    let line = captured
        .text
        .lines()
        .find(|line| line.contains(&record))
        .unwrap_or_else(|| panic!("worker-panicked record missing: {}", captured.text));
    assert!(line.ends_with("reason=worker-panicked outcome=panicked; worker join completed"));
    assert!(!captured.text.contains("reason=deadline-expired"));
    assert!(captured
        .text
        .contains("sot-sock: server teardown failed; see worker panic and deadline records"));
}

#[test]
fn worker_panic_is_reported_reader() {
    panicked_worker("reaper::worker_panic_is_reported_reader", Role::Reader);
}

#[test]
fn worker_panic_is_reported_writer() {
    panicked_worker("reaper::worker_panic_is_reported_writer", Role::Writer);
}

/// A panicking worker with its peer held: only the unfinished peer is named by the expiry record.
fn panic_with_held_peer(test: &str, panicking: Role, held: Role) {
    if child_role(test) {
        let f = fixture(test, 1);
        f.server.set_teardown_deadline_for_test(SHORT);
        let (client, a) = connect(&f, test, "a");
        f.server.inject_worker_panic_for_test(a, panicking);
        let hold = f.server.hold_worker_exit_for_test(a, held);
        named!(test, "a.close", Some(a), f.server.close(a));
        let exit = format!("{}.exit", role_name(panicking));
        await_progress(
            &f.server,
            test,
            "peer.exit",
            "the panicking worker exited",
            Some(a),
            |r| r.conn == Some(a) && r.step == exit,
        );
        let want = format!("worker={}", role_name(held));
        await_progress(
            &f.server,
            test,
            "expiry.record",
            "expiry names the held peer alone",
            Some(a),
            |r| r.conn == Some(a) && r.step == "pending.expired" && r.result == want,
        );
        named!(test, "a.release", Some(a), hold.release());
        let closes = drain_closes(&f.server, test, &[a]);
        assert_eq!(
            closes[0].1,
            ClosedReason::Error("connection worker panicked".into())
        );
        named!(test, "client.drop", None, drop(client));
        named!(test, "server.drop", None, drop(f));
        return;
    }
    let captured = capture(test, ISOLATION_TIMEOUT, None);
    let panic_record = format!(
        "sot-sock: connection teardown failed conn=0 worker={} elapsed_ms=",
        role_name(panicking)
    );
    let expiry_record = format!(
        "sot-sock: connection teardown failed conn=0 worker={} elapsed_ms=",
        role_name(held)
    );
    assert!(captured.text.lines().any(|l| l.contains(&panic_record)
        && l.ends_with("reason=worker-panicked outcome=panicked; worker join completed")));
    assert!(captured.text.lines().any(|l| l.contains(&expiry_record)
        && l.ends_with("reason=deadline-expired; unfinished workers remain owned")));
    assert!(!captured.text.contains("worker=both"));
}

#[test]
fn panic_with_held_peer_reports_only_unfinished_expiry_reader_held() {
    panic_with_held_peer(
        "reaper::panic_with_held_peer_reports_only_unfinished_expiry_reader_held",
        Role::Writer,
        Role::Reader,
    );
}

#[test]
fn panic_with_held_peer_reports_only_unfinished_expiry_writer_held() {
    panic_with_held_peer(
        "reaper::panic_with_held_peer_reports_only_unfinished_expiry_writer_held",
        Role::Reader,
        Role::Writer,
    );
}

/// A connection is refused, closed without an `Accepted`, while another's teardown is pending: live plus pending
/// never exceeds the bound. Red before B2, which counts live connections only.
fn refused_while_pending(test: &str, c_label: &str, f: &Fixture) -> UnixStream {
    let mut client_c = io_named!(
        test,
        &format!("{c_label}.connect"),
        "connected",
        None,
        UnixStream::connect(&f.path)
    )
    .unwrap();
    let wait = WaitContext::new(test, "c.refused", "EOF without Accepted", None, TIMEOUT);
    let n = wait.read(Some(&f.server), &mut client_c, &mut [0u8; 8]);
    assert!(
        matches!(n, Ok(0)),
        "connection admitted while a pending teardown held the only slot: {n:?}"
    );
    client_c
}

#[test]
fn pending_teardown_stays_inside_connection_capacity() {
    let test = "reaper::pending_teardown_stays_inside_connection_capacity";
    isolated!(test);
    let f = fixture(test, 1);
    let (client_a, a) = connect(&f, test, "a");
    let hold = f.server.hold_worker_exit_for_test(a, Role::Reader);
    named!(test, "a.close", Some(a), f.server.close(a));
    reached(&f.server, test, "a.held", "A reader held", a, &hold);
    let client_c = refused_while_pending(test, "c", &f);
    named!(test, "a.release", Some(a), hold.release());
    drain_closes(&f.server, test, &[a]);
    let (client_d, _) = connect(&f, test, "d");
    named!(
        test,
        "clients.drop",
        None,
        drop((client_a, client_c, client_d))
    );
    named!(test, "server.drop", None, drop(f));
}

#[test]
fn blocked_close_stays_inside_connection_capacity() {
    let test = "reaper::blocked_close_stays_inside_connection_capacity";
    isolated!(test);
    let f = fixture(test, 1);
    let (mut client_a, a) = connect(&f, test, "a");
    for marker in 0..64u64 {
        f.server
            .send(a, vec![marker as u8; 16], Some(marker))
            .unwrap();
        read_exact_named(&f.server, test, "a.drain", a, &mut client_a, 16);
        let want = format!("ok marker={marker}");
        await_progress(
            &f.server,
            test,
            "a.enqueued",
            "Sent enqueued",
            Some(a),
            |r| r.conn == Some(a) && r.step == "sent.enqueue" && r.result == want,
        );
    }
    named!(test, "a.close", Some(a), f.server.close(a));
    await_progress(
        &f.server,
        test,
        "a.closed.full",
        "Closed(A) met a full channel",
        Some(a),
        |r| r.conn == Some(a) && r.step == "closed.enqueue" && r.result == "full",
    );
    let client_c = refused_while_pending(test, "c", &f);
    let wait = WaitContext::new(test, "drain", "64 markers and the close", None, TIMEOUT);
    let (mut markers, mut closed) = (0, false);
    while markers < 64 || !closed {
        match wait.next(&f.server) {
            LaneEvent::Sent(..) => markers += 1,
            LaneEvent::Closed(id, _) if id == a => closed = true,
            other => panic!("unexpected event {other:?}"),
        }
    }
    let (client_d, _) = connect(&f, test, "d");
    named!(
        test,
        "clients.drop",
        None,
        drop((client_a, client_c, client_d))
    );
    named!(test, "server.drop", None, drop(f));
}

/// Shutdown applies one short absolute deadline to a pair that is already pending, and the pair stays owned.
/// Preservation: the parent already bounds the join and returns false.
#[test]
fn shutdown_observes_pending_connections() {
    let test = "reaper::shutdown_observes_pending_connections";
    isolated!(test);
    let mut f = fixture(test, 2);
    let (client_a, a) = connect(&f, test, "a");
    let hold = f.server.hold_worker_exit_for_test(a, Role::Reader);
    named!(test, "a.close", Some(a), f.server.close(a));
    reached(&f.server, test, "a.held", "A reader held", a, &hold);
    named!(
        test,
        "listener.disconnect",
        None,
        f.server.disconnect_listener()
    );
    let budget = Duration::from_millis(300);
    let started = Instant::now();
    let ok = WaitContext::from_origin(
        test,
        "workers.join",
        "false while A is pending",
        None,
        started,
        started + budget,
    )
    .workers(&mut f.server);
    assert!(!ok, "shutdown reported success with A still pending");
    assert!(
        started.elapsed() < budget + Duration::from_secs(2),
        "shutdown took another per-pair budget: {:?}",
        started.elapsed()
    );
    named!(test, "a.release", Some(a), hold.release());
    await_progress(
        &f.server,
        test,
        "a.joined",
        "A joined after release",
        Some(a),
        at(a, "reader.join.end"),
    );
    named!(test, "client.drop", None, drop(client_a));
    named!(test, "server.drop", None, drop(f));
}

/// Phase one cancels registered pairs but leaves them for the reaper to claim; none is joined by the caller.
fn registered_pairs_through_reaper(test: &str, held: Role) {
    let mut f = fixture(test, 2);
    let (client_a, a) = connect(&f, test, "a");
    let (client_b, b) = connect(&f, test, "b");
    let hold = f.server.hold_worker_exit_for_test(a, held);
    named!(
        test,
        "listener.disconnect",
        None,
        f.server.disconnect_listener()
    );
    for conn in [a, b] {
        let route = await_progress(
            &f.server,
            test,
            "phase_one.route",
            "phase one routes the pair to the reaper",
            Some(conn),
            at(conn, "phase_one.route"),
        );
        assert_eq!(
            route.result, "reaper",
            "registered pair bypassed reaper (conn {conn}: {})",
            route.result
        );
    }
    let budget = Duration::from_millis(500);
    let started = Instant::now();
    let ok = WaitContext::from_origin(
        test,
        "workers.join",
        "false while A is held",
        None,
        started,
        started + budget,
    )
    .workers(&mut f.server);
    assert!(!ok, "shutdown reported success with A held");
    for conn in [a, b] {
        await_progress(
            &f.server,
            test,
            "claim",
            "the reaper claimed the pair",
            Some(conn),
            at(conn, "claim"),
        );
    }
    for step in ["reader.join.end", "writer.join.end"] {
        await_progress(
            &f.server,
            test,
            "b.joined",
            "B joined while A is held",
            Some(b),
            at(b, step),
        );
    }
    await_progress(
        &f.server,
        test,
        "a.expired",
        "A expiry",
        Some(a),
        at(a, "pending.expired"),
    );
    named!(test, "a.release", Some(a), hold.release());
    await_progress(
        &f.server,
        test,
        "a.done",
        "A retired",
        Some(a),
        at(a, "pending.done"),
    );
    named!(test, "clients.drop", None, drop((client_a, client_b)));
    named!(test, "server.drop", None, drop(f));
}

#[test]
fn shutdown_routes_registered_pairs_through_reaper_reader_held() {
    let test = "reaper::shutdown_routes_registered_pairs_through_reaper_reader_held";
    isolated!(test);
    registered_pairs_through_reaper(test, Role::Reader);
}

#[test]
fn shutdown_routes_registered_pairs_through_reaper_writer_held() {
    let test = "reaper::shutdown_routes_registered_pairs_through_reaper_writer_held";
    isolated!(test);
    registered_pairs_through_reaper(test, Role::Writer);
}
