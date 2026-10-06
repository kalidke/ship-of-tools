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
use sot_log::test_isolated::run_isolated;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(target_os = "linux")]
use std::path::Path;
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

/// One absolute deadline and one caller for a named blocking operation.
struct WaitContext {
    test: String,
    step: String,
    expected: String,
    conn: Option<ConnId>,
    started: Instant,
    deadline: Instant,
    caller: String,
    records: std::cell::RefCell<String>,
}

impl WaitContext {
    #[track_caller]
    fn new(test: &str, step: &str, expected: &str, conn: Option<ConnId>, bound: Duration) -> Self {
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
        let started = Instant::now();
        let wait = Self {
            test: test.into(),
            step: step.into(),
            expected: expected.into(),
            conn,
            started,
            deadline: started + bound,
            caller: format!("{file}:{}", at.line()),
            records: Default::default(),
        };
        wait.record("begin");
        wait
    }

    fn record(&self, result: &str) {
        let _ = result;
    }

    fn emit(&self, line: &str) {
        let _ = line;
    }

    fn run<T>(&self, operation: impl FnOnce() -> T) -> T {
        self.record("begin");
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)) {
            Ok(value) => {
                self.record("ok");
                value
            }
            Err(panic) => {
                self.record("error");
                std::panic::resume_unwind(panic)
            }
        }
    }

    fn io<T, E: WaitError>(&self, operation: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        self.record("begin");
        let result = operation();
        match &result {
            Ok(_) => self.record("ok"),
            Err(e) => {
                self.record(e.kind());
                self.emit(&format!("socket-error step={} error={e}", self.step));
            }
        }
        result
    }

    fn join<T>(
        &self,
        operation: impl FnOnce() -> std::thread::Result<T>,
    ) -> std::thread::Result<T> {
        self.record("begin");
        let joined = operation();
        self.record(if joined.is_ok() { "ok" } else { "error" });
        joined
    }

    fn syscall(&self, operation: impl FnOnce() -> i32) -> (i32, Option<std::io::Error>) {
        self.record("begin");
        let rc = operation();
        let error = (rc < 0).then(std::io::Error::last_os_error);
        self.record(if rc < 0 { "error" } else { "ok" });
        (rc, error)
    }

    fn fail(&self, result: &str, why: &str, server: Option<&SocketServer>) -> ! {
        self.record(result);
        if let Some(server) = server {
            self.emit(&server.progress_for_test().to_string());
        }
        panic!("{}reason={why}", self.records.borrow());
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
        let left = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(bound);
        self.record("begin");
        let result = receiver.recv_timeout(left);
        match &result {
            Ok(_) => self.record("ok"),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.record("timeout");
                if let Some(server) = server {
                    self.emit(&server.progress_for_test().to_string());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                self.record("error");
                self.emit("socket-error disconnected channel");
            }
        }
        result
    }

    fn pause(&self, duration: Duration) {
        self.run(|| {
            std::thread::sleep(
                duration.min(self.deadline.saturating_duration_since(Instant::now())),
            )
        });
    }

    fn event(&self, server: &SocketServer) -> LaneEvent {
        self.until(server, |_| true)
    }

    fn until(
        &self,
        server: &SocketServer,
        mut wanted: impl FnMut(&LaneEvent) -> bool,
    ) -> LaneEvent {
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            let event = original_next_event(server, left);
            if wanted(&event) {
                return event;
            }
        }
    }
}

trait WaitError: std::fmt::Display {
    fn kind(&self) -> &'static str;
}
impl WaitError for std::io::Error {
    fn kind(&self) -> &'static str {
        if matches!(
            self.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
        ) {
            "timeout"
        } else {
            "error"
        }
    }
}
impl WaitError for TransportError {
    fn kind(&self) -> &'static str {
        if let TransportError::Io { source, .. } = self {
            WaitError::kind(source)
        } else {
            "error"
        }
    }
}

macro_rules! named {
    ($test:expr, "child.wait", $expected:expr, $conn:expr, $body:expr) => {
        WaitContext::new(
            $test,
            "child.wait",
            $expected,
            $conn,
            sot_log::test_isolated::ISOLATION_TIMEOUT,
        )
        .run(|| $body)
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
        match wait.io(|| (&*client).write(&payload)) {
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

mod diagnostics;

// Parent adapter: the original event receiver and panic, unchanged.
fn original_next_event(server: &SocketServer, timeout: Duration) -> LaneEvent {
    server
        .events()
        .recv_timeout(timeout)
        .unwrap_or_else(|e| panic!("expected a transport event within {timeout:?}, got {e}"))
}
