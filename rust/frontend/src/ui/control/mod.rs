//! Agent control of the window: the fe.command route, FeCommand and its dispatch, the
//! sot_ui nav envelope, the fe-commands file channel and fe-state.json.

use super::*;

mod command;
mod dispatch;
mod envelope;
mod file_channel;

pub(in crate::ui) use command::*;
pub(in crate::ui) use envelope::*;
pub(in crate::ui) use file_channel::*;
