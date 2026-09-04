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
//! saved), per-row wrap flags, the input modes (application keypad,
//! application cursor, cursor visibility, bracketed paste, mouse protocol
//! mode and encoding), and — since format version 2 — the normal grid's
//! bounded scrollback ring (see [`MAX_SCROLLBACK_ROWS`]). A capsule that
//! keeps a ring hands it over at the same instant as the visible screen, so
//! an attach restores both atomically from one instant rather than the
//! client reconstructing history from a separately-cut log tail.
//!
//! Not carried, each for a reason:
//!
//! * **Scrollback capacity and offset.** Capacity is the *restorer's*
//!   configuration, not the capsule's — so [`crate::Screen::restore`] takes
//!   it as an argument instead of reading it off the wire. The offset is
//!   restore-time UI state (where the restorer happens to have scrolled to),
//!   never part of the terminal state a checkpoint describes, so it is
//!   always zero immediately after a restore regardless of how many rows
//!   the ring carries.
//! * **The alternate grid's scrollback.** It has none to carry: nothing
//!   ever scrolls a row off the alternate screen into a ring (real
//!   full-screen programs redraw in place), so [`crate::Screen::checkpoint`]
//!   does not even emit a count field for it — see the format below.
//! * **vte parser state.** The escape-sequence state machine is now an
//!   owned vendored module (`src/vte`), not an external crate, but its
//!   state is still deliberately not part of the checkpoint format. The
//!   cut contract is enforced by [`crate::Parser::is_ground`] instead: the
//!   producer only ever cuts the attach stream at a ground-state boundary,
//!   so no partial escape sequence or codepoint ever crosses a cut.
//!
//! # Format (version 2, little-endian)
//!
//! ```text
//! header
//!   magic        8  b"SOTVT100"
//!   version      2  u16 = 1 (legacy, no scrollback field at all) or 2
//!   rows         2  u16, 2..=256
//!   cols         2  u16, 2..=512
//!   modes        1  u8, the Screen mode bitfield
//!   mouse_mode   1  u8, MouseProtocolMode tag
//!   mouse_enc    1  u8, MouseProtocolEncoding tag
//!   attrs           current SGR attributes
//!   saved_attrs     attributes saved by DECSC
//! body
//!   grid            the normal grid, WITH a scrollback field (version 2+)
//!   grid            the alternate grid, WITHOUT one, ever
//!
//! grid
//!   pos          4  u16 row, u16 col
//!   saved_pos    4  u16 row, u16 col
//!   scroll_top   2  u16
//!   scroll_bot   2  u16
//!   origin_mode  1  u8, 0 or 1
//!   saved_origin 1  u8, 0 or 1
//!   rows            `rows` row records, the visible screen
//!   scrollback      present iff this is the normal grid AND version >= 2 —
//!                   see below
//!
//! scrollback
//!   count        2  u16, 0..=200 (`MAX_SCROLLBACK_ROWS`)
//!   rows            `count` row records, oldest first (`VecDeque`'s own
//!                   front-to-back order, which is also `scroll_up`'s own
//!                   push order)
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
//! the pinned version goldens below would each be pinning one of several
//! right answers.
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
//! The version field owns evolution instead — [`VERSION`] is the only
//! version this build ever *writes*, but [`MIN_READABLE_VERSION`] through
//! [`VERSION`] are all versions it *reads*: a version 1 payload has no
//! scrollback field anywhere and restores with an empty ring, exactly as if
//! its (absent) count had been zero. Padding buys no alignment in a
//! byte-oriented codec.
//!
//! # Size bound
//!
//! A cell costs at most `1 + 1 + 22 + 4 + 4 + 1 = 33` bytes, so a row at the
//! ADR 0041 cap of 512 columns costs at most `1 + 512 * 33 = 16,897` bytes.
//! Both grids at the 256-row cap cost 8,651,327 bytes including the fixed
//! header — unchanged from before the scrollback ring existed, since the
//! alternate grid never carries one and the normal grid's own visible rows
//! didn't move.
//!
//! The ring adds one count field plus up to [`MAX_SCROLLBACK_ROWS`] more
//! rows at that same worst-case width. The capsule side first reached for
//! 1000 rows of history; at 16,897 bytes each that alone is `2 + 1000 *
//! 16,897 = 16,897,002` bytes (~16.1 MiB) — past the ADR's 12 MiB checkpoint
//! bound before the two grids' own ~8.25 MiB are even added, so 1000 does
//! not fit. 200 does: `2 + 200 * 16,897 = 3,379,402` bytes (~3.22 MiB),
//! bringing the total to 12,030,729 bytes (~11.47 MiB) — inside the 12 MiB
//! bound with about 539 KiB to spare. [`MAX_CHECKPOINT_LEN`] states the
//! bound and `checkpoint_at_max_dimensions_is_within_budget` proves it
//! against a genuinely worst-case screen — full ring included — rather than
//! a typical one.

/// Identifies a checkpoint payload. Present so a mis-routed frame fails
/// loudly at the first byte instead of being decoded as a grid.
pub(crate) const MAGIC: &[u8; 8] = b"SOTVT100";

/// The format version this build writes.
pub(crate) const VERSION: u16 = 2;

/// The oldest format version this build still reads. Version 1 checkpoints
/// predate the scrollback ring and carry no scrollback field at all, for
/// either grid — restoring one produces an empty ring, exactly as if a
/// version 2 payload's count field had read zero. There is no reason to
/// stop reading it: nothing about it is unsafe or ambiguous, it just
/// describes a screen with no history, which is a real and legal state
/// version 2 can describe too.
pub(crate) const MIN_READABLE_VERSION: u16 = 1;

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

/// Maximum scrollback ring rows a checkpoint may carry — the normal grid
/// only, never the alternate grid (see `Screen::checkpoint` and the module
/// doc's "Size bound" section for why 1000, the capsule side's first
/// choice, does not fit the ADR 0041 12 MiB checkpoint budget, and 200
/// does). A checkpoint whose ring is longer, or a live grid whose ring
/// somehow grew past this, is refused rather than truncated silently — see
/// [`crate::grid::Grid::write_checkpoint`] and
/// [`crate::grid::Grid::read_checkpoint`].
pub(crate) const MAX_SCROLLBACK_ROWS: u16 = 200;

/// Fixed cost of the scrollback ring's row count — carried once, by the
/// normal grid only. The alternate grid emits no count field at all (it has
/// no ring, ever), so this is not doubled the way [`GRID_HEADER_LEN`] is.
const RING_COUNT_LEN: usize = 2;

/// The proven upper bound on a checkpoint, in bytes.
///
/// ADR 0041 budgets under 12 MiB for a checkpoint. This constant is that
/// bound computed from the format rather than measured, so it cannot drift
/// away from the encoder. See the module doc's "Size bound" section for the
/// worked arithmetic (8,651,327 bytes for both grids' visible rows,
/// 3,379,402 more for a full 200-row ring, 12,030,729 total).
pub const MAX_CHECKPOINT_LEN: usize = HEADER_LEN
    + 2 * (GRID_HEADER_LEN + as_len(MAX_ROWS) * MAX_ROW_LEN)
    + RING_COUNT_LEN
    + as_len(MAX_SCROLLBACK_ROWS) * MAX_ROW_LEN;

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
                "unsupported checkpoint version {v}, this build reads \
                 {MIN_READABLE_VERSION}..={VERSION}"
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
