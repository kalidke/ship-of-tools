//! Window-wide focus, prompts and close notices. No operation borrows State.

use super::*;
use std::time::Instant;

/// Immutable prompt observation; storage and editing belong to FocusPrompts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::ui) enum NavPrompt {
    CreateFile { dir_node_id: String, input: String },
    ConfirmDelete { node_id: String, label: String },
    ConfirmQuit { keep: bool },
    /// The raw/original pixel size in nanometers, sent without conversion.
    ScaleEntry { node_id: String, input: String },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::ui) struct PromptEffect {
    pub(in crate::ui) status: Option<String>,
    pub(in crate::ui) redraw: bool,
}
impl PromptEffect {
    pub(in crate::ui) fn message(status: impl Into<String>) -> Self {
        Self { status: Some(status.into()), redraw: true }
    }
}

pub(in crate::ui) struct FocusPrompts {
    focus: PaneFocus,
    nav_prompt: Option<NavPrompt>,
    // Leave retains this even after removing the scale prompt.
    scale_entry_prior_focus: Option<PaneFocus>,
    relaunch_stayed: bool,
    quit_hint: Option<(String, Instant)>,
}

impl FocusPrompts {
    pub(in crate::ui) fn new(focus: PaneFocus) -> Self {
        Self { focus, nav_prompt: None, scale_entry_prior_focus: None,
            relaunch_stayed: false, quit_hint: None }
    }
    pub(in crate::ui) fn focus(&self) -> PaneFocus { self.focus }
    pub(in crate::ui) fn prompt(&self) -> Option<&NavPrompt> { self.nav_prompt.as_ref() }
    pub(in crate::ui) fn quit_choice(&self) -> Option<bool> {
        match self.prompt() { Some(NavPrompt::ConfirmQuit { keep }) => Some(*keep), _ => None }
    }
    pub(in crate::ui) fn stayed(&self) -> bool { self.relaunch_stayed }
    pub(in crate::ui) fn hint(&self) -> Option<&str> { self.quit_hint.as_ref().map(|(h, _)| h.as_str()) }
    pub(in crate::ui) fn hint_deadline(&self) -> Option<Instant> { self.quit_hint.as_ref().map(|(_, t)| *t) }

