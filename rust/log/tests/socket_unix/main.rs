#![cfg(unix)]
//! Server-side contract tests for the L1-unix LU1b Unix-domain-socket
//! transport (`src/lane/socket_unix/`) — the mechanical twin of
//! `tests/pipe_win/`'s own contract suite, ported by TYPE SWAP: every
//! portable pipe test that exercises `PipeServer`'s public surface through
//! a raw client is ported here against `SocketServer` through a raw
//! `std::os::unix::net::UnixStream` client (the LU1c `SocketClient` —
//! `write_all`/`read`/`cancel` — and `challenge_unix.rs` are a SEPARATE
//! lane; nothing here drives either). Windows-only assertions (squat
//! detection via `FILE_FLAG_FIRST_PIPE_INSTANCE`, the pipe's SDDL, handle-
//! count via `GetProcessHandleCount`, client-side cancel/ConcurrentSubmit)
//! are replaced by their ADR 0043 analogues: owner-only socket-file mode
//! in a private runtime dir, `EADDRINUSE` while the name is held (freed
//! only by `disconnect_listener`), `/proc/self/fd` count, and accept-then-
//! close at capacity.
//!
//! # Process-isolated hang bounding
//!
//! Same rationale as `tests/pipe_win/`, through the one shared
//! `sot_log::test_isolated::run_isolated`: a real PROCESS boundary bounds every hang path,
//! including one inside a wedged `SocketServer::drop` running on the test
//! thread itself after an earlier assertion panics. Every test below that
//! touches `SocketServer`/a real `UnixStream` runs this way; the one
//! exception (`invalid_voyage_ids_and_max_connections_are_rejected_loudly`)
//! is provably non-wedging — every case in it fails before any socket
//! syscall is ever issued (rejected by validation).
//!
//! # `SOT_RUNTIME_DIR` isolation
//!
//! Every test sets `SOT_RUNTIME_DIR` to a fresh, mode-0700
//! `tempfile::tempdir()` — so tests never touch the real runtime dir, and
//! (since each one that touches real I/O runs in its own isolated child
//! process, per above) never race another test's own env var mutation.

