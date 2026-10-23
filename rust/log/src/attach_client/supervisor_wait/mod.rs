//! What `supervisor_client` still needs of the old supervisor lane until the row socket takes the daemon's calls
//! (pass 6c-1b): the bounded frame reader, the one end-run loop and its dispatcher.

mod lane_io;
mod quit;
mod wait;

pub(crate) use lane_io::FrameReader;
pub use quit::*;
pub(crate) use wait::run_end_run_and_wait;

use std::time::Duration;

/// A `hello`'s reply budget (ADR 0041 Lifecycle "Every op has one budget"); the supervisor client's connect and
/// challenge use it.
pub(crate) const HELLO_BUDGET: Duration = Duration::from_secs(2);
/// A `status`'s reply budget: a lane that accepts but does not answer within it is treated as absent.
pub(crate) const STATUS_BUDGET: Duration = Duration::from_secs(5);
/// Ordinary lane-operation write budget, shared with `supervisor_client::end_run`.
pub(crate) const WRITE_BUDGET: Duration = Duration::from_secs(2);
