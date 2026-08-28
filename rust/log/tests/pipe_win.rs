#![cfg(windows)]
//! Integration tests for the ADR 0041 step-5 pipe transport
//! (`src/pipe_win.rs`, unit U3 round 1 — TRANSPORT ONLY, no capsule). Lives
//! in `tests/` for the same structural reason `tests/conpty.rs` and
//! `tests/capsule_win.rs` do: this module's own types are `pub`
//! specifically so a real-pipe integration test can reach them, and an
//! integration test binary is the only kind of test that gets its own
//! separate process (needed here for `server`/`client` values that must
//! genuinely cross an OS pipe rather than share memory).
//!
//! Every test drives a PLAIN ECHO/probe consumer against `PipeServer`'s
//! event stream and `PipeClient`'s blocking read/write — no capsule, no
//! wire-frame parsing (that is `wire.rs`'s and the follow-up unit's job).

use sot_log::pipe_win::{connect_voyage_pipe, ClosedReason, PipeError, PipeServer, TransportEvent};
use std::time::{Duration, Instant};

/// A fresh, canonical lowercase-hyphenated UUID for one test's voyage id —
/// `now_v7` because that is the only generator feature this crate's `uuid`
/// dependency enables (`features = ["v7"]`); any generated version would
/// do, since only the STRING SHAPE matters to `pipe_win.rs`.
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

fn expect_accepted(server: &PipeServer, timeout: Duration) -> u64 {
    match next_event(server, timeout) {
        TransportEvent::Accepted(id) => id,
        other => panic!("expected Accepted, got {other:?}"),
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
/// security (a null `SECURITY_ATTRIBUTES`) is fine here: this probe is
/// only ever used either while `pipe_win.rs`'s own descriptor already owns
/// the name (proving the rival create fails) or on a name nothing else has
/// ever touched (proving it succeeds) — never both on the SAME live name,
/// so its own weaker descriptor is never load-bearing.
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
/// (not calling into `pipe_win.rs`'s or `fsutil.rs`'s private helpers) so a
/// bug in THEIR SID lookup could not also hide from this test. Identical in
/// spirit to `voyage.rs`'s own test helper of the same name.
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

/// Round-trip `pipe_path`'s DACL to SDDL text via `GetNamedSecurityInfoW` +
/// `ConvertSecurityDescriptorToStringSecurityDescriptorW` — Npfs (the named
/// pipe file system driver) answers the same `SE_FILE_OBJECT` security
/// queries a real file or directory does, which is why `voyage.rs`'s
/// directory-descriptor test helper of the same name generalizes here
/// unchanged apart from the object path.
fn security_descriptor_sddl(pipe_path: &str) -> String {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW, SDDL_REVISION_1,
        SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

    let wide_path = wide(pipe_path);
    unsafe {
        let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let rc = GetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut psd,
        );
        assert_eq!(rc, 0, "GetNamedSecurityInfoW({pipe_path}) failed: {rc}");
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
/// dialect the actual side comes back in (see `voyage.rs`'s identical
/// helper for why this is needed, not merely stylistic).
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

/// Test 1: one server, one client, bytes both ways; a marker-tagged send's
/// `Sent` event carries the exact bytes the client actually received.
#[test]
fn server_and_client_exchange_bytes_and_sent_carries_marker() {
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 4).unwrap();
    let client = connect_voyage_pipe(&id).unwrap();
    let conn_id = expect_accepted(&server, Duration::from_secs(5));

    client.write_all(b"hello from client").unwrap();
    match next_event(&server, Duration::from_secs(5)) {
        TransportEvent::Bytes(cid, bytes) => {
            assert_eq!(cid, conn_id);
            assert_eq!(bytes, b"hello from client");
        }
        other => panic!("expected Bytes, got {other:?}"),
    }

    server.send(conn_id, b"hello from server".to_vec(), Some(42)).unwrap();
    let mut buf = [0u8; 64];
    let n = client.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"hello from server");

    match next_event(&server, Duration::from_secs(5)) {
        TransportEvent::Sent(cid, marker) => {
            assert_eq!(cid, conn_id);
            assert_eq!(marker, 42);
        }
        other => panic!("expected Sent, got {other:?}"),
    }
}

/// Test 2: two clients connected to the same voyage pipe are multiplexed
/// by distinct `ConnId`s — `connect_voyage_pipe`'s own bounded retry
/// absorbs the ordinary race against the accept loop posting the SECOND
/// instance's `ConnectNamedPipe` after the first hand-off.
#[test]
fn two_concurrent_clients_multiplexed_by_conn_id() {
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 4).unwrap();

    let client_a = connect_voyage_pipe(&id).unwrap();
    let conn_a = expect_accepted(&server, Duration::from_secs(5));
    let client_b = connect_voyage_pipe(&id).unwrap();
    let conn_b = expect_accepted(&server, Duration::from_secs(5));
    assert_ne!(conn_a, conn_b);

    client_a.write_all(b"from A").unwrap();
    client_b.write_all(b"from B").unwrap();

    let mut seen = std::collections::HashMap::new();
    for _ in 0..2 {
        match next_event(&server, Duration::from_secs(5)) {
            TransportEvent::Bytes(cid, bytes) => {
                seen.insert(cid, bytes);
            }
            other => panic!("expected Bytes, got {other:?}"),
        }
    }
    assert_eq!(seen.get(&conn_a).map(Vec::as_slice), Some(&b"from A"[..]));
    assert_eq!(seen.get(&conn_b).map(Vec::as_slice), Some(&b"from B"[..]));
}

