use super::*;
use sot_protocol::ops::{whole_lines, TextPos};

fn sel(start: (u32, u32), end: (u32, u32), text: &str) -> SelectedText {
    SelectedText {
        node_id: String::new(),
        start: TextPos { line: start.0, byte: start.1 },
        end: TextPos { line: end.0, byte: end.1 },
        len: text.len() as u32,
        digest: selection_digest(text),
    }
}

#[test]
fn only_the_files_own_text_at_the_range_is_shared() {
    // (name, file bytes, start, end, the text of the range)
    let refused: Vec<(&str, &[u8], (u32, u32), (u32, u32), &str)> = vec![
        ("not UTF-8 inside the range", b"ab\xffcd", (0, 0), (0, 5), "ab\u{fffd}cd"),
        ("not UTF-8 before the range", b"\xffab\ncd", (1, 0), (1, 2), "cd"),
        ("lone CR before the end", b"ab\rcd\n", (0, 0), (0, 5), "ab\rcd"),
        ("lone CR on an earlier line", b"a\rb\ncd\n", (1, 0), (1, 2), "cd"),
        ("U+2028 before the end", "ab\u{2028}cd".as_bytes(), (0, 0), (0, 7), "ab\u{2028}cd"),
        ("U+2029 on an earlier line", "a\u{2029}\ncd".as_bytes(), (1, 0), (1, 2), "cd"),
        ("U+0085 before the end", "ab\u{85}cd".as_bytes(), (0, 0), (0, 6), "ab\u{85}cd"),
        ("end line outside the file", b"ab\ncd", (0, 0), (2, 1), "ab\ncd"),
        ("end past the bytes given", b"ab\ncd", (0, 0), (1, 3), "ab\ncd?"),
        ("end after a CRLF line's text", b"ab\r\ncd", (0, 0), (0, 3), "ab\r"),
        ("start byte past its line", b"ab\ncd", (0, 3), (1, 1), "\nc"),
        ("start inside a character", "é\nx".as_bytes(), (0, 1), (1, 1), "\u{a9}\nx"),
        ("end inside a character", "aé".as_bytes(), (0, 0), (0, 2), "a\u{c3}"),
        ("positions near u32::MAX", b"ab", (u32::MAX - 1, 0), (u32::MAX, u32::MAX), "x"),
        ("start after end", b"abc", (0, 3), (0, 1), "ab"),
        ("an empty range", b"abc", (0, 1), (0, 1), ""),
    ];
    for (name, bytes, s, e, text) in refused {
        assert_eq!(check_selection(bytes, &sel(s, e, text)), None, "{name}");
    }
    let mut wrong_len = sel((0, 0), (0, 2), "ab");
    wrong_len.len = 3;
    let mut wrong_digest = sel((0, 0), (0, 2), "ab");
    wrong_digest.digest ^= 1;
    let differs = sel((0, 0), (0, 2), "ac");
    for (name, s) in [("len differs", wrong_len), ("digest differs", wrong_digest), ("text differs", differs)] {
        assert_eq!(check_selection(b"ab\n", &s), None, "{name}");
    }

    // (name, file bytes, start, end, text, first line, last line)
    let accepted: Vec<(&str, &[u8], (u32, u32), (u32, u32), &str, usize, usize)> = vec![
        ("inside a line", b"hello world\n", (0, 6), (0, 11), "world", 1, 1),
        ("one whole line", b"ab\ncd\n", (1, 0), (1, 2), "cd", 2, 2),
        ("two lines", b"ab\ncd\nef\n", (0, 1), (1, 1), "b\nc", 1, 2),
        ("three lines", b"ab\ncd\nef\n", (0, 0), (2, 2), "ab\ncd\nef", 1, 3),
        ("CRLF within a line", b"ab\r\ncd\r\n", (0, 0), (0, 2), "ab", 1, 1),
        ("CRLF across lines", b"ab\r\ncd\r\nef", (0, 1), (2, 1), "b\ncd\ne", 1, 3),
        ("no final newline", b"ab\ncd", (1, 0), (1, 2), "cd", 2, 2),
        ("end at byte 0 of the next line", b"ab\ncd\n", (0, 0), (1, 0), "ab\n", 1, 1),
        ("the empty last line", b"ab\n", (0, 1), (1, 0), "b\n", 1, 1),
        ("the last character", b"ab\ncd", (1, 1), (1, 2), "d", 2, 2),
        ("multibyte text", "aé日\n".as_bytes(), (0, 1), (0, 6), "é日", 1, 1),
        ("a prefix ending mid-character after the range", &"ab\né".as_bytes()[..4], (0, 0), (0, 2), "ab", 1, 1),
        ("separators after the end", "ab\u{2028}\u{85}\rcd".as_bytes(), (0, 0), (0, 2), "ab", 1, 1),
        ("not UTF-8 after the end", b"ab\n\xff\xfe", (0, 0), (0, 2), "ab", 1, 1),
        ("a BOM and a tab are the file's bytes", "\u{feff}a\tb".as_bytes(), (0, 0), (0, 6), "\u{feff}a\tb", 1, 1),
    ];
    for (name, bytes, s, e, text, a, b) in accepted {
        let got = check_selection(bytes, &sel(s, e, text));
        assert_eq!(got, Some(FileText { text: text.to_string(), first_line: a, last_line: b }), "{name}");
    }
}

