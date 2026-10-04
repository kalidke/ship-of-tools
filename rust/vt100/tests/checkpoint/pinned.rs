//! The wire format is pinned: version 2 bytes, version 1 restore, the tag vocabulary, boundary invisibility.

use super::*;

// -- the format is pinned, not merely round-tripping -----------------------

/// A roundtrip test passes just as well if the encoder and decoder drift
/// together — a renumbered mouse tag, a reordered header. These bytes are
/// what version 1 means; changing them means changing the version.
#[test]
fn version_2_bytes_are_pinned() {
    let mut parser = Parser::new(2, 2, 0);
    parser.process(b"\x1b[?1006h\x1b[38;5;9;4mZ");

    #[rustfmt::skip]
    let expected: &[u8] = &[
        b'S', b'O', b'T', b'V', b'T', b'1', b'0', b'0', // magic
        2, 0,          // version 2
        2, 0,          // rows — grid::MIN_ROWS, the smallest a screen may be
        2, 0,          // cols
        0,             // modes: none set
        0,             // mouse protocol mode: None
        2,             // mouse protocol encoding: Sgr
        1, 9,          // attrs fg: Idx(9)
        0,             // attrs bg: Default
        0b0000_1000,   // attrs mode: underline
        0, 0, 0,       // saved attrs: default, default, no text mode
        // normal grid
        0, 0, 1, 0,    // pos: row 0, col 1 — the next drawable column
        0, 0, 0, 0,    // saved pos
        0, 0, 1, 0,    // scroll region rows 0..=1
        0,             // origin mode
        0,             // saved origin mode
        0,             // row 0: not wrapped
        0b0000_0011,   // cell (0, 0): length and attrs present
        1,             // packed length: 1 content byte, not wide
        b'Z',
        1, 9, 0, 0b0000_1000, // its attrs: fg Idx(9), bg default, underline
        0,             // cell (0, 1): empty, default attrs
        0,             // row 1: not wrapped
        0,             // cell (1, 0)
        0,             // cell (1, 1)
        0, 0,          // scrollback ring count: 0 — this parser has none
        // alternate grid: never entered, so blank rows at the same size,
        // and NO scrollback field at all (it never has one, any version)
        0, 0, 0, 0,
        0, 0, 0, 0,
        0, 0, 1, 0,
        0,
        0,
        0,             // row 0: not wrapped
        0,             // cell (0, 0)
        0,             // cell (0, 1)
        0,             // row 1: not wrapped
        0,             // cell (1, 0)
        0,             // cell (1, 1)
    ];
    assert_eq!(checkpoint(&parser), expected);
}

/// Version 1 predates the scrollback ring and has no field for it, for
/// either grid — the byte layout below is exactly the version-1 golden this
/// test replaced (`version_2_bytes_are_pinned`'s predecessor), unchanged,
/// because a real peer built before the ring existed can still send this
/// shape and restoring it must not refuse it. It simply describes a screen
/// with no history, the same legal state a version 2 payload describes when
/// its count field reads zero.
#[test]
fn version_1_payload_restores_with_an_empty_ring() {
    let mut parser = Parser::new(2, 2, 0);
    parser.process(b"\x1b[?1006h\x1b[38;5;9;4mZ");

    #[rustfmt::skip]
    let legacy: &[u8] = &[
        b'S', b'O', b'T', b'V', b'T', b'1', b'0', b'0', // magic
        1, 0,          // version 1 — no scrollback field anywhere
        2, 0,          // rows
        2, 0,          // cols
        0,             // modes: none set
        0,             // mouse protocol mode: None
        2,             // mouse protocol encoding: Sgr
        1, 9,          // attrs fg: Idx(9)
        0,             // attrs bg: Default
        0b0000_1000,   // attrs mode: underline
        0, 0, 0,       // saved attrs: default, default, no text mode
        // normal grid
        0, 0, 1, 0,    // pos: row 0, col 1
        0, 0, 0, 0,    // saved pos
        0, 0, 1, 0,    // scroll region rows 0..=1
        0,             // origin mode
        0,             // saved origin mode
        0,             // row 0: not wrapped
        0b0000_0011, 1, b'Z', 1, 9, 0, 0b0000_1000, // cell (0, 0)
        0,             // cell (0, 1)
        0,             // row 1: not wrapped
        0,             // cell (1, 0)
        0,             // cell (1, 1)
        // alternate grid: never entered, blank rows at the same size
        0, 0, 0, 0,
        0, 0, 0, 0,
        0, 0, 1, 0,
        0,
        0,
        0, 0, 0,
        0, 0, 0,
    ];

    // Restored at a NONZERO capacity, so an empty result proves the payload
    // itself carried no ring — not merely that the restorer's own capacity
    // truncated one away.
    let mut restored = Parser::new(2, 2, 50);
    restored
        .restore_screen(legacy)
        .expect("a version 1 payload must still restore");
    assert_visible_state_equal(parser.screen(), restored.screen());

    restored.screen_mut().set_scrollback(usize::MAX);
    assert_eq!(
        restored.screen().scrollback(),
        0,
        "a version 1 payload has no ring to restore, regardless of the \
         restorer's own capacity"
    );
}

