//! The capsule's lanes: the transport contract (`transport`), the client
//! seam (`client`) and the one platform bridge (`platform_transport`)
//! that the platform's lane server rides on.
pub mod attach_proto;
pub mod client;
pub mod pipe_win;
pub mod platform_transport;
pub mod socket_unix;
pub mod test_progress;
pub mod transport;
pub mod wire;
