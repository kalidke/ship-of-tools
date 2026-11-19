use super::{open_tail, pieces, strip_escapes, Piece, Piece::*, MAX_SEQUENCE};
use std::time::{Duration, Instant};

/// The cases `julia/repl/test/escape_cases.tsv` holds.
fn cases() -> Vec<(String, String, String)> {
    parse(include_str!("../../../julia/repl/test/escape_cases.tsv"))
}

/// The rows of a case table: the text, what is left of it when its whole sequences are taken out, and the tail a stream
/// holds back (`open_tail`).
fn parse(table: &str) -> Vec<(String, String, String)> {
    fn unescape(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            match (c, if c == '\\' { chars.next() } else { None }) {
                ('\\', Some('e')) => out.push('\u{1b}'),
                ('\\', Some('a')) => out.push('\u{7}'),
                ('\\', Some('t')) => out.push('\t'),
                ('\\', Some('\\')) => out.push('\\'),
                ('\\', other) => panic!("unknown escape \\{other:?}"),
                (c, _) => out.push(c),
            }
        }
        out
    }
    table
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let mut cols = l.split('\t').map(unescape);
            (cols.next().unwrap_or_default(), cols.next().unwrap_or_default(), cols.next().unwrap_or_default())
        })
        .collect()
}

#[test]
fn the_shared_cases_strip_and_split_as_the_table_says() {
    let cases = cases();
    assert!(cases.len() > 20);
    for (input, stripped, held) in cases {
        assert_eq!(strip_escapes(&input), stripped, "{input:?}");
        assert_eq!(open_tail(&input), input.len() - held.len(), "{input:?}: the held tail is where the table says");
        if !held.is_empty() {
            // Read whole, the held tail is data: what is left is the rest stripped, and the tail as it is.
            let rest = &input[..input.len() - held.len()];
            assert_eq!(strip_escapes(&input), format!("{}{held}", strip_escapes(rest)), "{input:?}");
            assert!(pieces(&held).iter().all(|p| matches!(p, Text(_))), "{held:?} is no whole sequence");
        }
    }
}

#[test]
fn colors_cursor_moves_titles_and_esc_forms_are_removed_whole() {
    assert_eq!(strip_escapes("\u{1b}[31mred\u{1b}[39m and \u{1b}[1;38;5;208mx\u{1b}[0m"), "red and x");
    assert_eq!(strip_escapes("t\u{1b}(Bu\u{1b}7v\u{1b}cw\u{1b}P1$q\u{1b}\\x\u{1b}Xs\u{1b}\\y\u{1b}^p\u{1b}\\z\u{1b}_a\u{1b}\\"), "tuvwxyz");
}

#[test]
fn pieces_name_the_text_and_each_whole_sequence_in_order() {
    assert_eq!(
        pieces("a\u{1b}[1;31mb\u{1b}[0m\u{1b}]0;t\u{7}c\u{1b}d"),
        [Text("a"), Csi { params: "1;31", final_byte: 'm' }, Text("b"), Csi { params: "0", final_byte: 'm' }, Other, Text("c"), Other]
    );
    assert_eq!(pieces("\u{1b}[2K\u{1b}[1A"), [Csi { params: "2", final_byte: 'K' }, Csi { params: "1", final_byte: 'A' }]);
    assert_eq!(pieces(""), Vec::<Piece>::new());
    assert_eq!(pieces("x\u{1b}[31"), [Text("x\u{1b}[31")], "a cut-off sequence is text");
    assert_eq!(pieces("a\u{1b}\u{1}b"), [Text("a\u{1b}\u{1}b")], "ESC before a control character is data");
}

#[test]
fn a_sequence_longer_than_the_bound_is_data_from_its_esc_on() {
    let fits = format!("\u{1b}]8;;{}\u{7}", "u".repeat(MAX_SEQUENCE - 6));
    assert_eq!(fits.len(), MAX_SEQUENCE);
    assert_eq!(strip_escapes(&format!("a{fits}b")), "ab");
    let too_long = format!("\u{1b}]8;;{}\u{7}", "u".repeat(MAX_SEQUENCE - 5));
    assert_eq!(strip_escapes(&format!("a{too_long}b")), format!("a{too_long}b"));
    // The same for a CSI, and for a string with no terminator at all: the text after it is still read.
    let long_csi = format!("\u{1b}[{}m", "1".repeat(MAX_SEQUENCE));
    assert_eq!(strip_escapes(&format!("{long_csi}\u{1b}[0mz")), format!("{long_csi}z"));
    assert_eq!(strip_escapes(&format!("\u{1b}]{}\u{1b}[0mz", "o".repeat(10))), format!("\u{1b}]{}z", "o".repeat(10)));
}

