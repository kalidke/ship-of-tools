//! Characterizations through public owner operations and the production effect continuations.
use super::*;
use super::layout_help::*;
use super::keybindings::ACTIONS;
use crate::ui::persist::settings::{Column, LayoutPreset, Slot};
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

const PLAIN: Modifiers = Modifiers { ctrl: false, super_: false, alt: false, shift: false };
const FOCI: [PaneFocus; 4] = [PaneFocus::NavTree, PaneFocus::Preview, PaneFocus::Llm, PaneFocus::Repl];
const DRAWERS: [DrawerContent; 6] = [DrawerContent::Closed, DrawerContent::Repl,
    DrawerContent::Terminal, DrawerContent::Monitor, DrawerContent::Help, DrawerContent::Chat];
const TENANTS: [DrawerContent; 4] = [DrawerContent::Repl, DrawerContent::Terminal, DrawerContent::Monitor, DrawerContent::Chat];

/// A row's drawer at its own height.
fn own_height(content: DrawerContent) -> RowDrawer { RowDrawer { content, full: false } }

struct Harness {
    layout: LayoutHelp, focus: FocusPrompts, bindings: KeyBindings, context: help::Context,
    now: Instant, events: Vec<String>, fail: bool, manual: Vec<String>, html: String,
    status: String, pastes: Vec<(PaneFocus, Vec<u8>)>,
}
impl Harness {
    fn new(drawer: DrawerContent, focus: PaneFocus, maximized: bool) -> Self {
        Self { layout: LayoutHelp::new(drawer, maximized, false, false), focus: FocusPrompts::new(focus),
            bindings: KeyBindings::defaults(), context: help::Context::default(), now: Instant::now(),
            events: Vec::new(), fail: false, manual: Vec::new(), html: String::new(),
            status: "unchanged".into(), pastes: Vec::new() }
    }
    /// A switch a person made.
    fn enter(&mut self, row: RowDrawer, focus: PaneFocus) { self.enter_on(row, focus, &LayoutPreset::default_laptop()); }
    fn enter_on(&mut self, row: RowDrawer, focus: PaneFocus, base: &LayoutPreset) { self.enter_as(row, focus, base, true); }
    fn enter_as(&mut self, row: RowDrawer, focus: PaneFocus, base: &LayoutPreset, person_driven: bool) {
        let e = self.layout.enter_row_drawer(row, focus, base, person_driven); e.apply(self, None);
    }
    fn open(&mut self) {
        let e = self.layout.open_help(self.context.clone(), self.focus.focus()); e.apply(self, None);
    }
    fn close(&mut self) { let e = self.layout.close_help(); e.apply(self, None); }
    fn toggle(&mut self, tenant: DrawerContent) {
        let e = self.layout.toggle_drawer(tenant, self.focus.focus()); e.apply(self, Some("bound".into()));
    }
    fn cycle(&mut self, tenant: DrawerContent) {
        self.cycle_on(tenant, &LayoutPreset::default_laptop());
    }
    fn cycle_on(&mut self, tenant: DrawerContent, base: &LayoutPreset) {
        let e = self.layout.cycle_drawer(tenant, self.focus.focus(), base); e.apply(self, Some("bound".into()));
    }
    fn action(&mut self, action: Action) -> bool { self.gated(action, LayoutGates::default()) }
    fn gated(&mut self, action: Action, gates: LayoutGates) -> bool {
        let mut context = self.context.clone();
        context.restore = self.layout.restore_available(self.focus.focus(), gates);
        context.modal = gates.editing || gates.prompt || gates.picker;
        let e = self.layout.layout_action(Some(action), &context, self.focus.focus(), gates);
        if let Some(e) = e { e.apply(self, Some("layout".into())); true } else { false }
    }
    fn key(&mut self, logical_key: Key, action: Option<Action>, repeat: bool, ctrl: bool, super_: bool) -> ControlFlow<()> {
        let now = self.now;
        let e = self.layout.help_key(HelpKey { logical_key: &logical_key, action, repeat, modifiers: Modifiers { ctrl, super_, alt: true, ..PLAIN } },
            self.context.clone(), self.focus.focus(), &self.bindings, || now);
        finish_help_key(e, self)
    }
    fn named(&mut self, action: Action) -> ControlFlow<()> {
        self.key(Key::Named(NamedKey::F1), Some(action), false, false, false)
    }
    fn browse(&mut self, action: Action) { assert!(self.named(action).is_break()); }
    fn search(&mut self, text: &str) {
        assert!(self.key(Key::Character(text.into()), None, false, false, false).is_break());
    }
    fn peek(&mut self) { self.layout.start_peek(self.context.clone(), self.now); }
}
impl LayoutPort for Harness {
    fn layout_focus(&mut self, focus: PaneFocus) {
        self.events.push(format!("focus:{focus:?}"));
        self.focus.set_focus(focus);
    }
    fn layout_monitor_sync(&mut self) { self.events.push("monitor".into()); }
    fn layout_label(&mut self, label: String) { self.events.push(format!("label:{label}")); }
    fn layout_redraw(&mut self) { tracing::debug!("recorder redraw"); self.events.push("redraw".into()); }
}
impl HelpBrowserPort for Harness {
    fn help_manual(&mut self, url: &str) -> std::result::Result<(), String> {
        self.events.push("manual".into()); self.manual.push(url.into());
        if self.fail { Err("manual refused".into()) } else { Ok(()) }
    }
    fn help_html(&mut self, html: &[u8]) -> std::result::Result<(), String> {
        tracing::debug!("recorder html"); self.events.push("html".into());
        self.html = String::from_utf8(html.to_vec()).unwrap();
        if self.fail { Err("sheet refused".into()) } else { Ok(()) }
    }
    fn help_status(&mut self, status: String) {
        tracing::debug!("recorder status"); self.events.push("status".into()); self.status = status;
    }
}
impl AgentRevealPort for Harness {
    fn layout_paste(&mut self, bytes: &[u8]) {
        self.events.push("paste".into()); self.pastes.push((self.focus.focus(), bytes.to_vec()));
    }
}
impl StartupHelpPort for Harness {
    fn start_help_peek(&mut self) {
        self.events.push("peek context".into()); let context = self.context.clone();
        self.events.push("peek clock".into()); self.layout.start_peek(context, self.now);
    }
    fn start_help_drawer(&mut self) { self.events.push("drawer context".into()); self.open(); }
}
fn retained(h: &Harness) -> (bool, bool, help::Context, String, usize, bool, Option<(help::Context, Instant)>) {
    (h.layout.maximized(), h.layout.wide_preview(), h.layout.help().context().clone(),
        h.layout.help().query().into(), h.layout.help().selected(), h.layout.help().all_panes(),
        h.layout.peek().map(|p| (p.context().clone(), p.started())))
}
fn browsing() -> Harness {
    let mut h = Harness::new(DrawerContent::Monitor, PaneFocus::Preview, true);
    h.context = help::Context { pane: help::Pane::Preview, image: true, ..Default::default() };
    h.action(Action::ToggleWidePreview);
    h.open(); h.search("zoom"); h.layout.move_help_selection(1, &h.bindings);
    h.action(Action::MaximizePane); h.peek(); h.events.clear(); h
}

