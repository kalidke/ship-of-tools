//! Tests for the key-binding catalog: defaults, chord parsing, merging, literal-text and shifted punctuation.
use super::*;

/// Test helper: the modifiers `Primary` resolves to on this OS (Ctrl on
/// Windows/Linux, Cmd/Super on macOS) -- mirrors `Chord::parse`'s own
/// "primary" branch so tests stay honest about what a respelled default
/// actually requires on each platform.
#[cfg(test)]
fn primary() -> Modifiers {
    Modifiers {
        ctrl: !cfg!(target_os = "macos"),
        super_: cfg!(target_os = "macos"),
        ..Modifiers::default()
    }
}



#[test]
fn defaults_parse_clean() {
    // The defaults() unwraps must never fire — guard here.
    let _ = KeyBindings::defaults();
}

#[test]
fn chord_parses_alt_equals() {
    let c = Chord::parse("Alt+=").unwrap();
    assert!(!c.ctrl);
    assert!(c.alt);
    assert_eq!(c.key, ChordKey::Char("=".into()));
}

#[test]
fn chord_parses_named() {
    let c = Chord::parse("Ctrl+ArrowRight").unwrap();
    assert!(c.ctrl);
    assert_eq!(c.key, ChordKey::Named(NamedKey::ArrowRight));
}

#[test]
fn chord_parses_function_key() {
    let c = Chord::parse("F11").unwrap();
    assert!(!c.ctrl && !c.alt && !c.shift);
    assert_eq!(c.key, ChordKey::Named(NamedKey::F11));
}

/// ADR 0034: the scalebar toggle is Primary+S (Ctrl on Windows/Linux, Cmd
/// on macOS; maintainer, 2026-07-20, respelled 2026-09-18). Pin it
/// against the two neighbours that make it a live collision risk — bare `s`
/// is the Sessions-mode switch and Primary+Shift+S is the selfie — so a
/// future rebind can't silently make one of them fire the scalebar (or
/// vice versa).
#[test]
fn scalebar_toggle_is_ctrl_s_and_does_not_collide() {
    let b = KeyBindings::defaults();
    let s = Key::Character("s".into());

    // Primary+S fires the toggle.
    assert_eq!(
        b.resolve(&s, None, primary(), false, |_| true),
        Some(Action::PreviewScalebarToggle)
    );
    // Bare `s` does NOT (that's Sessions mode).
    assert!(!b.matches(Action::PreviewScalebarToggle, &s, false, false, false));
    // ...and bare `s` still reaches Sessions mode.
    assert!(b.matches(Action::ModeSessions, &s, false, false, false));
    // Primary+S must not fire Sessions mode.
    assert_ne!(b.resolve(&s, None, primary(), false, |_| true), Some(Action::ModeSessions));
    // Primary+Shift+S is the selfie, not the scalebar.
    assert_eq!(
        b.resolve(&s, None, Modifiers { shift: true, ..primary() }, false, |_| true),
        Some(Action::Selfie)
    );
}

#[test]
fn chord_parses_literal_plus_after_modifier() {
    // "Ctrl++" — the trailing literal '+' must survive the '+'-delimited
    // modifier split (the font-zoom-in chord on US layouts).
    let c = Chord::parse("Ctrl++").unwrap();
    assert!(c.ctrl && !c.alt && !c.shift);
    assert_eq!(c.key, ChordKey::Char("+".into()));
    let plus = Key::Character("+".into());
    assert!(c.matches(&plus, true, false, false));
}

#[test]
fn merge_overrides_default() {
    let mut b = KeyBindings::defaults();
    b.merge_text("[keys]\npane.maximize = \"Ctrl+m\"\n");
    let key = Key::Character("m".into());
    assert!(b.matches(Action::MaximizePane, &key, true, false, false));
    // Default Alt+= is replaced, not extended:
    let alt_eq = Key::Character("=".into());
    assert!(!b.matches(Action::MaximizePane, &alt_eq, false, true, false));
}

#[test]
fn merge_supports_list() {
    let mut b = KeyBindings::defaults();
    b.merge_text("pane.maximize = [\"Alt+=\", \"Ctrl+m\"]\n");
    let m = Key::Character("m".into());
    let eq = Key::Character("=".into());
    assert!(b.matches(Action::MaximizePane, &m, true, false, false));
    assert!(b.matches(Action::MaximizePane, &eq, false, true, false));
}

