//! What ends with a daemon: the Linux lifetime guard, the one owner of "every process the daemon starts, at any depth,
//! ends with it". Capsules are outside it by design.

#[cfg(target_os = "linux")]
pub(crate) mod guard;