#[test]
fn drawer_toggles_select_one_tenant_and_bounce_only_drawer_focus() {
    for drawer in DRAWERS { for focus in FOCI { for tenant in TENANTS {
        let mut h = Harness::new(drawer, focus, false); h.toggle(tenant);
        let closed = drawer == tenant;
        assert_eq!(h.layout.drawer(), if closed { DrawerContent::Closed } else { tenant });
        let expected = if !closed { PaneFocus::Repl } else if focus == PaneFocus::Repl { PaneFocus::NavTree } else { focus };
        assert_eq!(h.focus.focus(), expected);
        let tail = &h.events[h.events.len()-3..];
        assert_eq!(tail, ["monitor", "label:bound", "redraw"]);
    } } }
}
#[test]
fn drawer_toggles_leave_other_layout_and_help_state_intact() {
    for tenant in TENANTS {
        let mut h = browsing(); let before = retained(&h); h.toggle(tenant);
        assert_eq!(retained(&h), before);
    }
}
#[test]
fn only_an_ordinary_drawer_toggle_requests_monitor_sync() {
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
    h.toggle(DrawerContent::Monitor); assert!(h.events.iter().any(|e| e == "monitor")); h.events.clear();
    h.open(); h.close();
    h.layout.show_repl_automatically(true, true);
    h.layout.show_demo_repl();
    h.layout.terminal_spawn_failed();
    assert!(!h.events.iter().any(|e| e == "monitor"));
}
#[test]
fn automatic_and_demo_repl_opening_keep_their_distinct_gates() {
    for enabled in [false, true] { for has_drawer in [false, true] { for drawer in DRAWERS {
        let mut h = Harness::new(drawer, PaneFocus::Llm, true);
        let before = retained(&h);
        h.layout.show_repl_automatically(enabled, has_drawer);
        assert_eq!(h.layout.drawer(), if enabled && has_drawer { DrawerContent::Repl } else { drawer });
        h.layout.show_demo_repl();
        assert_eq!(h.layout.drawer(), DrawerContent::Repl); assert_eq!(retained(&h), before);
        assert_eq!(h.focus.focus(), PaneFocus::Llm); assert!(h.events.is_empty());
    } } }
}
#[test]
fn terminal_spawn_failure_closes_only_the_drawer() {
    let mut h = browsing();
    h.toggle(DrawerContent::Terminal); let before = retained(&h); let focus = h.focus.focus(); h.events.clear();
    h.layout.terminal_spawn_failed();
    assert_eq!(h.layout.drawer(), DrawerContent::Closed); assert_eq!(retained(&h), before);
    assert_eq!(h.focus.focus(), focus); assert!(h.events.is_empty());
}
#[test]
fn maximization_tracks_focus_and_yields_to_a_leave_line() {
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
    h.action(Action::MaximizePane);
    for (focus, slot) in FOCI.into_iter().zip([Slot::Nav, Slot::Preview, Slot::Llm, Slot::Repl]) {
        h.focus.set_focus(focus);
        assert_eq!(h.layout.maximize_slot(h.focus.focus(), false), Some(slot));
        assert_eq!(h.layout.maximize_slot(h.focus.focus(), true), None);
        assert!(h.layout.maximized()); assert_eq!(h.layout.maximize_slot(focus, false), Some(slot));
    }
}
#[test]
fn restore_peels_maximize_before_wide_preview() {
    for focus in FOCI { for gate in [LayoutGates::default(), LayoutGates { editing: true, ..Default::default() },
        LayoutGates { prompt: true, ..Default::default() }, LayoutGates { picker: true, ..Default::default() }] {
        let mut h = Harness::new(DrawerContent::Terminal, PaneFocus::Preview, false);
        h.action(Action::ToggleWidePreview); h.action(Action::MaximizePane);
        h.focus.set_focus(focus);
        let modal = gate.editing || gate.prompt || gate.picker;
        assert_eq!(h.gated(Action::RestoreLayout, gate), !modal);
        assert_eq!(h.layout.maximized(), modal); assert!(h.layout.wide_preview());
        let reading = matches!(focus, PaneFocus::NavTree | PaneFocus::Preview);
        assert_eq!(h.gated(Action::RestoreLayout, gate), !modal && reading);
        assert_eq!(h.layout.wide_preview(), modal || !reading);
    } }
}
#[test]
fn wide_preview_moves_only_the_focus_it_hides() {
    for focus in FOCI {
        let mut h = Harness::new(DrawerContent::Monitor, focus, true);
        let before = retained(&h); h.action(Action::ToggleWidePreview);
        let expected = if focus == PaneFocus::Llm { PaneFocus::Preview } else { focus };
        assert_eq!(h.focus.focus(), expected); assert_eq!(h.layout.drawer(), DrawerContent::Monitor);
        assert_eq!(h.layout.maximized(), before.0); assert_eq!(h.layout.help().context(), &before.2);
        h.action(Action::ToggleWidePreview);
        assert_eq!(h.focus.focus(), expected); assert!(!h.layout.wide_preview());
    }
}
#[test]
fn layout_and_focus_projections_use_the_same_effective_preset() {
    let mut omitted = LayoutPreset::default_laptop();
    omitted.columns = vec![Column { slot: Slot::Nav, width: 0.4 }, Column { slot: Slot::Llm, width: 0.6 }];
    let mut custom = omitted.clone(); custom.drawer = Some(Slot::Preview);
    let mut drawerless = LayoutPreset::default_laptop(); drawerless.drawer = None;
    for (base, wide_widths) in [
        (LayoutPreset::default_laptop(), vec![0.18, 0.82]),
        (LayoutPreset::default_portrait(), vec![0.30, 0.70]),
        (omitted, vec![1.0]), (custom, vec![1.0]), (drawerless, vec![0.18, 0.82]),
    ] {
        for wide in [false, true] { for drawer in DRAWERS {
            let mut h = Harness::new(drawer, PaneFocus::NavTree, true);
            if wide { h.action(Action::ToggleWidePreview); }
            let p = h.layout.effective_preset(&base); let (columns, slot) = h.layout.spatial_slots(&base, h.focus.focus());
            assert_eq!(columns, p.columns.iter().map(|c| c.slot).collect::<Vec<_>>());
            assert!((p.columns.iter().map(|c| c.width).sum::<f32>() - 1.0).abs() < 0.001);
            let expected_widths: Vec<f32> = if wide { wide_widths.clone() }
                else { base.columns.iter().map(|c| c.width).collect() };
            assert_eq!(p.columns.len(), expected_widths.len());
            for (column, expected_width) in p.columns.iter().zip(expected_widths) {
                assert!((column.width - expected_width).abs() < 0.000_001,
                    "{:?}: expected {expected_width}, got {}", column.slot, column.width);
            }
            assert_eq!(columns, base.columns.iter().filter(|c| !wide || c.slot != Slot::Llm).map(|c| c.slot).collect::<Vec<_>>());
            assert_eq!(p.drawer_height, base.drawer_height);
            let expected = if drawer == DrawerContent::Help { base.drawer.or(Some(Slot::Repl)) } else { base.drawer };
            assert_eq!(p.drawer, expected); assert_eq!(slot, if drawer.is_open() { expected } else { None });
            h.focus.move_focus(SpatialDir::Down, &columns, slot);
            assert_eq!(h.focus.focus(), match slot { Some(Slot::Preview) => PaneFocus::Preview,
                Some(Slot::Repl) => PaneFocus::Repl, _ => PaneFocus::NavTree });
        } }
    }
}
#[test]
fn help_close_restores_the_first_origin_once() {
    for drawer in DRAWERS { for focus in FOCI { for maximized in [false, true] {
        let mut h = Harness::new(drawer, focus, maximized); h.open();
        h.focus.set_focus(PaneFocus::Preview); h.open();
        h.action(Action::ToggleWidePreview); h.close();
        assert_eq!((h.layout.drawer(), h.focus.focus(), h.layout.maximized()), (drawer, focus, maximized));
        assert!(h.layout.wide_preview()); h.close();
        assert_eq!((h.layout.drawer(), h.focus.focus(), h.layout.maximized()), (DrawerContent::Closed, PaneFocus::NavTree, maximized));
    } } }
    let mut h = Harness::new(DrawerContent::Help, PaneFocus::Repl, true); h.close();
    assert!(h.layout.maximized()); assert_eq!(h.focus.focus(), PaneFocus::NavTree);
}
#[test]
fn ordinary_drawer_toggle_abandons_help_return_but_automatic_opening_keeps_it() {
    for path in 0..4 {
        let mut h = Harness::new(DrawerContent::Monitor, PaneFocus::Llm, true); h.open();
        match path {
            0 => { let e = h.layout.toggle_drawer(DrawerContent::Repl, h.focus.focus()); e.apply(&mut h, None); }
            1 => h.layout.show_repl_automatically(true, true),
            2 => h.layout.show_demo_repl(),
            _ => h.layout.terminal_spawn_failed(),
        }
        h.close();
        assert_eq!(h.layout.drawer(), if path == 0 { DrawerContent::Closed } else { DrawerContent::Monitor });
        assert_eq!(h.focus.focus(), if path == 0 { PaneFocus::NavTree } else { PaneFocus::Llm });
        assert_eq!(h.layout.maximized(), path != 0);
    }
}
#[test]
fn help_open_resets_browsing_without_resetting_wide_preview() {
    let mut h = browsing(); h.browse(Action::HelpScope);
    h.layout.move_help_selection(1, &h.bindings);
    assert_eq!(h.layout.help().selected(), 1);
    h.peek();
    h.context = help::Context { pane: help::Pane::Terminal, ..Default::default() }; h.open();
    assert_eq!(h.layout.help().context(), &h.context); assert_eq!(h.layout.help().query(), "");
    assert_eq!((h.layout.help().selected(), h.layout.help().all_panes(), h.layout.peek().is_none(),
        h.layout.maximized(), h.layout.wide_preview()), (0, false, true, false, true));
    assert_eq!(h.layout.drawer(), DrawerContent::Help); assert_eq!(h.focus.focus(), PaneFocus::Repl);
}
#[test]
fn pending_help_starts_fire_once_in_peek_then_drawer_order() {
    for peek in [false, true] { for drawer in [false, true] {
        let mut h = Harness::new(DrawerContent::Terminal, PaneFocus::Llm, true);
        h.layout = LayoutHelp::new(DrawerContent::Terminal, true, drawer, peek);
        let requests = h.layout.take_startup_requests(); start_pending(requests, &mut h);
        assert_eq!(h.layout.drawer(), if drawer { DrawerContent::Help } else { DrawerContent::Terminal });
        assert_eq!(h.layout.peek().is_some(), peek && !drawer);
        if peek { assert_eq!(&h.events[..2], ["peek context", "peek clock"]); }
        if drawer && peek { assert!(h.events.iter().position(|e| e == "drawer context").unwrap() > 1); }
        h.layout.show_demo_repl(); h.layout.expire_peek(|| h.now + PEEK_HOLD + PEEK_FADE);
        h.events.clear(); let requests = h.layout.take_startup_requests(); start_pending(requests, &mut h);
        assert!(h.events.is_empty()); assert_eq!(h.layout.drawer(), DrawerContent::Repl); assert!(h.layout.peek().is_none());
    } }
}
#[test]
fn peek_keys_preserve_context_repeat_and_escape_rules() {
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::Preview, true);
    h.context.pane = help::Pane::Preview; let first = h.context.clone();
    assert!(h.named(Action::ToggleHelp).is_break()); let started = h.layout.peek().unwrap().started();
    h.context.pane = help::Pane::Agent; h.now += Duration::from_secs(1); h.events.clear();
    assert!(h.key(Key::Named(NamedKey::F1), Some(Action::ToggleHelp), true, false, false).is_break());
    assert_eq!(h.layout.peek().unwrap().started(), started); assert_eq!(h.layout.peek().unwrap().context(), &first);
    assert!(h.events.is_empty()); h.browse(Action::ToggleHelp); assert_eq!(h.layout.help().context(), &first);
    h.close(); h.browse(Action::ToggleHelp);
    assert!(h.key(Key::Named(NamedKey::Escape), None, false, false, false).is_break()); assert!(h.layout.peek().is_none());
    h.browse(Action::ToggleHelp);
    assert!(h.key(Key::Character("x".into()), None, false, false, false).is_continue()); assert!(h.layout.peek().is_none());
}
#[test]
fn help_browsing_consumes_search_and_passes_window_actions() {
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false); h.open();
    for a in [Action::Reconnect, Action::WorkspaceCycleNext, Action::RestoreLayout, Action::Quit] {
        assert!(h.named(a).is_continue());
    }
    for (ctrl, cmd) in [(true, false), (false, true)] {
        assert!(h.key(Key::Character("x".into()), None, false, ctrl, cmd).is_break());
    }
    assert_eq!(h.layout.help().query(), "");
    h.search("ab"); // The helper supplies Alt; Alt characters still search.
    assert!(h.key(Key::Named(NamedKey::Space), None, true, true, true).is_break());
    assert_eq!(h.layout.help().query(), "ab ");
    assert!(h.key(Key::Named(NamedKey::Backspace), None, true, false, false).is_break());
    assert_eq!(h.layout.help().query(), "ab");
    for a in [Action::HelpScope, Action::HelpManual, Action::HelpCheatSheet] {
        assert!(h.key(Key::Named(NamedKey::F1), Some(a), true, false, false).is_break());
    }
    assert!(!h.layout.help().all_panes() && h.manual.is_empty() && h.html.is_empty());
    h.focus.set_focus(PaneFocus::Preview); assert!(h.named(Action::HelpDown).is_continue());
    h.focus.set_focus(PaneFocus::Repl); h.layout.show_demo_repl(); assert!(h.named(Action::HelpDown).is_continue());
}
#[test]
fn help_search_scope_and_selection_follow_loaded_bindings() {
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::Preview, false);
    h.bindings.merge_text("preview.png.zoom_in = \"Cmd+z\""); h.context.pane = help::Pane::Preview; h.context.image = true;
    h.open(); h.search(&h.bindings.first_label(Action::PreviewPngZoomIn));
    assert!(h.layout.help().actions(&h.bindings).contains(&Action::PreviewPngZoomIn));
    assert_eq!(h.layout.help().context(), &h.context);
    h.layout.move_help_selection(isize::MAX, &h.bindings);
    assert_eq!(h.layout.help().selected(), h.layout.help().actions(&h.bindings).len()-1);
    h.open();
    for (a, selected) in [(Action::HelpPageDown, 8), (Action::HelpUp, 7), (Action::HelpPageUp, 0), (Action::HelpDown, 1)] {
        h.browse(a); assert_eq!(h.layout.help().selected(), selected);
    }
    h.browse(Action::HelpScope); assert!(h.layout.help().all_panes()); assert_eq!(h.layout.help().selected(), 0);
    assert_eq!(h.layout.help().actions(&h.bindings).len(), ACTIONS.len());
    h.search("zzzzzzzz"); h.layout.move_help_selection(isize::MAX, &h.bindings);
    assert_eq!(h.layout.help().selected(), 0); assert!(h.layout.help().selected_action(&h.bindings).is_none());
}
#[test]
fn help_wheel_moves_selection_only_when_it_owns_focus() {
    for drawer in DRAWERS { for focus in FOCI {
        let mut h = Harness::new(drawer, focus, false); h.layout.move_help_selection(4, &h.bindings);
        let before = h.layout.help().selected();
        let effect = h.layout.help_wheel(2, focus, &h.bindings);
        let owns = drawer == DrawerContent::Help && focus == PaneFocus::Repl;
        assert_eq!(effect.is_some(), owns);
        if let Some(e) = effect { e.apply(&mut h, None); } else { h.events.push("pane wheel".into()); }
        assert_eq!(h.layout.help().selected(), if owns { before - 2 } else { before });
        assert_eq!(h.events, if owns { vec!["redraw"] } else { vec!["pane wheel"] });
    } }
}
#[test]
fn help_browser_intents_keep_selection_status_and_failure_behavior() {
    let mut h = browsing(); h.open();
    let actions = h.layout.help().actions(&h.bindings);
    let selected = actions.iter().position(|a| help::manual_url(*a) != help::manual_url(actions[0])).unwrap();
    h.layout.move_help_selection(selected as isize, &h.bindings);
    let url = help::manual_url(actions[selected]);
    for failed in [false, true] {
        h.fail = failed; h.events.clear(); h.browse(Action::HelpManual);
        assert_eq!(h.manual.last().unwrap(), url);
        assert_eq!(h.status, if failed { "Open help manual failed: manual refused" } else { "unchanged" });
        assert_eq!(h.events, if failed { vec!["manual", "status", "redraw"] } else { vec!["manual", "redraw"] });
    }
    h.bindings.merge_text("preview.png.zoom_in = \"Cmd+z\"");
    for failed in [true, false] {
        h.fail = failed; h.events.clear(); h.browse(Action::HelpCheatSheet);
        assert_eq!(h.html, help::cheat_sheet_html(&h.bindings));
        assert_eq!(h.status, if failed { "Print cheat sheet failed: sheet refused" } else { "cheat sheet · opened in browser" });
        assert_eq!(h.events, ["html", "status", "redraw"]);
    }
    h.search("zzzzzzzz"); h.events.clear(); h.browse(Action::HelpManual); assert_eq!(h.events, ["redraw"]);
}
#[test]
fn peek_preparation_respects_dialog_context_expiry_and_fit() {
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::Preview, true); h.context.pane = help::Pane::Preview;
    let lines = help::peek_lines(&h.context, &h.bindings);
    let width = lines.iter().map(|s| unicode_width::UnicodeWidthStr::width(s.as_str())).max().unwrap() as u16 + 4;
    let height = lines.len() as u16 + 2;
    for (w, ht, delta, changed, dialog) in [(width, height, Duration::ZERO, false, false),
        (width, height, PEEK_HOLD, false, false), (width, height, PEEK_HOLD + PEEK_FADE - Duration::from_millis(1), false, false),
        (width, height, PEEK_HOLD + PEEK_FADE, false, false), (width, height, PEEK_HOLD + PEEK_FADE + Duration::from_nanos(1), false, false),
        (width, height, Duration::ZERO, true, false), (width, height, Duration::ZERO, false, true),
        (width-1, height, Duration::ZERO, false, false), (width, height-1, Duration::ZERO, false, false)] {
        h.layout = LayoutHelp::new(DrawerContent::Closed, true, false, false); h.focus.set_focus(PaneFocus::Preview); h.peek();
        let mut context = h.context.clone(); if changed { context.pane = help::Pane::Agent; }
        assert_eq!(h.layout.drawer(), DrawerContent::Closed);
        let prepared = h.layout.prepare_peek(dialog, Some(PeekInputs { context: Some(context), focus: h.focus.focus(), width: w, height: ht, now: h.now + delta }), &h.bindings);
        let valid = !dialog && !changed && delta < PEEK_HOLD + PEEK_FADE;
        let fits = w >= width && ht >= height;
        assert_eq!(prepared.overlay.is_some(), valid && fits); assert_eq!(prepared.clear_spill, dialog || valid && fits);
        prepared.layout.apply(&mut h, None);
        assert_eq!(h.layout.drawer(), if valid && !fits { DrawerContent::Help } else { DrawerContent::Closed });
        assert_eq!(h.layout.peek().is_some(), valid && fits);
    }
}
#[test]
fn agent_reveal_preserves_prompts_and_precedes_paste() {
    for quit in [false, true] {
        let mut h = browsing(); if quit { assert!(h.focus.ask_quit(true)); } else { h.focus.open_create("dir".into()); }
        h.events.clear(); let prompt = h.focus.prompt().cloned(); let before = retained(&h);
        let effect = h.layout.reveal_agent(); finish_agent_reveal(effect, &mut h, Some(b"crop"));
        assert_eq!(h.focus.prompt(), prompt.as_ref()); assert_eq!(h.focus.focus(), PaneFocus::Llm);
        assert_eq!(h.pastes, [(PaneFocus::Llm, b"crop".to_vec())]); assert_eq!(h.events, ["focus:Llm", "paste"]);
        let mut expected = before; expected.1 = false; assert_eq!(retained(&h), expected);
    }
}
#[test]
fn help_diagnostics_keep_targets_fields_and_order() {
    let capture = sot_platform::test_log::capture();
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::Preview, false);
    h.context.pane = help::Pane::Preview;
    let k = Key::Named(NamedKey::F1); let now = h.now;
    let effect = h.layout.help_key(HelpKey { logical_key: &k, action: Some(Action::ToggleHelp), repeat: false, modifiers: PLAIN },
        h.context.clone(), h.focus.focus(), &h.bindings, || { tracing::debug!("recorder clock"); now });
    assert!(finish_help_key(effect, &mut h).is_break());
    let effect = h.layout.help_key(HelpKey { logical_key: &k, action: Some(Action::ToggleHelp), repeat: true, modifiers: PLAIN },
        h.context.clone(), h.focus.focus(), &h.bindings, || panic!("repeat sampled the clock"));
    assert!(finish_help_key(effect, &mut h).is_break());
    h.browse(Action::ToggleHelp); h.fail = true; h.browse(Action::HelpCheatSheet);
    if h.named(Action::HelpDown).is_continue() { tracing::debug!("key pressed"); }
    let text = capture.text();
    let target = "sot::ui::input::global_keys:";
    for fields in ["context help requested repeat=false peek=false context=Context", "context help requested repeat=true peek=true",
        "help drawer key event.logical_key=Named(F1) action=Some(HelpCheatSheet)",
        "help cheat sheet: open_html_in_browser failed error=sheet refused"] {
        assert!(text.contains(&format!("{target} {fields}")), "{text}");
    }
    let at = |needle| text.find(needle).unwrap();
    for (a, b) in [("context help requested", "recorder clock"), ("help drawer key", "recorder html"),
        ("recorder html", "help cheat sheet:"), ("help cheat sheet:", "recorder status")] { assert!(at(a) < at(b)); }
    assert!(text[at("recorder status")..].contains("recorder redraw")); assert!(!text.contains("key pressed"));
}

