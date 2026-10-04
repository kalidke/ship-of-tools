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
//! Same rationale as `tests/pipe_win/`'s own [`run_isolated`] (copied
//! verbatim below, with `SOCKET_UNIX_TEST_CHILD` in place of
//! `PIPE_WIN_TEST_CHILD`): a real PROCESS boundary bounds every hang path,
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

#[cfg(target_os = "linux")]
use sot_log::lane::socket_unix::connect_voyage_socket;
use sot_log::lane::socket_unix::{voyage_socket_path, ConnId, SocketClient, SocketServer};
use sot_log::host::state_dir::current_uid;
#[cfg(target_os = "linux")]
use sot_log::lane::transport::CONNECT_BOUND;
use sot_log::lane::transport::{ClosedReason, LaneEvent, TransportError, TEARDOWN_AGGREGATE_DEADLINE};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(target_os = "linux")]
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A per-event bound used throughout (well inside `ISOLATION_TIMEOUT`, so
/// a stalled event always trips before the parent's own kill fires).
const TIMEOUT: Duration = Duration::from_secs(10);

/// The parent's hard wall-clock bound on one isolated child test.
const ISOLATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Re-invoke THIS test binary, running only `test_name`, as a child
/// process — see the module doc, and `tests/pipe_win/`'s identical
/// helper, which this is copied from verbatim (renamed env var only).
/// Returns `true` when called FROM WITHIN that child (so the caller
/// should run its real test body); returns `false` in the parent after
/// the child has run to completion (having already asserted success), so
/// the caller should just return.
fn run_isolated(test_name: &str) -> bool {
    if std::env::var("SOCKET_UNIX_TEST_CHILD").as_deref() == Ok(test_name) {
        return true;
    }
    let exe = std::env::current_exe().expect("current_exe");
    let mut child = std::process::Command::new(exe)
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("SOCKET_UNIX_TEST_CHILD", test_name)
        .spawn()
        .expect("failed to spawn isolated test child");
    let deadline = Instant::now() + ISOLATION_TIMEOUT;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                assert!(
                    status.success(),
                    "isolated test {test_name} failed in its child process: {status}"
                );
                return false;
            }
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "isolated test {test_name} did not complete within {ISOLATION_TIMEOUT:?} -- killed"
                    );
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

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

/// Bounded wait for the next transport event.
fn next_event(server: &SocketServer, timeout: Duration) -> LaneEvent {
    server
        .events()
        .recv_timeout(timeout)
        .unwrap_or_else(|e| panic!("expected a transport event within {timeout:?}, got {e}"))
}

fn expect_accepted(server: &SocketServer, timeout: Duration) -> ConnId {
    match next_event(server, timeout) {
        LaneEvent::Accepted(id) => id,
        other => panic!("expected Accepted, got {other:?}"),
    }
}

fn expect_closed(server: &SocketServer, conn_id: ConnId, timeout: Duration) -> ClosedReason {
    match next_event(server, timeout) {
        LaneEvent::Closed(id, reason) => {
            assert_eq!(id, conn_id, "Closed for the wrong connection");
            reason
        }
        other => panic!("expected Closed, got {other:?}"),
    }
}

/// Floods `client` (already connected) with fixed-size chunks until its
/// own kernel send buffer is full AND the SERVER's own reader has
/// genuinely stopped draining it (because the events channel it feeds is
/// itself full) — Codex review finding 6: OBSERVED, never assumed from a
/// fixed sleep or a fixed connection count. `client` is set non-blocking;
/// "`WouldBlock` for 500ms continuously (polled every 10ms)" is the
/// criterion for "the reader is genuinely blocked on a full channel".
/// Bounded by an overall 10s timeout so a genuine regression fails the
/// test loudly rather than hanging it. `65_536` matches
/// `lane/socket_unix/`'s own (crate-private) `READ_BUF_LEN` — not
/// importable from this integration-test crate, so duplicated as a
/// literal, the same way this file's other tests already hardcode it
/// (e.g. the `QueueFull`-flooding tests above).
fn saturate_via_stalled_writer(client: &UnixStream) {
    client.set_nonblocking(true).expect("set_nonblocking");
    let payload = vec![0xEFu8; 65_536];
    let overall_deadline = Instant::now() + Duration::from_secs(10);
    let mut would_block_since: Option<Instant> = None;
    loop {
        assert!(
            Instant::now() < overall_deadline,
            "saturation (the events channel genuinely full, observed via a persistently \
             WouldBlock-ing write) was not reached within 10s"
        );
        match (&*client).write(&payload) {
            Ok(_) => would_block_since = None,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let since = *would_block_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(500) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("unexpected write error while saturating: {e}"),
        }
    }
}

/// Poll `probe` every 10ms until it reports a nonzero count, or panic
/// loudly after `timeout` — Codex review round 2: WAIT on an OBSERVED
/// precondition (a real `TrySendError::Full`/abandonment the production
/// code itself counted, via `SocketServer::probe_*`) rather than assuming
/// one from a fixed sleep, a client-side stall heuristic, or a fixed
/// connection count.
fn wait_for_probe(mut probe: impl FnMut() -> usize, timeout: Duration, what: &str) {
    let deadline = Instant::now() + timeout;
    loop {
        if probe() > 0 {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}


mod client;
mod close;
mod connect;
mod teardown;
