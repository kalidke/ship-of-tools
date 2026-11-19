use super::*;

fn runs(text: &str, base: Style, sgr: &mut Sgr, overwrite: bool) -> Vec<Vec<(String, Option<Color>, Modifier)>> {
    styled_lines(text, base, sgr, overwrite).into_iter()
        .map(|l| l.into_iter().map(|(t, s)| (t, s.fg, s.add_modifier)).collect()).collect()
}

fn go(text: &str) -> Vec<Vec<(String, Option<Color>, Modifier)>> {
    runs(text, Style::default(), &mut Sgr::default(), true)
}

#[test]
fn the_sixteen_colors_are_the_terminal_drawers_named_colors() {
    for (code, color) in [(31, Color::Red), (32, Color::Green), (33, Color::Yellow), (34, Color::Blue), (35, Color::Magenta),
        (36, Color::Cyan), (37, Color::Gray), (90, Color::DarkGray), (91, Color::LightRed), (92, Color::LightGreen),
        (94, Color::LightBlue), (97, Color::White)] {
        let r = go(&format!("\u{1b}[{code}mx\u{1b}[39my"));
        assert_eq!(r, [[("x".into(), Some(color), Modifier::empty()), ("y".into(), None, Modifier::empty())]], "{code}");
    }
}

#[test]
fn bold_dim_reverse_and_their_resets() {
    let r = go("\u{1b}[1;2ma\u{1b}[22mb\u{1b}[7mc\u{1b}[27md\u{1b}[1me\u{1b}[0mf");
    let mods: Vec<Modifier> = r[0].iter().map(|(_, _, m)| *m).collect();
    assert_eq!(mods, [Modifier::BOLD | Modifier::DIM, Modifier::empty(), Modifier::REVERSED, Modifier::empty(), Modifier::BOLD, Modifier::empty()]);
}

#[test]
fn indexed_and_rgb_colors_pass_through_and_the_rest_is_ignored() {
    let r = go("\u{1b}[38;5;208ma\u{1b}[38;2;1;2;3mb\u{1b}[38;5;2mc\u{1b}[4;3;9md\u{1b}[2Ae\u{1b}]0;t\u{7}f");
    let colors: Vec<Option<Color>> = r[0].iter().map(|(_, c, _)| *c).collect();
    assert_eq!(colors, [Some(Color::Indexed(208)), Some(Color::Rgb(1, 2, 3)), Some(Color::Green)]);
    assert_eq!(r[0].iter().map(|(t, ..)| t.as_str()).collect::<String>(), "abcdef", "underline, italic, a cursor move and a title leave only text");
}

#[test]
fn a_reset_or_a_default_color_returns_to_the_streams_own_style() {
    let gray = Style::default().fg(Color::Gray);
    let mut sgr = Sgr::default();
    let r = runs("\u{1b}[31mred\u{1b}[39m gray \u{1b}[32mgreen\u{1b}[m gray", gray, &mut sgr, true);
    let fg: Vec<Option<Color>> = r[0].iter().map(|(_, c, _)| *c).collect();
    assert_eq!(fg, [Some(Color::Red), Some(Color::Gray), Some(Color::Green), Some(Color::Gray)]);
}

#[test]
fn the_state_a_frame_leaves_starts_the_next_frame() {
    let mut sgr = Sgr::default();
    runs("\u{1b}[31mstart", Style::default(), &mut sgr, true);
    assert_eq!(runs("more\u{1b}[39m", Style::default(), &mut sgr, true), [[("more".into(), Some(Color::Red), Modifier::empty())]]);
    assert_eq!(runs("done", Style::default(), &mut sgr, true), [[("done".into(), None, Modifier::empty())]]);
}

