//! The one bounded read of a small file another account's session could have replaced: a configuration file.

use std::io::{self, Read};
use std::path::Path;

/// The bytes of a regular file of at most `cap` bytes. The file is opened without blocking and without taking a
/// controlling terminal (Unix), so a FIFO planted at the path returns at once instead of waiting for a writer, and is
/// checked on the open handle, so no swap between a check and the read can change what is read. Not a regular file (a
/// FIFO, a device, a directory) is `InvalidInput`; a file over `cap` is `InvalidData`, after at most one byte over the
/// cap has been read; a missing file is `NotFound`, as `File::open` says.
///
/// Windows opens with std and applies the same handle check and cap; no test runs there, so a FIFO's equivalent (a
/// named pipe's name) is not promised.
pub fn read_regular(path: &Path, cap: u64) -> io::Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NONBLOCK | libc::O_NOCTTY,
    );
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "regular_tests.rs"]
mod tests;