/// The chat is shown in Chat and in Help opened from Chat, and nowhere else: not once the REPL opens over that Help, not
/// in Help opened from the REPL.
#[test]
fn chat_is_shown_in_chat_and_in_help_opened_from_chat_only() {
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
    assert!(!h.layout.chat_shown());
    h.toggle(DrawerContent::Chat);
    assert!(h.layout.chat_shown());
    h.open();
    assert!(h.layout.chat_shown(), "Help opened from Chat");
    h.layout.show_repl_automatically(true, true);
    assert!(!h.layout.chat_shown(), "the REPL over that Help");
    h.close();
    assert_eq!(h.layout.drawer(), DrawerContent::Chat);
    assert!(h.layout.chat_shown());
    h.toggle(DrawerContent::Chat);
    assert!(!h.layout.chat_shown(), "closed");
    let mut h = Harness::new(DrawerContent::Repl, PaneFocus::NavTree, false);
    h.open();
    assert!(!h.layout.chat_shown(), "Help opened from the REPL");
}

#[test]
fn a_rows_drawer_is_what_its_tenant_shows_and_under_help_what_help_covers() {
    for drawer in DRAWERS { for focus in FOCI {
        let mut h = Harness::new(drawer, focus, false);
        let own = if drawer == DrawerContent::Help { DrawerContent::Closed } else { drawer };
        assert_eq!(h.layout.row_drawer(), own_height(own));
        h.open();
        assert_eq!(h.layout.row_drawer(), own_height(own), "Help hid the row's drawer from its parked view");
    } }
}
#[test]
fn entering_a_row_shows_its_drawer_and_moves_only_drawer_focus_off_a_closed_one() {
    for from in DRAWERS { for focus in FOCI { for row in [DrawerContent::Closed, DrawerContent::Repl, DrawerContent::Terminal,
        DrawerContent::Monitor, DrawerContent::Chat] {
        let mut h = Harness::new(from, focus, false);
        h.enter(own_height(row), focus);
        assert_eq!(h.layout.drawer(), row);
        let expected = if focus == PaneFocus::Repl && row == DrawerContent::Closed { PaneFocus::NavTree } else { focus };
        assert_eq!(h.focus.focus(), expected);
        assert!(h.events.iter().any(|e| e == "monitor"));
        assert_eq!(h.layout.chat_shown(), row == DrawerContent::Chat);
    } } }
}
#[test]
fn a_switch_while_help_shows_closes_help_and_keeps_the_layout_it_hid() {
    let mut h = Harness::new(DrawerContent::Terminal, PaneFocus::Preview, true);
    h.open();
    assert!(!h.layout.maximized());
    let parked = h.layout.row_drawer();
    assert_eq!(parked, own_height(DrawerContent::Terminal));
    h.enter(own_height(DrawerContent::Chat), PaneFocus::Repl);
    assert_eq!((h.layout.drawer(), h.layout.maximized(), h.layout.chat_shown()), (DrawerContent::Chat, true, true));
    assert_eq!(h.focus.focus(), PaneFocus::Preview, "Help's focus stayed in the entered row's chat");
    h.layout.close_help();
    assert_eq!(h.layout.drawer(), DrawerContent::Closed, "the closed Help's origin brought the left row's drawer back");
    h.open();
    h.enter(parked, PaneFocus::Repl);
    assert_eq!(h.layout.drawer(), DrawerContent::Terminal);
}
#[test]
fn a_switch_while_help_shows_returns_the_focus_help_took_and_never_moves_it_into_the_entered_drawer() {
    // Help takes focus into the drawer; the maximize it hid follows focus, so the focus must go back with it.
    for origin in [PaneFocus::NavTree, PaneFocus::Preview, PaneFocus::Llm] {
        for row in [DrawerContent::Closed, DrawerContent::Repl, DrawerContent::Chat] {
            let mut h = Harness::new(DrawerContent::Closed, origin, true);
            h.open();
            assert_eq!(h.focus.focus(), PaneFocus::Repl);
            h.enter(own_height(row), h.focus.focus());
            assert_eq!((h.focus.focus(), h.layout.maximized()), (origin, true), "{origin:?} then {row:?}");
        }
    }
    // Help opened from the drawer itself: the focus stays there only where the entered row has a drawer.
    let mut h = Harness::new(DrawerContent::Repl, PaneFocus::Repl, false);
    h.open();
    h.enter(own_height(DrawerContent::Closed), PaneFocus::Repl);
    assert_eq!(h.focus.focus(), PaneFocus::NavTree);
}

