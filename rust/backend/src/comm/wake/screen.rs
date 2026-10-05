//! Whether a captured screen is a free prompt: the glyph, box, panel and typed-line tests, and the rows the wake holds.

use super::*;

/// The rule drawn above and below Claude Code's input box.
const RULE: char = '\u{2500}';

/// The prompt glyphs of an agent the daemon can read, none for one it cannot.
/// Claude Code draws `❯`, or a bare `>` when its unicode check fails, which on
/// Windows depends on the environment. Codex ships OFF: no Codex screen has
/// been captured, and a glyph is never guessed, so a Codex row counts as a row
/// the daemon cannot type into.
pub(super) fn prompt_glyphs(agent: &str, windows: bool) -> &'static [char] {
    match agent {
        "claude" if windows => &['\u{276f}', '>'],
        "claude" => &['\u{276f}'],
        _ => &[],
    }
}

/// A line of nothing but [`RULE`] (trailing spaces aside).
fn is_rule(line: &str) -> bool {
    let line = line.trim_end_matches(' ');
    !line.is_empty() && line.chars().all(|c| c == RULE)
}

/// An input-box border: a rule, or a rule with one ` label ` set into it, as Claude Code draws a named
/// session's box top (`───── name ─`; seen 2026-10-02, v2.1.285) and the box of an agent being viewed
/// (`──── task ─`, v2.1.287). A label does not say whose box it is; [`panel_refusal`] does. Known limits, both
/// fail closed: a label containing `─` is not a border, and neither is a label as wide as the line (no rule
/// left on one side).
fn is_border(line: &str) -> bool {
    let line = line.trim_end_matches(' ');
    if is_rule(line) {
        return true;
    }
    let Some(inner) = line.strip_prefix(RULE).and_then(|l| l.strip_suffix(RULE)) else {
        return false;
    };
    let label = inner.trim_matches(RULE);
    label.len() > 2 && label.starts_with(' ') && label.ends_with(' ') && !label.contains(RULE)
}

/// Whether the input prompt's own separator, U+00A0 after the glyph, is required on Windows. On Linux it always
/// is. Off until a Windows screen read shows the NBSP survives ConPTY; while off, a bare glyph also counts there,
/// and an agents-view task box with an empty placeholder (a voice state) reads free: a named Windows-only gap.
const NBSP_ON_WINDOWS: bool = false;

fn nbsp_required(windows: bool) -> bool {
    !windows || NBSP_ON_WINDOWS
}

/// The agents panel's mark for the agent the input reaches, and for every other agent.
const PANEL_DOT: char = '\u{25cf}';
const PANEL_RING: char = '\u{25ef}';

/// What the screen is as a prompt for the wake: `Ok(Empty)` when it is free, `Ok(HoldsLine)` when the box holds just
/// the wake line (the rest of the structural test passing), else the reason it is neither. Free is all of: (a) only spaces before
/// the glyph; (b) the cursor just after the glyph, or one more; (c) the line directly between the input box's
/// two borders (a rule, optionally labelled); (d) after the glyph U+00A0, the main prompt's own mark (menus and
/// dialog inputs draw an ASCII space), then nothing but spaces, or, where the NBSP is not required
/// ([`NBSP_ON_WINDOWS`]), nothing but spaces at all; (e) the input reaches the main agent ([`panel_refusal`]); (f) the wake line fits on the prompt row ([`fits`]).
/// The wake reads the cursor's row with dim cells blank (`screen::free_test_lines`), so Claude Code's dim
/// suggestion or placeholder reads empty and a typed draft reads not free wherever its cursor sits. One frame
/// cannot tell a working row, whose input box is live too; the hold in `wake_if_free` does.
pub(super) fn prompt_of(lines: &[String], cursor: Option<(u16, u16)>, agent: &str, windows: bool) -> Result<Prompt, &'static str> {
    match input_refused(lines, cursor, agent, windows, Expect::Empty) {
        None if !fits(lines, cursor, WAKE_LINE) => Err("pane too narrow for the wake line"),
        None => Ok(Prompt::Empty),
        // Any refusal while the box holds the wake's own line: an earlier wake typed it and did not send it.
        Some(_) if typed_refusal(lines, cursor, agent, windows, WAKE_LINE).is_none() => Ok(Prompt::HoldsLine),
        Some(reason) => Err(reason),
    }
}

/// What a prompt [`prompt_of`] accepts holds: nothing (the free test), or exactly the wake line, typed by an earlier
/// wake and not sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Prompt {
    Empty,
    HoldsLine,
}

/// Whether `text`, typed after the glyph and its NBSP, leaves two columns before the end of the box's lower rule,
/// which Claude Code draws the pane's full width: one for the cursor after the text, one spare, as the column at
/// which Claude Code wraps its input has not been measured. The gate before Enter reads the prompt's one row, so a
/// line that would wrap is never typed. Asked only once [`input_refused`] has passed, so the cursor's row lies
/// between two borders.
fn fits(lines: &[String], cursor: Option<(u16, u16)>, text: &str) -> bool {
    let Some((row, _)) = cursor else {
        return false;
    };
    let row = row as usize;
    let width = lines[row + 1].trim_end_matches(' ').chars().count();
    let glyph = lines[row].chars().take_while(|c| *c == ' ').count();
    glyph + 2 + text.chars().count() + 2 <= width
}

/// What the input box is expected to hold: nothing (the free test), or exactly the typed wake line (the gate
/// before Enter).
enum Expect<'a> {
    Empty,
    Typed(&'a str),
}