    pub(in crate::ui) fn set_focus(&mut self, to: PaneFocus) {
        self.focus = to;
    }
    pub(in crate::ui) fn move_focus(&mut self, dir: SpatialDir,
        columns: &[crate::ui::persist::settings::Slot], drawer: Option<crate::ui::persist::settings::Slot>) {
        self.set_focus(self.focus.move_in(dir, columns, drawer));
    }
    pub(in crate::ui) fn open_create(&mut self, dir_node_id: String) -> PromptEffect {
        self.nav_prompt = Some(NavPrompt::CreateFile { dir_node_id, input: String::new() });
        PromptEffect::message("new file or dir/: ")
    }
    pub(in crate::ui) fn open_delete(&mut self, node_id: String, label: String) -> PromptEffect {
        let effect = PromptEffect::message(format!("delete {label}? [y/N]"));
        self.nav_prompt = Some(NavPrompt::ConfirmDelete { node_id, label });
        effect
    }
    pub(in crate::ui) fn push_char(&mut self, c: char) -> bool {
        match self.nav_prompt.as_mut() {
            Some(NavPrompt::CreateFile { input, .. }) => {
                if !name_char_allowed(input, c) { return false; }
                input.push(c);
                true
            }
            Some(NavPrompt::ScaleEntry { input, .. }) => {
                if c.is_ascii_digit() || (c == '.' && !input.contains('.')) {
                    input.push(c);
                    true
                } else { false }
            }
            _ => false,
        }
    }
    pub(in crate::ui) fn backspace(&mut self) -> bool {
        match self.nav_prompt.as_mut() {
            Some(NavPrompt::CreateFile { input, .. }) | Some(NavPrompt::ScaleEntry { input, .. }) => {
                input.pop();
                true
            }
            _ => false,
        }
    }
    pub(in crate::ui) fn create_draft(&self) -> Option<(String, String)> {
        match self.prompt() {
            Some(NavPrompt::CreateFile { dir_node_id, input }) => Some((dir_node_id.clone(), input.trim().to_string())),
            _ => None,
        }
    }
    pub(in crate::ui) fn delete_target(&self) -> Option<(String, String)> {
        match self.prompt() {
            Some(NavPrompt::ConfirmDelete { node_id, label }) => Some((node_id.clone(), label.clone())),
            _ => None,
        }
    }
    pub(in crate::ui) fn scale_draft(&self) -> Option<(String, String)> {
        match self.prompt() {
            Some(NavPrompt::ScaleEntry { node_id, input }) => Some((node_id.clone(), input.trim().to_string())),
            _ => None,
        }
    }
    pub(in crate::ui) fn complete_create(&mut self, sent: bool) {
        if sent { self.nav_prompt = None; }
    }
    pub(in crate::ui) fn complete_delete(&mut self) {
        self.nav_prompt = None;
    }
    pub(in crate::ui) fn begin_scale(&mut self, node_id: String) -> PromptEffect {
        self.nav_prompt = Some(NavPrompt::ScaleEntry { node_id, input: String::new() });
        self.scale_entry_prior_focus = Some(self.focus);
        self.set_focus(PaneFocus::NavTree);
        PromptEffect::message("pixel size (nm): ")
    }
    /// Success closes before the save marker; restoration follows it.
    pub(in crate::ui) fn complete_scale(&mut self) {
        self.nav_prompt = None;
    }
    pub(in crate::ui) fn restore_scale_focus(&mut self) {
        if let Some(prior) = self.scale_entry_prior_focus.take() { self.set_focus(prior); }
    }
    pub(in crate::ui) fn cancel(&mut self) -> PromptEffect {
        let status = match self.prompt() {
            Some(NavPrompt::ConfirmDelete { .. }) => "delete · cancelled",
            Some(NavPrompt::ConfirmQuit { .. }) => "quit · cancelled",
            Some(NavPrompt::ScaleEntry { .. }) => "pixel size · cancelled",
            _ => "new file · cancelled",
        };
        self.nav_prompt = None;
        self.restore_scale_focus();
        PromptEffect::message(status)
    }
    pub(in crate::ui) fn ask_quit(&mut self, keep: bool) -> bool {
        if matches!(self.prompt(), Some(p) if !matches!(p, NavPrompt::ConfirmQuit { .. })) {
            tracing::info!(target: "sot::ui::app::exit", "quit prompt: refused, another prompt is open");
            return false;
        }
        self.nav_prompt = Some(NavPrompt::ConfirmQuit { keep });
        tracing::info!(target: "sot::ui::app::exit", keep, "quit prompt: open");
        true
    }
    pub(in crate::ui) fn answer_quit(&mut self, step: QuitPromptStep) -> (PromptEffect, Option<LeaveIntent>) {
        match step {
            QuitPromptStep::Stay { keep } => {
                self.nav_prompt = Some(NavPrompt::ConfirmQuit { keep });
                (PromptEffect { redraw: true, ..Default::default() }, None)
            }
            QuitPromptStep::Cancel => (self.cancel(), None),
            QuitPromptStep::Leave(intent) => (PromptEffect::default(), Some(intent)),
            QuitPromptStep::Ignore => (PromptEffect::default(), None),
        }
    }
    pub(in crate::ui) fn begin_leave(&mut self) {
        self.nav_prompt = None;
        self.relaunch_stayed = false;
    }
    /// After lease stay succeeds.
    pub(in crate::ui) fn record_stay(&mut self, intent: LeaveIntent) {
        if intent == LeaveIntent::Keep { let _ = self.ask_quit(true); }
        self.relaunch_stayed = intent == LeaveIntent::Handover;
    }
    pub(in crate::ui) fn dismiss_stay_key(&mut self, consumes_text: bool) -> Option<bool> {
        std::mem::take(&mut self.relaunch_stayed).then_some(!consumes_text)
    }
    pub(in crate::ui) fn dismiss_stay_press(&mut self) -> bool {
        std::mem::take(&mut self.relaunch_stayed)
    }
    pub(in crate::ui) fn show_hint(&mut self, chord: &str, now: impl FnOnce() -> Instant) {
        let text = global_keys::quit_hint(chord);
        self.quit_hint = Some((text, now() + crate::ui::app::QUIT_HINT_HOLD));
    }
    pub(in crate::ui) fn expire_hint(&mut self, now: Instant) -> bool {
        if self.hint_deadline().is_some_and(|until| now >= until) {
            self.quit_hint = None;
            true
        } else { false }
    }
}

fn name_char_allowed(buf: &str, c: char) -> bool {
    if c == '\\' { return false; }
    if c == '/' { return !buf.is_empty() && !buf.ends_with('/'); }
    !buf.ends_with('/')
}
