//! Runtime layout and contextual Help. Operations own transitions; State applies owned effects.
use super::*;
pub(in crate::ui) use super::global_keys::{LayoutEffect, LayoutPort, HelpKeyEffect, HelpBrowser,
    HelpBrowserPort, AgentRevealPort, finish_help_key, finish_agent_reveal};
use super::keybindings::{Scope, ACTIONS};
use crate::ui::persist::settings::{LayoutPreset, Slot};
use std::time::{Duration, Instant};

pub const PEEK_HOLD: Duration = Duration::from_secs(5);
pub const PEEK_FADE: Duration = Duration::from_millis(240);

#[derive(Debug, Clone)]
pub struct Peek { context: help::Context, started: Instant }
impl Peek {
    pub fn context(&self) -> &help::Context { &self.context }
    pub fn started(&self) -> Instant { self.started }
    pub fn opacity(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed <= PEEK_HOLD { 1.0 } else {
            (1.0 - (elapsed - PEEK_HOLD).as_secs_f32() / PEEK_FADE.as_secs_f32()).clamp(0.0, 1.0)
        }
    }
}

#[derive(Default)]
pub struct Help {
    context: help::Context,
    query: String,
    selected: usize,
    all_panes: bool,
    peek: Option<Peek>,
}
impl Help {
    pub fn context(&self) -> &help::Context { &self.context }
    pub fn query(&self) -> &str { &self.query }
    pub fn selected(&self) -> usize { self.selected }
    pub fn all_panes(&self) -> bool { self.all_panes }
    pub fn actions(&self, bindings: &KeyBindings) -> Vec<Action> {
        let query = self.query.to_lowercase();
        let mut actions: Vec<_> = ACTIONS.iter()
            .filter(|s| self.all_panes || self.context.allows(s.action))
            .filter(|s| query.split_whitespace().all(|word| {
                format!("{} {} {} {} {}", s.label, s.detail, s.name, s.group, bindings.labels(s.action))
                    .to_lowercase().contains(word)
            }))
            .map(|s| s.action).collect();
        actions.sort_by_key(|a| matches!(a.spec().scope, Scope::Global | Scope::Workspace | Scope::Restore));
        actions
    }
    pub fn selected_action(&self, bindings: &KeyBindings) -> Option<Action> {
        self.actions(bindings).get(self.selected).copied()
    }
    fn open(&mut self, context: help::Context) {
        self.context = context;
        self.query.clear();
        self.selected = 0;
        self.all_panes = false;
        self.peek = None;
    }
    fn move_selection(&mut self, delta: isize, bindings: &KeyBindings) {
        let count = self.actions(bindings).len();
        self.selected = self.selected.saturating_add_signed(delta).min(count.saturating_sub(1));
    }
}

#[derive(Clone, Copy)]
struct HelpOrigin { focus: PaneFocus, drawer: DrawerContent, maximized: bool }

pub(in crate::ui) struct LayoutHelp {
    drawer: DrawerContent,
    maximized: bool,
    wide_preview: bool,
    help: Help,
    help_origin: Option<HelpOrigin>,
    help_start_pending: bool,
    help_peek_start_pending: bool,
}

#[derive(Default, Clone, Copy)]
pub(in crate::ui) struct LayoutGates {
    pub editing: bool, pub prompt: bool, pub picker: bool,
}
impl LayoutGates {
    fn reading(self, focus: PaneFocus) -> bool {
        !self.editing && !self.prompt && !self.picker
            && matches!(focus, PaneFocus::NavTree | PaneFocus::Preview)
    }
}

pub(in crate::ui) struct HelpKey<'a> {
    pub logical_key: &'a Key, pub action: Option<Action>, pub repeat: bool,
    pub modifiers: Modifiers,
}
pub(in crate::ui) struct StartupRequests { peek: bool, drawer: bool }
pub(in crate::ui) trait StartupHelpPort { fn start_help_peek(&mut self); fn start_help_drawer(&mut self); }
pub(in crate::ui) fn start_pending(requests: StartupRequests, port: &mut impl StartupHelpPort) {
    if requests.peek { port.start_help_peek(); }
    if requests.drawer { port.start_help_drawer(); }
}

