//! The chat drawer's State side: forwards chat events, `bus.lost` and each link's change to the private owner
//! (`drawer.rs`, over the private `owner.rs`), and sends the requests it returns.

use std::time::Instant;

use crate::net::dial::HostKey;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use sot_protocol::ops::{ChatCode, ChatPostRes};
use super::super::*;
use crate::ui::input::keypress::KeyPress;
use crate::ui::persist::chat_name::save_chat_name;
use crate::ui::persist::settings::Settings;
use sot_platform::host::env::Env;

mod compose;
mod drawer;
mod owner;
mod project;
mod row_channel;
mod thread;
mod view;
pub(in crate::ui) use compose::{compose_rows, ComposeText, Edit};
pub(in crate::ui) use drawer::ChatDrawer;
pub(in crate::ui) use view::{ChatHit, Step};
#[cfg(test)]
pub(in crate::ui) use view::ChatView;
use owner::Out;
#[cfg(test)]
mod compose_tests;
#[cfg(test)]
mod project_tests;
#[cfg(test)]
mod row_channel_tests;
#[cfg(test)]
mod thread_tests;
#[cfg(test)]
mod view_tests;
/// The chat's native State fixture: a `State` exists only inside a native window, so it runs in the window_minimized
/// target's fixtures child (`ui/app/native_children.rs`).
#[cfg(all(test, feature = "test-window-progress"))]
#[path = "native_fixture_tests.rs"]
mod native_fixture;

impl State {
    /// Sends what the owner asked for; a request that cannot be queued is dropped with a warning (the owner's pace, or
    /// the next connection, ask again).
    fn with_chat(&mut self, f: impl FnOnce(&mut ChatDrawer, Instant) -> Vec<Out>) {
        for (host, req) in f(&mut self.chat, Instant::now()) {
            let post = if let OutgoingReq::ChatPost { req } = &req { Some(req.clone()) } else { None };
            if let Err(e) = self.send_to(&host, req) {
                tracing::warn!(error = %e, %host, "drop chat request — channel closed");
                // A post the channel refused was never written: the owner reads it as refused, not as waiting for a reply.
                if let Some(req) = post {
                    let reply = ChatReply::Done(ChatPostRes::Refused { code: ChatCode::Unavailable });
                    self.chat.on_evt(&host, ChatEvt::Posted { req, reply }, Instant::now());
                }
            }
        }
        if let Some(words) = self.chat.take_notice() {
            self.status.set(words);
        }
    }

    /// Enter in the Chat tenant: posts the draft of the row the chat shows, under `[chat] name`; a refusal goes to the status line.
    pub(in crate::ui) fn send_chat_draft(&mut self) {
        let row = wanted_row(self.layout_help.chat_shown(), || self.active_result_workspace());
        let name = self.settings.chat_name.clone();
        let mut refused = None;
        self.with_chat(|c, now| c.send(row.as_ref(), name.as_deref(), now).unwrap_or_else(|words| {
            refused = Some(words);
            Vec::new()
        }));
        if let Some(words) = refused {
            self.status.set(words);
        }
        self.window.request_redraw();
    }

    /// The person edits the draft of the row the chat shows, read at this key; a refusal goes to the status line.
    pub(in crate::ui) fn edit_chat_draft(&mut self, edit: Edit) {
        let row = wanted_row(self.layout_help.chat_shown(), || self.active_result_workspace());
        if let Err(words) = self.chat.edit(row.as_ref(), edit) {
            self.status.set(words);
        }
        self.window.request_redraw();
    }

