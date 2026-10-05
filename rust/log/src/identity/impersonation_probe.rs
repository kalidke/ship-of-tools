//! Test support, Windows only: the impersonation level a pipe's server gets over a client's handle, to pin that every
//! client of this box's daemon pipe opens it at identification level (ADR 0049, User isolation).

#![cfg(all(windows, any(test, feature = "test-support")))]

use crate::host::wide_null;
use std::sync::atomic::{AtomicU32, Ordering};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{GetTokenInformation, RevertToSelf, TokenImpersonationLevel, TOKEN_QUERY};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, ImpersonateNamedPipeClient, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

static NEXT: AtomicU32 = AtomicU32::new(0);

/// Serve a one-instance pipe at a fresh name; the server half (accept, read one byte, impersonate the client, read the
/// token's impersonation level, revert) runs on its own thread and sends the level back. `dial(name)` runs on the calling
/// thread and must connect and write one byte; it returns its connection, which stays open until the level has arrived,
/// so the client never closes before the server measures. A dial that panics fails the test at once. Returns the level:
/// `SecurityIdentification` (1) when the client opened the pipe at identification level, `SecurityImpersonation` (2) at
/// the default. Panics on any OS failure or if no level arrives within 10 s.
pub fn level_seen_by_server<R>(dial: impl FnOnce(&str) -> R) -> i32 {
    let name = format!(r"\\.\pipe\sot-impersonation-probe-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst));
    let wide = wide_null(&name);
    let server: HANDLE = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,
            4096,
            4096,
            0,
            std::ptr::null(),
        )
    };
    assert_ne!(server, INVALID_HANDLE_VALUE, "CreateNamedPipeW: {}", std::io::Error::last_os_error());
    let (tx, rx) = std::sync::mpsc::channel();
    // A HANDLE is a raw pointer, which a thread will not take; its value crosses as an integer.
    let server_addr = server as usize;
    let serving = std::thread::spawn(move || {
        let level = serve_one(server_addr as HANDLE);
        let _ = tx.send(level);
        unsafe {
            DisconnectNamedPipe(server_addr as HANDLE);
            CloseHandle(server_addr as HANDLE);
        }
    });
    let connection = dial(&name);
    let level = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the server half sent no level: it panicked or timed out");
    drop(connection);
    serving.join().expect("the server half panicked");
    level
}

/// Accept one client on `server`, read its byte, impersonate it and return the token's impersonation level.
fn serve_one(server: HANDLE) -> i32 {
    let connected = unsafe { ConnectNamedPipe(server, std::ptr::null_mut()) } != 0
        || std::io::Error::last_os_error().raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32);
    assert!(connected, "ConnectNamedPipe: {}", std::io::Error::last_os_error());
    let (mut byte, mut read) = ([0u8; 1], 0u32);
    let ok = unsafe { ReadFile(server, byte.as_mut_ptr().cast(), 1, &mut read, std::ptr::null_mut()) };
    assert!(ok != 0 && read == 1, "the client wrote no byte: {}", std::io::Error::last_os_error());
    assert_ne!(unsafe { ImpersonateNamedPipeClient(server) }, 0, "{}", std::io::Error::last_os_error());
    let mut token: HANDLE = std::ptr::null_mut();
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    assert_ne!(opened, 0, "OpenThreadToken: {}", std::io::Error::last_os_error());
    let (mut level, mut returned) = (0i32, 0u32);
    let got = unsafe {
        GetTokenInformation(token, TokenImpersonationLevel, (&mut level as *mut i32).cast(), 4, &mut returned)
    };
    assert_ne!(got, 0, "GetTokenInformation: {}", std::io::Error::last_os_error());
    unsafe {
        RevertToSelf();
        CloseHandle(token);
    }
    level
}
