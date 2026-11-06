//! sot-chat: the project chat's policy. This stage holds normalization (Slack's raw message JSON into the one wire
//! record), `http`, the one way out of the crate to the network, and `api`, the Slack Web API calls the first stage
//! needs, and `socket`, the Socket Mode listener, and `config`, the two configuration files. Only `config` calls a file method.

pub mod api;
pub mod config;
pub mod http;
pub mod normalize;
pub mod socket;

pub use normalize::{normalize, Context, Normalized, Unrepresentable};

#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod config_tests;
#[cfg(test)]
mod fake_slack;
#[cfg(test)]
mod http_tests;
#[cfg(test)]
mod socket_tests;
