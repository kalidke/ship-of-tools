//! vt100 helpers shared by the Terminal drawer and the agent pane: scrolling, key encoding, painting.

use crate::ui::*;

/// Move a terminal's scrollback view by `delta` rows (positive = older).
///
/// The emulator owns this offset and maintains it: `Grid::scroll_up` grows
/// it as lines enter scrollback while the view is held back, which is what
/// keeps the rows being read still under arriving output. Write it only on
/// a user action; never mirror it in `State`.
pub(in crate::ui) fn scroll_ring(scr: &mut vt100::Screen, delta: i32) {
    let cur = scr.scrollback() as i32;
    scr.set_scrollback((cur + delta).max(0) as usize);
}

/// Translate a key event into the byte sequence a PTY expects. Shared by
/// the LLM pane (remote tmux pty) and the local terminal drawer (G4). With
/// `ctrl`, ASCII letters map to control codes (Ctrl+C → 0x03, Ctrl+B →
/// 0x02, …) so shell editing, signals, and tmux prefixes work; non-letters
/// under Ctrl pass through verbatim. Returns `None` for keys with no PTY
/// encoding (bare modifiers, etc.).
pub(in crate::ui) fn key_to_pty_bytes(key: &Key, ctrl: bool, shift: bool, super_: bool) -> Option<Vec<u8>> {
    // A Command chord is never terminal input: it either resolved to an app
    // action above (handled before this call), or it is residual input that
    // must not leak into the shell as a raw keystroke -- macOS delivers
    // Cmd+<letter> as a plain Character with `super_` set, which is exactly
    // the bug this guard exists for. Gated to macOS: on Windows/Linux the OS
    // or window manager owns Super chords and nothing changes there.
    if cfg!(target_os = "macos") && super_ {
        return None;
    }
    match key {
        Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
        Key::Named(NamedKey::Backspace) => Some(b"\x7f".to_vec()),
        // Shift+Tab is BackTab (CSI Z). TUIs that live in the pty — Claude Code
        // most of all — read it to cycle backwards / toggle modes (the plan-mode
        // chord). Without the shift check this collapsed to a plain `\t`, so the
        // chord was dead through the FE. Plain Tab stays `\t` for completion.
        Key::Named(NamedKey::Tab) => {
            if shift {
                Some(b"\x1b[Z".to_vec())
            } else {
                Some(b"\t".to_vec())
            }
        }
        Key::Named(NamedKey::Escape) => Some(b"\x1b".to_vec()),
        Key::Named(NamedKey::Space) => Some(b" ".to_vec()),
        Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B".to_vec()),
        Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C".to_vec()),
        Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D".to_vec()),
        Key::Named(NamedKey::Home) => Some(b"\x1b[H".to_vec()),
        Key::Named(NamedKey::End) => Some(b"\x1b[F".to_vec()),
        Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~".to_vec()),
        Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~".to_vec()),
        Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
        Key::Character(s) => {
            if ctrl {
                let mut out = Vec::with_capacity(s.len());
                for c in s.chars() {
                    let lower = c.to_ascii_lowercase();
                    if lower.is_ascii_lowercase() {
                        out.push((lower as u8) - b'a' + 1);
                    } else {
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    }
                }
                if out.is_empty() {
                    None
                } else {
                    Some(out)
                }
            } else {
                Some(s.as_bytes().to_vec())
            }
        }
        _ => None,
    }
}

/// Paint the vt100 terminal grid into `rect`. Each cell of the
/// emulator screen at (row, col) is written at (rect.x + col,
/// rect.y + row) with the cell's foreground colour and bold/italic
/// attributes mapped onto ratatui Style. Background colour is
/// dropped for now — the chrome pipeline doesn't carry it yet
/// (planned bg colour + underline modifiers).
pub(in crate::ui) fn paint_terminal(
    buf: &mut ratatui::buffer::Buffer,
    rect: ratatui::layout::Rect,
    screen: &vt100::Screen,
) {
    let cols = rect.width;
    let rows = rect.height;
    // vt100 reports cursor in row-major (row, col) of its own grid; we
    // overlay it via Modifier::REVERSED below. Captured before the cell
    // walk so the cursor cell's pre-existing style still wins for color.
    let (cur_row, cur_col) = screen.cursor_position();
    let cursor_hidden = screen.hide_cursor();
    for row in 0..rows {
        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            let contents = cell.contents();
            // Empty cell: nothing to draw — the wireframe / surrounding
            // paint already cleared this area.
            let glyph = if contents.is_empty() { " " } else { contents };
            let mut fg = vt100_color_to_ratatui(cell.fgcolor());
            // Dim on the default foreground would otherwise fall back to
            // the chrome's white default — the Claude Code CLI uses dim
            // to muff out its suggested-prompt placeholder, so promote
            // it to Gray so the muting actually shows.
            if cell.dim() && matches!(fg, Color::Reset) {
                fg = Color::Gray;
            }
            let mut style = Style::default().fg(fg);
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.dim() {
                style = style.add_modifier(Modifier::DIM);
            }
            if cell.italic() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            // Block cursor XOR's REVERSED with the cell's existing
            // inverse state — so a cursor sitting on an inverse status-bar
            // cell flips back to non-reversed instead of disappearing,
            // matching how xterm/alacritty draw their block cursors.
            // Selection visibility lives on the GPU side as a yellow
            // quad rendered before text, not in this REVERSED flag —
            // REVERSED inverted both fg and bg, which made selected text
            // hard to read.
            let is_cursor = !cursor_hidden && row == cur_row && col == cur_col;
            let reverse = cell.inverse() ^ is_cursor;
            if reverse {
                style = style.add_modifier(Modifier::REVERSED);
            }
            buf.set_string(rect.x + col, rect.y + row, glyph, style);
        }
    }
}

/// Map a vt100 fg/bg colour to the nearest ratatui Color. ANSI 16-colour
/// palette goes through the named variants; indexed (256-colour) and
/// RGB pass through as Color::Indexed / Color::Rgb so the existing
/// chrome render path can emit them.
fn vt100_color_to_ratatui(c: vt100::Color) -> Color {
    use vt100::Color as V;
    match c {
        V::Default => Color::Reset,
        V::Idx(0) => Color::Black,
        V::Idx(1) => Color::Red,
        V::Idx(2) => Color::Green,
        V::Idx(3) => Color::Yellow,
        V::Idx(4) => Color::Blue,
        V::Idx(5) => Color::Magenta,
        V::Idx(6) => Color::Cyan,
        V::Idx(7) => Color::Gray,
        V::Idx(8) => Color::DarkGray,
        V::Idx(9) => Color::LightRed,
        V::Idx(10) => Color::LightGreen,
        V::Idx(11) => Color::LightYellow,
        V::Idx(12) => Color::LightBlue,
        V::Idx(13) => Color::LightMagenta,
        V::Idx(14) => Color::LightCyan,
        V::Idx(15) => Color::White,
        V::Idx(other) => Color::Indexed(other),
        V::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
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
}
