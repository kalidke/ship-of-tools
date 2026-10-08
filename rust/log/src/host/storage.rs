//! Which native errors mean the volume or quota is full.

use crate::lane::transport::TransportError;
use crate::Error;

/// The one table: the native storage-exhaustion codes of this platform.
pub(crate) fn native_storage_code(e: &std::io::Error) -> Option<i32> {
    let code = e.raw_os_error()?;
    #[cfg(unix)]
    let full = [libc::ENOSPC, libc::EDQUOT];
    #[cfg(windows)]
    let full = {
        use windows_sys::Win32::Foundation::{ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL};
        [ERROR_DISK_FULL as i32, ERROR_HANDLE_DISK_FULL as i32]
    };
    full.contains(&code).then_some(code)
}

/// The native code when `e` is storage exhaustion, read from the `io::Error`
/// an `Error::Io` or a transport error carries, never from text.
pub fn storage_exhaustion(e: &Error) -> Option<i32> {
    match e {
        Error::Io(io)
        | Error::Transport(TransportError::Io { source: io, .. })
        | Error::Transport(TransportError::RuntimeDir(io)) => native_storage_code(io),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_codes_are_recognized_by_native_code_only() {
        #[cfg(unix)]
        let (full, other) = ([libc::ENOSPC, libc::EDQUOT], [libc::EIO, libc::EACCES]);
        #[cfg(windows)]
        let (full, other) = ([112, 39], [5, 1117]);
        for code in full {
            let io = || std::io::Error::from_raw_os_error(code);
            assert_eq!(storage_exhaustion(&Error::Io(io())), Some(code));
            let transport = TransportError::Io {
                op: "write",
                source: io(),
            };
            assert_eq!(storage_exhaustion(&Error::Transport(transport)), Some(code));
            let runtime = TransportError::RuntimeDir(io());
            assert_eq!(storage_exhaustion(&Error::Transport(runtime)), Some(code));
        }
        for code in other {
            let io = std::io::Error::from_raw_os_error(code);
            assert_eq!(storage_exhaustion(&Error::Io(io)), None);
        }
        let unsupported =
            std::io::Error::new(std::io::ErrorKind::Unsupported, "incompatible volume");
        assert_eq!(storage_exhaustion(&Error::Io(unsupported)), None);
        let text = Error::State("No space left on device (os error 28)".into());
        assert_eq!(storage_exhaustion(&text), None);
        let text_io = std::io::Error::new(
            std::io::ErrorKind::Other,
            "No space left on device (os error 28)",
        );
        assert_eq!(storage_exhaustion(&Error::Io(text_io)), None);
    }
}
