//! The help drawer's state: the context it opens for, and opening and closing it.

use super::*;

impl State {
    pub(in crate::ui) fn help_peek_expired(&self) -> bool {
        self.help.peek.as_ref().is_some_and(|p| p.opacity(std::time::Instant::now()) <= 0.0)
    }

    pub(in crate::ui) fn help_context(&self) -> help::Context {
        let pane = match self.focus {
            PaneFocus::NavTree => help::Pane::Nav,
            PaneFocus::Preview => help::Pane::Preview,
            PaneFocus::Llm => help::Pane::Agent,
            PaneFocus::Repl => match self.drawer {
                DrawerContent::Terminal => help::Pane::Terminal,
                DrawerContent::Monitor => help::Pane::Monitor,
                DrawerContent::Help => help::Pane::Help,
                _ => help::Pane::Repl,
            },
        };
        let editing = self.focus == PaneFocus::Preview && self.edit_state.is_some();
        let prompt = self.focus == PaneFocus::NavTree && self.nav_prompt.is_some();
        let picker = self.focus == PaneFocus::NavTree && self.workspace_picker.is_some();
        let file = if pane == help::Pane::Preview { self.previewed_files_path() }
            else if pane == help::Pane::Nav { self.cursored_files_path() } else { None };
        help::Context {
            pane, mode: self.mode,
            file, image: self.preview_png.is_some(),
            pages: self.preview_page.is_some_and(|(_, count)| count > 1),
            editable: self.preview_png.is_none() && self.previewed_files_path().is_some()
                || self.concept.as_ref().is_some_and(|c| c.exists && self.concept_target_fired.as_ref() == Some(&c.target)),
            confirmation: if matches!(self.nav_prompt, Some(NavPrompt::ConfirmDelete { .. })) && prompt {
                help::Confirmation::Delete
            } else if editing && self.edit_state.as_ref().is_some_and(|e| e.confirm_discard) {
                help::Confirmation::Discard
            } else if editing && self.edit_state.as_ref().is_some_and(|e| e.stale_banner) {
                help::Confirmation::Stale
            } else { help::Confirmation::None },
            workspace_locked: self.edit_state.is_some(),
            picker, prompt, editing, modal: editing || prompt || picker,
            restore: self.maximized || self.wide_preview && self.edit_state.is_none()
                && self.nav_prompt.is_none() && self.workspace_picker.is_none()
                && matches!(self.focus, PaneFocus::NavTree | PaneFocus::Preview),
            session: self.tree.rows.get(self.tree.selected).is_some_and(|r| r.node.kind == "session"),
            alternate_screen: match pane {
                help::Pane::Terminal => {
                    #[cfg(windows)]
                    let screen = self.local_term.as_ref().map(|t| t.screen())
                        .or_else(|| self.attach_term.as_ref().map(|t| t.screen()));
                    #[cfg(not(windows))]
                    let screen = self.local_term.as_ref().map(|t| t.screen());
                    screen.is_some_and(|s| s.alternate_screen())
                }
                help::Pane::Agent => {
                    // A capsule row in alternate-screen mode receives the
                    // original key instead of having it consumed as a
                    // local-ring scrollback page.
                    self.pane_feed == PaneFeed::Capsule && self.pane_attach_term.as_ref()
                        .is_some_and(|t| t.screen().alternate_screen())
                },
                _ => false,
            },
        }
    }

    pub(in crate::ui) fn open_help_drawer(&mut self, context: help::Context) {
        if self.help_origin.is_none() {
            self.help_origin = Some((self.focus, self.drawer, self.maximized));
        }
        self.help.open(context);
        self.drawer = DrawerContent::Help;
        self.maximized = false;
        self.set_focus(PaneFocus::Repl);
        self.window.request_redraw();
    }

    pub(in crate::ui) fn close_help_drawer(&mut self) {
        if let Some((focus, drawer, maximized)) = self.help_origin.take() {
            self.set_focus(focus);
            self.drawer = drawer;
            self.maximized = maximized;
        } else {
            self.drawer = DrawerContent::Closed;
            self.set_focus(PaneFocus::NavTree);
        }
        self.help.peek = None;
        self.window.request_redraw();
    }
}
