//! Shutdown: the registration cutoff and the result every `shutdown(2)` caller records.

use super::reaper::{isolated, *};
use super::*;
use sot_log::lane::client::Client;
use sot_log::lane::test_progress::{assert_absent, available, Checkpoint, Pause, Role};

/// Shutdown beats a registration the acceptor had already started: the pair is aborted and joined by the acceptor,
/// and no `Accepted`, gate-open or registration follows. Preservation on Unix, whose cutoff already shares the lock.
#[test]
fn registration_after_shutdown_is_rejected() {
    let test = "shutdown::registration_after_shutdown_is_rejected";
    isolated!(test);
    let mut f = fixture(test, 2);
    let barrier: Pause = f.server.pause_registration_for_test();
    let client = io_named!(
        test,
        "a.connect",
        "connected",
        None,
        UnixStream::connect(&f.path)
    )
    .unwrap();
    assert!(
        barrier.wait_reached(RECORD),
        "the acceptor never reached the registration barrier"
    );
    named!(
        test,
        "listener.disconnect",
        None,
        f.server.disconnect_listener()
    );
    named!(test, "acceptor.release", None, barrier.release());
    let started = Instant::now();
    let ok = WaitContext::from_origin(
        test,
        "workers.join",
        "workers complete",
        None,
        started,
        started + RECORD,
    )
    .workers(&mut f.server);
    assert!(ok, "registration after shutdown left a worker behind");
    let rejected = await_progress(
        &f.server,
        test,
        "registration.cutoff",
        "late registration rejected",
        None,
        |r| r.step == "registration.cutoff" && r.result == "rejected",
    );
    let conn = rejected.conn.expect("the assigned id");
    for step in ["reader.join.end", "writer.join.end"] {
        await_progress(
            &f.server,
            test,
            "unwound",
            "never-registered pair joined",
            Some(conn),
            at(conn, step),
        );
    }
    await_progress(
        &f.server,
        test,
        "shutdown.result",
        "the unwind shutdown result",
        Some(conn),
        at(conn, "shutdown.result"),
    );
    assert_absent(
        || f.server.progress_for_test(),
        "a late insertion",
        |r| r.step == "registered",
    );
    assert_absent(
        || f.server.progress_for_test(),
        "a late gate-open",
        |r| r.conn == Some(conn) && r.step == "gate.open",
    );
    assert_absent(
        || f.server.progress_for_test(),
        "a late Accepted",
        |r| r.step == "accepted.enqueue",
    );
    assert!(matches!(
        f.server.events().try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    named!(test, "client.drop", None, drop(client));
    named!(test, "server.drop", None, drop(f));
}

/// A pair that expired its normal-close budget earlier and is still unfinished at the shutdown deadline fails the
/// teardown: the reaper cannot end while it owns the pair.
fn expired_then_unfinished_at_shutdown(test: &str, role: Role) {
    let mut f = fixture(test, 1);
    f.server.set_teardown_deadline_for_test(SHORT);
    let (client, a) = connect(&f, test, "a");
    let hold = f.server.hold_worker_exit_for_test(a, role);
    named!(test, "a.close", Some(a), f.server.close(a));
    let want = format!("worker={}", role_name(role));
    await_progress(
        &f.server,
        test,
        "expiry.record",
        "expiry reported before shutdown",
        Some(a),
        |r| r.conn == Some(a) && r.step == "pending.expired" && r.result == want,
    );
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
        "false while A is still unfinished",
        None,
        started,
        started + budget,
    )
    .workers(&mut f.server);
    assert!(!ok, "shutdown reported success with A still unfinished");
    named!(test, "a.release", Some(a), hold.release());
    await_progress(
        &f.server,
        test,
        "a.done",
        "A retired after release",
        Some(a),
        at(a, "pending.done"),
    );
    named!(test, "client.drop", None, drop(client));
    named!(test, "server.drop", None, drop(f));
}

#[test]
fn expired_pair_unfinished_at_shutdown_fails_teardown_reader_held() {
    let test = "shutdown::expired_pair_unfinished_at_shutdown_fails_teardown_reader_held";
    isolated!(test);
    expired_then_unfinished_at_shutdown(test, Role::Reader);
}

#[test]
fn expired_pair_unfinished_at_shutdown_fails_teardown_writer_held() {
    let test = "shutdown::expired_pair_unfinished_at_shutdown_fails_teardown_writer_held";
    isolated!(test);
    expired_then_unfinished_at_shutdown(test, Role::Writer);
}

/// A reaper pass at or after the shutdown deadline that finds a pair's worker unfinished fails the teardown even if the
/// worker finishes just after that pass and the reaper has exited before `join_workers` samples it.
fn deadline_pass_latches_without_the_reaper_sample(test: &str, role: Role) {
    let mut f = fixture(test, 1);
    let (client, a) = connect(&f, test, "a");
    let hold = f.server.hold_worker_exit_for_test(a, role);
    let join = f.server.pause_join_for_test();
    let view = f.server.progress_view_for_test();
    named!(test, "a.close", Some(a), f.server.close(a));
    reached(&f.server, test, "a.held", "the worker held", a, &hold);
    named!(
        test,
        "listener.disconnect",
        None,
        f.server.disconnect_listener()
    );
    // A step the recorder reports, read through the view while another thread holds `&mut` of the server.
    let seen = |step: &str, conn: Option<ConnId>| {
        let deadline = Instant::now() + RECORD;
        loop {
            let snapshot = available(|| view.snapshot());
            if snapshot
                .records
                .iter()
                .any(|r| r.step == step && r.conn == conn)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "socket wait {step}: not observed within {RECORD:?}\n{snapshot}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let ok = std::thread::scope(|scope| {
        let joining = scope.spawn(|| {
            let started = Instant::now();
            WaitContext::from_origin(
                test,
                "workers.join",
                "the sample sees a finished reaper",
                None,
                started,
                started + Duration::from_millis(300),
            )
            .workers(&mut f.server)
        });
        assert!(
            join.wait_reached(RECORD),
            "join_workers never reached its reaper sample"
        );
        seen("pending.deadline", Some(a));
        named!(test, "a.release", Some(a), hold.release());
        seen("reaper.exit", None);
        named!(test, "join.release", None, join.release());
        joining.join().unwrap()
    });
    assert!(
        !ok,
        "a pair unfinished at the shutdown deadline pass left a clean teardown"
    );
    named!(test, "client.drop", None, drop(client));
    named!(test, "server.drop", None, drop(f));
}

#[test]
fn deadline_pass_fails_teardown_though_the_reaper_exits_first_reader_held() {
    let test = "shutdown::deadline_pass_fails_teardown_though_the_reaper_exits_first_reader_held";
    isolated!(test);
    deadline_pass_latches_without_the_reaper_sample(test, Role::Reader);
}

#[test]
fn deadline_pass_fails_teardown_though_the_reaper_exits_first_writer_held() {
    let test = "shutdown::deadline_pass_fails_teardown_though_the_reaper_exits_first_writer_held";
    isolated!(test);
    deadline_pass_latches_without_the_reaper_sample(test, Role::Writer);
}

/// A server thread that panicked fails the teardown for good: a panicked reaper leaves its pending pairs unjoined, and
/// the shutdown that joined it used to see only a finished thread.
fn server_thread_panic(test: &str, thread: &str) {
    let mut f = fixture(test, 2);
    let client = if thread == "reaper" {
        let (client, a) = connect(&f, test, "a");
        f.server.inject_reaper_panic_for_test();
        named!(test, "a.close", Some(a), f.server.close(a));
        client
    } else {
        f.server.inject_acceptor_panic_for_test();
        io_named!(
            test,
            "a.connect",
            "connected",
            None,
            UnixStream::connect(&f.path)
        )
        .unwrap()
    };
    await_progress(
        &f.server,
        test,
        "panic.injected",
        "the thread panicked at its injected site",
        None,
        |r| r.result == "panic injected",
    );
    named!(
        test,
        "listener.disconnect",
        None,
        f.server.disconnect_listener()
    );
    let ok = WaitContext::new(
        test,
        "workers.join",
        "teardown fails for a panicked server thread",
        None,
        RECORD,
    )
    .workers(&mut f.server);
    assert!(!ok, "a panicked {thread} thread reported a clean teardown");
    named!(test, "client.drop", None, drop(client));
    named!(test, "server.drop", None, drop(f));
}

#[test]
fn a_panicked_reaper_fails_the_teardown() {
    let test = "shutdown::a_panicked_reaper_fails_the_teardown";
    isolated!(test);
    server_thread_panic(test, "reaper");
}

#[test]
fn a_panicked_acceptor_fails_the_teardown() {
    let test = "shutdown::a_panicked_acceptor_fails_the_teardown";
    isolated!(test);
    server_thread_panic(test, "acceptor");
}

/// The shutdown record of one caller: its result agrees with its errno, and it is attributed to that caller.
fn shutdown_result<'a>(
    snapshot: &'a sot_log::lane::test_progress::Snapshot,
    conn: Option<ConnId>,
    caller: &str,
) -> &'a Checkpoint {
    let record = snapshot
        .records
        .iter()
        .find(|r| r.conn == conn && r.step == "shutdown.result" && r.caller.ends_with(caller))
        .unwrap_or_else(|| panic!("no shutdown.result for {caller}: {snapshot}"));
    assert_eq!(
        record.errno.is_none(),
        record.result.starts_with("rc=0"),
        "errno disagrees with the result: {record}"
    );
    record
}

