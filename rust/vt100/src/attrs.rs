use crate::term::BufWrite as _;

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

    fn intensity(&self) -> u8 {
        self.mode & TEXT_MODE_INTENSITY
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

    pub fn write_escape_code_diff(
        &self,
        contents: &mut Vec<u8>,
        other: &Self,
    ) {
        if self != other && self == &Self::default() {
            crate::term::ClearAttrs.write_buf(contents);
            return;
        }

        let attrs = crate::term::Attrs::default();

        let attrs = if self.fgcolor == other.fgcolor {
            attrs
        } else {
            attrs.fgcolor(self.fgcolor)
        };
        let attrs = if self.bgcolor == other.bgcolor {
            attrs
        } else {
            attrs.bgcolor(self.bgcolor)
        };
        let attrs = if self.intensity() == other.intensity() {
            attrs
        } else {
            attrs.intensity(match self.intensity() {
                0 => crate::term::Intensity::Normal,
                TEXT_MODE_BOLD => crate::term::Intensity::Bold,
                TEXT_MODE_DIM => crate::term::Intensity::Dim,
                _ => unreachable!(),
            })
        };
        let attrs = if self.italic() == other.italic() {
            attrs
        } else {
            attrs.italic(self.italic())
        };
        let attrs = if self.underline() == other.underline() {
            attrs
        } else {
            attrs.underline(self.underline())
        };
        let attrs = if self.inverse() == other.inverse() {
            attrs
        } else {
            attrs.inverse(self.inverse())
        };

        attrs.write_buf(contents);
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
            tag => Err(crate::checkpoint::CheckpointError::UnknownTag {
                field,
                tag,
            }),
        }
    }
}

impl Attrs {
    pub(crate) fn write_checkpoint(&self, out: &mut Vec<u8>) {
        self.fgcolor.write_checkpoint(out);
        self.bgcolor.write_checkpoint(out);
        out.push(self.mode);
    }

    pub(crate) fn read_checkpoint(
        r: &mut crate::checkpoint::Reader<'_>,
        field: &'static str,
    ) -> Result<Self, crate::checkpoint::CheckpointError> {
        let fgcolor = Color::read_checkpoint(r, "fgcolor")?;
        let bgcolor = Color::read_checkpoint(r, "bgcolor")?;
        let mode = r.u8()?;
        if mode & !TEXT_MODE_KNOWN != 0 {
            return Err(crate::checkpoint::CheckpointError::InvalidBits {
                field,
                value: mode,
            });
        }
        // Bold and dim are mutually exclusive: every setter clears the
        // intensity bits before setting one. Both bits at once is a state the
        // parser cannot reach, and `write_escape_code_diff` answers it with
        // `unreachable!()` — so accepting it here would turn a corrupt
        // checkpoint into a later panic.
        if mode & TEXT_MODE_INTENSITY == TEXT_MODE_INTENSITY {
            return Err(crate::checkpoint::CheckpointError::InvalidBits {
                field,
                value: mode,
            });
        }
        Ok(Self {
            fgcolor,
            bgcolor,
            mode,
        })
    }
}
