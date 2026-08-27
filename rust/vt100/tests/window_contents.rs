//! Upstream's `window_contents.rs`, reduced to the part that survives the
//! fork.
//!
//! Most of this file tested the escape-sequence *writing* stack — `formatted`,
//! `diff_basic`, `diff_erase`, the `rows_formatted` / `rows_diff` halves of
//! `rows`, and a 7625-input crawl corpus driving `contents_diff`. All of that
//! is API this fork deleted (see `CHANGELOG.md`), so the tests went with it
//! and the 31 MB corpus was not vendored. What remains here is what the tests
//! also claimed about the *parsed* screen: which text lands in which row, at
//! which window offset, where the cursor ends up, and what
//! `contents_between` returns.

mod helpers;

/// The 24 rows of a default screen, with the named rows overridden. Upstream
/// spelled all 24 out at every assertion.
fn expected_rows(overrides: &[(usize, &str)]) -> Vec<String> {
    let mut rows = vec![String::new(); 24];
    for (i, text) in overrides {
        rows[*i] = (*text).to_string();
    }
    rows
}

#[test]
fn empty_cells() {
    let mut parser = vt100_ctt::Parser::default();
    parser.process(b"\x1b[5C\x1b[32m bar\x1b[H\x1b[31mfoo");
    assert_eq!(parser.screen().contents(), "foo   bar");
    helpers::assert_roundtrips(&mut parser);
}

/// Cursor placement at and past the right edge, including the pending-wrap
/// column (80 on an 80-column screen) that only becomes a real wrap when the
/// next character arrives.
#[test]
fn cursor_positioning() {
    let mut parser = vt100_ctt::Parser::default();

    parser.process(b":\x1b[K");
    assert_eq!(parser.screen().cursor_position(), (0, 1));

    parser.process(b"a");
    assert_eq!(parser.screen().cursor_position(), (0, 2));

    parser.process(b"\x1b[1;2H\x1b[K");
    assert_eq!(parser.screen().cursor_position(), (0, 1));
    assert_eq!(parser.screen().contents(), ":");

    parser.process(b"\x1b[H\x1b[J\x1b[4;80H");
    assert_eq!(parser.screen().cursor_position(), (3, 79));

    parser.process(b"a");
    assert_eq!(parser.screen().cursor_position(), (3, 80));
    helpers::assert_roundtrips(&mut parser);

    parser.process(b"\n");
    assert_eq!(parser.screen().cursor_position(), (4, 80));
    helpers::assert_roundtrips(&mut parser);

    parser.process(b"b");
    assert_eq!(parser.screen().cursor_position(), (5, 1));
}

#[test]
fn rows() {
    let mut parser = vt100_ctt::Parser::default();
    let screen1 = parser.screen().clone();
    let blank = expected_rows(&[]);
    assert_eq!(screen1.rows(0, 80).collect::<Vec<String>>(), blank);
    assert_eq!(screen1.rows(5, 15).collect::<Vec<String>>(), blank);

    parser
        .process(b"\x1b[31mfoo\x1b[10;10H\x1b[32mbar\x1b[20;20H\x1b[33mbaz");
    let screen2 = parser.screen().clone();
    assert_eq!(
        screen2.rows(0, 80).collect::<Vec<String>>(),
        expected_rows(&[
            (0, "foo"),
            (9, "         bar"),
            (19, "                   baz"),
        ])
    );
    assert_eq!(
        screen2.rows(5, 15).collect::<Vec<String>>(),
        expected_rows(&[(9, "    bar"), (19, "              b")])
    );
}

#[test]
fn contents_between() {
    let mut parser = vt100_ctt::Parser::default();
    assert_eq!(parser.screen().contents_between(0, 0, 0, 0), "");
    assert_eq!(parser.screen().contents_between(0, 0, 5, 0), "\n\n\n\n\n");
    assert_eq!(parser.screen().contents_between(5, 0, 0, 0), "");

    parser.process(
        b"Lorem ipsum dolor sit amet, consectetur adipiscing elit, \
        sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\n\n\
        Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris \
        nisi ut aliquip ex ea commodo consequat.\n\n\
        Duis aute irure dolor in reprehenderit in voluptate velit esse cillum \
        dolore eu fugiat nulla pariatur.\n\n\
        Excepteur sint occaecat cupidatat non proident, sunt in culpa qui \
        officia deserunt mollit anim id est laborum.",
    );
    assert_eq!(parser.screen().contents_between(0, 0, 0, 0), "");
    assert_eq!(
        parser.screen().contents_between(0, 0, 0, 26),
        "Lorem ipsum dolor sit amet"
    );
    assert_eq!(parser.screen().contents_between(0, 26, 0, 0), "");
    assert_eq!(
        parser.screen().contents_between(0, 57, 1, 43),
        "sed do eiusmod tempor incididunt ut labore et dolore magna aliqua."
    );
    assert_eq!(
        parser.screen().contents_between(0, 57, 2, 0),
        "sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\n"
    );
    assert_eq!(parser.screen().contents_between(2, 0, 0, 57), "");
}
