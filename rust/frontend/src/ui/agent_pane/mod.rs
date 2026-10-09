//! The agent pane: its screen choice, attach client with warm pool, and input.

use super::*;

mod attach;
mod input;
mod presentation;
mod replies;
pub(in crate::ui) mod keys;
mod screen;

pub(in crate::ui) use attach::*;
pub(in crate::ui) use presentation::*;
pub(in crate::ui) use screen::*;
