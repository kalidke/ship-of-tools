#![cfg(unix)]
//! Server-side contract tests for the L1-unix LU1b Unix-domain-socket
//! transport (`src/socket_unix.rs`) — the mechanical twin of
//! `tests/pipe_win.rs`'s own contract suite, ported by TYPE SWAP: every
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
//! Same rationale as `tests/pipe_win.rs`'s own [`run_isolated`] (copied
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
use sot_log::socket_unix::connect_voyage_socket;
use sot_log::socket_unix::{
    voyage_socket_path, ClosedReason, ConnId, SocketClient, SocketError, SocketServer,
    TransportEvent,
};
use sot_log::state_dir::current_uid;
#[cfg(target_os = "linux")]
use sot_log::transport::CONNECT_BOUND;
use sot_log::transport::TEARDOWN_AGGREGATE_DEADLINE;
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
/// process — see the module doc, and `tests/pipe_win.rs`'s identical
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
fn next_event(server: &SocketServer, timeout: Duration) -> TransportEvent {
    server
        .events()
        .recv_timeout(timeout)
        .unwrap_or_else(|e| panic!("expected a transport event within {timeout:?}, got {e}"))
}

fn expect_accepted(server: &SocketServer, timeout: Duration) -> ConnId {
    match next_event(server, timeout) {
        TransportEvent::Accepted(id) => id,
        other => panic!("expected Accepted, got {other:?}"),
    }
}

fn expect_closed(server: &SocketServer, conn_id: ConnId, timeout: Duration) -> ClosedReason {
    match next_event(server, timeout) {
        TransportEvent::Closed(id, reason) => {
            assert_eq!(id, conn_id, "Closed for the wrong connection");
            reason
        }
        other => panic!("expected Closed, got {other:?}"),
    }
}

/// Pull `Bytes` events for `conn_id` until `expected_len` bytes have
/// accumulated — the stream is byte-type, so a single write is not
/// guaranteed to surface as a single `Bytes` event.
fn accumulate_bytes(
    server: &SocketServer,
    conn_id: ConnId,
    expected_len: usize,
    timeout: Duration,
) -> Vec<u8> {
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    while out.len() < expected_len {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "only got {} of {expected_len} expected bytes: {out:?}",
            out.len()
        );
        match next_event(server, remaining) {
            TransportEvent::Bytes(cid, bytes) => {
                assert_eq!(cid, conn_id, "Bytes for the wrong connection");
                out.extend(bytes);
            }
            other => panic!("expected Bytes, got {other:?}"),
        }
    }
    assert_eq!(
        out.len(),
        expected_len,
        "accumulated more than expected: {out:?}"
    );
    out
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
/// `socket_unix.rs`'s own (crate-private) `READ_BUF_LEN` — not
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

/// One connect -> accept -> server-close -> confirmed-closed -> client
/// drop cycle, used by the churn/leak test. `#[cfg(target_os = "linux")]`:
/// its one caller is the `/proc/self/fd`-based leak test, itself gated
/// the same way (the macOS CI leg still compiles this whole file, so an
/// ungated helper with no non-Linux caller would warn there).
#[cfg(target_os = "linux")]
fn churn_one(server: &SocketServer, path: &Path) {
    let client = UnixStream::connect(path).unwrap();
    let conn_id = expect_accepted(server, TIMEOUT);
    server.close(conn_id);
    expect_closed(server, conn_id, TIMEOUT);
    drop(client);
}

/// Test 1: one server, one client, bytes both ways (accumulated); a
/// marker-tagged send's `Sent` event fires once its `write` physically
/// completes.
#[test]
fn server_and_client_exchange_bytes_and_sent_carries_marker() {
    if !run_isolated("server_and_client_exchange_bytes_and_sent_carries_marker") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 4).unwrap();
    let mut client = UnixStream::connect(&path).unwrap();
    let conn_id = expect_accepted(&server, TIMEOUT);

    let outbound = b"hello from client";
    client.write_all(outbound).unwrap();
    let got = accumulate_bytes(&server, conn_id, outbound.len(), TIMEOUT);
    assert_eq!(got, outbound);

    let inbound = b"hello from server";
    server.send(conn_id, inbound.to_vec(), Some(42)).unwrap();
    let mut buf = vec![0u8; inbound.len()];
    let mut got = 0;
    while got < buf.len() {
        got += client.read(&mut buf[got..]).unwrap();
    }
    assert_eq!(buf, inbound);

    match next_event(&server, TIMEOUT) {
        TransportEvent::Sent(cid, marker) => {
            assert_eq!(cid, conn_id);
            assert_eq!(marker, 42);
        }
        other => panic!("expected Sent, got {other:?}"),
    }

    drop(server);
}

