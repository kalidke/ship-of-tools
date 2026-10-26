use super::*;
use super::keypress::{input_continuation, stayed_key_continuation, InputRoute};
use super::mouse::{pointer_continuation, agent_press_continuation};
use super::global_keys::{quit_chord_route, quit_chord_continuation};
use crate::ui::{FilePromptContext, confirm_create, confirm_delete};
use crate::ui::preview::image::overlay::{ScalePromptContext, confirm_scale};
use std::ops::ControlFlow::{Break, Continue};
use std::time::{Duration, Instant};

const PANES: [PaneFocus; 4] = [PaneFocus::NavTree, PaneFocus::Preview, PaneFocus::Llm, PaneFocus::Repl];

fn owner(focus: PaneFocus) -> FocusPrompts { FocusPrompts::new(focus) }

fn type_text(p: &mut FocusPrompts, text: &str) {
    for c in text.chars() { p.push_char(c); }
}

fn open_variant(p: &mut FocusPrompts, variant: usize) {
    match variant {
        0 => { p.open_create("files:sub".into()); type_text(p, "typed-name/"); }
        1 => { p.open_delete("files:sub/a".into(), "a".into()); }
        2 => { p.begin_scale("files:image.png".into()); type_text(p, "12.5"); }
        3 => { assert!(p.ask_quit(true)); }
        _ => {}
    }
}

#[test]
fn focus_changes_only_the_selected_pane() {
    let now = Instant::now();
    for initial in PANES { for to in PANES { for variant in 0..5 {
        let mut p = owner(initial);
        assert!(p.prompt().is_none() && !p.stayed() && p.hint().is_none());
        p.begin_scale("files:prior.png".into()); p.begin_leave(); p.set_focus(initial);
        open_variant(&mut p, variant);
        p.record_stay(LeaveIntent::Handover); p.show_hint("Fixture+Q", || now);
        let before = p.prompt().cloned();
        let hint = p.hint().unwrap().to_string(); let deadline = p.hint_deadline();
        p.set_focus(to);
        assert_eq!(p.focus(), to); assert_eq!(p.prompt(), before.as_ref());
        assert!(p.stayed()); assert_eq!(p.hint(), Some(hint.as_str()));
        assert_eq!(p.hint_deadline(), deadline);
        assert!(p.cancel().redraw); assert_eq!(p.focus(), initial);
    } } }
}
fn raw(p: &mut FocusPrompts, logical: Key, repeat: bool, synthetic: bool, pressed: bool,
    quit_again: bool, consumes_text: bool) -> (Vec<&'static str>, Option<LeaveIntent>) {
    let keep = p.quit_choice();
    let mut probe = (p, Vec::new(), None);
    input_continuation(&mut probe, &logical, repeat, synthetic, pressed, keep, quit_again,
        |s| { s.1.push("prepare"); (None, ()) }, |_| false, |s, route| match route {
            InputRoute::Prompt(step) => {
                s.1.push("question");
                s.2 = s.0.answer_quit(step).1;
            }
            InputRoute::Next(_, ()) => {
                let dismissed = s.0.dismiss_stay_key(consumes_text);
                let _ = stayed_key_continuation(s, dismissed, |s| s.1.push("redraw"), |s| {
                    s.1.push("next"); Continue(())
                });
            }
        });
    (probe.1, probe.2)
}

#[test]
fn a_focus_move_leaves_the_quit_question_open() {
    for initial in PANES {
        for keep in [false, true] {
            let mut p = owner(initial);
            assert!(p.ask_quit(keep));
            for to in PANES {
                p.set_focus(to);
                assert_eq!(p.quit_choice(), Some(keep));
            }
            let (seen, leave) = raw(&mut p, Key::Named(NamedKey::Tab), false, false, true, false, false);
            assert_eq!((seen, leave), (vec!["question"], None));
            assert_eq!(p.quit_choice(), Some(!keep));
            let (seen, leave) = raw(&mut p, Key::Named(NamedKey::Enter), false, false, true, false, false);
            assert_eq!(seen, vec!["question"]);
            assert_eq!(leave, Some(if keep { LeaveIntent::Close } else { LeaveIntent::Keep }));
        }
    }
}

