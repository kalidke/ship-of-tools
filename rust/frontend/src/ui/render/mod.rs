//! The window's pixels: glyph text, the cell backend, textured quads, the surface and
//! capture. Knows no panes or modes.

use super::*;

pub(crate) mod cells;
pub(in crate::ui) mod brushes;
pub(in crate::ui) mod canvas;
pub(in crate::ui) mod gpu;
pub(crate) mod quad;
pub(crate) mod text;
mod capture;
mod pass;
pub(in crate::ui) mod surface;

pub(in crate::ui) use capture::*;
pub(in crate::ui) use surface::*;