/// Test 2: two clients connected to the same voyage socket are
/// multiplexed by distinct `ConnId`s.
#[test]
fn two_concurrent_clients_multiplexed_by_conn_id() {
    if !run_isolated("two_concurrent_clients_multiplexed_by_conn_id") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 4).unwrap();

    let mut client_a = UnixStream::connect(&path).unwrap();
    let conn_a = expect_accepted(&server, TIMEOUT);
    let mut client_b = UnixStream::connect(&path).unwrap();
    let conn_b = expect_accepted(&server, TIMEOUT);
    assert_ne!(conn_a, conn_b);

    client_a.write_all(b"from A").unwrap();
    client_b.write_all(b"from B").unwrap();

    let mut a_got = Vec::new();
    let mut b_got = Vec::new();
    let deadline = Instant::now() + TIMEOUT;
    while a_got.len() < 6 || b_got.len() < 6 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "timed out: a={a_got:?} b={b_got:?}");
        match next_event(&server, remaining) {
            TransportEvent::Bytes(cid, bytes) if cid == conn_a => a_got.extend(bytes),
            TransportEvent::Bytes(cid, bytes) if cid == conn_b => b_got.extend(bytes),
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(a_got, b"from A");
    assert_eq!(b_got, b"from B");

    drop(server);
}

/// ADR 0043, acceptance matrix "teardown composes": real worker fan-out
/// (several live connections torn down at once, none of them having
/// disconnected on their own) completes well inside the pinned aggregate
/// deadline.
#[test]
fn worst_case_worker_fan_out_completes_well_inside_the_aggregate_budget() {
    if !run_isolated("worst_case_worker_fan_out_completes_well_inside_the_aggregate_budget") {
        return;
    }
    let _rt = isolated_runtime_dir();
    assert!(Duration::from_secs(5) < TEARDOWN_AGGREGATE_DEADLINE);
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let max_connections = 8;
    let mut server = SocketServer::bind(&id, max_connections).unwrap();

    let mut clients = Vec::new();
    for _ in 0..max_connections {
        let client = UnixStream::connect(&path).unwrap();
        expect_accepted(&server, TIMEOUT);
        clients.push(client); // every connection stays LIVE -- worst case
    }

    server.disconnect_listener();
    let started = Instant::now();
    let ok = server.join_workers(started + Duration::from_secs(5));
    assert!(
        ok,
        "real teardown of {max_connections} live connections did not finish within a 5s budget \
         (took at least {:?})",
        started.elapsed()
    );
    drop(clients);
}

/// ADR 0043, acceptance matrix "teardown composes": a GENUINELY STALLED
/// connection worker (one whose peer never drains, never closes) must not
/// prevent the OTHER connections from being cancelled and torn down, and
/// the AGGREGATE join must still resolve within a small bound.
#[test]
fn stalled_worker_does_not_block_teardown_of_healthy_connections() {
    if !run_isolated("stalled_worker_does_not_block_teardown_of_healthy_connections") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let max_connections = 4;
    let mut server = SocketServer::bind(&id, max_connections).unwrap();

    // One connection whose client never reads and never writes again --
    // outbound bytes queued for it will sit until the server side
    // shuts down the fd out from under it. Flood until the outbound
    // budget genuinely reports full, proving the writer thread has real
    // in-flight/backed-up work when teardown begins.
    let stalled_client = UnixStream::connect(&path).unwrap();
    let stalled_conn = expect_accepted(&server, TIMEOUT);
    let payload = vec![0xABu8; 65_536];
    let mut saw_full = false;
    for _ in 0..128 {
        match server.send(stalled_conn, payload.clone(), None) {
            Ok(()) => {}
            Err(SocketError::QueueFull(cid)) => {
                assert_eq!(cid, stalled_conn);
                saw_full = true;
                break;
            }
            Err(other) => panic!("unexpected send error: {other}"),
        }
    }
    assert!(
        saw_full,
        "expected the outbound budget to report full against a stalled peer"
    );

    let mut healthy_clients = Vec::new();
    for _ in 0..2 {
        let c = UnixStream::connect(&path).unwrap();
        expect_accepted(&server, TIMEOUT);
        healthy_clients.push(c);
    }

    let started = Instant::now();
    // A budget an order of magnitude under the pinned 20s: `shutdown(2)`
    // unsticks the stalled writer promptly (ADR 0043 decision 5), so
    // healthy AND stalled connections alike tear down promptly -- no
    // Windows-style completion-proof scaffolding is needed to prove this.
    server.disconnect_listener();
    let ok = server.join_workers(started + Duration::from_secs(5));
    assert!(
        ok,
        "teardown with one stalled connection among several live ones did not finish within a \
         5s budget (took at least {:?})",
        started.elapsed()
    );
    drop(stalled_client);
    drop(healthy_clients);
}