#[test]
fn lines_follow_the_old_rules_for_a_terminal_stream_and_for_a_value() {
    let text = |l: Vec<Vec<(String, Option<Color>, Modifier)>>| -> Vec<String> {
        l.into_iter().map(|r| r.into_iter().map(|(t, ..)| t).collect()).collect()
    };
    // A stream: a lone CR overwrites, CRLF and LF end a line, a final newline starts none, U+2029 ends a line.
    assert_eq!(text(go("abc\rxy\r\nstdoutA\tB\u{2029}C\n\nD\n")), ["xyc", "stdoutA B", "C", "", "D"]);
    assert_eq!(text(go("")), Vec::<String>::new());
    assert_eq!(text(go("\n")), [""]);
    // A value or a message: a CR ends a line, as `cell_lines` has it.
    assert_eq!(text(runs("a\rb\r\nc", Style::default(), &mut Sgr::default(), false)), ["a", "b", "c"]);
}

#[test]
fn an_overwrite_keeps_the_style_of_what_was_written() {
    let r = go("aaaa\r\u{1b}[31mbb");
    assert_eq!(r, [[("bb".into(), Some(Color::Red), Modifier::empty()), ("aa".into(), None, Modifier::empty())]]);
}

#[test]
fn the_underline_color_and_the_itu_forms_are_read_whole() {
    // StyledStrings writes `ESC [ 58 ; 5 ; n m` and `58 ; 2 ; r ; g ; b`: neither is a code of its own. `38:2::r:g:b` has an
    // empty color-space field.
    for (params, fg, mods) in [
        ("58;5;1", None, Modifier::empty()),
        ("58;2;255;0;0", None, Modifier::empty()),
        ("58:2::255:0:0", None, Modifier::empty()),
        ("38:2::10:20:30", Some(Color::Rgb(10, 20, 30)), Modifier::empty()),
        ("38:2:10:20:30", Some(Color::Rgb(10, 20, 30)), Modifier::empty()),
        ("38:5:208", Some(Color::Indexed(208)), Modifier::empty()),
        ("1;38;5;2;58;5;1;4", Some(Color::Green), Modifier::BOLD),
        ("38;2;1;2", None, Modifier::empty()),
    ] {
        let r = go(&format!("\u{1b}[{params}mx"));
        assert_eq!(r, [[("x".into(), fg, mods)]], "{params}");
    }
}

#[test]
fn text_with_only_sgr_and_newlines_is_pushed_run_by_run() {
    // One run per style change, however long; the cell-at-a-time path is for a `\r` (a tab or a control character costs
    // its own cell only).
    let text = format!("{}\u{1b}[31m{}\u{1b}[39m{}\n", "a".repeat(10_000), "b".repeat(10_000), "c".repeat(10_000));
    let r = go(&text);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].iter().map(|(t, ..)| t.len()).collect::<Vec<_>>(), [10_000, 10_000, 10_000]);
    // A tab and a lone ESC (one before a control character) are a space each; `ESC c` is an escape form and goes whole.
    assert_eq!(go("a\tb\u{1b}\nx"), [[("a b ".into(), None, Modifier::empty())], [("x".into(), None, Modifier::empty())]]);
    assert_eq!(go("a\u{1b}cb"), [[("ab".into(), None, Modifier::empty())]]);
}

/// Characters written into cells while a reader reads `text` in one feed.
fn written(text: &str) -> usize {
    WRITTEN.with(|w| w.set(0));
    let mut reader = Reader::new(Style::default(), Sgr::default(), true);
    reader.feed(text);
    let _ = reader.finish();
    WRITTEN.with(|w| w.get())
}

/// Every function the reader honors, each with text that does it. `sample`'s match is exhaustive, so a function added to
/// `Func` does not compile here until it has a row, and the budget below holds every row to the same bound.
const ALL: [Func; 9] = [
    Func::Text, Func::Control, Func::Newline, Func::Return, Func::Column, Func::Sgr, Func::EraseEnd, Func::EraseStart, Func::EraseAll,
];

