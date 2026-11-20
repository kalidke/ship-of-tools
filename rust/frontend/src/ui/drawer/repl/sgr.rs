//! A program's text as styled rows: the SGR escapes a program prints (`ESC [ ... m`) become cell styles, through the color
//! map the Terminal drawer and the agent pane use. Every other escape sequence (`sot_protocol::escapes`) is dropped, and
//! a control character, a lone ESC among them, is a space.

use super::wrap::Run;
use crate::ui::drawer::terminal::vt::vt100_color_to_ratatui;
use crate::ui::*;
use ratatui::style::Modifier;
use sot_protocol::escapes::{open_tail, pieces, Piece};

/// What the SGR escapes read so far have set, over the stream's own base style: a reset or a default color returns to
/// the base, so stderr's gray stays under a program's `ESC [ 39 m`.
#[derive(Clone, Copy, Default, PartialEq)]
pub(super) struct Sgr {
    fg: Option<Color>,
    bg: Option<Color>,
    mods: Modifier,
}

/// A color of the 16 named ones (0 to 15) or of the 256-color palette, as the Terminal drawer maps it.
fn indexed(n: u16) -> Option<Color> {
    Some(vt100_color_to_ratatui(vt100::Color::Idx(u8::try_from(n).ok()?)))
}

impl Sgr {
    /// One SGR parameter list. The 16 colors (30-37, 40-47, 90-97, 100-107), the 256-color and RGB forms of 38 and 48 (in
    /// the `;` form and the ITU `:` form, whose empty color-space field is skipped), bold, dim, reverse, their resets and
    /// the default colors are read; underline, italic, the underline color (58, whose color is read and dropped) and
    /// the rest are ignored (a cell draws none of them).
    fn apply(&mut self, params: &str) {
        let mut groups = params.split(';');
        while let Some(group) = groups.next() {
            if group.contains(':') {
                let sub: Vec<u16> = group.split(':').map(|p| p.parse().unwrap_or(0)).collect();
                match sub[0] {
                    which @ (38 | 48 | 58) => {
                        let color = match sub.get(1) {
                            Some(5) => sub.get(2).and_then(|&n| indexed(n)),
                            Some(2) if sub.len() >= 5 => {
                                let rgb = &sub[sub.len() - 3..];
                                Some(Color::Rgb(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8))
                            }
                            _ => None,
                        };
                        self.set_color(which, color);
                    }
                    n => self.code(n),
                }
                continue;
            }
            let n = group.parse().unwrap_or(0);
            if matches!(n, 38 | 48 | 58) {
                let mut next = || groups.next().map(|g| g.parse::<u16>().unwrap_or(0));
                let color = match next() {
                    Some(5) => next().and_then(indexed),
                    Some(2) => match (next(), next(), next()) {
                        (Some(r), Some(g), Some(b)) => Some(Color::Rgb(r as u8, g as u8, b as u8)),
                        _ => None,
                    },
                    _ => None,
                };
                self.set_color(n, color);
            } else {
                self.code(n);
            }
        }
    }

    fn set_color(&mut self, which: u16, color: Option<Color>) {
        match which {
            38 => self.fg = color,
            48 => self.bg = color,
            _ => {} // 58: the underline's color, which no cell draws
        }
    }

    fn code(&mut self, n: u16) {
        match n {
            0 => *self = Sgr::default(),
            1 => self.mods.insert(Modifier::BOLD),
            2 => self.mods.insert(Modifier::DIM),
            7 => self.mods.insert(Modifier::REVERSED),
            22 => self.mods.remove(Modifier::BOLD | Modifier::DIM),
            27 => self.mods.remove(Modifier::REVERSED),
            30..=37 => self.fg = indexed(n - 30),
            90..=97 => self.fg = indexed(n - 90 + 8),
            39 => self.fg = None,
            40..=47 => self.bg = indexed(n - 40),
            100..=107 => self.bg = indexed(n - 100 + 8),
            49 => self.bg = None,
            _ => {}
        }
    }

    fn style(&self, base: Style) -> Style {
        let styled = Style { fg: self.fg.or(base.fg), bg: self.bg.or(base.bg), ..base };
        styled.add_modifier(self.mods)
    }
}

