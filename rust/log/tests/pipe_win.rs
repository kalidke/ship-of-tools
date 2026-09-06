#![cfg(windows)]
//! Integration tests for the ADR 0041 step-5 pipe transport
//! (`src/pipe_win.rs`, unit U3 round 3 — discharges the second Codex
//! adversarial round's test findings). Lives in `tests/` for the same
//! structural reason `tests/conpty.rs` and `tests/capsule_win.rs` do:
//! this module's own types are `pub` specifically so a real-pipe
//! integration test can reach them.
//!
//! # Process-isolated hang bounding (round-3 findings 7-8)
//!
//! An in-thread watchdog (spawn a thread, `recv_timeout` on a completion
//! signal) cannot actually bound every hang path: `client.read` blocking
//! directly on the TEST thread is not wrapped by it at all, and if an
//! assertion earlier in the same test panics, unwinding drops `server`
//! right there on the test thread — invoking a potentially wedged
//! `PipeServer::drop` completely outside any watchdog. A real PROCESS
//! boundary bounds both: [`run_isolated`] re-invokes THIS test binary as
//! a child process running only the one named test (`--exact
//! <name>`), and the parent kills that child if it outlives a hard
//! deadline — regardless of WHERE inside the child a hang occurs. Every
//! test below that touches `PipeServer`/`PipeClient` I/O runs this way;
//! the one exception (`invalid_voyage_ids_and_instance_counts_are_rejected_loudly`)
//! is provably non-wedging — every call in it fails before any Win32 I/O
//! call is ever issued (rejected by validation).
//!
//! The handle-count test additionally NEEDS isolation for correctness,
//! not just safety: `GetProcessHandleCount` measures the whole process,
//! so it would be confounded by every other pipe test running
//! concurrently in the shared default parallel runner. Isolated, it is
//! the only thing running in its process.
//!
//! Two structural fixes from round 2 stay: the pipe is byte-type, so
//! tests that check received content accumulate bytes across events
//! rather than assuming one write equals one `Bytes` event.

use sot_log::challenge::{challenge, ChallengeOutcome};
use sot_log::exchange::VoyageMgmtExchange;
use sot_log::pipe_win::{
    connect_voyage_pipe, ClosedReason, ConnId, PipeError, PipeServer, TransportEvent,
};
use sot_log::transport::TEARDOWN_AGGREGATE_DEADLINE;
use sot_log::wire::{self, MgmtReply, MgmtRequest, Survival};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A per-event bound used throughout (well inside `ISOLATION_TIMEOUT`, so
/// a stalled event always trips before the parent's own kill fires).
const TIMEOUT: Duration = Duration::from_secs(10);

/// The parent's hard wall-clock bound on one isolated child test.
const ISOLATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Re-invoke THIS test binary, running only `test_name`, as a child
/// process (round-3 findings 7-8) — see the module doc. Returns `true`
/// when called FROM WITHIN that child (so the caller should run its real
/// test body); returns `false` in the parent after the child has run to
/// completion (having already asserted success), so the caller should
/// just return.
///
/// Controlled by the `PIPE_WIN_TEST_CHILD` env var, set to `test_name`
/// only in the spawned child — the standard self-re-exec pattern for
/// isolating one test in its own process without a second binary.
fn run_isolated(test_name: &str) -> bool {
    if std::env::var("PIPE_WIN_TEST_CHILD").as_deref() == Ok(test_name) {
        return true;
    }
    let exe = std::env::current_exe().expect("current_exe");
    let mut child = std::process::Command::new(exe)
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("PIPE_WIN_TEST_CHILD", test_name)
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
                    panic!("isolated test {test_name} did not complete within {ISOLATION_TIMEOUT:?} -- killed");
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

/// Bounded wait for the next transport event.
fn next_event(server: &PipeServer, timeout: Duration) -> TransportEvent {
    server
        .events()
        .recv_timeout(timeout)
        .unwrap_or_else(|e| panic!("expected a transport event within {timeout:?}, got {e}"))
}

fn expect_accepted(server: &PipeServer, timeout: Duration) -> ConnId {
    match next_event(server, timeout) {
        TransportEvent::Accepted(id) => id,
        other => panic!("expected Accepted, got {other:?}"),
    }
}

fn expect_closed(server: &PipeServer, conn_id: ConnId, timeout: Duration) -> ClosedReason {
    match next_event(server, timeout) {
        TransportEvent::Closed(id, reason) => {
            assert_eq!(id, conn_id, "Closed for the wrong connection");
            reason
        }
        other => panic!("expected Closed, got {other:?}"),
    }
}

/// Pull `Bytes` events for `conn_id` until `expected_len` bytes have
/// accumulated — the pipe is byte-type, so a single write is not
/// guaranteed to surface as a single `Bytes` event.
fn accumulate_bytes(
    server: &PipeServer,
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

/// NUL-terminated UTF-16, matching `pipe_win.rs`'s own private helper.
fn wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Attempt to create the FIRST instance of `voyage_id`'s pipe name with
/// `FILE_FLAG_FIRST_PIPE_INSTANCE` — the squat-detection probe.
/// `max_instances` MUST match the server's own value under test (round-3
/// finding 9): Win32 requires every instance of a name to agree on
/// `nMaxInstances`, so a probe using a different value would mix the
/// intended `FIRST_PIPE_INSTANCE` failure with an unrelated
/// instance-count-mismatch failure.
fn try_create_first_instance(voyage_id: &str, max_instances: u32) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    let name = wide(&format!(r"\\.\pipe\sot-voyage-{voyage_id}"));
    let h = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS | PIPE_WAIT,
            max_instances,
            65536,
            65536,
            0,
            std::ptr::null(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        Err(std::io::Error::last_os_error())
    } else {
        unsafe { CloseHandle(h) };
        Ok(())
    }
}

/// Assert that a squat probe failed with one of the TWO documented codes
/// Windows can report for the same underlying protection:
/// `ERROR_ACCESS_DENIED` (5) is `FILE_FLAG_FIRST_PIPE_INSTANCE`'s own
/// check firing against a name that already has ANY instance;
/// `ERROR_PIPE_BUSY` (231) is the plain instance-count check firing
/// because `nMaxInstances` is already saturated (as it continuously is
/// under the continuous-hold design whenever `max_instances` is small).
/// Both mean the same thing: the name could not be taken.
fn assert_squat_check_failed(err: std::io::Error) {
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY};
    let code = err.raw_os_error();
    assert!(
        code == Some(ERROR_ACCESS_DENIED as i32) || code == Some(ERROR_PIPE_BUSY as i32),
        "expected ERROR_ACCESS_DENIED (5) or ERROR_PIPE_BUSY (231), got {err}"
    );
}

/// This process's own token-user SID, stringified — independently
/// derived so a bug in `pipe_win.rs`'s or `fsutil.rs`'s own SID lookup
/// could not also hide from this test.
fn current_user_sid_string() -> String {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        assert_ne!(
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token),
            0
        );
        let mut needed: u32 = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        assert!(
            needed > 0,
            "GetTokenInformation sizing call returned zero length"
        );
        let words = (needed as usize).div_ceil(8);
        let mut buf: Vec<u64> = vec![0u64; words];
        let buf_ptr = buf.as_mut_ptr().cast::<u8>();
        assert_ne!(
            GetTokenInformation(token, TokenUser, buf_ptr.cast(), needed, &mut needed),
            0
        );
        let sid = (*buf_ptr.cast::<TOKEN_USER>()).User.Sid;
        let mut sid_str: *mut u16 = std::ptr::null_mut();
        assert_ne!(ConvertSidToStringSidW(sid, &mut sid_str), 0);
        let len = (0..).take_while(|&i| *sid_str.add(i) != 0).count();
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(sid_str, len));
        LocalFree(sid_str as _);
        CloseHandle(token);
        s
    }
}

