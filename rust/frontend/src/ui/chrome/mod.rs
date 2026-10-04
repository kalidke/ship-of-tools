//! The window's pane chrome: focus and panes, the status line, nav spill, colours and the wireframe.

use super::*;

pub(crate) mod layout;
mod panes;
mod spill;
mod status;
mod theme;

pub(in crate::ui) use panes::*;
pub(in crate::ui) use spill::*;
pub(in crate::ui) use status::*;
pub(in crate::ui) use theme::*;
pub(in crate::ui) use layout::draw_wireframe;
