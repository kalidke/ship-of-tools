//! The help drawer's state: the context it opens for, and opening and closing it.

use super::*;

impl State {
    pub(in crate::ui) fn help_context(&self) -> help::Context {
        let pane = match self.focus_prompts.focus() {
            PaneFocus::NavTree => help::Pane::Nav,
            PaneFocus::Preview => help::Pane::Preview,
            PaneFocus::Llm => help::Pane::Agent,
            PaneFocus::Repl => match self.layout_help.drawer() {
                DrawerContent::Terminal => help::Pane::Terminal,
                DrawerContent::Monitor => help::Pane::Monitor,
                DrawerContent::Help => help::Pane::Help,
                _ => help::Pane::Repl,
            },
        };
        let editing = self.focus_prompts.focus() == PaneFocus::Preview && self.editor.is_editing();
        let prompt = self.focus_prompts.focus() == PaneFocus::NavTree && self.focus_prompts.prompt().is_some();
        let picker = self.focus_prompts.focus() == PaneFocus::NavTree && self.workspace_picker.is_some();
        let file = if pane == help::Pane::Preview { self.previewed_files_path() }
            else if pane == help::Pane::Nav { self.cursored_files_path() } else { None };
        help::Context {
            pane, mode: self.nav.mode(),
            file, image: self.raster.is_shown(),
            pages: self.pages.page().is_some_and(|(_, count)| count > 1),
            editable: !self.raster.is_shown() && self.previewed_files_path().is_some()
                || self.concept.info().is_some_and(|c| c.exists() && self.preview_requests.concept_target() == Some(c.target())),
            confirmation: if matches!(self.focus_prompts.prompt(), Some(NavPrompt::ConfirmDelete { .. })) && prompt {
                help::Confirmation::Delete
            } else if editing && self.editor.confirming_discard() {
                help::Confirmation::Discard
            } else if editing && self.editor.stale() {
                help::Confirmation::Stale
            } else { help::Confirmation::None },
            workspace_locked: self.editor.is_editing(),
            picker, prompt, editing, modal: editing || prompt || picker,
            restore: self.layout_help.restore_available(self.focus_prompts.focus(), self.layout_gates()),
            session: self.nav.tree().rows().get(self.nav.tree().selected()).is_some_and(|r| r.node.kind == "session"),
            alternate_screen: match pane {
                help::Pane::Terminal => {
                    let screen = self.term_drawer.screen();
                    screen.is_some_and(|s| s.alternate_screen())
                }
                help::Pane::Agent => {
                    // A capsule row in alternate-screen mode receives the
                    // original key instead of having it consumed as a
                    // local-ring scrollback page.
                    self.agent_pane.alternate_screen()
                },
                _ => false,
            },
        }
    }

    pub(in crate::ui) fn layout_gates(&self) -> layout_help::LayoutGates {
        layout_help::LayoutGates { editing: self.editor.is_editing(),
            prompt: self.focus_prompts.prompt().is_some(), picker: self.workspace_picker.is_some() }
    }

    pub(in crate::ui) fn open_help_drawer(&mut self, context: help::Context) {
        let effect = self.layout_help.open_help(context, self.focus_prompts.focus());
        effect.apply(self, None);
    }
}

impl layout_help::LayoutPort for State {
    fn layout_focus(&mut self, focus: PaneFocus) { self.set_focus(focus); }
    fn layout_monitor_sync(&mut self) { self.sync_monitor_subscription(); }
    fn layout_label(&mut self, label: String) { self.last_key = Some(label); }
    fn layout_redraw(&mut self) { self.window.request_redraw(); }
}
impl layout_help::HelpBrowserPort for State {
    fn help_manual(&mut self, url: &str) -> std::result::Result<(), String> {
        crate::browser_open::spawn_opener(url).map(|_| ()).map_err(|e| e.to_string())
    }
    fn help_html(&mut self, html: &[u8]) -> std::result::Result<(), String> {
        open_html_in_browser(html).map(|_| ()).map_err(|e| e.to_string())
    }
    fn help_status(&mut self, status: String) { self.status = status; }
}
impl layout_help::AgentRevealPort for State {
    fn layout_paste(&mut self, bytes: &[u8]) { self.send_pane_input(bytes); }
}
impl layout_help::StartupHelpPort for State {
    fn start_help_peek(&mut self) {
        let context = self.help_context();
        self.layout_help.start_peek(context, std::time::Instant::now());
    }
    fn start_help_drawer(&mut self) {
        let context = self.help_context();
        self.open_help_drawer(context);
    }
}
