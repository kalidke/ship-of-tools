#![cfg(windows)]
//! Integration tests for the ADR 0041 step-5 pipe transport
//! (`src/pipe_win.rs`, unit U3 round 2 — discharges the Codex adversarial
//! review of round 1, including its eight-test audit). Lives in `tests/`
//! for the same structural reason `tests/conpty.rs` and
//! `tests/capsule_win.rs` do: this module's own types are `pub`
//! specifically so a real-pipe integration test can reach them.
//!
//! Every test drives a PLAIN ECHO/probe consumer against `PipeServer`'s
//! event stream and `PipeClient`'s blocking read/write — no capsule, no
//! wire-frame parsing.
//!
//! Two structural fixes the round-1 audit asked for, applied throughout:
//! - The pipe is byte-type, so one write is not guaranteed to arrive as
//!   one `Bytes` event — every test that checks received content
//!   accumulates bytes across events up to the expected length rather
//!   than asserting a one-write-one-event correspondence.
//! - `PipeServer::drop` (and, in round 1, the now-deleted blocking `close`)
//!   is the one call in this module that can still hang the whole test
//!   runner if a regression reintroduces an unbounded wait. Every such
//!   call here runs through [`assert_completes_within`], a watchdog-THREAD
//!   pattern (not a genuine child process): the call runs on a background
//!   thread while the test thread bounds it with `recv_timeout`, so a
//!   regression fails the one test loudly instead of wedging the runner —
//!   Rust's default test harness `process::exit`s after the run rather
//!   than joining threads, so a hung watchdog thread cannot outlive the
//!   test binary either. This discharges the review's actual concern
//!   ("a regression fails rather than wedging the runner") with far less
//!   machinery than re-invoking the test binary as a child process would
//!   need.

use sot_log::pipe_win::{connect_voyage_pipe, ClosedReason, ConnId, PipeError, PipeServer, TransportEvent};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A fresh, canonical lowercase-hyphenated UUID for one test's voyage id —
/// `now_v7` because that is the only generator feature this crate's `uuid`
/// dependency enables (`features = ["v7"]`); only the STRING SHAPE matters
/// to `pipe_win.rs`.
fn fresh_voyage_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Bounded wait for the next transport event — every test asserts one
/// arrives rather than letting a stuck server hang the whole suite.
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
/// accumulated (or `timeout` elapses) — the pipe is byte-type, so a single
/// write is not guaranteed to surface as a single `Bytes` event (round-1
/// audit finding).
fn accumulate_bytes(server: &PipeServer, conn_id: ConnId, expected_len: usize, timeout: Duration) -> Vec<u8> {
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    while out.len() < expected_len {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "only got {} of {expected_len} expected bytes: {out:?}", out.len());
        match next_event(server, remaining) {
            TransportEvent::Bytes(cid, bytes) => {
                assert_eq!(cid, conn_id, "Bytes for the wrong connection");
                out.extend(bytes);
            }
            other => panic!("expected Bytes, got {other:?}"),
        }
    }
    assert_eq!(out.len(), expected_len, "accumulated more than expected: {out:?}");
    out
}

/// Run `f` (typically a `drop(server)`/`drop(client)` call) on a
/// background thread and bound its completion — see the module doc's
/// watchdog-thread note.
fn assert_completes_within<T: Send + 'static>(what: &str, timeout: Duration, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = f();
        let _ = tx.send(());
        result
    });
    match rx.recv_timeout(timeout) {
        Ok(()) => handle.join().expect("watchdog-monitored thread panicked"),
        Err(_) => panic!("{what} did not complete within {timeout:?} -- watchdog fired"),
    }
}

