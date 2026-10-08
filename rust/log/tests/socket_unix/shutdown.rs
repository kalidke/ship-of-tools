//! Shutdown: the registration cutoff and the result every `shutdown(2)` caller records.

use super::reaper::{isolated, *};
use super::*;
use sot_log::lane::client::Client;
use sot_log::lane::test_progress::{Checkpoint, Pause};

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
    assert_eq!(
        count(&f.server, |r| r.step == "registered"),
        0,
        "late insertion"
    );
    assert_eq!(
        count(&f.server, |r| r.conn == Some(conn) && r.step == "gate.open"),
        0,
        "late gate-open"
    );
    assert_eq!(
        count(&f.server, |r| r.step == "accepted.enqueue"),
        0,
        "late Accepted"
    );
    assert!(matches!(
        f.server.events().try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    named!(test, "client.drop", None, drop(client));
    named!(test, "server.drop", None, drop(f));
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
    let snapshot = f.server.progress_for_test();
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
        let snapshot = client.progress_for_test();
        let record = shutdown_result(&snapshot, None, "client.rs::cancel");
        eprintln!("fixture-proof test={test} client={label} {record}");
    }
    named!(test, "clients.drop", None, drop(clients));
    named!(test, "server.drop", None, drop(f));
}
