//! Canonical form: wide-glyph pairing, cross-field validity, one encoding per screen, reachable scroll regions.

use super::*;

// -- wide-character pairing ------------------------------------------------

/// A screen whose first two columns hold a wide glyph and its continuation.
fn wide_glyph_checkpoint() -> Vec<u8> {
    let mut parser = Parser::new(2, 6, 0);
    parser.process("日abcd".as_bytes());
    let bytes = checkpoint(&parser);
    // lead: flags = length present, packed len = wide | 3 content bytes
    assert_eq!(bytes[FIRST_CELL], 0b0000_0001);
    assert_eq!(bytes[FIRST_CELL + 1], 0b1000_0000 | 3);
    // continuation: flags = length present, packed len = continuation, no
    // content
    assert_eq!(bytes[FIRST_CELL + 5], 0b0000_0001);
    assert_eq!(bytes[FIRST_CELL + 6], 0b0100_0000);
    bytes
}

/// `screen::text` indexes `col + 1` on the strength of a wide cell having a
/// continuation after it. A lead admitted without one panics on the next
/// glyph written over it, so restore has to refuse it.
#[test]
fn rejects_wide_lead_without_its_continuation() {
    let mut bytes = wide_glyph_checkpoint();
    // Turn the continuation into an ordinary empty cell: flags 0, no length.
    bytes[FIRST_CELL + 5] = 0;
    bytes.remove(FIRST_CELL + 6);
    assert_eq!(
        restore(&bytes).err(),
        Some(CheckpointError::Malformed(
            "a wide character's lead without its continuation"
        ))
    );
}

/// The mirror case, and the worse one: `screen::text` indexes `col - 1` for a
/// continuation, which underflows outright at column zero.
#[test]
fn rejects_continuation_without_its_lead() {
    let mut bytes = wide_glyph_checkpoint();
    // Clear the wide bit on the lead, leaving the continuation unmatched.
    bytes[FIRST_CELL + 1] = 3;
    assert_eq!(
        restore(&bytes).err(),
        Some(CheckpointError::Malformed(
            "a wide character's continuation without its lead"
        ))
    );
}

#[test]
fn rejects_a_cell_that_is_both_halves_at_once() {
    let mut bytes = wide_glyph_checkpoint();
    bytes[FIRST_CELL + 1] = 0b1100_0000 | 3;
    assert_eq!(
        restore(&bytes).err(),
        Some(CheckpointError::Malformed(
            "a cell marked as both halves of a wide character"
        ))
    );
}

/// Validation must not refuse a screen the parser itself produces. Shrinking
/// used to cut a wide glyph in half and leave the lead behind, which this
/// would then reject — and which panicked on the next glyph even without a
/// checkpoint in the picture.
#[test]
fn shrinking_through_a_wide_glyph_does_not_orphan_it() {
    let mut parser = Parser::new(4, 10, 0);
    parser.process("12345678日".as_bytes());
    assert!(parser.screen().cell(0, 8).unwrap().is_wide());

    parser.screen_mut().set_size(4, 9); // drops the continuation column
    assert!(
        !parser.screen().cell(0, 8).unwrap().is_wide(),
        "shrinking left a wide lead with nowhere for its continuation"
    );

    // Both the direct write and the roundtrip must survive it.
    parser.process(b"\x1b[1;9Hz");
    assert_roundtrips(&mut parser);
}

// -- cross-field validity --------------------------------------------------

/// A corrupted mode byte used to be able to select a grid that had no rows,
/// which restored cleanly and then panicked on the first printable byte.
/// Materializing both grids is what removes the state entirely.
#[test]
fn a_selected_grid_always_has_rows() {
    let mut parser = Parser::new(10, 20, 0);
    parser.process(b"never touched the alternate grid");
    let mut bytes = checkpoint(&parser);
    bytes[MODES_OFFSET] |= 0b0000_1000; // MODE_ALTERNATE_SCREEN

    let mut restored = restore(&bytes).expect("still a valid screen");
    assert!(restored.screen().alternate_screen());
    restored.process(b"x");
    assert_eq!(restored.screen().cell(0, 0).unwrap().contents(), "x");
}

