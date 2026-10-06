//! When this daemon's sessions and children end: the window lease and its record, the start
//! plan, the close and the child signal.

pub(crate) mod child_signal;
pub(crate) mod contain;
pub(crate) mod lease;
pub(crate) mod shutdown;
pub(crate) mod startup;

#[cfg(test)]
mod start_tests;

#[cfg(test)]
pub(crate) mod exit_tests;

pub(crate) mod signal_exit;
