#![cfg(windows)]
//! Integration tests for the ADR 0041 step-5 pipe transport
//! (`src/lane/pipe_win/`, unit U3 round 3 — discharges the second Codex
//! adversarial round's test findings). Lives in `tests/` for the same
//! structural reason `tests/conpty.rs` and `tests/capsule/` do:
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
//!
//! L1-unix LU1a: `challenge`/`ChallengedProcess`/`authenticate_server`
//! moved to `sot_log::identity::challenge_win` (the Windows steps 1-3 half); the
//! `ChallengeableConnection` trait itself (`write_all`/`read`/`cancel`)
//! stays in `sot_log::identity::challenge`, and its raw-handle counterpart is now
//! the separate `sot_log::identity::challenge_win::PipeChallengeable` extension
//! trait — see `InvalidHandleConn`'s own two `impl` blocks below.

use sot_log::host::wide_null;
use sot_log::identity::challenge::ChallengeOutcome;
use sot_log::identity::challenge_win::challenge;
use sot_log::identity::exchange::VoyageMgmtExchange;
use sot_log::lane::attach_proto::ConnId;
use sot_log::lane::pipe_win::{connect_voyage_pipe, PipeServer};
use sot_log::lane::transport::{ClosedReason, LaneEvent, TransportError, CONNECT_BOUND, TEARDOWN_AGGREGATE_DEADLINE};
use sot_log::lane::wire::{self, MgmtReply, MgmtRequest, Survival};
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
fn next_event(server: &PipeServer, timeout: Duration) -> LaneEvent {
    server
        .events()
        .recv_timeout(timeout)
        .unwrap_or_else(|e| panic!("expected a transport event within {timeout:?}, got {e}"))
}

fn expect_accepted(server: &PipeServer, timeout: Duration) -> ConnId {
    match next_event(server, timeout) {
        LaneEvent::Accepted(id) => id,
        other => panic!("expected Accepted, got {other:?}"),
    }
}

fn expect_closed(server: &PipeServer, conn_id: ConnId, timeout: Duration) -> ClosedReason {
    match next_event(server, timeout) {
        LaneEvent::Closed(id, reason) => {
            assert_eq!(id, conn_id, "Closed for the wrong connection");
            reason
        }
        other => panic!("expected Closed, got {other:?}"),
    }
}

/// ADR 0043 decision 27: the transport's own connect no longer retries an
/// ABSENT pipe (only a busy one, within `CONNECT_BOUND`) — a caller
/// racing a server's own startup (a spawned child process binding its
/// pipe a moment after this test spawns it) now owns that readiness wait
/// itself. Polls `connect` every 50ms until it succeeds or `deadline` —
/// a GENEROUS bound, evidence of a genuinely broken startup, never a
/// tight race — expires, at which point the LAST error fails the test
/// loudly. Identical helper in `tests/e2e_pipe.rs` and
/// `tests/e2e_socket/` (no shared test module spans Windows-only and
/// Linux-only files).
fn wait_for_endpoint<T, E: std::fmt::Display>(connect: impl Fn() -> Result<T, E>, deadline: Duration) -> T {
    let started = Instant::now();
    loop {
        match connect() {
            Ok(v) => return v,
            Err(e) => {
                if started.elapsed() >= deadline {
                    panic!("endpoint did not become ready within {deadline:?}: {e}");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
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
            LaneEvent::Bytes(cid, bytes) => {
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

    let name = wide_null(&format!(r"\\.\pipe\sot-voyage-{voyage_id}"));
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
/// derived so a bug in `lane/pipe_win/`'s or `host/`'s own SID lookup
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

    let wide_sddl = wide_null(sddl);
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
    let name = wide_null(&format!(r"\\.\pipe\sot-voyage-{voyage_id}"));
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

mod challenge;
mod close;
mod connect;
mod teardown;
