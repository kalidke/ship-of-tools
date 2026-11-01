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

/// Paint `bytes` on a 1-row terminal of `cols` and hand the cells to the window's cell backend.
fn painted(cols: u16, bytes: &[u8]) -> crate::ui::render::cells::WgpuBackend {
    use ratatui::backend::Backend;
    let mut parser = vt100::Parser::new(1, cols, 0);
    parser.process(bytes);
    let area = ratatui::layout::Rect::new(0, 0, cols, 1);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    paint_terminal(&mut buf, area, parser.screen());
    let mut backend = crate::ui::render::cells::WgpuBackend::new(cols, 1);
    backend.draw((0..cols).map(|x| (x, 0, &buf[(x, 0)]))).unwrap();
    backend
}

#[test]
fn an_inverse_bar_keeps_its_letters() {
    // nano's title bar, and the highlight Claude Code draws: inverse video with default colours. The window drew
    // each inverse cell as a block of the default foreground, so the bar read as solid white with no text.
    use crate::ui::render::cells::{DEFAULT_FG, SURFACE_RGB};
    let backend = painted(8, b"\x1b[?25l\x1b[7m nano \x1b[0m");
    let lines = backend.project_lines(0.0, 0.0, 1.0, 1.0);
    let bar: String = lines.iter().filter(|l| l.color == Some(SURFACE_RGB)).map(|l| l.text.as_str()).collect();
    assert_eq!(bar, " nano ");
    let backs = backend.project_backs(0.0, 0.0, 1.0, 1.0);
    assert_eq!(backs.len(), 1);
    assert_eq!((backs[0].x, backs[0].w, backs[0].color), (0.0, 6.0, DEFAULT_FG));
}

#[test]
fn a_cell_background_reaches_the_backend_and_the_cursor_keeps_its_letter() {
    use crate::ui::render::cells::DEFAULT_FG;
    // Red background on "ab", then the cursor moved back onto "b".
    let backend = painted(4, b"\x1b[41mab\x1b[0m\x1b[D");
    let backs = backend.project_backs(0.0, 0.0, 1.0, 1.0);
    assert_eq!((backs[0].x, backs[0].w, backs[0].color), (0.0, 1.0, (205, 49, 49)));
    // The block cursor is an inverse cell: its own bar, with "b" drawn in the cell's background.
    assert_eq!((backs[1].x, backs[1].w, backs[1].color), (1.0, 1.0, DEFAULT_FG));
    let lines = backend.project_lines(0.0, 0.0, 1.0, 1.0);
    assert!(lines.iter().any(|l| l.text.starts_with('b') && l.color == Some((205, 49, 49))), "{:?}",
        lines.iter().map(|l| (&l.text, l.color)).collect::<Vec<_>>());
}
