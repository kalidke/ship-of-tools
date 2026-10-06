//! Socket diagnostic proofs observe flushed output of exact-body ISO children.

use super::*;
use sot_log::test_isolated::{enter, test_command, ChildWaitKind, ISOLATION_TIMEOUT};
use std::process::Stdio;

struct Captured {
    text: String,
    pid: u32,
    expired: bool,
    begin_observed: bool,
}

fn child_role(test: &str) -> bool {
    if std::env::var("SOT_TEST_SOCKET_ROLE").as_deref() == Ok(test) {
        enter(test);
        true
    } else {
        false
    }
}

/// ISO owns the child wait and cleanup; the capture file introduces no drain deadline.
fn capture(test: &str, bound: Duration, release_after_begin: Option<&str>) -> Captured {
    let output = tempfile::NamedTempFile::new().expect("capture file");
    let (mut command, entry) = test_command(test);
    let file = output.reopen().unwrap();
    let wait = WaitContext::new(
        test,
        "child.wait",
        "fixture ends with confirmed cleanup",
        None,
        bound,
    );
    let mut child = command
        .env("SOT_TEST_SOCKET_ROLE", test)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(file.try_clone().unwrap()))
        .stderr(Stdio::from(file.try_clone().unwrap()))
        .spawn()
        .expect("spawn the fixture child");
    let pid = child.id();
    let mut begin_observed = false;
    if let Some(step) = release_after_begin {
        let observe = WaitContext::from_origin(
            test,
            "begin.observe",
            "complete emitted begin before release",
            None,
            wait.started,
            wait.deadline,
        );
        while Instant::now() < observe.deadline {
            let text = std::fs::read_to_string(output.path()).unwrap();
            begin_observed = text.lines().any(|line| {
                line.contains(&format!("socket-test test={test} child={pid} "))
                    && line.contains(&format!(" step={step} "))
                    && line.ends_with("result=begin")
            });
            if begin_observed {
                break;
            }
            observe.pause(Duration::from_millis(5));
        }
        observe.complete(if begin_observed { "ok" } else { "timeout" }, None, None);
        if begin_observed {
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(b"r")
                .expect("release child");
            eprintln!("fixture-proof test={test} child={pid} begin={step} observed=before-release");
        }
    }
    let result = wait.child(&mut child);
    let confirmed = child
        .try_wait()
        .expect("confirm fixture termination")
        .is_some();
    entry.assert_once(pid);
    let text = std::fs::read_to_string(output.path()).unwrap();
    let expired = result
        .as_ref()
        .is_err_and(|error| matches!(error.kind, ChildWaitKind::Expired));
    assert!(
        confirmed
            && result
                .as_ref()
                .err()
                .is_none_or(|error| error.termination_confirmed),
        "owned child termination unconfirmed: {result:?}"
    );
    eprintln!("{text}");
    eprintln!(
        "body-proof test={test} child={pid} bodies=1 completed={} cleanup=confirmed",
        result.is_ok()
    );
    match result {
        Ok(status) => assert!(status.success(), "fixture child failed: {status}: {text}"),
        Err(error) if !expired => panic!("fixture supervision failed: {error}"),
        Err(_) => {}
    }
    if release_after_begin.is_some() {
        assert!(
            begin_observed,
            "child begin was not visible before release: {text}"
        );
    }
    Captured {
        text,
        pid,
        expired,
        begin_observed,
    }
}

