//! sot-chat: the project chat's policy. This stage holds normalization only: Slack's raw message JSON into the one
//! wire record. Nothing here touches the network.

pub mod normalize;

pub use normalize::{normalize, Context, Normalized, Unrepresentable};
