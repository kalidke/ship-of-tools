//! The one rule for a local endpoint reached by name: this box's daemon socket or pipe, or a hub relay socket, is spoken
//! to only when this OS account serves it (ADR 0049, User isolation). Unix checks the account that listens on the
//! socket, Windows the serving process.

use std::path::Path;

#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};

#[cfg(windows)]
use crate::identity::challenge::ChallengeOutcome;
#[cfg(windows)]
use crate::identity::challenge_win::{authenticate_steps_1_to_3, PipeChallengeable, QUERY_ACCESS};
use crate::lane::transport::TransportError;

/// Unix: refuse the connected client end `sock` unless the account the kernel recorded when the socket's listener called
/// `listen()` is this process's effective uid. That is the account of the process that called `listen()`, not of the
/// process serving the socket now: a listener this account hands to another process by `SCM_RIGHTS` keeps this
/// account's record, so that process, whatever account it runs as, is trusted as this account's and receives what the
/// client sends. Runs after the connect and before the first byte is written; a refused socket is closed unread.
#[cfg(unix)]
fn own_socket(sock: BorrowedFd<'_>, path: &Path) -> std::io::Result<()> {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    let why = match listener_euid(sock) {
        Ok(uid) if uid == me => return Ok(()),
        Ok(_) => "another OS account listens on this socket",
        // An OS call that fails never admits.
        Err(_) => "cannot tell which OS account listens on this socket",
    };
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!("{}: not connecting: {why}", path.display()),
    ))
}

/// The account recorded for the listener, read on the client's own end with each platform's one reader: Linux
/// `SO_PEERCRED`, the euid at the listener's last `listen()`, copied at the connect.
#[cfg(target_os = "linux")]
fn listener_euid(sock: BorrowedFd<'_>) -> std::io::Result<u32> {
    crate::identity::challenge_unix::peer_credentials(sock.as_raw_fd()).map(|c| c.uid)
}

/// macOS: `getpeereid`, cached at the listener's `listen()` and copied at the connect (never `LOCAL_PEERTOKEN`).
#[cfg(target_os = "macos")]
fn listener_euid(sock: BorrowedFd<'_>) -> std::io::Result<u32> {
    crate::identity::challenge_macos::peer_euid(sock.as_raw_fd())
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

/// The blocking connect for a local endpoint: the platform connector's fixed `CONNECT_BOUND` retry budget, then the
/// account check before the caller writes. An attempt or wait in progress finishes first (Unix's 20 ms sleep, Windows's
/// 200 ms wait); this is not an exact elapsed-time limit.
#[cfg(unix)]
pub fn connect_own(path: &Path) -> Result<crate::lane::socket_unix::SocketClient, TransportError> {
    #[allow(
        clippy::disallowed_methods,
        reason = "the rule runs right after the connect, before the caller writes"
    )]
    let client = crate::lane::socket_unix::connect_unix_socket_unchallenged(path)?;
    own_socket(client.as_fd(), path).map_err(|source| TransportError::Io {
        op: "connect_own",
        source,
    })?;
    Ok(client)
}

/// The blocking connect for a local endpoint: the platform connector's fixed `CONNECT_BOUND` retry budget, then the
/// account check before the caller writes. An attempt or wait in progress finishes first (Unix's 20 ms sleep, Windows's
/// 200 ms wait); this is not an exact elapsed-time limit.
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