/// The presets full height is tried on: the three defaults, and user presets with the agent column in each place and
/// with a `repl` column on either side of it.
fn presets() -> Vec<(&'static str, LayoutPreset)> {
    let user = |slots: &[Slot], widths: &[f32]| LayoutPreset {
        columns: slots.iter().zip(widths).map(|(&slot, &width)| Column { slot, width }).collect(),
        drawer: Some(Slot::Repl), drawer_height: 0.40,
    };
    vec![
        ("ultrawide", LayoutPreset::default_ultrawide()), ("laptop", LayoutPreset::default_laptop()),
        ("portrait", LayoutPreset::default_portrait()),
        ("nav,llm,preview", user(&[Slot::Nav, Slot::Llm, Slot::Preview], &[0.2, 0.4, 0.4])),
        ("nav,preview,repl,llm", user(&[Slot::Nav, Slot::Preview, Slot::Repl, Slot::Llm], &[0.2, 0.3, 0.1, 0.4])),
        ("nav,preview,llm,repl", user(&[Slot::Nav, Slot::Preview, Slot::Llm, Slot::Repl], &[0.2, 0.3, 0.3, 0.2])),
        ("repl,nav,llm", user(&[Slot::Repl, Slot::Nav, Slot::Llm], &[0.1, 0.3, 0.6])),
        ("llm,nav,preview", user(&[Slot::Llm, Slot::Nav, Slot::Preview], &[0.5, 0.2, 0.3])),
        ("llm", user(&[Slot::Llm], &[1.0])),
        ("nav", user(&[Slot::Nav], &[1.0])),
        ("narrow", user(&[Slot::Nav, Slot::Preview, Slot::Llm], &[0.01, 0.01, 0.98])),
    ]
}
fn rect(w: u16, h: u16) -> ratatui::layout::Rect { ratatui::layout::Rect { x: 0, y: 0, width: w, height: h } }
/// The geometry a frame lays out for this layout state and focus, as `draw_chrome` does.
fn laid_out(layout: &LayoutHelp, base: &LayoutPreset, focus: PaneFocus, w: u16, h: u16) -> crate::ui::chrome::layout::LayoutGeom {
    crate::ui::chrome::layout::compute(rect(w, h), &layout.effective_preset(base), layout.drawer().is_open(),
        layout.full_shown(base, focus), None)
}
/// Whether the focused pane is drawn, where it was drawn at the normal height (a column too narrow to lay out, or a
/// closed drawer, is no fault of the full height).
fn focus_is_drawn(layout: &LayoutHelp, base: &LayoutPreset, focus: PaneFocus) -> bool {
    let at = |full| crate::ui::chrome::layout::compute(rect(120, 40), &layout.effective_preset(base), layout.drawer().is_open(), full, None)
        .rect_for(focus.slot()).area();
    at(false) == 0 || laid_out(layout, base, focus, 120, 40).rect_for(focus.slot()).area() > 0
}

