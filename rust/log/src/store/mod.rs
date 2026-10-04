//! The ADR 0039 voyage store: record codec, envelope schema, segment files,
//! open-time recovery and the reader-first rollout gate.

pub mod envelope;
pub mod record;
pub mod recovery;
pub mod rollout;
pub mod segment;
