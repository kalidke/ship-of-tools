//! The capsule's lanes, client half only: the transport contract (`transport`), the client seam (`client`) and the two
//! platform clients. Every server is the row socket's (`crate::row`).
pub mod accept_policy;
pub mod client;
pub mod connect_own;
pub mod pipe_win;
pub mod socket_unix;
pub mod transport;
