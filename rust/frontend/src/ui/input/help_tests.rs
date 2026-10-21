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
    let p = Peek {
        started: start,
        context: Context::default(),
    };
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
    let mut h = Help::default();
    h.open(c.clone());
    h.query = "zoom".into();
    assert!(h.actions(&b).contains(&Action::PreviewPngZoomIn));
    assert_eq!(h.context, c); // searching doesn't replace the origin context
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