fn file_text(text: &str, a: usize, b: usize) -> FileText {
    FileText { text: text.to_string(), first_line: a, last_line: b }
}

fn parse(json: &str) -> serde_json::Value {
    serde_json::from_str(json).unwrap()
}

#[test]
fn the_rendering_is_the_hook_output_both_agents_accept_within_the_caps() {
    let v = parse(&render_selection(&file_text("a \"q\"\nb\\", 3, 4), "/p/src/f.jl", "src/f.jl"));
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["hookSpecificOutput", "systemMessage"]);
    let out = v["hookSpecificOutput"].as_object().unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out["hookEventName"], "UserPromptSubmit");
    assert_eq!(
        out["additionalContext"],
        "The user selected lines 3 to 4 of /p/src/f.jl in the Ship of Tools window:\na \"q\"\nb\\\nThis may or may not be related to the current task."
    );
    assert_eq!(v["systemMessage"], "Selection: src/f.jl lines 3-4");

    // a Windows path reaches the agent with its backslashes, escaped only in the JSON text
    let raw = render_selection(&file_text("x", 1, 1), "C:\\proj\\src\\f.jl", "src/f.jl");
    assert!(!raw.contains("C:\\proj\\src"), "the file's JSON text escapes each backslash");
    assert!(parse(&raw)["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains(" of C:\\proj\\src\\f.jl in the"));

    // a file name never breaks a line or carries an escape
    let v = parse(&render_selection(&file_text("x", 1, 1), "/p/a\nb\u{1b}[2J\u{2028}c\u{9b}\u{7f}", "a\nb\u{1b}c\u{2029}\u{9b}\u{7f}"));
    assert_eq!(v["systemMessage"], "Selection: a?b?c??? lines 1-1");
    assert!(v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains(" of /p/a?b?[2J?c?? in the"));

    // 1500 astral characters are 3000 UTF-16 units: cut at 2000 units (1000 characters), the size named in characters
    let emoji = "\u{1F600}".repeat(1500);
    let v = parse(&render_selection(&file_text(&emoji, 1, 1), "/p/f", "f"));
    let ctx = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    let kept = "\u{1F600}".repeat(1000);
    assert!(ctx.contains(&format!("{kept}... (truncated; the range holds 1500 characters)\n")), "{ctx}");
    assert!(!ctx.contains(&"\u{1F600}".repeat(1001)));
    let plain = "x".repeat(2000);
    let v = parse(&render_selection(&file_text(&plain, 1, 1), "/p/f", "f"));
    assert!(!v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("truncated"));

    // the caps hold in UTF-16 units at the longest paths, text and line numbers
    let path = format!("/{}", "\u{1F600}".repeat(2047));
    let long_path = format!("/{}", "d".repeat(100_000));
    for (p, rel) in [(path.as_str(), path.as_str()), (long_path.as_str(), long_path.as_str())] {
        let v = parse(&render_selection(&file_text(&emoji, 1_000_000_000, 2_000_000_000), p, rel));
        let ctx = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(ctx.encode_utf16().count() <= 9_000, "{}", ctx.encode_utf16().count());
        let msg = v["systemMessage"].as_str().unwrap();
        assert!(msg.encode_utf16().count() <= 200, "{}", msg.encode_utf16().count());
        assert!(msg.starts_with("Selection: ...") && msg.ends_with(" lines 1000000000-2000000000"), "{msg}");
    }
    let v = parse(&render_selection(&file_text(&emoji, 1, 1), &long_path, "f"));
    let ctx = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(ctx.contains(&format!(" of ...{} in the", "d".repeat(4093))));
}

#[test]
fn a_drawing_renders_as_the_hook_output_within_the_caps() {
    let keys = |v: &serde_json::Value| {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    let v = parse(&render_drawing(Some(("/p/fig.png", "fig.png")), "/p/.sot/captures/ink-r1.png", 3, 0xabc));
    assert_eq!(keys(&v), ["hookSpecificOutput", "systemMessage"]);
    assert_eq!(keys(&v["hookSpecificOutput"]), ["additionalContext", "hookEventName"]);
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    assert_eq!(v["systemMessage"], "Drawing: fig.png (3 strokes)");
    assert_eq!(
        v["hookSpecificOutput"]["additionalContext"],
        "The user drew over the image /p/fig.png in the Ship of Tools window. The image with the drawing over it is \
         /p/.sot/captures/ink-r1.png (drawing 0000000000000abc); read that image to see what the user marked.\n\
         This may or may not be related to the current task."
    );

    // the canvas has no figure: its wording and its message say sketch
    let v = parse(&render_drawing(None, "/p/.sot/captures/ink-r1.png", 1, u64::MAX));
    assert_eq!(v["systemMessage"], "Drawing: sketch (1 stroke)");
    assert_eq!(
        v["hookSpecificOutput"]["additionalContext"],
        "The user drew a sketch in the Ship of Tools window. The sketch is the image /p/.sot/captures/ink-r1.png \
         (drawing ffffffffffffffff); read that image to see it.\nThis may or may not be related to the current task."
    );

    // control characters in a path never reach the prompt or the status line
    let v = parse(&render_drawing(Some(("/p/a\nb", "a\u{1b}b")), "/p/c\nd", 2, 1));
    assert_eq!(v["systemMessage"], "Drawing: a?b (2 strokes)");
    assert!(v["hookSpecificOutput"]["additionalContext"].as_str().unwrap().contains("/p/a?b in the"));

    // two 4,096-unit paths, the most strokes and the longest names: the caps hold in UTF-16 units
    let long = format!("/{}", "\u{1F600}".repeat(2047));
    let longer = format!("/{}", "d".repeat(100_000));
    for (fig, pic) in [(long.as_str(), long.as_str()), (longer.as_str(), longer.as_str())] {
        let v = parse(&render_drawing(Some((fig, fig)), pic, 16_384, u64::MAX));
        let ctx = v["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(ctx.encode_utf16().count() <= 9_000, "{}", ctx.encode_utf16().count());
        let msg = v["systemMessage"].as_str().unwrap();
        assert!(msg.encode_utf16().count() <= 200, "{}", msg.encode_utf16().count());
        assert!(msg.starts_with("Drawing: ...") && msg.ends_with(" (16384 strokes)"), "{msg}");
    }
}

const MARKDOWN_FIXTURE: &str = r"---
title: plan
---

# Selection probe

First line of a paragraph
with `a code
span` and https://bare.example/url
last line of it.

- item one
  continued
- item two

> quoted line one
> quoted line two

| a | b |
|---|---|
| 1 | 2 |

$$
x = 1
$$

```julia
alpha = 1

beta = 2
```

    indented one
    indented two

***

Tail with a hard break\
after it.[^n]

[^n]: The note.
";

#[test]
fn a_whole_lines_range_is_the_files_own_lines() {
    let lf = MARKDOWN_FIXTURE.to_string();
    let crlf = lf.replace('\n', "\r\n");
    let no_final = lf.trim_end_matches('\n').to_string();
    let crlf_no_final = crlf.trim_end_matches('\n').to_string();
    // a non-ASCII line and two blank lines in a row, which the fixture lacks
    let extra = "a\n\n\né日\nb\n".to_string();
    for text in [lf, crlf, no_final, crlf_no_final, extra] {
        let count = text.split('\n').count();
        // every pair is built but a single empty line, counted from the text and not from `whole_lines`
        let empty = text.split('\n').filter(|l| l.strip_suffix('\r').unwrap_or(l).is_empty()).count();
        let mut accepted = 0;
        for first in 1..=count {
            for last in first..=count {
                let Some(((start, end), shown)) = whole_lines(&text, first, last) else { continue };
                let s = sel((start.0 as u32, start.1 as u32), (end.0 as u32, end.1 as u32), &shown);
                // the daemon reports the line before an end at byte 0 (`selection_check.rs`)
                let last_line = if end.1 == 0 { last - 1 } else { last };
                assert_eq!(
                    check_selection(text.as_bytes(), &s),
                    Some(file_text(&shown, first, last_line)),
                    "lines {first}..={last}"
                );
                accepted += 1;
            }
        }
        assert_eq!(accepted, count * (count + 1) / 2 - empty);
    }
}
