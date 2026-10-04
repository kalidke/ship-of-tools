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

#[path = "../helpers/mod.rs"]
mod helpers;

mod canonical;
mod pinned;
mod rejects;

// The round-trip oracle is shared with the vendored upstream suite, which
// drives it over the whole fixture corpus. It lives in `helpers` so both use
// the same one.
use helpers::{
    assert_roundtrips, assert_visible_state_equal, checkpoint, restore,
    roundtrip, ALT_ENTER, ALT_EXIT,
};
use vt100_ctt::{
    CheckpointError, Color, MouseProtocolEncoding, MouseProtocolMode, Parser,
    MAX_CHECKPOINT_LEN,
};

/// Offset of the screen-modes byte: magic 8, version 2, rows 2, cols 2.
const MODES_OFFSET: usize = 14;

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

/// Offsets into the normal grid's header, for a checkpoint whose current and
/// saved attributes are both default.
const GRID_START: usize = HEADER_LEN + 2 * DEFAULT_ATTRS_LEN;

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

/// A live ring only ever keeps its newest `scrollback_len` rows (see
/// `Grid::scroll_up`'s pop-front-on-overflow); a checkpoint's ring, on
/// restore, is trimmed the identical way against the *restorer's own*
/// capacity — proven at the end of this test by matching a narrow-capacity
/// restore against an independent parser fed the identical byte stream from
/// scratch at that same capacity, which is the oracle a checkpoint round
/// trip has to agree with.
#[test]
fn checkpoint_carries_the_scrollback_ring_and_restore_keeps_the_restorers_newest() {
    let stream: Vec<u8> = (0..20)
        .map(|i| format!("line {i}\r\n"))
        .collect::<String>()
        .into_bytes();

    let mut original = Parser::new(4, 20, 100);
    original.process(&stream);
    original.screen_mut().set_scrollback(usize::MAX);
    let full_ring_len = original.screen().scrollback();
    assert!(
        full_ring_len > 3,
        "the test needs a fuller ring than the narrow capacity used below"
    );

    let bytes = checkpoint(&original);

    // The visible screen and the restored offset are independent of the
    // restorer's own scrollback capacity — the ring is history, never the
    // screen itself, and the offset always starts at zero right after a
    // restore, however much (or little) history rides along with it.
    for cap in [0_usize, 3, full_ring_len, full_ring_len + 50] {
        let mut restored = Parser::new(4, 20, cap);
        restored.restore_screen(&bytes).expect("restore");
        assert_eq!(
            restored.screen().scrollback(),
            0,
            "cap={cap}: offset must start at zero after any restore"
        );
        original.screen_mut().set_scrollback(0);
        assert_eq!(
            restored.screen().contents(),
            original.screen().contents(),
            "cap={cap}: visible screen must match regardless of ring capacity"
        );

        restored.screen_mut().set_scrollback(usize::MAX);
        assert_eq!(
            restored.screen().scrollback(),
            cap.min(full_ring_len),
            "cap={cap}: kept ring length must be min(checkpoint's ring, restorer's capacity)"
        );
    }

    // Newest, not just fewest: a narrow-capacity restore must keep the rows
    // nearest the current screen, not the oldest ones.
    let narrow_cap = 3;
    let mut narrow = Parser::new(4, 20, narrow_cap);
    narrow.restore_screen(&bytes).expect("restore");

    let mut reference = Parser::new(4, 20, narrow_cap);
    reference.process(&stream);

    assert_eq!(
        checkpoint(&narrow),
        checkpoint(&reference),
        "a restore's kept rows must equal a live ring that always had this capacity"
    );
}

/// `Parser::restore_screen` replaces the screen wholesale
/// (`self.screen.screen = screen`), never merges into it — so restoring the
/// SAME checkpoint twice must not double the ring, which an append bug
/// would do silently.
#[test]
fn restoring_twice_replaces_the_ring_rather_than_appending() {
    let mut source = Parser::new(4, 20, 100);
    for i in 0..20 {
        source.process(format!("line {i}\r\n").as_bytes());
    }
    let bytes = checkpoint(&source);

    let mut client = Parser::new(4, 20, 100);
    client.restore_screen(&bytes).expect("first restore");
    client.screen_mut().set_scrollback(usize::MAX);
    let first_len = client.screen().scrollback();
    assert!(first_len > 0, "the test needs a nonempty ring");

    client.restore_screen(&bytes).expect("second restore");
    client.screen_mut().set_scrollback(usize::MAX);
    assert_eq!(
        client.screen().scrollback(),
        first_len,
        "a second restore of the SAME checkpoint must yield the same ring \
         length, not double it"
    );
}

