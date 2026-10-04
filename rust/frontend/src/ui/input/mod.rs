//! Input: the action catalog and its key chords, contextual help, clipboard paste.

use super::*;

pub(crate) mod help;
pub(crate) mod keybindings;
mod help_drawer;
mod paste;
pub(in crate::ui) mod mouse;
pub(in crate::ui) mod keypress;

pub(in crate::ui) use paste::*;