pub(in crate::ui) struct PeekInputs {
    pub context: Option<help::Context>, pub focus: PaneFocus, pub width: u16, pub height: u16, pub now: Instant,
}
#[derive(Default)]
pub(in crate::ui) struct PeekPreparation {
    pub clear_spill: bool, pub layout: LayoutEffect, pub overlay: Option<PeekOverlay>,
}
pub(in crate::ui) struct PeekOverlay { pub lines: Vec<String>, pub opacity: f32 }

impl LayoutHelp {
    pub(in crate::ui) fn new(drawer: DrawerContent, maximized: bool, start_help: bool, start_peek: bool) -> Self {
        Self { drawer, maximized, wide_preview: false, help: Help::default(), help_origin: None,
            help_start_pending: start_help, help_peek_start_pending: start_peek }
    }
    pub(in crate::ui) fn drawer(&self) -> DrawerContent { self.drawer }
    pub(in crate::ui) fn maximized(&self) -> bool { self.maximized }
    pub(in crate::ui) fn wide_preview(&self) -> bool { self.wide_preview }
    pub(in crate::ui) fn help(&self) -> &Help { &self.help }
    pub(in crate::ui) fn peek(&self) -> Option<&Peek> { self.help.peek.as_ref() }

    pub(in crate::ui) fn toggle_drawer(&mut self, tenant: DrawerContent, focus: PaneFocus) -> LayoutEffect {
        assert!(matches!(tenant, DrawerContent::Repl | DrawerContent::Terminal | DrawerContent::Monitor));
        self.help_origin = None;
        self.drawer = self.drawer.toggle(tenant);
        LayoutEffect { focus: if self.drawer.is_open() { Some(PaneFocus::Repl) }
            else if focus == PaneFocus::Repl { Some(PaneFocus::NavTree) } else { None },
            monitor_sync: true, redraw: true }
    }
    pub(in crate::ui) fn show_repl_automatically(&mut self, enabled: bool, base_has_drawer: bool) -> LayoutEffect {
        if enabled && base_has_drawer && self.drawer != DrawerContent::Repl { self.drawer = DrawerContent::Repl; }
        LayoutEffect::default()
    }
    pub(in crate::ui) fn show_demo_repl(&mut self) -> LayoutEffect {
        self.drawer = DrawerContent::Repl;
        LayoutEffect::default()
    }
    pub(in crate::ui) fn terminal_spawn_failed(&mut self) -> LayoutEffect {
        self.drawer = DrawerContent::Closed;
        LayoutEffect::default()
    }
    pub(in crate::ui) fn restore_available(&self, focus: PaneFocus, gates: LayoutGates) -> bool {
        self.maximized() || self.wide_preview() && gates.reading(focus)
    }
    pub(in crate::ui) fn layout_action(&mut self, action: Option<Action>, context: &help::Context,
        focus: PaneFocus, gates: LayoutGates) -> Option<LayoutEffect> {
        let action = action.filter(|a| context.allows(*a))?;
        let mut effect = LayoutEffect { redraw: true, ..Default::default() };
        match action {
            Action::MaximizePane => self.maximized = true,
            Action::RestoreLayout if self.maximized => self.maximized = false,
            Action::RestoreLayout if self.wide_preview && gates.reading(focus) => self.wide_preview = false,
            Action::ToggleWidePreview => {
                self.wide_preview = !self.wide_preview;
                if self.wide_preview && focus == PaneFocus::Llm { effect.focus = Some(PaneFocus::Preview); }
            }
            _ => return None,
        }
        Some(effect)
    }
    pub(in crate::ui) fn reveal_agent(&mut self) -> LayoutEffect {
        self.wide_preview = false;
        LayoutEffect { focus: Some(PaneFocus::Llm), ..Default::default() }
    }
    pub(in crate::ui) fn maximize_slot(&self, focus: PaneFocus, leave_line: bool) -> Option<Slot> {
        maximize_slot(self.maximized, focus, leave_line)
    }
    pub(in crate::ui) fn effective_preset(&self, base: &LayoutPreset) -> LayoutPreset {
        let mut preset = if self.wide_preview() { base.wide_preview() } else { base.clone() };
        if self.drawer == DrawerContent::Help && preset.drawer.is_none() { preset.drawer = Some(Slot::Repl); }
        preset
    }
    pub(in crate::ui) fn spatial_slots(&self, base: &LayoutPreset) -> (Vec<Slot>, Option<Slot>) {
        let preset = self.effective_preset(base);
        (preset.columns.iter().map(|c| c.slot).collect(), if self.drawer.is_open() { preset.drawer } else { None })
    }
    pub(in crate::ui) fn open_help(&mut self, context: help::Context, focus: PaneFocus) -> LayoutEffect {
        if self.help_origin.is_none() {
            self.help_origin = Some(HelpOrigin { focus, drawer: self.drawer, maximized: self.maximized });
        }
        self.help.open(context);
        self.drawer = DrawerContent::Help;
        self.maximized = false;
        LayoutEffect { focus: Some(PaneFocus::Repl), redraw: true, ..Default::default() }
    }
    pub(in crate::ui) fn close_help(&mut self) -> LayoutEffect {
        let focus = if let Some(origin) = self.help_origin.take() {
            self.drawer = origin.drawer;
            self.maximized = origin.maximized;
            origin.focus
        } else { self.drawer = DrawerContent::Closed; PaneFocus::NavTree };
        self.help.peek = None;
        LayoutEffect { focus: Some(focus), redraw: true, ..Default::default() }
    }
    pub(in crate::ui) fn move_help_selection(&mut self, delta: isize, bindings: &KeyBindings) {
        self.help.move_selection(delta, bindings);
    }
    /// Input is already converted to whole wheel rows by the pointer dispatcher.
    pub(in crate::ui) fn help_wheel(&mut self, rows: i32, focus: PaneFocus, bindings: &KeyBindings) -> Option<LayoutEffect> {
        if self.drawer != DrawerContent::Help || focus != PaneFocus::Repl { return None; }
        self.move_help_selection(-(rows as isize), bindings);
        Some(LayoutEffect { redraw: true, ..Default::default() })
    }
    pub(in crate::ui) fn start_peek(&mut self, context: help::Context, now: Instant) {
        self.help.peek = Some(Peek { context, started: now });
    }
    pub(in crate::ui) fn take_startup_requests(&mut self) -> StartupRequests {
        StartupRequests { peek: std::mem::take(&mut self.help_peek_start_pending),
            drawer: std::mem::take(&mut self.help_start_pending) }
    }
    pub(in crate::ui) fn expire_peek(&mut self, now: impl FnOnce() -> Instant) -> bool {
        if self.peek().is_some_and(|p| p.opacity(now()) <= 0.0) {
            self.help.peek = None;
            true
        } else { false }
    }
    pub(in crate::ui) fn peek_fading(&self, now: impl FnOnce() -> Instant) -> bool {
        self.peek().is_some_and(|p| now().saturating_duration_since(p.started()) >= PEEK_HOLD)
    }
    pub(in crate::ui) fn peek_deadline(&self) -> Option<Instant> { self.peek().map(|p| p.started() + PEEK_HOLD) }
    /// Called after chrome draw. A promotion changes the next frame's chrome, never the current one.
    pub(in crate::ui) fn prepare_peek(&mut self, dialog: bool, inputs: Option<PeekInputs>, bindings: &KeyBindings) -> PeekPreparation {
        if dialog { self.help.peek = None; return PeekPreparation { clear_spill: true, ..Default::default() }; }
        let Some(peek) = self.peek().cloned() else { return PeekPreparation::default(); };
        let inputs = inputs.expect("a live peek has preparation inputs");
        let opacity = peek.opacity(inputs.now);
        if opacity <= 0.0 || Some(&peek.context) != inputs.context.as_ref() {
            self.help.peek = None;
            return PeekPreparation::default();
        }
        let lines = help::peek_lines(&peek.context, bindings);
        let longest = lines.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.as_str())).max().unwrap_or(0);
        if (inputs.width.saturating_sub(4) as usize) < longest || inputs.height < lines.len() as u16 + 2 {
            let layout = self.open_help(peek.context, inputs.focus);
            PeekPreparation { layout, ..Default::default() }
        } else { PeekPreparation { clear_spill: true, overlay: Some(PeekOverlay { lines, opacity }), ..Default::default() } }
    }
    pub(in crate::ui) fn help_key(&mut self, key: HelpKey<'_>, context: help::Context, focus: PaneFocus,
        bindings: &KeyBindings, now: impl FnOnce() -> Instant) -> HelpKeyEffect {
        let HelpKey { logical_key, action, repeat, modifiers } = key;
        let Modifiers { ctrl, super_, .. } = modifiers;
        let mut effect = HelpKeyEffect::default();
        if !repeat && action == Some(Action::ToggleHelpDrawer) {
            effect.layout = Some(if self.drawer == DrawerContent::Help { self.close_help() } else { self.open_help(context, focus) });
            effect.consumed = true;
            return effect;
        }
        if action == Some(Action::ToggleHelp) {
            tracing::debug!(target: "sot::ui::input::global_keys", repeat, peek = self.help.peek.is_some(), ?context, "context help requested");
            effect.consumed = true;
            if repeat { return effect; }
            if self.drawer == DrawerContent::Help && focus == PaneFocus::Repl { effect.layout = Some(self.close_help()); }
            else if let Some(peek) = self.help.peek.take() { effect.layout = Some(self.open_help(peek.context, focus)); }
            else {
                self.start_peek(context, now());
                effect.layout = Some(LayoutEffect { redraw: true, ..Default::default() });
            }
            return effect;
        }
        effect.dismissed_peek = self.help.peek.take().is_some();
        if effect.dismissed_peek && *logical_key == Key::Named(NamedKey::Escape) { effect.consumed = true; return effect; }
        if self.drawer != DrawerContent::Help || focus != PaneFocus::Repl
            || action.is_some_and(|a| matches!(a.spec().scope, Scope::Global | Scope::Workspace | Scope::Restore | Scope::Quit)) {
            return effect;
        }
        tracing::debug!(target: "sot::ui::input::global_keys", event.logical_key = ?logical_key, ?action, "help drawer key");
        match logical_key {
            _ if action == Some(Action::HelpClose) => effect.layout = Some(self.close_help()),
            _ if action == Some(Action::HelpUp) => self.move_help_selection(-1, bindings),
            _ if action == Some(Action::HelpDown) => self.move_help_selection(1, bindings),
            _ if action == Some(Action::HelpPageUp) => self.move_help_selection(-8, bindings),
            _ if action == Some(Action::HelpPageDown) => self.move_help_selection(8, bindings),
            _ if action == Some(Action::HelpScope) && !repeat => { self.help.all_panes = !self.help.all_panes; self.help.selected = 0; }
            Key::Named(NamedKey::Backspace) => { self.help.query.pop(); self.help.selected = 0; }
            _ if action == Some(Action::HelpManual) && !repeat => {
                effect.browser = self.help.selected_action(bindings).map(HelpBrowser::Manual);
            }
            _ if action == Some(Action::HelpCheatSheet) && !repeat => {
                effect.browser = Some(HelpBrowser::CheatSheet(help::cheat_sheet_html(bindings)));
            }
            Key::Character(c) if !ctrl && !super_ => { self.help.query.push_str(c); self.help.selected = 0; }
            Key::Named(NamedKey::Space) => { self.help.query.push(' '); self.help.selected = 0; }
            _ => {}
        }
        effect.consumed = true;
        effect.browsing_redraw = true;
        effect
    }
}