fn record<'a>(capture: &'a Captured, test: &str, step: &str, outcome: &str) -> &'a str {
    let line = capture
        .text
        .lines()
        .find(|line| {
            line.contains(&format!("socket-test test={test} child={} ", capture.pid))
                && line.contains(&format!(" step={step} "))
                && line.ends_with(&format!("result={outcome}"))
        })
        .unwrap_or_else(|| {
            panic!(
                "wait diagnostic missing emitted {step}/{outcome}: {}",
                capture.text
            )
        });
    let line = &line[line.find("socket-test ").expect("complete socket record")..];
    for key in [
        "test",
        "child",
        "step",
        "expected",
        "conn",
        "elapsed_ms",
        "caller",
        "result",
    ] {
        let value = line
            .split_whitespace()
            .find_map(|part| part.strip_prefix(&format!("{key}=")))
            .unwrap_or_else(|| panic!("wait diagnostic missing emitted {key}: {line}"));
        assert!(
            !value.is_empty(),
            "wait diagnostic missing emitted {key}: {line}"
        );
        if key == "elapsed_ms" {
            value.parse::<u128>().expect("numeric elapsed_ms");
        }
        if key == "conn" && value != "pending" {
            value.parse::<u64>().expect("numeric connection id");
        }
        if key == "caller" {
            let (file, number) = value.rsplit_once(':').expect("caller and line");
            assert!(
                file.starts_with("rust/log/") && !file.contains(".."),
                "repository-relative caller required"
            );
            number.parse::<u32>().expect("numeric caller line");
        }
    }
    line
}

fn history(capture: &Captured) {
    assert!(
        capture
            .text
            .contains("transport-progress snapshot records=")
            && capture.text.contains("transport-progress transport=socket"),
        "wait diagnostic missing emitted progress snapshot: {}",
        capture.text
    );
}

fn server(test: &str) -> (RuntimeDirGuard, SocketServer, UnixStream, ConnId, String) {
    let root = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = UnixStream::connect(voyage_socket_path(&id).unwrap()).unwrap();
    let conn = expect_accepted(&server, test, "a.accept", TIMEOUT);
    WaitContext::new(
        test,
        "snapshot.prerequisite",
        "admitted connection history",
        Some(conn),
        TIMEOUT,
    )
    .available_snapshot(&server);
    eprintln!("fixture-proof test={test} conn={conn} accepted=true bodies=1");
    (root, server, client, conn, id)
}

fn missing_event(test: &str, unrelated: bool) {
    let (_root, server, mut client, conn, _id) = server(test);
    if unrelated {
        WaitContext::new(test, "unrelated.write", "Bytes queued", Some(conn), TIMEOUT)
            .write_all(Some(&server), &mut client, b"unrelated")
            .unwrap();
    }
    let wait = WaitContext::new(
        test,
        "absent.closed",
        "Closed(Eof)",
        Some(conn),
        Duration::from_millis(100),
    );
    let mut observed = false;
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait.until(&server, |event| {
            if matches!(event, LaneEvent::Bytes(id, _) if *id == conn) {
                observed = true;
            }
            matches!(event, LaneEvent::Closed(_, _))
        })
    }));
    assert!(failure.is_err(), "absent event must expire");
    assert!(
        !unrelated || observed,
        "unrelated-event fixture was not observed"
    );
    assert!(
        wait.started.elapsed() < Duration::from_secs(1),
        "unrelated events restarted the deadline"
    );
    if unrelated {
        eprintln!("fixture-proof test={test} unrelated=observed bodies=1");
    }
    named!(test, "server.drop", None, drop(server));
}

#[test]
fn absent_event_reports_named_wait() {
    let test = "diagnostics::absent_event_reports_named_wait";
    if child_role(test) {
        return missing_event(test, false);
    }
    let captured = capture(test, ISOLATION_TIMEOUT, None);
    record(&captured, test, "absent.closed", "begin");
    record(&captured, test, "absent.closed", "timeout");
    history(&captured);
}

#[test]
fn unrelated_event_reports_named_wait() {
    let test = "diagnostics::unrelated_event_reports_named_wait";
    if child_role(test) {
        return missing_event(test, true);
    }
    let captured = capture(test, ISOLATION_TIMEOUT, None);
    record(&captured, test, "absent.closed", "begin");
    record(&captured, test, "absent.closed", "timeout");
    assert!(
        captured.text.contains("unrelated=observed"),
        "unrelated-event fixture was not observed"
    );
    history(&captured);
}

#[test]
fn begin_is_visible_before_blocking() {
    let test = "diagnostics::begin_is_visible_before_blocking";
    if child_role(test) {
        let wait = WaitContext::new(test, "fixture.release", "release byte", None, TIMEOUT);
        let mut byte = [0];
        std::io::stdin().read_exact(&mut byte).unwrap();
        wait.complete("ok", None, None);
        return;
    }
    let captured = capture(test, ISOLATION_TIMEOUT, Some("fixture.release"));
    assert!(captured.begin_observed);
    record(&captured, test, "fixture.release", "begin");
    record(&captured, test, "fixture.release", "ok");
}

