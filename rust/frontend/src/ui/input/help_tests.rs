use super::*;
#[test]
fn nav_title_and_default_mode() {
    assert_eq!(Context::default().mode, Mode::Files);
    for (mode, title) in [
        (Mode::Files, "Files"),
        (Mode::Modules, "Modules"),
        (Mode::Sessions, "Sessions"),
        (Mode::Hosts, "Hosts"),
    ] {
        let c = Context { pane: Pane::Nav, mode, ..Context::default() };
        assert_eq!(c.title(), title);
    }
}
#[test]
fn fade_holds_for_five_seconds_and_expires() {
    let start = Instant::now();
    let mut owner = super::super::layout_help::LayoutHelp::new(crate::ui::DrawerContent::Closed, false, false, false);
    owner.start_peek(Context::default(), start);
    let p = owner.peek().unwrap();
    assert_eq!(p.opacity(start + PEEK_HOLD), 1.0);
    assert!((p.opacity(start + PEEK_HOLD + PEEK_FADE / 2) - 0.5).abs() < 0.01);
    assert_eq!(p.opacity(start + PEEK_HOLD + PEEK_FADE), 0.0);
}
#[test]
fn pane_context_controls_actions_and_binding_search() {
    let mut b = KeyBindings::defaults();
    b.merge_text("preview.png.zoom_in = \"Cmd+z\"");
    let c = Context {
        pane: Pane::Preview,
        image: true,
        ..Context::default()
    };
    assert!(c.allows(Action::PreviewPngZoomIn));
    assert!(!c.allows(Action::RunFresh));
    assert!(!c.allows(Action::CopyCode));
    use crate::ui::input::layout_help::{LayoutHelp, HelpKey};
    use crate::ui::{DrawerContent, PaneFocus, Key};
    let mut owner = LayoutHelp::new(DrawerContent::Closed, false, false, false);
    owner.open_help(c.clone(), PaneFocus::Preview);
    owner.help_key(HelpKey { logical_key: &Key::Character("zoom".into()), action: None,
        repeat: false, modifiers: crate::ui::Modifiers { ctrl: false, alt: false, shift: false, super_: false } }, c.clone(), PaneFocus::Repl, &b, Instant::now);
    let h = owner.help();
    assert!(h.actions(&b).contains(&Action::PreviewPngZoomIn));
    assert_eq!(h.context(), &c); // searching doesn't replace the origin context
    assert!(border(&c, &b, 100).contains(&b.first_label(Action::PreviewPngZoomIn)));
}
/// A hidden folder (e.g. a Julia depot) is a legitimate workspace root,
/// so the picker must offer the same hidden toggle Files mode has --
/// and only Files mode among the nav modes.
#[test]
fn hidden_toggle_is_allowed_in_files_nav_and_in_the_picker_only() {
    let files = Context { pane: Pane::Nav, mode: Mode::Files, ..Context::default() };
    assert!(files.allows(Action::ToggleHidden));
    let picker = Context { pane: Pane::Nav, mode: Mode::Sessions, picker: true, ..Context::default() };
    assert!(picker.allows(Action::ToggleHidden));
    let modules = Context { pane: Pane::Nav, mode: Mode::Modules, ..Context::default() };
    assert!(!modules.allows(Action::ToggleHidden));
    let preview = Context { pane: Pane::Preview, ..Context::default() };
    assert!(!preview.allows(Action::ToggleHidden));
}

#[test]
fn input_and_pagination_suppress_unrelated_actions() {
    let c = Context {
        pane: Pane::Preview,
        image: true,
        pages: true,
        ..Context::default()
    };
    assert!(c.allows(Action::PageNext));
    assert!(!c.allows(Action::ScrollPageDown));
    let c = Context {
        pane: Pane::Nav,
        picker: true,
        ..Context::default()
    };
    assert!(c.allows(Action::SessionCreate));
    assert!(!c.allows(Action::Quit));
}
#[test]
fn border_keeps_help_and_never_overflows() {
    let b = KeyBindings::defaults();
    for width in 0..100 {
        let s = border(&Context::default(), &b, width);
        assert!(unicode_width::UnicodeWidthStr::width(s.as_str()) <= width);
    }
    // The label is OS-rendered (glyphs on macOS), so derive the expectation.
    assert_eq!(
        border(&Context::default(), &b, 12),
        format!("{} Help", b.first_label(Action::ToggleHelp))
    );
}
#[test]
fn confirmation_help_exposes_only_the_active_decision() {
    let mut c = Context {
        pane: Pane::Nav,
        prompt: true,
        confirmation: Confirmation::Delete,
        ..Default::default()
    };
    assert!(c.allows(Action::DeleteConfirm));
    assert!(!c.allows(Action::Confirm)); // Enter cancels deletion; it must not say confirm.
    assert!(!c.allows(Action::NavOpen));
    c.pane = Pane::Preview;
    c.editing = true;
    c.confirmation = Confirmation::Discard;
    assert!(c.allows(Action::DiscardConfirm));
    assert!(!c.allows(Action::Cancel)); // Escape now means discard, not cancel.
    assert!(!c.allows(Action::EditSave));
    c.confirmation = Confirmation::Stale;
    assert!(c.allows(Action::StaleReload));
    assert!(c.allows(Action::StaleKeep));
    assert!(!c.allows(Action::EditSave));
    assert!(!c.allows(Action::DiscardConfirm));
}

