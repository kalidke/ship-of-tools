//! Structured checkpoint/restore of terminal state (ADR 0041 step 3).
//!
//! A capsule outlives the frontend that renders it. When a frontend attaches
//! — a fresh start or a relaunch — it must be handed the terminal state
//! *exactly*, not a reconstruction. ADR 0041 rejects the
//! `contents_formatted` approach explicitly: replaying synthesized escape
//! sequences cannot express the inactive grid or alternate-screen identity,
//! so a session that relaunched inside `vim` would come back wrong.
//!
//! This module defines the wire format for that hand-off and the shared
//! read/write primitives. The per-type encoders live next to the types they
//! serialize ([`crate::screen`], [`crate::grid`], [`crate::row`],
//! [`crate::cell`], [`crate::attrs`]), matching how `write_contents_*`
//! is already organized in this crate.
//!
//! # What rides, and what deliberately does not
//!
//! Carried: both grids, alternate-screen identity, current and saved cursor,
//! current and saved attributes, scroll region, origin mode (current and
//! saved), per-row wrap flags, and the input modes (application keypad,
//! application cursor, cursor visibility, bracketed paste, mouse protocol
//! mode and encoding).
//!
//! Not carried, each for a reason:
//!
//! * **Scrollback contents.** The capsule keeps none. Scrollback is
//!   frontend-side derived state accumulated from the live stream after
//!   attach; pre-attach history is served by frame replay from the voyage,
//!   which already records every byte. This deletion is what makes the
//!   ADR 0041 resource budget provable.
//! * **Scrollback capacity and offset.** Capacity is the *restorer's*
//!   configuration, not the capsule's — so [`crate::Screen::restore`] takes
//!   it as an argument instead of reading it off the wire. The offset is
//!   necessarily zero when there is no scrollback to be offset into.
//! * **vte parser state.** The escape-sequence state machine is now an
//!   owned vendored module (`src/vte`), not an external crate, but its
//!   state is still deliberately not part of the checkpoint format. The
//!   cut contract is enforced by [`crate::Parser::is_ground`] instead: the
//!   producer only ever cuts the attach stream at a ground-state boundary,
//!   so no partial escape sequence or codepoint ever crosses a cut.
//!
//! # Format (version 1, little-endian)
//!
//! ```text
//! header
//!   magic        8  b"SOTVT100"
//!   version      2  u16 = 1
//!   rows         2  u16, 2..=256
//!   cols         2  u16, 2..=512
//!   modes        1  u8, the Screen mode bitfield
//!   mouse_mode   1  u8, MouseProtocolMode tag
//!   mouse_enc    1  u8, MouseProtocolEncoding tag
//!   attrs           current SGR attributes
//!   saved_attrs     attributes saved by DECSC
//! body
//!   grid            the normal grid
//!   grid            the alternate grid
//!
//! grid
//!   pos          4  u16 row, u16 col
//!   saved_pos    4  u16 row, u16 col
//!   scroll_top   2  u16
//!   scroll_bot   2  u16
//!   origin_mode  1  u8, 0 or 1
//!   saved_origin 1  u8, 0 or 1
//!   rows            `rows` row records
//!
//! row
//!   wrapped      1  u8, 0 or 1
//!   cells           `cols` cell records
//!
//! cell
//!   flags        1  u8: bit0 = length byte present, bit1 = attributes
//!                   present. flags == 0 is an empty cell with default
//!                   attributes, and costs exactly one byte — which is the
//!                   overwhelmingly common case.
//!   len          1  u8, present iff bit0: the packed wide / wide-
//!                   continuation / length byte
//!   contents  0..22 present iff bit0: `len & 0x1f` UTF-8 bytes
//!   attrs           present iff bit1
//!
//! attrs
//!   fg              color
//!   bg              color
//!   mode         1  u8, the text-mode bitfield
//!
//! color
//!   tag          1  u8: 0 = Default, 1 = Idx, 2 = Rgb
//!   payload   0..3  none / u8 index / u8 red, green, blue
//! ```
//!
//! **Every screen has exactly one encoding.** Where the format offers a
//! short form — an omitted length for an empty cell, omitted default
//! attributes — the long form is refused on restore rather than accepted as
//! an alias. Without that, two byte strings could describe one screen, and
//! the pinned version-1 golden below would be pinning one of several right
//! answers.
//!
//! There is no checksum, and no reserved or padding fields.
//!
//! A checksum would guard a payload that is never persisted: it is produced
//! at attach and handed straight to a local pipe, which is reliable and
//! ordered. There is no durable record to tear and no historical tail to
//! classify — the cases ADR 0039's crc32c exists for — and structural
//! decoding already refuses malformed input. A checksum would also not
//! authenticate anything: the pipe is reachable only by the owning account,
//! so a hostile writer there is already inside the trust boundary.
//!
//! Reserved fields would guard against a reader that must skip what it does
//! not understand, and a checkpoint has no such reader: it describes exact
//! state, so a field an old decoder ignores is state it silently gets wrong.
//! The version field owns evolution instead. Padding buys no alignment in a
//! byte-oriented codec.
//!
//! # Size bound
//!
//! A cell costs at most `1 + 1 + 22 + 4 + 4 + 1 = 33` bytes. At the ADR 0041
//! cap of 512 columns by 256 rows that is at most 4.125 MiB per grid and
//! 8.25 MiB for both, plus a fixed header — inside the ADR's 12 MiB bound
//! with room to spare. [`MAX_CHECKPOINT_LEN`] states the bound and
//! `checkpoint_at_max_dimensions_is_within_budget` proves it against a
//! genuinely worst-case screen rather than a typical one.