#[test]
fn stalled_peer_eof_reports_read_timeout() {
    let test = "diagnostics::stalled_peer_eof_reports_read_timeout";
    if child_role(test) {
        let (mut client, _peer) = UnixStream::pair().unwrap();
        eprintln!("fixture-proof test={test} peer=open bytes=0 bodies=1");
        let started = Instant::now();
        let error = close::read_a_eof(test, None, None, &mut client, &mut [0; 16]).unwrap_err();
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        assert!(
            started.elapsed() >= TIMEOUT && started.elapsed() < TIMEOUT + Duration::from_secs(2),
            "a.eof did not finish at its read bound"
        );
        return;
    }
    let captured = capture(test, TIMEOUT + Duration::from_secs(2), None);
    assert!(!captured.expired, "a.eof did not finish at its read bound");
    record(&captured, test, "a.eof", "begin");
    record(&captured, test, "a.eof", "timeout");
    assert!(captured.text.contains("reason=no-server"));
}

#[test]
fn io_timeout_with_server_reports_snapshot() {
    let test = "diagnostics::io_timeout_with_server_reports_snapshot";
    if child_role(test) {
        let (_root, server, mut client, conn, _id) = server(test);
        // The real server keeps the connected peer open and sends nothing.
        close::read_a_eof(test, Some(&server), Some(conn), &mut client, &mut [0; 16]).unwrap_err();
        return;
    }
    let captured = capture(test, TIMEOUT + Duration::from_secs(2), None);
    assert!(!captured.expired, "a.eof did not finish at its read bound");
    record(&captured, test, "a.eof", "timeout");
    assert!(captured.text.contains("socket-error step=a.eof error="));
    assert!(
        (captured
            .text
            .contains("transport-progress snapshot records=")
            || (captured
                .text
                .contains("transport-progress snapshot unavailable skipped=")
                && (captured.text.contains("reason=busy")
                    || captured.text.contains("reason=poisoned")))),
        "I/O timeout missing emitted progress snapshot"
    );
}