#[test]
fn shutdown_records_reaper_result() {
    let test = "shutdown::shutdown_records_reaper_result";
    isolated!(test);
    let f = fixture(test, 1);
    let (client, a) = connect(&f, test, "a");
    named!(test, "a.close", Some(a), f.server.close(a));
    assert_eq!(
        expect_closed(&f.server, test, "a.closed", a, TIMEOUT),
        ClosedReason::Closed
    );
    let snapshot = available(|| f.server.progress_for_test());
    shutdown_result(&snapshot, Some(a), "conn.rs::reaper_loop");
    named!(test, "client.drop", None, drop(client));
    named!(test, "server.drop", None, drop(f));
}

#[test]
fn shutdown_records_phase_one_result() {
    let test = "shutdown::shutdown_records_phase_one_result";
    isolated!(test);
    let mut f = fixture(test, 1);
    let (client, a) = connect(&f, test, "a");
    named!(
        test,
        "listener.disconnect",
        None,
        f.server.disconnect_listener()
    );
    let found = await_progress(
        &f.server,
        test,
        "a.shutdown",
        "phase-one shutdown result",
        Some(a),
        |r| {
            r.conn == Some(a)
                && r.step == "shutdown.result"
                && r.caller.ends_with("server.rs::disconnect_listener")
        },
    );
    assert_eq!(found.errno.is_none(), found.result.starts_with("rc=0"));
    assert!(
        WaitContext::new(test, "workers.join", "workers complete", None, RECORD)
            .workers(&mut f.server)
    );
    named!(test, "client.drop", None, drop(client));
    named!(test, "server.drop", None, drop(f));
}

