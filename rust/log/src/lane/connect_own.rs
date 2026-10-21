//! The one rule for a local endpoint reached by name: this box's daemon socket or pipe, or a hub relay socket, is spoken
//! to only when this OS account serves it (ADR 0049, User isolation). The connect is the lane's; the account check is
//! platform's (`identity::own_endpoint`).

use std::path::Path;

#[cfg(unix)]
use sot_platform::identity::own_endpoint::own_socket;
#[cfg(windows)]
use sot_platform::identity::own_endpoint::own_pipe;
use crate::lane::transport::TransportError;

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
    own_socket(client.socket_fd(), path).map_err(|source| TransportError::Io {
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
    use sot_platform::identity::challenge_win::PipeChallengeable as _;
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