fn sample(f: Func) -> &'static str {
    match f {
        Func::Text => "\u{4e2d}x", // a wide glyph and a narrow one
        Func::Control => "\t",
        Func::Newline => "\n",
        Func::Return => "\r",
        Func::Column => "\u{1b}[1G",
        Func::Sgr => "\u{1b}[31m\u{1b}[0m",
        Func::EraseEnd => "\u{1b}[K",
        Func::EraseStart => "\u{1b}[1K",
        Func::EraseAll => "\u{1b}[2K",
    }
}

/// What the reader works for `text`, counted as cells written, moved, blanked and scanned (the reader's one test counter).
fn work(reader: &mut Reader, text: &str) -> usize {
    WRITTEN.with(|w| w.set(0));
    reader.feed(text);
    WRITTEN.with(|w| w.get())
}

#[test]
fn every_function_costs_what_its_write_was_and_never_the_row_so_far() {
    // The rule is one invariant of `Row`: an operation costs what its write was, amortized. Each function, alone and in a pair
    // with a write, repeated on a row already 1, 4 and 16 times as long, whichever way the cursor stands (at the end of text,
    // at the end of cells, at the start, after an erase): the work is at most `C` times the bytes fed, with the same `C`.
    // (An erase had cost the row up to the cursor: 449 KiB of them froze the window for 4 seconds.)
    const C: usize = 2;
    const REPEAT: usize = 200;
    let pairs: Vec<(String, String)> = ALL.iter().map(|&f| (format!("{f:?}"), sample(f).to_string())).chain(
        ALL.iter().filter(|f| !matches!(f, Func::Text | Func::Newline)).map(|&f| (format!("Text then {f:?}"), format!("x{}", sample(f)))),
    ).chain(ALL.iter().filter(|f| matches!(f, Func::EraseStart | Func::EraseAll)).map(|&f| (format!("Return, Text, {f:?}"), format!("\rx{}", sample(f))))).collect();
    let setups: [(&str, fn(usize) -> String); 4] = [
        ("cursor at the end of text", |n| "x".repeat(n)),
        ("cursor at the end of cells", |n| format!("{}\r{}", "x".repeat(n), "y".repeat(n))),
        ("cursor at the start", |n| format!("{}\r", "x".repeat(n))),
        ("after an erase", |n| format!("{}\u{1b}[2K", "x".repeat(n))),
    ];
    for (name, text) in &pairs {
        for (setup_name, setup) in setups {
            for n in [2_000, 8_000, 32_000] {
                let mut reader = Reader::new(Style::default(), Sgr::default(), true);
                reader.feed(&setup(n));
                reader.feed(text); // once, uncounted: a row becomes cells, or ends, once, and the bytes before paid for it
                let bytes = text.len() * REPEAT;
                let cost = work(&mut reader, &text.repeat(REPEAT));
                assert!(cost <= C * bytes, "{name} on a row of {n}, {setup_name}: {cost} work for {bytes} bytes");
            }
        }
    }
}

#[test]
fn a_row_costs_what_is_written_to_it_never_the_row_so_far() {
    // The same, as the reviewers' own inputs: each costs a few times its length, in one write.
    let pairs = |n: usize| "\u{1b}[31mx\u{1b}[32my".repeat(n);
    for (name, text) in [
        ("a `\\r` and 3,000 colored pairs", format!("\r{}\n", pairs(3000))),
        ("a `\\r` and 6,000 colored pairs", format!("\r{}", pairs(6000))),
        ("a bar repainted 10,000 times", (0..10_000).map(|i| format!("\rProgress: {:3}%", i % 100)).collect::<String>()),
        ("a long line overwritten from its start", format!("{}\r{}", "a".repeat(60_000), "b".repeat(60_000))),
        ("a `\\r` on every other character", "a\rb".repeat(30_000)),
        ("a long line and 100,000 erases of its start", format!("{}{}", "x".repeat(60_000), "\u{1b}[2K".repeat(100_000))),
        ("the same with `1K`", format!("{}{}", "x".repeat(60_000), "\u{1b}[1K".repeat(100_000))),
        ("an erase and a letter, over and over", "\u{1b}[2Kx".repeat(100_000)),
    ] {
        let cost = written(&text);
        assert!(cost <= 2 * text.len(), "{name}: {cost} work for {} bytes", text.len());
    }
    // And what they draw is what a terminal draws.
    let mut reader = Reader::new(Style::default(), Sgr::default(), true);
    for i in 0..1000 {
        reader.feed(&format!("\rProgress: {i:3}%"));
    }
    reader.feed("\n");
    let lines = reader.finish();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].iter().map(|(t, _)| t.as_str()).collect::<String>(), "Progress: 999%");
}

