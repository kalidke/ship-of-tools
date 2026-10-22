//! Which native errors mean the volume or quota is full.

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

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
