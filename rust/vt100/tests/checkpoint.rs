//! Checkpoint/restore roundtrip and rejection tests (ADR 0041 step 3).
//!
//! These drive the public API only, which is the same surface the capsule and
//! the attaching frontend use.
//!
//! Two kinds of assertion appear here, and both are needed. Comparing the
//! re-serialized bytes proves the *structure* survived, including state with
//! no public getter (the inactive grid, the saved cursor, saved attributes).
//! But byte equality alone is circular: a field the encoder forgets is
//! equally absent on both sides. So the tests that matter also observe the
//! state through behavior — switching grids, restoring the saved cursor —
//! which no amount of symmetric forgetting can fake.

use vt100_ctt::{
    CheckpointError, Color, MouseProtocolEncoding, MouseProtocolMode, Parser,
    Screen, MAX_CHECKPOINT_LEN,
};

/// Switch to the alternate grid without the cursor save/clear that `?1049`
/// performs, so a test can look at the other grid without disturbing state.
const ALT_ENTER: &[u8] = b"\x1b[?47h";
const ALT_EXIT: &[u8] = b"\x1b[?47l";

/// Offset of the screen-modes byte: magic 8, version 2, rows 2, cols 2.
const MODES_OFFSET: usize = 14;

/// A checkpoint of a screen the parser produced must always be writable.
fn checkpoint(parser: &Parser) -> Vec<u8> {
    parser.screen().checkpoint().expect("checkpoint")
}

fn roundtrip(parser: &Parser) -> Parser {
    let bytes = checkpoint(parser);
    let mut restored = Parser::new(1, 1, 0);
    restored
        .restore_screen(&bytes)
        .expect("a freshly written checkpoint must restore");
    restored
}

/// Restores into a throwaway parser, which is the only public restore path.
fn restore(bytes: &[u8]) -> Result<Parser, CheckpointError> {
    let mut parser = Parser::new(1, 1, 0);
    parser.restore_screen(bytes)?;
    Ok(parser)
}

/// Compares everything the public API exposes about the *visible* screen.
fn assert_visible_state_equal(a: &Screen, b: &Screen) {
    assert_eq!(a.size(), b.size(), "size");
    let (rows, cols) = a.size();
    assert_eq!(a.cursor_position(), b.cursor_position(), "cursor position");
    assert_eq!(a.alternate_screen(), b.alternate_screen(), "alternate screen");
    assert_eq!(a.application_keypad(), b.application_keypad(), "keypad");
    assert_eq!(a.application_cursor(), b.application_cursor(), "cursor mode");
    assert_eq!(a.hide_cursor(), b.hide_cursor(), "hide cursor");
    assert_eq!(a.bracketed_paste(), b.bracketed_paste(), "bracketed paste");
    assert_eq!(a.mouse_protocol_mode(), b.mouse_protocol_mode(), "mouse mode");
    assert_eq!(
        a.mouse_protocol_encoding(),
        b.mouse_protocol_encoding(),
        "mouse encoding"
    );
    assert_eq!(a.fgcolor(), b.fgcolor(), "fgcolor");
    assert_eq!(a.bgcolor(), b.bgcolor(), "bgcolor");
    assert_eq!(a.bold(), b.bold(), "bold");
    assert_eq!(a.dim(), b.dim(), "dim");
    assert_eq!(a.italic(), b.italic(), "italic");
    assert_eq!(a.underline(), b.underline(), "underline");
    assert_eq!(a.inverse(), b.inverse(), "inverse");
    for row in 0..rows {
        assert_eq!(a.row_wrapped(row), b.row_wrapped(row), "row {row} wrapped");
        for col in 0..cols {
            assert_eq!(
                a.cell(row, col),
                b.cell(row, col),
                "cell ({row}, {col})"
            );
        }
    }
}