#[test]
fn a_write_ending_in_the_middle_of_a_sequence_is_read_with_the_next() {
    let mut reader = Reader::new(Style::default(), Sgr::default(), true);
    reader.feed("a\u{1b}[3");
    assert_eq!(reader.ended(), (vec![], Some(vec![("a [3".to_string(), Style::default())])), "what is held shows as text meanwhile");
    reader.feed("1mred\u{1b}[39m b");
    let lines = reader.finish();
    assert_eq!(lines[0].iter().map(|(t, s)| (t.as_str(), s.fg)).collect::<Vec<_>>(),
        [("a", None), ("red", Some(Color::Red)), (" b", None)]);
}

/// The text of a line.
fn plain(line: &[Run]) -> String {
    line.iter().map(|(t, _)| t.as_str()).collect()
}

/// What the reader's lines say once it has read `pieces` in turn and the text ends.
fn read_in(pieces: &[&str]) -> Vec<String> {
    let mut reader = Reader::new(Style::default(), Sgr::default(), true);
    for p in pieces {
        reader.feed(p);
    }
    reader.finish().iter().map(|l| plain(l)).collect()
}

#[test]
fn text_a_stray_string_opener_holds_is_still_text_with_every_line_it_ends() {
    // An unfinished string holds up to 4096 bytes, newlines among them: when the text ends there, that is text (the
    // grammar's own rule), and every line in it is a line. The five string openers and an unfinished CSI.
    for opener in ["\u{1b}]", "\u{1b}P", "\u{1b}X", "\u{1b}^", "\u{1b}_"] {
        let shown = opener.replace('\u{1b}', " ");
        // In one write, at the frame's end.
        assert_eq!(read_in(&[&format!("start {opener}x\nline 1\nline 2\n")]), [format!("start {shown}x"), "line 1".into(), "line 2".into()], "{opener:?}");
        // Across writes: the held text and then the lines.
        assert_eq!(read_in(&["start ", &format!("{opener}t\nline 1\n"), "line 2\n"]), [format!("start {shown}t"), "line 1".into(), "line 2".into()], "{opener:?}");
        // The child's own test frame, and a frame that ends in the opener.
        assert_eq!(read_in(&[&format!("{opener}never ends\nline 2\n")]), [format!("{shown}never ends"), "line 2".into()], "{opener:?}");
        assert_eq!(read_in(&["a\n", opener]), ["a".to_string(), shown.clone()], "{opener:?}");
    }
    // An unfinished CSI ends where the next write shows it is none, or where the text ends.
    assert_eq!(read_in(&["start \u{1b}[1;", "\nline 1\n"]), ["start  [1;", "line 1"]);
    assert_eq!(read_in(&["start \u{1b}[1;"]), ["start  [1;"]);
    // The lines are in the reader's `ended` too, which the drawer reads while the run is open.
    let mut reader = Reader::new(Style::default(), Sgr::default(), true);
    reader.feed("start \u{1b}]0;t\nline 1\nline 2");
    let (ended, open) = reader.ended();
    assert_eq!(ended.iter().map(|l| plain(l)).collect::<Vec<_>>(), ["start  ]0;t", "line 1"]);
    assert_eq!(open.map(|l| plain(&l)), Some("line 2".to_string()));
    // A value's text is read the same way (a frame that is one write).
    let mut sgr = Sgr::default();
    let lines = styled_lines("v1\u{1b}_x\nv2\nv3", Style::default(), &mut sgr, false);
    assert_eq!(lines.iter().map(|l| plain(l)).collect::<Vec<_>>(), ["v1 _x", "v2", "v3"]);
}