#[test]
fn outcomes_distinguish_disconnect_success_and_io_error() {
    let test = "diagnostics::outcomes_distinguish_disconnect_success_and_io_error";
    if child_role(test) {
        let (sent, receiver) = std::sync::mpsc::channel();
        drop(sent);
        let wait = WaitContext::new(test, "channel.disconnect", "disconnected", None, TIMEOUT);
        assert!(matches!(
            wait.receive_from(&receiver, TIMEOUT, None),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        ));
        let (mut a, mut b) = UnixStream::pair().unwrap();
        WaitContext::new(test, "socket.write", "one byte", None, TIMEOUT)
            .write_all(None, &mut a, b"x")
            .unwrap();
        let mut byte = [0];
        assert_eq!(
            WaitContext::new(test, "socket.read", "one byte", None, TIMEOUT)
                .read(None, &mut b, &mut byte)
                .unwrap(),
            1
        );
        assert_eq!(byte, *b"x");
        let invalid = WaitContext::new(test, "socket.invalid", "InvalidInput", None, TIMEOUT);
        assert_eq!(
            invalid
                .io(|| a.set_read_timeout(Some(Duration::ZERO)))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        return;
    }
    let captured = capture(test, ISOLATION_TIMEOUT, None);
    for (step, outcome) in [
        ("channel.disconnect", "error"),
        ("socket.write", "ok"),
        ("socket.read", "ok"),
        ("socket.invalid", "error"),
    ] {
        record(&captured, test, step, "begin");
        record(&captured, test, step, outcome);
    }
    assert!(captured.text.contains("disconnected channel"));
}

#[test]
fn child_cutoff_preserves_last_begin() {
    let test = "diagnostics::child_cutoff_preserves_last_begin";
    if child_role(test) {
        let _root = isolated_runtime_dir();
        let _server = SocketServer::bind(&fresh_voyage_id(), 1).unwrap();
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
        return;
    }
    let captured = capture(test, ISOLATION_TIMEOUT, None);
    assert!(captured.expired, "expected confirmed child cutoff");
    assert!(
        captured.text.contains("server=bound"),
        "hang fixture prerequisite not observed"
    );
    record(&captured, test, "fixture.hang", "begin");
    eprintln!("cutoff-proof test={test} bound=30s last_begin=fixture.hang cleanup=confirmed");
}

fn available_during_fixture_hold(
    wait: &WaitContext,
    server: &SocketServer,
) -> sot_log::lane::test_progress::Snapshot {
    let mut held = Some(server.hold_progress_for_test());
    let mut observed = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait.available_snapshot_observing(server, || {
            observed = true;
            drop(held.take());
        })
    }));
    drop(held); // Every failure releases the fixture before assertions or ordinary server cleanup.
    match result {
        Ok(snapshot) => {
            assert!(observed, "snapshot poll did not observe the held recorder");
            snapshot
        }
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

fn retention(test: &str) {
    let (_root, server, client, conn, id) = server(test);
    let origin = Instant::now();
    let prerequisite = WaitContext::from_origin(
        test,
        "snapshot.prerequisite",
        "admitted witness",
        Some(conn),
        origin,
        origin + TIMEOUT,
    )
    .available_snapshot(&server);
    let witness = prerequisite
        .records
        .iter()
        .find(|r| r.conn == Some(conn))
        .expect("connection checkpoint prerequisite")
        .clone();
    eprintln!(
        "fixture-proof test={test} witness={} admitted=true bodies=1",
        witness.step
    );
    server.close(conn);
    expect_closed(&server, test, "a.closed", conn, TIMEOUT);
    assert!(
        matches!(
            server.send(conn, b"x".to_vec(), None),
            Err(TransportError::UnknownConnection(_))
        ),
        "connection was not removed"
    );
    let wait = WaitContext::from_origin(
        test,
        "snapshot.available.after_close",
        "retained admitted witness",
        Some(conn),
        origin,
        origin + TIMEOUT,
    );
    let after = available_during_fixture_hold(&wait, &server);
    assert!(
        after.records.iter().any(|r| r.conn == witness.conn
            && r.step == witness.step
            && r.elapsed_ms == witness.elapsed_ms),
        "admitted witness lost after removal"
    );
    wait.emit(&after.to_string());
    drop(client);
    let origin = Instant::now();
    let wait = WaitContext::from_origin(
        test,
        "snapshot.available.after_churn",
        "more than 256 admitted records",
        None,
        origin,
        origin + TIMEOUT,
    );
    loop {
        wait.check(Some(&server));
        let client = UnixStream::connect(voyage_socket_path(&id).unwrap()).unwrap();
        let conn = expect_accepted(
            &server,
            test,
            "churn.accept",
            wait.deadline.saturating_duration_since(Instant::now()),
        );
        server.close(conn);
        expect_closed(
            &server,
            test,
            "churn.closed",
            conn,
            wait.deadline.saturating_duration_since(Instant::now()),
        );
        drop(client);
        let snapshot = server.progress_for_test();
        if !snapshot.unavailable && snapshot.overwritten.is_some_and(|count| count > 0) {
            let available = available_during_fixture_hold(&wait, &server);
            assert_eq!(available.records.len(), 256);
            assert!(available.overwritten.unwrap() > 0);
            wait.emit(&available.to_string());
            break;
        }
    }
    named!(test, "server.drop", None, drop(server));
}

#[test]
fn progress_survives_connection_removal() {
    let test = "diagnostics::progress_survives_connection_removal";
    if child_role(test) {
        return retention(test);
    }
    let captured = capture(test, ISOLATION_TIMEOUT, None);
    assert!(captured.text.contains("admitted=true"));
    record(&captured, test, "snapshot.available.after_close", "ok");
    record(&captured, test, "snapshot.available.after_churn", "ok");
    history(&captured);
}

#[test]
fn snapshot_poll_waits_for_available_history() {
    let test = "diagnostics::snapshot_poll_waits_for_available_history";
    if child_role(test) {
        return retention(test);
    }
    let captured = capture(test, ISOLATION_TIMEOUT, None);
    assert!(
        captured.text.contains("snapshot unavailable") && captured.text.contains("reason=busy")
    );
    record(&captured, test, "snapshot.available.after_close", "ok");
    record(&captured, test, "snapshot.available.after_churn", "ok");
    history(&captured);
}

#[test]
fn snapshot_poll_times_out_when_busy() {
    let test = "diagnostics::snapshot_poll_times_out_when_busy";
    if child_role(test) {
        let _root = isolated_runtime_dir();
        let server = SocketServer::bind(&fresh_voyage_id(), 1).unwrap();
        let held = server.hold_progress_for_test();
        let wait = WaitContext::new(
            test,
            "snapshot.always.busy",
            "available history",
            None,
            Duration::from_millis(100),
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            wait.available_snapshot(&server)
        }));
        drop(held);
        assert!(result.is_err());
        return;
    }
    let captured = capture(test, ISOLATION_TIMEOUT, None);
    record(&captured, test, "snapshot.always.busy", "timeout");
    assert!(
        captured.text.contains("snapshot unavailable") && captured.text.contains("reason=busy")
    );
}

