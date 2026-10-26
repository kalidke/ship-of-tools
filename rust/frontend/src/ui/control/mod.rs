//! Agent control of the window: the fe.command route, FeCommand and its dispatch, the reply bar for a page's
//! offered reply, the sot_ui nav envelope, the fe-commands file channel and fe-state.json.

use super::*;

mod channel;
mod command;
mod dispatch;
mod envelope;
mod file_channel;
mod reply_bar;
mod reply_dispatch;
mod replies;

pub(in crate::ui) use channel::ControlChannel;
pub(in crate::ui) use command::*;
pub(in crate::ui) use envelope::*;
pub(in crate::ui) use file_channel::*;
pub(in crate::ui) use reply_bar::*;
pub(in crate::ui) use reply_dispatch::bar_key_kind;