/// `Grid::set_size` resizes the scrollback ring's rows with the same
/// clip/pad policy as the visible ones (the same `Row::resize` call), so a
/// post-restore reflow (ADR 0042 L1b) or a later pane resize leaves every
/// ring row at the pane's current width instead of a stale one.
///
/// This deliberately does not go through the shared `assert_roundtrips`
/// helper: its restore target is a fixed `Parser::new(2, 2, 0)` (a capacity
/// of zero was always fine before the ring existed, since nothing carried
/// one), which would silently drop this test's ring on every round trip and
/// prove nothing about it. Restoring here at a matching capacity is what
/// actually exercises the ring.
#[test]
fn set_size_resizes_scrollback_ring_rows_too() {
    let mut parser = Parser::new(4, 10, 100);
    for i in 0..8 {
        parser.process(format!("row number {i}\r\n").as_bytes());
    }
    parser.screen_mut().set_scrollback(usize::MAX);
    let ring_len_before = parser.screen().scrollback();
    assert!(ring_len_before > 0, "the test needs a nonempty ring");
    parser.screen_mut().set_scrollback(0);

    // Widen, then narrow past the original width — every ring row must
    // come along, matching whatever width the visible rows themselves
    // would show at each size.
    for (rows, cols) in [(4_u16, 20_u16), (4, 3)] {
        parser.screen_mut().set_size(rows, cols);
        assert_eq!(parser.screen().size(), (rows, cols));

        parser.screen_mut().set_scrollback(usize::MAX);
        assert_eq!(
            parser.screen().scrollback(),
            ring_len_before,
            "resizing must not itself drop or duplicate ring rows"
        );
        parser.screen_mut().set_scrollback(0);

        // Every ring row (and the visible ones) must now be exactly `cols`
        // wide: a row `set_size` missed would make this checkpoint disagree
        // with a freshly restored copy of itself the moment either side is
        // resized again.
        let bytes = checkpoint(&parser);
        let mut restored = Parser::new(rows, cols, 100);
        restored.restore_screen(&bytes).expect("restore");
        assert_eq!(checkpoint(&restored), bytes);
    }
}

