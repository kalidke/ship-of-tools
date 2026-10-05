//! The one rule for a local endpoint reached by name: this box's daemon socket or pipe, or a hub relay socket, is spoken
//! to only when this OS account serves it (ADR 0049, User isolation). Unix checks the folder, Windows the serving process.

use std::path::Path;

#[cfg(unix)]
use crate::host::state_dir::is_private_dir;
#[cfg(windows)]
use crate::identity::challenge::ChallengeOutcome;
#[cfg(windows)]
use crate::identity::challenge_win::{authenticate_steps_1_to_3, PipeChallengeable, QUERY_ACCESS};
use crate::lane::transport::TransportError;

/// Unix: refuse `path` unless its folder is a private folder of this OS account (`is_private_dir`). Runs before the
/// connect, so a refused socket is never reached.
#[cfg(unix)]
pub fn own_socket(path: &Path) -> std::io::Result<()> {
    use std::io::ErrorKind;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    if is_private_dir(dir) {
        return Ok(());
    }
    let (kind, why) = match std::fs::symlink_metadata(dir) {
        Err(e) if e.kind() == ErrorKind::NotFound => (ErrorKind::NotFound, "does not exist"),
        _ => (ErrorKind::PermissionDenied, "is not a private folder of this OS account"),
    };
    Err(std::io::Error::new(
        kind,
        format!("{}: not connecting: {} {why}", path.display(), dir.display()),
    ))
}

/// Windows: refuse the connected client end `pipe` unless the process serving it runs as this OS account
/// (steps 1-3 of the challenge, `authenticate_steps_1_to_3`). Runs before the first byte is written.
#[cfg(windows)]
pub fn own_pipe(pipe: std::os::windows::io::BorrowedHandle<'_>, path: &Path) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    // Steps 1-3 only: no wire I/O, and the process handle they open is dropped at once.
    let why = match authenticate_steps_1_to_3(pipe.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE, QUERY_ACCESS) {
        ChallengeOutcome::Proven(_) => return Ok(()),
        ChallengeOutcome::Foreign => "another OS account serves this pipe",
        // An OS call that fails never admits.
        ChallengeOutcome::Undetermined => "cannot tell which OS account serves this pipe",
    };
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!("{}: not connecting: {why}", path.display()),
    ))
}

/// The blocking connect the stdio bridge and the lane dial share: the platform connector, then the rule.
#[cfg(unix)]
pub fn connect_own(path: &Path) -> Result<crate::lane::socket_unix::SocketClient, TransportError> {
    own_socket(path).map_err(|source| TransportError::Io {
        op: "connect_own",
        source,
    })?;
    #[allow(clippy::disallowed_methods, reason = "the rule runs first")]
    let client = crate::lane::socket_unix::connect_unix_socket_unchallenged(path)?;
    Ok(client)
}

/// The blocking connect the stdio bridge and the lane dial share: the platform connector, then the rule.
#[cfg(windows)]
pub fn connect_own(path: &Path) -> Result<crate::lane::pipe_win::PipeClient, TransportError> {
    use std::os::windows::io::{BorrowedHandle, RawHandle};
    let text = path.to_str().ok_or_else(|| TransportError::Io {
        op: "connect_own",
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "pipe path is not valid Unicode"),
    })?;
    // The connector's mid-dial cancel hook. Nothing sets it: the dial is the caller's first act on this endpoint.
    let dial_cancel = std::sync::atomic::AtomicBool::new(false);
    #[allow(clippy::disallowed_methods, reason = "the rule runs right after the connect, before the caller writes")]
    let client = crate::lane::pipe_win::connect_pipe_path_unchallenged(text, &dial_cancel)?;
    // SAFETY: the handle is owned by `client`, which outlives this borrow.
    let handle = unsafe { BorrowedHandle::borrow_raw(client.raw_handle() as RawHandle) };
    own_pipe(handle, path).map_err(|source| TransportError::Io {
        op: "connect_own",
        source,
    })?;
    Ok(client)
}