/// Round-trip a LIVE PIPE HANDLE's DACL to SDDL text via `GetSecurityInfo`
/// (Microsoft directs named-pipe security queries through the
/// HANDLE-based `GetSecurityInfo`, not the name-based
/// `GetNamedSecurityInfoW`).
fn security_descriptor_sddl(handle: windows_sys::Win32::Foundation::HANDLE) -> String {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
        SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

    unsafe {
        let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let rc = GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut psd,
        );
        assert_eq!(rc, 0, "GetSecurityInfo failed: {rc}");
        let mut sddl_ptr: *mut u16 = std::ptr::null_mut();
        let mut sddl_len: u32 = 0;
        let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
            psd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut sddl_ptr,
            &mut sddl_len,
        );
        assert_ne!(
            ok, 0,
            "ConvertSecurityDescriptorToStringSecurityDescriptorW failed"
        );
        let len = (0..).take_while(|&i| *sddl_ptr.add(i) != 0).count();
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(sddl_ptr, len));
        LocalFree(sddl_ptr as _);
        LocalFree(psd as _);
        s
    }
}

/// Round-trip an SDDL STRING through the converter pair to ITS canonical
/// form, so the expected side speaks the same well-known-SID-aliasing
/// dialect the actual side comes back in.
fn canonical_sddl(sddl: &str) -> String {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW,
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

    let wide_sddl = wide(sddl);
    unsafe {
        let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        assert_ne!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide_sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut psd,
                std::ptr::null_mut(),
            ),
            0,
            "string->SD failed for {sddl}"
        );
        let mut out_ptr: *mut u16 = std::ptr::null_mut();
        let mut out_len: u32 = 0;
        let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
            psd,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut out_ptr,
            &mut out_len,
        );
        assert_ne!(ok, 0, "SD->string failed for {sddl}");
        let len = (0..).take_while(|&i| *out_ptr.add(i) != 0).count();
        let out = String::from_utf16_lossy(std::slice::from_raw_parts(out_ptr, len));
        LocalFree(out_ptr as _);
        LocalFree(psd as _);
        out
    }
}

/// Open a raw handle to the voyage's pipe with `READ_CONTROL` for
/// security queries — bypassing `connect_voyage_pipe` (no reason to
/// expose its raw handle) since this is a test-only need.
fn open_pipe_handle(voyage_id: &str) -> windows_sys::Win32::Foundation::HANDLE {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, READ_CONTROL,
    };
    let name = wide(&format!(r"\\.\pipe\sot-voyage-{voyage_id}"));
    let h = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        h,
        INVALID_HANDLE_VALUE,
        "CreateFileW failed: {}",
        std::io::Error::last_os_error()
    );
    h
}

fn process_handle_count() -> u32 {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
    let mut count: u32 = 0;
    let ok = unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) };
    assert_ne!(
        ok,
        0,
        "GetProcessHandleCount failed: {}",
        std::io::Error::last_os_error()
    );
    count
}

/// One connect -> accept -> server-close -> confirmed-closed -> client
/// drop cycle, used by the churn/leak test.
fn churn_one(server: &PipeServer, id: &str) {
    let client = connect_voyage_pipe(id).unwrap();
    let conn_id = expect_accepted(server, TIMEOUT);
    server.close(conn_id);
    expect_closed(server, conn_id, TIMEOUT);
    drop(client);
}

/// Test 1: one server, one client, bytes both ways (accumulated); a
/// marker-tagged send's `Sent` event fires once its `WriteFile`
/// physically completes.
#[test]
fn server_and_client_exchange_bytes_and_sent_carries_marker() {
    if !run_isolated("server_and_client_exchange_bytes_and_sent_carries_marker") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 4).unwrap();
    let client = connect_voyage_pipe(&id).unwrap();
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

/// Test 2: two clients connected to the same voyage pipe are multiplexed
/// by distinct `ConnId`s.
#[test]
fn two_concurrent_clients_multiplexed_by_conn_id() {
    if !run_isolated("two_concurrent_clients_multiplexed_by_conn_id") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 4).unwrap();

    let client_a = connect_voyage_pipe(&id).unwrap();
    let conn_a = expect_accepted(&server, TIMEOUT);
    let client_b = connect_voyage_pipe(&id).unwrap();
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

/// Test 3: squat detection AND continuous name hold. With
/// `max_instances == 1`, several connect/close cycles run in a row; the
/// `FIRST_PIPE_INSTANCE` rival probe (using the SAME `nMaxInstances`,
/// round-3 finding 9) must fail EVERY time, including immediately after
/// each teardown. Only after `PipeServer::drop` does the probe succeed.
#[test]
fn rival_first_instance_create_fails_continuously_then_frees_on_drop() {
    if !run_isolated("rival_first_instance_create_fails_continuously_then_frees_on_drop") {
        return;
    }
    let id = fresh_voyage_id();
    let max_instances = 1;
    let server = PipeServer::bind(&id, max_instances).unwrap();

    for _ in 0..3 {
        assert_squat_check_failed(try_create_first_instance(&id, max_instances).unwrap_err());

        let client = connect_voyage_pipe(&id).unwrap();
        let conn_id = expect_accepted(&server, TIMEOUT);
        server.close(conn_id);
        assert_eq!(
            expect_closed(&server, conn_id, TIMEOUT),
            ClosedReason::Closed
        );
        drop(client);

        assert_squat_check_failed(try_create_first_instance(&id, max_instances).unwrap_err());
    }

    drop(server);
    try_create_first_instance(&id, max_instances)
        .unwrap_or_else(|e| panic!("expected the freed name to bind again: {e}"));
}

