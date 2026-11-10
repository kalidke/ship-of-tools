//! Input: the action catalog and its key chords, contextual help, clipboard paste.

use super::*;

pub(in crate::ui) mod help;
pub(crate) mod keybindings;
mod help_drawer;
mod paste;
pub(in crate::ui) mod mouse;
pub(in crate::ui) mod pointer;
pub(in crate::ui) mod keypress;
pub(in crate::ui) mod global_keys;

pub(in crate::ui) use paste::*;

/// Whether a chord that resolved to no editor action types its character. A Command chord on macOS and a Ctrl chord
/// do not: winit's `logical_key` can carry the bare letter under Ctrl, so Ctrl+Q (the window's quit, reaching here
/// because the editor has the keys first) would type a "q". Ctrl with Alt is AltGr on Windows, which types.
pub(in crate::ui) fn chord_types_text(ctrl: bool, alt: bool, super_: bool) -> bool {
    !(cfg!(target_os = "macos") && super_) && !(ctrl && !alt)
}

pub(in crate::ui) mod layout_help;
pub(in crate::ui) use layout_help::LayoutHelp;

#[cfg(test)]
mod layout_help_tests;

mod focus_prompts;
pub(in crate::ui) use focus_prompts::{FocusPrompts, NavPrompt, PromptEffect};

#[cfg(test)]
mod focus_prompts_tests;