/// `Row::resize` clears a row's own `wrapped` flag internally (it has
/// to: a wrap flag computed for one width is not meaningful at another),
/// so `Grid::set_size` must call it ONLY when the column count actually
/// changes (Codex round on #194, finding 2) — a SAME-size `set_size`
/// call, exactly what `fe_client_io.rs`'s `pump` makes unconditionally
/// right after every restore, must be a pure no-op on wrap flags, for
/// both the visible screen and scrolled-off history.
///
/// This proves it via checkpoint byte-identity rather than hand-picking
/// which row ended up wrapped: if `set_size` cleared any wrap flag, the
/// re-encoded checkpoint would disagree with the one taken before it.
#[test]
fn restore_then_same_size_set_size_keeps_wrap_flags() {
    let mut original = Parser::new(3, 10, 20);
    // A wrapped logical line long enough that scrolling it through a
    // 3-row screen leaves one wrapped row in the ring and, by writing a
    // second overlong line right after, at least one wrapped row still
    // visible.
    original.process(b"abcdefghijklmnop\r\n");
    original.process(b"qrstuvwxyz0123456\r\n");

    original.screen_mut().set_scrollback(usize::MAX);
    assert!(
        original.screen().scrollback() > 0,
        "the test needs a nonempty ring"
    );
    original.screen_mut().set_scrollback(0);

    let (rows, cols) = original.screen().size();
    assert!(
        (0..rows).any(|r| original.screen().row_wrapped(r)),
        "the test needs a wrapped visible row"
    );

    let bytes = checkpoint(&original);
    let mut restored = Parser::new(rows, cols, 20);
    restored.restore_screen(&bytes).expect("restore");

    // Same-size `set_size` — exactly what `fe_client_io.rs`'s `pump`
    // does unconditionally right after every restore.
    restored.screen_mut().set_size(rows, cols);

    assert_eq!(
        checkpoint(&restored),
        bytes,
        "a same-size set_size must not change the checkpoint -- in particular, it must not clear wrap flags"
    );
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
    // Must match the vt100 fork's own `checkpoint::MAX_SCROLLBACK_ROWS`
    // (`pub(crate)` there, unreachable from this external test crate --
    // duplicated the same way `ROWS`/`COLS` above already duplicate
    // `checkpoint::MAX_ROWS`/`MAX_COLS`).
    const SCROLLBACK_CAP: usize = 200;

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

    let mut parser = Parser::new(ROWS, COLS, SCROLLBACK_CAP);
    // 24-bit foreground and background plus bold, italic, underline and
    // inverse: the largest attribute block the format can emit. Set once,
    // up front -- every row written after it, visible or scrolled off,
    // inherits it.
    parser.process(b"\x1b[1;3;4;7;38;2;1;2;3;48;2;4;5;6m");

    // Scroll ROWS + SCROLLBACK_CAP maximum-cost lines through the normal
    // grid -- not absolute positioning, which would never touch the ring
    // at all. The oldest SCROLLBACK_CAP end up in scrollback at full
    // capacity and full per-row cost; the newest ROWS stay visible. Before
    // this, the proof below used a 0-capacity parser, so "at maximum
    // dimensions" never actually included a full ring.
    for _ in 0..(u32::from(ROWS) + SCROLLBACK_CAP as u32) {
        parser.process(row.as_bytes());
        parser.process(b"\r\n");
    }
    parser.screen_mut().set_scrollback(usize::MAX);
    assert_eq!(
        parser.screen().scrollback(),
        SCROLLBACK_CAP,
        "the ring must be at its full capacity for this to be a genuine worst case"
    );
    parser.screen_mut().set_scrollback(0);

    // The alternate grid, filled the same maximum-cost way as before --
    // it never carries a ring, so absolute positioning (no scrolling side
    // effects to account for) is simplest here.
    parser.process(ALT_ENTER);
    parser.process(b"\x1b[1;3;4;7;38;2;1;2;3;48;2;4;5;6m");
    for r in 1..=ROWS {
        parser.process(format!("\x1b[{r};1H").as_bytes());
        parser.process(row.as_bytes());
    }

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

    // NOT the shared `restore()` helper: it restores into a fixed
    // `Parser::new(2, 2, 0)` -- zero scrollback capacity -- which would
    // silently drop this checkpoint's whole 200-row ring and make the
    // round trip below fail for a reason that has nothing to do with the
    // size bound. Match the source parser's own capacity instead.
    let mut restored = Parser::new(ROWS, COLS, SCROLLBACK_CAP);
    restored
        .restore_screen(&bytes)
        .expect("max-dimension restore");
    assert_eq!(checkpoint(&restored), bytes);
}

/// Codex round on #194, finding 4's companion: the ring's own count field
/// is bounded BEFORE any row is read, so a payload claiming more rows
/// than the format allows is refused on sight, not after failing to find
/// them or truncating silently.
#[test]
fn rejects_a_scrollback_ring_count_exceeding_the_format_bound() {
    let parser = Parser::new(2, 2, 0);
    let mut bytes = checkpoint(&parser);
    // The normal grid's ring count field: right after its 2x2 rows, each
    // one byte of wrap flag plus two one-byte empty-default cells (this
    // screen has no content at all) -- computed the same way
    // `GRID_START`/`GRID_HEADER_LEN` above already compute the fixed
    // header, extended past the rows this specific empty 2x2 screen
    // encodes.
    const RING_COUNT: usize = GRID_START + GRID_HEADER_LEN + 2 * (1 + 2);
    assert_eq!(
        &bytes[RING_COUNT..RING_COUNT + 2],
        &[0, 0],
        "offset math is wrong: this is not an empty ring's count field"
    );
    bytes[RING_COUNT..RING_COUNT + 2].copy_from_slice(&201u16.to_le_bytes());
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::Malformed(
            "the scrollback ring count exceeds the format's row bound"
        ))
    ));
}
