//! Real failed and hung socket cases prove named records, one deadline and server progress.

use super::*;
use sot_log::test_isolated::{enter, test_command, wait_within, ISOLATION_TIMEOUT};
use std::process::Stdio;

fn panic_text(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = panic.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "non-string panic".into()
    }
}

fn assert_schema(text: &str, test: &str, step: &str, conn: ConnId) {
    for (field, required) in [
        ("test", format!("socket-test test={test}")),
        ("child", format!(" child={}", std::process::id())),
        ("step", format!(" step={step}")),
        ("expected", " expected=Closed(Eof)".into()),
        ("connection", format!(" conn={conn}")),
        ("elapsed", " elapsed_ms=".into()),
        (
            "caller",
            " caller=rust/log/tests/socket_unix/diagnostics.rs:".into(),
        ),
        ("begin", " result=begin".into()),
        ("timeout", " result=timeout".into()),
        (
            "progress snapshot",
            "transport-progress snapshot records=".into(),
        ),
        (
            "progress checkpoint",
            "transport-progress transport=socket".into(),
        ),
    ] {
        assert!(
            text.contains(&required),
            "wait diagnostic missing {field}: {text}"
        );
    }
    assert!(
        !text.contains("caller=/"),
        "caller must be repository-relative: {text}"
    );
}

fn missing_event(test: &'static str, unrelated: bool) {
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = io_named!(
        test,
        "server.bind",
        "bound",
        None,
        SocketServer::bind(&id, 2)
    )
    .unwrap();
    let mut client = io_named!(
        test,
        "a.connect",
        "connected",
        None,
        UnixStream::connect(&path)
    )
    .unwrap();
    let conn = expect_accepted(&server, test, "a.accept", TIMEOUT);
    eprintln!("fixture-proof test={test} connection=accepted bodies=1");
    let feeder = if unrelated {
        io_named!(
            test,
            "unrelated.write",
            "Bytes queued",
            Some(conn),
            client.write_all(b"unrelated")
        )
        .unwrap();
        Some(std::thread::spawn(move || {
            let feed = WaitContext::new(
                test,
                "unrelated.feed",
                "Bytes while wait expires",
                Some(conn),
                TIMEOUT,
            );
            for _ in 0..20 {
                feed.io(|| client.write_all(b"unrelated")).unwrap();
                feed.pause(Duration::from_millis(10));
            }
        }))
    } else {
        None
    };
    let wait = WaitContext::new(
        test,
        "absent.closed",
        "Closed(Eof)",
        Some(conn),
        Duration::from_millis(100),
    );
    let mut observed_unrelated = false;
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait.until(&server, |event| {
            if matches!(event, LaneEvent::Bytes(id, _) if *id == conn) {
                observed_unrelated = true;
            }
            matches!(event, LaneEvent::Closed(_, _))
        })
    }))
    .expect_err("an absent Closed must time out");
    if unrelated {
        assert!(
            observed_unrelated,
            "unrelated-event fixture was not observed"
        );
        eprintln!("fixture-proof test={test} unrelated=observed bodies=1");
    }
    let text = format!("{}{}", wait.records.borrow(), panic_text(panic));
    assert_schema(&text, test, "absent.closed", conn);
    assert!(
        wait.started.elapsed() < Duration::from_secs(1),
        "unrelated events restarted the deadline"
    );
    assert_eq!(wait.deadline, wait.started + Duration::from_millis(100));
    if let Some(feeder) = feeder {
        WaitContext::new(test, "feeder.join", "feeder ends", Some(conn), TIMEOUT)
            .join(|| feeder.join())
            .unwrap();
    }
    named!(test, "server.drop", None, drop(server));
}

#[test]
fn absent_event_reports_named_wait() {
    let test = "diagnostics::absent_event_reports_named_wait";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None,
        run_isolated(test)
    ) {
        return;
    }
    missing_event(test, false);
}

#[test]
fn unrelated_event_reports_named_wait() {
    let test = "diagnostics::unrelated_event_reports_named_wait";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None,
        run_isolated(test)
    ) {
        return;
    }
    missing_event(test, true);
}

#[test]
fn stalled_peer_eof_reports_read_timeout() {
    let test = "diagnostics::stalled_peer_eof_reports_read_timeout";
    let bound = TIMEOUT + Duration::from_secs(2);
    assert!(bound < ISOLATION_TIMEOUT);
    let (panic, text) =
        supervise_fixture(test, "diagnostics::stalled_peer_eof_role", "read", bound);
    assert!(
        panic.is_none(),
        "a.eof did not finish at its read bound: {text}"
    );
    for field in [
        "step=a.eof",
        "expected=zero bytes",
        "result=timeout",
        "fixture-proof",
        "bodies=1",
    ] {
        assert!(
            text.contains(field),
            "wait diagnostic missing {field}: {text}"
        );
    }
}

