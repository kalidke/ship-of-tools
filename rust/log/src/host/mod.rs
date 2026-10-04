//! The platform subsystem: state dirs, durable publication, kernel locks.
//! The glob re-exports below keep the old `crate::fsutil::...` paths
//! (`lib.rs` aliases `host as fsutil`) until the crate's re-export cleanup.

mod durable;
pub mod state_dir;
pub mod winhandle;

pub use durable::*;
