use super::*;

fn plain(rows: &Rows) -> Vec<String> {
    rows.0.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
}

fn lines(text: &[&str]) -> Vec<String> {
    text.iter().map(|s| s.to_string()).collect()
}

/// Plain lines in no style, as `uniform` makes them.
fn plain_rows(text: &[&str], cols: usize, prefix: Option<Span<'static>>, hang: usize) -> Rows {
    rows(&uniform(&lines(text), Style::default(), prefix, hang), cols)
}

#[test]
fn a_line_is_cut_at_the_width_and_nothing_is_lost() {
    let r = plain_rows(&["abcdefghij", "", "xy"], 4, None, 0);
    assert_eq!(plain(&r), ["abcd", "efgh", "ij", "", "xy"]);
    assert_eq!(r.1, [0, 3, 4], "each logical line's first row");
    // Spaces stay: a line with them is cut at the edge, not at a word.
    assert_eq!(plain(&plain_rows(&["ab  cd  ef"], 5, None, 0)), ["ab  c", "d  ef"]);
}

#[test]
fn a_wide_glyph_or_a_tab_at_the_edge_is_neither_split_nor_lost() {
    let wide = plain_rows(&["ab中cd"], 3, None, 0);
    assert_eq!(plain(&wide), ["ab", "中c", "d"]);
    // A glyph wider than the pane still takes a row, so the cut always advances.
    assert_eq!(plain(&plain_rows(&["中中"], 1, None, 0)), ["中", "中"]);
    // A tab is already a space by the cell rule, and a space at the edge starts the next row.
    let tab = plain_rows(&[&crate::ui::cell_lines("ab\tcd").concat()], 3, None, 0);
    assert_eq!(plain(&tab).concat(), "ab cd");
}

#[test]
fn width_zero_returns_the_text_whole() {
    assert_eq!(plain(&plain_rows(&["abcdefghij"], 0, None, 0)), ["abcdefghij"]);
}

#[test]
fn the_prefix_takes_the_first_row_and_the_hang_the_rest() {
    let prompt = Some(Span::raw("julia> "));
    let r = plain_rows(&["abcdefghij", "kl"], 10, prompt, 7);
    assert_eq!(plain(&r), ["julia> abc", "       def", "       ghi", "       j", "       kl"]);
    let label = Some(Span::raw("ERROR: "));
    assert_eq!(plain(&plain_rows(&["abcdefghijkl"], 10, label, 0)), ["ERROR: abc", "defghijkl"]);
}

#[test]
fn a_frame_changing_nothing_is_not_wrapped_again() {
    let key = (("h".to_string(), "r".to_string()), 1, 0);
    let (mut wrap, mut made) = (Wrap { cols: 5, ..Wrap::default() }, 0);
    let mut ask = |wrap: &mut Wrap, len: usize| {
        wrap.begin();
        wrap.frame(key.clone(), len, |c| { made += 1; (plain_rows(&["abcdefghij"], c, None, 0), Sgr::default()) });
        wrap.end();
    };
    for _ in 0..3 {
        ask(&mut wrap, 10);
    }
    ask(&mut wrap, 11);
    wrap.cols = 4;
    ask(&mut wrap, 11);
    assert_eq!(made, 3, "the same width and length reuse the rows; more text, or another width, wraps again");
    wrap.begin();
    wrap.end();
    assert!(wrap.frames.is_empty(), "a frame no longer in the log is dropped");
}

#[test]
fn a_run_keeps_its_style_across_a_cut() {
    let red = Style::default().fg(Color::Red);
    let line = Logical {
        runs: vec![("ab".to_string(), Style::default()), ("cdef".to_string(), red), ("g".to_string(), Style::default())],
        head: None, indent: 0,
    };
    let r = rows(&[line], 3);
    let spans: Vec<Vec<(String, Option<Color>)>> =
        r.0.iter().map(|l| l.spans.iter().map(|s| (s.content.to_string(), s.style.fg)).collect()).collect();
    assert_eq!(spans[0], [("ab".into(), None), ("c".into(), Some(Color::Red))]);
    assert_eq!(spans[1], [("def".into(), Some(Color::Red))]);
    assert_eq!(spans[2], [("g".into(), None)]);
}

