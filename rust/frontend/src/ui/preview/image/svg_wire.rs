//! What the window and its drawing child (`svg_child`, `sot --draw-svg`) agree on, on std alone: the magic that opens a
//! request and a reply, the tags of an ask and of a job's reply, and the codes the child ends with. The test target
//! `svg_child` includes this file, so the numbers have one owner.

use std::io::{self, Read};

/// The first four bytes of a request on stdin and of a reply on stdout.
pub(crate) const MAGIC: [u8; 4] = *b"SVG1";
/// An ask: draw onto exactly `w` by `h` pixels (two u32 little endian follow).
pub(crate) const ASK_STRETCH: u8 = b'S';
/// An ask: draw to fill a pane of `pane_w` by `pane_h` pixels (a texture limit `cap` follows, three u32 little endian),
/// the size being the child's to work out from the file's own: its aspect, no side past `cap`, no more than
/// `MAX_RASTER_PIXELS` in all.
pub(crate) const ASK_FIT: u8 = b'F';
/// An ask: decode the job's bytes as a picture (a chat picture's original) and answer it as PNG, fitted to its own aspect with
/// no side past `cap` (one u32 little endian follows) and no more than `MAX_RASTER_PIXELS` in all.
pub(crate) const ASK_PICTURE: u8 = b'P';
/// The most pixels a picture drawn to fit has: 16 Mpx, over a 4K screen's 8.3 Mpx.
pub(crate) const MAX_RASTER_PIXELS: u64 = 16 * 1024 * 1024;
/// The most bytes of a picture's PNG reply: its RGBA at `MAX_RASTER_PIXELS` and room for the encoding's own bytes.
pub(crate) const MAX_PICTURE_PNG: u64 = 4 * MAX_RASTER_PIXELS + (8 << 20);
/// A job's reply: the pixels (`w` and `h` as u32 little endian, then `4 * w * h` RGBA bytes) or the error (a u16 length
/// and that many bytes of UTF-8, at most `ERROR_BYTES`).
pub(crate) const REPLY_PIXELS: u8 = b'R';
/// A picture's reply: its `w` and `h` (u32 little endian), a u64 length, and that many bytes of PNG.
pub(crate) const REPLY_PNG: u8 = b'N';
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

/// How to draw one job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    Stretch(u32, u32),
    Fit { pane: (u32, u32), cap: u32 },
    Picture { cap: u32 },
}

impl Ask {
    /// The ask's bytes on the request: its tag and its u32s.
    pub(crate) fn to_bytes(self) -> Vec<u8> {
        let (tag, words) = match self {
            Ask::Stretch(w, h) => (ASK_STRETCH, vec![w, h]),
            Ask::Fit { pane, cap } => (ASK_FIT, vec![pane.0, pane.1, cap]),
            Ask::Picture { cap } => (ASK_PICTURE, vec![cap]),
        };
        std::iter::once(tag).chain(words.into_iter().flat_map(u32::to_le_bytes)).collect()
    }

    /// Whether a reply of `w` by `h` is what was asked: a stretch's size (a zero side is drawn as one), or a fit's or a
    /// picture's within its limits. Only a picture's reply is PNG (`png`), and only its reply.
    pub(crate) fn accepts(self, w: u32, h: u32, png: bool) -> bool {
        let within = |cap: u32| (1..=cap.max(1)).contains(&w) && (1..=cap.max(1)).contains(&h) && u64::from(w) * u64::from(h) <= MAX_RASTER_PIXELS;
        match self {
            Ask::Stretch(sw, sh) => !png && (w, h) == (sw.max(1), sh.max(1)),
            Ask::Fit { cap, .. } => !png && within(cap),
            Ask::Picture { cap } => png && within(cap),
        }
    }
}
