//! What the window and its drawing child (`svg_child`, `sot --draw-svg`) agree on, on std alone: the magic that opens a
//! request and a reply, the tags of an ask and of a job's reply, and the codes the child ends with. The test target
//! `svg_child` includes this file, so the numbers have one owner.

use std::io::{self, Read};

/// The first four bytes of a request on stdin and of a reply on stdout.
pub(crate) const MAGIC: [u8; 4] = *b"SVG1";
/// An ask: draw onto exactly `w` by `h` pixels (two u32 little endian follow).
pub(crate) const ASK_STRETCH: u8 = b'S';
/// A job's reply: the pixels (`w` and `h` as u32 little endian, then `4 * w * h` RGBA bytes) or the error (a u16 length
/// and that many bytes of UTF-8, at most `ERROR_BYTES`).
pub(crate) const REPLY_PIXELS: u8 = b'R';
pub(crate) const REPLY_ERROR: u8 = b'E';
pub(crate) const ERROR_BYTES: usize = 1024;

/// The child ended at its heap cap, at its CPU limit, in a crash, or because its request or its bounds could not be had.
/// Each is an exit with a code, never a signal and never a core, except on Windows: there only a panic on the draw
/// thread ends with `EXIT_CRASH`, and an abort or a stack overflow ends with the OS's own crash status.
pub(crate) const EXIT_HEAP: i32 = 71;
// Windows has no CPU bound here, so no code for one.
#[cfg(unix)]
pub(crate) const EXIT_CPU: i32 = 72;
pub(crate) const EXIT_CRASH: i32 = 73;
pub(crate) const EXIT_REFUSED: i32 = 74;

/// A refusal of what a peer sent: the text is shown where the refusal ends.
pub(crate) fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

/// The next `N` bytes of `from`.
pub(crate) fn word<const N: usize>(from: &mut impl Read) -> io::Result<[u8; N]> {
    let mut word = [0; N];
    from.read_exact(&mut word)?;
    Ok(word)
}