    /// Enter on the name prompt (`apply_name`): the status line says the outcome.
    fn save_chat_name(&mut self) {
        let words = apply_name(&mut self.chat, &mut self.settings, &Env::process());
        self.status.set(words);
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
    Mark(bool),
    Thread,
}

/// What a key does in the Chat tenant (`DrawerInput::Chat`): PageUp and PageDown move a third of the body, End returns
/// to the newest and ArrowUp and ArrowDown move the mark (Alt+Up and Alt+Down are `line_scroll_target`'s, the wheel `chat_wheel`'s).
pub(in crate::ui) fn chat_move(action: Option<Action>) -> Option<Move> {
    match action? {
        Action::ScrollPageUp => Some(Move::Scroll(true, Step::Page)),
        Action::ScrollPageDown => Some(Move::Scroll(false, Step::Page)),
        Action::ChatLatest => Some(Move::Latest),
        Action::ChatMarkOlder => Some(Move::Mark(true)),
        Action::ChatMarkNewer => Some(Move::Mark(false)),
        Action::ChatThreadOpen => Some(Move::Thread),
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

/// What a key does to the name prompt.
#[derive(Debug, PartialEq)]
pub(in crate::ui) enum NameStep<'a> {
    Save,
    Cancel,
    Paste,
    Edit(Edit<'a>),
    /// A held Enter, Escape or paste: taken, and nothing done.
    Held,
}

/// What a key is while the foot asks for the name, when it is the prompt's: Enter saves, Escape cancels, paste and the
/// draft's edits edit the name; a reading key (scroll, mark, thread) is not (`None`), so it still acts.
pub(in crate::ui) fn name_step<'a>(action: Option<Action>, key: &'a Key, repeat: bool, mods: (bool, bool, bool)) -> Option<NameStep<'a>> {
    match action {
        Some(Action::ChatSend | Action::ReturnNav | Action::Paste) if repeat => Some(NameStep::Held),
        Some(Action::ChatSend) => Some(NameStep::Save),
        Some(Action::ReturnNav) => Some(NameStep::Cancel),
        Some(Action::Paste) => Some(NameStep::Paste),
        _ => draft_edit(action, key, mods).map(NameStep::Edit),
    }
}

/// Saves the typed name through `save_chat_name`; the words of the outcome. On success the prompt closes and the setting
/// is the name; on a refusal or a failed write the prompt stays open and the setting is as it was.
pub(in crate::ui) fn apply_name(chat: &mut ChatDrawer, settings: &mut Settings, env: &Env) -> String {
    match save_chat_name(settings, chat.asking().unwrap_or_default(), env) {
        Ok(path) => {
            chat.cancel_name();
            format!("chat name saved to {}", path.display())
        }
        Err(words) => words,
    }
}

/// A key while the foot asks for the posting name (`name_step`); whether it was taken.
pub(in crate::ui) fn name_key(state: &mut State, key: &KeyPress<'_>, label: &str) -> bool {
    if state.chat.asking().is_none() {
        return false;
    }
    let KeyPress { event, action, ctrl, alt, super_, .. } = *key;
    let Some(step) = name_step(action, &event.logical_key, event.repeat, (ctrl, alt, super_)) else { return false };
    let refused = match step {
        NameStep::Save => {
            state.save_chat_name();
            None
        }
        NameStep::Cancel => {
            state.chat.cancel_name();
            None
        }
        NameStep::Paste => read_clipboard_text().and_then(|text| state.chat.edit_name(Edit::Text(&text)).err()),
        NameStep::Edit(edit) => state.chat.edit_name(edit).err(),
        NameStep::Held => None,
    };
    if let Some(words) = refused {
        state.status.set(words);
    }
    state.last_key = Some(label.to_string());
    state.window.request_redraw();
    true
}

/// A key or paste in the Chat tenant (`DrawerInput::Chat`) edits the draft, and `chat.send` posts it; whether it was taken.
pub(in crate::ui) fn chat_key(state: &mut State, key: KeyPress<'_>, label: String) -> bool {
    let KeyPress { event, action, ctrl, alt, super_, .. } = key;
    if action == Some(Action::ChatSend) {
        if !event.repeat {
            state.send_chat_draft();
        }
    } else if action == Some(Action::ChatCopySelection) {
        if !event.repeat {
            state.status.set(copy_status(state.chat.copy_text(), |text| write_clipboard_text(text).map_err(|e| e.to_string())));
        }
    } else if action == Some(Action::Paste) {
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

/// The status line's words for `chat.copy_selection`: the text goes to the clipboard, and nothing to copy is said.
fn copy_status(text: Option<String>, clipboard: impl FnOnce(String) -> Result<(), String>) -> String {
    match text.map(clipboard) {
        Some(Ok(())) => "copied chat text".into(),
        Some(Err(e)) => format!("copy failed · {e}"),
        None => "nothing to copy: select text or mark a message".into(),
    }
}

/// Applies a move; whether there was one.
pub(in crate::ui) fn chat_apply(state: &mut State, mv: Option<Move>) -> bool {
    match mv {
        Some(Move::Scroll(up, step)) => state.chat.scroll(up, step),
        Some(Move::Latest) => state.chat.latest(),
        Some(Move::Mark(older)) => state.chat.mark(older),
        Some(Move::Thread) => state.with_chat(|c, _| c.open_thread()),
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