/// ADR 0043, acceptance matrix "teardown composes", "total-deadline
/// propagation": the shared deadline is REAL and honored against REAL OS
/// threads. An essentially-zero budget against otherwise-healthy, real
/// connections still returns promptly (never hangs out to the pinned
/// 20s) -- the natural race (some threads may already have finished
/// before the first `is_finished` poll) means this asserts BOUNDED total
/// time, not a specific `true`/`false` outcome.
#[test]
fn join_workers_deadline_is_enforced_against_real_threads_not_merely_computed() {
    if !run_isolated(
        "join_workers_deadline_is_enforced_against_real_threads_not_merely_computed",
    ) {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let max_connections = 4;
    let mut server = SocketServer::bind(&id, max_connections).unwrap();

    let mut clients = Vec::new();
    for _ in 0..3 {
        let c = UnixStream::connect(&path).unwrap();
        expect_accepted(&server, TIMEOUT);
        clients.push(c);
    }

    server.disconnect_listener();
    let started = Instant::now();
    let _ok = server.join_workers(started + Duration::from_millis(1));
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "join_workers with an essentially-zero budget must return promptly, not silently wait \
         out the full aggregate regardless of outcome (took {elapsed:?})"
    );
    drop(clients);
}

/// Test: a client that connects and never reads while the server floods
/// it. The outbound BYTE budget eventually reports full once the
/// head-of-line `write` is stuck in the kernel; `close` is
/// fire-and-forget, so the bound under test is how promptly the `Closed`
/// event follows.
#[test]
fn flooded_never_reading_client_close_completes_within_bound() {
    if !run_isolated("flooded_never_reading_client_close_completes_within_bound") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = UnixStream::connect(&path).unwrap(); // deliberately never reads
    let conn_id = expect_accepted(&server, TIMEOUT);

    let payload = vec![0xABu8; 65_536];
    let mut saw_full = false;
    for _ in 0..128 {
        match server.send(conn_id, payload.clone(), None) {
            Ok(()) => {}
            Err(SocketError::QueueFull(cid)) => {
                assert_eq!(cid, conn_id);
                saw_full = true;
                break;
            }
            Err(other) => panic!("unexpected send error: {other}"),
        }
    }
    assert!(
        saw_full,
        "expected the outbound budget to report full against a non-reading peer"
    );

    server.close(conn_id);
    assert_eq!(
        expect_closed(&server, conn_id, TIMEOUT),
        ClosedReason::Closed
    );

    drop(server);
    drop(client);
}

/// Test: a pending accept with no client ever connecting — server drop
/// must return promptly.
#[test]
fn pending_accept_with_no_client_drops_promptly() {
    if !run_isolated("pending_accept_with_no_client_drops_promptly") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let server = SocketServer::bind(&id, 1).unwrap();
    drop(server);
}

/// Drop-vs-lifecycle-delivery regression: saturate the events channel and
/// never drain it, then drop the server. `Drop` must still return -- it
/// must not deadlock behind its own `send_lifecycle_event` escape by
/// joining the accept thread before setting `dropping`. Codex review
/// round 2, finding 2: this must exercise a LIFECYCLE send blocked behind
/// the full channel -- `send_lifecycle_event`'s own `dropping` escape
/// hatch is the regression this test names, not merely `deliver_bytes`'s
/// separate (and separately tested) abandon path.
#[test]
fn drop_returns_even_with_a_saturated_events_channel() {
    if !run_isolated("drop_returns_even_with_a_saturated_events_channel") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = UnixStream::connect(&path).unwrap();
    let _conn_id = expect_accepted(&server, TIMEOUT);

    // Never drain events() -- flood until the events channel is OBSERVED
    // genuinely full (a real `TrySendError::Full` the production code
    // itself counted), not assumed from a client-side stall heuristic.
    saturate_via_stalled_writer(&client);
    wait_for_probe(
        || server.probe_events_full_bytes(),
        Duration::from_secs(10),
        "the events channel to genuinely report Full for a Bytes delivery",
    );

    // Connect a SECOND client so the acceptor's own `Accepted` for it
    // blocks in `send_lifecycle_event`'s retry loop against the SAME full
    // channel -- proving the actual regression under test, not merely
    // that the reader's own (separate) `deliver_bytes` retry stalled.
    let second_client = UnixStream::connect(&path).unwrap();
    wait_for_probe(
        || server.probe_events_full_lifecycle(),
        Duration::from_secs(10),
        "a lifecycle event (this second connection's own Accepted) to genuinely block \
         against the full channel",
    );

    // Must return even though nobody ever drained events(), and within
    // the SAME aggregate teardown bound every other test in this suite is
    // held to.
    let started = Instant::now();
    drop(server);
    assert!(
        started.elapsed() < TEARDOWN_AGGREGATE_DEADLINE,
        "Drop did not return within the teardown aggregate deadline (took {:?})",
        started.elapsed()
    );
    drop(client);
    drop(second_client);
}