#[test]
fn spatial_focus_walks_only_the_supplied_visible_slots() {
    use crate::ui::persist::settings::Slot as S;
    use PaneFocus::*;
    use SpatialDir::*;
    let full = &[S::Nav, S::Preview, S::Llm][..];
    let wide = &[S::Nav, S::Preview][..];
    let omitted = &[S::Nav, S::Llm][..];
    let portrait = &[S::Preview][..];
    for (from, dir, columns, drawer, want) in [
        (NavTree, Right, full, None, Preview), (Llm, Right, full, None, Llm),
        (Preview, Down, full, Some(S::Repl), Repl), (Repl, Up, full, Some(S::Repl), Preview),
        (Repl, Right, full, Some(S::Repl), Llm), (Repl, Down, full, Some(S::Repl), Repl),
        (Llm, Left, wide, None, NavTree), (Preview, Right, wide, None, Preview),
        (NavTree, Right, omitted, None, Llm), (Repl, Up, omitted, Some(S::Repl), NavTree),
        (NavTree, Down, omitted, Some(S::Preview), Preview),
        (Preview, Right, omitted, Some(S::Preview), Llm),
        (Llm, Right, portrait, None, Preview), (Llm, Down, &[][..], None, Llm),
    ] {
        let mut p = owner(from);
        p.move_focus(dir, columns, drawer);
        assert_eq!(p.focus(), want);
    }
}

#[test]
fn prompt_edits_keep_each_variants_input_rules() {
    let mut p = owner(PaneFocus::Preview);
    assert!(!p.backspace() && !p.push_char('a'));
    p.open_create("files:".into()); assert!(p.backspace());
    assert!(!p.push_char('/') && !p.push_char('\\'));
    type_text(&mut p, "sub");
    assert!(!p.push_char('\\'));
    assert!(p.push_char('/'));
    for c in ['/', '\\', 'x'] { assert!(!p.push_char(c)); }
    assert_eq!(p.create_draft(), Some(("files:".into(), "sub/".into())));
    assert!(p.backspace() && p.push_char('x'));
    assert_eq!(p.create_draft().unwrap().1, "subx");
    p.begin_scale("files:image.png".into()); assert!(p.backspace()); type_text(&mut p, "12.5");
    for c in ['.', 'e', '-', ' ', 'é', '４', '/', '\\'] { assert!(!p.push_char(c)); }
    assert_eq!(p.scale_draft().unwrap().1, "12.5");
    for _ in 0..5 { assert!(p.backspace()); }
    assert_eq!(p.scale_draft().unwrap().1, "");
    p.open_delete("files:a".into(), "a".into()); assert!(!p.backspace() && !p.push_char('a'));
    p.cancel(); p.ask_quit(false); assert!(!p.backspace() && !p.push_char('1'));
}
struct Probe {
    p: FocusPrompts,
    sent: bool,
    collision: bool,
    created: Option<String>,
    deleted: Option<String>,
    events: Vec<String>,
    effects: Vec<PromptEffect>,
}
impl Probe {
    fn new(focus: PaneFocus) -> Self {
        Self { p: owner(focus), sent: true, collision: false,
            created: None, deleted: None, events: vec![], effects: vec![] }
    }
    fn status(&self) -> &str {
        let effect = self.effects.last().unwrap(); assert!(effect.redraw);
        effect.status.as_deref().unwrap()
    }
    fn send_result(&self) -> Result<(), String> { if self.sent { Ok(()) } else { Err("fixture closed".into()) } }
}
impl FilePromptContext for Probe {
    fn prompts(&mut self) -> &mut FocusPrompts { &mut self.p }
    fn file_exists(&self, _: &str) -> bool { self.collision }
    fn send_create(&mut self, node_id: &str, is_dir: bool) -> Result<(), String> {
        assert!(self.p.create_draft().is_some());
        self.events.push(format!("create:{node_id}:{is_dir}")); self.send_result()
    }
    fn send_delete(&mut self, node_id: &str) -> Result<(), String> {
        assert!(self.p.delete_target().is_some());
        self.events.push(format!("delete:{node_id}")); self.send_result()
    }
    fn mark_created(&mut self, node_id: String) {
        assert!(self.p.prompt().is_some());
        self.created = Some(node_id); self.events.push("pending-create".into());
    }
    fn mark_deleted(&mut self, node_id: String) {
        assert!(self.p.prompt().is_some());
        self.deleted = Some(node_id); self.events.push("pending-delete".into());
    }
    fn prompt_effect(&mut self, effect: PromptEffect) { self.effects.push(effect); }
}