#[test]
fn annotation_editing_and_terminal_paging_match_the_active_view() {
    let c = Context {
        pane: Pane::Preview,
        editable: true,
        ..Default::default()
    };
    assert!(
        c.allows(Action::EditFile),
        "Annotations need not have a files-mode path"
    );
    for pane in [Pane::Agent, Pane::Terminal] {
        let mut c = Context {
            pane,
            ..Default::default()
        };
        assert!(c.allows(Action::ScrollPageUp));
        c.alternate_screen = true;
        assert!(!c.allows(Action::ScrollPageUp));
    }
}

#[test]
fn cheat_sheet_lists_every_action() {
    let b = KeyBindings::defaults();
    let html = cheat_sheet_html(&b);
    for spec in ACTIONS.iter() {
        assert!(html.contains(spec.name), "missing action {}", spec.name);
    }
}
#[test]
fn html_escape_covers_the_five_special_characters() {
    assert_eq!(
        html_escape("<script>&\"'"),
        "&lt;script&gt;&amp;&quot;&#39;"
    );
    assert_eq!(html_escape("plain text"), "plain text");
}

#[test]
fn the_drawing_keys_resolve_only_where_drawing_is() {
    use winit::keyboard::Key;
    let b = KeyBindings::defaults();
    let chars = |k: &str| Key::Character(k.into());
    let plain = crate::ui::Modifiers { ctrl: false, alt: false, shift: false, super_: false };
    let press = |c: &Context, key: &Key, m: crate::ui::Modifiers| b.resolve(key, Some(key), m, c.consumes_text(), |a| c.allows(a));
    let drawing = Context { pane: Pane::Preview, image: true, drawing: true, ..Context::default() };
    let image = Context { drawing: false, ..drawing.clone() };
    let text = Context { image: false, drawing: false, ..drawing.clone() };
    let editing = Context { editing: true, ..drawing.clone() };
    let nav = Context { pane: Pane::Nav, mode: Mode::Files, file: Some("a.png".into()), ..Context::default() };
    let primary = crate::ui::Modifiers { ctrl: !cfg!(target_os = "macos"), super_: cfg!(target_os = "macos"), ..plain };
    // `d` turns drawing on over an image only; Delete and the undo keys need drawing on as well.
    assert_eq!(press(&image, &chars("d"), plain), Some(Action::PreviewDraw));
    assert_eq!(press(&drawing, &chars("d"), plain), Some(Action::PreviewDraw));
    assert_ne!(press(&text, &chars("d"), plain), Some(Action::PreviewDraw));
    assert_ne!(press(&image, &chars("u"), plain), Some(Action::PreviewInkUndo));
    assert_eq!(press(&drawing, &chars("u"), plain), Some(Action::PreviewInkUndo));
    assert_eq!(press(&drawing, &chars("z"), primary), Some(Action::PreviewInkUndo));
    assert_eq!(press(&drawing, &Key::Named(winit::keyboard::NamedKey::Delete), plain), Some(Action::PreviewInkClear));
    assert_ne!(press(&image, &Key::Named(winit::keyboard::NamedKey::Delete), plain), Some(Action::PreviewInkClear));
    // Editing keeps its own Undo and Delete; the navigation pane keeps upload, download and delete.
    for (key, m) in [("u", plain), ("z", primary)] {
        assert_ne!(press(&editing, &chars(key), m), Some(Action::PreviewInkUndo));
    }
    assert_ne!(press(&editing, &Key::Named(winit::keyboard::NamedKey::Delete), plain), Some(Action::PreviewInkClear));
    assert_eq!(press(&nav, &chars("d"), plain), Some(Action::Download));
    assert_eq!(press(&nav, &chars("u"), plain), Some(Action::Upload));
    assert!(!nav.allows(Action::PreviewInkUndo) && !nav.allows(Action::PreviewInkClear) && !nav.allows(Action::PreviewDraw));
}

