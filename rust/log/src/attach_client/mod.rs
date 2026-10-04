//! One client for every viewer of a capsule (`client`), plus the daemon's
//! supervisor-lane client (`supervisor_client`).
pub mod client;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub mod supervisor_client;