#[test]
fn restore_default_is_escape_not_alt_minus() {
    let b = KeyBindings::defaults();
    let esc = Key::Named(NamedKey::Escape);
    assert!(b.matches(Action::RestoreLayout, &esc, false, false, false));
    // Alt+- is no longer the restore binding.
    let minus = Key::Character("-".into());
    assert!(!b.matches(Action::RestoreLayout, &minus, false, true, false));
}

#[test]
fn unknown_action_ignored() {
    let mut b = KeyBindings::defaults();
    b.merge_text("nonsense.action = \"x\"\n");
    // Defaults still intact.
    let eq = Key::Character("=".into());
    assert!(b.matches(Action::MaximizePane, &eq, false, true, false));
}

#[test]
fn wide_preview_default_is_alt_plus_and_stays_off_maximize() {
    let b = KeyBindings::defaults();
    let plus = Key::Character("+".into());
    let eq = Key::Character("=".into());
    // Alt+Shift+= reports character "+" with shift held on US layouts;
    // the chord doesn't declare shift so both report states match.
    assert!(b.matches(Action::ToggleWidePreview, &plus, false, true, true));
    assert!(b.matches(Action::ToggleWidePreview, &plus, false, true, false));
    // Same keycap, unshifted: that's maximize, not wide-preview…
    assert!(!b.matches(Action::ToggleWidePreview, &eq, false, true, false));
    assert!(b.matches(Action::MaximizePane, &eq, false, true, false));
    // …and the shifted "+" must not fire maximize.
    assert!(!b.matches(Action::MaximizePane, &plus, false, true, true));
    // Ctrl++ is font zoom, not wide-preview.
    assert!(!b.matches(Action::ToggleWidePreview, &plus, true, false, false));
}