/// ADR 0041 step 6 U1b, Lifecycle "the pipe NAME disappears before any
/// blocking join" — Codex round-2b Blocker 1 discharge: `disconnect_listener`
/// ALONE — never `join_workers`, never `Drop` — must free the name for a
/// `FIRST_PIPE_INSTANCE` rival probe the INSTANT it returns, WHILE A
/// CONNECTION IS STILL LIVE, not only after it has already closed. The
/// client's own handle is left open throughout (never dropped before the
/// probe): the SERVER-side instance `disconnect_listener` closes is what a
/// squat probe actually checks for, so a still-open client handle must not
/// keep the name held. NO POLL LOOP inside `disconnect_listener` itself:
/// it closes the live connection's handle synchronously (round 1) and the
/// pending accept's own listening instance handle synchronously too
/// (round 2b — cancelling alone only REQUESTS cancellation, asynchronously;
/// closing the handle directly is what actually makes the instance stop
/// existing) — a poll THERE would silently tolerate exactly the residual
/// delay this round's fix exists to remove.
///
/// Codex round-3 test-premise-gap fix: `max_instances = 2` means a SECOND
/// instance becomes the accept loop's own pending `ConnectNamedPipe` the
/// moment the first connection is accepted, but the accept loop reaching
/// that point is a RACE against this test thread — the previous version
/// of this test called `disconnect_listener` right after `expect_accepted`
/// with no guarantee that race had resolved, so it exercised live-
/// connection closure but did NOT deterministically prove a pending
/// accept handle existed at teardown too (the actual defect this test
/// exists to catch — Codex round-3 finding 1).
///
/// Codex round-4 finding 3: `AcceptState::current` alone is populated
/// BEFORE `ConnectNamedPipe` is ever issued, so polling it cannot prove
/// submission happened at all — and a separate "poll, then call
/// `disconnect_listener`" pair leaves a TOCTOU gap between the two calls.
///
/// Codex round-5 fix 2a/2b/2c: plain `SlotState::Pending` is ALSO set for
/// a synchronously-completed op still awaiting result collection, so
/// even polling THAT is not proof of a genuine `ERROR_IO_PENDING`
/// submission — and merely being in one function does not itself close
/// a TOCTOU between a pre-check and a later act.
/// `assert_accept_parked_then_disconnect_listener_for_test` polls the
/// accept slot's OWN genuine-async-pending signal (set only when `issue`
/// actually returns `ERROR_IO_PENDING`) purely to decide WHEN to call
/// `disconnect_listener`, then returns the TOCTOU-free LATCH that
/// `disconnect_listener`'s own synchronized cancellation records at the
/// exact instant it cancels — the proof and the act share one critical
/// section, so nothing can go stale in between.
#[test]
fn disconnect_listener_frees_the_name_even_with_a_live_connection() {
    if !run_isolated("disconnect_listener_frees_the_name_even_with_a_live_connection") {
        return;
    }
    let id = fresh_voyage_id();
    let max_instances = 2;
    let mut server = PipeServer::bind(&id, max_instances).unwrap();

    // A LIVE connection: never closed by either side before the probe.
    let client = connect_voyage_pipe(&id).unwrap();
    expect_accepted(&server, TIMEOUT);

    assert!(
        server.assert_accept_parked_then_disconnect_listener_for_test(TIMEOUT),
        "expected a second pending accept instance (max_instances=2) to be genuinely parked \
         (ConnectNamedPipe issued) before teardown"
    );

    try_create_first_instance(&id, max_instances)
        .unwrap_or_else(|e| panic!("expected the name to be immediately winnable: {e}"));
    drop(client);
    drop(server);
}

/// ADR 0041 step 6 U1b, acceptance matrix "teardown composes": real
/// worker fan-out (several live connections torn down at once, none of
/// them having disconnected on their own) completes well inside the
/// pinned aggregate deadline — proven against a budget a full order of
/// magnitude smaller than [`TEARDOWN_AGGREGATE_DEADLINE`], which is
/// exactly the margin that constant claims to have.
#[test]
fn worst_case_worker_fan_out_completes_well_inside_the_aggregate_budget() {
    if !run_isolated("worst_case_worker_fan_out_completes_well_inside_the_aggregate_budget") {
        return;
    }
    assert!(Duration::from_secs(5) < TEARDOWN_AGGREGATE_DEADLINE);
    let id = fresh_voyage_id();
    let max_instances = 8;
    let mut server = PipeServer::bind(&id, max_instances).unwrap();

    let mut clients = Vec::new();
    for _ in 0..max_instances {
        let client = connect_voyage_pipe(&id).unwrap();
        expect_accepted(&server, TIMEOUT);
        clients.push(client); // every connection stays LIVE -- worst case
    }

    server.disconnect_listener();
    let started = Instant::now();
    let ok = server.join_workers(started + Duration::from_secs(5));
    assert!(
        ok,
        "real teardown of {max_instances} live connections did not finish within a 5s budget \
         (took at least {:?})",
        started.elapsed()
    );
    drop(clients);
}

