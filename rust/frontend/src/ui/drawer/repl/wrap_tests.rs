use super::*;

fn plain(rows: &Rows) -> Vec<String> {
    rows.0.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
}

fn lines(text: &[&str]) -> Vec<String> {
    text.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_line_is_cut_at_the_width_and_nothing_is_lost() {
    let r = rows(&lines(&["abcdefghij", "", "xy"]), Style::default(), 4, None, 0);
    assert_eq!(plain(&r), ["abcd", "efgh", "ij", "", "xy"]);
    assert_eq!(r.1, [0, 3, 4], "each logical line's first row");
    // Spaces stay: a line with them is cut at the edge, not at a word.
    assert_eq!(plain(&rows(&lines(&["ab  cd  ef"]), Style::default(), 5, None, 0)), ["ab  c", "d  ef"]);
}

#[test]
fn a_wide_glyph_or_a_tab_at_the_edge_is_neither_split_nor_lost() {
    let wide = rows(&lines(&["ab中cd"]), Style::default(), 3, None, 0);
    assert_eq!(plain(&wide), ["ab", "中c", "d"]);
    // A glyph wider than the pane still takes a row, so the cut always advances.
    assert_eq!(plain(&rows(&lines(&["中中"]), Style::default(), 1, None, 0)), ["中", "中"]);
    // A tab is already a space by the cell rule, and a space at the edge starts the next row.
    let tab = rows(&crate::ui::cell_lines("ab\tcd"), Style::default(), 3, None, 0);
    assert_eq!(plain(&tab).concat(), "ab cd");
}

#[test]
fn width_zero_returns_the_text_whole() {
    assert_eq!(plain(&rows(&lines(&["abcdefghij"]), Style::default(), 0, None, 0)), ["abcdefghij"]);
}

#[test]
fn the_prefix_takes_the_first_row_and_the_hang_the_rest() {
    let prompt = Some(Span::raw("julia> "));
    let r = rows(&lines(&["abcdefghij", "kl"]), Style::default(), 10, prompt, 7);
    assert_eq!(plain(&r), ["julia> abc", "       def", "       ghi", "       j", "       kl"]);
    let label = Some(Span::raw("ERROR: "));
    assert_eq!(plain(&rows(&lines(&["abcdefghijkl"]), Style::default(), 10, label, 0)), ["ERROR: abc", "defghijkl"]);
}

#[test]
fn a_frame_changing_nothing_is_not_wrapped_again() {
    let key = (("h".to_string(), "r".to_string()), 1, 0);
    let (mut wrap, mut made) = (Wrap { cols: 5, ..Wrap::default() }, 0);
    for _ in 0..3 {
        wrap.begin();
        wrap.frame(key.clone(), 10, |c| { made += 1; rows(&lines(&["abcdefghij"]), Style::default(), c, None, 0) });
        wrap.end();
    }
    assert_eq!(made, 1, "the same width and length reuse the rows");
    wrap.begin();
    wrap.frame(key.clone(), 11, |c| { made += 1; rows(&lines(&["abcdefghijk"]), Style::default(), c, None, 0) });
    wrap.end();
    wrap.cols = 4;
    wrap.begin();
    wrap.frame(key.clone(), 11, |c| { made += 1; rows(&lines(&["abcdefghijk"]), Style::default(), c, None, 0) });
    wrap.end();
    assert_eq!(made, 3, "more text, or another width, wraps again");
    wrap.begin();
    wrap.end();
    assert!(wrap.frames.is_empty(), "a frame no longer in the log is dropped");
}
