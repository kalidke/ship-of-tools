use super::*;

#[test]
fn hostile_text_is_refused_and_plain_text_is_marked() {
    for text in ["", "   ", "a\nb", "a\rb", "\u{1b}[201~x", "\u{1b}", "\u{3}", "\u{7f}", "\u{85}",
        "\u{2028}", "\u{2029}", "\u{202e}", "\u{2066}", "\u{200b}", "\u{feff}"] {
        assert!(line(text).is_err(), "accepted {text:?}");
    }
    // Every member of each forbidden range, including C0, DEL and C1.
    for code in (0..=0x1f).chain(0x7f..=0x9f).chain(0x200b..=0x200f).chain(0x2028..=0x202e)
        .chain(0x2060..=0x2069).chain(0xfff9..=0xfffb).chain([0xfeff, 0xfffe, 0xffff]) {
        // U+202F is intentionally outside the ranges above.
        let c = char::from_u32(code).unwrap();
        let error = line(&format!("a{c}b")).expect_err("unsafe character");
        assert!(error.to_string().contains(&format!("U+{code:04X}")));
        assert!(check_line(&format!("[sot-page] a{c}b")).is_err());
    }
    assert!(line(&"a".repeat(513)).is_err());
    assert!(line(&"é".repeat(257)).is_err(), "the limit counts bytes");
    for (text, want) in [
        ("(a) approve — merge it", "[sot-page] (a) approve — merge it"),
        ("/clear", "[sot-page] /clear"), ("  ok  ", "[sot-page] ok"),
        ("!echo", "[sot-page] !echo"), ("#remember", "[sot-page] #remember"),
    ] {
        assert_eq!(line(text).unwrap(), want);
        assert!(check_line(want).is_ok());
    }
    for text in ["a".repeat(512), "é".repeat(256)] {
        assert!(check_line(&line(&text).unwrap()).is_ok());
    }
    for offered in ["approve", "[sot-page] ", "[sot-page]  ok", "[sot-page] ok ", "[sot-page] a\nb"] {
        assert!(check_line(offered).is_err(), "accepted {offered:?}");
    }
}