/// The shape of a real attach: the capsule checkpoints at a parser
/// ground-state boundary, the frontend restores, and the stream resumes.
/// Whatever the cut, the result must equal never having been interrupted.
#[test]
fn a_cut_at_a_ground_state_boundary_is_invisible() {
    // Every chunk is a complete sequence or run of text, which is what "cut
    // at a ground-state boundary" means for the producer.
    let chunks: &[&[u8]] = &[
        b"\x1b[1;33mstarting up\r\n",
        b"\x1b[5;18r",              // scroll region
        b"\x1b[?6h\x1b[2;4H",       // origin mode, then address within it
        b"\x1b7",                   // save cursor, attrs, origin mode
        "wide 日本 text that will wrap past the right margin".as_bytes(),
        b"\x1b[?47h",               // alternate grid
        b"\x1b[2J\x1b[3;3Hfull screen program\x1b[7m",
        b"\x1b[?1000h\x1b[?1006h",  // mouse reporting
        b"\x1b[?47l",               // back to the normal grid
        b"\x1b8",                   // restore cursor, attrs, origin mode
        b"\x1b[?25l tail",
    ];

    let mut uninterrupted = Parser::new(20, 30, 0);
    for chunk in chunks {
        uninterrupted.process(chunk);
    }
    let expected = checkpoint(&uninterrupted);

    for cut in 0..=chunks.len() {
        let mut before = Parser::new(20, 30, 0);
        for chunk in &chunks[..cut] {
            before.process(chunk);
        }
        let mut after = restore(&checkpoint(&before)).expect("restore");
        for chunk in &chunks[cut..] {
            after.process(chunk);
        }
        assert_eq!(
            checkpoint(&after),
            expected,
            "a checkpoint taken after chunk {cut} changed the outcome"
        );
    }
}

/// Origin mode saved by DECSC has no getter either, so it is observed the
/// same way the saved cursor is: restore it and see where addressing lands.
#[test]
fn saved_origin_mode_survives() {
    let mut parser = Parser::new(24, 80, 0);
    parser.process(b"\x1b[5;20r\x1b[?6h"); // scroll region, origin mode on
    parser.process(b"\x1b7"); // DECSC captures origin mode too
    parser.process(b"\x1b[?6l"); // and now off

    let mut restored = roundtrip(&parser);
    restored.process(b"\x1b8"); // DECRC brings origin mode back
    restored.process(b"\x1b[1;1H");
    assert_eq!(
        restored.screen().cursor_position(),
        (4, 0),
        "saved origin mode did not survive"
    );
}

