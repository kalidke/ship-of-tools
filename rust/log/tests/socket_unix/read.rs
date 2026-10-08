//! Bounded socket reads observe timeout setup and read separately under one original context.

use super::*;

impl WaitContext {
    fn io_phase(
        &self,
        attempt: u32,
        phase: &str,
        result: &str,
        bytes: Option<usize>,
        error: Option<&std::io::Error>,
    ) {
        self.emit(&format!("socket-io test={} child={} step={} conn={} attempt={attempt} phase={phase} result={result} bytes={} kind={} errno={} caller={} reason={}",
            self.test, std::process::id(), self.step, self.conn.map_or("pending".into(), |id| id.to_string()),
            bytes.map_or("none".into(), |n| n.to_string()), error.map_or("none".into(), |e| format!("{:?}", e.kind())),
            error.and_then(std::io::Error::raw_os_error).map_or("none".into(), |n| n.to_string()), self.caller,
            error.map_or("none".into(), ToString::to_string)));
    }

    fn read_operations(
        &self,
        server: Option<&SocketServer>,
        stream: &mut UnixStream,
        bytes: &mut [u8],
        setup: impl FnOnce(&UnixStream, Duration) -> std::io::Result<()>,
        read: impl FnOnce(&mut UnixStream, &mut [u8]) -> std::io::Result<usize>,
    ) -> std::io::Result<usize> {
        let attempt = self.attempts.get() + 1;
        self.attempts.set(attempt);
        self.io_phase(attempt, "deadline.remaining", "begin", None, None);
        let left = match self.remaining_io(server) {
            Ok(left) => {
                self.io_phase(attempt, "deadline.remaining", "ok", None, None);
                left
            }
            Err(error) => {
                self.io_phase(attempt, "deadline.remaining", "timeout", None, Some(&error));
                return Err(error);
            }
        };
        self.io_phase(attempt, "deadline.setup", "begin", None, None);
        if let Err(error) = setup(stream, left) {
            self.io_phase(
                attempt,
                "deadline.setup",
                WaitError::kind(&error, true),
                None,
                Some(&error),
            );
            self.complete(WaitError::kind(&error, true), Some(&error), server);
            return Err(error);
        }
        self.io_phase(attempt, "deadline.setup", "ok", None, None);
        // Setup spends the same context; it neither completes the wait nor renews its deadline.
        if let Err(error) = self.remaining_io(server) {
            self.io_phase(attempt, "deadline.remaining", "timeout", None, Some(&error));
            return Err(error);
        }
        self.io_phase(attempt, "read", "begin", None, None);
        let result = read(stream, bytes);
        match &result {
            Ok(n) => self.io_phase(attempt, "read", "ok", Some(*n), None),
            Err(error) => self.io_phase(
                attempt,
                "read",
                WaitError::kind(error, true),
                None,
                Some(error),
            ),
        }
        if result.is_err() {
            self.io_outcome(&result, true, server);
        }
        result
    }

    pub(super) fn read_attempt(
        &self,
        server: Option<&SocketServer>,
        stream: &mut UnixStream,
        bytes: &mut [u8],
    ) -> std::io::Result<usize> {
        self.read_operations(
            server,
            stream,
            bytes,
            |s, left| s.set_read_timeout(Some(left)),
            |s, b| s.read(b),
        )
    }

    pub(super) fn read(
        &self,
        server: Option<&SocketServer>,
        stream: &mut UnixStream,
        bytes: &mut [u8],
    ) -> std::io::Result<usize> {
        let result = self.read_attempt(server, stream, bytes);
        self.io_outcome(&result, true, server);
        result
    }
}

fn child_role(test: &str) -> bool {
    if std::env::var("SOT_TEST_SOCKET_ROLE").as_deref() == Ok(test) {
        sot_log::test_isolated::enter(test);
        true
    } else {
        false
    }
}

fn phase_lines<'a>(capture: &'a Captured, test: &str, step: &str) -> Vec<&'a str> {
    let prefix = format!("socket-io test={test} child={} step={step} ", capture.pid);
    capture
        .text
        .lines()
        .filter_map(|line| {
            let start = line.find(&prefix)?;
            Some(&line[start..])
        })
        .collect()
}