#[test]
fn shutdown_records_unregistered_unwind_result() {
    let test = "shutdown::shutdown_records_unregistered_unwind_result";
    isolated!(test);
    let mut f = fixture(test, 1);
    let barrier = f.server.pause_registration_for_test();
    let client = io_named!(
        test,
        "a.connect",
        "connected",
        None,
        UnixStream::connect(&f.path)
    )
    .unwrap();
    assert!(barrier.wait_reached(RECORD));
    named!(
        test,
        "listener.disconnect",
        None,
        f.server.disconnect_listener()
    );
    barrier.release();
    let found = await_progress(
        &f.server,
        test,
        "unwind.shutdown",
        "unwind shutdown result",
        None,
        |r| r.step == "shutdown.result" && r.caller.ends_with("accept.rs::handle_new_connection"),
    );
    assert!(found.conn.is_some(), "the assigned, unpublished id");
    assert_eq!(found.errno.is_none(), found.result.starts_with("rc=0"));
    assert!(
        WaitContext::new(test, "workers.join", "workers complete", None, RECORD)
            .workers(&mut f.server)
    );
    named!(test, "client.drop", None, drop(client));
    named!(test, "server.drop", None, drop(f));
}

#[test]
fn shutdown_records_client_cancel_result() {
    let test = "shutdown::shutdown_records_client_cancel_result";
    isolated!(test);
    let f = fixture(test, 4);
    let mut clients = Vec::new();
    for label in ["direct", "trait", "write"] {
        let (stream, conn) = connect(&f, test, label);
        clients.push((SocketClient::from_stream_for_test(stream, 0), conn));
    }
    named!(test, "client.cancel", None, clients[0].0.cancel());
    named!(
        test,
        "client.trait.cancel",
        None,
        Client::cancel(&clients[1].0)
    );
    // A terminal write failure latches through the same method.
    let write_conn = clients[2].1;
    named!(
        test,
        "server.close",
        Some(write_conn),
        f.server.close(write_conn)
    );
    let wait = WaitContext::new(
        test,
        "client.write.fails",
        "terminal write error",
        None,
        TIMEOUT,
    );
    let failed = loop {
        wait.check(Some(&f.server));
        match clients[2].0.write_all(&[7u8; 4096]) {
            Ok(()) => wait.pause(Duration::from_millis(5)),
            Err(error) => break error,
        }
    };
    wait.complete("ok", None, Some(&f.server));
    eprintln!("fixture-proof test={test} write-failure={failed}");
    for (label, (client, _)) in ["direct", "trait", "write-failure"].iter().zip(&clients) {
        let snapshot = available(|| client.progress_for_test());
        let record = shutdown_result(&snapshot, None, "client.rs::cancel");
        eprintln!("fixture-proof test={test} client={label} {record}");
    }
    named!(test, "clients.drop", None, drop(clients));
    named!(test, "server.drop", None, drop(f));
}
