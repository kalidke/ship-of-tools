//! Who the calling process is to messaging: its context, its self file, a derived handle, its project's root in one
//! spelling, and the agent-layer walk that keeps a second agent inside a session from acting as its handle.

pub(crate) mod context;
pub(crate) mod derive;
pub(crate) mod layers;
pub(crate) mod paths;
pub(crate) mod self_file;