/// The full check: the visible screen, the serialized structure, and — by
/// switching grids on both — the grid that was *not* visible.
///
/// The caller's parser is switched to the other grid and back, so it is left
/// showing what it showed on entry. The one lasting effect is that looking at
/// the alternate grid allocates its row storage, which is inherent to looking
/// at it; tests that care about that allocation compare checkpoints directly
/// instead of coming through here.
fn assert_roundtrips(original: &mut Parser) {
    let mut restored = roundtrip(original);
    assert_visible_state_equal(original.screen(), restored.screen());
    assert_eq!(
        checkpoint(original),
        checkpoint(&restored),
        "re-serialized structure"
    );

    let (away, back) = if original.screen().alternate_screen() {
        (ALT_EXIT, ALT_ENTER)
    } else {
        (ALT_ENTER, ALT_EXIT)
    };
    original.process(away);
    restored.process(away);
    assert_visible_state_equal(original.screen(), restored.screen());

    original.process(back);
    restored.process(back);
    assert_visible_state_equal(original.screen(), restored.screen());
}

#[test]
fn empty_screen_roundtrips() {
    let mut parser = Parser::new(24, 80, 0);
    assert_roundtrips(&mut parser);
}

#[test]
fn plain_text_roundtrips() {
    let mut parser = Parser::new(24, 80, 0);
    parser.process(b"hello \x1b[31mred\x1b[m and \x1b[1;4;38;2;10;20;30mfancy");
    assert_roundtrips(&mut parser);
    assert!(parser.screen().contents().contains("fancy"));
}

/// The roundtrip ADR 0041 names by name. `?1049` is the sequence a real
/// full-screen program uses: it saves the cursor, switches grids, and clears.
#[test]
fn alternate_screen_and_saved_cursor_roundtrip() {
    let mut parser = Parser::new(24, 80, 0);
    parser.process(b"normal grid content\r\n\x1b[5;10Hmore");
    parser.process(b"\x1b[?1049h");
    parser.process(b"\x1b[2;3Halternate grid content\x1b[33m");
    assert!(parser.screen().alternate_screen());
    assert_roundtrips(&mut parser);
}

/// The reason `contents_formatted` was rejected: it cannot express the grid
/// that is not on screen. Restore, then leave the alternate grid, and the
/// normal grid's content must still be there — byte for byte.
#[test]
fn inactive_grid_survives_the_checkpoint() {
    let mut original = Parser::new(10, 40, 0);
    original.process(b"\x1b[32mgrid one is still here\x1b[m");
    original.process(ALT_ENTER);
    original.process(b"\x1b[2Jgrid two");

    let mut restored = roundtrip(&original);
    assert!(restored.screen().alternate_screen());
    assert!(restored.screen().contents().contains("grid two"));
    assert!(!restored.screen().contents().contains("grid one"));

    restored.process(ALT_EXIT);
    assert!(!restored.screen().alternate_screen());
    assert!(
        restored.screen().contents().contains("grid one is still here"),
        "the inactive grid did not survive: {:?}",
        restored.screen().contents()
    );
    assert_eq!(
        restored.screen().cell(0, 0).unwrap().fgcolor(),
        Color::Idx(2),
        "inactive grid attributes did not survive"
    );
}

/// The saved cursor has no public getter, so it is observed the only way it
/// can be: by restoring it and looking at where the cursor lands.
#[test]
fn saved_cursor_survives_and_restores() {
    let mut original = Parser::new(24, 80, 0);
    original.process(b"\x1b[7;13H\x1b[1;35m");
    original.process(b"\x1b7"); // DECSC: save cursor, attrs, origin mode
    original.process(b"\x1b[1;1H\x1b[m");
    assert_eq!(original.screen().cursor_position(), (0, 0));

    let mut restored = roundtrip(&original);
    restored.process(b"\x1b8"); // DECRC
    original.process(b"\x1b8");

    assert_eq!(restored.screen().cursor_position(), (6, 12));
    assert_visible_state_equal(original.screen(), restored.screen());
    assert!(restored.screen().bold(), "saved attributes did not survive");
    assert_eq!(restored.screen().fgcolor(), Color::Idx(5));
}

#[test]
fn wrapped_lines_roundtrip() {
    let mut parser = Parser::new(6, 10, 0);
    // Longer than a row, so the parser sets the wrap flag on the rows it
    // continued from rather than starting a fresh logical line.
    parser.process(b"abcdefghijklmnopqrstuvwxyz");
    assert!(parser.screen().row_wrapped(0));
    assert!(parser.screen().row_wrapped(1));
    assert!(!parser.screen().row_wrapped(2));
    assert_roundtrips(&mut parser);
}