use sot_log::host::state_dir::current_uid;
use sot_log::lane::attach_proto::ConnId;
#[cfg(target_os = "linux")]
use sot_log::lane::socket_unix::connect_voyage_socket;
use sot_log::lane::socket_unix::{voyage_socket_path, SocketClient, SocketServer};
#[cfg(target_os = "linux")]
use sot_log::lane::transport::CONNECT_BOUND;
use sot_log::lane::transport::{
    ClosedReason, LaneEvent, TransportError, TEARDOWN_AGGREGATE_DEADLINE,
};
use sot_log::test_isolated::{
    supervise_fixture_until, test_command, ChildWaitKind, FixtureFailure,
};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(target_os = "linux")]
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A per-event bound used throughout (well inside `sot_log::test_isolated::ISOLATION_TIMEOUT`, so
/// a stalled event always trips before the parent's own kill fires).
const TIMEOUT: Duration = Duration::from_secs(10);

/// A fresh, canonical lowercase-hyphenated UUID for one test's voyage id.
fn fresh_voyage_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Points `SOT_RUNTIME_DIR` at a fresh, mode-0700 tempdir for the
/// lifetime of the returned guard — only ever called from INSIDE an
/// isolated child process (or the one non-isolated, non-I/O test), so a
/// plain `set_var` needs no cross-test mutex (mirrors the module doc's
/// "`SOT_RUNTIME_DIR` isolation" section).
struct RuntimeDirGuard {
    _tmp: tempfile::TempDir,
}

fn isolated_runtime_dir() -> RuntimeDirGuard {
    // `tempdir_in("/tmp")`, never the default `$TMPDIR`: on the macOS CI
    // runner `$TMPDIR` is `/var/folders/<xx>/<28 chars>/T/` (~56 bytes) and
    // `sun_path` is only 104 there, so `<tmp>/.tmpXXXXXX/voyage-<uuid>.sock`
    // overflowed and every isolated child died at `bind` with `PathTooLong`
    // (PR #214's first CI run). Production runtime dirs are short
    // (`/run/user/<uid>/sot`, `/tmp/sot-<uid>`); this keeps the TEST's
    // socket paths inside the tightest platform bound.
    let tmp = tempfile::Builder::new()
        .prefix("sot-t")
        .tempdir_in("/tmp")
        .expect("tempdir under /tmp");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::env::set_var("SOT_RUNTIME_DIR", tmp.path());
    RuntimeDirGuard { _tmp: tmp }
}

/// One caller, original timer origin, deadline and observed terminal outcome per operation.
struct WaitContext {
    test: String,
    step: String,
    expected: String,
    conn: Option<ConnId>,
    started: Instant,
    deadline: Instant,
    caller: String,
    completed: std::cell::Cell<bool>,
    attempts: std::cell::Cell<u32>,
}

impl WaitContext {
    #[track_caller]
    fn new(test: &str, step: &str, expected: &str, conn: Option<ConnId>, bound: Duration) -> Self {
        let started = Instant::now();
        Self::from_origin(test, step, expected, conn, started, started + bound)
    }

    #[track_caller]
    fn from_origin(
        test: &str,
        step: &str,
        expected: &str,
        conn: Option<ConnId>,
        started: Instant,
        deadline: Instant,
    ) -> Self {
        assert!(
            test.contains("::") && !test.trim().is_empty(),
            "qualified test identity required"
        );
        assert!(!step.trim().is_empty(), "nonempty wait step required");
        assert!(
            !expected.trim().is_empty(),
            "nonempty wait expectation required"
        );
        let at = std::panic::Location::caller();
        let file = at.file().replace('\\', "/");
        let file = if let Some(i) = file.find("rust/") {
            file[i..].to_string()
        } else if let Some(i) = file.find("log/") {
            format!("rust/{}", &file[i..])
        } else {
            format!("rust/log/{file}")
        };
        let wait = Self {
            test: test.into(),
            step: step.into(),
            expected: expected.into(),
            conn,
            started,
            deadline,
            caller: format!("{file}:{}", at.line()),
            completed: Default::default(),
            attempts: Default::default(),
        };
        wait.record("begin");
        wait
    }

    fn record(&self, result: &str) {
        if result == "begin" {
            self.write_record(result);
        } else {
            self.complete(result, None, None);
        }
    }

    fn write_record(&self, result: &str) {
        let conn = self
            .conn
            .map_or_else(|| "pending".into(), |id| id.to_string());
        self.emit(&format!("socket-test test={} child={} step={} expected={} conn={} elapsed_ms={} caller={} result={result}",
            self.test, std::process::id(), self.step, self.expected, conn, self.started.elapsed().as_millis(), self.caller));
    }

    fn emit(&self, line: &str) {
        let mut err = std::io::stderr().lock();
        writeln!(err, "{line}").expect("write socket diagnostic");
        err.flush().expect("flush socket diagnostic");
    }

    fn complete(
        &self,
        result: &str,
        reason: Option<&dyn std::fmt::Display>,
        server: Option<&SocketServer>,
    ) {
        if self.completed.replace(true) {
            return;
        }
        self.write_record(result);
        if let Some(reason) = reason {
            self.emit(&format!("socket-error step={} error={reason}", self.step));
        }
        if result == "timeout" {
            match server {
                Some(server) => self.emit(&server.progress_for_test().to_string()),
                None => self.emit(
                    "transport-progress snapshot unavailable skipped=unknown reason=no-server",
                ),
            }
        }
    }

    /// Infallible work without a deadline-taking or boolean completion boundary.
    fn run<T>(&self, operation: impl FnOnce() -> T) -> T {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)) {
            Ok(value) => {
                self.complete("ok", None, None);
                value
            }
            Err(panic) => {
                self.complete("error", Some(&"operation panicked"), None);
                std::panic::resume_unwind(panic)
            }
        }
    }

    fn attempt_io<T, E: WaitError>(
        &self,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        operation()
    }

    fn io<T, E: WaitError>(&self, operation: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let result = operation();
        self.io_outcome(&result, false, None);
        result
    }

    fn io_outcome<T, E: WaitError>(
        &self,
        result: &Result<T, E>,
        blocking: bool,
        server: Option<&SocketServer>,
    ) {
        match result {
            Ok(_) => self.complete("ok", None, server),
            Err(error) => self.complete(error.kind(blocking), Some(error), server),
        }
    }

    fn remaining_io(&self, server: Option<&SocketServer>) -> std::io::Result<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let error = std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "absolute deadline expired before I/O",
            );
            self.complete("timeout", Some(&error), server);
            Err(error)
        } else {
            Ok(left)
        }
    }

    fn write_all(
        &self,
        server: Option<&SocketServer>,
        stream: &mut UnixStream,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        let result = (|| {
            let mut pending = bytes;
            while !pending.is_empty() {
                let left = self.remaining_io(server)?;
                stream.set_write_timeout(Some(left))?;
                let n = stream.write(pending)?;
                if n == 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::WriteZero));
                }
                pending = &pending[n..];
            }
            Ok(())
        })();
        self.io_outcome(&result, true, server);
        result
    }

    fn join<T>(
        &self,
        operation: impl FnOnce() -> std::thread::Result<T>,
    ) -> std::thread::Result<T> {
        let joined = operation();
        self.complete(if joined.is_ok() { "ok" } else { "error" }, None, None);
        joined
    }

    fn bounded_join<T>(
        &self,
        thread: std::thread::JoinHandle<T>,
    ) -> Result<std::thread::Result<T>, std::thread::JoinHandle<T>> {
        while !thread.is_finished() && Instant::now() < self.deadline {
            self.pause(Duration::from_millis(5));
        }
        if !thread.is_finished() {
            self.complete("timeout", Some(&"worker join deadline expired"), None);
            return Err(thread);
        }
        Ok(self.join(|| thread.join()))
    }

    fn workers(&self, server: &mut SocketServer) -> bool {
        let ok = server.join_workers(self.deadline);
        self.complete(
            if ok { "ok" } else { "timeout" },
            (!ok).then_some(&"worker join deadline expired" as &dyn std::fmt::Display),
            Some(server),
        );
        ok
    }

    fn isolated(&self) -> bool {
        let result = sot_log::test_isolated::run_isolated_until(&self.test, self.deadline);
        self.child_outcome(&result);
        result.unwrap_or_else(|error| panic!("{error}"))
    }

    fn child_outcome<T>(&self, result: &Result<T, sot_log::test_isolated::ChildWaitError>) {
        match result {
            Ok(_) => self.complete("ok", None, None),
            Err(error) => self.complete(
                if matches!(error.kind, sot_log::test_isolated::ChildWaitKind::Expired) {
                    "timeout"
                } else {
                    "error"
                },
                Some(error),
                None,
            ),
        }
    }

    fn syscall(&self, operation: impl FnOnce() -> i32) -> (i32, Option<std::io::Error>) {
        let rc = operation();
        let error = (rc < 0).then(std::io::Error::last_os_error);
        self.complete(
            if rc < 0 { "error" } else { "ok" },
            error.as_ref().map(|e| e as &dyn std::fmt::Display),
            None,
        );
        (rc, error)
    }

    fn fail(&self, result: &str, why: &str, server: Option<&SocketServer>) -> ! {
        self.complete(result, Some(&why), server);
        panic!("socket wait {}: {why}", self.step);
    }

    fn check(&self, server: Option<&SocketServer>) {
        if Instant::now() >= self.deadline {
            self.fail("timeout", "absolute deadline expired", server);
        }
    }

    fn receive(
        &self,
        server: &SocketServer,
        bound: Duration,
    ) -> Result<LaneEvent, std::sync::mpsc::RecvTimeoutError> {
        self.receive_from(server.events(), bound, Some(server))
    }

    fn receive_from(
        &self,
        receiver: &std::sync::mpsc::Receiver<LaneEvent>,
        bound: Duration,
        server: Option<&SocketServer>,
    ) -> Result<LaneEvent, std::sync::mpsc::RecvTimeoutError> {
        let result = self.receive_attempt(receiver, bound);
        match &result {
            Ok(_) => self.complete("ok", None, server),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.complete("timeout", Some(&"timed out waiting on channel"), server)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                self.complete("error", Some(&"disconnected channel"), server)
            }
        }
        result
    }

    fn receive_attempt(
        &self,
        receiver: &std::sync::mpsc::Receiver<LaneEvent>,
        slice: Duration,
    ) -> Result<LaneEvent, std::sync::mpsc::RecvTimeoutError> {
        receiver.recv_timeout(
            self.deadline
                .saturating_duration_since(Instant::now())
                .min(slice),
        )
    }

    fn pause(&self, duration: Duration) {
        std::thread::sleep(duration.min(self.deadline.saturating_duration_since(Instant::now())));
    }

    fn event(&self, server: &SocketServer) -> LaneEvent {
        self.until(server, |_| true)
    }

    fn next(&self, server: &SocketServer) -> LaneEvent {
        self.check(Some(server));
        self.receive_attempt(
            server.events(),
            self.deadline.saturating_duration_since(Instant::now()),
        )
        .unwrap_or_else(|error| {
            self.fail(
                if error == std::sync::mpsc::RecvTimeoutError::Timeout {
                    "timeout"
                } else {
                    "error"
                },
                &error.to_string(),
                Some(server),
            )
        })
    }

    fn until(
        &self,
        server: &SocketServer,
        mut wanted: impl FnMut(&LaneEvent) -> bool,
    ) -> LaneEvent {
        loop {
            let event = self.next(server);
            if wanted(&event) {
                self.complete("ok", None, Some(server));
                return event;
            }
        }
    }

    fn available_snapshot(&self, server: &SocketServer) -> sot_log::lane::test_progress::Snapshot {
        self.available_snapshot_observing(server, || {})
    }

    fn prerequisite_history(
        &self,
        server: &SocketServer,
    ) -> sot_log::lane::test_progress::Snapshot {
        loop {
            self.check(Some(server));
            let snapshot = server.progress_for_test();
            if !snapshot.unavailable && snapshot.records.iter().any(|r| r.conn == self.conn) {
                self.emit(&snapshot.to_string());
                self.complete("ok", None, Some(server));
                return snapshot;
            }
            self.pause(Duration::from_millis(5));
        }
    }

    fn available_snapshot_observing(
        &self,
        server: &SocketServer,
        mut unavailable: impl FnMut(),
    ) -> sot_log::lane::test_progress::Snapshot {
        loop {
            self.check(Some(server));
            let snapshot = server.progress_for_test();
            if !snapshot.unavailable {
                self.complete("ok", None, Some(server));
                return snapshot;
            }
            self.emit(&snapshot.to_string());
            unavailable();
            self.pause(Duration::from_millis(5));
        }
    }
}