/// Invalid voyage ids and out-of-range connection ceilings are rejected
/// loudly. Provably non-wedging (every case fails before any socket
/// syscall is ever issued), so this test is NOT process-isolated.
#[test]
fn invalid_voyage_ids_and_max_connections_are_rejected_loudly() {
    let _rt = isolated_runtime_dir();
    let bad_ids = [
        "../../../etc/passwd",
        "not-a-uuid",
        "550E8400-E29B-41D4-A716-446655440000", // uppercase
        "550e8400e29b41d4a716446655440000",     // no hyphens ("simple" form)
        "550e8400-e29b-41d4-a716-44665544000",  // one hex digit short
        "{550e8400-e29b-41d4-a716-446655440000}", // braced GUID form
        "",
    ];
    for bad in bad_ids {
        let err = SocketServer::bind(bad, 1).unwrap_err();
        assert!(
            matches!(err, SocketError::InvalidVoyageId(_)),
            "id {bad:?}: got {err}"
        );
        let path_err = voyage_socket_path(bad).unwrap_err();
        assert!(
            matches!(path_err, SocketError::InvalidVoyageId(_)),
            "id {bad:?}: got {path_err}"
        );
    }

    let id = fresh_voyage_id();
    assert!(matches!(
        SocketServer::bind(&id, 0).unwrap_err(),
        SocketError::InvalidMaxConnections
    ));
    assert!(matches!(
        SocketServer::bind(&id, 256).unwrap_err(),
        SocketError::InvalidMaxConnections
    ));
}

/// `close` on the server side gives the client an ordered EOF; dropping a
/// client gives the server a `Closed(Eof)`.
#[test]
fn server_close_yields_client_eof_and_client_drop_yields_server_closed() {
    if !run_isolated("server_close_yields_client_eof_and_client_drop_yields_server_closed") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();

    let mut client_a = UnixStream::connect(&path).unwrap();
    let conn_a = expect_accepted(&server, TIMEOUT);
    server.close(conn_a);
    let mut buf = [0u8; 16];
    let n = client_a.read(&mut buf).unwrap();
    assert_eq!(n, 0, "expected ordered EOF after a server-initiated close");
    assert_eq!(
        expect_closed(&server, conn_a, TIMEOUT),
        ClosedReason::Closed
    );

    let client_b = UnixStream::connect(&path).unwrap();
    let conn_b = expect_accepted(&server, TIMEOUT);
    drop(client_b);
    assert_eq!(expect_closed(&server, conn_b, TIMEOUT), ClosedReason::Eof);

    drop(server);
}

/// PRIMARY, deterministic: the client connects, the test waits for
/// `Accepted` (proving registration definitely happened) BEFORE closing,
/// then asserts `Closed(Eof)`. The instant-close race itself is a
/// separate smoke test below.
#[test]
fn eof_before_registration_is_handled_cleanly() {
    if !run_isolated("eof_before_registration_is_handled_cleanly") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();

    let client = UnixStream::connect(&path).unwrap();
    let conn_id = expect_accepted(&server, TIMEOUT); // synchronize FIRST
    drop(client); // now close, after registration is proven

    assert_eq!(expect_closed(&server, conn_id, TIMEOUT), ClosedReason::Eof);

    let client2 = UnixStream::connect(&path).unwrap();
    let _ = expect_accepted(&server, TIMEOUT);
    drop(client2);

    drop(server);
}