/// The layout golden above pins field order, but it can only pin the tag
/// values the one screen it encodes happens to use — which leaves `Color::Rgb`,
/// four of five mouse modes, every screen-mode bit, most text-mode bits, the
/// wide flags and the two cell flags individually unpinned. The exhaustive
/// matches in the encoder make ADDING an enum variant a compile error; they do
/// nothing against a RENUMBERING, which is exactly what a wire vocabulary
/// needs pinned.
#[test]
fn version_1_vocabulary_is_pinned() {
    /// A minimal screen after `seq`, so the header sits at fixed offsets.
    fn after(seq: &[u8]) -> Vec<u8> {
        let mut parser = Parser::new(2, 2, 0);
        parser.process(seq);
        checkpoint(&parser)
    }
    const MOUSE_MODE: usize = MODES_OFFSET + 1;
    const MOUSE_ENC: usize = MODES_OFFSET + 2;
    const ATTRS: usize = MODES_OFFSET + 3;

    // Screen modes.
    assert_eq!(after(b"\x1b=")[MODES_OFFSET], 0b0000_0001, "keypad");
    assert_eq!(after(b"\x1b[?1h")[MODES_OFFSET], 0b0000_0010, "app cursor");
    assert_eq!(after(b"\x1b[?25l")[MODES_OFFSET], 0b0000_0100, "hide cursor");
    assert_eq!(after(b"\x1b[?47h")[MODES_OFFSET], 0b0000_1000, "alt screen");
    assert_eq!(
        after(b"\x1b[?2004h")[MODES_OFFSET],
        0b0001_0000,
        "bracketed paste"
    );

    // Mouse protocol mode tags.
    assert_eq!(after(b"")[MOUSE_MODE], 0, "mouse None");
    assert_eq!(after(b"\x1b[?9h")[MOUSE_MODE], 1, "mouse Press");
    assert_eq!(after(b"\x1b[?1000h")[MOUSE_MODE], 2, "mouse PressRelease");
    assert_eq!(after(b"\x1b[?1002h")[MOUSE_MODE], 3, "mouse ButtonMotion");
    assert_eq!(after(b"\x1b[?1003h")[MOUSE_MODE], 4, "mouse AnyMotion");

    // Mouse protocol encoding tags.
    assert_eq!(after(b"")[MOUSE_ENC], 0, "encoding Default");
    assert_eq!(after(b"\x1b[?1005h")[MOUSE_ENC], 1, "encoding Utf8");
    assert_eq!(after(b"\x1b[?1006h")[MOUSE_ENC], 2, "encoding Sgr");

    // Color tags, including RGB channel order.
    assert_eq!(after(b"\x1b[39m")[ATTRS], 0, "Color::Default");
    assert_eq!(&after(b"\x1b[31m")[ATTRS..ATTRS + 2], &[1, 1], "Color::Idx");
    assert_eq!(
        &after(b"\x1b[38;2;7;8;9m")[ATTRS..ATTRS + 4],
        &[2, 7, 8, 9],
        "Color::Rgb, and red-green-blue order"
    );

    // Text-mode bits. Both colors are default here, so each is one tag byte
    // and the mode byte follows them.
    const MODE_BYTE: usize = ATTRS + 2;
    assert_eq!(after(b"\x1b[1m")[MODE_BYTE], 0b0000_0001, "bold");
    assert_eq!(after(b"\x1b[2m")[MODE_BYTE], 0b0000_0010, "dim");
    assert_eq!(after(b"\x1b[3m")[MODE_BYTE], 0b0000_0100, "italic");
    assert_eq!(after(b"\x1b[4m")[MODE_BYTE], 0b0000_1000, "underline");
    assert_eq!(after(b"\x1b[7m")[MODE_BYTE], 0b0001_0000, "inverse");

    // The two cell flags, separately — the layout golden only ever shows them
    // together as 3, so swapping their assignments would pass it.
    let mut text_only = Parser::new(2, 2, 0);
    text_only.process(b"q");
    assert_eq!(
        checkpoint(&text_only)[FIRST_CELL],
        0b0000_0001,
        "cell flag: length present"
    );
    let mut attrs_only = Parser::new(2, 2, 0);
    // Erase to a red background, then reset the CURRENT attributes so the
    // header keeps its default-attrs length and FIRST_CELL still lands.
    attrs_only.process(b"\x1b[41m\x1b[2J\x1b[m");
    assert_eq!(
        checkpoint(&attrs_only)[FIRST_CELL],
        0b0000_0010,
        "cell flag: attributes present"
    );

    // The packed wide bits.
    let mut wide = Parser::new(2, 2, 0);
    wide.process("日".as_bytes());
    let bytes = checkpoint(&wide);
    assert_eq!(bytes[FIRST_CELL + 1], 0b1000_0000 | 3, "wide lead bit");
    assert_eq!(bytes[FIRST_CELL + 6], 0b0100_0000, "wide continuation bit");

    // Grid booleans, and the one-past-the-end cursor ADR 0041 turns on.
    const GRID: usize = HEADER_LEN + 2 * DEFAULT_ATTRS_LEN;
    let mut origin = Parser::new(4, 4, 0);
    origin.process(b"\x1b[2;4r\x1b[?6h");
    assert_eq!(checkpoint(&origin)[GRID + 12], 1, "origin mode");

    let mut pending = Parser::new(2, 2, 0);
    pending.process(b"ab");
    let bytes = checkpoint(&pending);
    assert_eq!(
        u16::from_le_bytes([bytes[GRID + 2], bytes[GRID + 3]]),
        2,
        "the pending-wrap cursor sits one past the last column"
    );

    let mut wrapped = Parser::new(3, 2, 0);
    wrapped.process(b"abc");
    let bytes = checkpoint(&wrapped);
    assert_eq!(bytes[GRID + 14], 1, "row 0 wrapped flag");
}
