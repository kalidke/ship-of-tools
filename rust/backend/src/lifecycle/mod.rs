//! When this daemon's sessions and children end: the window lease and its record, the start
//! plan, the close and the child signal.

pub(crate) mod child_signal;
pub(crate) mod lease;
pub(crate) mod shutdown;
pub(crate) mod startup;