#[test]
fn create_submission_closes_only_after_a_successful_send() {
    let mut s = Probe::new(PaneFocus::NavTree);
    s.p.open_create("files:sub".into()); type_text(&mut s.p, "   ");
    let before = s.p.prompt().cloned(); confirm_create(&mut s);
    assert_eq!(s.p.prompt(), before.as_ref());
    assert!(s.events.is_empty() && s.created.is_none());
    assert_eq!(s.status(), "new file · name is empty");
    for is_dir in [false, true] {
        s.p.open_create("files:sub".into()); type_text(&mut s.p, if is_dir { "a/" } else { "a" });
        let before = s.p.prompt().cloned();
        let kind = if is_dir { "new dir" } else { "new file" };
        s.collision = true; confirm_create(&mut s);
        assert!(s.events.is_empty() && s.created.is_none());
        assert_eq!(s.status(), format!("{kind} · 'a' already exists"));
        assert_eq!(s.p.prompt(), before.as_ref());
        s.collision = false; s.sent = false; confirm_create(&mut s);
        assert_eq!(s.p.prompt(), before.as_ref()); assert!(s.created.is_none());
        assert_eq!(s.status(), format!("{kind} · channel closed"));
        s.sent = true; s.events.clear(); confirm_create(&mut s);
        assert!(s.p.prompt().is_none()); assert_eq!(s.created.as_deref(), Some("files:sub/a"));
        assert_eq!(s.events, [format!("create:files:sub/a:{is_dir}"), "pending-create".into()]);
        assert_eq!(s.status(), "creating a…"); s.created = None; s.events.clear();
    }
}
#[test]
fn delete_submission_closes_on_both_send_outcomes() {
    for sent in [false, true] {
        let mut s = Probe::new(PaneFocus::NavTree);
        s.sent = sent;
        s.p.open_delete("files:sub/a".into(), "a".into());
        assert_eq!(s.p.delete_target(), Some(("files:sub/a".into(), "a".into())));
        confirm_delete(&mut s);
        assert!(s.p.prompt().is_none());
        assert_eq!(s.deleted.as_deref(), sent.then_some("files:sub/a"));
        let mut want = vec!["delete:files:sub/a".to_string()];
        if sent { want.push("pending-delete".into()); }
        assert_eq!(s.events, want);
        assert_eq!(s.effects, vec![PromptEffect::message(if sent { "deleting a…" } else { "delete · channel closed" })]);
    }
}

impl ScalePromptContext for Probe {
    fn prompts(&mut self) -> &mut FocusPrompts { &mut self.p }
    fn send_scale(&mut self, node_id: &str, nm_per_px: f64) -> Result<(), String> {
        assert!(self.p.scale_draft().is_some());
        self.events.push(format!("scale:{node_id}:{nm_per_px}")); self.send_result()
    }
    fn enable_scalebar(&mut self) {
        assert!(self.p.scale_draft().is_some());
        self.events.push("scalebar".into());
    }
    fn mark_scale_saved(&mut self, node_id: String, raw: String) {
        assert!(self.p.prompt().is_none());
        assert_eq!(self.p.focus(), PaneFocus::Llm);
        self.events.push(format!("save:{node_id}:{raw}"));
    }
    fn scale_effect(&mut self, effect: PromptEffect) { self.effects.push(effect); }
}

