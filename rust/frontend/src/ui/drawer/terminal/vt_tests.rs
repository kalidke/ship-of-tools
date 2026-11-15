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
    let mut parser = vt100::Parser::new(1, cols, 0);
    parser.process(bytes);
    painted_screen(parser.screen())
}

/// The cells of `screen`, as the window's cell backend holds them.
fn painted_screen(screen: &vt100::Screen) -> crate::ui::render::cells::WgpuBackend {
    use ratatui::backend::Backend;
    let (rows, cols) = screen.size();
    let area = ratatui::layout::Rect::new(0, 0, cols, rows);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    paint_terminal(&mut buf, area, screen);
    let mut backend = crate::ui::render::cells::WgpuBackend::new(cols, rows);
    backend.draw((0..rows).flat_map(|y| (0..cols).map(move |x| (x, y))).map(|(x, y)| (x, y, &buf[(x, y)]))).unwrap();
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
    assert_eq!(bar, " nano", "the bar's last cell is a space, which draws nothing but its rectangle");
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

/// A screen moved by one row of the ring shapes only the runs of the row that came into view: the rows that stayed
/// take their buffers wherever they now are, and the blank cells at the ends of the rows are no runs.
#[test]
fn a_screen_one_row_further_up_the_ring_shapes_only_the_row_that_came_into_view() {
    use crate::ui::render::text::ShapedRuns;
    let mut parser = vt100::Parser::new(6, 40, 30);
    // No cursor: it is an inverse cell at one place on the screen, whatever row is under it.
    parser.process(b"\x1b[?25l");
    for i in 0..20 {
        // A colored lead, a plain body and, on every third row, nothing at all.
        let body = if i % 3 == 2 { String::new() } else { format!("\x1b[3{}mrow {i:02}\x1b[0m body of row {i}", 1 + i % 6) };
        parser.process(format!("{body}\r\n").as_bytes());
    }
    let mut fonts = cosmic_text::FontSystem::new();
    let metrics = cosmic_text::Metrics::new(14.0, 18.0);
    let mut runs = ShapedRuns::default();
    let mut frame = |offset: usize, parser: &mut vt100::Parser| {
        parser.screen_mut().set_scrollback(offset);
        let lines = painted_screen(parser.screen()).project_lines(0.0, 0.0, 1.0, 1.0);
        assert!(lines.iter().all(|l| !l.text.trim().is_empty() && !l.text.ends_with(' ')), "{:?}", lines.iter().map(|l| &l.text).collect::<Vec<_>>());
        (lines.len(), runs.shape(&mut fonts, metrics, 800, 600, &lines))
    };
    let (held, first) = frame(3, &mut parser);
    assert_eq!(first, held, "the first frame shapes every run");
    for offset in 4..9 {
        let (held, shaped) = frame(offset, &mut parser);
        let came = frame(offset, &mut parser);
        assert_eq!(came.1, 0, "the same screen again shapes nothing");
        // The row that came into view is the screen's first; the row that left shaped nothing new either way.
        parser.screen_mut().set_scrollback(offset);
        let top: Vec<String> = painted_screen(parser.screen()).project_lines(0.0, 0.0, 1.0, 1.0).into_iter().filter(|l| l.y == 0.0).map(|l| l.text).collect();
        assert_eq!(shaped, top.len(), "offset {offset}: {held} runs held, shaped {shaped}, the top row's runs {top:?}");
    }
}
