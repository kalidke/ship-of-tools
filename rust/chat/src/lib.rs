//! sot-chat: the project chat's policy. This stage holds normalization (Slack's raw message JSON into the one wire
//! record) and `http`, the one way out of the crate to the network. No Slack API method is here yet.

pub mod http;
pub mod normalize;

pub use normalize::{normalize, Context, Normalized, Unrepresentable};

#[cfg(test)]
mod fake_slack;
#[cfg(test)]
mod http_tests;
