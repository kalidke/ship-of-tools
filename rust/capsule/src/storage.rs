//! The capsule's reading of platform's storage-exhaustion table.

use crate::Error;

/// The native code when `e` is storage exhaustion, read from the `io::Error` an `Error::Io` carries, never from text.
/// A transport error is not read: a full runtime folder (where sockets live) is not the state root's volume being
/// full.
pub fn storage_exhaustion(e: &Error) -> Option<i32> {
    match e {
        Error::Io(io) => sot_platform::host::native_storage_code(io),
        _ => None,
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
