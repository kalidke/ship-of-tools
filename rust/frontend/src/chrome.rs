// chrome.rs — ratatui chrome rendered into the wgpu surface.
//
// Per ADR 0011: ratatui owns layout, borders, labels, focus, keymaps. This
// module is the glue: a custom `Backend` impl that absorbs ratatui's cell
// stream into an in-memory grid, plus a converter that turns that grid into
// `text::Line`s for the TextLayer.
//
// Limitations of this step (deliberate — keep the spike scope tight):
//   - Foreground color is honoured (per-cell, projected into runs of
//     same-colour cells so each run becomes a coloured TextArea). Background
//     colour and text modifiers (bold/italic/etc.) are still dropped; the
//     grid records them but `project_lines` doesn't emit them yet.
//   - Per-cell glyph width is approximated as a constant; we trust monospace
//     metrics from cosmic-text rather than per-cell measuring.
//   - The cursor position is tracked but not rendered. Cursor draw lands once
//     a quad pipeline exists for the bg layer (next bg-color step).

use std::io;

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::style::{Color, Modifier};

use crate::text::Line;

/// xterm 256-colour palette → RGB. 0..=15 mirror the named ANSI palette
/// (we hand those off to `Color::Gray` etc. upstream, but the table here
/// keeps the function total); 16..=231 is the 6×6×6 RGB cube with the
/// xterm-conventional levels {0, 95, 135, 175, 215, 255}; 232..=255 is a
/// 24-step grayscale ramp from #080808 to #eeeeee. Without this, an
/// `\x1b[38;5;240m` from a CLI's dim placeholder lands as `None` and falls
/// through to the chrome's default white foreground.
fn xterm_indexed_rgb(n: u8) -> (u8, u8, u8) {
    const ANSI_16: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (170, 170, 170),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (229, 229, 229),
    ];
    if n < 16 {
        return ANSI_16[n as usize];
    }
    if n >= 232 {
        let v = (n - 232) as u32 * 10 + 8;
        return (v as u8, v as u8, v as u8);
    }
    const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let i = (n - 16) as u32;
    let r = (i / 36) % 6;
    let g = (i / 6) % 6;
    let b = i % 6;
    (CUBE[r as usize], CUBE[g as usize], CUBE[b as usize])
}

/// Map a ratatui colour to RGB. Returns `None` for `None` / `Reset` (fall
/// through to the TextLayer's default). `Indexed` lands on the xterm
/// 256-colour palette so PTY-blit grays (e.g. `\x1b[38;5;240m`) survive
/// the chrome boundary instead of falling back to default white.
pub(crate) fn ratatui_color_to_rgb(c: Option<Color>) -> Option<(u8, u8, u8)> {
    // ANSI colour values pinned to the VS Code "Dark+" palette so captures
    // are predictable across runs and platforms. The exact tones aren't
    // load-bearing — they just need to be visually distinct.
    match c? {
        Color::Reset => None,
        // Pure (0,0,0) Black goes invisible on the chrome's near-black
        // surface bg — the Claude CLI / tmux status bar / shell prompts
        // routinely emit `\x1b[30m` and the user can't see them. Promote
        // to a "near-black-but-visible" tone; still clearly the darkest
        // foreground but separated from the surface bg by enough delta
        // to read. Same trade as the Color::Gray promotion below.
        Color::Black => Some((50, 50, 50)),
        Color::Red => Some((205, 49, 49)),
        Color::Green => Some((13, 188, 121)),
        Color::Yellow => Some((229, 229, 16)),
        Color::Blue => Some((36, 114, 200)),
        Color::Magenta => Some((188, 63, 188)),
        Color::Cyan => Some((17, 168, 205)),
        // Gray (ANSI 7) is the dim-foreground tone; White (ANSI 15) is the
        // bright foreground. VS Code Dark+ ships them identical, but that
        // collapses Color::Gray and Color::White visually — split them so
        // chrome that explicitly asks for gray gets a visibly dimmer color.
        Color::Gray => Some((170, 170, 170)),
        Color::DarkGray => Some((102, 102, 102)),
        Color::LightRed => Some((241, 76, 76)),
        Color::LightGreen => Some((35, 209, 139)),
        Color::LightYellow => Some((245, 245, 67)),
        Color::LightBlue => Some((59, 142, 234)),
        Color::LightMagenta => Some((214, 112, 214)),
        Color::LightCyan => Some((41, 184, 219)),
        Color::White => Some((229, 229, 229)),
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(n) => Some(xterm_indexed_rgb(n)),
    }
}

