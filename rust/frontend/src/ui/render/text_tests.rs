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

/// A run is the extent of the flagged glyphs of one stretch, whichever way the line runs (the glyphs of a right-to-left
/// paragraph descend in x, and a name's glyphs can split into two stretches around text that is not flagged), and it leaves
/// out the blanks that end a stretch: the space that ends a wrapped line, which can hang past the width, carries no rectangle.
#[test]
fn a_flagged_run_covers_its_glyphs_in_a_right_to_left_line_and_leaves_out_a_trailing_blank() {
    let mut fonts = FontSystem::new();
    let flag = Attrs::new().metadata(1);
    let plain = Attrs::new();
    // The runs of a text shaped at `width`, each checked against the glyphs of its layout line: it has a width, its edges are the
    // edges of flagged glyphs that are not blank, it holds every one of the stretch's glyphs, and it holds no glyph that is not
    // flagged. Every flagged glyph that is not blank lies in a run. The runs and the number of lines that hold flagged glyphs.
    let check = |fonts: &mut FontSystem, width: f32, spans: &[(&str, Attrs)]| -> (usize, usize) {
        let mut buffer = Buffer::new(fonts, Metrics::new(14.0, 18.0));
        buffer.set_size(fonts, Some(width), None);
        set_buffer_rich_text(&mut buffer, fonts, spans.iter().copied(), Attrs::new(), Shaping::Advanced);
        buffer.shape_until_scroll(fonts, false);
        let runs = flagged_runs(&buffer, |m| m & 1 != 0);
        let (mut found, mut lines) = (0, 0);
        for line in buffer.layout_runs() {
            let solid = |g: &&cosmic_text::LayoutGlyph| &line.text[g.start..g.end] != " ";
            let flagged: Vec<_> = line.glyphs.iter().filter(|g| g.metadata & 1 != 0).filter(solid).collect();
            let mine: Vec<_> = runs.iter().filter(|r| r.1 == line.line_top).collect();
            lines += usize::from(!flagged.is_empty());
            found += mine.len();
            for r in &mine {
                let (x0, x1) = (r.0, r.0 + r.2);
                assert!(r.2 > 0.0, "{spans:?}: {r:?}");
                assert!(flagged.iter().any(|g| (g.x - x0).abs() < 0.01) && flagged.iter().any(|g| (g.x + g.w - x1).abs() < 0.01), "{spans:?}: {r:?} is not bounded by glyphs");
                assert!(line.glyphs.iter().filter(solid).filter(|g| g.metadata & 1 == 0).all(|g| g.x + g.w / 2.0 < x0 || g.x + g.w / 2.0 > x1), "{spans:?}: {r:?} holds a glyph that is not flagged");
            }
            assert!(flagged.iter().all(|g| mine.iter().any(|r| g.x >= r.0 - 0.01 && g.x + g.w <= r.0 + r.2 + 0.01)), "{spans:?}: a flagged glyph is in no run: {mine:?}");
        }
        assert_eq!(found, runs.len(), "{spans:?}");
        (runs.len(), lines)
    };
    // Right to left: the paragraph's first strong character is Hebrew, so the glyphs descend in x, and the `@` of `@ada` (a
    // neutral between the two) lies right of ` x` while `ada` lies left of it: two runs around the text between.
    assert_eq!(check(&mut fonts, 300.0, &[("שלום ", plain), ("@ada", flag), (" x", plain)]), (2, 1));
    assert_eq!(check(&mut fonts, 300.0, &[("שלום ", plain), ("@ada", flag)]).0, 1);
    assert_eq!(check(&mut fonts, 300.0, &[("hi ", plain), ("@דוד", flag), (" first", plain)]).0, 1);
    assert_eq!(check(&mut fonts, 300.0, &[("דוד ", plain), ("@ada", flag), (" x", plain)]).0, 2);
    // Blanks that end a stretch, at a line's end or before text that is not flagged, carry no rectangle.
    assert_eq!(check(&mut fonts, 300.0, &[("ab  ", flag), ("\ncd", plain)]), (1, 1));
    assert_eq!(check(&mut fonts, 300.0, &[("ab  ", flag), ("cd", plain)]), (1, 1));
    assert_eq!(check(&mut fonts, 300.0, &[("a b", flag), ("  ", flag)]), (1, 1));
    // A flagged line that wraps ends in a space; the run stops at the last word, whether or not the space fits the width.
    for width in [60.0, 70.0, 80.0, 90.0, 100.0, 110.0, 120.0] {
        let (runs, lines) = check(&mut fonts, width, &[("alpha beta gamma delta epsilon zeta eta theta", flag)]);
        assert!(runs >= 2 && runs == lines, "{width}: {runs} runs on {lines} lines");
    }
}
