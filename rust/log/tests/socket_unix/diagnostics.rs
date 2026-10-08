//! Socket diagnostic proofs validate flushed child output, observed role/pid start and exact failure causes.

use super::*;
use sot_log::test_isolated::{
    enter, fixture_start_matches, fixture_start_record, readiness_failure, readiness_scenarios,
    verify_readiness, verify_readiness_controls, FixtureOutcome, ReadinessCase, ISOLATION_TIMEOUT,
};

fn child_role(test: &str) -> bool {
    if std::env::var("SOT_TEST_SOCKET_ROLE").as_deref() == Ok(test) {
        enter(test);
        true
    } else {
        false
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

fn context_block<'a>(
    capture: &'a Captured,
    test: &str,
    step: &str,
    terminal: bool,
) -> Vec<&'a str> {
    let lines: Vec<_> = capture.text.lines().collect();
    let record = record(
        capture,
        test,
        step,
        if terminal { "timeout" } else { "begin" },
    );
    let start = lines
        .iter()
        .position(|line| line.ends_with(record))
        .unwrap()
        + 1;
    lines[start..]
        .iter()
        .copied()
        .take_while(|line| !line.starts_with("socket-test "))
        .collect()
}

fn snapshot_valid(lines: &[&str], conn: &str, unavailable: bool) -> bool {
    let headers: Vec<_> = lines
        .iter()
        .filter(|line| line.starts_with("transport-progress snapshot "))
        .collect();
    if headers.len() != 1 {
        return false;
    }
    let fields: Vec<_> = headers[0].split_whitespace().collect();
    let number = |field: &str, key: &str| {
        field
            .strip_prefix(key)
            .is_some_and(|n| n.parse::<u64>().is_ok())
    };
    if fields.get(2) == Some(&"unavailable") {
        return unavailable
            && fields.len() == 5
            && number(fields[3], "skipped=")
            && matches!(fields[4], "reason=busy" | "reason=poisoned");
    }
    fields.len() == 5
        && number(fields[2], "records=")
        && number(fields[3], "overwritten=")
        && number(fields[4], "skipped=")
        && lines.iter().any(|line| {
            line.starts_with("transport-progress transport=socket ")
                && line
                    .split_whitespace()
                    .any(|field| field == format!("conn={conn}"))
        })
}

fn checked_history(capture: &Captured, test: &str) {
    let begin = record(capture, test, "absent.closed", "begin");
    let conn = begin
        .split_whitespace()
        .find_map(|s| s.strip_prefix("conn="))
        .unwrap();
    assert!(
        snapshot_valid(
            &context_block(capture, test, "snapshot.prerequisite", false),
            conn,
            false
        ),
        "prerequisite history was not emitted"
    );
    record(capture, test, "snapshot.prerequisite", "ok");
    let prerequisite = capture.text.find("step=snapshot.prerequisite ").unwrap();
    let absent = capture.text.find("step=absent.closed ").unwrap();
    assert!(
        prerequisite < absent,
        "prerequisite history was not emitted"
    );
    assert!(
        snapshot_valid(
            &context_block(capture, test, "absent.closed", true),
            conn,
            true
        ),
        "timeout snapshot missing explicit availability accounting"
    );
}

pub(super) fn server(test: &str) -> (RuntimeDirGuard, SocketServer, UnixStream, ConnId, String) {
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
    .prerequisite_history(&server);
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
    let variant = std::env::var("SOT_TEST_SOCKET_HISTORY").unwrap_or_default();
    let held = if variant == "busy" {
        Some(server.hold_progress_for_test())
    } else {
        None
    };
    if variant == "poisoned" {
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = server.hold_progress_for_test();
            panic!("deliberate recorder poison");
        }));
        assert!(poisoned.is_err());
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
    drop(held);
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

