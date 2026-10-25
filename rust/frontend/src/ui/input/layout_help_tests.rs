//! Characterizations through public owner operations and the production effect continuations.
use super::*;
use super::layout_help::*;
use super::keybindings::ACTIONS;
use crate::ui::persist::settings::{Column, LayoutPreset, Slot};
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

const PLAIN: Modifiers = Modifiers { ctrl: false, super_: false, alt: false, shift: false };
const FOCI: [PaneFocus; 4] = [PaneFocus::NavTree, PaneFocus::Preview, PaneFocus::Llm, PaneFocus::Repl];
const DRAWERS: [DrawerContent; 5] = [DrawerContent::Closed, DrawerContent::Repl,
    DrawerContent::Terminal, DrawerContent::Monitor, DrawerContent::Help];
const TENANTS: [DrawerContent; 3] = [DrawerContent::Repl, DrawerContent::Terminal, DrawerContent::Monitor];

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
    fn open(&mut self) {
        let e = self.layout.open_help(self.context.clone(), self.focus.focus()); e.apply(self, None);
    }
    fn close(&mut self) { let e = self.layout.close_help(); e.apply(self, None); }
    fn toggle(&mut self, tenant: DrawerContent) {
        let e = self.layout.toggle_drawer(tenant, self.focus.focus()); e.apply(self, Some("bound".into()));
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
    fn layout_focus(&mut self, focus: PaneFocus) { self.events.push(format!("focus:{focus:?}")); self.focus.set_focus(focus); }
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
    let e = h.layout.show_repl_automatically(true, true); e.apply(&mut h, None);
    let e = h.layout.show_demo_repl(); e.apply(&mut h, None);
    let e = h.layout.terminal_spawn_failed(); e.apply(&mut h, None);
    assert!(!h.events.iter().any(|e| e == "monitor"));
}
#[test]
fn automatic_and_demo_repl_opening_keep_their_distinct_gates() {
    for enabled in [false, true] { for has_drawer in [false, true] { for drawer in DRAWERS {
        let mut h = Harness::new(drawer, PaneFocus::Llm, true);
        let before = retained(&h);
        let e = h.layout.show_repl_automatically(enabled, has_drawer); e.apply(&mut h, None);
        assert_eq!(h.layout.drawer(), if enabled && has_drawer { DrawerContent::Repl } else { drawer });
        let e = h.layout.show_demo_repl(); e.apply(&mut h, None);
        assert_eq!(h.layout.drawer(), DrawerContent::Repl); assert_eq!(retained(&h), before);
        assert_eq!(h.focus.focus(), PaneFocus::Llm); assert!(h.events.is_empty());
    } } }
}
#[test]
fn terminal_spawn_failure_closes_only_the_drawer() {
    let mut h = browsing();
    h.toggle(DrawerContent::Terminal); let before = retained(&h); let focus = h.focus.focus(); h.events.clear();
    let e = h.layout.terminal_spawn_failed(); e.apply(&mut h, None);
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
    for base in [LayoutPreset::default_laptop(), LayoutPreset::default_portrait(), omitted, custom, drawerless] {
        for wide in [false, true] { for drawer in DRAWERS {
            let mut h = Harness::new(drawer, PaneFocus::NavTree, true);
            if wide { h.action(Action::ToggleWidePreview); }
            let p = h.layout.effective_preset(&base); let (columns, slot) = h.layout.spatial_slots(&base);
            assert_eq!(columns, p.columns.iter().map(|c| c.slot).collect::<Vec<_>>());
            assert!((p.columns.iter().map(|c| c.width).sum::<f32>() - 1.0).abs() < 0.001);
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
        let e = match path { 0 => h.layout.toggle_drawer(DrawerContent::Repl, h.focus.focus()),
            1 => h.layout.show_repl_automatically(true, true), 2 => h.layout.show_demo_repl(), _ => h.layout.terminal_spawn_failed() };
        e.apply(&mut h, None); h.close();
        assert_eq!(h.layout.drawer(), if path == 0 { DrawerContent::Closed } else { DrawerContent::Monitor });
        assert_eq!(h.focus.focus(), if path == 0 { PaneFocus::NavTree } else { PaneFocus::Llm });
        assert_eq!(h.layout.maximized(), path != 0);
    }
}
#[test]
fn help_open_resets_browsing_without_resetting_wide_preview() {
    let mut h = browsing(); h.browse(Action::HelpScope); h.peek();
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
        (width, height, PEEK_HOLD, false, false), (width, height, PEEK_HOLD + PEEK_FADE - Duration::from_nanos(1), false, false),
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
