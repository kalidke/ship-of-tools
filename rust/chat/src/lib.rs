//! sot-chat: the project chat's policy. This stage holds normalization (Slack's raw message JSON into the one wire
//! record), `http`, the one way out of the crate to the network, and `api`, the Slack Web API calls the first stage
//! needs, and `socket`, the Socket Mode listener, and `config`, the two configuration files, and `service`, the one running listener, and `watch`, the model of the watched channels. Only `config` calls a file method (`service` takes a lock through platform).

// The fake Slack's server key is committed: a build without debug assertions (every release build) never has the feature.
#[cfg(all(feature = "test-support", not(debug_assertions)))]
compile_error!("sot-chat's `test-support` feature is for tests only and is refused in a build without debug assertions; a release-profile test build is refused by design, so tests need debug assertions");

pub mod api;
pub mod config;
#[cfg(any(test, feature = "test-support"))]
pub mod fake_slack;
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
mod http_tests;
#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod socket_tests;
#[cfg(test)]
mod watch_tests;
