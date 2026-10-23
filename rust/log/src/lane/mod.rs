//! The capsule's lanes: the transport contract (`transport`) and the client seam (`client`).
pub mod accept_policy;
pub mod client;
pub mod connect_own;
pub mod exchange;
pub(crate) mod pending;
pub mod pipe_win;
#[cfg(test)]
mod reaper_tests;
pub mod socket_unix;
pub mod test_progress;
pub mod transport;
pub mod wire;