#[test]
fn an_unfinished_string_thousands_of_bytes_long_loses_no_line_in_it() {
    let body = "x".repeat(3000);
    assert_eq!(read_in(&[&format!("\u{1b}]{body}\nline 1\n")]), [format!(" ]{body}"), "line 1".to_string()]);
}

/// A tiny seeded generator, the same on every run.
fn rng(seed: u64) -> impl FnMut(usize) -> usize {
    let mut state = seed;
    move |n| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    }
}

#[test]
fn what_is_drawn_is_the_same_whatever_the_pieces_the_text_arrived_in() {
    // Seeded random texts of words, newlines, `\r`, SGR, erase in line, every opener, every terminator, tabs and wide
    // glyphs, cut at random points and fed in turn, read the same as whole: lines and styles.
    let parts = [
        "word", "ab", " ", "\n", "\r", "\r\n", "\t", "\u{1b}[31m", "\u{1b}[0m", "\u{1b}[1m", "\u{1b}[K", "\u{1b}[1K", "\u{1b}[2K", "\u{1b}[2J", "\u{1b}[1G", "\u{1b}[G",
        "\u{1b}]", "\u{1b}P", "\u{1b}X", "\u{1b}^", "\u{1b}_", "\u{1b}[", "\u{1b}", "\u{7}", "\u{1b}\\", "\u{4e2d}", "\u{e9}", "[", "1;", "m",
    ];
    let mut next = rng(0x2545_f491_4f6c_dd1d);
    for case in 0..3000 {
        let text: String = (0..1 + next(24)).map(|_| parts[next(parts.len())]).collect();
        let whole = {
            let mut r = Reader::new(Style::default(), Sgr::default(), true);
            r.feed(&text);
            r.finish()
        };
        let mut cuts: Vec<usize> = (0..next(4)).map(|_| next(text.len() + 1)).filter(|&c| text.is_char_boundary(c)).collect();
        cuts.sort_unstable();
        let mut r = Reader::new(Style::default(), Sgr::default(), true);
        let mut from = 0;
        for c in cuts.iter().copied().chain([text.len()]) {
            r.feed(&text[from..c]);
            // What it would be if the text ended at each cut is also what the whole text's prefix reads as.
            let mut prefix = Reader::new(Style::default(), Sgr::default(), true);
            prefix.feed(&text[..c]);
            let (a, b) = (r.clone().finish(), prefix.finish());
            assert!(a == b, "case {case}: {text:?} cut at {cuts:?}: {a:?} against {b:?}");
            from = c;
        }
        assert_eq!(r.finish(), whole, "case {case}: {text:?} cut at {cuts:?}");
    }
}