/// NUL-terminated UTF-16, matching `pipe_win.rs`'s own private helper — a
/// test-local duplicate is simpler than exposing it.
fn wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Attempt to create the FIRST instance of `voyage_id`'s pipe name with
/// `FILE_FLAG_FIRST_PIPE_INSTANCE` — the squat-detection probe. Default
/// security (a null `SECURITY_ATTRIBUTES`) is fine: this probe is only
/// ever used either while `pipe_win.rs`'s own descriptor already owns the
/// name (proving the rival create fails) or on a name nothing else has
/// ever touched (proving it succeeds).
fn try_create_first_instance(voyage_id: &str) -> std::io::Result<()> {
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
            2,
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

/// This process's own token-user SID, stringified — independently derived
/// so a bug in `pipe_win.rs`'s or `fsutil.rs`'s own SID lookup could not
/// also hide from this test.
fn current_user_sid_string() -> String {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        assert_ne!(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token), 0);
        let mut needed: u32 = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        assert!(needed > 0, "GetTokenInformation sizing call returned zero length");
        let words = (needed as usize).div_ceil(8);
        let mut buf: Vec<u64> = vec![0u64; words];
        let buf_ptr = buf.as_mut_ptr().cast::<u8>();
        assert_ne!(GetTokenInformation(token, TokenUser, buf_ptr.cast(), needed, &mut needed), 0);
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
/// (Codex review, finding 11: named pipes are not among the documented
/// object types for the NAME-based `GetNamedSecurityInfoW`; Microsoft
/// directs named-pipe security queries through the HANDLE-based
/// `GetSecurityInfo` instead — this replaces round 1's name-based helper).
/// `handle` needs `READ_CONTROL` access, which `open_pipe_handle` below
/// requests explicitly.
fn security_descriptor_sddl(handle: windows_sys::Win32::Foundation::HANDLE) -> String {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
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
        assert_ne!(ok, 0, "ConvertSecurityDescriptorToStringSecurityDescriptorW failed");
        let len = (0..).take_while(|&i| *sddl_ptr.add(i) != 0).count();
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(sddl_ptr, len));
        LocalFree(sddl_ptr as _);
        LocalFree(psd as _);
        s
    }
}

/// Round-trip an SDDL STRING through the converter pair to ITS canonical
/// form, so the expected side speaks the same well-known-SID-aliasing
/// dialect the actual side comes back in (`voyage.rs`'s and round 1's
/// identical helper; same technique the coordinator asked this test to
/// reuse).
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

/// Open a raw handle to the voyage's pipe with `READ_CONTROL` for security
/// queries — deliberately bypassing `connect_voyage_pipe` (which has no
/// reason to expose its raw handle) since this is a test-only need, same
/// pattern as `try_create_first_instance`.
fn open_pipe_handle(voyage_id: &str) -> windows_sys::Win32::Foundation::HANDLE {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, READ_CONTROL};
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
    assert_ne!(h, INVALID_HANDLE_VALUE, "CreateFileW failed: {}", std::io::Error::last_os_error());
    h
}

fn process_handle_count() -> u32 {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
    let mut count: u32 = 0;
    let ok = unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) };
    assert_ne!(ok, 0, "GetProcessHandleCount failed: {}", std::io::Error::last_os_error());
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

/// Test 1: one server, one client, bytes both ways (accumulated, not
/// assumed to arrive as one `Bytes` event each); a marker-tagged send's
/// `Sent` event fires once its `WriteFile` physically completes.
#[test]
fn server_and_client_exchange_bytes_and_sent_carries_marker() {
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

    assert_completes_within("server drop", TIMEOUT, move || drop(server));
}

/// Test 2: two clients connected to the same voyage pipe are multiplexed
/// by distinct `ConnId`s (bytes accumulated per connection, not assumed
/// to arrive as one event each) — `connect_voyage_pipe`'s own bounded
/// retry absorbs the ordinary race against the accept loop posting the
/// SECOND instance's `ConnectNamedPipe` after the first hand-off.
#[test]
fn two_concurrent_clients_multiplexed_by_conn_id() {
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 4).unwrap();

    let client_a = connect_voyage_pipe(&id).unwrap();
    let conn_a = expect_accepted(&server, TIMEOUT);
    let client_b = connect_voyage_pipe(&id).unwrap();
    let conn_b = expect_accepted(&server, TIMEOUT);
    assert_ne!(conn_a, conn_b);

    client_a.write_all(b"from A").unwrap();
    client_b.write_all(b"from B").unwrap();

    // Events for the two connections may interleave; drain by connection.
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

    assert_completes_within("server drop", TIMEOUT, move || drop(server));
}

