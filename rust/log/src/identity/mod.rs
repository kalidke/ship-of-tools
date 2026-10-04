//! Peer identity: the same-connection challenge (OS steps 1-3 per platform, the shared wire
//! steps 4-5) and the exchange and deadline it runs on. Re-exported at the crate root.

pub mod challenge;
pub mod challenge_macos;
pub mod challenge_unix;
pub mod challenge_win;
pub mod deadline;
pub mod exchange;
pub(crate) mod exit_watch_macos;
// Decision 0031: this process's OS account, as issued by the OS (hello's account guard).
pub mod os_account;
// Decision 0031: whose OS account is on the far end of an accepted loopback TCP connection.
pub mod peer_owner;
