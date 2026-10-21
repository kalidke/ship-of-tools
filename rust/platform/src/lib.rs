//! The platform subsystem: one answer per machine fact and one primitive per OS mechanism (`CLAUDE.md` is its
//! contract). It depends on nothing of ours; every other crate names its items by path (`sot_platform::host::...`),
//! and nothing re-exports them.

pub mod host;
pub mod identity;
#[cfg(feature = "test-support")]
pub mod test_isolated;
#[cfg(feature = "test-support")]
pub mod test_wait;

/// The two ways a platform primitive fails: the OS said no (`Io`, its code kept so a caller can recognize storage
/// exhaustion), or the primitive refused its input or a state it must not run in (`State`). Upper crates convert it
/// into their own error; they do not match on it across a crate boundary.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("state: {0}")]
    State(String),
}

pub type Result<T> = std::result::Result<T, Error>;
