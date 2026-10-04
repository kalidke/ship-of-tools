//! Peer identity: the same-connection challenge (OS steps 1-3 per platform, the shared wire
//! steps 4-5) and the exchange and deadline it runs on. Re-exported at the crate root.

pub mod challenge;
pub mod challenge_macos;
pub mod challenge_unix;
pub mod challenge_win;
pub mod connect_own;
pub mod deadline;
pub mod exchange;
pub(crate) mod exit_watch_macos;