#[test]
fn scale_resolution_restores_the_captured_focus_once() {
    for initial in PANES {
        let mut s = Probe::new(initial);
        assert_eq!(s.p.begin_scale("files:image.png".into()), PromptEffect::message("pixel size (nm): "));
        assert_eq!(s.p.focus(), PaneFocus::NavTree);
        s.p.set_focus(PaneFocus::Llm); confirm_scale(&mut s);
        assert!(s.events.is_empty()); assert_eq!(s.status(), "pixel size · enter a positive number in nm");
        assert_eq!(s.p.scale_draft(), Some(("files:image.png".into(), "".into())));
        assert_eq!(s.p.focus(), PaneFocus::Llm);
        type_text(&mut s.p, "12.5"); s.sent = false; confirm_scale(&mut s);
        assert_eq!(s.p.scale_draft().unwrap().1, "12.5");
        assert_eq!(s.p.focus(), PaneFocus::Llm); assert_eq!(s.status(), "pixel size · channel closed");
        s.events.clear(); s.sent = true; confirm_scale(&mut s);
        assert_eq!(s.events, ["scale:files:image.png:12.5", "scalebar", "save:files:image.png:12.5"]);
        assert!(s.p.prompt().is_none()); assert_eq!(s.p.focus(), initial);
        assert_eq!(s.status(), "pixel size 12.5 nm · saving…");
        s.p.set_focus(PaneFocus::Repl); s.p.cancel(); assert_eq!(s.p.focus(), PaneFocus::Repl);
        s.p.set_focus(initial); s.p.begin_scale("files:again.png".into());
        s.p.set_focus(PaneFocus::Llm); s.p.cancel(); assert_eq!(s.p.focus(), initial);
        s.p.set_focus(PaneFocus::NavTree); s.p.cancel(); assert_eq!(s.p.focus(), PaneFocus::NavTree);
    }
}
#[test]
fn cancellation_reports_the_prompt_that_was_cancelled() {
    for (variant, message) in [(0, "new file · cancelled"), (1, "delete · cancelled"),
        (2, "pixel size · cancelled"), (3, "quit · cancelled"), (4, "new file · cancelled")] {
        let mut p = owner(PaneFocus::Preview);
        open_variant(&mut p, variant);
        p.record_stay(LeaveIntent::Handover);
        p.show_hint("Fixture+Q", Instant::now);
        p.set_focus(PaneFocus::Repl);
        assert_eq!(p.cancel(), PromptEffect::message(message));
        assert!(p.prompt().is_none() && p.stayed() && p.hint().is_some());
        assert_eq!(p.focus(), if variant == 2 { PaneFocus::Preview } else { PaneFocus::Repl });
    }
}

#[test]
fn quit_never_overwrites_another_prompt() {
    for variant in 0..3 {
        for to in PANES {
            let mut p = owner(PaneFocus::Preview);
            open_variant(&mut p, variant);
            p.set_focus(to);
            let before = p.prompt().cloned();
            for keep in [false, true] {
                assert!(!p.ask_quit(keep));
                assert_eq!(p.prompt(), before.as_ref());
                assert_eq!(p.focus(), to);
            }
            p.cancel();
            assert_eq!(p.focus(), if variant == 2 { PaneFocus::Preview } else { to });
        }
    }
    let mut p = owner(PaneFocus::NavTree);
    for keep in [false, true, false] {
        assert!(p.ask_quit(keep)); assert_eq!(p.quit_choice(), Some(keep));
    }
}

fn pointer(p: &mut FocusPrompts, pressed: bool, left: bool, inside: bool, at: Option<(u16, u16)>) -> Vec<String> {
    let mut s = (p, Vec::new());
    pointer_continuation(&mut s, pressed, left, |s, pressed| s.1.push(format!("presence:{pressed}")),
        |s| s.0.quit_choice().map(|keep| {
            let d = close_dialog(ratatui::layout::Rect::new(0, 0, 120, 36), CloseDialogText::Ask { keep, notice: None });
            quit_prompt_click(d.answers, if left { at } else { None })
        }),
        |s, step| { s.1.push(format!("answer:{step:?}")); s.0.answer_quit(step); },
        |s| { let dismissed = s.0.dismiss_stay_press(); if dismissed { s.1.push("redraw".into()); } dismissed },
        |s| agent_press_continuation(s, inside.then_some(()), |s| {
            s.0.set_focus(PaneFocus::Llm); s.1.push("focus".into());
        }, |s, cell| s.1.push(format!("press:{}", cell.is_some()))),
        |s| s.1.push("release".into()));
    s.1
}