/// The error phase is accepted only against independently observed operation callbacks.
fn checked_error(capture: &Captured, test: &str, phase: &str) {
    let observed = capture
        .text
        .lines()
        .find(|line| line.starts_with("read-observed "))
        .expect("independent operation observations missing");
    let field = |key: &str| {
        observed
            .split_whitespace()
            .find_map(|s| s.strip_prefix(key))
            .unwrap()
    };
    assert_eq!(field("phase="), phase, "wrong selected operation cause");
    assert_eq!(field("setup="), "1", "setup callback never ran");
    assert_eq!(field("read="), if phase == "read" { "1" } else { "0" });
    let lines = phase_lines(capture, test, "controlled.read");
    let failing: Vec<_> = lines
        .iter()
        .filter(|l| l.contains("result=error "))
        .collect();
    assert!(
        failing.len() == 1
            && failing[0].contains(&format!("phase={phase} "))
            && failing[0].contains(&format!("kind={} ", field("kind=")))
            && failing[0].contains(&format!("errno={} ", field("errno="))),
        "bounded read lost the failing operation phase"
    );
    if phase == "read" {
        assert!(
            lines
                .iter()
                .position(|l| l.contains("phase=deadline.setup result=ok "))
                .unwrap()
                < lines
                    .iter()
                    .position(|l| l.contains("phase=read result=begin "))
                    .unwrap()
        );
    } else {
        assert!(
            !lines.iter().any(|l| l.contains("phase=read ")),
            "setup failure reached read"
        );
    }
    assert_eq!(
        capture
            .text
            .lines()
            .filter(|l| l.starts_with("socket-test ")
                && l.contains("step=controlled.read ")
                && l.ends_with("result=error"))
            .count(),
        1
    );
}

fn controlled_failure(test: &str, phase: &str) {
    let (mut client, peer) = UnixStream::pair().unwrap();
    let setup_count = std::cell::Cell::new(0);
    let read_count = std::cell::Cell::new(0);
    // Observe the native setter's error first; the read control returns an equal-kind error after real setup.
    let selected = peer.set_read_timeout(Some(Duration::ZERO)).unwrap_err();
    let errno = selected.raw_os_error();
    let kind = selected.kind();
    let wait = WaitContext::new(
        test,
        "controlled.read",
        "selected operation failure",
        None,
        TIMEOUT,
    );
    let error = wait
        .read_operations(
            None,
            &mut client,
            &mut [0],
            |stream, left| {
                setup_count.set(setup_count.get() + 1);
                stream.set_read_timeout(Some(if phase == "deadline.setup" {
                    Duration::ZERO
                } else {
                    left
                }))
            },
            |_, _| {
                read_count.set(read_count.get() + 1);
                Err(errno.map_or_else(
                    || std::io::Error::from(kind),
                    std::io::Error::from_raw_os_error,
                ))
            },
        )
        .unwrap_err();
    assert_eq!(error.kind(), kind);
    assert_eq!(error.raw_os_error(), errno);
    eprintln!(
        "read-observed phase={phase} setup={} read={} kind={kind:?} errno={}",
        setup_count.get(),
        read_count.get(),
        errno.map_or("none".into(), |n| n.to_string())
    );
}

#[test]
fn setup_and_read_failures_have_distinct_captured_phases() {
    let test = "read::setup_and_read_failures_have_distinct_captured_phases";
    if child_role(test) {
        let phase = std::env::var("SOT_TEST_SOCKET_HISTORY").unwrap();
        return controlled_failure(test, &phase);
    }
    for phase in ["deadline.setup", "read"] {
        let captured =
            capture_variant(test, sot_log::test_isolated::ISOLATION_TIMEOUT, None, phase);
        checked_error(&captured, test, phase);
        let wrong = std::panic::catch_unwind(|| {
            checked_error(
                &captured,
                test,
                if phase == "read" {
                    "deadline.setup"
                } else {
                    "read"
                },
            )
        });
        assert!(
            wrong.is_err()
                && panic_message(wrong.unwrap_err()).contains("wrong selected operation cause")
        );
        eprintln!("read-phase-proof phase={phase} bodies=1 cleanup=confirmed provenance=controlled-operation");
    }
}