/// The encoder must not be able to emit a payload its own decoder refuses,
/// or the size bound is only a claim.
#[test]
fn refuses_to_checkpoint_a_screen_beyond_the_supported_size() {
    for (rows, cols) in [(257_u16, 80_u16), (24, 513), (300, 600)] {
        let parser = Parser::new(rows, cols, 0);
        assert_eq!(
            parser.screen().checkpoint().err(),
            Some(CheckpointError::Unrepresentable(
                "terminal dimensions are outside the supported range"
            )),
            "{cols}x{rows} was serialized despite being unrestorable"
        );
    }
}

/// An oversized payload is refused before any of it is decoded, so a valid
/// header followed by megabytes of trailing bytes cannot make a caller
/// buffer them all first.
#[test]
fn rejects_an_oversized_payload_without_decoding_it() {
    let mut bytes = b"SOTVT100".to_vec();
    bytes.resize(MAX_CHECKPOINT_LEN + 1, 0);
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::Malformed(
            "larger than any checkpoint this format can produce"
        ))
    ));
}

// -- one screen, one encoding ---------------------------------------------

const SCROLL_TOP: usize = GRID_START + 8;
const SCROLL_BOTTOM: usize = GRID_START + 10;

/// The format offers short forms — an omitted length for an empty cell,
/// omitted default attributes. Accepting the long form as an alias would
/// mean two byte strings describe one screen, so the golden below and every
/// byte-level comparison downstream would be pinning one of several right
/// answers. Single-byte corruption cannot find these, because spelling a
/// field out makes the payload longer.
#[test]
fn rejects_an_empty_cell_written_the_long_way() {
    let mut parser = Parser::new(2, 2, 0);
    parser.process(b"");
    let mut bytes = checkpoint(&parser);
    assert_eq!(bytes[FIRST_CELL], 0, "expected an empty default cell");
    bytes[FIRST_CELL] = 0b0000_0001; // claim a length field
    bytes.insert(FIRST_CELL + 1, 0); // whose packed length is zero
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::Malformed(
            "empty cell written with an explicit length"
        ))
    ));
}

#[test]
fn rejects_default_attributes_written_the_long_way() {
    let parser = Parser::new(2, 2, 0);
    let mut bytes = checkpoint(&parser);
    bytes[FIRST_CELL] = 0b0000_0010; // claim an attributes field
    bytes.splice(FIRST_CELL + 1..FIRST_CELL + 1, [0, 0, 0]); // all default
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::Malformed(
            "default attributes written explicitly"
        ))
    ));
}

// -- the scroll region must be one a parse can reach -----------------------

/// `set_scroll_region` takes an explicit region only when `top < bottom`, so
/// `0..=0` on a two-row screen is unreachable. It also panics: `col_wrap`
/// subtracts the rows it scrolled from the pre-wrap row, which underflows at
/// row zero on the next glyph that wraps.
#[test]
fn rejects_a_scroll_region_no_parse_can_produce() {
    let mut parser = Parser::new(2, 2, 0);
    parser.process(b"ab");
    let mut bytes = checkpoint(&parser);
    bytes[SCROLL_TOP..SCROLL_TOP + 2].copy_from_slice(&0_u16.to_le_bytes());
    bytes[SCROLL_BOTTOM..SCROLL_BOTTOM + 2]
        .copy_from_slice(&0_u16.to_le_bytes());
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::Malformed(
            "a scroll region shape no parse can produce"
        ))
    ));
}

/// The mirror of the test above, and the reason its rule is not simply
/// "reject equal endpoints": shrinking clamps a region's bottom down, which
/// can leave it equal to the top at the last row. That screen is real and
/// must still restore.
#[test]
fn accepts_the_equal_endpoint_region_a_resize_produces() {
    let mut parser = Parser::new(10, 4, 0);
    parser.process(b"\x1b[3;6r"); // rows 2..=5, a valid explicit region
    parser.screen_mut().set_size(3, 4); // bottom clamps down onto the top

    let bytes = checkpoint(&parser);
    assert_eq!(
        u16::from_le_bytes([bytes[SCROLL_TOP], bytes[SCROLL_TOP + 1]]),
        2
    );
    assert_eq!(
        u16::from_le_bytes([bytes[SCROLL_BOTTOM], bytes[SCROLL_BOTTOM + 1]]),
        2,
        "the resize was supposed to produce equal endpoints"
    );
    assert_roundtrips(&mut parser);
}

