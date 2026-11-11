use super::*;

/// Each bidi paragraph separator, and the line separator and non-characters of earlier findings, between
/// left-to-right and right-to-left text: no line cosmic-text shapes holds two paragraphs, so none panics, and `\n`
/// and `\r` still end a line. Then the list itself: every character on which cosmic-text's own paragraph split
/// (`BidiParagraphs`, Unicode's bidi class B) splits is `\n`, `\r` or one of `PARAGRAPH_SEPARATORS`.
#[test]
fn the_shaper_gets_one_bidi_paragraph_per_line() {
    let mut fonts = FontSystem::new();
    // (the character between `AA` and `אב`, the buffer's lines)
    let cases: &[(char, &[&str])] = &[
        ('\n', &["AA", "אב"]),
        ('\r', &["AA", "אב"]),
        ('\u{1c}', &["AA אב"]),
        ('\u{1d}', &["AA אב"]),
        ('\u{1e}', &["AA אב"]),
        ('\u{85}', &["AA אב"]),
        ('\u{2029}', &["AA אב"]),
        ('\u{2028}', &["AA\u{2028}אב"]),
        ('\u{fffe}', &["AA\u{fffe}אב"]),
        ('\u{ffff}', &["AA\u{ffff}אב"]),
    ];
    for &(c, want) in cases {
        let text = format!("AA{c}אב");
        let mut plain = Buffer::new(&mut fonts, Metrics::new(14.0, 18.0));
        set_buffer_text(&mut plain, &mut fonts, &text, Attrs::new(), Shaping::Advanced);
        let mut rich = Buffer::new(&mut fonts, Metrics::new(14.0, 18.0));
        let spans = [("AA", Attrs::new()), (&text[2..], Attrs::new().weight(Weight::BOLD))];
        set_buffer_rich_text(&mut rich, &mut fonts, spans, Attrs::new(), Shaping::Advanced);
        for (how, buffer) in [("set_buffer_text", &mut plain), ("set_buffer_rich_text", &mut rich)] {
            buffer.shape_until_scroll(&mut fonts, false);
            let lines: Vec<&str> = buffer.lines.iter().map(|line| line.text()).collect();
            assert_eq!(lines, want, "{how} {c:?}");
        }
    }
    let splits: Vec<char> = (0..=0x10ffff)
        .filter_map(char::from_u32)
        .filter(|c| cosmic_text::BidiParagraphs::new(&format!("a{c}b")).count() > 1)
        .collect();
    assert_eq!(splits, ['\n', '\r', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2029}']);
}

/// The runs of flagged glyphs a renderer paints behind (inline code, strike, a chat badge) are found mid-line, at a
/// line's end (the flush after the last glyph, the case a lost flush hides) and across a wrap, one rectangle per visual
/// line, each at its line's top and height.
#[test]
fn flagged_runs_are_found_mid_line_at_a_lines_end_and_across_a_wrap() {
    let mut fonts = FontSystem::new();
    let mut buffer = Buffer::new(&mut fonts, Metrics::new(14.0, 18.0));
    buffer.set_size(&mut fonts, Some(120.0), None);
    let flag = Attrs::new().metadata(1);
    let spans = [
        ("ab ", Attrs::new()), ("XY", flag), (" cd\nab ", Attrs::new()), ("ZZ", flag),
        ("\nalpha beta gamma delta epsilon zeta", flag),
    ];
    set_buffer_rich_text(&mut buffer, &mut fonts, spans, Attrs::new(), Shaping::Advanced);
    buffer.shape_until_scroll(&mut fonts, false);
    let runs = flagged_runs(&buffer, |m| m & 1 != 0);
    let lines: Vec<_> = buffer.layout_runs().map(|r| (r.line_top, r.line_height, r.line_w)).collect();
    assert!(lines.len() >= 4, "the third line wraps: {lines:?}");
    assert_eq!(runs.len(), lines.len(), "one run per visual line that holds flagged glyphs: {runs:?}");
    let (mid, end, wrapped) = (runs[0], runs[1], &runs[2..]);
    assert!(mid.0 > 0.0 && mid.2 > 0.0 && mid.0 + mid.2 < lines[0].2, "mid-line, with text on both sides: {mid:?}");
    assert!((end.0 + end.2 - lines[1].2).abs() < 0.01 && end.2 > 0.0, "the run at a line's end reaches its end: {end:?}");
    assert_eq!((mid.1, mid.3, end.1, end.3), (lines[0].0, 18.0, lines[1].0, 18.0));
    for (run, line) in wrapped.iter().zip(&lines[2..]) {
        assert_eq!((run.0, run.1, run.3), (0.0, line.0, 18.0), "a wrapped run starts its line and fills its height");
        assert!(run.2 > 0.0 && run.2 <= line.2 + 0.01, "{run:?} in {line:?}");
    }
    assert!(wrapped.len() >= 2 && wrapped[0].1 < wrapped[1].1, "the run continues on the next visual line: {wrapped:?}");
}