#[test]
fn input_modes_roundtrip() {
    let mut parser = Parser::new(24, 80, 0);
    parser.process(b"\x1b[?1h\x1b=\x1b[?25l\x1b[?2004h");
    let screen = parser.screen();
    assert!(screen.application_cursor());
    assert!(screen.application_keypad());
    assert!(screen.hide_cursor());
    assert!(screen.bracketed_paste());
    assert_roundtrips(&mut parser);
}

#[test]
fn mouse_protocol_roundtrips() {
    let mut parser = Parser::new(24, 80, 0);
    parser.process(b"\x1b[?1003h\x1b[?1006h");
    assert_eq!(
        parser.screen().mouse_protocol_mode(),
        MouseProtocolMode::AnyMotion
    );
    assert_eq!(
        parser.screen().mouse_protocol_encoding(),
        MouseProtocolEncoding::Sgr
    );
    assert_roundtrips(&mut parser);
}

#[test]
fn scroll_region_and_origin_mode_roundtrip() {
    let mut parser = Parser::new(24, 80, 0);
    parser.process(b"\x1b[5;20r\x1b[?6h\x1b[3;4H");
    assert_roundtrips(&mut parser);

    // Origin mode is observed through behavior: with it set, a cursor
    // address is relative to the scroll region's top.
    let mut restored = roundtrip(&parser);
    restored.process(b"\x1b[1;1H");
    assert_eq!(restored.screen().cursor_position(), (4, 0));
}

#[test]
fn wide_characters_roundtrip() {
    let mut parser = Parser::new(6, 10, 0);
    parser.process("日本語テキスト".as_bytes());
    let cell = parser.screen().cell(0, 0).unwrap();
    assert!(cell.is_wide());
    assert!(parser.screen().cell(0, 1).unwrap().is_wide_continuation());
    assert_roundtrips(&mut parser);
}

#[test]
fn combining_characters_roundtrip() {
    let mut parser = Parser::new(4, 10, 0);
    parser.process("a\u{301}\u{302}\u{303}e\u{304}".as_bytes());
    assert_roundtrips(&mut parser);
}

/// After a glyph lands in the last column the cursor sits one past the end,
/// pending a wrap. That is an ordinary state, and a bounds check written as
/// `col < cols` would refuse every screen in it.
#[test]
fn pending_wrap_cursor_roundtrips() {
    let mut parser = Parser::new(4, 10, 0);
    parser.process(b"0123456789");
    assert_eq!(parser.screen().cursor_position(), (0, 10));
    assert_roundtrips(&mut parser);
    assert_eq!(roundtrip(&parser).screen().cursor_position(), (0, 10));
}

/// The pending-wrap position is reachable at two columns past the last
/// drawable one when the final glyph is wide.
#[test]
fn pending_wrap_after_wide_glyph_roundtrips() {
    let mut parser = Parser::new(4, 10, 0);
    parser.process("12345678日".as_bytes());
    assert_eq!(parser.screen().cursor_position(), (0, 10));
    assert_roundtrips(&mut parser);
}

#[test]
fn resize_before_checkpoint_roundtrips() {
    let mut parser = Parser::new(24, 80, 0);
    parser.process(b"content that will be reflowed by the resize below");
    parser.screen_mut().set_size(10, 30);
    assert_roundtrips(&mut parser);
}

#[test]
fn scrollback_is_not_carried_and_capacity_comes_from_the_restorer() {
    let mut parser = Parser::new(4, 20, 100);
    for i in 0..20 {
        parser.process(format!("line {i}\r\n").as_bytes());
    }
    parser.screen_mut().set_scrollback(5);
    assert_eq!(parser.screen().scrollback(), 5);

    // A restored screen has no scrollback to be offset into, so the offset
    // is necessarily zero however far back the source was scrolled.
    let restored = roundtrip(&parser);
    assert_eq!(restored.screen().scrollback(), 0);

    // Capacity belongs to the restoring parser, not to the checkpoint: two
    // parsers configured differently restore the same bytes to the same
    // screen.
    let bytes = checkpoint(&parser);
    let mut roomy = Parser::new(1, 1, 10_000);
    let mut none = Parser::new(1, 1, 0);
    roomy.restore_screen(&bytes).unwrap();
    none.restore_screen(&bytes).unwrap();
    assert_eq!(checkpoint(&roomy), checkpoint(&none));
}

