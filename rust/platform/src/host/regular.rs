//! The one bounded read of a small file another account's session could have replaced: a configuration file.

use std::io::{self, Read};
use std::path::Path;

/// The text of a regular file of at most `cap` bytes. The file is opened without blocking and without taking a
/// controlling terminal (Unix), so a FIFO planted at the path returns at once instead of waiting for a writer, and is
/// checked on the open handle, so no swap between a check and the read can change what is read. Not a regular file (a
/// FIFO, a device, a directory) is `InvalidInput`; a file over `cap` or not UTF-8 is `InvalidData`, each with its own
/// message, the first after at most one byte over the cap has been read; a missing file is `NotFound`, as `File::open`
/// says. Limit: only a FIFO's wait is ended; a regular file on a hung network mount still blocks in the OS.
///
/// Windows opens with std and applies the same handle check, cap and text rule. A directory is refused there too, but
/// by the open itself (std's `File::open` on a directory fails with `PermissionDenied`); a named pipe's name is not
/// promised refused, since no test runs one.
pub fn read_regular(path: &Path, cap: u64) -> io::Result<String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NONBLOCK | libc::O_NOCTTY,
    );
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("larger than {cap} bytes"),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8 text"))
}

#[cfg(test)]
#[path = "regular_tests.rs"]
mod tests;