/// Smoke test: a client that connects and disconnects with NO
/// synchronization at all. Ported defensively (accepting either honest
/// outcome, matching `tests/pipe_win.rs`'s own version) even though a
/// Unix listen backlog makes the accept side considerably more
/// deterministic than a named pipe's `ConnectNamedPipe` — this only
/// proves the race never wedges anything and never poisons the socket for
/// the next client.
#[test]
fn eof_before_registration_smoke_test_accepts_either_honest_outcome() {
    if !run_isolated("eof_before_registration_smoke_test_accepts_either_honest_outcome") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();

    let client = UnixStream::connect(&path).unwrap();
    drop(client); // no synchronization -- this IS the race under test

    match server.events().recv_timeout(Duration::from_secs(2)) {
        Ok(TransportEvent::Accepted(conn_id)) => {
            assert_eq!(expect_closed(&server, conn_id, TIMEOUT), ClosedReason::Eof);
        }
        Err(_timed_out) => {
            // The other honest outcome: no event at all for this attempt.
        }
        Ok(other) => panic!("unexpected event: {other:?}"),
    }

    // Whichever happened, the socket must still be healthy.
    let client2 = UnixStream::connect(&path).unwrap();
    let _ = expect_accepted(&server, TIMEOUT);
    drop(client2);

    drop(server);
}

/// Sequential connect/close churn must not grow this process's OS fd
/// count without bound. Isolated so `/proc/self/fd` is not confounded by
/// other tests running concurrently.
#[test]
#[cfg(target_os = "linux")]
fn sequential_connect_close_churn_does_not_leak_fds() {
    if !run_isolated("sequential_connect_close_churn_does_not_leak_fds") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 4).unwrap();

    for _ in 0..5 {
        churn_one(&server, &path);
    }

    let before = open_fd_count();
    for _ in 0..50 {
        churn_one(&server, &path);
    }
    let after = open_fd_count();

    assert!(
        after <= before + 6,
        "fd count grew from {before} to {after} across 50 connect/close cycles in isolation -- \
         suspected leak"
    );

    drop(server);
}

#[cfg(target_os = "linux")]
fn open_fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .expect("read /proc/self/fd")
        .count()
}

/// New coverage (round-3 finding 1's Unix analogue): once the events
/// channel saturates and stays that way past the `Bytes` abandon bound,
/// the reader force-closes the connection and a `Closed` is GUARANTEED to
/// eventually appear in the backlog once drained — never a silent stream
/// gap.
#[test]
fn event_channel_saturation_abandons_bytes_and_guarantees_closed() {
    if !run_isolated("event_channel_saturation_abandons_bytes_and_guarantees_closed") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = UnixStream::connect(&path).unwrap();
    let conn_id = expect_accepted(&server, TIMEOUT);

    // OBSERVE the stall (Codex review round 2), rather than assuming it
    // from a fixed sleep: flood until the reader's own delivery retry is
    // genuinely stuck against the never-drained events channel.
    saturate_via_stalled_writer(&client);
    wait_for_probe(
        || server.probe_events_full_bytes(),
        Duration::from_secs(10),
        "the events channel to genuinely report Full for a Bytes delivery",
    );

    // WAIT for the reader's own retry loop to have genuinely given up and
    // force-closed the connection -- `probe_bytes_abandoned` is the
    // production code's own count of that, never a fixed sleep guessing
    // at `BYTES_ABANDON_AFTER` (crate-private, 5s -- not importable from
    // this integration-test crate) plus a margin.
    wait_for_probe(
        || server.probe_bytes_abandoned(),
        Duration::from_secs(10),
        "deliver_bytes to genuinely abandon this connection's Bytes delivery",
    );

    // Drain everything queued; the LAST event must be the guaranteed
    // Closed this abandonment produces -- every Bytes event this
    // connection will ever produce was necessarily enqueued BEFORE the
    // reader gave up (nothing more is ever sent for it afterward), so in
    // FIFO delivery order Closed is the true tail, not merely "observed
    // at some point".
    let mut last: Option<TransportEvent> = None;
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        match server.events().recv_timeout(Duration::from_secs(1)) {
            Ok(evt) => last = Some(evt),
            Err(_) => break,
        }
    }
    match last {
        Some(TransportEvent::Closed(cid, ClosedReason::Error(msg))) => {
            assert_eq!(cid, conn_id, "Closed for the wrong connection");
            assert!(
                msg.contains("abandoned"),
                "expected an abandonment message, got {msg:?}"
            );
        }
        other => panic!(
            "expected the drained tail to be Closed(_, Error(msg containing \"abandoned\")), \
             got {other:?}"
        ),
    }

    drop(client);
    drop(server);
}