pub(super) fn check_timeout(capture: &Captured, test: &str, step: &str) {
    let lines = phase_lines(capture, test, step);
    let setup = lines
        .iter()
        .position(|l| l.contains("phase=deadline.setup result=ok "))
        .expect("successful timeout setup missing");
    let begin = lines
        .iter()
        .position(|l| l.contains("phase=read result=begin "))
        .expect("read entry missing");
    let end = lines
        .iter()
        .position(|l| l.contains("phase=read result=timeout "))
        .expect("read timeout phase missing");
    assert!(setup < begin && begin < end, "read phase order changed");
    assert!(lines[end].contains("kind=WouldBlock ") || lines[end].contains("kind=TimedOut "));
    assert_eq!(
        capture
            .text
            .lines()
            .filter(|l| l.starts_with("socket-test ")
                && l.contains(&format!("step={step} "))
                && l.ends_with("result=timeout"))
            .count(),
        1
    );
    let lines: Vec<_> = capture.text.lines().collect();
    let terminal = lines
        .iter()
        .position(|line| {
            line.starts_with("socket-test ")
                && line.contains(&format!("step={step} "))
                && line.ends_with("result=timeout")
        })
        .unwrap();
    let immediate: Vec<_> = lines[terminal + 1..]
        .iter()
        .take_while(|line| !line.starts_with("socket-test "))
        .collect();
    assert!(
        immediate
            .iter()
            .any(|line| line.starts_with("transport-progress snapshot ")),
        "read timeout missing its immediate snapshot"
    );
}

fn real_reads(test: &str) {
    let (mut client, mut peer) = UnixStream::pair().unwrap();
    peer.write_all(b"xy").unwrap();
    let wait = WaitContext::new(test, "data.eof", "data then strict EOF", None, TIMEOUT);
    let deadline = wait.deadline;
    let mut byte = [0];
    assert_eq!(wait.read_attempt(None, &mut client, &mut byte).unwrap(), 1);
    assert_eq!(byte, *b"x");
    drop(peer);
    assert_eq!(wait.read_attempt(None, &mut client, &mut byte).unwrap(), 1);
    assert_eq!(byte, *b"y");
    assert_eq!(wait.read(None, &mut client, &mut byte).unwrap(), 0);
    assert_eq!(wait.deadline, deadline);
    let (_root, server, mut client, conn, _id) = diagnostics::server(test);
    server.send(conn, b"p".to_vec(), Some(1)).unwrap();
    let wait = WaitContext::new(
        test,
        "partial.timeout",
        "partial data then timeout",
        Some(conn),
        Duration::from_millis(200),
    );
    let deadline = wait.deadline;
    assert_eq!(
        wait.read_attempt(Some(&server), &mut client, &mut byte)
            .unwrap(),
        1
    );
    assert_eq!(byte, *b"p");
    wait.pause(Duration::from_millis(50));
    let error = wait
        .read(Some(&server), &mut client, &mut byte)
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    assert_eq!(
        wait.deadline, deadline,
        "partial work restarted the read deadline"
    );
    assert!(Instant::now() >= deadline);
    eprintln!("read-preservation data=xy eof=0 partial=p deadline=original bodies=1");
}

#[test]
fn real_reads_preserve_eof_data_and_original_deadline() {
    let test = "read::real_reads_preserve_eof_data_and_original_deadline";
    if child_role(test) {
        return real_reads(test);
    }
    let captured = capture(test, sot_log::test_isolated::ISOLATION_TIMEOUT, None);
    assert!(captured
        .text
        .contains("read-preservation data=xy eof=0 partial=p deadline=original bodies=1"));
    check_timeout(&captured, test, "partial.timeout");
    let lines = phase_lines(&captured, test, "data.eof");
    assert!(lines
        .iter()
        .any(|l| l.contains("phase=read result=ok bytes=0 ")));
    assert!(captured.text.contains("transport-progress snapshot "));
    assert_eq!(
        captured
            .text
            .lines()
            .filter(|l| l.starts_with("socket-test ")
                && l.contains("step=data.eof ")
                && l.ends_with("result=ok"))
            .count(),
        1
    );
}

pub(super) fn check_snapshot(captured: &Captured) {
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
