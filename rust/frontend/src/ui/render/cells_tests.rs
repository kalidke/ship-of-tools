use super::*;
use ratatui::style::Style;

fn backend_with(cells: &[(u16, &str, Style)]) -> WgpuBackend {
    let cols = cells.iter().map(|(c, _, _)| c + 1).max().unwrap_or(1);
    let mut backend = WgpuBackend::new(cols, 1);
    let built: Vec<(u16, Cell)> = cells
        .iter()
        .map(|(col, sym, style)| {
            let mut cell = Cell::default();
            cell.set_symbol(sym);
            cell.set_style(*style);
            (*col, cell)
        })
        .collect();
    backend.draw(built.iter().map(|(col, cell)| (*col, 0, cell))).unwrap();
    backend
}

fn quad(x: f32, w: f32, color: (u8, u8, u8)) -> CellQuad {
    CellQuad { x, y: 0.0, w, h: 1.0, color }
}

#[test]
fn the_surface_colour_in_bytes_is_the_clear_colour() {
    assert_eq!(SURFACE_RGB, (5, 9, 23));
}

#[test]
fn an_inverse_cell_keeps_its_letter_on_a_bar_of_its_foreground() {
    // nano's title bar and Claude Code's cursor: inverse with neither colour set. The letter was drawn as a block in
    // the default foreground, which hid it.
    let backend = backend_with(&[(0, "A", Style::default().add_modifier(Modifier::REVERSED))]);
    let lines = backend.project_lines(0.0, 0.0, 1.0, 1.0);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text, "A");
    assert_eq!(lines[0].color, Some(SURFACE_RGB));
    assert_eq!(backend.project_backs(0.0, 0.0, 1.0, 1.0), vec![quad(0.0, 1.0, DEFAULT_FG)]);
}

#[test]
fn an_inverse_cell_with_both_colours_swaps_them() {
    let style = Style::default().fg(Color::Red).bg(Color::Yellow).add_modifier(Modifier::REVERSED);
    let backend = backend_with(&[(0, "x", style)]);
    assert_eq!(backend.project_lines(0.0, 0.0, 1.0, 1.0)[0].color, Some((229, 229, 16)));
    assert_eq!(backend.project_backs(0.0, 0.0, 1.0, 1.0), vec![quad(0.0, 1.0, (205, 49, 49))]);
}

#[test]
fn a_background_is_one_rect_per_run_of_its_colour() {
    let blue = Style::default().bg(Color::Blue);
    let backend = backend_with(&[
        (0, "a", blue),
        (1, "b", blue),
        (2, " ", Style::default()),
        (3, "c", blue),
        (4, "d", Style::default().bg(Color::Black)),
    ]);
    assert_eq!(
        backend.project_backs(0.0, 0.0, 1.0, 1.0),
        vec![quad(0.0, 2.0, (36, 114, 200)), quad(3.0, 1.0, (36, 114, 200)), quad(4.0, 1.0, (0, 0, 0))]
    );
    // The glyphs keep the default foreground: a background does not recolour or split the text.
    let lines = backend.project_lines(0.0, 0.0, 1.0, 1.0);
    assert_eq!(lines.iter().map(|l| l.text.as_str()).collect::<String>(), "ab cd");
    assert!(lines.iter().all(|l| l.color.is_none()));
}

#[test]
fn an_inverse_box_cell_draws_its_arms_in_its_background() {
    let backend = backend_with(&[(0, "\u{2502}", Style::default().add_modifier(Modifier::REVERSED))]);
    assert!(backend.project_lines(0.0, 0.0, 1.0, 1.0).is_empty());
    let arms = backend.project_border_quads(0.0, 0.0, 1.0, 1.0, 0.2);
    assert_eq!(arms.len(), 1);
    assert_eq!(arms[0].color, SURFACE_RGB);
    assert_eq!(backend.project_backs(0.0, 0.0, 1.0, 1.0), vec![quad(0.0, 1.0, DEFAULT_FG)]);
}

#[test]
fn reversal_exchanges_the_resolved_colours() {
    // Red on black, inverted: black glyph on a red bar, the black the background had (not the glyph promotion).
    let style = Style::default().fg(Color::Red).bg(Color::Black).add_modifier(Modifier::REVERSED);
    let backend = backend_with(&[(0, "x", style)]);
    assert_eq!(backend.project_lines(0.0, 0.0, 1.0, 1.0)[0].color, Some((0, 0, 0)));
    assert_eq!(backend.project_backs(0.0, 0.0, 1.0, 1.0), vec![quad(0.0, 1.0, (205, 49, 49))]);
}

#[test]
fn a_wide_glyph_s_background_covers_its_width() {
    // ratatui's diff sends only the wide glyph's own cell; the one it covers held an earlier frame's letter.
    let mut backend = backend_with(&[(0, " ", Style::default()), (1, "z", Style::default()), (2, " ", Style::default())]);
    let mut wide = Cell::default();
    wide.set_symbol("\u{754c}");
    wide.set_style(Style::default().add_modifier(Modifier::REVERSED));
    backend.draw(std::iter::once((0, 0, &wide))).unwrap();
    assert_eq!(backend.project_backs(0.0, 0.0, 1.0, 1.0), vec![quad(0.0, 2.0, DEFAULT_FG)]);
    let text: String = backend.project_lines(0.0, 0.0, 1.0, 1.0).iter().map(|l| l.text.as_str()).collect();
    assert!(!text.contains('z'), "{text:?}");
}