/// What the reader does with the text it reads: every function it honors. The reader dispatches on this alone, and the tests
/// drive each one (`sgr_tests.rs`, `Func::sample`), so a function cannot be added without a row in the budget they hold every
/// function to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Func {
    /// Printable text, written at the cursor.
    Text,
    /// A control character that is no line end (a tab): a space.
    Control,
    /// A line end (`\n`, U+0085, U+2028, U+2029).
    Newline,
    /// A `\r`: the cursor goes to the start of the line.
    Return,
    /// Cursor to column 1 (`CSI G`, `CSI 1G`), which a repaint by column sends in place of a `\r`.
    Column,
    /// Select graphic rendition: colors and weights (`CSI m`).
    Sgr,
    /// Erase in line from the cursor on (`CSI K`, `CSI 0K`).
    EraseEnd,
    /// Erase in line from the start through the cursor (`CSI 1K`).
    EraseStart,
    /// Erase the whole line, the cursor kept (`CSI 2K`).
    EraseAll,
}

impl Func {
    /// The function a CSI sequence is, if the reader honors it.
    fn of_csi(params: &str, final_byte: char) -> Option<Func> {
        Some(match (final_byte, params) {
            ('m', _) => Func::Sgr,
            ('K', "" | "0") => Func::EraseEnd,
            ('K', "1") => Func::EraseStart,
            ('K', "2") => Func::EraseAll,
            ('G', "" | "0" | "1") => Func::Column,
            _ => return None,
        })
    }

    /// The function a control character is.
    fn of_control(c: char) -> Func {
        match c {
            '\r' => Func::Return,
            '\n' | '\u{85}' | '\u{2028}' | '\u{2029}' => Func::Newline,
            _ => Func::Control,
        }
    }
}

/// The most one line takes in before the reader ends it, as if the program had, counted in characters (cells), the unit a cursor
/// column is in, whatever the way the row was written. A line past it goes on in a row of its own, so cutting the line a program
/// has open costs a bounded row, whatever the program writes into it.
const LINE_CAP: usize = 64 * 1024;

/// The row being written, with one rule for every operation on it: an operation costs what its write was (the bytes of the
/// text, the cells it names), amortized, and never the row so far. Styled runs while text only goes on at its end; one cell
/// per character once a `\r` or an erase has made the cursor a place in the row, so a write over what is there costs what is
/// written. An erase does not blank cells that are already blank: `blank_to` is how far the row's start is known to be
/// blank, a write below it lowers it, and an erase spends only the cells between it and the cursor (the cursor reaches a
/// column only by writing the cells before it, which paid for them). A cleared cell is a space in the reader's base style.
#[derive(Clone, Default)]
struct Row {
    runs: Vec<Run>,
    cells: Option<Vec<(char, Style)>>,
    col: usize,
    /// The cells below this index are spaces in the base style.
    blank_to: usize,
    /// The characters in `runs`.
    chars: usize,
    /// Something was written, or a `\r` came: the row exists, as a line of its own, even when it is empty.
    touched: bool,
}

impl Row {
    /// Writes `text` in `style` at the cursor: appended to the last run, or over the cells from the cursor on.
    fn write(&mut self, text: &str, style: Style) {
        self.touched = true;
        count(text.len());
        match &mut self.cells {
            None => {
                self.chars += text.chars().count();
                match self.runs.last_mut() {
                    _ if text.is_empty() => {}
                    Some((s, st)) if *st == style => s.push_str(text),
                    _ => self.runs.push((text.to_string(), style)),
                }
            }
            Some(cells) => {
                self.blank_to = self.blank_to.min(self.col);
                for c in text.chars() {
                    match cells.get_mut(self.col) {
                        Some(cell) => *cell = (c, style),
                        None => cells.push((c, style)),
                    }
                    self.col += 1;
                }
            }
        }
    }

    /// The cursor's column, in characters.
    fn cursor(&self) -> usize {
        if self.cells.is_none() { self.chars } else { self.col }
    }

    /// The cursor goes to the start, and from now on the row is a vector of cells.
    fn home(&mut self) {
        self.touched = true;
        self.to_cells();
        self.col = 0;
    }