#[test]
fn selfie_default_is_ctrl_shift_s() {
    let b = KeyBindings::defaults();
    let s = Key::Character("S".into());
    let shift_primary = Modifiers { shift: true, ..primary() };
    assert_eq!(b.resolve(&s, None, shift_primary, false, |_| true), Some(Action::Selfie));
    // Lowercase (caps-lock / layouts that don't upcase) still matches.
    let lower = Key::Character("s".into());
    assert_eq!(b.resolve(&lower, None, shift_primary, false, |_| true), Some(Action::Selfie));
    // Primary without Shift must NOT trigger it.
    assert_ne!(b.resolve(&s, None, primary(), false, |_| true), Some(Action::Selfie));
}
#[test]
fn help_supports_control_question_mark_and_remapping() {
    let mut b = KeyBindings::defaults();
    let q = Key::Character("?".into());
    // The default differs by OS (finding 4, v0.6.5 macOS field
    // report): everywhere else it's Primary+? i.e. Ctrl+Shift+?; on
    // macOS it's the BARE Cmd+/ instead, because Cmd+Shift+/ is the
    // system's own reserved "Show Help menu" shortcut and never
    // reaches the app at all.
    if cfg!(target_os = "macos") {
        let cmd = Modifiers { super_: true, ..Modifiers::default() };
        assert_eq!(
            b.resolve(&Key::Character("/".into()), None, cmd, false, |_| true),
            Some(Action::ToggleHelp)
        );
        let reserved = Modifiers { super_: true, shift: true, ..Modifiers::default() };
        assert_ne!(
            b.resolve(&q, Some(&Key::Character("/".into())), reserved, false, |_| true),
            Some(Action::ToggleHelp)
        );
    } else {
        let default_mods = Modifiers { shift: true, ..primary() };
        assert_eq!(
            b.resolve(&q, Some(&Key::Character("/".into())), default_mods, false, |_| true),
            Some(Action::ToggleHelp)
        );
    }
    assert_eq!(b.resolve(&q, None, Modifiers::default(), false, |_| true), None);
    b.merge_text("help.toggle = \"Cmd+Shift+/\"");
    let cmd_shift = Modifiers {
        super_: true,
        shift: true,
        ..Modifiers::default()
    };
    assert_eq!(
        b.resolve(&q, Some(&Key::Character("/".into())), cmd_shift, false, |_| true),
        Some(Action::ToggleHelp)
    );
    // An override REPLACES the whole default chord list (finding 5):
    // once help.toggle is remapped, whichever OS-specific default it
    // used to resolve through no longer fires.
    if cfg!(target_os = "macos") {
        let cmd = Modifiers { super_: true, ..Modifiers::default() };
        assert_ne!(
            b.resolve(&Key::Character("/".into()), None, cmd, false, |_| true),
            Some(Action::ToggleHelp)
        );
    } else {
        let default_mods = Modifiers { shift: true, ..primary() };
        assert_ne!(
            b.resolve(&q, None, default_mods, false, |_| true),
            Some(Action::ToggleHelp)
        );
    }
    assert_eq!(b.labels_for(Action::ToggleHelp, true), "⇧⌘/");
}
#[test]
fn control_command_shift_and_text_case_are_distinct() {
    let b = KeyBindings::defaults();
    let c = crate::ui::input::help::Context {
        file: Some("fit.jl".into()),
        ..Default::default()
    };
    assert_eq!(
        b.resolve(
            &Key::Character("r".into()),
            None,
            Modifiers::default(),
            false,
            |a| c.allows(a)
        ),
        Some(Action::RunFresh)
    );
    assert_eq!(
        b.resolve(
            &Key::Character("R".into()),
            None,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            false,
            |a| c.allows(a)
        ),
        Some(Action::RunCurrent)
    );
    let image = crate::ui::input::help::Context {
        pane: crate::ui::input::help::Pane::Preview,
        image: true,
        ..Default::default()
    };
    // Bare Super+S: on macOS that IS Primary+S (the scalebar toggle's
    // own default, respelled 2026-09-18); everywhere else Super+S binds
    // nothing.
    assert_eq!(
        b.resolve(
            &Key::Character("s".into()),
            None,
            Modifiers {
                super_: true,
                ..Modifiers::default()
            },
            false,
            |a| image.allows(a)
        ),
        if cfg!(target_os = "macos") {
            Some(Action::PreviewScalebarToggle)
        } else {
            None
        }
    );
    assert_eq!(
        b.resolve(
            &Key::Character("S".into()),
            None,
            Modifiers { shift: true, ..primary() },
            false,
            |a| image.allows(a)
        ),
        Some(Action::Selfie)
    );
    assert_eq!(
        b.resolve(
            &Key::Named(NamedKey::ArrowUp),
            None,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            false,
            |a| image.allows(a)
        ),
        Some(Action::PreviewPngZoomIn)
    );
}
#[test]
fn remapped_file_actions_and_conflicts_are_honest() {
    let mut b = KeyBindings::defaults();
    // Override with the same "Primary" spelling drawer.monitor's own
    // default now uses, so the collision holds on every OS (Ctrl+m on
    // Windows/Linux, Cmd+m on macOS) instead of only on the ones where
    // Primary still happens to mean Ctrl.
    b.merge_text("files.run_fresh = \"F8\"\npane.maximize = \"Primary+m\"");
    let c = crate::ui::input::help::Context {
        file: Some("fit.jl".into()),
        ..Default::default()
    };
    assert_eq!(
        b.resolve(&Key::Named(NamedKey::F8), None, Modifiers::default(), false, |a| c
            .allows(a)),
        Some(Action::RunFresh)
    );
    assert_eq!(
        b.resolve(
            &Key::Character("r".into()),
            None,
            Modifiers::default(),
            false,
            |a| c.allows(a)
        ),
        None
    );
    assert!(b
        .active_labels(Action::ToggleMonitorDrawer, false, |a| c.allows(a))
        .is_empty());
    assert_eq!(
        b.active_labels(Action::MaximizePane, false, |a| c.allows(a)).len(),
        1
    );
}
#[test]
fn option_letter_uses_layout_base_and_punctuation_keeps_identity() {
    let c = Chord::parse("Option+z").unwrap();
    assert!(c.matches_input(
        &Key::Character("Ω".into()),
        Some(&Key::Character("z".into())),
        Modifiers {
            alt: true,
            ..Modifiers::default()
        }
    ));
    assert!(!Chord::parse("Alt+=").unwrap().matches_input(
        &Key::Character("+".into()),
        Some(&Key::Character("=".into())),
        Modifiers {
            alt: true,
            shift: true,
            ..Modifiers::default()
        }
    ));
    let mut b = KeyBindings::defaults();
    b.merge_text("help.toggle = [\"Ctrl+,\", \"#\"] # comment");
    assert_eq!(b.labels_for(Action::ToggleHelp, false), "Ctrl+, / #");
}