#[test]
fn a_logical_line_with_no_head_has_the_whole_width_and_its_continuation_rows_an_indent() {
    let line = Logical { runs: vec![("    at abcdefgh".to_string(), Style::default())], head: None, indent: 4 };
    assert_eq!(plain(&rows(&[line], 8)), ["    at a", "    bcde", "    fgh"]);
}

#[test]
fn a_cluster_is_never_cut_and_no_row_overfills_the_pane_as_it_is_drawn() {
    // A heart with its emoji selector (0.1 counts 1 cell, the buffer draws 2), a family and a flag joined by ZWJ and
    // regional indicators, a keycap: measured and segmented as ratatui's buffer draws them.
    let samples = ["ab\u{2764}\u{FE0F}cd", "x\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}y", "a\u{1F1FA}\u{1F1F8}b", "1\u{FE0F}\u{20E3}2", "e\u{301}e\u{301}"];
    let clusters = |s: &str| Span::raw(s.to_string()).styled_graphemes(Style::default()).count();
    for sample in samples {
        for cols in 2..=12 {
            let rows = plain(&rows_of(&[sample], cols));
            assert_eq!(rows.concat(), sample, "{sample:?} at {cols}: nothing lost");
            assert_eq!(rows.iter().map(|r| clusters(r)).sum::<usize>(), clusters(sample), "{sample:?} at {cols}: no cluster cut: {rows:?}");
            for row in &rows {
                assert!(Span::raw(row.clone()).width() <= cols || clusters(row) == 1, "{sample:?} at {cols}: {row:?} overfills");
            }
        }
    }
    // The heart takes the two cells the buffer gives it: it does not fit beside `ab` in 3 cells.
    assert_eq!(plain(&rows_of(&["ab\u{2764}\u{FE0F}cd"], 3)), ["ab", "\u{2764}\u{FE0F}c", "d"]);
    assert_eq!(plain(&rows_of(&["a\u{1F1FA}\u{1F1F8}b"], 2)), ["a", "\u{1F1FA}\u{1F1F8}", "b"]);
}

fn rows_of(text: &[&str], cols: usize) -> Rows {
    plain_rows(text, cols, None, 0)
}

#[test]
fn a_newline_in_a_line_with_wide_text_is_cut_not_a_panic() {
    // No caller passes one (`cell_lines` ends rows at it), but the cut must not trust that: the segmentation skips a
    // newline, so the offsets are read from the text itself.
    for width in 1..=6 {
        let r = rows_of(&["ab\ncd中ef"], width);
        assert_eq!(plain(&r).concat(), "ab\ncd中ef", "{width}");
    }
}

#[test]
fn a_cut_never_loops_on_a_trailing_newline_and_never_splits_a_mark_from_its_letter() {
    // A text ending in a newline the segmentation drops: the last cut must end the line, not ask for more rows forever.
    for width in 1..=5 {
        assert_eq!(plain(&rows_of(&["ab\n", "abc中\n"], width)).concat().replace('\n', ""), "ab".to_string() + "abc中");
    }
    // The ASCII fast path must not cut between a letter and the mark that follows it.
    assert_eq!(plain(&rows_of(&["abc\u{301}d"], 3)), ["abc\u{301}", "d"], "the mark stays with its letter");
    assert_eq!(plain(&rows_of(&["abc\u{301}d"], 2)), ["ab", "c\u{301}d"]);
    assert_eq!(plain(&rows_of(&["abcd"], 3)), ["abc", "d"]);
}

#[test]
fn cutting_a_long_line_reads_the_line_once() {
    // A megabyte in one logical line is cut into rows in time that does not grow with the number of rows: each cut reads
    // only what fits its row.
    let (ascii, mixed) = ("x".repeat(1 << 20), format!("é{}", "x".repeat(1 << 20)));
    let start = std::time::Instant::now();
    for text in [&ascii, &mixed] {
        assert_eq!(plain(&rows_of(&[text], 20)).concat(), *text);
    }
    assert!(start.elapsed() < std::time::Duration::from_secs(3), "{:?}", start.elapsed());
}