#[test]
fn pointer_modals_precede_agent_focus_and_selection() {
    let d = close_dialog(ratatui::layout::Rect::new(0, 0, 120, 36), CloseDialogText::Ask { keep: false, notice: None });
    let [no, yes] = d.answers.unwrap();
    for (left, at, want) in [(true, Some((no.x, no.y)), "Leave(Close)"),
        (true, Some((yes.x, yes.y)), "Leave(Keep)"), (true, Some((0, 0)), "Cancel"), (false, None, "Cancel")] {
        let mut p = owner(PaneFocus::Preview); p.ask_quit(false);
        p.record_stay(LeaveIntent::Handover);
        assert_eq!(pointer(&mut p, true, left, true, at), ["presence:true".into(), format!("answer:{want}")]);
        assert_eq!(p.focus(), PaneFocus::Preview);
        assert!(p.stayed());
    }
    let mut p = owner(PaneFocus::Preview); p.record_stay(LeaveIntent::Handover);
    assert_eq!(pointer(&mut p, false, true, true, None), ["presence:false", "release"]);
    assert!(p.stayed());
    assert_eq!(pointer(&mut p, true, true, true, None), ["presence:true", "redraw"]);
    assert_eq!(p.focus(), PaneFocus::Preview);
    assert_eq!(pointer(&mut p, true, true, true, None), ["presence:true", "focus", "press:true"]);
    assert_eq!(p.focus(), PaneFocus::Llm);
    p.set_focus(PaneFocus::Repl);
    assert_eq!(pointer(&mut p, true, true, false, None), ["presence:true", "press:false"]);
    assert_eq!(p.focus(), PaneFocus::Repl);
    p.ask_quit(false);
    assert_eq!(pointer(&mut p, false, true, true, None), ["presence:false", "release"]);
    assert_eq!(p.quit_choice(), Some(false));
}

#[test]
fn leave_clears_only_the_current_leave_prompt_state() {
    for initial in PANES {
        let mut p = owner(initial); p.begin_scale("files:image.png".into()); type_text(&mut p, "2");
        p.record_stay(LeaveIntent::Handover); p.show_hint("Fixture+Q", Instant::now);
        let hint = p.hint().unwrap().to_string(); let deadline = p.hint_deadline();
        p.set_focus(PaneFocus::Llm); p.begin_leave();
        assert!(p.prompt().is_none() && !p.stayed()); assert_eq!(p.focus(), PaneFocus::Llm);
        assert_eq!(p.hint(), Some(hint.as_str())); assert_eq!(p.hint_deadline(), deadline);
        assert_eq!(p.cancel(), PromptEffect::message("new file · cancelled")); assert_eq!(p.focus(), initial);
        p.set_focus(PaneFocus::Repl); p.cancel(); assert_eq!(p.focus(), PaneFocus::Repl);
    }
}
#[test]
fn a_stayed_notice_preserves_a_text_consumers_next_key() {
    for consumes in [false, true] {
        for repeat in [false, true] {
            let mut p = owner(PaneFocus::Llm); p.record_stay(LeaveIntent::Handover);
            for (synthetic, pressed) in [(true, true), (false, false)] {
                assert_eq!(raw(&mut p, Key::Character("x".into()), repeat, synthetic, pressed, false, consumes).0, Vec::<&str>::new());
                assert!(p.stayed());
            }
            assert!(raw(&mut p, Key::Named(NamedKey::Control), repeat, false, true, false, consumes).0.is_empty());
            assert!(p.stayed());
            let want = if consumes { vec!["prepare", "redraw", "next"] } else { vec!["prepare", "redraw"] };
            assert_eq!(raw(&mut p, Key::Character("x".into()), repeat, false, true, false, consumes).0, want);
            assert!(!p.stayed());
            assert_eq!(raw(&mut p, Key::Character("y".into()), false, false, true, false, consumes).0, ["prepare", "next"]);
            p.record_stay(LeaveIntent::Handover);
            assert!(p.dismiss_stay_press()); assert!(!p.dismiss_stay_press());
        }
    }
}