/// The rule above must also not catch a whole-screen region on the smallest
/// screen there is.
///
/// This test used to be about a ONE-row screen, whose region is `0..=0` — the
/// equal-endpoint shape the rule has to admit. That screen is no longer
/// constructible (`grid::MIN_ROWS`), and the equal-endpoint case is covered
/// by the resize above, which is where it actually comes from.
#[test]
fn accepts_the_smallest_screens_whole_screen_region() {
    let mut parser = Parser::new(2, 4, 0);
    parser.process(b"hi");
    assert_roundtrips(&mut parser);
}

/// The scroll-region rule compares against the row count, and doing that
/// arithmetic before range-checking the field overflows on `u16::MAX` — a
/// panic inside the check that exists to prevent one.
#[test]
fn rejects_extreme_scroll_region_values_without_overflowing() {
    for (top, bottom) in [
        (u16::MAX, u16::MAX),
        (0, u16::MAX),
        (u16::MAX, 0),
        (u16::MAX - 1, u16::MAX),
    ] {
        let mut parser = Parser::new(2, 2, 0);
        parser.process(b"ab");
        let mut bytes = checkpoint(&parser);
        bytes[SCROLL_TOP..SCROLL_TOP + 2].copy_from_slice(&top.to_le_bytes());
        bytes[SCROLL_BOTTOM..SCROLL_BOTTOM + 2]
            .copy_from_slice(&bottom.to_le_bytes());
        assert!(
            matches!(
                restore(&bytes),
                Err(CheckpointError::Malformed(
                    "a scroll region reaching past the last row"
                        | "a scroll region shape no parse can produce"
                ))
            ),
            "scroll region {top}..={bottom} was not refused cleanly"
        );
    }
}


// -- rows the parser could not have produced -------------------------------

#[test]
fn rejects_a_wide_continuation_carrying_text() {
    let mut bytes = wide_glyph_checkpoint();
    // Give the continuation one byte of its own text. `Grid::write_contents`
    // skips continuations, so this would be invisible in the row's string
    // form while a cell renderer drew it.
    bytes[FIRST_CELL + 6] = 0b0100_0000 | 1;
    bytes.insert(FIRST_CELL + 7, b'x');
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::Malformed(
            "a wide continuation carrying its own text"
        ))
    ));
}

#[test]
fn rejects_a_wide_continuation_with_its_own_attributes() {
    let mut bytes = wide_glyph_checkpoint();
    bytes[FIRST_CELL + 5] = 0b0000_0011; // length and attrs present
    bytes.splice(FIRST_CELL + 7..FIRST_CELL + 7, [1, 4, 0, 0]); // fg Idx(4)
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::Malformed(
            "a wide continuation with its own attributes"
        ))
    ));
}

#[test]
fn rejects_a_wide_lead_with_no_text() {
    let mut bytes = wide_glyph_checkpoint();
    bytes[FIRST_CELL + 1] = 0b1000_0000; // wide, but zero content bytes
    bytes.drain(FIRST_CELL + 2..FIRST_CELL + 5);
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::Malformed(
            "a wide lead with no text"
        ))
    ));
}

/// The rule this branch tried to add and had to withdraw, kept as the case
/// that disproved it. A one-row scroll region makes `col_wrap` scroll the
/// wrapping row away and mark the blank row above it instead — an odd flag on
/// an odd row, but exactly what the parser produces, so refusing it refused a
/// real session.
#[test]
fn a_row_wrapped_from_nothing_is_real_and_must_restore() {
    let mut parser = Parser::new(10, 2, 0);
    parser.process(b"\x1b[3;6r");       // scroll region rows 2..=5
    parser.screen_mut().set_size(3, 2);  // clamps it to the equal-endpoint 2..=2
    parser.process(b"abc");              // the third character wraps

    assert!(parser.screen().row_wrapped(1), "expected the odd wrap flag");
    assert!(
        !parser.screen().cell(1, 1).unwrap().has_contents(),
        "expected its last cell to be blank"
    );
    assert_roundtrips(&mut parser);
}