/// ADR 0043 decision 3: the socket file is owner-only (0600) inside a
/// private, owner-only (0700) runtime dir — the Unix analogue of
/// `pipe_win.rs`'s own SDDL descriptor test.
#[test]
fn socket_is_owner_only_in_a_private_dir() {
    if !run_isolated("socket_is_owner_only_in_a_private_dir") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 1).unwrap();

    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let meta = std::fs::symlink_metadata(&path).expect("stat the socket file");
    assert!(!meta.file_type().is_symlink(), "socket file must not be a symlink");
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o600,
        "socket file must be owner-only 0600"
    );
    assert_eq!(meta.uid(), current_uid());

    let parent = path.parent().expect("socket path has a parent");
    let parent_meta = std::fs::symlink_metadata(parent).expect("stat the runtime dir");
    assert!(!parent_meta.file_type().is_symlink());
    assert_eq!(
        parent_meta.permissions().mode() & 0o777,
        0o700,
        "runtime dir must be owner-only 0700"
    );
    assert_eq!(parent_meta.uid(), current_uid());

    drop(server);
}

/// ADR 0043 decision 2, property 3/5: a rival RAW `UnixListener::bind` on
/// the SAME path must fail `EADDRINUSE` while the server is live (the
/// name is held continuously), and succeed ONLY after
/// `disconnect_listener` — proving the name is freed SYNCHRONOUSLY, not
/// merely eventually. A second `SocketServer::bind` is deliberately NOT
/// the probe here (it would itself unlink-and-rebind, telling us nothing
/// about whether the FIRST server was still actually holding the name).
#[test]
fn rival_bind_fails_while_held_and_succeeds_after_disconnect_listener() {
    if !run_isolated("rival_bind_fails_while_held_and_succeeds_after_disconnect_listener") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let mut server = SocketServer::bind(&id, 2).unwrap();

    let err = UnixListener::bind(&path).expect_err("expected the held name to refuse a rival bind");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::AddrInUse,
        "expected EADDRINUSE while the server is live, got {err}"
    );

    server.disconnect_listener();
    UnixListener::bind(&path)
        .unwrap_or_else(|e| panic!("expected the freed name to bind again: {e}"));

    // Codex review finding 1 (P1): `disconnect_listener` must unlink its
    // OWN endpoint EXACTLY ONCE. Bind a REPLACEMENT `SocketServer` at the
    // identical voyage id (as a real caller's next leg would), then drop
    // the OLD (already torn-down) server — its `Drop` calls
    // `disconnect_listener` again. If that second call unlinked
    // unconditionally, it would delete the REPLACEMENT's endpoint out
    // from under it: prove it did not by connecting a fresh client to the
    // replacement and observing `Accepted`, and by confirming its socket
    // file still exists.
    let mut replacement = SocketServer::bind(&id, 2).unwrap();
    drop(server);

    let _client = UnixStream::connect(&path)
        .unwrap_or_else(|e| panic!("replacement server should still accept connections: {e}"));
    expect_accepted(&replacement, TIMEOUT);
    assert!(
        path.exists(),
        "the replacement's own socket file must still exist after the old server's Drop"
    );

    replacement.disconnect_listener();
}

/// ADR 0043 decision 4: Unix cannot refuse a connection at connect time —
/// the kernel completes the handshake from the listen backlog regardless
/// of `max_connections` — so at capacity the acceptor accepts and closes
/// immediately. The excess client sees EOF promptly; the first
/// (already-registered) connection is unaffected.
#[test]
fn capacity_excess_connection_is_closed_immediately() {
    if !run_isolated("capacity_excess_connection_is_closed_immediately") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 1).unwrap();

    let first = UnixStream::connect(&path).unwrap();
    let _first_conn = expect_accepted(&server, TIMEOUT);

    let mut second = UnixStream::connect(&path).unwrap();
    second
        .set_read_timeout(Some(TIMEOUT))
        .expect("set_read_timeout");
    let mut buf = [0u8; 16];
    let n = second
        .read(&mut buf)
        .expect("read should observe an ordered EOF, not an error");
    assert_eq!(n, 0, "expected the excess connection to see EOF promptly");

    // No event is ever queued for the excess connection, and the first
    // (still-live, at-capacity) connection is unaffected.
    assert!(
        server.events().recv_timeout(Duration::from_millis(200)).is_err(),
        "no event expected: the excess connection never registers, and the first stays live"
    );

    drop(first);
    drop(second);
    drop(server);
}