#[test]
fn the_canvas_key_is_the_previews_and_the_canvas_takes_the_images_and_the_texts_keys() {
    use winit::keyboard::Key;
    let b = KeyBindings::defaults();
    let shift_d = Key::Character("D".into());
    let shift = crate::ui::Modifiers { ctrl: false, alt: false, shift: true, super_: false };
    let press = |c: &Context| b.resolve(&shift_d, Some(&shift_d), shift, c.consumes_text(), |a| c.allows(a));
    let preview = Context { pane: Pane::Preview, ..Context::default() };
    // over text, over an image and over the canvas alike; not in the editor, nor in the tree (where Shift+d destroys)
    for c in [preview.clone(), Context { image: true, ..preview.clone() }, Context { canvas: true, drawing: true, ..preview.clone() }] {
        assert_eq!(press(&c), Some(Action::PreviewCanvas));
    }
    assert_ne!(press(&Context { editing: true, ..preview.clone() }), Some(Action::PreviewCanvas));
    assert_ne!(press(&Context { pane: Pane::Nav, mode: Mode::Sessions, session: true, ..Context::default() }), Some(Action::PreviewCanvas));
    // the canvas is neither text nor image, and its drawing keys are live
    let canvas = Context { canvas: true, drawing: true, image: true, pages: true, editable: true, ..preview };
    assert!(!canvas.allows(Action::PreviewUp) && !canvas.allows(Action::PreviewPngZoomIn) && canvas.allows(Action::PreviewInkUndo));
    // the page and edit keys do not reach the hidden preview
    assert!(!canvas.allows(Action::PageNext) && !canvas.allows(Action::EditFile));
}
#[test]
fn the_chat_drawer_is_a_tenant_whose_keys_reach_no_repl_handler() {
    use crate::ui::{DrawerContent, PaneFocus};
    use crate::ui::input::global_keys::{line_scroll_target, LineScroll};
    // Every tenant: its pane, and who its keys, paste, wheel and line scroll reach.
    use DrawerContent::*;
    for (drawer, pane, input) in [
        (Closed, Pane::Repl, DrawerInput::Repl), (Repl, Pane::Repl, DrawerInput::Repl),
        (Terminal, Pane::Terminal, DrawerInput::Terminal), (Monitor, Pane::Monitor, DrawerInput::Inert),
        (Help, Pane::Help, DrawerInput::Inert), (Chat, Pane::Chat, DrawerInput::Inert),
    ] {
        assert_eq!(Pane::of_drawer(drawer), pane, "{drawer:?}");
        assert_eq!(DrawerInput::of(drawer), input, "{drawer:?}");
        let want = match (input, drawer) {
            (DrawerInput::Repl, _) => LineScroll::Repl,
            (_, Chat) => LineScroll::Chat,
            _ => LineScroll::Nowhere,
        };
        assert_eq!(line_scroll_target(PaneFocus::Repl, drawer), want, "{drawer:?} line scroll");
        assert_eq!(line_scroll_target(PaneFocus::Preview, drawer), LineScroll::Preview);
        assert_eq!(line_scroll_target(PaneFocus::NavTree, drawer), LineScroll::Nowhere, "{drawer:?} without the focus");
        assert_eq!(line_scroll_target(PaneFocus::Llm, drawer), LineScroll::Nowhere);
    }
    let chat = Context { pane: Pane::Chat, ..Context::default() };
    for a in [Action::ReplSubmit, Action::ReplClear, Action::ReplInterrupt, Action::ReplHistoryPrev, Action::ReplNewline] {
        assert!(!chat.allows(a), "{a:?} reached the REPL from the chat drawer");
    }
    for a in [Action::Paste, Action::ScrollPageUp, Action::ScrollLineUp, Action::ReturnNav, Action::Quit, Action::ToggleChatDrawer] {
        assert!(chat.allows(a), "{a:?} is refused in the chat drawer");
    }
    assert!(chat.consumes_text());
}