/// macOS Option transforms non-letter keys too (⌥= delivers "≠", base
/// "="), so `pane.maximize` (Alt+=) needs the layout base on that OS.
/// `matches_input_on`'s explicit `mac` argument lets this run on any
/// host: pass `true` for the branch macOS gets, `false` for the branch
/// every other OS gets (they must both hold, since CI only runs Linux).
#[test]
fn option_equals_uses_layout_base_on_macos_only() {
    let c = Chord::parse("Alt+=").unwrap();
    let ne = Key::Character("≠".into());
    let base = Some(Key::Character("=".into()));
    let alt = Modifiers { alt: true, ..Modifiers::default() };
    assert!(c.matches_input_on(&ne, base.as_ref(), alt, true));
    assert!(!c.matches_input_on(&ne, base.as_ref(), alt, false));
    // The existing negative case (Alt+= must not claim Alt+Shift+=)
    // holds either way -- Shift mismatches the chord on both branches.
    let alt_shift = Modifiers { alt: true, shift: true, ..Modifiers::default() };
    assert!(!c.matches_input_on(&ne, base.as_ref(), alt_shift, true));
}

/// ⌥⇧= delivers "±" on a Mac; this already fires on every OS today
/// through the pre-existing Windows shifted-base path (`us_shifted("=")
/// == "+"` against the base), not the new macOS-only branch above --
/// pinned here so that stays true once that branch exists alongside it.
#[test]
fn option_shift_equals_fires_via_shifted_base_on_every_os() {
    let c = Chord::parse("Alt++").unwrap();
    let pm = Key::Character("±".into());
    let base = Some(Key::Character("=".into()));
    let alt_shift = Modifiers { alt: true, shift: true, ..Modifiers::default() };
    assert!(c.matches_input_on(&pm, base.as_ref(), alt_shift, true));
    assert!(c.matches_input_on(&pm, base.as_ref(), alt_shift, false));
}

#[test]
fn primary_uses_frontend_os_and_control_never_becomes_command() {
    let primary = Chord::parse("Primary+p").unwrap();
    assert_eq!(primary.super_, cfg!(target_os = "macos"));
    assert_eq!(primary.ctrl, !cfg!(target_os = "macos"));
    let control = Chord::parse("Control+p").unwrap();
    assert!(control.ctrl && !control.super_);
    assert_eq!(control.label(true), "⌃P");
    let command = Chord::parse("Command+p").unwrap();
    assert!(command.super_ && !command.ctrl);
    assert_eq!(command.label(true), "⌘P");
    let win = Chord::parse("Win+p").unwrap();
    assert_eq!(win, command);
    assert_eq!(
        win.label(false),
        if cfg!(windows) { "Win+P" } else { "Super+P" }
    );
}

#[cfg(test)]
mod literal_text_tests {
    use super::*;
    use winit::keyboard::Key;

    /// Field case (2026-09-06): a machine's keybindings.toml binds bare `?`
    /// to help; after the Help rollout that override must not steal a typed
    /// `?` from a terminal or agent pane, while a chord with a modifier
    /// still fires there.
    #[test]
    fn a_bare_character_override_stays_text_where_the_pane_types() {
        let mut b = KeyBindings::defaults();
        b.merge_text("help.toggle = \"?\"");
        let q = Key::Character("?".into());
        // Navigation focus: the override fires.
        assert_eq!(b.resolve(&q, None, Modifiers::default(), false, |_| true), Some(Action::ToggleHelp));
        // A text-consuming pane: the character is typed, nothing fires.
        assert_eq!(b.resolve(&q, None, Modifiers::default(), true, |_| true), None);
        // ...but a modified chord (Primary+t, the terminal drawer) still does.
        let t = Key::Character("t".into());
        assert_eq!(b.resolve(&t, None, primary(), true, |_| true), Some(Action::ToggleTerminalDrawer));
        // Help advertises accordingly: no label for the bare override in a text pane.
        assert!(b.active_labels(Action::ToggleHelp, true, |_| true).is_empty());
        assert!(!b.active_labels(Action::ToggleHelp, false, |_| true).is_empty());
    }
}