/// ADR 0041 step 6 U1b, acceptance matrix "teardown composes" — Codex
/// round-1 Blocker 3 discharge: a GENUINELY STALLED connection worker
/// (one whose peer never drains, never closes, and whose own I/O the
/// server side cannot otherwise unstick) must not prevent the OTHER
/// connections from being cancelled and torn down, and the AGGREGATE
/// join must still resolve (loud on expiry) within a small bound rather
/// than hanging on the one stuck worker. `flooded_never_reading_client_
/// close_completes_within_bound` (below) already proves a single
/// never-reading watcher's OWN close completes bounded; this test proves
/// the WHOLE-SERVER teardown composes the same way when one connection
/// is stalled and several healthy ones are live alongside it.
#[test]
fn stalled_worker_does_not_block_teardown_of_healthy_connections() {
    if !run_isolated("stalled_worker_does_not_block_teardown_of_healthy_connections") {
        return;
    }
    let id = fresh_voyage_id();
    let max_instances = 4;
    let mut server = PipeServer::bind(&id, max_instances).unwrap();

    // One connection whose CLIENT never reads and never writes again --
    // outbound bytes queued for it will sit until the server side closes
    // the handle out from under it. A few healthy connections alongside
    // it, established BEFORE the final pending-at-teardown proof below
    // (Codex round-5 fix 3), so the whole scenario is real by the time
    // that proof runs.
    let stalled_client = connect_voyage_pipe(&id).unwrap();
    let stalled_conn = expect_accepted(&server, TIMEOUT);
    // Codex round-3 test-premise-gap fix: a single 4 KiB send into a pipe
    // configured with 64 KiB buffers never actually stalls -- flood until
    // the outbound budget genuinely reports full (the SAME pattern
    // `flooded_never_reading_client_close_completes_within_bound` uses),
    // proving the writer thread has real in-flight/backed-up work when
    // teardown begins, not an idle connection.
    let payload = vec![0xABu8; 65_536];
    let mut saw_full = false;
    for _ in 0..128 {
        match server.send(stalled_conn, payload.clone(), None) {
            Ok(()) => {}
            Err(PipeError::QueueFull(cid)) => {
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
        let c = connect_voyage_pipe(&id).unwrap();
        expect_accepted(&server, TIMEOUT);
        healthy_clients.push(c);
    }

    // Codex round-4 finding 3 / round-5 finding 2: `QueueFull` alone only
    // proves the outbound BYTE budget is reserved, and plain
    // `SlotState::Pending` is ALSO set for a synchronously-completed
    // write still awaiting result collection -- neither proves the
    // writer thread has reached a GENUINE `ERROR_IO_PENDING` `WriteFile`.
    // This poll is a best-effort PRE-check deciding WHEN it is worth
    // proceeding to teardown; it is NOT the proof (ignoring a timeout
    // here just means the fused proof below will legitimately fail
    // instead, with a clearer message about what was actually observed).
    let _ = server.conn_write_pending_for_test(stalled_conn, TIMEOUT);

    // Codex round-5 fix 2b/2c/3: fuse the proof with the act. Real
    // Windows CI diagnosis (this round): relying on `close_all`'s
    // `CloseHandle` alone to unstick a write genuinely stalled on full-
    // buffer backpressure was NOT observed to complete within this
    // test's 5s teardown budget -- `disconnect_listener` now issues an
    // explicit `CancelIoEx` per connection FIRST, which is what actually
    // and promptly unsticks it; the assert below reads the TOCTOU-free
    // latch that SAME cancellation recorded, not a stale pre-check.
    server.disconnect_listener();
    assert_eq!(
        server.conn_write_was_genuinely_pending_at_teardown_for_test(stalled_conn),
        Some(true),
        "expected the stalled connection's writer to be GENUINELY pending (ERROR_IO_PENDING) \
         at the exact instant disconnect_listener cancelled it"
    );

    let started = Instant::now();
    // A budget an order of magnitude under the pinned 20s: teardown must
    // not need to wait out the stalled connection's own I/O at all --
    // cancelling then closing its handle (disconnect_listener, above) is
    // what unsticks it, so healthy AND stalled connections alike tear
    // down promptly.
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

/// ADR 0041 step 6 U1b, acceptance matrix "teardown composes" — Codex
/// round-1 Blocker 3 discharge, "total-deadline propagation": the shared
/// deadline is REAL and honored against REAL OS threads, not merely a
/// number `join_within`'s own pure unit tests exercise against fully
/// controlled fake threads. `disconnect_listener`'s own redesign makes a
/// worker un-unstickable by ORDINARY means hard to construct (closing
/// every handle is specifically what unsticks them) — so this proves
/// deadline ENFORCEMENT itself is real and workload-independent: an
/// essentially-zero budget against otherwise-healthy, real connections
/// still returns promptly (never hangs out to the pinned 20s), and the
/// natural race (some threads may still finish before the first
/// `is_finished` poll) means this asserts BOUNDED total time, not a
/// specific `true`/`false` outcome — either is legitimate, a HANG is not.
#[test]
fn join_workers_deadline_is_enforced_against_real_threads_not_merely_computed() {
    if !run_isolated("join_workers_deadline_is_enforced_against_real_threads_not_merely_computed") {
        return;
    }
    let id = fresh_voyage_id();
    let max_instances = 4;
    let mut server = PipeServer::bind(&id, max_instances).unwrap();

    let mut clients = Vec::new();
    for _ in 0..3 {
        let c = connect_voyage_pipe(&id).unwrap();
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

/// Test 4: the pipe's own security descriptor, queried on a LIVE HANDLE
/// via `GetSecurityInfo` — protected, owner-only full access, NO `OI`/`CI`
/// inheritance flags.
#[test]
fn pipe_descriptor_is_protected_owner_only_with_no_container_inherit_flags() {
    if !run_isolated("pipe_descriptor_is_protected_owner_only_with_no_container_inherit_flags") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 1).unwrap();

    let handle = open_pipe_handle(&id);
    let sid = current_user_sid_string();
    let expected = canonical_sddl(&format!("D:P(A;;FA;;;{sid})"));
    let actual = security_descriptor_sddl(handle);
    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };

    assert_eq!(actual, expected);
    assert!(
        !actual.contains("OICI"),
        "pipe descriptor must carry no OI/CI flags: {actual}"
    );

    drop(server);
}

/// Test 5: a client that connects and never reads while the server floods
/// it. The outbound BYTE budget eventually reports full once the
/// head-of-line `WriteFile` is stuck in the kernel; `close` is
/// fire-and-forget, so the bound under test is how promptly the `Closed`
/// event follows.
#[test]
fn flooded_never_reading_client_close_completes_within_bound() {
    if !run_isolated("flooded_never_reading_client_close_completes_within_bound") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();
    let client = connect_voyage_pipe(&id).unwrap(); // deliberately never reads
    let conn_id = expect_accepted(&server, TIMEOUT);

    let payload = vec![0xABu8; 65_536];
    let mut saw_full = false;
    for _ in 0..128 {
        match server.send(conn_id, payload.clone(), None) {
            Ok(()) => {}
            Err(PipeError::QueueFull(cid)) => {
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

/// Test 6: a pending accept with no client ever connecting — server drop
/// must return promptly.
#[test]
fn pending_accept_with_no_client_drops_promptly() {
    if !run_isolated("pending_accept_with_no_client_drops_promptly") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 1).unwrap();
    drop(server);
}

/// New coverage (drop-vs-lifecycle-delivery regression): saturate the
/// events channel and never drain it, then drop the server. `Drop` must
/// still return -- it MUST NOT deadlock behind its own
/// `send_lifecycle_event` escape by joining the accept thread before
/// setting `dropping` (the accept thread's own `Accepted`/`AcceptError`
/// publishes can be stuck retrying against the very saturation this test
/// creates).
#[test]
fn drop_returns_even_with_a_saturated_events_channel() {
    if !run_isolated("drop_returns_even_with_a_saturated_events_channel") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();

    // Churn connections without ever draining events() -- each churn
    // queues at least Accepted + Closed(Eof), so a generous number of
    // attempts guarantees the channel fills well past its capacity, at
    // which point the accept thread itself is blocked delivering an
    // Accepted through send_lifecycle_event's retry loop.
    for _ in 0..200 {
        match connect_voyage_pipe(&id) {
            Ok(client) => drop(client),
            Err(_) => break, // the accept side is now saturated/stalled -- as intended
        }
    }

    // Must return even though nobody ever drained events().
    drop(server);
}

/// Test 7: invalid voyage ids and out-of-range instance counts are
/// rejected loudly. Provably non-wedging (every case fails before any
/// Win32 I/O call), so this test is NOT process-isolated.
#[test]
fn invalid_voyage_ids_and_instance_counts_are_rejected_loudly() {
    let bad_ids = [
        "../../../etc/passwd",
        "not-a-uuid",
        "550E8400-E29B-41D4-A716-446655440000",   // uppercase
        "550e8400e29b41d4a716446655440000",       // no hyphens ("simple" form)
        "550e8400-e29b-41d4-a716-44665544000",    // one hex digit short
        "{550e8400-e29b-41d4-a716-446655440000}", // braced GUID form
        "",
    ];
    for bad in bad_ids {
        let err = PipeServer::bind(bad, 1).unwrap_err();
        assert!(
            matches!(err, PipeError::InvalidVoyageId(_)),
            "id {bad:?}: got {err}"
        );
        let err2 = connect_voyage_pipe(bad).unwrap_err();
        assert!(
            matches!(err2, PipeError::InvalidVoyageId(_)),
            "id {bad:?}: got {err2}"
        );
    }

    try_create_first_instance("not-a-uuid", 1)
        .unwrap_or_else(|e| panic!("expected a rejected id to create no pipe at all: {e}"));

    let id = fresh_voyage_id();
    assert!(matches!(
        PipeServer::bind(&id, 0).unwrap_err(),
        PipeError::InvalidMaxInstances
    ));
    assert!(matches!(
        PipeServer::bind(&id, 256).unwrap_err(),
        PipeError::InvalidMaxInstances
    ));
}

/// Test 8: `close` on the server side gives the client an ordered EOF;
/// dropping a client gives the server a `Closed(Eof)`.
#[test]
fn server_close_yields_client_eof_and_client_drop_yields_server_closed() {
    if !run_isolated("server_close_yields_client_eof_and_client_drop_yields_server_closed") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();

    let client_a = connect_voyage_pipe(&id).unwrap();
    let conn_a = expect_accepted(&server, TIMEOUT);
    server.close(conn_a);
    let mut buf = [0u8; 16];
    let n = client_a.read(&mut buf).unwrap();
    assert_eq!(n, 0, "expected ordered EOF after a server-initiated close");
    assert_eq!(
        expect_closed(&server, conn_a, TIMEOUT),
        ClosedReason::Closed
    );

    let client_b = connect_voyage_pipe(&id).unwrap();
    let conn_b = expect_accepted(&server, TIMEOUT);
    drop(client_b);
    assert_eq!(expect_closed(&server, conn_b, TIMEOUT), ClosedReason::Eof);

    drop(server);
}

/// Test 9 (PRIMARY, deterministic — round-3 finding 9): the client
/// connects, the test waits for `Accepted` (proving registration
/// definitely happened) BEFORE closing, then asserts `Closed(Eof)`. The
/// instant-close race itself is a separate smoke test below.
#[test]
fn eof_before_registration_is_handled_cleanly() {
    if !run_isolated("eof_before_registration_is_handled_cleanly") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();

    let client = connect_voyage_pipe(&id).unwrap();
    let conn_id = expect_accepted(&server, TIMEOUT); // synchronize FIRST
    drop(client); // now close, after registration is proven

    assert_eq!(expect_closed(&server, conn_id, TIMEOUT), ClosedReason::Eof);

    let client2 = connect_voyage_pipe(&id).unwrap();
    let _ = expect_accepted(&server, TIMEOUT);
    drop(client2);

    drop(server);
}

/// Test 9b (smoke test, round-3 finding 9): a client that connects and
/// disconnects with NO synchronization at all is a genuine race with
/// `ConnectNamedPipe`'s own completion. Two outcomes are both honest: the
/// accept loop's own connect-error handling registers the connection
/// anyway (see `accept_loop`'s `match connect_result`), producing
/// `Accepted` then `Closed(Eof)`; OR Windows reports a "nobody ever
/// connected" condition (the `ERROR_NO_DATA` family, per Microsoft's
/// `ConnectNamedPipe` documentation) before the accept thread even
/// issues the call, producing NO event for this attempt at all. Neither
/// is a bug — this test only proves the race never wedges anything and
/// never poisons the pipe for the next client.
#[test]
fn eof_before_registration_smoke_test_accepts_either_honest_outcome() {
    if !run_isolated("eof_before_registration_smoke_test_accepts_either_honest_outcome") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();

    let client = connect_voyage_pipe(&id).unwrap();
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

    // Whichever happened, the pipe must still be healthy.
    let client2 = connect_voyage_pipe(&id).unwrap();
    let _ = expect_accepted(&server, TIMEOUT);
    drop(client2);

    drop(server);
}

/// Test 10 (round-3 finding 8): sequential connect/close churn must not
/// grow this process's OS handle count without bound. Isolated so
/// `GetProcessHandleCount` is not confounded by other tests running
/// concurrently; the slack is small precisely because isolation removes
/// that confound.
#[test]
fn sequential_connect_close_churn_does_not_leak_handles() {
    if !run_isolated("sequential_connect_close_churn_does_not_leak_handles") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 4).unwrap();

    for _ in 0..5 {
        churn_one(&server, &id);
    }

    let before = process_handle_count();
    for _ in 0..50 {
        churn_one(&server, &id);
    }
    let after = process_handle_count();

    assert!(
        after <= before + 6,
        "handle count grew from {before} to {after} across 50 connect/close cycles in isolation -- suspected leak"
    );

    drop(server);
}

/// New coverage (round-3 finding 9): a `PipeClient::read` blocked on one
/// thread is unblocked by `cancel()` called from another, returning
/// `PipeError::Cancelled`.
#[test]
fn client_read_cancel_unblocks_from_another_thread() {
    if !run_isolated("client_read_cancel_unblocks_from_another_thread") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();
    let client = Arc::new(connect_voyage_pipe(&id).unwrap());
    let _conn_id = expect_accepted(&server, TIMEOUT);

    let reader_client = Arc::clone(&client);
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        reader_client.read(&mut buf) // blocks -- the server never sends anything
    });

    std::thread::sleep(Duration::from_millis(300)); // let the read actually become Pending
    client.cancel();

    let result = reader.join().unwrap();
    assert!(
        matches!(result, Err(PipeError::Cancelled)),
        "expected Cancelled, got {result:?}"
    );

    drop(server);
}

/// New coverage (round-3 finding 9): a `PipeClient::write_all` blocked on
/// one thread (the pipe's kernel buffer saturated because nobody drains
/// it) is unblocked by `cancel()` called from another.
#[test]
fn client_write_cancel_unblocks_from_another_thread() {
    if !run_isolated("client_write_cancel_unblocks_from_another_thread") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();
    let client = Arc::new(connect_voyage_pipe(&id).unwrap());
    let _conn_id = expect_accepted(&server, TIMEOUT);
    // Deliberately never drain `server.events()` from here on -- that is
    // what eventually stalls the server's reader (see `deliver_bytes`)
    // and lets the raw pipe buffer fill up behind it, giving the
    // client's own `write_all` something real to block on.

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

    std::thread::sleep(Duration::from_secs(2)); // let the flood saturate the events channel + pipe buffer
    client.cancel();

    let result = writer.join().unwrap();
    assert!(
        matches!(result, PipeError::Cancelled),
        "expected Cancelled, got {result:?}"
    );

    drop(server);
}

/// New coverage (round-3 finding 1): once the events channel saturates
/// and stays that way past the `Bytes` abandon bound, the reader force-
/// closes the connection and a `Closed` is GUARANTEED to eventually
/// appear in the backlog once drained — never a silent stream gap.
#[test]
fn event_channel_saturation_abandons_bytes_and_guarantees_closed() {
    if !run_isolated("event_channel_saturation_abandons_bytes_and_guarantees_closed") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();
    let client = connect_voyage_pipe(&id).unwrap();
    let conn_id = expect_accepted(&server, TIMEOUT);

    let flooder = std::thread::spawn(move || {
        let payload = vec![0xEFu8; 65_536];
        loop {
            if client.write_all(&payload).is_err() {
                return; // expected once the connection is torn down under it
            }
        }
    });

    // Let the reader saturate the events channel and hit its abandon
    // bound WITHOUT this test draining anything -- that stall is exactly
    // what proves the guarantee (pipe_win.rs's own BYTES_ABANDON_AFTER is
    // 5s; wait well past it).
    std::thread::sleep(Duration::from_secs(8));

    let mut saw_closed = false;
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        match server.events().recv_timeout(Duration::from_secs(1)) {
            Ok(TransportEvent::Closed(cid, _)) if cid == conn_id => {
                saw_closed = true;
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert!(
        saw_closed,
        "expected a guaranteed Closed after Bytes abandonment"
    );

    let _ = flooder.join();
    drop(server);
}

/// New coverage (round-3 finding 2/9): a SECOND concurrent same-direction
/// `PipeClient::read` returns `PipeError::ConcurrentSubmit` rather than
/// racing the first caller's `OVERLAPPED`.
#[test]
fn concurrent_same_direction_client_read_returns_distinct_error() {
    if !run_isolated("concurrent_same_direction_client_read_returns_distinct_error") {
        return;
    }
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();
    let client = Arc::new(connect_voyage_pipe(&id).unwrap());
    let _conn_id = expect_accepted(&server, TIMEOUT);

    let a = Arc::clone(&client);
    let reader_a = std::thread::spawn(move || {
        let mut buf = [0u8; 16];
        a.read(&mut buf) // blocks -- nobody ever sends
    });

    std::thread::sleep(Duration::from_millis(300)); // let A's read actually become Pending

    let mut buf_b = [0u8; 16];
    let result_b = client.read(&mut buf_b);
    assert!(
        matches!(result_b, Err(PipeError::ConcurrentSubmit)),
        "expected ConcurrentSubmit, got {result_b:?}"
    );

    client.cancel();
    let result_a = reader_a.join().unwrap();
    assert!(
        matches!(result_a, Err(PipeError::Cancelled)),
        "expected Cancelled, got {result_a:?}"
    );

    drop(server);
}

// Not exercised (round-3 finding 9's explicit "don't invent a seam"
// guidance): `thread::Builder::spawn` failure injection (no seam exists
// to force it deterministically) and `TransportEvent::AcceptError`
// (every path to it is a genuine OS resource exhaustion this test suite
// has no deterministic way to trigger).

// ---------------------------------------------------------------------
// ADR 0041 U0 round-1: the same-connection challenge's real-pipe tests
// (moved here from `src/challenge.rs`'s own unit tests -- security-
// sensitive Windows I/O belongs in a named integration target the
// windows-2022 job runs, not in the crate's own `--lib` test binary).
// Deadlines are GENEROUS (30s), not the tight 5s the original in-crate
// tests used, so two-core CI contention can't flake a genuinely correct
// challenge into a spurious `Undetermined` (round-1 review, "replace
// independent real deadlines with generous ones").
// ---------------------------------------------------------------------

fn self_pid_and_created() -> (u32, u64) {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetProcessTimes};
    unsafe {
        let pid = GetCurrentProcessId();
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        assert_ne!(GetProcessTimes(GetCurrentProcess(), &mut creation, &mut exit, &mut kernel, &mut user), 0);
        let created = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        (pid, created)
    }
}

/// The `status` request has no body, so its ENCODED length alone is what
/// we wait for; the pipe is byte-type, so a single write is not
/// guaranteed to surface as a single `Bytes` event.
fn await_status_request(server: &PipeServer, conn_id: ConnId, timeout: Duration) {
    let expected = wire::encode_mgmt_request(&MgmtRequest::Status).unwrap();
    let mut got = Vec::new();
    let deadline = Instant::now() + timeout;
    while got.len() < expected.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for the status request");
        match server.events().recv_timeout(remaining) {
            Ok(TransportEvent::Bytes(cid, bytes)) if cid == conn_id => got.extend(bytes),
            Ok(other) => panic!("unexpected event waiting for status: {other:?}"),
            Err(_) => panic!("timed out waiting for the status request"),
        }
    }
    assert_eq!(got, expected);
}

/// Real challenge, real pipe, SAME process on both ends — a genuine
/// same-user server, proven. Also this test's own shared "give me a real
/// proven process" helper for the two tests below it.
fn self_proven_challenge() -> ChallengeOutcome<sot_log::challenge::ChallengedProcess> {
    let voyage_id = fresh_voyage_id();
    let server = PipeServer::bind(&voyage_id, 1).expect("bind");
    let client = connect_voyage_pipe(&voyage_id).expect("connect");

    std::thread::scope(|scope| {
        let challenge_handle = scope.spawn(|| {
            let mut exchange = VoyageMgmtExchange::default();
            challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30))
        });

        let conn_id = expect_accepted(&server, TIMEOUT);
        await_status_request(&server, conn_id, TIMEOUT);
        let (pid, created) = self_pid_and_created();
        let reply = wire::encode_mgmt_reply(&MgmtReply::StatusOk { pid, created, survival: Survival::Normal }).unwrap();
        server.send(conn_id, reply, None).expect("send status_ok");

        challenge_handle.join().expect("challenge thread panicked")
    })
}

#[test]
fn challenge_proves_a_genuine_same_user_server() {
    if !run_isolated("challenge_proves_a_genuine_same_user_server") {
        return;
    }
    let (pid, created) = self_pid_and_created();
    match self_proven_challenge() {
        ChallengeOutcome::Proven(p) => {
            assert_eq!(p.pid(), pid);
            assert_eq!(p.created(), created);
        }
        other => panic!("expected Proven, got {other:?}"),
    }
}

#[test]
fn challenged_process_reverify_and_wait_reflect_a_live_self_proof() {
    if !run_isolated("challenged_process_reverify_and_wait_reflect_a_live_self_proof") {
        return;
    }
    let ChallengeOutcome::Proven(p) = self_proven_challenge() else {
        panic!("expected Proven")
    };
    assert!(p.reverify().unwrap());
    // Still running: this handle names our OWN test process.
    assert!(!p.wait(Duration::from_millis(50)).unwrap());
}

#[test]
fn challenge_rejects_a_pid_creation_mismatch_as_foreign() {
    if !run_isolated("challenge_rejects_a_pid_creation_mismatch_as_foreign") {
        return;
    }
    let voyage_id = fresh_voyage_id();
    let server = PipeServer::bind(&voyage_id, 1).expect("bind");
    let client = connect_voyage_pipe(&voyage_id).expect("connect");

    let outcome = std::thread::scope(|scope| {
        let challenge_handle = scope.spawn(|| {
            let mut exchange = VoyageMgmtExchange::default();
            challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30))
        });

        let conn_id = expect_accepted(&server, TIMEOUT);
        await_status_request(&server, conn_id, TIMEOUT);
        // A well-formed status_ok, but a FABRICATED pid/creation that
        // does not match the real server process (this test binary
        // itself) — the SID check upstream cannot catch this: same
        // account, wrong reply.
        let reply = wire::encode_mgmt_reply(&MgmtReply::StatusOk { pid: 1, created: 0, survival: Survival::Normal }).unwrap();
        server.send(conn_id, reply, None).expect("send status_ok");

        challenge_handle.join().expect("challenge thread panicked")
    });

    assert!(matches!(outcome, ChallengeOutcome::Foreign), "{outcome:?}");
}