/// ISO supervises this fixture directly; files retain flushed evidence even at cutoff.
fn supervise_fixture(
    test: &str,
    role: &str,
    mode: &str,
    bound: Duration,
) -> (Option<String>, String) {
    let output = tempfile::tempfile().expect("fixture output");
    let (mut command, entry) = test_command(role);
    let mut child = io_named!(
        test,
        "child.spawn",
        "fixture running",
        None,
        command
            .env("SOT_TEST_SOCKET_ROLE", mode)
            .stdin(Stdio::null())
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output.try_clone().unwrap()))
            .spawn()
    )
    .unwrap();
    let pid = child.id();
    let wait = WaitContext::new(
        test,
        "child.wait",
        &format!("child {pid} ends"),
        None,
        bound,
    );
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait.run(|| wait_within(&mut child, bound))
    }));
    entry.assert_once(pid);
    assert!(
        io_named!(
            test,
            "child.confirm",
            "owned child reaped",
            None,
            child.try_wait()
        )
        .unwrap()
        .is_some(),
        "owned child termination unconfirmed"
    );
    use std::io::{Seek, SeekFrom};
    let mut output = output;
    output.seek(SeekFrom::Start(0)).unwrap();
    let mut text = String::new();
    output.read_to_string(&mut text).unwrap();
    if mode == "hang" {
        assert!(
            text.contains(&format!("test={role} child={pid}")),
            "child cutoff lost named socket wait: {text}"
        );
    }
    wait.emit(&text);
    eprintln!("body-proof test={role} child={pid} bodies=1 completed=true");
    match result {
        Ok(status) => {
            assert!(status.success(), "fixture child failed: {status}: {text}");
            (None, text)
        }
        Err(panic) => (Some(panic_text(panic)), text),
    }
}

#[test]
fn stalled_peer_eof_role() {
    let test = "diagnostics::stalled_peer_eof_role";
    if std::env::var("SOT_TEST_SOCKET_ROLE").as_deref() != Ok("read") {
        return;
    }
    enter(test);
    let (mut client, peer) = io_named!(
        test,
        "peer.pair",
        "connected pair",
        None,
        UnixStream::pair()
    )
    .unwrap();
    // The retained peer is open and sends no bytes: this is the actual a.eof read path.
    let wait = WaitContext::new(
        test,
        "peer.open",
        "peer kept open without bytes",
        None,
        TIMEOUT,
    );
    wait.record("ok");
    eprintln!("fixture-proof test={test} peer=open bytes=0 bodies=1");
    std::io::stderr().flush().unwrap();
    let started = Instant::now();
    let result = close::read_a_eof(test, None, &mut client, &mut [0u8; 16]);
    let error = result.expect_err("stalled peer must reach its local read timeout");
    assert!(
        matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        "a.eof expected read timeout, got {error}"
    );
    assert!(started.elapsed() >= TIMEOUT, "read bound fired early");
    assert!(
        started.elapsed() < TIMEOUT + Duration::from_secs(2),
        "a.eof did not finish at its read bound"
    );
    named!(test, "peer.drop", "peer dropped", None, drop(peer));
}

#[test]
fn progress_survives_connection_removal() {
    let test = "diagnostics::progress_survives_connection_removal";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None,
        run_isolated(test)
    ) {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = io_named!(
        test,
        "server.bind",
        "bound",
        None,
        SocketServer::bind(&id, 1)
    )
    .unwrap();
    let client = io_named!(
        test,
        "a.connect",
        "connected",
        None,
        UnixStream::connect(&path)
    )
    .unwrap();
    let conn = expect_accepted(&server, test, "a.accept", TIMEOUT);
    named!(
        test,
        "a.close",
        "close requested",
        Some(conn),
        server.close(conn)
    );
    expect_closed(&server, test, "a.closed", conn, TIMEOUT);
    let snapshot = server.progress_for_test();
    assert!(!snapshot.unavailable, "snapshot unavailable after Closed");
    for step in [
        "registered",
        "accepted.enqueue",
        "gate.open",
        "reader.gate.wait",
        "writer.gate.wait",
        "reader.enter",
        "writer.enter",
        "teardown.enqueue",
        "teardown.dequeue",
        "shutdown.enter",
        "shutdown.result",
        "reader.join.begin",
        "reader.join.end",
        "writer.join.begin",
        "writer.join.end",
        "closed.enqueue",
    ] {
        assert!(
            snapshot
                .records
                .iter()
                .any(|r| r.conn == Some(conn) && r.step == step),
            "missing progress checkpoint {step}: {snapshot}"
        );
    }
    named!(
        test,
        "client.drop",
        "client dropped",
        Some(conn),
        drop(client)
    );
    for _ in 0..24 {
        let client = io_named!(
            test,
            "churn.connect",
            "connected",
            None,
            UnixStream::connect(&path)
        )
        .unwrap();
        let conn = expect_accepted(&server, test, "churn.accept", TIMEOUT);
        named!(test, "churn.close", Some(conn), server.close(conn));
        expect_closed(&server, test, "churn.closed", conn, TIMEOUT);
        named!(test, "churn.drop", Some(conn), drop(client));
    }
    let snapshot = server.progress_for_test();
    assert_eq!(snapshot.records.len(), 256, "real churn must fill the ring");
    assert!(
        snapshot.overwritten > 0,
        "real churn must overwrite old checkpoints"
    );
    named!(test, "server.drop", None, drop(server));
}

