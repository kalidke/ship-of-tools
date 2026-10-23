#![cfg(windows)]
//! The client half of the named-pipe transport (`src/lane/pipe_win/`), against the row acceptor: a real `RowServer` over
//! a fake `RowRun` on a row pipe, or a plain pipe instance where the peer must not answer. The server half of the lanes
//! is gone, and what the row acceptor keeps of its properties is tested with it (`tests/row_pipe.rs`: every pipe
//! instance is owner-only).
//!
//! Every test that touches a pipe runs in its own process (`sot_platform::test_isolated::run_isolated`): a real PROCESS
//! boundary bounds every hang path, including one inside a wedged server's `drop` running on the test thread after an
//! earlier assertion panicked.

use sot_platform::host::wide_null;
use sot_platform::test_isolated::run_isolated;
use std::time::{Duration, Instant};

/// A per-step bound used throughout (well inside `sot_platform::test_isolated::ISOLATION_TIMEOUT`, so a stalled step
/// always trips before the parent's own kill fires).
const TIMEOUT: Duration = Duration::from_secs(10);

/// A pipe handle that closes when it drops.
struct Handle(windows_sys::Win32::Foundation::HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// The name of a row pipe no other test shares.
fn fresh_row_pipe() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    sot_log::row::row_socket_path(&format!("t{}-{n}", std::process::id())).expect("the pipe name")
}

/// A row server over a fake run, on a fresh pipe.
fn bind_row() -> (std::path::PathBuf, sot_log::row::RowServer, std::sync::Arc<sot_log::row::RowRun>) {
    let path = fresh_row_pipe();
    let run = sot_log::row::RowRun::new("run-one", "ready");
    let server = sot_log::row::RowServer::bind(&path, std::sync::Arc::clone(&run)).expect("bind the pipe");
    (path, server, run)
}

/// ONE instance of a pipe that exists, is owner-only by default and is never read: a peer that accepts and says
/// nothing. The pipe's buffers are small, so a client that writes to it fills them at once.
fn silent_pipe(path: &std::path::Path) -> Handle {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX};
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    let name = wide_null(path.to_str().expect("a pipe name"));
    let h = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS | PIPE_WAIT,
            1,
            512,
            512,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(h, INVALID_HANDLE_VALUE, "CreateNamedPipeW failed: {}", std::io::Error::last_os_error());
    Handle(h)
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
fn wait_for_endpoint<T, E: std::fmt::Display>(
    connect: impl Fn() -> Result<T, E>,
    deadline: Duration,
) -> T {
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

#[path = "../support/row_challenge.rs"]
mod row_challenge;

mod challenge;
mod close;
mod connect;
