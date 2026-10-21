//! Which native errors mean the volume or quota is full.

use crate::Error;

/// The one table: the native storage-exhaustion codes of this platform, read from an `io::Error`, never from its text.
/// A caller with an error type of its own reads its `Io` variant through this (`sot_log::storage_exhaustion`).
pub fn native_storage_code(e: &std::io::Error) -> Option<i32> {
    let code = e.raw_os_error()?;
    #[cfg(unix)]
    let full = [libc::ENOSPC, libc::EDQUOT];
    #[cfg(windows)]
    let full = {
        use windows_sys::Win32::Foundation::{
            ERROR_DISK_FULL, ERROR_DISK_QUOTA_EXCEEDED, ERROR_HANDLE_DISK_FULL,
        };
        [
            ERROR_DISK_FULL as i32,
            ERROR_HANDLE_DISK_FULL as i32,
            ERROR_DISK_QUOTA_EXCEEDED as i32,
        ]
    };
    full.contains(&code).then_some(code)
}

/// The native code when `e` is storage exhaustion, read from the `io::Error` an `Error::Io` carries, never from text.
pub(crate) fn storage_exhaustion(e: &Error) -> Option<i32> {
    match e {
        Error::Io(io) => native_storage_code(io),
        _ => None,
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