/// Why `text`, just typed, does not sit alone in main's input box (`None` when it does): every structural check of [`prompt_of`] (a box,
/// the glyph, nothing before it, [`panel_refusal`] below) except the cursor column, then after the glyph the
/// NBSP (a space where the NBSP is not required), exactly `text`, and spaces. Enter goes only when this holds.
pub(crate) fn typed_refusal(lines: &[String], cursor: Option<(u16, u16)>, agent: &str, windows: bool, text: &str) -> Option<&'static str> {
    input_refused(lines, cursor, agent, windows, Expect::Typed(text))
}

fn input_refused(lines: &[String], cursor: Option<(u16, u16)>, agent: &str, windows: bool, expect: Expect) -> Option<&'static str> {
    let glyphs = prompt_glyphs(agent, windows);
    if glyphs.is_empty() {
        return Some("no wake predicate for this agent");
    }
    let Some((row, col)) = cursor else {
        return Some("no cursor");
    };
    let (row, col) = (row as usize, col as usize);
    let boxed = row > 0 && row + 1 < lines.len() && is_border(&lines[row - 1]) && is_border(&lines[row + 1]);
    if !boxed {
        return Some("not in an input box");
    }
    let cells: Vec<char> = lines[row].chars().collect();
    let Some(g) = cells.iter().position(|c| glyphs.contains(c)) else {
        return Some("no prompt glyph");
    };
    if !cells[..g].iter().all(|c| *c == ' ') {
        return Some("text before the glyph");
    }
    let rest = &cells[g + 1..];
    let blank = |cs: &[char]| cs.iter().all(|c| *c == ' ');
    match expect {
        Expect::Empty => {
            if col != g + 1 && col != g + 2 {
                return Some("cursor not at the prompt");
            }
            let empty = (rest.first() == Some(&'\u{a0}') && blank(&rest[1..])) || (!nbsp_required(windows) && blank(rest));
            if !empty {
                return Some("input not empty");
            }
        }
        Expect::Typed(text) => {
            let after = match rest.first() {
                Some('\u{a0}') => &rest[1..],
                Some(' ') if !nbsp_required(windows) => &rest[1..],
                _ => return Some("typed text not in main's input box"),
            };
            let want: Vec<char> = text.chars().collect();
            if !after.starts_with(&want) || !blank(&after[want.len()..]) {
                return Some("typed text not in main's input box");
            }
        }
    }
    panel_refusal(&lines[row + 2..])
}

/// Whether the input reaches the main agent, read from the rows below the box. If no line there carries
/// [`PANEL_DOT`] or [`PANEL_RING`], there is no agents panel and it does. Otherwise every panel line starts with two
/// spaces (a focused panel draws `❯ ` on the line under its cursor) and the one dotted line is `  ● main`. Claude
/// Code's footer is not read: its wording changes with the version, the permission mode, the background tasks and
/// the width, so it is no evidence of focus. With
/// agent view off, as the daemon runs every new capsule row, focus on the panel draws the panel cursor at once and
/// moves the terminal cursor off the prompt (Claude Code 2.1.288, 2026-10-03). Known gap, fails open: with agent view
/// on, 2.1.287's first ↓ moves focus to the panel and draws nothing (capture 05), and that frame reads free; there
/// the line types into the prompt and, if it shows, Enter opens the selected footer item rather than sending it
/// (Claude Code docs, keybindings, "Footer actions"), so the line is left unsent.
fn panel_refusal(below: &[String]) -> Option<&'static str> {
    let panel: Vec<&str> = below.iter().map(|l| l.trim_end_matches(' ')).filter(|l| l.contains(PANEL_DOT) || l.contains(PANEL_RING)).collect();
    if panel.is_empty() {
        return None;
    }
    if panel.iter().any(|l| !l.starts_with("  ")) {
        return Some("agents panel focused or unrecognised");
    }
    let dotted: Vec<&str> = panel.iter().copied().filter(|l| l.contains(PANEL_DOT)).collect();
    if dotted != ["  \u{25cf} main"] {
        return Some("the panel's dot is not on main");
    }
    None
}

/// The screen lines as the headless client reads them (`current_lines` in
/// rows/run/headless.rs), trimming ASCII spaces only: the no-break space after the
/// glyph is the main input prompt's own mark, and `trim_end` would strip it.
pub(super) fn wake_lines(screen: &vt100_ctt::Screen) -> Vec<String> {
    let (_, cols) = screen.size();
    screen.rows(0, cols).map(|line| line.trim_end_matches(' ').to_string()).collect()
}

/// [`wake_lines`] as the wake's free test reads them: on the cursor's row a dim cell (SGR 2) reads as a
/// space. Claude Code draws its prompt suggestion and placeholders dim, and they are not input; a typed draft
/// is not dim. Only that row: the hold compares [`wake_lines`] whole.
pub(crate) fn free_test_lines(screen: &vt100_ctt::Screen) -> Vec<String> {
    let (row, _) = screen.cursor_position();
    let (_, cols) = screen.size();
    let mut lines = wake_lines(screen);
    if let Some(line) = lines.get_mut(row as usize) {
        *line = (0..cols)
            .filter_map(|col| screen.cell(row, col))
            .filter(|cell| !cell.is_wide_continuation())
            .map(|cell| if cell.dim() || !cell.has_contents() { " " } else { cell.contents() })
            .collect::<String>()
            .trim_end_matches(' ')
            .to_string();
    }
    lines
}

/// The rows the wake's hold compares: from the top of the screen through
/// the line under the cursor, which for claude is the input box's lower
/// rule. Claude Code draws a background-agent footer below the box that
/// ticks every second at rest, while a working turn's spinner is always
/// above the box.
pub(crate) fn held_rows(lines: &[String], cursor_row: u16) -> &[String] {
    &lines[..lines.len().min(cursor_row as usize + 2)]
}

#[cfg(test)]
#[path = "screen_tests.rs"]
mod tests;