trait WaitError: std::fmt::Display {
    fn kind(&self, blocking: bool) -> &'static str;
}
impl WaitError for std::io::Error {
    fn kind(&self, blocking: bool) -> &'static str {
        if self.kind() == std::io::ErrorKind::TimedOut
            || (blocking && self.kind() == std::io::ErrorKind::WouldBlock)
        {
            "timeout"
        } else {
            "error"
        }
    }
}
impl WaitError for TransportError {
    fn kind(&self, blocking: bool) -> &'static str {
        if let TransportError::Io { source, .. } = self {
            WaitError::kind(source, blocking)
        } else {
            "error"
        }
    }
}

macro_rules! named {
    ($test:expr, "child.wait", $expected:expr, $conn:expr) => {
        WaitContext::new(
            $test,
            "child.wait",
            $expected,
            $conn,
            sot_log::test_isolated::ISOLATION_TIMEOUT,
        )
        .isolated()
    };
    ($test:expr, "server.drop", $conn:expr, $body:expr) => {
        WaitContext::new(
            $test,
            "server.drop",
            "drop completes",
            $conn,
            TEARDOWN_AGGREGATE_DEADLINE,
        )
        .run(|| $body)
    };
    ($test:expr, $step:expr, $conn:expr, $body:expr) => {
        named!($test, $step, concat!($step, " result"), $conn, $body)
    };
    ($test:expr, $step:expr, $expected:expr, $conn:expr, $body:expr) => {
        WaitContext::new($test, $step, $expected, $conn, TIMEOUT).run(|| $body)
    };
}
macro_rules! io_named {
    ($test:expr, $step:expr, $conn:expr, $body:expr) => {
        io_named!($test, $step, concat!($step, " result"), $conn, $body)
    };
    ($test:expr, $step:expr, $expected:expr, $conn:expr, $body:expr) => {
        WaitContext::new($test, $step, $expected, $conn, TIMEOUT).io(|| $body)
    };
}

