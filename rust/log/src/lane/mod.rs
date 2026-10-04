//! The capsule's lanes: the transport contract (`transport`), the client
//! seam (`client`) and the two platform bridges (`pipe_transport`,
//! `socket_transport`) that each lane server rides on.
pub mod attach_proto;
pub mod client;
pub mod pipe_transport;
pub mod socket_transport;
pub mod transport;
pub mod wire;
