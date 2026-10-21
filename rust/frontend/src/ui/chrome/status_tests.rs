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
    let preset = crate::ui::persist::settings::LayoutPreset::default_ultrawide();
    let nav = |slot| crate::ui::chrome::layout::compute(area, &preset, false, slot).rect_for(crate::ui::persist::settings::Slot::Nav);
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
fn cell_text_table() {
    // (text, its rows, its one row). A CSI sequence goes whole; `\r\n`, `\n`, a lone `\r`, U+0085, U+2028 and
    // U+2029 end a row, a final one starts none; any other control character is a space.
    let cases: &[(&str, &[&str], &str)] = &[
        ("", &[], ""),
        ("ready", &["ready"], "ready"),
        ("a\nb", &["a", "b"], "a ⏎ b"),
        ("a\r\nb", &["a", "b"], "a ⏎ b"),
        ("a\rb", &["a", "b"], "a ⏎ b"),
        ("a\r\r\nb", &["a", "", "b"], "a ⏎  ⏎ b"),
        ("a\n\nb", &["a", "", "b"], "a ⏎  ⏎ b"),
        ("tail\n", &["tail"], "tail"),
        ("tail\r", &["tail"], "tail"),
        ("a\tb\u{1b}[31mc", &["a bc"], "a bc"),
        ("\u{1b}[1;31mERROR\u{1b}[0m x", &["ERROR x"], "ERROR x"),
        ("a\u{1b}b", &["a b"], "a b"),
        ("a\u{1b}[", &["a ["], "a ["),
        ("a\u{85}b\u{2028}c\u{2029}d", &["a", "b", "c", "d"], "a ⏎ b ⏎ c ⏎ d"),
        ("tail\u{2029}", &["tail"], "tail"),
    ];
    for (text, rows, row) in cases {
        assert_eq!(cell_lines(text), *rows, "{text:?}");
        assert_eq!(cell_row(text), *row, "{text:?}");
    }
    assert!(
        matches!(cell_row("plain"), std::borrow::Cow::Borrowed(_)),
        "clean text is not copied"
    );
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
    let short: Vec<String> = status_spans("ready", 40, usize::MAX)
        .iter()
        .map(line_text)
        .collect();
    assert_eq!(short, ["status: ready"]);
    let text = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima";
    let lines: Vec<String> = status_spans(text, 20, usize::MAX)
        .iter()
        .map(line_text)
        .collect();
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
fn status_spans_pick_whole_lines_by_priority() {
    // (text, width, max_rows, the rows shown). Whole when they fit; past the cap the first line, the second, the
    // last, then the rest in order, each whole, but the first two cut rather than lost; never fewer than 3 rows.
    let ten = (0..10).map(|n| format!("r{n}")).collect::<Vec<_>>().join("\n");
    let (x, hint) = ("x".repeat(50), "hint ".repeat(10));
    let long = format!("head\n{x}\nmid\n{}", hint.trim_end());
    let y = "y".repeat(100);
    let cases: &[(&str, usize, usize, &[&str])] = &[
        (&ten, 40, 10, &["status: r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9"]),
        (&ten, 40, 9, &["status: r0", "r1", "r2", "r3", "r4", "r5", "r6", "… 2 lines not shown", "r9"]),
        (&ten, 40, 4, &["status: r0", "r1", "… 7 lines not shown", "r9"]),
        (&ten, 40, 0, &["status: r0", "r1", "… 8 lines not shown"]),
        (&long, 20, 6, &["status: head", &x[..20], &x[..20], &x[..10], "mid", "… 1 line not shown"]),
        (&long, 20, 4, &["status: head", &x[..20], &x[..20], "… 2 lines not shown"]),
        (&y, 20, 3, &["status: yyyyyyyyyyyy", &y[..20], "…"]),
    ];
    for (text, width, max_rows, want) in cases {
        let shown: Vec<String> = status_spans(text, *width, *max_rows).iter().map(line_text).collect();
        assert_eq!(shown, *want, "width {width}, max_rows {max_rows}");
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