#[test]
fn deadline_adapters_preserve_origin_and_outcome() {
    let test = "diagnostics::deadline_adapters_preserve_origin_and_outcome";
    if child_role(test) {
        let (mut command, entry) = test_command("diagnostics::deadline_child_role");
        let output = tempfile::NamedTempFile::new().unwrap();
        let mut child = command
            .env("SOT_TEST_SOCKET_ROLE", "diagnostics::deadline_child_role")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::from(output.reopen().unwrap()))
            .spawn()
            .unwrap();
        let pid = child.id();
        let ready = WaitContext::new(test, "child.entry", "held child's entry", None, TIMEOUT);
        while !std::fs::read_to_string(output.path())
            .unwrap()
            .contains("role=entered")
        {
            ready.check(None);
            ready.pause(Duration::from_millis(5));
        }
        ready.complete("ok", None, None);
        let work = WaitContext::new(
            test,
            "child.expiry",
            "expiry at original deadline",
            None,
            Duration::from_millis(600),
        );
        work.pause(Duration::from_millis(400));
        let error = work.child(&mut child).expect_err("held child must expire");
        entry.assert_once(pid);
        assert!(matches!(error.kind, ChildWaitKind::Expired) && error.termination_confirmed);
        assert!(
            work.started.elapsed() < Duration::from_millis(950),
            "child deadline was recomputed"
        );
        let (entered, entry) = std::sync::mpsc::channel();
        let (release, held) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            entered.send(()).unwrap();
            held.recv().unwrap();
        });
        entry.recv_timeout(TIMEOUT).unwrap();
        let wait = WaitContext::new(
            test,
            "join.expiry",
            "held worker finishes",
            None,
            Duration::from_millis(100),
        );
        let unfinished = wait
            .bounded_join(worker)
            .expect_err("held worker must reach the join deadline");
        release.send(()).unwrap();
        unfinished.join().unwrap();
        let work = WaitContext::new(
            "diagnostics::successful_iso_role",
            "iso.success",
            "successful ISO parent false",
            None,
            ISOLATION_TIMEOUT,
        );
        assert!(!work.isolated(), "successful ISO parent false is ok");
        return;
    }
    let captured = capture(test, ISOLATION_TIMEOUT, None);
    record(&captured, test, "child.expiry", "timeout");
    record(&captured, test, "join.expiry", "timeout");
    record(
        &captured,
        "diagnostics::successful_iso_role",
        "iso.success",
        "ok",
    );
}

#[test]
fn deadline_child_role() {
    let test = "diagnostics::deadline_child_role";
    if !child_role(test) {
        return;
    }
    eprintln!("fixture-proof test={test} role=entered bodies=1");
    std::io::stderr().flush().unwrap();
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
}

#[test]
fn successful_iso_role() {
    if !WaitContext::new(
        "diagnostics::successful_iso_role",
        "child.wait",
        "isolated role completes",
        None,
        ISOLATION_TIMEOUT,
    )
    .isolated()
    {
        return;
    }
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