/// The alternate grid allocates its rows lazily, but that is an allocation
/// optimization rather than terminal state — nothing can observe the
/// difference, because reaching the alternate grid allocates it. The format
/// therefore writes blank rows instead of the distinction, which is also what
/// keeps a corrupted mode byte from selecting a grid with no rows for the
/// next glyph to land in.
#[test]
fn unallocated_alternate_grid_materializes_as_blank_rows() {
    // Identical in every way except that one has touched the alternate grid
    // and so has it allocated, and the other never has.
    let mut never = Parser::new(8, 20, 0);
    never.process(b"identical normal grid");

    let mut entered = Parser::new(8, 20, 0);
    entered.process(b"identical normal grid");
    entered.process(ALT_ENTER);
    entered.process(ALT_EXIT);

    assert_eq!(
        checkpoint(&never),
        checkpoint(&entered),
        "allocation state must not be visible on the wire"
    );
    assert_eq!(checkpoint(&never), checkpoint(&roundtrip(&never)));

    // And the materialized grid is usable, which the elided one was not.
    let mut restored = roundtrip(&never);
    restored.process(ALT_ENTER);
    restored.process(b"now drawing on the alternate grid");
    assert!(restored.screen().contents().contains("now drawing"));
}

/// The bound ADR 0041 requires proven. `MAX_CHECKPOINT_LEN` is arithmetic on
/// the format and is asserted at compile time; this proves the *encoder*
/// honors it on a screen built to be as expensive as the parser can make one
/// — maximum dimensions, both grids full, every cell carrying the longest
/// content the cell struct accepts plus two RGB colors and text attributes.
#[test]
fn checkpoint_at_max_dimensions_is_within_budget() {
    const ROWS: u16 = 256;
    const COLS: u16 = 512;

    // A one-byte base plus four-byte combining marks is what drives the
    // cell's content field closest to full: `Cell::append` stops accepting
    // once the length reaches 18, so the last mark can carry it to 21.
    let mut glyph = String::from("a");
    for _ in 0..5 {
        glyph.push('\u{101fd}');
    }
    let mut row = String::with_capacity(glyph.len() * usize::from(COLS));
    for _ in 0..COLS {
        row.push_str(&glyph);
    }

    let mut parser = Parser::new(ROWS, COLS, 0);
    let fill = |parser: &mut Parser| {
        // 24-bit foreground and background plus bold, italic, underline and
        // inverse: the largest attribute block the format can emit.
        parser.process(b"\x1b[1;3;4;7;38;2;1;2;3;48;2;4;5;6m");
        for r in 1..=ROWS {
            parser.process(format!("\x1b[{r};1H").as_bytes());
            parser.process(row.as_bytes());
        }
    };
    fill(&mut parser);
    parser.process(ALT_ENTER);
    fill(&mut parser);

    let sample = parser.screen().cell(0, 0).unwrap();
    assert_eq!(sample.contents().len(), 21, "cell content field not filled");
    assert_eq!(sample.fgcolor(), Color::Rgb(1, 2, 3));

    let bytes = checkpoint(&parser);
    assert!(
        bytes.len() <= MAX_CHECKPOINT_LEN,
        "checkpoint of {} bytes exceeds the stated bound of {}",
        bytes.len(),
        MAX_CHECKPOINT_LEN
    );
    assert!(
        bytes.len() < 12 * 1024 * 1024,
        "checkpoint of {} bytes exceeds the ADR 0041 budget",
        bytes.len()
    );

    let restored = restore(&bytes).expect("max-dimension restore");
    assert_eq!(checkpoint(&restored), bytes);
}