/// Test 3: squat detection AND continuous name hold (finding 7). With
/// `max_instances == 1` to stress the tightest case, several
/// connect/close cycles run in a row; the `FIRST_PIPE_INSTANCE` rival
/// probe must fail EVERY time, including immediately after each
/// teardown — the instance is disconnected-and-recycled, never actually
/// closed, so the OS-level instance count never touches zero while the
/// server lives. Only after `PipeServer::drop` does the probe succeed.
#[test]
fn rival_first_instance_create_fails_continuously_then_frees_on_drop() {
    use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;

    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 1).unwrap();

    for _ in 0..3 {
        let err = try_create_first_instance(&id).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32), "unexpected squat-check error: {err}");

        let client = connect_voyage_pipe(&id).unwrap();
        let conn_id = expect_accepted(&server, TIMEOUT);
        server.close(conn_id);
        assert_eq!(expect_closed(&server, conn_id, TIMEOUT), ClosedReason::Closed);
        drop(client);

        // Immediately after teardown: recycled, not closed. The window
        // round 1's test 3 missed must not exist.
        let err = try_create_first_instance(&id).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32), "squat window reopened after teardown");
    }

    assert_completes_within("server drop", TIMEOUT, move || drop(server));
    try_create_first_instance(&id).unwrap_or_else(|e| panic!("expected the freed name to bind again: {e}"));
}

/// Test 4: the pipe's own security descriptor, queried on a LIVE HANDLE
/// via `GetSecurityInfo` (finding 11) — protected, owner-only full
/// access, and (unlike the voyage-tree directory descriptor) NO `OI`/`CI`
/// inheritance flags, since a pipe object has no children to inherit
/// onto. The expected SDDL is the CORRECT six-field ACE form
/// (`D:P(A;;FA;;;<sid>)` — empty flags field, not an omitted one; see
/// `fsutil.rs`'s `owner_protected_descriptor_with_ace` doc for the
/// five-field bug this shape fixes), round-tripped through the same
/// converter so a regression in either direction is a string mismatch,
/// not a parse failure at `bind` time.
#[test]
fn pipe_descriptor_is_protected_owner_only_with_no_container_inherit_flags() {
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 1).unwrap();

    let handle = open_pipe_handle(&id);
    let sid = current_user_sid_string();
    let expected = canonical_sddl(&format!("D:P(A;;FA;;;{sid})"));
    let actual = security_descriptor_sddl(handle);
    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };

    assert_eq!(actual, expected);
    assert!(!actual.contains("OICI"), "pipe descriptor must carry no OI/CI flags: {actual}");

    assert_completes_within("server drop", TIMEOUT, move || drop(server));
}

/// Test 5: a client that connects and never reads while the server floods
/// it. The outbound BYTE budget eventually reports full once the
/// head-of-line `WriteFile` itself is stuck in the kernel (the pipe's own
/// 64 KiB buffer also fills). `close` is fire-and-forget (finding 6): it
/// cannot itself hang, so the bound under test is how promptly the
/// `Closed` event follows — proving `IoSlot::cancel` actually aborted the
/// otherwise-eternal write — and, separately, that dropping the server
/// afterward (which joins every remaining thread) is itself prompt.
#[test]
fn flooded_never_reading_client_close_completes_within_bound() {
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
    assert!(saw_full, "expected the outbound budget to report full against a non-reading peer");

    server.close(conn_id);
    assert_eq!(expect_closed(&server, conn_id, TIMEOUT), ClosedReason::Closed);

    assert_completes_within("server drop after a stuck write", TIMEOUT, move || drop(server));
    drop(client);
}

/// Test 6: a pending accept with no client ever connecting — server drop
/// must return promptly (the accept thread's blocked `ConnectNamedPipe`
/// is cancelled via `IoSlot`, not merely abandoned) rather than hang the
/// runner (watchdog-bounded per the module doc).
#[test]
fn pending_accept_with_no_client_drops_promptly() {
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 1).unwrap();
    assert_completes_within("drop with a pending, client-less accept", TIMEOUT, move || drop(server));
}

