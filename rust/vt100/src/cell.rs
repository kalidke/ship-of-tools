use unicode_width::UnicodeWidthChar as _;

// chosen to make the size of the cell struct 32 bytes
const CONTENT_BYTES: usize = 22;

const IS_WIDE: u8 = 0b1000_0000;
const IS_WIDE_CONTINUATION: u8 = 0b0100_0000;
const LEN_BITS: u8 = 0b0001_1111;

/// Represents a single terminal cell.
#[derive(Clone, Debug, Eq)]
pub struct Cell {
    contents: [u8; CONTENT_BYTES],
    len: u8,
    attrs: crate::attrs::Attrs,
}
const _: () = assert!(std::mem::size_of::<Cell>() == 32);

impl PartialEq<Self> for Cell {
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len {
            return false;
        }
        if self.attrs != other.attrs {
            return false;
        }
        let len = self.len();
        self.contents[..len] == other.contents[..len]
    }
}

impl Cell {
    pub(crate) fn new() -> Self {
        Self {
            contents: Default::default(),
            len: 0,
            attrs: crate::attrs::Attrs::default(),
        }
    }

    fn len(&self) -> usize {
        usize::from(self.len & LEN_BITS)
    }

    pub(crate) fn set(&mut self, c: char, a: crate::attrs::Attrs) {
        self.len = 0;
        self.append_char(0, c);
        // strings in this context should always be an arbitrary character
        // followed by zero or more zero-width characters, so we should only
        // have to look at the first character
        self.set_wide(c.width().unwrap_or(1) > 1);
        self.attrs = a;
    }

    pub(crate) fn append(&mut self, c: char) {
        let len = self.len();
        if len >= CONTENT_BYTES - 4 {
            return;
        }
        if len == 0 {
            self.contents[0] = b' ';
            self.len += 1;
        }

        // we already checked that we have space for another codepoint
        self.append_char(self.len(), c);
    }

    // Writes bytes representing c at start
    // Requires caller to verify start <= CODEPOINTS_IN_CELL * 4
    fn append_char(&mut self, start: usize, c: char) {
        c.encode_utf8(&mut self.contents[start..]);
        self.len += u8::try_from(c.len_utf8()).unwrap();
    }

    pub(crate) fn clear(&mut self, attrs: crate::attrs::Attrs) {
        self.len = 0;
        self.attrs = attrs;
    }

    /// Returns the text contents of the cell.
    ///
    /// Can include multiple unicode characters if combining characters are
    /// used. Parsing puts at most one character of non-zero width here; a
    /// cell restored from a checkpoint may hold more, because restore does
    /// not consult a width table (see `Row::check_invariants` for why) and
    /// width tables shift between `unicode-width` releases in any case.
    // Since contents has been constructed by appending chars encoded as UTF-8 it will be valid UTF-8
    #[allow(clippy::missing_panics_doc)]
    #[must_use]
    pub fn contents(&self) -> &str {
        std::str::from_utf8(&self.contents[..self.len()]).unwrap()
    }

    /// Returns whether the cell contains any text data.
    #[must_use]
    pub fn has_contents(&self) -> bool {
        self.len() > 0
    }

    /// Returns whether the text data in the cell represents a wide character.
    #[must_use]
    pub fn is_wide(&self) -> bool {
        self.len & IS_WIDE != 0
    }

    /// Returns whether the cell contains the second half of a wide character
    /// (in other words, whether the previous cell in the row contains a wide
    /// character)
    #[must_use]
    pub fn is_wide_continuation(&self) -> bool {
        self.len & IS_WIDE_CONTINUATION != 0
    }

    fn set_wide(&mut self, wide: bool) {
        if wide {
            self.len |= IS_WIDE;
        } else {
            self.len &= !IS_WIDE;
        }
    }

    pub(crate) fn set_wide_continuation(&mut self, wide: bool) {
        if wide {
            self.len |= IS_WIDE_CONTINUATION;
        } else {
            self.len &= !IS_WIDE_CONTINUATION;
        }
    }

    pub(crate) fn attrs(&self) -> &crate::attrs::Attrs {
        &self.attrs
    }

    /// Returns the foreground color of the cell.
    #[must_use]
    pub fn fgcolor(&self) -> crate::Color {
        self.attrs.fgcolor
    }

    /// Returns the background color of the cell.
    #[must_use]
    pub fn bgcolor(&self) -> crate::Color {
        self.attrs.bgcolor
    }

    /// Returns whether the cell should be rendered with the bold text
    /// attribute.
    #[must_use]
    pub fn bold(&self) -> bool {
        self.attrs.bold()
    }

    /// Returns whether the cell should be rendered with the dim text
    /// attribute.
    #[must_use]
    pub fn dim(&self) -> bool {
        self.attrs.dim()
    }