// -- rejection: restore is fail-closed ------------------------------------

fn sample_checkpoint() -> Vec<u8> {
    let mut parser = Parser::new(12, 40, 0);
    parser.process(b"sample \x1b[36mcontent\x1b[m\r\nsecond line");
    parser.process(ALT_ENTER);
    parser.process(b"alternate");
    parser.process(ALT_EXIT);
    checkpoint(&parser)
}

#[test]
fn rejects_bad_magic() {
    let mut bytes = sample_checkpoint();
    bytes[0] = b'X';
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::BadMagic)
    ));
}

#[test]
fn rejects_unsupported_version() {
    let mut bytes = sample_checkpoint();
    bytes[8] = 99;
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::UnsupportedVersion(99))
    ));
}

#[test]
fn rejects_degenerate_and_oversize_dimensions() {
    for (rows, cols) in
        [(0_u16, 40_u16), (12, 0), (257, 40), (12, 513), (600, 600)]
    {
        let mut bytes = sample_checkpoint();
        bytes[10..12].copy_from_slice(&rows.to_le_bytes());
        bytes[12..14].copy_from_slice(&cols.to_le_bytes());
        assert!(
            matches!(
                restore(&bytes),
                Err(CheckpointError::InvalidSize { .. })
            ),
            "{cols}x{rows} was not refused"
        );
    }
}

#[test]
fn rejects_undefined_mode_bits() {
    let mut bytes = sample_checkpoint();
    bytes[MODES_OFFSET] = 0b1000_0000;
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::InvalidBits { .. })
    ));
}

#[test]
fn rejects_unknown_mouse_tags() {
    let mut bytes = sample_checkpoint();
    bytes[MODES_OFFSET + 1] = 42;
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::UnknownTag { .. })
    ));

    let mut bytes = sample_checkpoint();
    bytes[MODES_OFFSET + 2] = 42;
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::UnknownTag { .. })
    ));
}

/// Bold and dim together is a state the parser cannot reach — every setter
/// clears the intensity bits before setting one — so restore refuses it under
/// the same rule as everything else here: only screens the parser could
/// itself have produced.
#[test]
fn rejects_simultaneous_bold_and_dim() {
    let mut bytes = sample_checkpoint();
    // The sample's current attributes are default, so both colors encode as
    // a bare tag and the text-mode byte follows them.
    bytes[MODES_OFFSET + 5] = 0b0000_0011;
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::InvalidBits { .. })
    ));
}

#[test]
fn rejects_trailing_bytes() {
    let mut bytes = sample_checkpoint();
    bytes.push(0);
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::TrailingBytes(1))
    ));
}

#[test]
fn rejects_every_truncation() {
    let bytes = sample_checkpoint();
    for len in 0..bytes.len() {
        assert!(
            restore(&bytes[..len]).is_err(),
            "a {len}-byte prefix of a {}-byte checkpoint was accepted",
            bytes.len()
        );
    }
}

/// Restore takes bytes off a transport. It must refuse anything it cannot
/// decode into a screen it could itself have produced — never panic, and
/// never round-trip to something different.
#[test]
fn restore_never_panics_on_corrupt_bytes() {
    let bytes = sample_checkpoint();
    for i in 0..bytes.len() {
        for patch in [0x00_u8, 0x01, 0x7f, 0x80, 0xff] {
            let mut corrupt = bytes.clone();
            corrupt[i] = patch;
            if let Ok(restored) = restore(&corrupt) {
                // Anything accepted must be internally consistent: it has to
                // re-serialize to exactly the bytes it was decoded from.
                assert_eq!(
                    checkpoint(&restored),
                    corrupt,
                    "byte {i} patched to {patch:#04x} restored to a screen \
                     that does not re-serialize to its own input"
                );
            }
        }
    }
}

