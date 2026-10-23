//! One client for every viewer of a row (`client`, over the attach worker in `attach`), plus the daemon's
//! supervisor-lane client (`supervisor_client`, until the row socket takes its calls).
pub mod attach;
pub mod client;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub mod supervisor_client;
mod supervisor_wait;
