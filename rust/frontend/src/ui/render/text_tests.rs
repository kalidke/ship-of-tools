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
