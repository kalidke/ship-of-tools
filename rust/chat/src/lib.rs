//! sot-chat: the project chat's policy. This stage holds normalization (Slack's raw message JSON into the one wire
//! record), `http`, the one way out of the crate to the network, and `api`, the Slack Web API calls the first stage
//! needs, and `socket`, the Socket Mode listener, and `config`, the two configuration files, and `service`, the one running listener, and `watch`, the model of the watched channels. Only `config` calls a file method (`service` takes a lock through platform).

pub mod api;
pub mod config;
pub mod http;
pub mod normalize;
pub mod service;
pub mod socket;
pub mod watch;

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
mod service_tests;
#[cfg(test)]
mod socket_tests;
#[cfg(test)]
mod watch_tests;