// ---------------------------------------------------------------------
// L1-unix LU1c: `SocketClient` (`write_all`/`read`/`cancel`, and the
// bounded connect retry loop). Mirrors the analogous section of
// `tests/pipe_win.rs`. `SocketClient::from_stream_for_test` builds a
// client around a plain `UnixStream::connect` — the `pub(crate)`
// unchallenged constructors are not reachable from this separate
// integration-test crate, exactly like `pipe_win::
// connect_voyage_pipe_unchallenged` is not reachable from
// `tests/pipe_win.rs` either.
// ---------------------------------------------------------------------

/// A `SocketClient::read` blocked on one thread is unblocked by
/// `cancel()` called from another, returning `SocketError::Cancelled`.
#[test]
fn client_read_cancel_unblocks_from_another_thread() {
    if !run_isolated("client_read_cancel_unblocks_from_another_thread") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = Arc::new(SocketClient::from_stream_for_test(
        UnixStream::connect(&path).unwrap(),
        0,
    ));
    let _conn_id = expect_accepted(&server, TIMEOUT);

    let reader_client = Arc::clone(&client);
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        reader_client.read(&mut buf) // blocks -- the server never sends anything
    });

    std::thread::sleep(Duration::from_millis(300)); // let the read actually become pending
    client.cancel();

    let result = reader.join().unwrap();
    assert!(
        matches!(result, Err(SocketError::Cancelled)),
        "expected Cancelled, got {result:?}"
    );

    drop(server);
}

/// A `SocketClient::write_all` blocked on one thread (the kernel send
/// buffer saturated because nobody drains it) is unblocked by `cancel()`
/// called from another.
#[test]
fn client_write_cancel_unblocks_from_another_thread() {
    if !run_isolated("client_write_cancel_unblocks_from_another_thread") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = Arc::new(SocketClient::from_stream_for_test(
        UnixStream::connect(&path).unwrap(),
        0,
    ));
    let _conn_id = expect_accepted(&server, TIMEOUT);
    // Deliberately never drain `server.events()` from here on -- that is
    // what eventually stalls the server's reader and lets the raw socket
    // buffer fill up behind it, giving the client's own `write_all`
    // something real to block on.

    let writer_client = Arc::clone(&client);
    let writer = std::thread::spawn(move || {
        let payload = vec![0xCDu8; 65_536];
        loop {
            match writer_client.write_all(&payload) {
                Ok(()) => {}
                Err(e) => return e,
            }
        }
    });

    std::thread::sleep(Duration::from_secs(2)); // let the flood saturate the events channel + socket buffer
    client.cancel();

    let result = writer.join().unwrap();
    assert!(
        matches!(result, SocketError::Cancelled),
        "expected Cancelled, got {result:?}"
    );

    drop(server);
}

/// A SECOND concurrent same-direction `SocketClient::read` returns
/// `SocketError::ConcurrentSubmit` rather than racing the first caller.
#[test]
fn concurrent_same_direction_client_read_returns_distinct_error() {
    if !run_isolated("concurrent_same_direction_client_read_returns_distinct_error") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = Arc::new(SocketClient::from_stream_for_test(
        UnixStream::connect(&path).unwrap(),
        0,
    ));
    let _conn_id = expect_accepted(&server, TIMEOUT);

    let a = Arc::clone(&client);
    let reader_a = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        a.read(&mut buf) // blocks -- nobody ever sends
    });

    std::thread::sleep(Duration::from_millis(300)); // let A's read actually become pending

    let mut buf_b = [0u8; 16];
    let result_b = client.read(&mut buf_b);
    assert!(
        matches!(result_b, Err(SocketError::ConcurrentSubmit)),
        "expected ConcurrentSubmit, got {result_b:?}"
    );

    client.cancel();
    let result_a = reader_a.join().unwrap();
    assert!(
        matches!(result_a, Err(SocketError::Cancelled)),
        "expected Cancelled, got {result_a:?}"
    );

    drop(server);
}