#[test]
fn child_cutoff_preserves_last_begin() {
    let test = "diagnostics::child_cutoff_preserves_last_begin";
    if std::env::var("SOT_TEST_SOCKET_ROLE").as_deref() == Ok("hang") {
        enter(test);
        let _rt = isolated_runtime_dir();
        let id = fresh_voyage_id();
        let server = io_named!(
            test,
            "server.bind",
            "bound",
            None,
            SocketServer::bind(&id, 1)
        )
        .unwrap();
        eprintln!("fixture-proof test={test} server=bound bodies=1");
        let wait = WaitContext::new(
            test,
            "fixture.hang",
            "closure deliberately remains blocked",
            None,
            TIMEOUT,
        );
        wait.run(|| loop {
            std::thread::park();
        });
        named!(test, "server.drop", None, drop(server));
        return;
    }
    let (cutoff, text) = supervise_fixture(test, test, "hang", ISOLATION_TIMEOUT);
    assert!(
        text.contains("fixture-proof") && text.contains("server=bound"),
        "hang fixture prerequisite not observed: {text}"
    );
    assert!(
        cutoff.as_deref().is_some_and(
            |s| s.contains("did not complete within 30s") && !s.contains("unconfirmed")
        ),
        "expected confirmed child cutoff: {cutoff:?}"
    );
    assert!(
        text.contains(&format!("socket-test test={test} child="))
            && text.contains("step=fixture.hang")
            && text.contains("conn=pending")
            && text.contains("result=begin"),
        "child cutoff lost named socket wait: {text}"
    );
    eprintln!("cutoff-proof test={test} bound=30s last_begin=fixture.hang cleanup=confirmed");
}

#[test]
fn outcomes_distinguish_disconnect_success_and_io_error() {
    let test = "diagnostics::outcomes_distinguish_disconnect_success_and_io_error";
    if !named!(
        test,
        "child.wait",
        "isolated body and bounded completion",
        None,
        run_isolated(test)
    ) {
        return;
    }
    let (sent, received) = std::sync::mpsc::channel();
    drop(sent);
    let wait = WaitContext::new(test, "channel.disconnect", "disconnected", None, TIMEOUT);
    assert!(matches!(
        wait.receive_from(&received, TIMEOUT, None),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
    ));
    assert!(
        wait.records.borrow().contains("result=error")
            && wait.records.borrow().contains("disconnected channel")
    );
    let (mut a, mut b) = io_named!(test, "pair", "connected", None, UnixStream::pair()).unwrap();
    let work = WaitContext::new(test, "socket.work", "one byte", None, TIMEOUT);
    work.io(|| a.write_all(b"x")).unwrap();
    let mut byte = [0];
    assert_eq!(work.io(|| b.read(&mut byte)).unwrap(), 1);
    assert_eq!(byte, *b"x");
    assert!(work.records.borrow().contains("result=ok"));
    let invalid = WaitContext::new(test, "socket.invalid", "InvalidInput", None, TIMEOUT);
    assert_eq!(
        invalid
            .io(|| a.set_read_timeout(Some(Duration::ZERO)))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert!(
        invalid.records.borrow().contains("result=error")
            && !invalid.records.borrow().contains("result=timeout")
    );
}

#[test]
fn empty_wait_labels_are_rejected() {
    for (test, step, expected) in [
        ("", "step", "result"),
        ("diagnostics::labels", "", "result"),
        ("diagnostics::labels", "step", ""),
    ] {
        assert!(
            std::panic::catch_unwind(|| WaitContext::new(test, step, expected, None, TIMEOUT))
                .is_err()
        );
    }
}