    /// Returns whether the cell should be rendered with the italic text
    /// attribute.
    #[must_use]
    pub fn italic(&self) -> bool {
        self.attrs.italic()
    }

    /// Returns whether the cell should be rendered with the underlined text
    /// attribute.
    #[must_use]
    pub fn underline(&self) -> bool {
        self.attrs.underline()
    }

    /// Returns whether the cell should be rendered with the inverse text
    /// attribute.
    #[must_use]
    pub fn inverse(&self) -> bool {
        self.attrs.inverse()
    }
}

// ---------------------------------------------------------------------------
// Checkpoint codec (ADR 0041 step 3). Format spec: `crate::checkpoint`.
// ---------------------------------------------------------------------------

/// The cell record carries a packed length byte and the content bytes.
const CELL_FLAG_LEN: u8 = 0b0000_0001;
/// The cell record carries non-default attributes.
const CELL_FLAG_ATTRS: u8 = 0b0000_0010;
/// Every cell-record flag this version defines.
const CELL_FLAGS_KNOWN: u8 = CELL_FLAG_LEN | CELL_FLAG_ATTRS;

/// Bits of the packed `len` byte that this crate assigns. Bit 5 is
/// deliberately unassigned upstream; refusing it on restore keeps the
/// unassigned bit genuinely unassigned instead of letting a corrupt payload
/// smuggle state through it.
const LEN_BYTE_KNOWN: u8 = IS_WIDE | IS_WIDE_CONTINUATION | LEN_BITS;

impl Cell {
    pub(crate) fn write_checkpoint(&self, out: &mut Vec<u8>) {
        let default_attrs = crate::attrs::Attrs::default();
        let mut flags = 0;
        // `len` is non-zero for a cell with contents *and* for an empty
        // wide-continuation cell, so test the packed byte rather than the
        // content length.
        if self.len != 0 {
            flags |= CELL_FLAG_LEN;
        }
        if self.attrs != default_attrs {
            flags |= CELL_FLAG_ATTRS;
        }
        out.push(flags);
        // An empty cell with default attributes — by far the most common
        // cell on any screen — costs exactly this one zero byte.
        if flags & CELL_FLAG_LEN != 0 {
            out.push(self.len);
            out.extend_from_slice(&self.contents[..self.len()]);
        }
        if flags & CELL_FLAG_ATTRS != 0 {
            self.attrs.write_checkpoint(out);
        }
    }

    pub(crate) fn read_checkpoint(
        r: &mut crate::checkpoint::Reader<'_>,
    ) -> Result<Self, crate::checkpoint::CheckpointError> {
        let flags = r.u8()?;
        if flags & !CELL_FLAGS_KNOWN != 0 {
            return Err(crate::checkpoint::CheckpointError::Malformed(
                "cell flags carry undefined bits",
            ));
        }
        let mut cell = Self::new();
        if flags & CELL_FLAG_LEN != 0 {
            let len = r.u8()?;
            // The encoder sets this flag only for a non-zero packed length,
            // so a zero here is a second spelling of the empty cell. Two
            // spellings of one screen would mean checkpoint bytes could
            // differ for identical state, which the golden test and every
            // byte-level comparison downstream rely on not happening.
            if len == 0 {
                return Err(crate::checkpoint::CheckpointError::Malformed(
                    "empty cell written with an explicit length",
                ));
            }
            if len & !LEN_BYTE_KNOWN != 0 {
                return Err(crate::checkpoint::CheckpointError::Malformed(
                    "cell length byte carries undefined bits",
                ));
            }
            // `LEN_BITS` allows 31 but the content field holds 22, so the
            // length has to be checked against the array, not the mask.
            let content_len = usize::from(len & LEN_BITS);
            if content_len > CONTENT_BYTES {
                return Err(crate::checkpoint::CheckpointError::Malformed(
                    "cell length exceeds the content field",
                ));
            }
            let contents = r.take(content_len)?;
            // `Cell::contents` hands out a `&str` via an unchecked-by-
            // construction `from_utf8().unwrap()`. Validating here is what
            // keeps that unwrap true for restored cells.
            std::str::from_utf8(contents).map_err(|_| {
                crate::checkpoint::CheckpointError::Malformed(
                    "cell contents are not valid UTF-8",
                )
            })?;
            cell.contents[..content_len].copy_from_slice(contents);
            cell.len = len;
        }
        if flags & CELL_FLAG_ATTRS != 0 {
            let attrs =
                crate::attrs::Attrs::read_checkpoint(r, "a cell's attributes are undefined")?;
            // Same reasoning as the length above: the encoder omits default
            // attributes, so spelling them out is a second encoding of one
            // cell.
            if attrs == crate::attrs::Attrs::default() {
                return Err(crate::checkpoint::CheckpointError::Malformed(
                    "default attributes written explicitly",
                ));
            }
            cell.attrs = attrs;
        }
        Ok(cell)
    }
}
