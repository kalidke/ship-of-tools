//! What this box's daemon derives from hosts.toml: its topology store and `topology.set`,
//! `sotd status`, `sotd stdio-bridge`, `sotd same-version` and the one-shot blocking client.

pub(crate) mod cli;
pub(crate) mod dial;
pub(crate) mod relay_units;
pub(crate) mod same_version;
pub(crate) mod set;
pub(crate) mod status;
pub(crate) mod stdio_bridge;
pub(crate) mod store;
