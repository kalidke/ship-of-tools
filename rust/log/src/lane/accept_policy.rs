//! What an accept loop does with an accept error: the one policy the daemon's session listener and both capsule lane
//! acceptors share.

use std::io;
use std::time::Duration;

/// How long an accept loop waits after a resource error before it accepts again: long enough that a descriptor
/// shortage is not retried in a hot spin, short enough that a freed descriptor is used at once.
pub const ACCEPT_SLEEP: Duration = Duration::from_millis(100);

/// What the loop does next with an accept error.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum AcceptAction {
    /// Accept again at once: the error belongs to one connection (a signal, a peer that reset before the accept).
    Retry,
    /// Wait [`ACCEPT_SLEEP`], then accept again: the process or the system is out of descriptors or memory, which
    /// ends by itself.
    Sleep,
    /// The listener is dead: the loop ends.
    Dead,
}

/// The one accept policy. `Interrupted`, `ConnectionAborted` and `WouldBlock` retry (ADR 0043 decision 4's family);
/// EMFILE, ENFILE, ENOBUFS and ENOMEM sleep (their Windows counterparts too); anything else is a dead listener.
pub fn accept_policy(err: &io::Error) -> AcceptAction {
    if matches!(
        err.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted | io::ErrorKind::WouldBlock
    ) {
        return AcceptAction::Retry;
    }
    if err.kind() == io::ErrorKind::OutOfMemory || err.raw_os_error().is_some_and(is_resource_exhaustion) {
        return AcceptAction::Sleep;
    }
    AcceptAction::Dead
}

#[cfg(unix)]
fn is_resource_exhaustion(code: i32) -> bool {
    matches!(code, libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)
}

/// ERROR_NOT_ENOUGH_MEMORY, ERROR_OUTOFMEMORY, ERROR_NO_SYSTEM_RESOURCES, WSAEMFILE and WSAENOBUFS.
#[cfg(windows)]
fn is_resource_exhaustion(code: i32) -> bool {
    matches!(code, 8 | 14 | 1450 | 10024 | 10055)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_kind_has_its_action() {
        let kind = |k: io::ErrorKind| accept_policy(&io::Error::from(k));
        assert_eq!(kind(io::ErrorKind::Interrupted), AcceptAction::Retry);
        assert_eq!(kind(io::ErrorKind::ConnectionAborted), AcceptAction::Retry);
        assert_eq!(kind(io::ErrorKind::WouldBlock), AcceptAction::Retry);
        assert_eq!(kind(io::ErrorKind::OutOfMemory), AcceptAction::Sleep);
        assert_eq!(kind(io::ErrorKind::PermissionDenied), AcceptAction::Dead);
    }

    #[cfg(unix)]
    #[test]
    fn resource_errors_sleep_and_a_bad_listener_is_dead() {
        let os = |c: i32| accept_policy(&io::Error::from_raw_os_error(c));
        for code in [libc::EMFILE, libc::ENFILE, libc::ENOBUFS, libc::ENOMEM] {
            assert_eq!(os(code), AcceptAction::Sleep, "errno {code}");
        }
        assert_eq!(os(libc::ECONNABORTED), AcceptAction::Retry);
        assert_eq!(os(libc::EBADF), AcceptAction::Dead);
        assert_eq!(os(libc::EINVAL), AcceptAction::Dead);
    }
}
