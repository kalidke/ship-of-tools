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

/// Serve a one-instance pipe at a fresh name, run `dial(name)` on another thread (it must connect and write one byte),
/// and return the token impersonation level the server gets by impersonating that client after reading the byte:
/// `SecurityIdentification` (1) when the client opened the pipe at identification level, `SecurityImpersonation` (2) at
/// the default. Panics on any OS failure.
pub fn level_seen_by_server(dial: impl FnOnce(&str) + Send + 'static) -> i32 {
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
    let dialer = std::thread::spawn({
        let name = name.clone();
        move || dial(&name)
    });
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
    dialer.join().expect("the dial panicked");
    unsafe {
        DisconnectNamedPipe(server);
        CloseHandle(server);
    }
    level
}