/// Real deadline expiry, real pending `read`, real `conn.cancel()` — as
/// opposed to `challenge_classifies_connection_death_mid_challenge_as_undetermined`
/// below (an ordered EOF, a DIFFERENT path entirely). The server accepts
/// and receives `status`, but never replies and never closes: the
/// client's `read` is genuinely blocked until the watchdog's deadline
/// fires and `cancel()` unblocks it. Deterministic despite the short
/// deadline: the SERVER thread never does anything that could race it
/// (no reply, no close), so the only way this test can reach
/// `Undetermined` is via the cancellation path being exercised for real.
#[test]
fn challenge_cancels_a_genuinely_pending_read_when_the_deadline_expires() {
    if !run_isolated("challenge_cancels_a_genuinely_pending_read_when_the_deadline_expires") {
        return;
    }
    let voyage_id = fresh_voyage_id();
    let server = PipeServer::bind(&voyage_id, 1).expect("bind");
    let client = connect_voyage_pipe(&voyage_id).expect("connect");

    let outcome = std::thread::scope(|scope| {
        let challenge_handle = scope.spawn(|| {
            let mut exchange = VoyageMgmtExchange::default();
            // A short but real deadline -- the server below never replies,
            // so this can only resolve via the watchdog's own cancel.
            challenge(&client, &mut exchange, Instant::now() + Duration::from_millis(300))
        });

        let conn_id = expect_accepted(&server, TIMEOUT);
        await_status_request(&server, conn_id, TIMEOUT);
        // Deliberately never reply and never close -- the client's read
        // stays genuinely pending until the deadline cancels it.

        let outcome = challenge_handle.join().expect("challenge thread panicked");
        // Keep the server (and the connection) alive until the challenge
        // side has already concluded, so the server side is never what
        // ends the connection here.
        drop(server);
        outcome
    });

    assert!(matches!(outcome, ChallengeOutcome::Undetermined), "{outcome:?}");
}

