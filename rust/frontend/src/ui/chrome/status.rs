//! The status line and its neighbours: the cell-text rule, wrapping, pinned nav rows, the clock, battery and version
//! labels.

use super::*;

/// Text from outside the window (a daemon's reply, an agent's words, a file's name, a paste) as the rows a cell run
/// may draw. ratatui drops a `\n` inside a span and gives a `\r` or a `\t` a cell of its own, which the glyph layer
/// then draws as a line break or a tab stop, and cosmic-text panics on a second bidi paragraph in one line, so no
/// control character or separator may reach a cell: an ANSI CSI sequence (a colour code) is removed whole; `\r\n`,
/// `\n`, a lone `\r`, U+0085, U+2028 and U+2029 each end a row (a final one starts none, as in `str::lines`); and any
/// other control character is a space.
pub(in crate::ui) fn cell_lines(text: &str) -> Vec<String> {
    strip_csi(text)
        .replace("\r\n", "\n")
        .replace(|c| matches!(c, '\r' | '\u{85}' | '\u{2028}' | '\u{2029}'), "\n")
        .lines()
        .map(|row| row.replace(char::is_control, " "))
        .collect()
}

/// The same text where only one row exists: its rows joined by ` ⏎ `. Text with no control character or separator
/// comes back as it is.
pub(in crate::ui) fn cell_row(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains(|c: char| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')) {
        std::borrow::Cow::Owned(cell_lines(text).join(" ⏎ "))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// `text` without its ANSI CSI sequences: ESC `[`, parameter bytes 0x30-0x3F, intermediate bytes 0x20-0x2F and one
/// final byte 0x40-0x7E. Any other ESC stays, for `cell_lines` to make a space.
fn strip_csi(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("\u{1b}[") {
        return std::borrow::Cow::Borrowed(text);
    }
    let (mut out, mut rest) = (String::with_capacity(text.len()), text);
    while let Some(i) = rest.find("\u{1b}[") {
        out.push_str(&rest[..i]);
        let seq = &rest[i + 2..];
        let params = seq.find(|c| !matches!(c, '\u{30}'..='\u{3f}')).unwrap_or(seq.len());
        let end = seq[params..].find(|c| !matches!(c, '\u{20}'..='\u{2f}')).map_or(seq.len(), |k| params + k);
        if matches!(seq[end..].chars().next(), Some('\u{40}'..='\u{7e}')) {
            rest = &seq[end + 1..];
        } else {
            out.push('\u{1b}');
            rest = &rest[i + 1..];
        }
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

/// Wrap a status text into lines of display cells: the first line has
/// `first_w` cells (the pane's width less the "status: " label), the rest
/// `rest_w`. Breaks at a space where one falls in the line, else mid-word;
/// a first line with no room for text comes back empty. Width is in cells,
/// so wide glyphs never straddle a break; a glyph wider than `rest_w` still
/// takes a line to itself, so the loop always advances. `rest_w == 0` (no
/// pane) returns the text whole.
pub(in crate::ui) fn wrap_status(text: &str, first_w: usize, rest_w: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    if rest_w == 0 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut rest = text;
    let mut limit = first_w;
    let mut first = true;
    loop {
        // The longest prefix that fits `limit` cells, and the last space in it.
        let (mut w, mut end, mut last_space) = (0usize, 0usize, None);
        for (i, ch) in rest.char_indices() {
            let cw = ch.width().unwrap_or(0);
            if w + cw > limit {
                break;
            }
            if ch == ' ' {
                last_space = Some(i);
            }
            w += cw;
            end = i + ch.len_utf8();
        }
        if end == rest.len() {
            out.push(rest.to_string());
            return out;
        }
        let (cut, skip) = if rest[end..].starts_with(' ') {
            (end, 1)
        } else if let Some(sp) = last_space.filter(|&sp| sp > 0) {
            (sp, 1)
        } else if end == 0 && first {
            (0, 0)
        } else if end == 0 {
            (rest.chars().next().map_or(0, char::len_utf8), 0)
        } else {
            (end, 0)
        };
        out.push(rest[..cut].to_string());
        rest = &rest[cut + skip..];
        if rest.is_empty() {
            return out;
        }
        limit = rest_w;
        first = false;
    }
}

/// The nav pane's status lines: "status: " heads the first, each line of the
/// text (`cell_lines`) starts a row, and each wraps under it at `width` cells.
/// At most `max_rows` rows (`status_rows`).
pub(in crate::ui) fn status_spans(
    status: &str,
    width: usize,
    max_rows: usize,
) -> Vec<RtLine<'static>> {
    let label = "status: ";
    let text = Style::default().fg(Color::LightGreen);
    let first_w = width.saturating_sub(unicode_width::UnicodeWidthStr::width(label));
    let wrapped: Vec<Vec<String>> = cell_lines(status)
        .iter()
        .enumerate()
        .map(|(i, line)| wrap_status(line, if i == 0 { first_w } else { width }, width))
        .collect();
    let mut segs = status_rows(wrapped, max_rows).into_iter();
    let mut lines = vec![RtLine::from(vec![
        Span::styled(label, Style::default().fg(Color::DarkGray)),
        Span::styled(segs.next().unwrap_or_default(), text),
    ])];
    lines.extend(segs.map(|seg| RtLine::from(vec![Span::styled(seg, text)])));
    lines
}

/// The rows of a status's wrapped lines that fit `max_rows` (at least 3). Past
/// that, lines are picked by priority: the first (what failed), the second (its
/// first cause), the last (how it ended, or a hint), then the rest in order. A
/// line shows whole or not at all, except that the first two show their start,
/// cut to the rows left, rather than nothing; the front lines stop at the first
/// that does not fit, so one row, where the gap is, counts the lines not shown.
fn status_rows(wrapped: Vec<Vec<String>>, max_rows: usize) -> Vec<String> {
    let max_rows = max_rows.max(3);
    if wrapped.iter().map(Vec::len).sum::<usize>() <= max_rows {
        return wrapped.concat();
    }
    let n = wrapped.len();
    let (mut room, mut front, mut shown, mut cut) = (max_rows - 1, Vec::new(), 0, false);
    while shown < n.min(2) && room > 0 && !cut {
        let take = wrapped[shown].len().min(room);
        front.extend_from_slice(&wrapped[shown][..take]);
        room -= take;
        cut = take < wrapped[shown].len();
        shown += 1;
    }
    let last = (!cut && shown < n && wrapped[n - 1].len() <= room).then_some(n - 1);
    room -= last.map_or(0, |l| wrapped[l].len());
    while !cut && shown < last.unwrap_or(n) && wrapped[shown].len() <= room {
        room -= wrapped[shown].len();
        front.extend_from_slice(&wrapped[shown]);
        shown += 1;
    }
    let hidden = n - shown - usize::from(last.is_some());
    front.push(match hidden {
        0 => "…".to_string(),
        1 => "… 1 line not shown".to_string(),
        _ => format!("… {hidden} lines not shown"),
    });
    front.extend(last.into_iter().flat_map(|l| wrapped[l].iter().cloned()));
    front
}

/// The nav pane's pinned rows, drawn under the scrolled list so no scroll
/// hides them: the lease notice, then `line` (the not-ended count or
/// what a leave waits for), then the open prompt, each row of each (`cell_lines`)
/// wrapped at `width` cells. A pane too short for them all gives whole lines
/// in priority order (the prompt, then `line`, then the notice) and drops the
/// rest whole; a prompt that does not fit keeps only its last row. A prompt
/// is its text and its choice (empty for none); the choice joins the last
/// wrapped row if it fits there, else takes its own row, so the kept last row
/// always holds it whole; in a pane narrower than the choice it is dropped,
/// never cut. `line` may be two lines, kept or dropped together. Returns the
/// height left to the list, the rows, and whether `line` was drawn whole.
pub(in crate::ui) fn nav_pinned_rows(
    prompt: Option<(&str, &str)>,
    notice: Option<&str>,
    line: Option<&str>,
    width: usize,
    pane_height: usize,
) -> (usize, Vec<String>, bool) {
    let wrap = |text: Option<&str>| -> Vec<String> {
        text.map(|t| {
            cell_lines(t)
                .iter()
                .flat_map(|l| wrap_status(l, width, width))
                .collect()
        })
        .unwrap_or_default()
    };
    let prompt_in = prompt;
    let mut prompt = wrap(prompt.map(|(text, _)| text));
    if let Some((_, choice)) = prompt_in.filter(|(_, c)| !c.is_empty() && c.chars().count() <= width) {
        match prompt.last_mut() {
            Some(last) if last.chars().count() + 3 + choice.chars().count() <= width => {
                last.push_str("   ");
                last.push_str(choice);
            }
            _ => prompt.push(choice.to_string()),
        }
    }
    if prompt.len() > pane_height {
        prompt.drain(..prompt.len() - pane_height.min(1));
    }
    let mut left = pane_height - prompt.len();
    let mut whole = |rows: Vec<String>| {
        if rows.len() > left {
            return Vec::new();
        }
        left -= rows.len();
        rows
    };
    let line = whole(wrap(line));
    let notice = whole(wrap(notice));
    let drawn = !line.is_empty();
    let rows = [notice, line, prompt].concat();
    (pane_height - rows.len(), rows, drawn)
}

impl State {
    /// Refresh the cached battery label if it's stale (or never queried).
    /// The OS query (via the cross-platform `battery` crate) is not free, so
    /// it runs at most once per `BATTERY_QUERY_INTERVAL`; between refreshes the
    /// per-second clock repaint reuses the cached value. On no battery present
    /// (desktop / CI) or any query error we set the label to `None` so the
    /// chrome paints nothing for the battery — never a fake `0%` or `N/A`.
    pub(in crate::ui) fn refresh_battery_label(&mut self) {
        /// How often to hit the OS for a fresh battery reading.
        const BATTERY_QUERY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

        let now = std::time::Instant::now();
        let fresh = self
            .last_battery_query
            .map(|t| now.duration_since(t) < BATTERY_QUERY_INTERVAL)
            .unwrap_or(false);
        if fresh {
            return;
        }
        self.last_battery_query = Some(now);
        self.battery_label = query_battery_label();
    }
}

/// Middle-truncate `s` to at most `max` chars, biasing the tail so a path's
/// basename + extension stay visible (`src/very/long/Mod…Name.jl`). Returns
/// `s` unchanged when it already fits. The full name is always recoverable
/// via Ctrl+C in NavTree (copies the absolute path).
pub(in crate::ui) fn middle_truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max <= 3 {
        return chars.iter().take(max).collect();
    }
    let keep = max - 1; // one cell for the ellipsis
    let head = keep / 2; // favour the tail so the extension survives
    let tail = keep - head;
    let head_s: String = chars.iter().take(head).collect();
    let tail_s: String = chars.iter().skip(chars.len() - tail).collect();
    format!("{head_s}…{tail_s}")
}

/// Query the OS for the primary battery's charge level and return a short
/// chrome label like `85%`, or `+85%` while charging (the leading `+` marks a
/// charging state). Returns `None` when there's no battery (desktop / CI) or
/// the query errors — the caller renders nothing in that case, never a fake
/// `0%`/`N/A`. Cross-platform via the `battery` (starship-battery) crate; no
/// OS-specific calls here.
///
/// This reads the OS and is not free; callers must rate-limit it (see
/// `State::refresh_battery_label`), not call it per frame.
fn query_battery_label() -> Option<String> {
    use battery::State as BatState;

    let manager = battery::Manager::new().ok()?;
    // First battery only. `batteries()` can error; an empty iterator means no
    // battery present — both yield `None` (paint nothing).
    let battery = manager.batteries().ok()?.next()?.ok()?;

    // State of charge is a ratio in [0, 1]; render as a whole percent.
    let ratio = battery.state_of_charge().value;
    if !ratio.is_finite() {
        return None;
    }
    let pct = (ratio * 100.0).round().clamp(0.0, 100.0) as u32;

    let charging = matches!(battery.state(), BatState::Charging);
    Some(if charging {
        format!("+{pct}%")
    } else {
        format!("{pct}%")
    })
}

/// Build the top-right chrome clock text: the local date prefixed onto the
/// existing `HH:MM:SS` time (unchanged), e.g. `Tue Sep 15 15:42:07`. The
/// date is the first thing dropped when `available_width` is too narrow for
/// the dated form — the time itself never shrinks or disappears here; the
/// caller's own width guard still decides whether even the time-only label
/// fits at all.
pub(in crate::ui) fn clock_label(now: chrono::NaiveDateTime, available_width: u16) -> String {
    let time = now.format("%H:%M:%S").to_string();
    let dated = format!("{} {time}", now.format("%a %b %-d"));
    // Same one-cell-each-side padding the render guard checks the label
    // against (` {label} `), so this mirrors that fit check exactly.
    if dated.chars().count() as u16 + 2 <= available_width {
        dated
    } else {
        time
    }
}

/// Overlay a pane title onto a border row, clamped to `max_w` cells so
/// it can't overrun the pane's width and clobber the inner-cross
/// junction or the neighbouring pane's title. The wireframe was
/// painted first with `─` everywhere on the edge; this writes the
/// title text starting at `(x, y)` with the given style, replacing
/// those cells. Title strings carry their own leading/trailing space
/// so the line breaks cleanly on either side of the label. The text is
/// one row, so a line break in it shows as `cell_row`'s ` ⏎ `.
pub(in crate::ui) fn write_title(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    title: &str,
    max_w: u16,
    style: Style,
) {
    if y >= buf.area.height {
        return;
    }
    let cap = (max_w as usize).min(buf.area.width.saturating_sub(x) as usize);
    let s: String = cell_row(title).chars().take(cap).collect();
    buf.set_string(x, y, &s, style);
}

/// Build the bottom-edge version stamp: `" fe <fe> · be <be> "`, plus a
/// `skew` flag that is true when the two halves disagree.
///
/// Both halves are ALWAYS shown, even when they match. The point of the
/// stamp is that FE and BE drift apart independently — one gets rebuilt, the
/// other doesn't — so collapsing to a single version in the happy case would
/// hide exactly the field you are watching. `None` (a pre-versioning daemon,
/// or no hello yet) renders `be ?` and is NOT counted as skew: unknown is not
/// the same as different, and colouring it as a mismatch would cry wolf on
/// every launch before the first hello lands.
pub(in crate::ui) fn version_label(fe: &str, be: Option<&str>) -> (String, bool) {
    let be = be.unwrap_or("?");
    let skew = be != "?" && be != fe;
    (format!(" fe {fe} · be {be} "), skew)
}

/// How long a pushed `notify` toast stays pinned on the status line — long
/// enough to read across a workspace switch (which rebuilds the status
/// immediately and would otherwise clobber it). After this window
/// `about_to_wait` restores the normal connection status on the idle tick.
// 10s (was 4s): fe-command notifies render only on the one-line status bar
// today and were reliably missed at 4s (2026-07-10 papers-geometry diagnosis).
// A real toast surface is queued (ops TODO, rides the R6 redraw decomposition).
pub(in crate::ui) const NOTIFY_STICKY: std::time::Duration = std::time::Duration::from_secs(10);

/// Short human-readable name for a winit logical key — what to surface in the
/// chrome and in tracing logs. Falls back to a `Debug` print for keys we
/// haven't pattern-matched yet.
pub(in crate::ui) fn key_label(k: &Key) -> String {
    match k {
        Key::Character(s) => s.to_string(),
        Key::Named(n) => match n {
            NamedKey::Enter => "Enter".to_string(),
            NamedKey::Tab => "Tab".to_string(),
            NamedKey::Space => "Space".to_string(),
            NamedKey::Backspace => "Backspace".to_string(),
            NamedKey::Escape => "Escape".to_string(),
            NamedKey::ArrowUp => "Up".to_string(),
            NamedKey::ArrowDown => "Down".to_string(),
            NamedKey::ArrowLeft => "Left".to_string(),
            NamedKey::ArrowRight => "Right".to_string(),
            other => format!("{other:?}"),
        },
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