/// The four possible arms of a box-drawing glyph, measured *from the cell
/// centre*. A glyph lights the arms toward the edges it connects to: `│`
/// has up+down, `┌` has down+right, `┼` all four, and so on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Arms {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
}

/// A solid-colour rectangle (physical px, origin top-left) emitted for a
/// single box-drawing cell. Rendered as a quad sized to the exact cell so
/// stacked borders tile seamlessly — font metrics never enter the picture,
/// which is what fixes the sub-cell gaps cosmic-text leaves between stacked
/// `│` glyphs (the resolved monospace font doesn't fill the leading-padded
/// cell). See `project_border_quads`.
#[derive(Clone, Copy, Debug)]
pub struct BorderQuad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: (u8, u8, u8),
}

/// Map a box-drawing `char` to its lit arms, or `None` for any char not in
/// the covered set. `None` chars keep rendering as font glyphs, so there's
/// no regression for emoji / other non-ASCII. Covers the light single-line
/// set ratatui uses for pane borders plus the four rounded corners.
pub fn box_arms(ch: char) -> Option<Arms> {
    // (up, down, left, right)
    let a = |up, down, left, right| Some(Arms { up, down, left, right });
    match ch {
        '\u{2500}' => a(false, false, true, true), // ─
        '\u{2502}' => a(true, true, false, false), // │
        '\u{250C}' => a(false, true, false, true), // ┌
        '\u{2510}' => a(false, true, true, false), // ┐
        '\u{2514}' => a(true, false, false, true), // └
        '\u{2518}' => a(true, false, true, false), // ┘
        '\u{251C}' => a(true, true, false, true),  // ├
        '\u{2524}' => a(true, true, true, false),  // ┤
        '\u{252C}' => a(false, true, true, true),  // ┬
        '\u{2534}' => a(true, false, true, true),  // ┴
        '\u{253C}' => a(true, true, true, true),   // ┼
        '\u{256D}' => a(false, true, false, true), // ╭ (rounded ┌)
        '\u{256E}' => a(false, true, true, false), // ╮ (rounded ┐)
        '\u{2570}' => a(true, false, false, true), // ╰ (rounded └)
        '\u{256F}' => a(true, false, true, false), // ╯ (rounded ┘)
        _ => None,
    }
}

/// Arms for a cell *symbol* — `Some` only when the symbol is exactly one
/// covered box-drawing scalar (a bare glyph, not a multi-char grapheme).
/// Shared by `project_lines` (to skip glyph emission) and
/// `project_border_quads` (to emit the quad) so both agree on which cells
/// are borders.
fn box_arms_of_symbol(sym: &str) -> Option<Arms> {
    let mut it = sym.chars();
    let c = it.next()?;
    if it.next().is_some() {
        return None; // multi-scalar grapheme — not a bare box glyph
    }
    box_arms(c)
}

pub struct WgpuBackend {
    cols: u16,
    rows: u16,
    /// Row-major cell grid. Length is exactly cols * rows.
    cells: Vec<Cell>,
    cursor: Position,
    cursor_visible: bool,
}