    /// The row as a vector of cells, the cursor where text ended (a row already of cells keeps its cursor). A row is made
    /// into cells once, and what it costs is the text already written to it.
    fn to_cells(&mut self) {
        if self.cells.is_none() {
            let cells: Vec<(char, Style)> = self.runs.drain(..).flat_map(|(t, st)| t.chars().map(move |c| (c, st)).collect::<Vec<_>>()).collect();
            count(cells.len());
            self.col = cells.len();
            self.blank_to = 0;
            self.cells = Some(cells);
        }
    }

    /// Erase in line: `EraseEnd` removes the cells from the cursor on (the window has no row width to paint to), `EraseStart`
    /// blanks the row's start through the cursor, `EraseAll` blanks it up to the cursor and removes the rest, the cursor kept.
    /// Cleared cells are spaces in `blank`. An erase on a row nothing was written to makes no row.
    fn erase(&mut self, f: Func, blank: Style) {
        if !self.touched || (self.cells.is_none() && f == Func::EraseEnd) {
            return; // nothing there, or the cursor at the end of the text: nothing to the right of it
        }
        self.to_cells();
        let (col, cells) = (self.col, self.cells.as_mut().unwrap());
        let upto = match f {
            Func::EraseEnd => {
                cells.truncate(col);
                self.blank_to = self.blank_to.min(col);
                return;
            }
            Func::EraseStart => (col + 1).min(cells.len()),
            _ => {
                cells.truncate(col);
                col
            }
        };
        let from = self.blank_to.min(upto);
        cells[from..upto].fill((' ', blank));
        count(upto - from);
        self.blank_to = self.blank_to.max(upto).min(cells.len());
    }

    /// The row's styled runs, adjacent cells of one style joined.
    fn runs(&self, base: Style) -> Vec<Run> {
        let mut out: Vec<Run> = match &self.cells {
            None => self.runs.clone(),
            Some(cells) => {
                count(cells.len());
                let mut runs: Vec<Run> = Vec::new();
                for &(c, st) in cells {
                    match runs.last_mut() {
                        Some((s, last)) if *last == st => s.push(c),
                        _ => runs.push((c.to_string(), st)),
                    }
                }
                runs
            }
        };
        if out.is_empty() {
            out.push((String::new(), base));
        }
        out
    }
}

/// Counts a cell written, for the tests that hold a row to a budget of work; the product keeps no count.
#[cfg_attr(not(test), allow(unused_variables))]
fn count(n: usize) {
    #[cfg(test)]
    WRITTEN.with(|w| w.set(w.get() + n));
}

