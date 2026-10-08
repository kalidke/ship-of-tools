//! The admission of a capsule birth: what a start or resume is told when the row's authority fence is already claimed.
//!
//! Every start path (create, boot resume, attach, explicit start, a reset's respawn, reauth, the watchdog's restart)
//! reaches one launch (`spawn_detached_supervisor`), and that launch claims the row's `supervisor.lock` before it
//! accepts anything. A claim that cannot be taken means a supervisor is alive or a birth is in flight; either way the
//! launch forks nothing and the caller is told the authority is pending. It is never told success, never forks a
//! replacement, and never reclaims the fence: the original, once it has taken over its claim, answers the lane.

use std::io;

/// The text of a pending-authority refusal; the one string every layer compares against.
pub const PENDING_AUTHORITY: &str =
    "capsule supervisor authority is pending: another process holds this row's authority fence (a birth in flight or a supervisor not yet answering)";

/// The error a launch returns when the fence is claimed (the kind is `WouldBlock`).
#[derive(Debug)]
pub(crate) struct PendingAuthority;

impl std::fmt::Display for PendingAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(PENDING_AUTHORITY)
    }
}

impl std::error::Error for PendingAuthority {}

/// The pending refusal as an `io::Error`.
pub(crate) fn pending() -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, PendingAuthority)
}

/// Whether `error` is a pending-authority refusal.
pub(crate) fn is_pending(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<PendingAuthority>())
}

/// Whether `text` is the pending-authority refusal as a message, for the layers that carry errors as strings.
pub(crate) fn is_pending_text(text: &str) -> bool {
    text == PENDING_AUTHORITY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pending_refusal_is_told_apart_from_every_other_error() {
        assert!(is_pending(&pending()));
        assert!(!is_pending(&io::Error::new(
            io::ErrorKind::WouldBlock,
            "something else"
        )));
        assert!(!is_pending(&io::Error::from(io::ErrorKind::NotFound)));
        assert!(is_pending_text(&pending().to_string()));
        assert!(!is_pending_text("capsule supervisor spawn failed: nope"));
    }
}