impl WgpuBackend {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols.max(1),
            rows: rows.max(1),
            cells: vec![Cell::default(); (cols.max(1) as usize) * (rows.max(1) as usize)],
            cursor: Position { x: 0, y: 0 },
            cursor_visible: true,
        }
    }

    /// Reset cell storage to a new (cols, rows). Called when the window resizes.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols.max(1);
        self.rows = rows.max(1);
        self.cells.clear();
        self.cells.resize(
            (self.cols as usize) * (self.rows as usize),
            Cell::default(),
        );
    }

    fn idx(&self, col: u16, row: u16) -> Option<usize> {
        if col < self.cols && row < self.rows {
            Some(row as usize * self.cols as usize + col as usize)
        } else {
            None
        }
    }

    /// Project the current cell state onto `Line`s, one *run* of
    /// same-foreground-colour cells per `Line`. Each Line is positioned at
    /// `(origin_x + col * cell_w, origin_y + row * cell_h)` for its starting
    /// column, so different-colour spans on the same row stack side-by-side
    /// at the right pixel offsets.
    ///
    /// Returns more entries than rows when chrome uses multiple colours per
    /// line (typical: a status line has "status: " in dim grey and the value
    /// in green). Glyphon batches the resulting TextAreas; the extra count
    /// is cheap.
    pub fn project_lines(
        &self,
        origin_x: f32,
        origin_y: f32,
        cell_w: f32,
        cell_h: f32,
    ) -> Vec<Line> {
        let mut out = Vec::with_capacity(self.rows as usize);
        for row in 0..self.rows {
            let row_start = row as usize * self.cols as usize;
            let y = origin_y + row as f32 * cell_h;
            // Walk the row, emitting one Line per run of cells that share
            // (fg, bold, italic) AND are all plain ASCII. Any cell whose
            // symbol contains non-ASCII bytes (box-drawing glyphs,
            // emoji, anything beyond U+007F) ends the current run and
            // is emitted as its own single-cell Line. That guarantees
            // pixel-perfect positioning for box-drawing chars whose
            // font advance doesn't equal cell_w — cosmic-text lays out
            // a run proportionally by font advance, so a long mixed
            // run drifts sub-pixel offsets that break wireframe
            // junctions visually. ASCII runs still batch so the
            // TextArea count stays modest.
            let mut col: u16 = 0;
            while col < self.cols {
                let i = row_start + col as usize;
                let sym = self.cells[i].symbol();
                let run_style = run_signature(&self.cells[i]);
                let (fg, bold, italic, dim, reversed) = run_style;
                // REVERSED cells get a `█` full-block in the cell's fg
                // colour — the chrome's TextLayer doesn't honour the
                // REVERSED modifier (no bg-colour quad pipeline yet),
                // so we substitute a foreground-colour glyph that's
                // visible at the cell position. Per-cell so multi-cell
                // REVERSED runs (tmux status bar) stay aligned with
                // cell_w even if the bare block has slight advance
                // mismatch. The underlying glyph is lost; for the
                // cursor cell that's expected (typical xterm/alacritty
                // block-cursor behaviour).
                if reversed {
                    out.push(Line {
                        text: "\u{2588}".to_string(),
                        x: origin_x + col as f32 * cell_w,
                        y,
                        color: ratatui_color_to_rgb(fg),
                        bold,
                        italic,
                        dim,
                    });
                    col += 1;
                    continue;
                }
                // Box-drawing chars (│ ─ ┌ …) are rendered as solid quads by
                // `project_border_quads` — font-independent and gap-free by
                // construction — so skip emitting a glyph Line for them here
                // to avoid double-rendering. Non-box non-ASCII (emoji, other
                // symbols) still falls through to the glyph path below.
                // Reversed box cells were already handled as a block above,
                // so they don't reach here.
                if box_arms_of_symbol(sym).is_some() {
                    col += 1;
                    continue;
                }
                if !sym.is_ascii() {
                    out.push(Line {
                        text: sym.to_string(),
                        x: origin_x + col as f32 * cell_w,
                        y,
                        color: ratatui_color_to_rgb(fg),
                        bold,
                        italic,
                        dim,
                    });
                    col += 1;
                    continue;
                }
                let run_start_col = col;
                let mut text = String::new();
                while col < self.cols {
                    let j = row_start + col as usize;
                    let s = self.cells[j].symbol();
                    if !s.is_ascii() || run_signature(&self.cells[j]) != run_style {
                        break;
                    }
                    text.push_str(s);
                    col += 1;
                }
                out.push(Line {
                    text,
                    x: origin_x + run_start_col as f32 * cell_w,
                    y,
                    color: ratatui_color_to_rgb(fg),
                    bold,
                    italic,
                    dim,
                });
            }
        }
        out
    }

    /// Project every box-drawing cell onto solid-colour `BorderQuad`s using
    /// the ARMS-FROM-CENTRE model: each lit arm spans from the cell centre
    /// out to the cell edge. A run of `│` cells therefore produces vertical
    /// rects that meet exactly on the shared cell boundary (no font-leading
    /// gap), and a junction like `├` overlaps its neighbours' arms so corners
    /// connect. `thickness` is the bar width in physical px.
    ///
    /// The origin / cell size MUST match the `project_lines` call for the same
    /// frame so the quads land on the glyph grid exactly. Cells whose symbol
    /// isn't a covered box char are skipped (they still render as glyphs via
    /// `project_lines`); REVERSED cells are skipped too, since `project_lines`
    /// already substitutes a full block for them.
    pub fn project_border_quads(
        &self,
        origin_x: f32,
        origin_y: f32,
        cell_w: f32,
        cell_h: f32,
        thickness: f32,
    ) -> Vec<BorderQuad> {
        let t = thickness.max(1.0);
        let mut out = Vec::new();
        for row in 0..self.rows {
            let row_start = row as usize * self.cols as usize;
            for col in 0..self.cols {
                let cell = &self.cells[row_start + col as usize];
                // REVERSED box cells render as a full block via project_lines;
                // leave them to that path so a box char under the cursor /
                // selection stays a block, not a hairline.
                if cell.style().add_modifier.contains(Modifier::REVERSED) {
                    continue;
                }
                let Some(arms) = box_arms_of_symbol(cell.symbol()) else {
                    continue;
                };
                let x0 = origin_x + col as f32 * cell_w;
                let y0 = origin_y + row as f32 * cell_h;
                let cx = x0 + cell_w * 0.5;
                let cy = y0 + cell_h * 0.5;
                // Default fg (204,204,204) matches project_lines'/text layer's
                // fallback when the cell carries no explicit colour.
                let color = ratatui_color_to_rgb(cell.style().fg).unwrap_or((204, 204, 204));
                // Vertical bar (up and/or down arm).
                if arms.up || arms.down {
                    let top = if arms.up { y0 } else { cy - t * 0.5 };
                    let bottom = if arms.down { y0 + cell_h } else { cy + t * 0.5 };
                    out.push(BorderQuad {
                        x: cx - t * 0.5,
                        y: top,
                        w: t,
                        h: bottom - top,
                        color,
                    });
                }
                // Horizontal bar (left and/or right arm).
                if arms.left || arms.right {
                    let left = if arms.left { x0 } else { cx - t * 0.5 };
                    let right = if arms.right { x0 + cell_w } else { cx + t * 0.5 };
                    out.push(BorderQuad {
                        x: left,
                        y: cy - t * 0.5,
                        w: right - left,
                        h: t,
                        color,
                    });
                }
            }
        }
        out
    }
}

