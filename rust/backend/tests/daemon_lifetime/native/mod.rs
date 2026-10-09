//! The fixture's native process authority, one file per OS. An identity is a process the fixture opened while it was
//! alive and has proved to be the one it meant; death is read from that identity, never from a number or a name, and
//! the fixture ends what it started through it.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{start_ticks, Identity};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{start_ticks, Identity};