/// The counterweight to every rule above: none of them may refuse a screen
/// the parser actually produces. This drives a wide spread of real terminal
/// traffic through a range of geometries and requires every resulting screen
/// to checkpoint and restore.
#[test]
fn no_rule_here_refuses_a_screen_the_parser_produces() {
    const TRAFFIC: &[&[u8]] = &[
        b"plain text",
        b"\x1b[1;3;4;7;31;44mevery attribute at once\x1b[m",
        b"\x1b[38;2;1;2;3;48;2;4;5;6mtruecolor\x1b[m",
        "wide 日本語 and combining a\u{301}e\u{302}".as_bytes(),
        "trailing wide glyph at the very edge 日".as_bytes(),
        b"long enough to wrap several times over a narrow terminal indeed",
        b"\x1b[2;4r\x1b[?6hinside an origin-mode scroll region",
        b"\x1b[?47halternate\x1b[?47lnormal",
        b"\x1b[?1049hfull screen program\x1b[?1049l",
        b"\x1b[2Jcleared\x1b[1;1Hoverwritten",
        b"\x1b[41m\x1b[2Jerased to a background color",
        b"\x1b[3;3H\x1b[1@inserted cell\x1b[2P deleted cells",
        b"\x1b[1Linserted line\x1b[1Mdeleted line",
        b"\x1b[5Sscrolled up\x1b[3Tscrolled down",
        b"\x1b7saved\x1b[9;9H\x1b8restored",
        b"tab\there\tand\there",
        b"\x1b[?9h\x1b[?1005hmouse on",
        b"\x1b[10Xerased cells",
        "\u{1f600} emoji then text".as_bytes(),
    ];
    // Sizes that exercise the edges: smallest workable, odd widths that
    // split wide glyphs, and something ordinary.
    //
    // One-row and one-column geometries are absent because they no longer
    // exist: `grid::MIN_ROWS` / `MIN_COLS` raise any smaller request to 2x2.
    // They used to be absent for a worse reason — some traffic PANICKED the
    // parser there before a checkpoint was ever taken — and that is what the
    // minimum was introduced to settle. `geometry.rs` pins both halves.
    const SIZES: &[(u16, u16)] =
        &[(2, 2), (3, 5), (4, 7), (5, 9), (24, 80)];

    for &(rows, cols) in SIZES {
        for chunk in TRAFFIC {
            let mut parser = Parser::new(rows, cols, 0);
            parser.process(chunk);
            let bytes = parser.screen().checkpoint().unwrap_or_else(|e| {
                panic!("{cols}x{rows} {chunk:?} would not checkpoint: {e}")
            });
            let mut restored = Parser::new(2, 2, 0);
            restored.restore_screen(&bytes).unwrap_or_else(|e| {
                panic!("{cols}x{rows} {chunk:?} would not restore: {e}")
            });
            assert_eq!(checkpoint(&restored), bytes);

            // And again around a resize in each direction. Both orders
            // matter: resizing after the traffic reflows what is already
            // there, while RESUMING traffic after a resize is what composes
            // a clamped scroll region with a later wrap — the composition
            // that produced this suite's one real false rejection, and that
            // a resize-last corpus cannot reach.
            //
            // The shrink is `-2`, not `max(3) - 1`: the latter leaves the
            // smallest sizes unchanged and quietly tests nothing.
            for &(r2, c2) in &[
                (rows.saturating_sub(2).max(2), cols.saturating_sub(2).max(2)),
                (rows + 3, cols + 3),
            ] {
                for resume in [false, true] {
                    let mut resized = Parser::new(rows, cols, 0);
                    resized.process(chunk);
                    resized.screen_mut().set_size(r2, c2);
                    if resume {
                        resized.process(chunk);
                    }
                    let label =
                        format!("{cols}x{rows}->{c2}x{r2} resume={resume} {chunk:?}");
                    let bytes =
                        resized.screen().checkpoint().unwrap_or_else(|e| {
                            panic!("{label} would not checkpoint: {e}")
                        });
                    let mut restored = Parser::new(2, 2, 0);
                    restored.restore_screen(&bytes).unwrap_or_else(|e| {
                        panic!("{label} would not restore: {e}")
                    });
                }
            }
        }
    }
}