/// Test 3: squat detection. A rival `CreateNamedPipeW` carrying
/// `FILE_FLAG_FIRST_PIPE_INSTANCE` fails with `ERROR_ACCESS_DENIED` while
/// the server holds any instance of the name; dropping the server frees it.
#[test]
fn rival_first_instance_create_fails_while_server_lives_then_frees_on_drop() {
    use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;

    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();

    let err = try_create_first_instance(&id).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32), "unexpected squat-check error: {err}");

    drop(server);
    try_create_first_instance(&id).unwrap_or_else(|e| panic!("expected the freed name to bind again: {e}"));
}

/// Test 4: the pipe's own security descriptor — protected, owner-only full
/// access, and (unlike the voyage-tree directory descriptor) NO `OI`/`CI`
/// inheritance flags, since a pipe object has no children to inherit onto.
#[test]
fn pipe_descriptor_is_protected_owner_only_with_no_container_inherit_flags() {
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 1).unwrap();
    let pipe_path = format!(r"\\.\pipe\sot-voyage-{id}");

    let sid = current_user_sid_string();
    let expected = canonical_sddl(&format!("D:P(A;FA;;;{sid})"));
    let actual = security_descriptor_sddl(&pipe_path);
    assert_eq!(actual, expected);
    assert!(!actual.contains("OICI"), "pipe descriptor must carry no OI/CI flags: {actual}");

    drop(server);
}

/// Test 5: a client that connects and never reads while the server floods
/// it. The outbound queue (capacity 8) eventually reports full once the
/// head-of-line `WriteFile` itself is stuck in the kernel (the pipe's own
/// 64 KiB buffer also fills); `close` still completes promptly —
/// `CancelIoEx` proving itself against a write that would otherwise never
/// finish — and the subsequent server drop (which joins every remaining
/// thread) is equally prompt.
#[test]
fn flooded_never_reading_client_close_completes_within_bound() {
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 2).unwrap();
    let client = connect_voyage_pipe(&id).unwrap(); // deliberately never reads
    let conn_id = expect_accepted(&server, Duration::from_secs(5));

    let payload = vec![0xABu8; 65_536];
    let mut saw_full = false;
    for _ in 0..64 {
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
    assert!(saw_full, "expected the outbound queue to report full against a non-reading peer");

    let start = Instant::now();
    server.close(conn_id).unwrap();
    assert!(start.elapsed() < Duration::from_secs(10), "close on a stuck write did not complete promptly");

    let start = Instant::now();
    drop(server);
    assert!(start.elapsed() < Duration::from_secs(10), "server drop did not complete promptly (a thread did not join)");
    drop(client);
}

/// Test 6: a pending accept with no client ever connecting — server drop
/// must return promptly (the accept thread's blocked `ConnectNamedPipe` is
/// cancelled, not merely abandoned) rather than hang.
#[test]
fn pending_accept_with_no_client_drops_promptly() {
    let id = fresh_voyage_id();
    let server = PipeServer::bind(&id, 1).unwrap();
    let start = Instant::now();
    drop(server);
    assert!(start.elapsed() < Duration::from_secs(10), "drop with a pending, client-less accept hung");
}

/// Test 7: invalid voyage ids — path-traversal shapes, wrong length,
/// uppercase, hyphen-less "simple" form, braced form, and empty — are all
/// refused loudly by both `bind` and `connect_voyage_pipe`, and `bind`
/// never gets far enough to create anything under the rejected name (shown
/// by a first-instance create against one of those exact strings still
/// succeeding afterward).
#[test]
fn invalid_voyage_ids_are_rejected_loudly_and_create_nothing() {
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
    // for a rejected id, that name would already be squatted — instead the
    // very first bad id above is still free to bind under raw Win32 here.
    try_create_first_instance("not-a-uuid").unwrap_or_else(|e| panic!("expected a rejected id to create no pipe at all: {e}"));
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
    let conn_a = expect_accepted(&server, Duration::from_secs(5));
    server.close(conn_a).unwrap();
    let mut buf = [0u8; 16];
    let n = client_a.read(&mut buf).unwrap();
    assert_eq!(n, 0, "expected ordered EOF after a server-initiated close");
    match next_event(&server, Duration::from_secs(5)) {
        TransportEvent::Closed(cid, ClosedReason::Closed) => assert_eq!(cid, conn_a),
        other => panic!("expected Closed(Closed), got {other:?}"),
    }

    // Client drop -> server's OWN reader observes the disconnect and
    // reports `Closed(Eof)` without any `close`/`drain_and_close` call.
    let client_b = connect_voyage_pipe(&id).unwrap();
    let conn_b = expect_accepted(&server, Duration::from_secs(5));
    drop(client_b);
    match next_event(&server, Duration::from_secs(5)) {
        TransportEvent::Closed(cid, ClosedReason::Eof) => assert_eq!(cid, conn_b),
        other => panic!("expected Closed(Eof), got {other:?}"),
    }
}