#[track_caller]
fn next_event(
    server: &SocketServer,
    test: &str,
    step: &str,
    expected: &str,
    conn: Option<ConnId>,
    timeout: Duration,
) -> LaneEvent {
    WaitContext::new(test, step, expected, conn, timeout).event(server)
}

#[track_caller]
fn expect_accepted(server: &SocketServer, test: &str, step: &str, timeout: Duration) -> ConnId {
    match next_event(server, test, step, "Accepted", None, timeout) {
        LaneEvent::Accepted(id) => id,
        other => panic!("expected Accepted, got {other:?}"),
    }
}

#[track_caller]
fn expect_closed(
    server: &SocketServer,
    test: &str,
    step: &str,
    conn_id: ConnId,
    timeout: Duration,
) -> ClosedReason {
    match next_event(server, test, step, "Closed", Some(conn_id), timeout) {
        LaneEvent::Closed(id, reason) => {
            assert_eq!(id, conn_id, "Closed for the wrong connection");
            reason
        }
        other => panic!("expected Closed, got {other:?}"),
    }
}

#[track_caller]
fn saturate_via_stalled_writer(server: &SocketServer, test: &str, client: &UnixStream) {
    let wait = WaitContext::new(
        test,
        "saturate",
        "persistent WouldBlock for 500ms",
        None,
        TIMEOUT,
    );
    io_named!(
        test,
        "saturate.nonblocking",
        "nonblocking socket",
        None,
        client.set_nonblocking(true)
    )
    .unwrap();
    let payload = vec![0xEFu8; 65_536];
    let mut would_block_since: Option<Instant> = None;
    loop {
        wait.check(Some(server));
        match wait.attempt_io(|| (&*client).write(&payload)) {
            Ok(_) => would_block_since = None,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let since = *would_block_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(500) {
                    wait.record("ok");
                    return;
                }
                wait.pause(Duration::from_millis(10));
            }
            Err(e) => wait.fail("error", &e.to_string(), Some(server)),
        }
    }
}

