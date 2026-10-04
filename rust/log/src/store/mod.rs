//! The ADR 0039 voyage store: record codec, envelope schema, segment files,
//! open-time recovery and the reader-first rollout gate.

pub(crate) mod dedupe;
pub mod envelope;
pub mod record;
pub mod recovery;
pub mod rollout;
pub mod segment;
pub mod verify;
pub mod voyage;

#[cfg(test)]
#[cfg(any(target_os = "linux", windows))]
mod support_tests;