/// Test 7: invalid voyage ids — path-traversal shapes, wrong length,
/// uppercase, hyphen-less "simple" form, braced form, and empty — are all
/// refused loudly by both `bind` and `connect_voyage_pipe`, and `bind`
/// never gets far enough to create anything under the rejected name.
/// Also covers `max_instances`'s documented `1..=255` range (finding 7).
#[test]
fn invalid_voyage_ids_and_instance_counts_are_rejected_loudly() {
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
        let err = PipeServer::bind(bad, 1).unwrap_err();
        assert!(matches!(err, PipeError::InvalidVoyageId(_)), "id {bad:?}: got {err}");
        let err2 = connect_voyage_pipe(bad).unwrap_err();
        assert!(matches!(err2, PipeError::InvalidVoyageId(_)), "id {bad:?}: got {err2}");
    }

    // "creates nothing": if `bind` had gotten as far as `CreateNamedPipeW`
    // for a rejected id, that name would already be squatted.
    try_create_first_instance("not-a-uuid").unwrap_or_else(|e| panic!("expected a rejected id to create no pipe at all: {e}"));

    let id = fresh_voyage_id();
    assert!(matches!(PipeServer::bind(&id, 0).unwrap_err(), PipeError::InvalidMaxInstances));
    assert!(matches!(PipeServer::bind(&id, 256).unwrap_err(), PipeError::InvalidMaxInstances));
}

/// Test 8: `close` on the server side gives the client an ordered EOF
/// (ADR 0041: "there is no `detach` op — ordered pipe EOF is detach");
/// dropping a client gives the server a `Closed(Eof)`.
#[test]
fn server_close_yields_client_eof_and_client_drop_yields_server_closed() {
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();

    // Server-initiated close -> client observes ordered EOF, server itself
    // reports `Closed`.
    let client_a = connect_voyage_pipe(&id).unwrap();
    let conn_a = expect_accepted(&server, TIMEOUT);
    server.close(conn_a);
    let mut buf = [0u8; 16];
    let n = client_a.read(&mut buf).unwrap();
    assert_eq!(n, 0, "expected ordered EOF after a server-initiated close");
    assert_eq!(expect_closed(&server, conn_a, TIMEOUT), ClosedReason::Closed);

    // Client drop -> server's OWN reader observes the disconnect and
    // reports `Closed(Eof)` without any `close` call.
    let client_b = connect_voyage_pipe(&id).unwrap();
    let conn_b = expect_accepted(&server, TIMEOUT);
    drop(client_b);
    assert_eq!(expect_closed(&server, conn_b, TIMEOUT), ClosedReason::Eof);

    assert_completes_within("server drop", TIMEOUT, move || drop(server));
}

/// New coverage (finding 4 / round-1 audit miss): a client that connects
/// and disconnects IMMEDIATELY — before the server-side start gate could
/// plausibly have opened yet — must still be cleanly `Accepted` then
/// `Closed(Eof)`, with its instance recycled rather than leaked (proven by
/// a subsequent connect succeeding immediately).
#[test]
fn eof_before_registration_is_handled_cleanly() {
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();

    let client = connect_voyage_pipe(&id).unwrap();
    drop(client);

    let conn_id = expect_accepted(&server, TIMEOUT);
    assert_eq!(expect_closed(&server, conn_id, TIMEOUT), ClosedReason::Eof);

    let client2 = connect_voyage_pipe(&id).unwrap();
    let _ = expect_accepted(&server, TIMEOUT);
    drop(client2);

    assert_completes_within("server drop", TIMEOUT, move || drop(server));
}

/// New coverage (finding 5 / round-1 audit miss): sequential connect/close
/// churn must not grow this process's OS handle count without bound — the
/// reaper joins and recycles every connection immediately rather than
/// accumulating retired threads or handles.
#[test]
fn sequential_connect_close_churn_does_not_leak_handles() {
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 4).unwrap();

    // Warm-up: let the instance pool and any one-time allocations settle
    // before taking the baseline measurement.
    for _ in 0..5 {
        churn_one(&server, &id);
    }

    let before = process_handle_count();
    for _ in 0..50 {
        churn_one(&server, &id);
    }
    let after = process_handle_count();

    assert!(
        after <= before + 20,
        "handle count grew from {before} to {after} across 50 connect/close cycles -- suspected leak"
    );

    assert_completes_within("server drop", TIMEOUT, move || drop(server));
}
