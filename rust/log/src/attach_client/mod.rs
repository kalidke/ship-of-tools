//! One client for every viewer of a row (`client`, over the attach worker in `attach`), plus the daemon's calls on a
//! row's supervisor (`supervisor_client`, over the row socket).
pub mod attach;
pub mod client;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
pub mod supervisor_client;