#[test]
fn challenge_classifies_connection_death_mid_challenge_as_undetermined() {
    if !run_isolated("challenge_classifies_connection_death_mid_challenge_as_undetermined") {
        return;
    }
    let voyage_id = fresh_voyage_id();
    let server = PipeServer::bind(&voyage_id, 1).expect("bind");
    let client = connect_voyage_pipe(&voyage_id).expect("connect");

    let outcome = std::thread::scope(|scope| {
        let challenge_handle = scope.spawn(|| {
            let mut exchange = VoyageMgmtExchange::default();
            challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30))
        });

        let conn_id = expect_accepted(&server, TIMEOUT);
        await_status_request(&server, conn_id, TIMEOUT);
        // The server closes without ever answering `status`.
        server.close(conn_id);

        challenge_handle.join().expect("challenge thread panicked")
    });

    assert!(matches!(outcome, ChallengeOutcome::Undetermined), "{outcome:?}");
}

/// The "child" half of `cross_process_challenge_proves_a_real_child_server`
/// below: binds a real named pipe server for the voyage id named by
/// `PIPE_WIN_XPROC_VOYAGE_ID`, answers exactly one `status` request with
/// THIS PROCESS's own real pid/creation time, then exits. A normal test
/// pass never sets that env var, so this is a silent no-op then — the
/// parent test is the only thing that ever invokes this BY NAME with it
/// set, in a dedicated child process.
#[test]
fn cross_process_challenge_server_role() {
    let Ok(voyage_id) = std::env::var("PIPE_WIN_XPROC_VOYAGE_ID") else {
        return;
    };
    let server = PipeServer::bind(&voyage_id, 1).expect("server role: bind");
    let conn_id = expect_accepted(&server, Duration::from_secs(30));
    await_status_request(&server, conn_id, Duration::from_secs(30));
    let (pid, created) = self_pid_and_created();
    let reply = wire::encode_mgmt_reply(&MgmtReply::StatusOk { pid, created, survival: Survival::Normal }).unwrap();
    server.send(conn_id, reply, None).expect("server role: send status_ok");
}

