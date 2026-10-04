//! The status line and its neighbours: wrapping, pinned nav rows, the clock, battery and version labels.

use super::*;

/// Wrap a status text into lines of display cells: the first line has
/// `first_w` cells (the pane's width less the "status: " label), the rest
/// `rest_w`. Breaks at a space where one falls in the line, else mid-word;
/// a first line with no room for text comes back empty. Width is in cells,
/// so wide glyphs never straddle a break; a glyph wider than `rest_w` still
/// takes a line to itself, so the loop always advances. `rest_w == 0` (no
/// pane) returns the text whole.
fn wrap_status(text: &str, first_w: usize, rest_w: usize) -> Vec<String> {
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

/// The nav pane's status lines: "status: " heads the first, and the text
/// wraps under it at `width` cells.
pub(in crate::ui) fn status_spans(status: &str, width: usize) -> Vec<RtLine<'static>> {
    let label = "status: ";
    let text = Style::default().fg(Color::LightGreen);
    let first_w = width.saturating_sub(unicode_width::UnicodeWidthStr::width(label));
    let mut segs = wrap_status(status, first_w, width).into_iter();
    let mut lines = vec![RtLine::from(vec![
        Span::styled(label, Style::default().fg(Color::DarkGray)),
        Span::styled(segs.next().unwrap_or_default(), text),
    ])];
    lines.extend(segs.map(|seg| RtLine::from(vec![Span::styled(seg, text)])));
    lines
}

