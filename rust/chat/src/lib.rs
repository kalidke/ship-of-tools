//! sot-chat: the project chat's policy. This stage holds normalization (Slack's raw message JSON into the one wire
//! record), `http`, the one way out of the crate to the network, and `api`, the Slack Web API calls the first stage
//! needs, and `socket`, the Socket Mode listener, and `config`, the configuration files (`chat.toml` and one secrets file per workspace), and `service`, the running listener of one workspace, and `watch`, the model of the watched channels, and `mail`, one record as the body of one mail message, and `state`, a delivering channel's boundary file and filed set, and `traverse`, the recovery walk of one channel, and `delivery`, the decisions of what to file next, and `mention`, a post's `@` names made Slack's mentions. Only `config` and `state` call a file method (`service` takes a lock through platform).

// The fake Slack's server key is committed: a build without debug assertions (every release build) never has the feature.
#[cfg(all(feature = "test-support", not(debug_assertions)))]
compile_error!("sot-chat's `test-support` feature is for tests only and is refused in a build without debug assertions; a release-profile test build is refused by design, so tests need debug assertions");

pub mod api;
pub mod config;
pub mod delivery;
#[cfg(any(test, feature = "test-support"))]
pub mod fake_slack;
pub mod http;
pub mod mail;
mod mention;
pub mod normalize;
pub mod service;
pub mod socket;
pub mod state;
pub mod traverse;
pub mod watch;

pub use normalize::{normalize, Context, Normalized, Unrepresentable};

#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod config_tests;
#[cfg(test)]
mod delivery_tests;
#[cfg(test)]
mod http_tests;
#[cfg(test)]
mod mail_tests;
#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod socket_tests;
#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod traverse_tests;
#[cfg(test)]
mod watch_tests;