/// The real cross-process test (ADR 0041 U0 round-1 required test):
/// `GetNamedPipeServerProcessId`, called on the CLIENT's own handle
/// (this process), must resolve to a GENUINELY DIFFERENT process's real
/// pid — closing the Microsoft-docs ambiguity Codex flagged (the API's
/// own parameter prose describes a handle from `CreateNamedPipe`, not a
/// client's `CreateFile` handle; Chromium's own client-side use is the
/// only precedent, not a documented guarantee). Every same-process test
/// above proves the PROTOCOL; only this one proves the OS call actually
/// crosses a real process boundary the way `challenge()` depends on.
#[test]
fn cross_process_challenge_proves_a_real_child_server() {
    if !run_isolated("cross_process_challenge_proves_a_real_child_server") {
        return;
    }
    let voyage_id = fresh_voyage_id();
    let exe = std::env::current_exe().expect("current_exe");
    let child = std::process::Command::new(&exe)
        .arg("--exact")
        .arg("cross_process_challenge_server_role")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env("PIPE_WIN_XPROC_VOYAGE_ID", &voyage_id)
        .env_remove("PIPE_WIN_TEST_CHILD")
        .spawn()
        .expect("failed to spawn the cross-process server child");
    let child_pid = child.id();

    struct KillGuard(Option<std::process::Child>);
    impl Drop for KillGuard {
        fn drop(&mut self) {
            if let Some(mut c) = self.0.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
    let mut guard = KillGuard(Some(child));

    // `connect_voyage_pipe`'s own ~2s internal retry on `FILE_NOT_FOUND`
    // absorbs the child's own startup race (it hasn't bound yet) -- no
    // extra synchronization needed.
    let client = connect_voyage_pipe(&voyage_id).expect("connect to the cross-process server");
    let mut exchange = VoyageMgmtExchange::default();
    let outcome = challenge(&client, &mut exchange, Instant::now() + Duration::from_secs(30));

    match outcome {
        ChallengeOutcome::Proven(p) => {
            assert_eq!(p.pid(), child_pid, "the challenged pid must be the REAL CHILD's, not our own");
            assert_ne!(p.pid(), std::process::id(), "a same-process pid here would prove nothing cross-process");
        }
        other => panic!("expected Proven against a real cross-process server, got {other:?}"),
    }

    // The child already answered and is expected to exit on its own;
    // reap it normally, then defuse the guard's own kill (a no-op by
    // then, kept only for the panic/early-return paths above).
    if let Some(c) = guard.0.as_mut() {
        let _ = c.wait();
    }
    guard.0 = None;
}

// ---------------------------------------------------------------------
// U1a: SID authentication enforcement in the shared `connect_voyage_pipe`
// constructor. Every step-5 client — mgmt or attach, tests and the e2e
// harness included — now gets steps 1-3 of ADR 0041's same-connection
// challenge (`challenge::authenticate_server`) as PART of connecting, not
// as a separate call the caller has to remember to make. This is
// DELIBERATELY WEAKER than the full five-step `challenge()` (Codex round-1
// Blocker 1): no reply round trip happens here, so no liveness/pid-binding
// proof is claimed at this layer — see `connect_voyage_pipe`'s own doc for
// the full reasoning and why the attach lane's `hello` still gets to be
// the connection's first frame.
// ---------------------------------------------------------------------

/// The pass case: against a genuine same-account server,
/// `connect_voyage_pipe`'s SID authentication is a transparent
/// pass-through — the connection it hands back is fully usable for the
/// caller's own intended protocol (an ordinary mgmt round trip here), not
/// merely "connected". This exercises the ENFORCED CONSTRUCTOR ITSELF
/// (`connect_voyage_pipe`, not `authenticate_server` or `challenge`
/// directly), so a regression that stopped calling `authenticate_server`
/// at all would still pass this test (nothing here proves enforcement
/// happened) -- that is exactly why the failure-mapping unit tests in
/// `pipe_win.rs` itself (`map_sid_auth_outcome`) exist alongside it: they
/// prove the CONSTRUCTOR's mapping logic in isolation, and this test
/// proves the happy path stays usable end to end.
#[test]
fn connect_voyage_pipe_sid_authentication_enforced_pass_against_a_genuine_server() {
    if !run_isolated("connect_voyage_pipe_sid_authentication_enforced_pass_against_a_genuine_server") {
        return;
    }
    let voyage_id = fresh_voyage_id();
    let server = PipeServer::bind(&voyage_id, 1).expect("bind");
    let client = connect_voyage_pipe(&voyage_id)
        .expect("SID-authentication-enforced connect must pass against a genuine same-account server");

    let conn_id = expect_accepted(&server, TIMEOUT);
    let probe = wire::encode_mgmt_request(&MgmtRequest::Probe).unwrap();
    client.write_all(&probe).expect("the connection must still be fully usable after authentication");
    let expected = wire::encode_mgmt_request(&MgmtRequest::Probe).unwrap();
    let got = accumulate_bytes(&server, conn_id, expected.len(), TIMEOUT);
    assert_eq!(got, expected);

    let reply = wire::encode_mgmt_reply(&MgmtReply::ProbeOk).unwrap();
    server.send(conn_id, reply.clone(), None).expect("send probe_ok");
    let mut buf = [0u8; 512];
    let n = client.read(&mut buf).expect("read probe_ok");
    assert_eq!(&buf[..n], reply.as_slice());
}

/// A `ChallengeableConnection` whose `raw_handle()` is deliberately
/// invalid (`INVALID_HANDLE_VALUE`, the exact value a failed `CreateFileW`
/// itself would have produced) — proves `authenticate_server`'s own
/// typed-failure surface with NO real pipe, process, or account boundary
/// needed: step 1 (`GetNamedPipeServerProcessId`) itself fails against
/// this handle, deterministically, on every run. `write_all`/`read`/
/// `cancel` are never reached (`authenticate_server` never touches
/// `IdentityExchange`, unlike the full `challenge()`) — `unreachable!()`
/// makes that a loud, checked assumption rather than a silent one.
///
/// A genuine WRONG-SID server (an other-user process winning the pipe
/// name first) is the ADR's own step-7 REAL-MACHINE acceptance row ("on a
/// real Windows machine, an OTHER-USER process pre-binds... every public
/// client entry point classifies it FOREIGN") — not constructible in CI
/// without a second real account, so it is deliberately not attempted
/// here; this test instead proves the OTHER typed failure — `Undetermined`
/// — that steps 1-3 alone can already produce.
struct InvalidHandleConn;
impl sot_log::challenge::ChallengeableConnection for InvalidHandleConn {
    fn raw_handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE
    }
    fn write_all(&self, _bytes: &[u8]) -> std::io::Result<()> {
        unreachable!("authenticate_server never sends a request")
    }
    fn read(&self, _buf: &mut [u8]) -> std::io::Result<usize> {
        unreachable!("authenticate_server never reads a reply")
    }
    fn cancel(&self) {
        unreachable!("authenticate_server never arms a reply watchdog")
    }
}

#[test]
fn authenticate_server_is_undetermined_when_step_one_itself_fails() {
    use sot_log::challenge::{authenticate_server, SidAuthOutcome};
    let outcome = authenticate_server(&InvalidHandleConn);
    assert!(matches!(outcome, SidAuthOutcome::Undetermined), "{outcome:?}");
}
