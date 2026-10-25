//! Input: the action catalog and its key chords, contextual help, clipboard paste.

use super::*;

pub(in crate::ui) mod help;
pub(crate) mod keybindings;
mod help_drawer;
mod paste;
pub(in crate::ui) mod mouse;
pub(in crate::ui) mod keypress;
pub(in crate::ui) mod global_keys;

pub(in crate::ui) use paste::*;

pub(in crate::ui) mod layout_help;
pub(in crate::ui) use layout_help::LayoutHelp;

#[cfg(test)]
mod layout_help_tests;

mod focus_prompts;
pub(in crate::ui) use focus_prompts::{FocusPrompts, NavPrompt, PromptEffect};

#[cfg(test)]
mod focus_prompts_tests;
