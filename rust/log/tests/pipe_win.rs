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

use sot_log::pipe_win::{
    connect_voyage_pipe, ClosedReason, ConnId, PipeError, PipeServer, TransportEvent,
};
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