/// A key on the drawer cycle, for each tenant: closed, open, full height, closed.
#[test]
fn a_drawers_own_key_cycles_closed_open_full_closed() {
    let base = LayoutPreset::default_laptop();
    for tenant in TENANTS {
        let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
        h.cycle(tenant);
        assert_eq!((h.layout.drawer(), h.layout.full_shown(&base, h.focus.focus()), h.focus.focus()), (tenant, false, PaneFocus::Repl), "{tenant:?} opens");
        h.cycle(tenant);
        assert_eq!((h.layout.drawer(), h.layout.full_shown(&base, h.focus.focus()), h.focus.focus()), (tenant, true, PaneFocus::Repl), "{tenant:?} full");
        h.cycle(tenant);
        assert_eq!((h.layout.drawer(), h.layout.full_shown(&base, h.focus.focus()), h.focus.focus()), (DrawerContent::Closed, false, PaneFocus::NavTree), "{tenant:?} closes");
        h.cycle(tenant);
        assert_eq!((h.layout.drawer(), h.layout.full_shown(&base, h.focus.focus())), (tenant, false), "the next open is at the normal height");
    }
}
/// Full height is the whole height of the columns the drawer covers and moves nothing else: the agent pane and every
/// column right of it keep their rects at every window width, on every preset that has a full height. A preset
/// without one (no drawer, the agent column first or alone) goes from open to closed.
#[test]
fn full_height_covers_its_columns_and_moves_nothing_else_on_every_preset() {
    use crate::ui::chrome::layout::compute;
    for (name, base) in presets() {
        assert_eq!(base.full_span().is_none(), matches!(name, "llm,nav,preview" | "llm"), "{name}: the agent column first has no full height");
        let Some(span) = base.full_span() else {
            let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
            h.cycle_on(DrawerContent::Chat, &base);
            assert_eq!(h.layout.drawer(), DrawerContent::Chat);
            h.cycle_on(DrawerContent::Chat, &base);
            assert_eq!(h.layout.drawer(), DrawerContent::Closed, "{name}: open, then closed");
            continue;
        };
        assert_eq!(span, base.columns.iter().position(|c| c.slot == Slot::Llm).unwrap_or(base.columns.len()), "{name}");
        for w in 20..=300 { for h in [12, 40] {
            let (open, full) = (compute(rect(w, h), &base, true, false, None), compute(rect(w, h), &base, true, true, None));
            let ctx = format!("{name} {w}x{h}");
            assert_eq!((full.llm, full.preview.filter(|_| base.columns[span..].iter().any(|c| c.slot == Slot::Preview))),
                (open.llm, open.preview.filter(|_| base.columns[span..].iter().any(|c| c.slot == Slot::Preview))), "{ctx}: nothing right of the drawer moves");
            let (open_repl, full_repl) = (open.repl.unwrap(), full.repl.unwrap_or_default());
            if full_repl.height == open_repl.height { assert_eq!(full.repl, open.repl, "{ctx}: a width with no full height is the normal one"); continue; }
            assert_eq!((full_repl.x, full_repl.width, full_repl.y, full_repl.height), (open_repl.x, open_repl.width, 1, h - 2), "{ctx}: the drawer's extent");
            for covered in &base.columns[..span] {
                if covered.slot != Slot::Repl { assert_eq!(full.rect_for(covered.slot).area(), 0, "{ctx}: {:?} is covered", covered.slot); }
            }
            assert!(full.hlines.is_empty(), "{ctx}");
        } }
    }
}
/// Another tenant's key swaps and keeps the height; the key of a drawer that is open but not full goes to full.
#[test]
fn another_drawers_key_swaps_and_keeps_the_height() {
    let base = LayoutPreset::default_laptop();
    for first in TENANTS { for second in TENANTS.into_iter().filter(|t| *t != first) {
        for full in [false, true] {
            let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
            h.cycle(first); if full { h.cycle(first); }
            h.cycle(second);
            assert_eq!((h.layout.drawer(), h.layout.full_shown(&base, h.focus.focus())), (second, full), "{first:?} then {second:?}");
        }
    } }
}
/// A close by any path ends the full height, and `toggle_drawer`, which the Connect step and the fixtures use, closes
/// a drawer in one step whatever its height. A row's parked view holds the drawer and its height (next test), so a
/// row never meets the height of the row left.
#[test]
fn every_close_ends_the_full_height_and_a_toggle_closes_in_one_step() {
    let base = LayoutPreset::default_laptop();
    let full = |tenant| { let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false); h.cycle(tenant); h.cycle(tenant); h };
    for tenant in TENANTS {
        let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false); h.cycle(tenant); h.toggle(tenant);
        assert_eq!(h.layout.drawer(), DrawerContent::Closed, "toggle closes an open {tenant:?}");
        let mut h = full(tenant); h.toggle(tenant);
        assert_eq!(h.layout.drawer(), DrawerContent::Closed, "toggle closes a full {tenant:?}");
        h.cycle(tenant); assert!(!h.layout.full_shown(&base, h.focus.focus()));
        let mut h = full(tenant); h.layout.terminal_spawn_failed();
        h.cycle(tenant); assert!(!h.layout.full_shown(&base, h.focus.focus()), "after a spawn failure");
        let mut h = full(tenant);
        h.enter(own_height(DrawerContent::Closed), PaneFocus::Repl);
        assert_eq!(h.layout.row_drawer(), RowDrawer::default(), "a row with no drawer");
        h.cycle(tenant); assert!(!h.layout.full_shown(&base, h.focus.focus()), "opens at the normal height");
        let mut h = full(tenant);
        h.enter(own_height(DrawerContent::Terminal), PaneFocus::Repl);
        assert!(!h.layout.full_shown(&base, h.focus.focus()), "a row's drawer open at its own height meets no full height");
        assert_eq!(h.layout.row_drawer(), own_height(DrawerContent::Terminal), "and parks at it");
    }
}
/// A row's drawer is kept whole: the tenant and the height. A row parks what it shows, whichever drawer the entered row
/// has (none, open at its own height, full) and whether the row was left under Help, and finds it again as it left it.
#[test]
fn a_row_comes_back_to_its_drawer_at_the_height_it_left_it() {
    let base = LayoutPreset::default_laptop();
    for tenant in TENANTS { for help in [false, true] { for other in [RowDrawer::default(), own_height(DrawerContent::Chat),
        RowDrawer { content: DrawerContent::Monitor, full: true }] {
        let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
        h.cycle(tenant); h.cycle(tenant);
        if help { h.open(); }
        let left = h.layout.row_drawer();
        assert_eq!(left, RowDrawer { content: tenant, full: true }, "{tenant:?} help={help}: parked whole");
        h.enter(other, h.focus.focus());
        assert_eq!(h.layout.row_drawer(), other, "{tenant:?} then {other:?}");
        h.enter(left, h.focus.focus());
        assert_eq!((h.layout.drawer(), h.layout.row_drawer()), (tenant, left), "{tenant:?} help={help} then {other:?}");
        assert!(h.layout.full_shown(&base, h.focus.focus()), "{tenant:?} help={help} then {other:?}: full where the row left it");
        assert_eq!(h.focus.focus(), PaneFocus::Repl, "the full drawer holds the focus it covers the nav of");
    } } }
    // A row left at its own height comes back at it, and a closed drawer is never full.
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
    h.cycle(DrawerContent::Chat);
    let left = h.layout.row_drawer();
    assert_eq!(left, own_height(DrawerContent::Chat));
    h.enter(RowDrawer { content: DrawerContent::Terminal, full: true }, PaneFocus::Repl);
    h.enter(left, PaneFocus::Repl);
    assert!(!h.layout.full_shown(&base, PaneFocus::Repl), "the full height of the row left did not carry over");
    h.enter(RowDrawer { content: DrawerContent::Closed, full: true }, PaneFocus::Repl);
    assert_eq!(h.layout.row_drawer(), RowDrawer::default());
    h.cycle(DrawerContent::Chat);
    assert!(!h.layout.full_shown(&base, h.focus.focus()), "a closed drawer entered full opened at its own height");
}
/// What the cycle's fixes pinned holds on the first frame after a switch, on every preset: the focus is on a pane the
/// full height leaves drawn (a drawer that covers the pane the focus was on takes it; one beside it keeps it), a
/// layout with no full height shows the drawer open, and an answer that is the preview ends what covers it.
#[test]
fn the_rules_of_full_height_hold_on_the_arrival_at_a_full_row() {
    let full = RowDrawer { content: DrawerContent::Terminal, full: true };
    for (name, base) in presets() { for focus in FOCI {
        let mut h = Harness::new(DrawerContent::Closed, focus, false);
        h.enter_on(full, focus, &base);
        let (layout, shown) = h.layout.frame_layout(&base, h.focus.focus());
        assert!(focus_is_drawn(&h.layout, &base, h.focus.focus()), "{name} {focus:?}: the focus is on a pane that is not drawn");
        assert_eq!(h.layout.drawer(), DrawerContent::Terminal, "{name} {focus:?}: the drawer is open");
        assert_eq!(layout.drawer.is_some(), base.drawer.is_some(), "{name} {focus:?}");
        let Some(n) = base.full_span() else {
            assert!(!shown, "{name} {focus:?}: a layout with no full height shows the drawer open");
            assert_eq!(h.focus.focus(), focus, "{name} {focus:?}: the focus did not move for a height the layout lacks");
            continue;
        };
        let leaves = focus == PaneFocus::Repl || base.columns[n..].iter().any(|c| c.slot == focus.slot());
        assert!(shown, "{name} {focus:?}: full where the row was left");
        assert_eq!(h.focus.focus(), if leaves { focus } else { PaneFocus::Repl }, "{name} {focus:?}: the covered pane's focus goes to the drawer");
        h.layout.uncover_preview();
        assert!(!h.layout.full_shown(&base, h.focus.focus()), "{name} {focus:?}: an answer that is the preview ends it");
        assert_eq!(h.layout.drawer(), DrawerContent::Terminal);
    } }
}
/// A switch nobody at the window asked for (a goto, a control cycle, an urgent result, a row that closed, a created
/// row), or one made with a pane maximized, brings the full drawer back without the focus: what the person is typing in
/// stays theirs, and the frame ends the height for a focus on a covered pane as it does by any path.
#[test]
fn an_arrival_nobody_asked_for_or_over_a_maximized_pane_moves_no_focus() {
    let full = RowDrawer { content: DrawerContent::Chat, full: true };
    for (name, base) in presets() { for focus in FOCI {
        let mut h = Harness::new(DrawerContent::Closed, focus, false);
        h.enter_as(full, focus, &base, false);
        assert_eq!(h.focus.focus(), focus, "{name} {focus:?}: an arrival nobody asked for moved the focus");
        let (_, shown) = h.layout.frame_layout(&base, focus);
        assert!(focus_is_drawn(&h.layout, &base, focus), "{name} {focus:?}");
        let leaves = base.full_span().is_some_and(|n| focus == PaneFocus::Repl || base.columns[n..].iter().any(|c| c.slot == focus.slot()));
        assert_eq!(shown, leaves, "{name} {focus:?}: full only where the focus is on a pane it leaves drawn");
        assert_eq!(h.layout.drawer(), DrawerContent::Chat);
    } }
    for focus in [PaneFocus::NavTree, PaneFocus::Preview] {
        let mut h = Harness::new(DrawerContent::Closed, focus, true);
        h.enter(full, focus);
        assert_eq!((h.focus.focus(), h.layout.maximized()), (focus, true), "{focus:?}: a person's step with a pane maximized");
    }
}
/// The row left parks the height that is shown: with the focus put on a pane the full height covers and no frame
/// between, the flag is stale, and the row comes back at its own height, with no focus taken.
#[test]
fn a_row_left_with_the_focus_on_a_covered_pane_is_parked_at_its_own_height() {
    let base = LayoutPreset::default_laptop();
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
    h.cycle(DrawerContent::Chat); h.cycle(DrawerContent::Chat);
    h.focus.set_focus(PaneFocus::NavTree);
    let left = h.layout.park_drawer(&base, PaneFocus::NavTree);
    assert_eq!(left, own_height(DrawerContent::Chat));
    h.enter(own_height(DrawerContent::Terminal), PaneFocus::NavTree);
    h.enter(left, PaneFocus::NavTree);
    assert_eq!((h.layout.drawer(), h.focus.focus()), (DrawerContent::Chat, PaneFocus::NavTree));
    assert!(!h.layout.full_shown(&base, PaneFocus::NavTree));
    // With the focus in the drawer the record is full, as the drawer's key left it.
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
    h.cycle(DrawerContent::Chat); h.cycle(DrawerContent::Chat);
    assert_eq!(h.layout.park_drawer(&base, PaneFocus::Repl), RowDrawer { content: DrawerContent::Chat, full: true });
}
/// Full height is drawn only while the focus is on a pane it leaves drawn, on every preset: the drawer, or a column
/// right of the covered ones. Focus anywhere else shows the normal layout, whatever put it there, and the next press
/// of the drawer's key then goes to full height (the flag does not come back by itself).
#[test]
fn full_height_is_shown_only_while_the_focus_is_drawn() {
    for (name, base) in presets() {
        for focus in FOCI { for drawer in TENANTS { for full in [false, true] {
            let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
            h.cycle_on(drawer, &base); if full { h.cycle_on(drawer, &base); }
            h.focus.set_focus(focus);
            let shown = h.layout.full_shown(&base, focus);
            let survives = base.full_span().is_some_and(|n| focus == PaneFocus::Repl || base.columns[n..].iter().any(|c| c.slot == focus.slot()));
            assert_eq!(shown, full && survives && base.full_span().is_some(), "{name} {focus:?}");
            assert!(focus_is_drawn(&h.layout, &base, focus), "{name} {focus:?} full={full}: focus is on a pane that is not drawn");
            h.layout.frame_layout(&base, focus);
            h.focus.set_focus(PaneFocus::Repl);
            assert_eq!(h.layout.full_shown(&base, PaneFocus::Repl), full && survives, "{name} {focus:?}: the flag is over once settled");
        } } }
    }
    let base = LayoutPreset::default_laptop();
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
    h.cycle(DrawerContent::Chat); h.cycle(DrawerContent::Chat);
    h.focus.set_focus(PaneFocus::NavTree); h.layout.frame_layout(&base, PaneFocus::NavTree);
    h.focus.set_focus(PaneFocus::Repl);
    assert!(!h.layout.full_shown(&base, PaneFocus::Repl), "focus coming back does not bring the full height back");
    h.cycle(DrawerContent::Chat);
    assert!(h.layout.full_shown(&base, PaneFocus::Repl), "the key does");
}
/// The key settles a full height that is not shown before it reads it: Escape gives the nav the keys, and the drawer's
/// key, pressed before any frame is drawn, goes to full height (it is open at its height) instead of closing it.
#[test]
fn the_drawers_key_meets_the_height_that_is_shown_not_a_stale_flag() {
    let base = LayoutPreset::default_laptop();
    for tenant in TENANTS {
        let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
        h.cycle(tenant); h.cycle(tenant);
        h.focus.set_focus(PaneFocus::NavTree);
        h.cycle(tenant);
        assert_eq!(h.layout.drawer(), tenant, "{tenant:?}: the drawer was shown at its height, so the key does not close it");
        assert!(h.layout.full_shown(&base, h.focus.focus()), "{tenant:?}: it goes full and takes the focus");
    }
}
/// `compute` keeps the normal height where the column just left of the agent column is under 2 cells wide, because
/// the drawer then spans the whole width as it does at the normal height: the agent pane, laid out and 70 cells wide,
/// does not make up for it.
#[test]
fn full_height_waits_for_a_column_left_of_the_agent_wide_enough_to_lay_out() {
    use crate::ui::chrome::layout::compute;
    let base = LayoutPreset {
        columns: vec![Column { slot: Slot::Nav, width: 0.40 }, Column { slot: Slot::Preview, width: 0.01 },
            Column { slot: Slot::Llm, width: 0.59 }],
        drawer: Some(Slot::Repl), drawer_height: 0.40,
    };
    let (open, full) = (compute(rect(120, 40), &base, true, false, None), compute(rect(120, 40), &base, true, true, None));
    assert_eq!(full.llm.map(|r| r.width), Some(70), "the agent pane is laid out");
    assert_eq!((full.repl, full.llm, full.nav, full.hlines.clone()), (open.repl, open.llm, open.nav, open.hlines), "the normal height");
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
    h.cycle_on(DrawerContent::Chat, &base); h.cycle_on(DrawerContent::Chat, &base);
    assert!(h.layout.full_shown(&base, PaneFocus::Repl), "the layout state is full; only the geometry declines");
}
/// An answer that is the preview ends what covers the preview, the full height and maximize alike: the drawer is at its
/// height, the window is whole, and the drawer's key goes to full height again.
#[test]
fn an_uncovered_preview_ends_the_full_height_and_maximize_and_the_key_goes_full_again() {
    let base = LayoutPreset::default_laptop();
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
    h.cycle(DrawerContent::Chat); h.cycle(DrawerContent::Chat);
    assert!(h.layout.full_shown(&base, PaneFocus::Repl));
    h.layout.uncover_preview();
    assert!(!h.layout.full_shown(&base, PaneFocus::Repl) && h.layout.drawer() == DrawerContent::Chat);
    h.cycle(DrawerContent::Chat);
    assert!(h.layout.full_shown(&base, PaneFocus::Repl));
    assert!(h.action(Action::MaximizePane) && h.layout.maximized());
    h.layout.uncover_preview();
    assert!(!h.layout.maximized() && !h.layout.full_shown(&base, PaneFocus::Repl));
}
/// The key paths that once rested the focus on a pane the full height hid: Help then a spatial move then a row
/// switch or an automatic REPL open; a reveal of the agent; the wide-preview key. After every step, on every preset,
/// the focused pane is drawn.
#[test]
fn no_key_path_rests_the_focus_on_a_pane_that_is_not_drawn() {
    for (name, base) in presets() {
        let check = |h: &Harness, step: &str| assert!(focus_is_drawn(&h.layout, &base, h.focus.focus()), "{name}: {step}: {:?}", h.focus.focus());
        let start = || { let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
            h.cycle_on(DrawerContent::Terminal, &base); h.cycle_on(DrawerContent::Terminal, &base); h };
        for dir in [SpatialDir::Up, SpatialDir::Left, SpatialDir::Right] {
            let spatial = |h: &mut Harness| { let (c, d) = h.layout.spatial_slots(&base, h.focus.focus()); h.focus.move_focus(dir, &c, d); };
            for row in [DrawerContent::Chat, DrawerContent::Closed, DrawerContent::Terminal] {
                let mut h = start(); h.open(); spatial(&mut h); check(&h, "help, spatial");
                h.enter(own_height(row), h.focus.focus());
                h.layout.frame_layout(&base, h.focus.focus()); check(&h, "row switch");
            }
            let mut h = start(); h.open(); spatial(&mut h); spatial(&mut h);
            h.layout.show_repl_automatically(true, true);
            h.layout.frame_layout(&base, h.focus.focus()); check(&h, "run opens the REPL");
            let mut h = start(); spatial(&mut h); h.layout.frame_layout(&base, h.focus.focus()); check(&h, "spatial");
            let e = h.layout.reveal_agent(); e.apply(&mut h, None); h.layout.frame_layout(&base, h.focus.focus()); check(&h, "reveal");
        }
        let mut h = start(); h.action(Action::ToggleWidePreview); h.layout.frame_layout(&base, h.focus.focus()); check(&h, "wide preview");
    }
}
/// Help is at the drawer's height over a full drawer and the full height returns with the drawer under it; the
/// maximized window shows the full drawer alone and restores to it.
#[test]
fn help_and_maximize_over_a_full_drawer() {
    use crate::ui::chrome::layout::compute;
    let base = LayoutPreset::default_laptop();
    let mut h = Harness::new(DrawerContent::Closed, PaneFocus::NavTree, false);
    h.cycle(DrawerContent::Terminal); h.cycle(DrawerContent::Terminal);
    h.open();
    assert_eq!(h.layout.drawer(), DrawerContent::Help);
    assert!(!h.layout.full_shown(&base, h.focus.focus()), "Help is at the drawer's height");
    h.close();
    assert_eq!((h.layout.drawer(), h.layout.full_shown(&base, h.focus.focus())), (DrawerContent::Terminal, true));
    assert!(h.action(Action::MaximizePane));
    let slot = h.layout.maximize_slot(h.focus.focus(), false);
    assert_eq!(slot, Some(Slot::Repl));
    assert_eq!(compute(rect(120, 40), &h.layout.effective_preset(&base), true, true, slot).repl.unwrap().height, 38);
    assert!(h.action(Action::RestoreLayout));
    assert_eq!((h.layout.maximized(), h.layout.full_shown(&base, h.focus.focus()), h.layout.drawer()), (false, true, DrawerContent::Terminal));
}
