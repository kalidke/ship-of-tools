#![cfg(windows)]
//! Proves `winhandle::harden_own_stdio` clears `HANDLE_FLAG_INHERIT`,
//! leaving stderr alone unless asked (decision 25's leg-shared log). Only
//! test in this file/binary -- it mutates this process's REAL
//! `STD_*_HANDLE`s, which would race a concurrent test in the SAME process
//! (as `tests/pipe_win.rs` notes for `GetProcessHandleCount`); one binary
//! per `tests/*.rs` file already isolates it.

use std::os::windows::io::AsRawHandle;
use windows_sys::Win32::Foundation::{GetHandleInformation, SetHandleInformation, HANDLE_FLAG_INHERIT};
use windows_sys::Win32::System::Console::{
    GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

fn is_inheritable(std_handle: STD_HANDLE) -> bool {
    unsafe {
        let mut flags: u32 = 0;
        assert_ne!(GetHandleInformation(GetStdHandle(std_handle), &mut flags), 0);
        flags & HANDLE_FLAG_INHERIT != 0
    }
}

/// A plain temp file, forced inheritable via the same primitive the fix
/// uses to clear it -- stands in for a launcher-provided stdio handle.
fn inheritable_temp_handle(dir: &std::path::Path, name: &str) -> std::fs::File {
    let file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(name)).unwrap();
    unsafe {
        assert_ne!(SetHandleInformation(file.as_raw_handle() as _, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT), 0);
    }
    file
}

#[test]
fn harden_clears_stdin_stdout_and_stderr_only_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let saved = unsafe { (GetStdHandle(STD_INPUT_HANDLE), GetStdHandle(STD_OUTPUT_HANDLE), GetStdHandle(STD_ERROR_HANDLE)) };
    let (a, b, c) = (
        inheritable_temp_handle(dir.path(), "in"),
        inheritable_temp_handle(dir.path(), "out"),
        inheritable_temp_handle(dir.path(), "err"),
    );
    unsafe {
        assert_ne!(SetStdHandle(STD_INPUT_HANDLE, a.as_raw_handle() as _), 0);
        assert_ne!(SetStdHandle(STD_OUTPUT_HANDLE, b.as_raw_handle() as _), 0);
        assert_ne!(SetStdHandle(STD_ERROR_HANDLE, c.as_raw_handle() as _), 0);
    }
    assert!(is_inheritable(STD_INPUT_HANDLE) && is_inheritable(STD_OUTPUT_HANDLE) && is_inheritable(STD_ERROR_HANDLE));

    sot_log::winhandle::harden_own_stdio(false).unwrap();
    assert!(!is_inheritable(STD_INPUT_HANDLE), "stdin must no longer be inheritable");
    assert!(!is_inheritable(STD_OUTPUT_HANDLE), "stdout must no longer be inheritable");
    assert!(is_inheritable(STD_ERROR_HANDLE), "stderr (decision 25's leg-shared log) must stay inheritable");

    sot_log::winhandle::harden_own_stdio(true).unwrap();
    assert!(!is_inheritable(STD_ERROR_HANDLE), "harden_own_stdio(true) must also clear stderr");

    unsafe {
        SetStdHandle(STD_INPUT_HANDLE, saved.0);
        SetStdHandle(STD_OUTPUT_HANDLE, saved.1);
        SetStdHandle(STD_ERROR_HANDLE, saved.2);
    }
}
