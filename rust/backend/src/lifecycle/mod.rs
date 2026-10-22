//! When this daemon's sessions and children end: the window lease and its record, the start
//! plan and the close.

pub(crate) mod lease;
pub(crate) mod shutdown;
pub(crate) mod signal_exit;
pub(crate) mod startup;
#[cfg(feature = "daemon-lifetime-faults")]
pub(crate) mod test_gates;
