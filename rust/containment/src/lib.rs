//! The containment subsystem: every child the daemon starts lives in a kill domain its owner ends, and every wait on one
//! is bounded (`CLAUDE.md` is its contract). It depends on `sot-platform` alone; the daemon's binary names its items by
//! path (`sot_containment::Signal`) and nothing re-exports them.

#[cfg(feature = "test-support")]
pub mod contain;
#[cfg(not(feature = "test-support"))]
mod contain;
#[cfg(target_os = "linux")]
pub mod guard;
mod signal;

#[cfg(all(feature = "test-support", unix))]
pub mod leftover;
#[cfg(all(feature = "test-support", unix))]
pub mod watched;
#[cfg(all(feature = "test-support", windows))]
#[path = "watched_windows.rs"]
pub mod watched;

#[cfg(unix)]
pub use signal::reset_child_signal;
pub use signal::{process, Contained, ContainedStd, Signal};

#[cfg(test)]
mod start_tests;