/// Every scan reads the text once: these inputs took a quadratic scan minutes. The ceiling is wall time, generous for a
/// loaded machine and a debug build, and a hundred times under what the quadratic code took.
#[test]
fn a_megabyte_of_openers_is_read_in_bounded_time() {
    let mib = 1 << 20;
    for (name, text) in [
        ("OSC openers", "\u{1b}]".repeat(mib / 2)),
        ("DCS openers", "\u{1b}P".repeat(mib / 2)),
        ("CSI openers", "\u{1b}[".repeat(mib / 2)),
        ("nF openers", "\u{1b}(".repeat(mib / 2)),
        ("long CSI runs", format!("\u{1b}[{}", "1".repeat(MAX_SEQUENCE + 1)).repeat(mib / (MAX_SEQUENCE + 3))),
        ("openers and BELs far apart", format!("{}\u{7}", "\u{1b}]x".repeat(mib / 3))),
        ("openers and an ST far off", format!("{}\u{1b}\\", "\u{1b}]x".repeat(mib / 3))),
    ] {
        let start = Instant::now();
        let kept = strip_escapes(&text).len();
        assert!(start.elapsed() < Duration::from_secs(3), "{name}: {:?}", start.elapsed());
        assert!(kept <= text.len());
    }
}

fn escaped(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\u{1b}', "\\e").replace('\u{7}', "\\a").replace('\t', "\\t")
}

/// Random texts of the characters that make and break sequences, the same on every run (a fixed seed), with the strip and
/// the held tail the grammar gives each: `julia/repl/test/escape_random.tsv`.
fn generated() -> String {
    let parts = [
        "\u{1b}", "\u{1b}[", "\u{1b}]", "\u{1b}P", "\u{1b}X", "\u{1b}^", "\u{1b}_", "\u{1b}\\", "\u{7}", "[", "]", "\\", "(", ")",
        "0", "1", ";", "m", "?", "$", " ", "a", "b", "\u{e9}", "\u{6587}", "@", "7", "c", "\t",
    ];
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    };
    let mut out = String::from(
        "# Random cases for the terminal-escape grammar: the same columns as escape_cases.tsv. Made by\n\
         # `generated` in rust/protocol/src/escapes_tests.rs (fixed seed; SOT_ESCAPE_REGEN=1 rewrites this file).\n",
    );
    for _ in 0..3000 {
        let text: String = (0..1 + next(14)).map(|_| parts[next(parts.len())]).collect();
        let tail = &text[open_tail(&text)..];
        out += &format!("{}\t{}\t{}\n", escaped(&text), escaped(&strip_escapes(&text)), escaped(tail));
    }
    out
}

#[test]
fn the_random_cases_are_what_the_grammar_gives_and_cutting_at_the_held_tail_never_splits_a_sequence() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../julia/repl/test/escape_random.tsv");
    let made = generated();
    if std::env::var_os("SOT_ESCAPE_REGEN").is_some() {
        std::fs::write(&path, &made).unwrap();
    }
    assert!(std::fs::read_to_string(&path).unwrap_or_default() == made, "escape_random.tsv is stale: SOT_ESCAPE_REGEN=1 cargo test -p sot-protocol escapes");
    for (text, stripped, held) in parse(&made) {
        assert_eq!(strip_escapes(&text), stripped);
        assert_eq!(held, text[open_tail(&text)..]);
        // A stream cut anywhere: each chunk is sent up to its held tail and the tail goes with the next chunk, so the
        // sequences are the whole text's, and the stripped chunks join into the whole text's strip.
        for cut in (0..=text.len()).filter(|&c| text.is_char_boundary(c)) {
            let first = open_tail(&text[..cut]);
            assert!(first <= cut);
            let joined = format!("{}{}", strip_escapes(&text[..first]), strip_escapes(&text[first..]));
            assert_eq!(joined, stripped, "{text:?} cut at {cut}");
            // The held tail is still whole-or-unfinished from its own start: no sequence lies across `first`.
            assert!(pieces(&text[..first]).iter().all(|p| !matches!(p, Text(t) if t.contains('\u{1b}') && t.len() > MAX_SEQUENCE)));
        }
    }
}

#[test]
fn what_a_chunk_ends_in_the_middle_of_is_held_only_while_it_could_still_be_finished() {
    assert_eq!(open_tail("abc"), 3);
    assert_eq!(open_tail("a\u{1b}"), 1, "an ESC alone");
    assert_eq!(open_tail("a\u{1b}[1;"), 1, "a CSI without its final byte");
    assert_eq!(open_tail("a\u{1b}]0;t\u{1b}"), 1, "a string whose ST has begun");
    assert_eq!(open_tail("a\u{1b}]0;t\u{7}b"), 8, "a whole OSC is no tail");
    // An opener inside a whole string is the string's, not a tail of its own.
    let inner = "a\u{1b}]0;t\u{1b}Xu\u{7}";
    assert_eq!(open_tail(inner), inner.len());
    // A string with no end holds what follows it, up to the bound, and then it is data and nothing is held.
    let body = "x".repeat(MAX_SEQUENCE - 3);
    assert_eq!(open_tail(&format!("\u{1b}X{body}")), 0, "one byte short of the bound: a terminator could still end it");
    assert_eq!(open_tail(&format!("\u{1b}X{body}y")), MAX_SEQUENCE, "at the bound no terminator fits: data");
    // A later opener inside the unfinished tail is held with it, from the first.
    assert_eq!(open_tail("p\u{1b}Xabc\u{1b}[3"), 1);
}