#[track_caller]
fn wait_for_probe(
    server: &SocketServer,
    test: &str,
    step: &str,
    mut probe: impl FnMut() -> usize,
    timeout: Duration,
    what: &str,
) {
    let wait = WaitContext::new(test, step, what, None, timeout);
    loop {
        if probe() > 0 {
            wait.record("ok");
            return;
        }
        wait.check(Some(server));
        wait.pause(Duration::from_millis(10));
    }
}

mod client;
mod close;
mod connect;
mod teardown;

#[derive(Clone)]
struct Captured {
    text: String,
    pid: u32,
    expired: bool,
    begin_observed: bool,
}

/// ISO owns the child wait and cleanup; the capture file introduces no drain deadline.
fn capture(test: &str, bound: Duration, release_after_begin: Option<&str>) -> Captured {
    capture_variant(test, bound, release_after_begin, "ordinary")
}

fn capture_variant(test: &str, bound: Duration, release: Option<&str>, variant: &str) -> Captured {
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
    let child = command
        .env("SOT_TEST_SOCKET_ROLE", test)
        .env("SOT_TEST_SOCKET_HISTORY", variant)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(file.try_clone().unwrap()))
        .stderr(Stdio::from(file.try_clone().unwrap()))
        .spawn()
        .expect("spawn the fixture child");
    let pid = child.id();
    let mut begin_observed = false;
    let outcome = supervise_fixture_until(child, entry, wait.deadline, |child, _, deadline| {
        if let Some(step) = release {
            let observe = WaitContext::from_origin(
                test,
                "begin.observe",
                "complete emitted begin before release",
                None,
                wait.started,
                *deadline,
            );
            while Instant::now() < observe.deadline {
                let text = std::fs::read_to_string(output.path())
                    .map_err(|e| FixtureFailure::Error(e.to_string()))?;
                begin_observed = text.split_inclusive('\n').any(|line| {
                    line.contains(&format!("socket-test test={test} child={pid} "))
                        && line.contains(&format!(" step={step} "))
                        && line.ends_with("result=begin\n")
                });
                if begin_observed {
                    break;
                }
                observe.pause(Duration::from_millis(5));
            }
            observe.complete(if begin_observed { "ok" } else { "timeout" }, None, None);
            if !begin_observed {
                return Err(FixtureFailure::Timeout(
                    "child begin was not visible before release".into(),
                ));
            }
            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(b"r")
                .map_err(|e| FixtureFailure::Error(e.to_string()))?;
            eprintln!("fixture-proof test={test} child={pid} begin={step} observed=before-release");
        }
        Ok(())
    });
    wait.child_outcome(&outcome.wait);
    outcome.report(test);
    // A final capture read can fail only after all mandatory child checks have been attempted.
    let text = std::fs::read_to_string(output.path());
    let expired = outcome
        .wait
        .as_ref()
        .is_err_and(|e| matches!(e.kind, ChildWaitKind::Expired));
    assert!(
        outcome.termination.is_ok() && outcome.entry.is_ok() && outcome.output.is_ok(),
        "fixture finalization failed; see retained errors above"
    );
    let text = text.expect("read finalized fixture capture");
    eprintln!("{text}");
    if let Err(error) = outcome.work {
        panic!("{error}: {text}");
    }
    match outcome.wait {
        Ok(status) => assert!(status.success(), "fixture child failed: {status}: {text}"),
        Err(error) if !expired => panic!("fixture supervision failed: {error}"),
        Err(_) => {}
    }
    eprintln!(
        "body-proof test={test} child={pid} bodies=1 completed={} cleanup=confirmed",
        !expired
    );
    Captured {
        text,
        pid,
        expired,
        begin_observed,
    }
}

fn observe_marker(
    wait: &WaitContext,
    path: &std::path::Path,
    marker: &str,
) -> Result<(), FixtureFailure> {
    while Instant::now() < wait.deadline {
        let text =
            std::fs::read_to_string(path).map_err(|e| FixtureFailure::Error(e.to_string()))?;
        if text
            .split_inclusive('\n')
            .any(|line| line.ends_with('\n') && line.trim_end().ends_with(marker.trim_end()))
        {
            return Ok(());
        }
        wait.pause(Duration::from_millis(5));
    }
    Err(FixtureFailure::Timeout(
        "expected readiness marker missing".into(),
    ))
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(text) => *text,
        Err(payload) => payload.downcast_ref::<&str>().copied().unwrap_or("").into(),
    }
}

mod diagnostics;

mod privileged;
mod read;