fn check_missing_event(test: &str, unrelated: bool) {
    if child_role(test) {
        return missing_event(test, unrelated);
    }
    for variant in ["ordinary", "busy", "poisoned"] {
        let captured = capture_variant(test, ISOLATION_TIMEOUT, None, variant);
        assert!(
            !unrelated || captured.text.contains("unrelated=observed"),
            "unrelated-event fixture was not observed"
        );
        checked_history(&captured, test);
    }
}

#[test]
fn absent_event_reports_named_wait() {
    check_missing_event("diagnostics::absent_event_reports_named_wait", false);
}

#[test]
fn unrelated_event_reports_named_wait() {
    check_missing_event("diagnostics::unrelated_event_reports_named_wait", true);
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
    read::check_timeout(&captured, test, "a.eof");
    assert!(captured.text.contains("socket-error step=a.eof error="));
    read::check_snapshot(&captured);
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

fn check_retention(test: &str) {
    if child_role(test) {
        return retention(test);
    }
    let captured = capture(test, ISOLATION_TIMEOUT, None);
    let text = &captured.text;
    assert!(text.contains("admitted=true"));
    assert!(text.contains("snapshot unavailable") && text.contains("reason=busy"));
    record(&captured, test, "snapshot.available.after_close", "ok");
    record(&captured, test, "snapshot.available.after_churn", "ok");
    assert!(text.contains("transport-progress snapshot records="));
    assert!(text.contains("transport-progress transport=socket"));
}

#[test]
fn progress_survives_connection_removal() {
    check_retention("diagnostics::progress_survives_connection_removal");
}

#[test]
fn snapshot_poll_waits_for_available_history() {
    check_retention("diagnostics::snapshot_poll_waits_for_available_history");
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
        let ready = WaitContext::new(test, "child.entry", "held child's entry", None, TIMEOUT);
        let child = command
            .env("SOT_TEST_SOCKET_ROLE", "diagnostics::deadline_child_role")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::from(output.reopen().unwrap()))
            .spawn()
            .unwrap();
        let mut work = None;
        let outcome = supervise_fixture_until(child, entry, ready.deadline, |_, _, deadline| {
            observe_marker(&ready, output.path(), "role=entered bodies=1\n")?;
            ready.complete("ok", None, None);
            let timer = WaitContext::new(
                test,
                "child.expiry",
                "expiry at original deadline",
                None,
                Duration::from_millis(600),
            );
            *deadline = timer.deadline;
            timer.pause(Duration::from_millis(400));
            work = Some(timer);
            Ok(())
        });
        outcome.report("diagnostics::deadline_child_role");
        if let Some(work) = &work {
            work.child_outcome(&outcome.wait);
        }
        assert!(outcome.work.is_ok() && outcome.entry.is_ok() && outcome.termination.is_ok());
        let error = outcome.wait.unwrap_err();
        assert!(matches!(error.kind, ChildWaitKind::Expired) && error.termination_confirmed);
        assert!(
            work.unwrap().started.elapsed() < Duration::from_millis(950),
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
const READINESS_ROLE: &str = "diagnostics::readiness_child_role";
fn readiness_fixture(
    case: ReadinessCase,
    control: &str,
    held: bool,
    zero: bool,
) -> (FixtureOutcome<()>, bool) {
    let output = tempfile::NamedTempFile::new().unwrap();
    let role = READINESS_ROLE;
    let (mut command, entry) = test_command(role);
    let ready = WaitContext::new(role, "readiness.start", "fixture started", None, TIMEOUT);
    let child = command
        .env("SOT_TEST_SOCKET_ROLE", role)
        .env("SOT_TEST_SOCKET_ZERO_ENTRY", if zero { "1" } else { "0" })
        .env("SOT_TEST_SOCKET_HELD", if held { "1" } else { "0" })
        .env("SOT_TEST_FIXTURE_WITNESS", control)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::from(output.reopen().unwrap()))
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut start = false;
    let outcome = supervise_fixture_until(child, entry, ready.deadline, |_, _, deadline| {
        let missing = output.path().with_extension("missing");
        let path = if control == "before" {
            &missing
        } else {
            output.path()
        };
        observe_marker(&ready, path, "fixture-start-written")?;
        let text = std::fs::read_to_string(output.path())
            .map_err(|e| FixtureFailure::Error(e.to_string()))?;
        start = fixture_start_matches(&text, role, pid);
        eprintln!("fixture-witness child={pid} record={text:?}");
        let bound = Duration::from_millis(150);
        let wait = WaitContext::new(role, "readiness.missing", "ready", None, bound);
        *deadline = wait.started + Duration::from_millis(750);
        readiness_failure(
            case,
            control,
            &output.path().with_extension("missing"),
            || observe_marker(&wait, output.path(), "withheld-ready"),
        )
    });
    (outcome, start)
}

#[test]
fn readiness_failure_finishes_owned_child_checks() {
    for (case, held, zero) in readiness_scenarios() {
        let (outcome, start) = readiness_fixture(case, "valid", held, zero);
        verify_readiness(&outcome, READINESS_ROLE, start, case, held, zero);
    }
}
#[test]
fn readiness_proof_rejects_wrong_failure() {
    verify_readiness_controls(READINESS_ROLE, |case, control| {
        readiness_fixture(case, control, false, false)
    });
}

#[test]
fn readiness_child_role() {
    let role = READINESS_ROLE;
    if std::env::var("SOT_TEST_SOCKET_ROLE").as_deref() != Ok(role) {
        return;
    }
    if std::env::var("SOT_TEST_SOCKET_ZERO_ENTRY").as_deref() != Ok("1") {
        enter(role);
    }
    let pid = std::process::id();
    let record = match std::env::var("SOT_TEST_FIXTURE_WITNESS").as_deref() {
        Ok("pid") => fixture_start_record(role, 0),
        Ok("role") => fixture_start_record("diagnostics::wrong_role", pid),
        Ok("partial") => format!("fixture-start test={role}\n"),
        _ => fixture_start_record(role, pid),
    };
    eprint!("{record}fixture-start-written\n");
    std::io::stderr().flush().unwrap();
    if std::env::var("SOT_TEST_SOCKET_HELD").as_deref() == Ok("1") {
        loop {
            std::thread::park();
        }
    }
    std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
}

#[test]
fn history_blocks_reject_missing_or_malformed_output() {
    let test = "diagnostics::history_blocks_reject_missing_or_malformed_output";
    if child_role(test) {
        return missing_event(test, false);
    }
    let captured = capture(test, ISOLATION_TIMEOUT, None);
    checked_history(&captured, test);
    let begin = record(&captured, test, "snapshot.prerequisite", "begin");
    let end = record(&captured, test, "snapshot.prerequisite", "ok");
    let mut text = captured.text.clone();
    let start = text.find(begin).unwrap() + begin.len();
    let finish = text.find(end).unwrap();
    text.replace_range(start..finish, "\n");
    let missing = Captured {
        text,
        ..captured.clone()
    };
    let error = std::panic::catch_unwind(|| checked_history(&missing, test)).unwrap_err();
    assert!(panic_message(error).contains("prerequisite history was not emitted"));
    for malformed in [
        "",
        "transport-progress snapshot unavailable skipped=unknown reason=busy\n",
        "transport-progress snapshot unavailable skipped=1 reason=no-server\n",
        "transport-progress snapshot records=bad overwritten=0 skipped=0\n",
    ] {
        let terminal = record(&captured, test, "absent.closed", "timeout");
        let offset = captured.text.find(terminal).unwrap() + terminal.len() + 1;
        let tail = captured.text[offset..]
            .find("socket-test ")
            .map_or(captured.text.len(), |n| offset + n);
        let mut damaged = captured.clone();
        damaged.text.replace_range(offset..tail, malformed);
        let error = std::panic::catch_unwind(|| checked_history(&damaged, test)).unwrap_err();
        assert!(panic_message(error)
            .contains("timeout snapshot missing explicit availability accounting"));
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