/// Property 34: once cancelled, a `SocketClient` permanently rejects
/// EVERY later submission -- `read` and `write_all` alike -- without
/// ever touching the OS again. Cancelling twice is idempotent.
#[test]
fn cancelled_client_rejects_later_submissions() {
    if !run_isolated("cancelled_client_rejects_later_submissions") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 2).unwrap();
    let client = SocketClient::from_stream_for_test(UnixStream::connect(&path).unwrap(), 0);
    let _conn_id = expect_accepted(&server, TIMEOUT);

    client.cancel();

    let mut buf = [0u8; 16];
    assert!(
        matches!(client.read(&mut buf), Err(SocketError::Cancelled)),
        "a cancelled client must permanently reject a later read"
    );
    assert!(
        matches!(client.write_all(b"x"), Err(SocketError::Cancelled)),
        "a cancelled client must permanently reject a later write"
    );

    // Idempotent: cancelling again must not panic or change the outcome.
    client.cancel();
    assert!(matches!(client.read(&mut buf), Err(SocketError::Cancelled)));

    drop(server);
}

/// ADR 0043 decision 4, property 18: with nothing ever listening at all
/// (no socket file exists), every `connect(2)` attempt sees `ENOENT`,
/// retried within [`CONNECT_BOUND`] before failing loudly. Exercised
/// through the fully public `connect_voyage_socket` -- since connect
/// never succeeds, the subsequent challenge step is never reached, so
/// this proves the CONNECT retry loop specifically, not the challenge.
#[test]
#[cfg(target_os = "linux")]
fn connect_retries_within_the_bound_then_fails_when_nothing_listens() {
    if !run_isolated("connect_retries_within_the_bound_then_fails_when_nothing_listens") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();

    let started = Instant::now();
    let err = connect_voyage_socket(&id).unwrap_err();
    let elapsed = started.elapsed();

    assert!(
        matches!(err, SocketError::Io { op, .. } if op.contains("connect")),
        "expected a connect-family error, got {err}"
    );
    assert!(
        elapsed >= CONNECT_BOUND,
        "expected the retry loop to actually run out the {CONNECT_BOUND:?} bound, took {elapsed:?}"
    );
    // "one attempt may overrun the bound by a single sleep" -- a generous
    // upper margin, not a tight one, so CI scheduling jitter can't flake
    // this.
    assert!(
        elapsed < CONNECT_BOUND + Duration::from_secs(5),
        "retry loop ran far longer than the bound plus one overrun sleep: {elapsed:?}"
    );
}

/// ADR 0043 decision 4, property 18: a socket special file that exists
/// but has nobody listening behind it (bound, then the listener dropped
/// without unlinking) makes every `connect(2)` see `ECONNREFUSED`,
/// retried the same way `ENOENT` is.
#[test]
#[cfg(target_os = "linux")]
fn connect_refused_by_a_stale_socket_file_retries_then_fails() {
    if !run_isolated("connect_refused_by_a_stale_socket_file_retries_then_fails") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    // `std`'s own `UnixListener` does not unlink its socket file on
    // `Drop` -- binding then immediately dropping leaves a stale special
    // file on disk with nobody behind it, so a real client's `connect(2)`
    // sees `ECONNREFUSED`, not `ENOENT`.
    drop(UnixListener::bind(&path).unwrap());

    let started = Instant::now();
    let err = connect_voyage_socket(&id).unwrap_err();
    let elapsed = started.elapsed();

    assert!(
        matches!(err, SocketError::Io { op, .. } if op.contains("connect")),
        "expected a connect-family error, got {err}"
    );
    assert!(
        elapsed >= CONNECT_BOUND,
        "expected the retry loop to actually run out the {CONNECT_BOUND:?} bound, took {elapsed:?}"
    );
}

/// ADR 0043 decision 4: at capacity the acceptor accepts and closes
/// immediately -- from a real `SocketClient`'s own perspective (not a raw
/// `UnixStream`, see `capacity_excess_connection_is_closed_immediately`
/// above for that variant), the excess connection's first `read` sees an
/// early, ordinary `Ok(0)`.
#[test]
fn excess_connection_at_capacity_is_seen_as_early_eof() {
    if !run_isolated("excess_connection_at_capacity_is_seen_as_early_eof") {
        return;
    }
    let _rt = isolated_runtime_dir();
    let id = fresh_voyage_id();
    let path = voyage_socket_path(&id).unwrap();
    let server = SocketServer::bind(&id, 1).unwrap();

    let _first = UnixStream::connect(&path).unwrap();
    let _first_conn = expect_accepted(&server, TIMEOUT);

    let second = SocketClient::from_stream_for_test(UnixStream::connect(&path).unwrap(), 0);
    let mut buf = [0u8; 16];
    let n = second
        .read(&mut buf)
        .expect("read should observe an ordered EOF, not an error");
    assert_eq!(n, 0, "expected the excess connection to see EOF promptly");

    drop(server);
}
