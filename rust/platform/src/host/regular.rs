//! The one bounded read of a small file another account's session could have replaced: a configuration file.

use std::io::{self, Read};
use std::path::Path;

/// Opens `path` for reading only if it is a regular file: without blocking or taking a controlling terminal (Unix), so
/// a FIFO returns at once, and checked on the open handle. Anything else (a FIFO, a device, a directory) is
/// `InvalidInput`; a missing file is `NotFound`. On Windows an open refused at the instant its name is replaced is asked
/// again within the retry window ([`open_answered`]).
pub fn open_regular(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NONBLOCK | libc::O_NOCTTY,
    );
    let file = open_answered(&options, path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(test)]
thread_local! {
    pub(super) static ASKED_AGAIN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The open as the system answers it.
#[cfg(not(windows))]
fn open_answered(options: &std::fs::OpenOptions, path: &Path) -> io::Result<std::fs::File> {
    options.open(path)
}

/// Windows can answer the open of a name with access denied at the instant the name is replaced: the kernel's status is
/// then file deleted (or delete pending): rare (9 refused reads in 400,000 replaces under four readers, on a hosted
/// runner), in this process and in another. The replace does not wait on the reader, so the open is asked again within
/// `RETRY_DEADLINE_MS`, as a record's rename is, and a refusal that outlasts the window is the answer. Only those two
/// statuses are asked again: any other refusal (a denial by the file's ACL, a folder, a sharing violation) is answered
/// at once, with no wait.
#[cfg(windows)]
fn open_answered(options: &std::fs::OpenOptions, path: &Path) -> io::Result<std::fs::File> {
    use super::{RETRY_DEADLINE_MS, RETRY_STEP_MS};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, STATUS_DELETE_PENDING, STATUS_FILE_DELETED};
    let deadline = Instant::now() + Duration::from_millis(RETRY_DEADLINE_MS);
    loop {
        last_nt::clear();
        let opened = options.open(path);
        // The status is the thread's last one: read at once, before any other call of this thread can set it.
        let status = if opened.is_err() { last_nt::status() } else { 0 };
        match opened {
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32)
                && (status == STATUS_FILE_DELETED || status == STATUS_DELETE_PENDING)
                && Instant::now() < deadline =>
            {
                // Probe only, never landed: this thread's refused opens that were asked again.
                #[cfg(test)]
                ASKED_AGAIN.with(|n| n.set(n.get() + 1));
                crate::hold_point!("platform.regular.refused", path.display());
                std::thread::sleep(Duration::from_millis(RETRY_STEP_MS));
            }
            answered => return answered,
        }
    }
}

/// The NT status behind the thread's last refused call (Win32 keeps it beside its own code).
#[cfg(windows)]
mod last_nt {
    use std::ffi::c_void;

    type Get = unsafe extern "system" fn() -> i32;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleA(name: *const u8) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> Option<Get>;
    }

    /// Sets the thread's last status to a success's, so the next refusal's status is that call's own.
    pub fn clear() {
        // SAFETY: a pure lookup of a status code; it sets the thread's last status as a side effect.
        unsafe { windows_sys::Win32::Foundation::RtlNtStatusToDosError(0) };
    }

    /// The thread's last NT status, 0 where it cannot be read (the export is looked up once).
    pub fn status() -> i32 {
        static GET: std::sync::OnceLock<Option<Get>> = std::sync::OnceLock::new();
        // SAFETY: ntdll is loaded in every Windows process; the export has the signature `Get`.
        let get = GET.get_or_init(|| unsafe { GetProcAddress(GetModuleHandleA(b"ntdll.dll\0".as_ptr()), b"RtlGetLastNtStatus\0".as_ptr()) });
        // SAFETY: the call takes nothing and reads the thread's own last status.
        get.map_or(0, |get| unsafe { get() })
    }
}

/// The text of a regular file of at most `cap` bytes. The file is opened without blocking and without taking a
/// controlling terminal (Unix), so a FIFO planted at the path returns at once instead of waiting for a writer, and is
/// checked on the open handle, so no swap between a check and the read can change what is read. Not a regular file (a
/// FIFO, a device, a directory) is `InvalidInput`; a file over `cap` or not UTF-8 is `InvalidData`, each with its own
/// message, the first after at most one byte over the cap has been read; a missing file is `NotFound`, as `File::open`
/// says. Limit: only a FIFO's wait is ended; a regular file on a hung network mount still blocks in the OS.
///
/// Windows opens with std and applies the same handle check, cap and text rule. A directory is refused there too, but
/// by the open itself (std's `File::open` on a directory fails with `PermissionDenied`); a named pipe's name is not
/// promised refused, since no test runs one.
pub fn read_regular(path: &Path, cap: u64) -> io::Result<String> {
    let file = open_regular(path)?;
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("larger than {cap} bytes"),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8 text"))
}

#[cfg(test)]
#[path = "regular_tests.rs"]
mod tests;