/// Identifies a checkpoint payload. Present so a mis-routed frame fails
/// loudly at the first byte instead of being decoded as a grid.
pub(crate) const MAGIC: &[u8; 8] = b"SOTVT100";

/// The format version this build writes, and the only one it reads.
pub(crate) const VERSION: u16 = 1;

/// Maximum rows, from the ADR 0041 resource budget. The matching lower
/// bound is `grid::MIN_ROWS`, and it is a different kind of rule — see there.
pub(crate) const MAX_ROWS: u16 = 256;

/// Maximum columns, from the ADR 0041 resource budget.
pub(crate) const MAX_COLS: u16 = 512;

/// Worst-case encoded size of a single cell: flags, length byte, the full
/// 22-byte content field, two RGB colors with their tags, and the text mode.
const MAX_CELL_LEN: usize = 1 + 1 + 22 + 4 + 4 + 1;

/// Widens a dimension for the size arithmetic below.
///
/// The crate warns on every `as` conversion to catch silent narrowing. This
/// is the opposite — `u16` to `usize` is lossless on every platform this
/// builds for — so the conversion is confined to one justified place rather
/// than sprinkled through the constants.
#[allow(clippy::as_conversions)]
const fn as_len(v: u16) -> usize {
    v as usize
}

/// Worst-case encoded size of a row: the wrap flag plus a full complement of
/// worst-case cells.
const MAX_ROW_LEN: usize = 1 + as_len(MAX_COLS) * MAX_CELL_LEN;

/// Fixed per-grid overhead ahead of the row records: both cursors, the
/// scroll region, and the two origin-mode flags.
const GRID_HEADER_LEN: usize = 4 + 4 + 2 + 2 + 1 + 1;

/// Fixed header overhead: the magic through `saved_attrs`, taking both
/// attribute blocks at their worst case.
const HEADER_LEN: usize = 8 + 2 + 2 + 2 + 1 + 1 + 1 + 9 + 9;

/// The proven upper bound on a checkpoint, in bytes.
///
/// ADR 0041 budgets under 12 MiB for a checkpoint. This constant is that
/// bound computed from the format rather than measured, so it cannot drift
/// away from the encoder.
pub const MAX_CHECKPOINT_LEN: usize =
    HEADER_LEN + 2 * (GRID_HEADER_LEN + as_len(MAX_ROWS) * MAX_ROW_LEN);

const _: () = assert!(MAX_CHECKPOINT_LEN < 12 * 1024 * 1024);

/// Why a checkpoint could not be written or restored.
///
/// Three outcomes, because three is what a caller can act on. Only
/// [`UnsupportedVersion`](Self::UnsupportedVersion) changes anyone's
/// behavior — an attach negotiating versions refuses the connection rather
/// than retrying — so it is the only one carrying data. Everything else is a
/// payload that will not decode or a screen that will not encode, and the
/// string says which so a log is still worth reading.
///
/// Restore is fail-closed: every rejection here is a refusal to build a
/// `Screen`, never a partially-applied one, and no input — corrupt,
/// truncated, or hostile — reaches a panic.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CheckpointError {
    /// The payload announces a format version this build cannot read.
    UnsupportedVersion(u16),
    /// The payload is not a checkpoint this build can decode.
    Malformed(&'static str),
    /// The screen cannot be expressed in this format.
    Unrepresentable(&'static str),
}

impl std::fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion(v) => write!(
                f,
                "unsupported checkpoint version {v}, this build reads {VERSION}"
            ),
            Self::Malformed(what) => write!(f, "malformed checkpoint: {what}"),
            Self::Unrepresentable(what) => {
                write!(f, "screen cannot be checkpointed: {what}")
            }
        }
    }
}

impl std::error::Error for CheckpointError {}

/// Cursor over a checkpoint payload.
///
/// Every accessor is bounds-checked and returns [`CheckpointError`] rather
/// than panicking, so a decode of arbitrary bytes is safe by construction.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub(crate) fn take(
        &mut self,
        n: usize,
    ) -> Result<&'a [u8], CheckpointError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|end| *end <= self.buf.len())
            .ok_or(CheckpointError::Malformed(
                "payload ended before the checkpoint was complete",
            ))?;
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, CheckpointError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, CheckpointError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// Reads a byte that encodes a boolean, refusing any other value. A
    /// permissive `!= 0` would silently accept payloads this encoder can
    /// never produce, which is how format drift goes unnoticed.
    pub(crate) fn bool(
        &mut self,
        field: &'static str,
    ) -> Result<bool, CheckpointError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(CheckpointError::Malformed(field)),
        }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    pub(crate) fn finish(self) -> Result<(), CheckpointError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(CheckpointError::Malformed("bytes remained after the checkpoint"))
        }
    }
}

/// Appends a little-endian `u16`.
pub(crate) fn write_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
