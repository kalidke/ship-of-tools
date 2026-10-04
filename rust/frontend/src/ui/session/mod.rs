//! The session view: which session row the window is on, its key, strip order, snapshots, switch, picker and presence.

use super::*;

mod badge;
mod picker;
mod presence;
mod snapshot;
mod switch;
mod workspace_key;
mod workspace_list;

pub(in crate::ui) use picker::*;
pub(in crate::ui) use presence::*;
pub(in crate::ui) use snapshot::*;
#[cfg(test)]
pub(in crate::ui) use workspace_key::*;
pub(in crate::ui) use workspace_list::*;