/// `(fg, bold, italic, dim, reversed)` tuple — what defines a `Line` run
/// boundary. We don't include underline/strikethrough yet because
/// cosmic-text's Attrs doesn't expose them; add when needed. REVERSED
/// joins the tuple so the cursor cell (paint_terminal flips REVERSED
/// on for the vt100 cursor position) gets its own Line and can be
/// rendered as a visible block glyph in project_lines.
fn run_signature(cell: &Cell) -> (Option<Color>, bool, bool, bool, bool) {
    let style = cell.style();
    let mods = style.add_modifier;
    (
        style.fg,
        mods.contains(Modifier::BOLD),
        mods.contains(Modifier::ITALIC),
        mods.contains(Modifier::DIM),
        mods.contains(Modifier::REVERSED),
    )
}

impl Backend for WgpuBackend {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        for (col, row, cell) in content {
            if let Some(i) = self.idx(col, row) {
                self.cells[i] = cell.clone();
            }
        }
        Ok(())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.cursor_visible = false;
        Ok(())
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.cursor_visible = true;
        Ok(())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(self.cursor)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.cursor = position.into();
        Ok(())
    }

    fn clear(&mut self) -> io::Result<()> {
        for c in self.cells.iter_mut() {
            *c = Cell::default();
        }
        Ok(())
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        match clear_type {
            ClearType::All => self.clear(),
            ClearType::AfterCursor => {
                if let Some(start) = self.idx(self.cursor.x, self.cursor.y) {
                    for c in self.cells[start..].iter_mut() {
                        *c = Cell::default();
                    }
                }
                Ok(())
            }
            ClearType::BeforeCursor => {
                if let Some(end) = self.idx(self.cursor.x, self.cursor.y) {
                    for c in self.cells[..=end].iter_mut() {
                        *c = Cell::default();
                    }
                }
                Ok(())
            }
            ClearType::CurrentLine => {
                let row = self.cursor.y;
                if row < self.rows {
                    let start = row as usize * self.cols as usize;
                    let end = start + self.cols as usize;
                    for c in self.cells[start..end].iter_mut() {
                        *c = Cell::default();
                    }
                }
                Ok(())
            }
            ClearType::UntilNewLine => {
                if let Some(start) = self.idx(self.cursor.x, self.cursor.y) {
                    let row = self.cursor.y;
                    let end = (row as usize + 1) * self.cols as usize;
                    for c in self.cells[start..end].iter_mut() {
                        *c = Cell::default();
                    }
                }
                Ok(())
            }
        }
    }

    fn size(&self) -> io::Result<Size> {
        Ok(Size::new(self.cols, self.rows))
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        Ok(WindowSize {
            columns_rows: Size::new(self.cols, self.rows),
            pixels: Size::new(0, 0),
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