#[test]
fn restore_never_panics_on_arbitrary_bytes() {
    // A deterministic spread of shapes rather than a random one, so a
    // failure here is reproducible.
    let mut seed = 0x9e37_79b9_u32;
    for len in [0_usize, 1, 7, 20, 21, 64, 512, 4096] {
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            bytes.push(u8::try_from(seed >> 24).unwrap());
        }
        let _ = restore(&bytes);

        // Same, but with a valid magic so the decoder gets past the first
        // gate and into the fields that actually parse.
        let mut with_magic = b"SOTVT100".to_vec();
        with_magic.extend_from_slice(&bytes);
        let _ = restore(&with_magic);
    }
}

#[test]
fn failed_restore_leaves_the_parser_untouched() {
    let mut parser = Parser::new(12, 40, 0);
    parser.process(b"do not disturb");
    let before = checkpoint(&parser);

    assert!(parser.restore_screen(b"garbage").is_err());
    assert_eq!(checkpoint(&parser), before);
}

/// A checkpoint describes a screen, never a half-consumed escape sequence.
/// The parser therefore returns to ground on restore, so the producer's
/// contract — cut the stream that follows at a ground-state boundary — is
/// what the two sides agree on rather than a leftover partial sequence.
#[test]
fn restore_resets_the_escape_sequence_parser() {
    let mut parser = Parser::new(4, 20, 0);
    parser.process(b"\x1b[31"); // a CSI left deliberately unterminated
    let bytes = checkpoint(&Parser::new(4, 20, 0));
    parser.restore_screen(&bytes).unwrap();

    // Had the half-parsed CSI survived, this `m` would have completed it and
    // set the foreground to red instead of being printed.
    parser.process(b"m");
    assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "m");
    assert_eq!(parser.screen().fgcolor(), Color::Default);
}

#[test]
fn restore_adopts_the_checkpoints_dimensions() {
    let mut source = Parser::new(30, 100, 0);
    source.process(b"content at a different size");

    let mut target = Parser::new(5, 5, 0);
    target.restore_screen(&checkpoint(&source)).unwrap();
    assert_eq!(target.screen().size(), (30, 100));
    assert_visible_state_equal(source.screen(), target.screen());
}

/// The one-byte empty cell is what keeps an ordinary screen cheap enough to
/// send on every attach. Without it a 200x50 screen would cost at least
/// 10,000 bytes in flag bytes alone and far more in practice; the bound test
/// above proves the ceiling, and this proves the common case is nowhere near
/// it.
#[test]
fn a_typical_screen_is_small() {
    let mut parser = Parser::new(50, 200, 0);
    for r in 1..=50 {
        parser.process(format!("\x1b[{r};1H").as_bytes());
        parser
            .process(b"\x1b[36ma fairly ordinary line of terminal output\x1b[m");
    }
    let len = checkpoint(&parser).len();
    assert!(
        len < 64 * 1024,
        "a typical 200x50 screen serialized to {len} bytes"
    );
    assert_roundtrips(&mut parser);
}

// -- wide-character pairing ------------------------------------------------

/// Byte offsets into a checkpoint of a screen whose current and saved
/// attributes are both default. Written out rather than searched for, so a
/// format change breaks these tests loudly instead of quietly relocating the
/// bytes they mutate.
const HEADER_LEN: usize = 8 + 2 + 2 + 2 + 1 + 1 + 1;
const DEFAULT_ATTRS_LEN: usize = 1 + 1 + 1;
const GRID_HEADER_LEN: usize = 2 + 2 + 2 + 2 + 2 + 2 + 1 + 1;
/// The flags byte of the first cell of the first row of the normal grid.
const FIRST_CELL: usize =
    HEADER_LEN + 2 * DEFAULT_ATTRS_LEN + GRID_HEADER_LEN + 1;

/// A one-row screen whose first two columns hold a wide glyph and its
/// continuation.
fn wide_glyph_checkpoint() -> Vec<u8> {
    let mut parser = Parser::new(1, 6, 0);
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
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::OrphanedWideHalf { col: 0 })
    ));
}

/// The mirror case, and the worse one: `screen::text` indexes `col - 1` for a
/// continuation, which underflows outright at column zero.
#[test]
fn rejects_continuation_without_its_lead() {
    let mut bytes = wide_glyph_checkpoint();
    // Clear the wide bit on the lead, leaving the continuation unmatched.
    bytes[FIRST_CELL + 1] = 3;
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::OrphanedWideHalf { col: 1 })
    ));
}