#[cfg(test)]
thread_local! {
    /// The cells written on this thread since a test last set it to zero.
    pub(super) static WRITTEN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A program's text read as a terminal reads it, line by line, a write at a time: SGR escapes color, every other sequence
/// is dropped, a `\n` (or U+0085, U+2028, U+2029) ends a line, a control character is a space, and with `overwrite` a lone
/// `\r` returns to the start of the line and what follows writes over it (without, it ends a line). What it keeps is what
/// it read: the lines it ended, the line it is in, and the SGR state, so the next write costs what it adds. A write that
/// ends in the middle of an escape sequence is held and read with the next one.
#[derive(Clone)]
pub(super) struct Reader {
    base: Style,
    overwrite: bool,
    sgr: Sgr,
    done: Vec<Vec<Run>>,
    row: Row,
    held: String,
}

impl Reader {
    pub(super) fn new(base: Style, sgr: Sgr, overwrite: bool) -> Self {
        Reader {
            base,
            overwrite,
            sgr,
            done: Vec::new(),
            row: Row::default(),
            held: String::new(),
        }
    }

    /// The lines ended so far.
    pub(super) fn lines(&self) -> &[Vec<Run>] {
        &self.done
    }

    /// The SGR state after what was read.
    pub(super) fn sgr(&self) -> Sgr {
        self.sgr
    }

    /// Reads one more write of the stream.
    pub(super) fn feed(&mut self, text: &str) {
        if self.held.is_empty() {
            let at = open_tail(text);
            self.process(&text[..at]);
            self.held.push_str(&text[at..]);
        } else {
            let joined = std::mem::take(&mut self.held) + text;
            let at = open_tail(&joined);
            self.process(&joined[..at]);
            self.held = joined[at..].to_string();
        }
    }

    /// What the text read so far would be if it ended here: every line the held text ends, that text read as text, and the
    /// line it leaves open (none if nothing was written to it). The reader itself is left as it is.
    pub(super) fn ended(&self) -> (Vec<Vec<Run>>, Option<Vec<Run>>) {
        if self.held.is_empty() {
            return (Vec::new(), self.row.touched.then(|| self.row.runs(self.base)));
        }
        let mut end = Reader { done: Vec::new(), row: self.row.clone(), held: String::new(), ..Reader::new(self.base, self.sgr, self.overwrite) };
        end.process(&self.held);
        let open = end.row.touched.then(|| end.row.runs(end.base));
        (end.done, open)
    }

    /// All the lines, the open one last.
    pub(super) fn finish(mut self) -> Vec<Vec<Run>> {
        let (extra, open) = self.ended();
        self.done.extend(extra);
        self.done.extend(open);
        self.done
    }

    /// Writes `text` at the cursor, ending the line where it reaches `LINE_CAP` characters.
    fn put(&mut self, text: &str, style: Style) {
        let mut rest = text;
        loop {
            let room = LINE_CAP.saturating_sub(self.row.cursor());
            // `len` is at least the characters, so a text that fits in bytes fits; else the room-th character is where it cuts.
            let cut = if rest.len() <= room { None } else { rest.char_indices().nth(room).map(|(at, _)| at) };
            let Some(cut) = cut else {
                self.row.write(rest, style);
                return;
            };
            self.row.write(&rest[..cut], style);
            let row = std::mem::take(&mut self.row);
            self.done.push(row.runs(self.base));
            rest = &rest[cut..];
        }
    }

    /// Does one function: the one place the reader's operations are dispatched, and the one the tests drive. `arg` is the text
    /// of `Text` and the parameters of `Sgr`.
    fn apply(&mut self, f: Func, arg: &str, style: Style) {
        match f {
            Func::Text => self.put(arg, style),
            Func::Control => self.put(" ", style),
            Func::Return if self.overwrite => self.row.home(),
            Func::Newline | Func::Return => {
                let row = std::mem::take(&mut self.row);
                self.done.push(row.runs(self.base));
            }
            Func::Column => {
                if self.overwrite {
                    self.row.home();
                }
            }
            Func::Sgr => self.sgr.apply(arg),
            Func::EraseEnd | Func::EraseStart | Func::EraseAll => self.row.erase(f, self.base),
        }
    }

    fn process(&mut self, text: &str) {
        let special = |c: char| c.is_control() || matches!(c, '\u{85}' | '\u{2028}' | '\u{2029}');
        for piece in pieces(text) {
            match piece {
                Piece::Csi { params, final_byte } => {
                    if let Some(f) = Func::of_csi(params, final_byte) {
                        self.apply(f, params, Style::default());
                    }
                }
                Piece::Other => {}
                Piece::Text(t) => {
                    let style = self.sgr.style(self.base);
                    let mut rest = t;
                    while !rest.is_empty() {
                        let at = rest.find(special).unwrap_or(rest.len());
                        let (plain, tail) = rest.split_at(at);
                        if !plain.is_empty() {
                            self.apply(Func::Text, plain, style);
                        }
                        let mut chars = tail.chars();
                        let Some(c) = chars.next() else { break };
                        rest = chars.as_str();
                        match Func::of_control(c) {
                            Func::Return if rest.starts_with('\n') => {} // `\r\n`: the `\n` ends the line
                            f => self.apply(f, "", style),
                        }
                    }
                }
            }
        }
    }
}

/// A program's `text` as logical lines of styled runs, in `base` with `sgr` applied; `sgr` ends as the text left it, so
/// the next frame of the stream starts from it. See [`Reader`] for what is read; a final line the text does not end is
/// the last, and a final newline starts none.
pub(super) fn styled_lines(text: &str, base: Style, sgr: &mut Sgr, overwrite: bool) -> Vec<Vec<Run>> {
    let mut reader = Reader::new(base, *sgr, overwrite);
    reader.feed(text);
    *sgr = reader.sgr();
    reader.finish()
}

#[cfg(test)]
#[path = "sgr_tests.rs"]
mod tests;