#[cfg(test)]
mod windows_shifted_punctuation_tests {
    use super::*;
    use winit::keyboard::Key;

    /// Field (2026-09-06), verbatim from a Windows frontend log: pressing
    /// Ctrl+Shift+/ delivers `Character("/")` with ctrl held -- never "?".
    /// The shipped default `Ctrl+?` must fire on it, and the unshifted chord
    /// `Ctrl+/` must not (the user typed "?").
    #[test]
    fn windows_delivers_the_unshifted_key_and_ctrl_question_still_fires() {
        let mut b = KeyBindings::defaults();
        // This test documents literal Windows delivery, independent of
        // whatever Primary resolves to on the OS actually running it --
        // bind the literal chord instead of relying on the (now
        // OS-resolved) default.
        b.merge_text("help.toggle = \"Ctrl+?\"");
        let slash = Key::Character("/".into());
        let ctrl_shift = Modifiers { ctrl: true, shift: true, ..Modifiers::default() };
        assert_eq!(
            b.resolve(&slash, Some(&slash), ctrl_shift, false, |_| true),
            Some(Action::ToggleHelp)
        );
        let mut c = KeyBindings::defaults();
        c.merge_text("preview.table_reset = \"Ctrl+/\"");
        // Ctrl+Shift+/ (typed "?") is NOT the unshifted chord...
        assert_ne!(c.resolve(&slash, Some(&slash), ctrl_shift, false, |_| true), Some(Action::TableReset));
        // ...but plain Ctrl+/ is, and it is not Help.
        let ctrl = Modifiers { ctrl: true, ..Modifiers::default() };
        assert_eq!(c.resolve(&slash, Some(&slash), ctrl, false, |_| true), Some(Action::TableReset));
        assert_ne!(b.resolve(&slash, Some(&slash), ctrl, false, |_| true), Some(Action::ToggleHelp));
    }

    /// Linux delivers the shifted symbol itself; unchanged there. macOS's
    /// own default is no longer Primary+? at all (finding 4, v0.6.5 macOS
    /// field report: Cmd+Shift+/ is the system's reserved Help-menu
    /// shortcut), so this only holds off-macOS; macOS gets its own
    /// assertion of the bare Cmd+/ it actually defaults to.
    #[test]
    fn a_directly_delivered_question_mark_still_matches() {
        let b = KeyBindings::defaults();
        if cfg!(target_os = "macos") {
            let slash = Key::Character("/".into());
            let cmd = Modifiers { super_: true, ..Modifiers::default() };
            assert_eq!(b.resolve(&slash, None, cmd, false, |_| true), Some(Action::ToggleHelp));
        } else {
            let q = Key::Character("?".into());
            let m = Modifiers { shift: true, ..primary() };
            assert_eq!(b.resolve(&q, Some(&Key::Character("/".into())), m, false, |_| true), Some(Action::ToggleHelp));
        }
    }
}

#[test]
fn load_layered_reads_the_file_sot_keybindings_names() {
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("SOT_KEYBINDINGS", v),
                None => std::env::remove_var("SOT_KEYBINDINGS"),
            }
        }
    }
    let _restore = Restore(std::env::var_os("SOT_KEYBINDINGS"));
    let dir = std::env::temp_dir().join(format!("sot-keybindings-pin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("keybindings.toml");
    std::fs::write(&file, "[keys]\npane.maximize = \"Ctrl+Alt+j\"\n").unwrap();
    let j = Key::Character("j".into());

    std::env::set_var("SOT_KEYBINDINGS", &file);
    let b = KeyBindings::load_layered();
    assert!(b.matches(Action::MaximizePane, &j, true, true, false));

    // A variable naming a missing file falls through to the later sources.
    std::env::set_var("SOT_KEYBINDINGS", dir.join("missing.toml"));
    let b = KeyBindings::load_layered();
    assert!(!b.matches(Action::MaximizePane, &j, true, true, false));
    let _ = std::fs::remove_dir_all(&dir);
}
