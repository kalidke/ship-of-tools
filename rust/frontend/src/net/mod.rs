//! fe-net: the window's connection set, its reconnect memory, one control transport per host.

pub(crate) mod dial;
pub(crate) mod hosts;
pub(crate) mod identity;
pub(crate) mod state;
pub(crate) mod transport;

#[cfg(test)]
#[path = "link_fixture_tests.rs"]
pub(crate) mod link_fixture;