#[test]
fn a_rejected_quit_chord_shows_a_hint_and_continues() {
    for rebound in [false, true] {
        let mut bindings = KeyBindings::defaults();
        if rebound { bindings.merge_text(&format!("{} = \"F12\"", Action::Quit.spec().name)); }
        let key = if rebound { Key::Named(NamedKey::F12) } else { Key::Character("q".into()) };
        let mods = Modifiers { ctrl: !rebound && !cfg!(target_os = "macos"),
            super_: !rebound && cfg!(target_os = "macos"), alt: false, shift: false };
        let label = bindings.first_label(Action::Quit);
        for pane in [help::Pane::Agent, help::Pane::Terminal, help::Pane::Nav, help::Pane::Preview] {
            for repeat in [false, true] {
                let c = help::Context { pane, ..Default::default() };
                let action = bindings.resolve(&key, Some(&key), mods, c.consumes_text(), |a| c.allows(a));
                let route = quit_chord_route(action, repeat, pane, || bindings.resolve(&key, Some(&key), mods,
                    c.consumes_text(), |a| a == Action::Quit).is_some());
                let mut s = (owner(PaneFocus::Llm), vec![]);
                let result = quit_chord_continuation(&mut s, route, |s| s.1.push("request"), |s| {
                    s.0.show_hint(&label, Instant::now); s.1.push("hint");
                }, |s| { s.1.push("next"); Continue(()) });
                let rejected = matches!(pane, help::Pane::Agent | help::Pane::Terminal);
                assert_eq!(result, if rejected { Continue(()) } else { Break(()) });
                assert_eq!(s.1, match (rejected, repeat) {
                    (true, false) => vec!["hint", "next"], (true, true) => vec!["next"],
                    (false, false) => vec!["request"], (false, true) => vec![],
                });
                let hint = (rejected && !repeat).then(|| global_keys::quit_hint(&label));
                assert_eq!(s.0.hint(), hint.as_deref());
            }
        }
        let mut seen = vec![];
        let _ = quit_chord_continuation(&mut seen, quit_chord_route(None, false, help::Pane::Preview, || true),
            |s| s.push("request"), |s| s.push("hint"), |s| { s.push("next"); Continue(()) });
        assert_eq!(seen, ["next"]);
    }
}

#[test]
fn dialog_priority_preserves_question_leave_stayed_and_hint() {
    let mut p = owner(PaneFocus::Preview);
    p.show_hint("Fixture+Q", Instant::now);
    fn text<'a>(p: &'a FocusPrompts, line: Option<&'a str>, stay_notice: Option<&'a str>) -> Option<CloseDialogText<'a>> {
        close_dialog_text(p.prompt(), Some("standing"), line, p.stayed().then_some(stay_notice).flatten(), p.hint())
    }
    assert_eq!(text(&p, None, None), Some(CloseDialogText::Hint(p.hint().unwrap())));
    p.record_stay(LeaveIntent::Handover);
    assert!(matches!(text(&p, None, None), Some(CloseDialogText::Hint(_))));
    assert_eq!(text(&p, None, Some("stayed")), Some(CloseDialogText::Stayed("stayed")));
    assert_eq!(text(&p, Some("leaving"), Some("stayed")), Some(CloseDialogText::Leave("leaving")));
    p.ask_quit(true);
    let question = text(&p, Some("leaving"), Some("stayed")).unwrap();
    assert_eq!(question, CloseDialogText::Ask { keep: true, notice: Some("standing") });
    let rendered = close_dialog(ratatui::layout::Rect::new(0, 0, 120, 36), question);
    assert!(rendered.answers.is_some());
    assert!(rendered.rows.iter().any(|(_, _, row, _)| row.contains(QUIT_QUESTION)));
    p.cancel(); p.begin_leave();
    assert!(matches!(text(&p, None, Some("stayed")), Some(CloseDialogText::Hint(_))));
    assert!(p.expire_hint(Instant::now() + Duration::from_secs(5)));
    assert_eq!(text(&p, None, Some("stayed")), None);
}