#[test]
fn erase_in_line_clears_what_a_shorter_repaint_would_leave() {
    // A terminal draws `9 left`; the reader left `9 leftt` when it dropped the erase.
    assert_eq!(read_in(&["\r10 left\u{1b}[K", "\r9 left\u{1b}[K\n"]), ["9 left"]);
    assert_eq!(read_in(&["\rprocessing long_file_name.jl", "\r\u{1b}[2Kdone\n"]), ["done"]);
    // Each form, at a cursor in the middle of a row.
    assert_eq!(read_in(&["abcdef\r\u{1b}[3C\n"]), ["abcdef"], "a cursor move is no erase");
    assert_eq!(read_in(&["abcdef\rxy\u{1b}[K\n"]), ["xy"], "no parameter and 0: from the cursor on");
    assert_eq!(read_in(&["abcdef\rxy\u{1b}[0K\n"]), ["xy"]);
    assert_eq!(read_in(&["abcdef\rxy\u{1b}[1K\n"]), ["   def"], "1: the start through the cursor");
    assert_eq!(read_in(&["abcdef\rxy\u{1b}[2K\n"]), ["  "], "2: the whole row, the cursor where it was");
    assert_eq!(read_in(&["abcdef\rxy\u{1b}[2Kz\n"]), ["  z"]);
    assert_eq!(read_in(&["abc\u{1b}[2Kd\n"]), ["   d"], "a row with no `\\r` has its cursor at its end");
    assert_eq!(read_in(&["abc\u{1b}[?1K\n"]), ["abc"], "another parameter is none");
    // A shrinking progress bar, as a meter sends it: the repaint, then the erase.
    let bar: Vec<String> = (0..300).rev().map(|i| format!("\rprocessing {}\u{1b}[K", "x".repeat(i % 40))).collect();
    let mut pieces: Vec<&str> = bar.iter().map(String::as_str).collect();
    pieces.push("\n");
    assert_eq!(read_in(&pieces), ["processing "]);
    // Erased cells are spaces in the base style, whatever background is selected (the window has no row width to paint a
    // background to, so a painted one would stop where the text did).
    let mut sgr = Sgr::default();
    let lines = styled_lines("abcdef\rxy\u{1b}[42m\u{1b}[1K", Style::default(), &mut sgr, true);
    assert_eq!(lines[0].iter().map(|(t, s)| (t.as_str(), s.bg)).collect::<Vec<_>>(), [("   def", None)]);
    // An erase on a row nothing was written to makes no row.
    assert_eq!(read_in(&["abc\n\u{1b}[K"]), ["abc"]);
    assert_eq!(read_in(&["abc\n\u{1b}[2K\u{1b}[1K"]), ["abc"]);
    assert_eq!(read_in(&["abc\n\r\u{1b}[K"]), ["abc", ""], "a `\\r` makes the row, the erase leaves it empty");
    // Cursor to column 1 is a `\r`: a repaint by column.
    assert_eq!(read_in(&["10 left", "\u{1b}[1G9 left\u{1b}[K\n"]), ["9 left"]);
    assert_eq!(read_in(&["a", "\u{1b}[G", "b\n"]), ["b"]);
    assert_eq!(read_in(&["10 left", "\u{1b}[0G9 left\u{1b}[K\n"]), ["9 left"], "0 is the default, column 1");
    assert_eq!(read_in(&["abc\u{1b}[2Gd\n"]), ["abcd"], "another column is no move");
}

#[test]
fn a_single_line_reads_as_the_terminal_emulator_reads_it_in_every_honored_function() {
    // The reference is the vendored terminal emulator, fed the same bytes: text, `\r`, SGR, the three erases and column 1,
    // in seeded random order, compared as the line's text with its trailing blanks set aside (the drawer has no row width).
    // Tabs, wide glyphs, other controls and other cursor moves differ by the stated limits and are not generated. This pins
    // what each function does to the row, every one beside every other: a row that kept its blank-to mark after a write
    // over it would leave an erase's blanks standing over text written after it.
    let parts = [
        "a", "bc", "def", "x y", " ", "\r", "\r", "\u{1b}[31m", "\u{1b}[1;44m", "\u{1b}[0m", "\u{1b}[K", "\u{1b}[0K", "\u{1b}[1K", "\u{1b}[2K", "\u{1b}[1G",
        "\u{1b}[G", "\u{1b}[0G",
    ];
    let mut next = rng(0x9e37_79b9_7f4a_7c15);
    for case in 0..4000 {
        let text: String = (0..1 + next(14)).map(|_| parts[next(parts.len())]).collect();
        let mut terminal = vt100::Parser::new(1, 300, 0);
        terminal.process(text.as_bytes());
        let want = terminal.screen().rows(0, 300).next().unwrap_or_default();
        let mut reader = Reader::new(Style::default(), Sgr::default(), true);
        reader.feed(&text);
        let got = reader.finish().first().map(|l| plain(l)).unwrap_or_default();
        assert_eq!(got.trim_end(), want.trim_end(), "case {case}: {text:?}");
    }
}