/// The nav pane's pinned rows, drawn under the scrolled list so no scroll
/// hides them: the lease notice, then `line` (the not-ended count or
/// `closing…`), then the open prompt, each wrapped at `width` cells. A pane
/// too short for them all gives whole lines in priority order (the prompt,
/// then `line`, then the notice) and drops the rest whole; a prompt that
/// does not fit keeps only its last row. A prompt is its text and its choice
/// (empty for none); the choice joins the last wrapped row if it fits there,
/// else takes its own row, so the kept last row always holds it whole; in a
/// pane narrower than the choice it is dropped, never cut. `line` may be two
/// lines, kept or dropped together. Returns the height left to the
/// list, the rows, and whether `line` was drawn whole.
pub(in crate::ui) fn nav_pinned_rows(
    prompt: Option<(&str, &str)>,
    notice: Option<&str>,
    line: Option<&str>,
    width: usize,
    pane_height: usize,
) -> (usize, Vec<String>, bool) {
    let wrap = |text: Option<&str>| -> Vec<String> {
        text.map(|t| t.split('\n').flat_map(|l| wrap_status(l, width, width)).collect()).unwrap_or_default()
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
/// so the line breaks cleanly on either side of the label.
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
    let s: String = title.chars().take(cap).collect();
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
mod tests {
    use super::*;

    #[test]
    fn no_lease_notice_is_pinned_above_a_tall_list() {
        use crate::lease::{lease_notice, Standing::*};
        for set in [&[Undetermined][..], &[Unsupported], &[Foreign], &[Unreached], &[]] {
            let notice = lease_notice(false, set).unwrap();
            let (_, pinned, _) = nav_pinned_rows(None, Some(notice), None, 80, 5);
            assert!(pinned.join(" ").contains("closing will not end sessions"), "{pinned:?}");
        }
    }

    #[test]
    fn leave_count_shows_with_nav_collapsed() {
        use crate::lease::{LeaveOutcome, LeaveStep, Leaving};
        let area = ratatui::layout::Rect::new(0, 0, 160, 48);
        let preset = crate::settings::LayoutPreset::default_ultrawide();
        let nav = |slot| crate::layout::compute(area, &preset, false, slot).rect_for(crate::settings::Slot::Nav);
        assert_eq!(nav(maximize_slot(true, PaneFocus::Preview, false)).width, 0, "a maximized preview hides nav");
        // A leave whose reply carried a count.
        let t0 = std::time::Instant::now();
        let (ack, rx) = tokio::sync::oneshot::channel();
        let mut leaving = Leaving::new(LeaveIntent::Close, 0, vec![("h".to_string(), rx)], t0);
        ack.send(LeaveOutcome::Replied(7)).unwrap();
        assert_eq!(leaving.poll(t0), LeaveStep::Show);
        // One frame, laid out as `redraw` lays it out with the preview maximized.
        let line = leaving.line();
        let rect = nav(maximize_slot(true, PaneFocus::Preview, line.is_some()));
        let (_, _, whole) = nav_pinned_rows(None, None, line.as_deref(), rect.width as usize, rect.height as usize);
        assert!(whole, "the count line is drawn whole in a {}x{} nav pane", rect.width, rect.height);
        assert_eq!(leaving.presented(t0), vec![("h".to_string(), 7)], "and acked");
    }

    fn fixed_now() -> chrono::NaiveDateTime {
        // A Monday, so the weekday abbreviation is unambiguous.
        chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(5, 7, 9)
            .unwrap()
    }

    #[test]
    fn clock_label_includes_date_when_there_is_room() {
        assert_eq!(clock_label(fixed_now(), 25), "Mon Jan 1 05:07:09");
    }

    #[test]
    fn clock_label_drops_the_date_when_narrow() {
        // Too narrow for "Mon Jan 1 05:07:09" (18 chars + 2 padding = 20);
        // the date is dropped first and the time survives unchanged.
        assert_eq!(clock_label(fixed_now(), 15), "05:07:09");
    }

    #[test]
    fn wrap_status_table() {
        // (description, text, first_w, rest_w, expected lines). A 20-wide pane
        // gives the text 12 cells after "status: " on the first line.
        let cases: &[(&str, &str, usize, usize, &[&str])] = &[
            ("empty", "", 12, 20, &[""]),
            ("fits", "ready", 12, 20, &["ready"]),
            ("exactly the first line", "abcdefghijkl", 12, 20, &["abcdefghijkl"]),
            ("one cell over, no space: hard break", "abcdefghijklm", 12, 20, &["abcdefghijkl", "m"]),
            ("two lines, break at the last space", "hello world again today", 12, 20, &["hello world", "again today"]),
            ("a space exactly at the edge", "hello world", 5, 13, &["hello", "world"]),
            ("three lines", "aaa bbb ccc ddd eee fff", 6, 14, &["aaa", "bbb ccc ddd", "eee fff"]),
            ("a word longer than the width", "abcdefghijklmnopqrstuv", 2, 10, &["ab", "cdefghijkl", "mnopqrstuv"]),
            ("width smaller than the label", "abcdefgh", 0, 5, &["", "abcde", "fgh"]),
            ("a wide glyph never straddles", "日本語", 3, 11, &["日", "本語"]),
            ("a glyph wider than the line still advances", "日本", 1, 1, &["", "日", "本"]),
            ("a trailing space at the break adds no empty line", "hello ", 5, 20, &["hello"]),
            ("no pane", "abc def", 0, 0, &["abc def"]),
        ];
        for (what, text, first_w, rest_w, want) in cases {
            let got = wrap_status(text, *first_w, *rest_w);
            assert_eq!(got, *want, "{what}");
            // Every line fits its width, and nothing but the break spaces is lost.
            if *rest_w > 0 {
                use unicode_width::UnicodeWidthStr;
                for (i, line) in got.iter().enumerate() {
                    let cap = if i == 0 { *first_w } else { *rest_w };
                    assert!(line.width() <= cap || line.chars().count() == 1, "{what}: line {i} too wide");
                }
            }
        }
        // A 120-character toast at the default nav width wraps in full.
        let toast = "The quick brown fox jumps over the lazy dog while the build finishes and every suite reports back to the lane ok";
        let lines = wrap_status(toast, 22, 30);
        assert_eq!(lines.join(" "), toast);
    }

    fn line_text(line: &RtLine) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn status_spans_show_text_once() {
        let short: Vec<String> = status_spans("ready", 40).iter().map(line_text).collect();
        assert_eq!(short, ["status: ready"]);
        let text = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima";
        let lines: Vec<String> = status_spans(text, 20).iter().map(line_text).collect();
        let segs = wrap_status(text, 12, 20);
        assert!(segs.len() > 2, "the text must wrap: {segs:?}");
        let mut want = vec![format!("status: {}", segs[0])];
        want.extend(segs[1..].iter().cloned());
        assert_eq!(lines, want);
        let all = lines.join(" ");
        assert_eq!(all, format!("status: {text}"));
        assert_eq!(all.matches("status: ").count(), 1);
        for seg in &segs {
            assert_eq!(all.matches(seg.as_str()).count(), 1, "{seg}");
        }
    }

    #[test]
    fn quit_prompt_visible_when_scrolled() {
        // The pinned rows never read the list or its scroll: however long
        // the list and however far it is scrolled, they sit under it.
        let (text, choice) = quit_prompt_line(false);
        let prompt = Some((text.as_str(), choice.as_str()));
        let (list_h, rows, _) = nav_pinned_rows(prompt, None, None, 30, 20);
        assert!(rows.join(" ").starts_with(&text) && rows.join(" ").ends_with(&choice), "{rows:?}");
        assert_eq!(list_h + rows.len(), 20);
        assert!(rows.len() > 1 && list_h > 0);
        // With no prompt open, the not-ended line shows under the notice.
        let notice = "closing will not end sessions: there is no backend on this computer";
        let ended = "2 sessions could not be ended and are still running";
        let (_, rows, whole) = nav_pinned_rows(None, Some(notice), Some(ended), 30, 20);
        assert_eq!((rows.join(" "), whole), (format!("{notice} {ended}"), true));
        // Nothing pinned: the list keeps the pane.
        assert_eq!(nav_pinned_rows(None, None, None, 30, 20), (20, vec![], false));
        assert_eq!(nav_pinned_rows(prompt, None, None, 30, 0), (0, vec![], false));
    }

    #[test]
    fn pinned_lines_are_whole_or_dropped() {
        let (text, choice) = quit_prompt_line(false);
        let prompt = Some((text.as_str(), choice.as_str()));
        let notice = "closing will not end sessions: there is no backend on this computer";
        let ended = "2 sessions could not be ended and are still running";
        let own = nav_pinned_rows(prompt, None, None, 30, 20).1;
        let flat = own.join(" ");
        // A height that fits all of them: every line whole, and the count is acked.
        let (list_h, rows, whole) = nav_pinned_rows(prompt, Some(notice), Some(ended), 30, 20);
        assert_eq!(rows.join(" "), format!("{notice} {ended} {flat}"));
        assert_eq!(list_h + rows.len(), 20);
        assert!(whole);
        // Height 1: the choice, whole, and no ack.
        let (list_h, rows, whole) = nav_pinned_rows(prompt, None, Some(ended), 30, 1);
        assert_eq!(list_h, 0);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].contains("[No]  Yes"), "{rows:?}");
        assert!(!whole);
        let (ktext, kchoice) = quit_prompt_line(true);
        let (_, rows, whole) = nav_pinned_rows(Some((&ktext, &kchoice)), None, Some(ended), 30, 1);
        assert!(rows.len() == 1 && rows[0].contains("No  [Yes]"), "{rows:?}");
        assert!(!whole);
        // Room for the prompt and the count only: the notice goes, whole.
        let h = own.len() + wrap_status(ended, 30, 30).len();
        let (_, rows, whole) = nav_pinned_rows(prompt, Some(notice), Some(ended), 30, h);
        assert_eq!((rows.join(" "), whole), (format!("{ended} {flat}"), true));
        // One row less: the count goes whole, never leaving its tail.
        let (_, rows, whole) = nav_pinned_rows(prompt, Some(notice), Some(ended), 30, h - 1);
        assert_eq!((rows.join(" "), whole), (flat.clone(), false));
    }

    #[test]
    fn quit_choice_is_whole_on_the_last_row() {
        // No room left on the last wrapped row: the choice has its own row.
        let rows = nav_pinned_rows(Some(("aaaa bbbb", "[No]  Yes")), None, None, 9, 20).1;
        assert_eq!(rows.last().map(String::as_str), Some("[No]  Yes"));
        assert_eq!(rows.len(), 2);
        // Room on the last row: the choice joins it after three spaces.
        let rows = nav_pinned_rows(Some(("aa", "[No]  Yes")), None, None, 20, 20).1;
        assert_eq!(rows, vec!["aa   [No]  Yes".to_string()]);
        // A short pane keeps the last row, which is the whole choice.
        let rows = nav_pinned_rows(Some(("aaaa bbbb", "[No]  Yes")), None, None, 9, 1).1;
        assert_eq!(rows, vec!["[No]  Yes".to_string()]);
    }

    #[test]
    fn narrow_pane_never_cuts_the_choice() {
        // A pane narrower than the choice's 9 columns drops the choice row
        // whole; the prompt's text still shows.
        for keep in [false, true] {
            let (_, choice) = quit_prompt_line(keep);
            let rows = nav_pinned_rows(Some(("aaaa bbbb", choice.as_str())), None, None, 8, 20).1;
            assert_eq!(rows, vec!["aaaa".to_string(), "bbbb".to_string()]);
            let rows = nav_pinned_rows(Some(("aaaa bbbb", choice.as_str())), None, None, 8, 1).1;
            assert_eq!(rows, vec!["bbbb".to_string()]);
        }
    }

    #[test]
    fn middle_truncate_keeps_short_strings() {
        assert_eq!(middle_truncate("short.jl", 20), "short.jl");
        assert_eq!(middle_truncate("exact", 5), "exact");
    }

    #[test]
    fn middle_truncate_preserves_extension_in_tail() {
        let out = middle_truncate("src/very/long/path/MyLongModuleName.jl", 18);
        assert_eq!(out.chars().count(), 18);
        assert!(out.contains('…'));
        // tail-biased: the extension survives the cut
        assert!(out.ends_with(".jl"), "got {out:?}");
        assert!(out.starts_with("src/"), "got {out:?}");
    }

    #[test]
    fn middle_truncate_degenerate_widths() {
        assert_eq!(middle_truncate("abcdef", 3), "abc");
        assert_eq!(middle_truncate("abcdef", 1), "a");
        assert_eq!(middle_truncate("abcdef", 0), "");
    }

    #[test]
    fn version_label_shows_both_halves_even_when_they_match() {
        // Always-both: the matching case must still print `be`, otherwise
        // the field you're watching is invisible exactly when it's healthy.
        let (s, skew) = version_label("0.5.8", Some("0.5.8"));
        assert_eq!(s, " fe 0.5.8 · be 0.5.8 ");
        assert!(!skew);
    }

    #[test]
    fn version_label_flags_skew() {
        let (s, skew) = version_label("0.5.8", Some("0.5.7"));
        assert_eq!(s, " fe 0.5.8 · be 0.5.7 ");
        assert!(skew);
        // Dev builds differ only in the sha — the common real-world skew,
        // since both sides carry the same `0.5.8-dev` prefix.
        let (_, skew) = version_label("0.5.8-dev+aaaaaaa", Some("0.5.8-dev+bbbbbbb"));
        assert!(skew);
    }

    #[test]
    fn version_label_unknown_backend_is_not_skew() {
        // Pre-hello / pre-versioning daemon. Unknown != different: colouring
        // this as a mismatch would cry wolf on every launch.
        let (s, skew) = version_label("0.5.8", None);
        assert_eq!(s, " fe 0.5.8 · be ? ");
        assert!(!skew);
    }
}
