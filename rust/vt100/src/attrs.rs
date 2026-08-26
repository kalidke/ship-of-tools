/// Represents a foreground or background color for cells.
#[derive(Eq, PartialEq, Debug, Copy, Clone, Default)]
pub enum Color {
    /// The default terminal color.
    #[default]
    Default,

    /// An indexed terminal color.
    Idx(u8),

    /// An RGB terminal color. The parameters are (red, green, blue).
    Rgb(u8, u8, u8),
}

const TEXT_MODE_INTENSITY: u8 = 0b0000_0011;
const TEXT_MODE_BOLD: u8 = 0b0000_0001;
const TEXT_MODE_DIM: u8 = 0b0000_0010;
const TEXT_MODE_ITALIC: u8 = 0b0000_0100;
const TEXT_MODE_UNDERLINE: u8 = 0b0000_1000;
const TEXT_MODE_INVERSE: u8 = 0b0001_0000;

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attrs {
    pub fgcolor: Color,
    pub bgcolor: Color,
    pub mode: u8,
}

impl Attrs {
    pub fn bold(&self) -> bool {
        self.mode & TEXT_MODE_BOLD != 0
    }

    pub fn dim(&self) -> bool {
        self.mode & TEXT_MODE_DIM != 0
    }

    pub fn set_bold(&mut self) {
        self.mode &= !TEXT_MODE_INTENSITY;
        self.mode |= TEXT_MODE_BOLD;
    }

    pub fn set_dim(&mut self) {
        self.mode &= !TEXT_MODE_INTENSITY;
        self.mode |= TEXT_MODE_DIM;
    }

    pub fn set_normal_intensity(&mut self) {
        self.mode &= !TEXT_MODE_INTENSITY;
    }

    pub fn italic(&self) -> bool {
        self.mode & TEXT_MODE_ITALIC != 0
    }

    pub fn set_italic(&mut self, italic: bool) {
        if italic {
            self.mode |= TEXT_MODE_ITALIC;
        } else {
            self.mode &= !TEXT_MODE_ITALIC;
        }
    }

    pub fn underline(&self) -> bool {
        self.mode & TEXT_MODE_UNDERLINE != 0
    }

    pub fn set_underline(&mut self, underline: bool) {
        if underline {
            self.mode |= TEXT_MODE_UNDERLINE;
        } else {
            self.mode &= !TEXT_MODE_UNDERLINE;
        }
    }

    pub fn inverse(&self) -> bool {
        self.mode & TEXT_MODE_INVERSE != 0
    }

    pub fn set_inverse(&mut self, inverse: bool) {
        if inverse {
            self.mode |= TEXT_MODE_INVERSE;
        } else {
            self.mode &= !TEXT_MODE_INVERSE;
        }
    }

}

// ---------------------------------------------------------------------------
// Checkpoint codec (ADR 0041 step 3). Format spec: `crate::checkpoint`.
// ---------------------------------------------------------------------------

/// Tag for [`Color::Default`] on the wire.
const COLOR_TAG_DEFAULT: u8 = 0;
/// Tag for [`Color::Idx`] on the wire.
const COLOR_TAG_IDX: u8 = 1;
/// Tag for [`Color::Rgb`] on the wire.
const COLOR_TAG_RGB: u8 = 2;

/// Every text-mode bit this version defines. Bits outside the mask are
/// refused on restore rather than carried through as state no encoder here
/// could have produced.
const TEXT_MODE_KNOWN: u8 = TEXT_MODE_INTENSITY
    | TEXT_MODE_ITALIC
    | TEXT_MODE_UNDERLINE
    | TEXT_MODE_INVERSE;

impl Color {
    pub(crate) fn write_checkpoint(self, out: &mut Vec<u8>) {
        // Exhaustive by construction: adding a variant to `Color` breaks this
        // match, which is the point — a silently shifted tag would make an
        // old checkpoint decode as the wrong color.
        match self {
            Self::Default => out.push(COLOR_TAG_DEFAULT),
            Self::Idx(i) => {
                out.push(COLOR_TAG_IDX);
                out.push(i);
            }
            Self::Rgb(r, g, b) => {
                out.push(COLOR_TAG_RGB);
                out.extend_from_slice(&[r, g, b]);
            }
        }
    }

    pub(crate) fn read_checkpoint(
        r: &mut crate::checkpoint::Reader<'_>,
        field: &'static str,
    ) -> Result<Self, crate::checkpoint::CheckpointError> {
        match r.u8()? {
            COLOR_TAG_DEFAULT => Ok(Self::Default),
            COLOR_TAG_IDX => Ok(Self::Idx(r.u8()?)),
            COLOR_TAG_RGB => {
                let bytes = r.take(3)?;
                Ok(Self::Rgb(bytes[0], bytes[1], bytes[2]))
            }
            _ => Err(crate::checkpoint::CheckpointError::Malformed(field)),
        }
    }
}

impl Attrs {
    pub(crate) fn write_checkpoint(&self, out: &mut Vec<u8>) {
        self.fgcolor.write_checkpoint(out);
        self.bgcolor.write_checkpoint(out);
        out.push(self.mode);
    }

    /// `undefined_bits` is the diagnostic for a text-mode byte carrying bits
    /// this version does not define. It is a whole message, not a field
    /// label: with three error variants the string is the entire report, so
    /// a caller reading a log gets only what is written here.
    pub(crate) fn read_checkpoint(
        r: &mut crate::checkpoint::Reader<'_>,
        undefined_bits: &'static str,
    ) -> Result<Self, crate::checkpoint::CheckpointError> {
        let fgcolor =
            Color::read_checkpoint(r, "undefined foreground color tag")?;
        let bgcolor =
            Color::read_checkpoint(r, "undefined background color tag")?;
        let mode = r.u8()?;
        if mode & !TEXT_MODE_KNOWN != 0 {
            return Err(crate::checkpoint::CheckpointError::Malformed(
                undefined_bits,
            ));
        }
        // Bold and dim are mutually exclusive: every setter clears the
        // intensity bits before setting one, so both at once is a state no
        // parse can reach.
        //
        // Refusing it follows this codec's rule, which is narrower than
        // "only screens the parser could produce": restore refuses the
        // unreachable shapes it can rule out CHEAPLY AND CERTAINLY, and
        // accepts the rest. Some unreachable states are deliberately let
        // through (see `Row::check_invariants`) because ruling them out
        // would depend on a Unicode width table, or on arithmetic internal
        // to the parser, and a rule that refuses a real screen is worse
        // than one that admits a harmless odd one.
        if mode & TEXT_MODE_INTENSITY == TEXT_MODE_INTENSITY {
            return Err(crate::checkpoint::CheckpointError::Malformed(
                "bold and dim set together",
            ));
        }
        Ok(Self {
            fgcolor,
            bgcolor,
            mode,
        })
    }
}
