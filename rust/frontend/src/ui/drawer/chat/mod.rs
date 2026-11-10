//! The chat drawer's State side: forwards chat events, `bus.lost` and each link's change to the private owner
//! (`drawer.rs`, over the private `owner.rs`), and sends the requests it returns.

use std::time::Instant;

use crate::net::dial::HostKey;
use crate::net::transport::ChatEvt;
use super::super::*;
use crate::ui::input::keypress::KeyPress;

mod compose;
mod drawer;
mod owner;
mod project;
mod row_channel;
mod view;
pub(in crate::ui) use compose::{compose_rows, ComposeText, Edit};
pub(in crate::ui) use drawer::ChatDrawer;
pub(in crate::ui) use view::Step;
use owner::Out;
#[cfg(test)]
mod compose_tests;
#[cfg(test)]
mod project_tests;
#[cfg(test)]
mod row_channel_tests;
#[cfg(test)]
mod view_tests;

impl State {
    /// Sends what the owner asked for; a request that cannot be queued is dropped with a warning (the owner's pace, or
    /// the next connection, ask again).
    fn with_chat(&mut self, f: impl FnOnce(&mut ChatDrawer, Instant) -> Vec<Out>) {
        for (host, req) in f(&mut self.chat, Instant::now()) {
            if let Err(e) = self.send_to(&host, req) {
                tracing::warn!(error = %e, %host, "drop chat request — channel closed");
            }
        }
    }

    /// The person edits the draft of the row the chat shows, read at this key; a refusal goes to the status line.
    pub(in crate::ui) fn edit_chat_draft(&mut self, edit: Edit) {
        let row = wanted_row(self.layout_help.chat_shown(), || self.active_result_workspace());
        if let Err(words) = self.chat.edit(row.as_ref(), edit) {
            self.status.set(words);
        }
        self.window.request_redraw();
    }

    pub(in crate::ui) fn on_chat(&mut self, host: &HostKey, evt: ChatEvt) {
        self.with_chat(|c, now| c.on_evt(host, evt, now));
    }

    pub(in crate::ui) fn on_chat_bus_lost(&mut self, host: &HostKey, bus: &str) {
        self.with_chat(|c, now| c.on_bus_lost(host, bus, now));
    }

    pub(in crate::ui) fn on_chat_connection(&mut self, host: &HostKey, up: bool) {
        self.with_chat(|c, now| c.on_connection(host, up, now));
    }

    pub(in crate::ui) fn tick_chat(&mut self) {
        let wanted = wanted_row(self.layout_help.chat_shown(), || self.active_result_workspace());
        let mut settling = false;
        self.with_chat(|c, now| {
            let (outs, wait) = c.tick(wanted, now);
            settling = wait;
            outs
        });
        if settling {
            self.pacer.mark_dirty();
        }
    }
}

/// A reader's move through the chat.
#[derive(Debug, PartialEq)]
pub(in crate::ui) enum Move {
    Scroll(bool, Step),
    Latest,
}

/// What a key does in the Chat tenant (`DrawerInput::Chat`): PageUp and PageDown move a third of the body and End returns
/// to the newest (Alt+Up and Alt+Down are `line_scroll_target`'s, the wheel `chat_wheel`'s).
pub(in crate::ui) fn chat_move(action: Option<Action>) -> Option<Move> {
    match action? {
        Action::ScrollPageUp => Some(Move::Scroll(true, Step::Page)),
        Action::ScrollPageDown => Some(Move::Scroll(false, Step::Page)),
        Action::ChatLatest => Some(Move::Latest),
        _ => None,
    }
}

/// The wheel moves one line in the Chat tenant; `rows_above` is positive toward older records.
pub(in crate::ui) fn chat_wheel(rows_above: i32) -> Option<Move> {
    (rows_above != 0).then_some(Move::Scroll(rows_above > 0, Step::Line))
}

/// The edit a key makes to the draft, when it makes one: a typed character or Space only where the chord types text
/// (`chord_types_text`), Backspace, and `chat.newline`; a key that resolved to another action matches no arm.
pub(in crate::ui) fn draft_edit(action: Option<Action>, key: &Key, (ctrl, alt, super_): (bool, bool, bool)) -> Option<Edit<'_>> {
    match (action, key) {
        (Some(Action::ChatNewline), _) => Some(Edit::Newline),
        (None, Key::Named(NamedKey::Backspace)) => Some(Edit::Backspace),
        (None, Key::Named(NamedKey::Space)) if chord_types_text(ctrl, alt, super_) => Some(Edit::Space),
        (None, Key::Character(s)) if chord_types_text(ctrl, alt, super_) => Some(Edit::Text(s.as_str())),
        _ => None,
    }
}

/// A key or paste in the Chat tenant (`DrawerInput::Chat`) edits the draft; whether it was taken.
pub(in crate::ui) fn chat_key(state: &mut State, key: KeyPress<'_>, label: String) -> bool {
    let KeyPress { event, action, ctrl, alt, super_, .. } = key;
    if action == Some(Action::Paste) {
        if let Some(text) = (!event.repeat).then(read_clipboard_text).flatten() {
            state.edit_chat_draft(Edit::Text(&text));
        }
    } else if let Some(edit) = draft_edit(action, &event.logical_key, (ctrl, alt, super_)) {
        state.edit_chat_draft(edit);
    } else {
        return false;
    }
    state.last_key = Some(label);
    true
}

/// Applies a move; whether there was one.
pub(in crate::ui) fn chat_apply(state: &mut State, mv: Option<Move>) -> bool {
    match mv {
        Some(Move::Scroll(up, step)) => state.chat.scroll(up, step),
        Some(Move::Latest) => state.chat.latest(),
        None => return false,
    }
    state.window.request_redraw();
    true
}

/// The row the chat follows: the selected row's host and canonical id (never the slug, which the daemon refuses as
/// stale), and only while the chat is shown. The selected row is not even resolved otherwise.
pub(in crate::ui) fn wanted_row(shown: bool, selected: impl FnOnce() -> Option<ResolvedWorkspace>) -> Option<ResultRowIdentity> {
    shown.then(selected).flatten().map(|row| row.identity().clone())
}