#[test]
fn rejects_a_cell_that_is_both_halves_at_once() {
    let mut bytes = wide_glyph_checkpoint();
    bytes[FIRST_CELL + 1] = 0b1100_0000 | 3;
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::OrphanedWideHalf { col: 0 })
    ));
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
        assert!(
            matches!(
                parser.screen().checkpoint(),
                Err(CheckpointError::Unrepresentable { .. })
            ),
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
        Err(CheckpointError::TooLarge { .. })
    ));
}

// -- the format is pinned, not merely round-tripping -----------------------

/// A roundtrip test passes just as well if the encoder and decoder drift
/// together — a renumbered mouse tag, a reordered header. These bytes are
/// what version 1 means; changing them means changing the version.
#[test]
fn version_1_bytes_are_pinned() {
    let mut parser = Parser::new(1, 2, 0);
    parser.process(b"\x1b[?1006h\x1b[38;5;9;4mZ");

    #[rustfmt::skip]
    let expected: &[u8] = &[
        b'S', b'O', b'T', b'V', b'T', b'1', b'0', b'0', // magic
        1, 0,          // version 1
        1, 0,          // rows
        2, 0,          // cols
        0,             // modes: none set
        0,             // mouse protocol mode: None
        2,             // mouse protocol encoding: Sgr
        1, 9,          // attrs fg: Idx(9)
        0,             // attrs bg: Default
        0b0000_1000,   // attrs mode: underline
        0, 0, 0,       // saved attrs: default, default, no text mode
        // normal grid
        0, 0, 1, 0,    // pos: row 0, col 1 — one past the glyph, pending wrap
        0, 0, 0, 0,    // saved pos
        0, 0, 0, 0,    // scroll region rows 0..=0
        0,             // origin mode
        0,             // saved origin mode
        0,             // row 0: not wrapped
        0b0000_0011,   // cell (0, 0): length and attrs present
        1,             // packed length: 1 content byte, not wide
        b'Z',
        1, 9, 0, 0b0000_1000, // its attrs: fg Idx(9), bg default, underline
        0,             // cell (0, 1): empty, default attrs
        // alternate grid: never entered, so blank rows at the same size
        0, 0, 0, 0,
        0, 0, 0, 0,
        0, 0, 0, 0,
        0,
        0,
        0,             // row 0: not wrapped
        0,             // cell (0, 0)
        0,             // cell (0, 1)
    ];
    assert_eq!(checkpoint(&parser), expected);
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

// -- one screen, one encoding ---------------------------------------------

/// Offsets into the normal grid's header, for a checkpoint whose current and
/// saved attributes are both default.
const GRID_START: usize = HEADER_LEN + 2 * DEFAULT_ATTRS_LEN;
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
    let mut parser = Parser::new(1, 1, 0);
    parser.process(b"");
    let mut bytes = checkpoint(&parser);
    assert_eq!(bytes[FIRST_CELL], 0, "expected an empty default cell");
    bytes[FIRST_CELL] = 0b0000_0001; // claim a length field
    bytes.insert(FIRST_CELL + 1, 0); // whose packed length is zero
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::NonCanonical { .. })
    ));
}

#[test]
fn rejects_default_attributes_written_the_long_way() {
    let parser = Parser::new(1, 1, 0);
    let mut bytes = checkpoint(&parser);
    bytes[FIRST_CELL] = 0b0000_0010; // claim an attributes field
    bytes.splice(FIRST_CELL + 1..FIRST_CELL + 1, [0, 0, 0]); // all default
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::NonCanonical { .. })
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
        Err(CheckpointError::InvalidScrollRegion { top: 0, bottom: 0 })
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

/// A one-row screen's region is `0..=0` by construction, which the rule above
/// must not catch.
#[test]
fn accepts_a_one_row_screens_whole_screen_region() {
    let mut parser = Parser::new(1, 4, 0);
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
                Err(CheckpointError::InvalidScrollRegion { .. })
            ),
            "scroll region {top}..={bottom} was not refused cleanly"
        );
    }
}
