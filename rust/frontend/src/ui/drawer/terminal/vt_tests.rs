use super::*;

#[test]
fn shift_tab_encodes_backtab_plain_tab_unchanged() {
    // Shift+Tab must reach the pty as BackTab (CSI Z) so a TUI in the pty —
    // Claude Code's plan-mode cycle above all — sees the chord. Regression
    // guard: before the `shift` arg this collapsed to a plain `\t`.
    assert_eq!(
        key_to_pty_bytes(&Key::Named(NamedKey::Tab), false, true, false),
        Some(b"\x1b[Z".to_vec())
    );
    // Plain Tab stays a literal tab for shell/editor completion.
    assert_eq!(
        key_to_pty_bytes(&Key::Named(NamedKey::Tab), false, false, false),
        Some(b"\t".to_vec())
    );
    // Ctrl+Tab (no shift) has no distinct pty encoding here — still `\t`.
    assert_eq!(
        key_to_pty_bytes(&Key::Named(NamedKey::Tab), true, false, false),
        Some(b"\t".to_vec())
    );
}

#[test]
fn command_chord_never_reaches_the_pty_as_text() {
    // macOS bug this guards: Cmd+C fell through and typed a literal "c"
    // into the shell/LLM pane, because the pty encoder had no idea
    // Command was held. Ctrl+C is unaffected -- still the interrupt byte.
    // macOS only: elsewhere the OS or window manager owns Super chords,
    // and whatever it lets through keeps typing exactly as before.
    assert_eq!(
        key_to_pty_bytes(&Key::Character("c".into()), false, false, true),
        if cfg!(target_os = "macos") { None } else { Some(b"c".to_vec()) }
    );
    assert_eq!(
        key_to_pty_bytes(&Key::Character("c".into()), true, false, false),
        Some(vec![0x03])
    );
}
