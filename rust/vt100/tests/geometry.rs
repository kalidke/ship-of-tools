//! The minimum terminal geometry (`grid::MIN_ROWS` x `MIN_COLS`), and the
//! maximum glyph width that makes it sufficient.
//!
//! A fork-specific rule, not upstream's. Below 2x2 the parser had inputs with
//! no right answer and nowhere to put one — a width-two glyph with no cell for
//! its continuation half, a wrap with nothing to scroll into — and both
//! PANICKED rather than degrading. The rule refuses the geometry instead of
//! inventing a rendering for a terminal nobody could read.
//!
//! Two halves, and both are load-bearing. A clamp that is not applied
//! everywhere leaves the panics reachable through whichever entrance it
//! missed; a clamp with no panic tests behind it would still pass if the
//! underlying crashes came back at 2x2.

/// Every entrance raises a smaller request, including the degenerate 0.
#[test]
fn constructors_raise_a_smaller_request_to_the_minimum() {
    for (rows, cols) in [(0, 0), (0, 40), (1, 1), (1, 40), (12, 0), (12, 1)] {
        let parser = vt100_ctt::Parser::new(rows, cols, 0);
        let (got_rows, got_cols) = parser.screen().size();
        assert!(
            got_rows >= 2 && got_cols >= 2,
            "Parser::new({rows}, {cols}) produced {got_rows}x{got_cols}"
        );
        assert_eq!(
            (got_rows, got_cols),
            (rows.max(2), cols.max(2)),
            "the clamp raised more than the failing dimension"
        );
    }
}

#[test]
fn set_size_raises_a_smaller_request_to_the_minimum() {
    let mut parser = vt100_ctt::Parser::new(24, 80, 0);
    parser.screen_mut().set_size(1, 40);
    assert_eq!(parser.screen().size(), (2, 40));
    parser.screen_mut().set_size(10, 1);
    assert_eq!(parser.screen().size(), (10, 2));
}

/// The alternate grid is resized alongside the normal one, so it must be
/// clamped alongside it too — otherwise entering it after a small resize
/// lands on exactly the geometry the rule exists to prevent.
#[test]
fn the_alternate_grid_is_clamped_too() {
    let mut parser = vt100_ctt::Parser::new(24, 80, 0);
    parser.screen_mut().set_size(1, 1);
    parser.process(b"\x1b[?47h");
    assert_eq!(parser.screen().size(), (2, 2));
    parser.process("\u{65e5}ab".as_bytes());
    parser.process(b"\x1b[?47l");
    assert_eq!(parser.screen().size(), (2, 2));
}

/// Formerly a panic: `size.cols - width` underflowed when a width-two glyph
/// met a one-column terminal, and fixing that underflow alone was not enough
/// — the glyph still had nowhere to put its continuation half and panicked
/// one unwrap later.
#[test]
fn a_wide_glyph_in_the_narrowest_terminal_does_not_panic() {
    let mut parser = vt100_ctt::Parser::new(4, 1, 0);
    parser.process("\u{4f60}\u{597d}".as_bytes());
    parser.process(b"ab");
    assert_eq!(parser.screen().size(), (4, 2));
}

/// Formerly a panic: `prev_pos.row -= scrolled` underflowed at row 0 when a
/// one-row terminal wrapped. `Parser::new(1, 2, 0).process(b"abc")` was the
/// whole repro.
#[test]
fn wrapping_in_the_shortest_terminal_does_not_panic() {
    let mut parser = vt100_ctt::Parser::new(1, 2, 0);
    parser.process(b"abc");
    assert_eq!(parser.screen().size(), (2, 2));
    // No newline: the second row is marked wrapped, so the two rows read back
    // as the one logical line they are.
    assert_eq!(parser.screen().contents(), "abc");
    assert!(parser.screen().row_wrapped(0));
}

/// Formerly a panic, and the one that showed a geometry minimum alone is not
/// enough: `unicode-width` reports a few characters as THREE cells wide since
/// 0.2 (U+17D8 KHMER SIGN BEYYAL at the time of writing), upstream's model has
/// no third cell, and `cols - width` underflowed at any terminal narrower than
/// the glyph. The width is clamped into the model (`grid::MAX_GLYPH_WIDTH`),
/// so such a character draws as an ordinary wide glyph.
///
/// Deliberately makes no assertion about what the table currently returns.
/// The property under test — this input neither panics nor produces a screen
/// that will not round trip — has to hold whatever the table says, and a
/// dependency bump that changed a width should not turn CI red on its own.
///
/// Found by `roundtrip_fuzz`, not by hand; that is why it varies geometry.
#[test]
fn a_glyph_wider_than_the_model_does_not_panic() {
    for (rows, cols) in [(2, 2), (2, 3), (3, 2), (4, 7), (24, 80)] {
        let mut parser = vt100_ctt::Parser::new(rows, cols, 0);
        // The shrunk counterexample: a wide glyph filling the row, then the
        // over-wide one with the cursor sitting one past the last column.
        parser.process("\u{2eaee}\u{17d8}".as_bytes());
        let bytes = parser
            .screen()
            .checkpoint()
            .unwrap_or_else(|e| panic!("{cols}x{rows} would not encode: {e}"));
        let mut restored = vt100_ctt::Parser::new(2, 2, 0);
        restored
            .restore_screen(&bytes)
            .unwrap_or_else(|e| panic!("{cols}x{rows} would not restore: {e}"));
        assert_eq!(
            restored.screen().checkpoint().expect("re-encode"),
            bytes,
            "{cols}x{rows} did not round trip"
        );
    }
}
