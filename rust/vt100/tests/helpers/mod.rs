//! Shared helpers for the vendored upstream test suite.
//!
//! # The oracle, and why it is not upstream's
//!
//! Upstream proved that a parsed screen was *completely* represented by
//! re-emitting it as escape sequences and re-parsing them: if the round trip
//! lost nothing, nothing was missing from the state. This fork deleted that
//! stack (see `CHANGELOG.md`) because ADR 0041 rejects it — synthesized escape
//! sequences cannot express the inactive grid or alternate-screen identity —
//! and replaced it with `Screen::checkpoint` / `Parser::restore_screen`.
//!
//! So the oracle here is the checkpoint round trip. It is the same *shape* of
//! proof, and a strictly stronger one: it compares the grid that was not
//! visible, which the escape-sequence round trip could never reach.
//!
//! One upstream check has no analog and is deliberately gone: the
//! `contents_diff` half, which proved an *incremental* update carried a screen
//! from one state to the next.
//!
//! Its natural replacement — restore a checkpoint taken mid-stream, then feed
//! the rest to both the restored parser and one that never restarted — was
//! tried here and does not belong here. The corpus splits its input *inside*
//! escape sequences and UTF-8 characters on purpose, and a checkpoint cannot
//! carry a half-read sequence: the escape-sequence state machine lives in
//! `vte` with private state. That is why the capsule only ever cuts the attach
//! stream at a ground-state boundary. The property is real, so it is tested
//! where the cut can be placed deliberately —
//! `checkpoint::a_cut_at_a_ground_state_boundary_is_invisible`.

// Each integration test file is its own crate and uses a different slice of
// this module, so items unused by one of them are not dead.
#![allow(dead_code)]

mod fixtures;

#[allow(unused_imports)]
pub use fixtures::fixture;

use vt100_ctt::{CheckpointError, Parser, Screen};

/// Switch to the alternate grid without the cursor save/clear that `?1049`
/// performs, so a test can look at the other grid without disturbing state.
pub const ALT_ENTER: &[u8] = b"\x1b[?47h";
pub const ALT_EXIT: &[u8] = b"\x1b[?47l";

/// A checkpoint of a screen the parser produced must always be writable.
pub fn checkpoint(parser: &Parser) -> Vec<u8> {
    parser.screen().checkpoint().expect("checkpoint")
}

pub fn roundtrip(parser: &Parser) -> Parser {
    let bytes = checkpoint(parser);
    let mut restored = Parser::new(2, 2, 0);
    restored
        .restore_screen(&bytes)
        .expect("a freshly written checkpoint must restore");
    restored
}

/// Restores into a throwaway parser, which is the only public restore path.
pub fn restore(bytes: &[u8]) -> Result<Parser, CheckpointError> {
    let mut parser = Parser::new(2, 2, 0);
    parser.restore_screen(bytes)?;
    Ok(parser)
}

/// Compares everything the public API exposes about the *visible* screen.
pub fn assert_visible_state_equal(a: &Screen, b: &Screen) {
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
    assert_eq!(a.contents(), b.contents(), "contents");
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

/// Everything a round trip must preserve, checked without driving either
/// parser: the visible screen, and the re-serialized bytes — which cover the
/// state with no public getter, including the grid that is not on screen.
pub fn assert_state_preserved(original: &Parser, restored: &Parser) {
    assert_visible_state_equal(original.screen(), restored.screen());
    assert_eq!(
        checkpoint(original),
        checkpoint(restored),
        "re-serialized structure"
    );
}

/// The full check: [`assert_state_preserved`], plus a look at the grid that
/// was *not* visible by switching both parsers to it and back. Byte equality
/// alone is circular — a field the encoder forgets is equally absent on both
/// sides — and this probe is what a symmetric omission cannot fake.
///
/// The probe FEEDS BYTES to the parser, so it requires one in ground state.
/// A parser stopped part-way through an escape sequence or a UTF-8 character
/// consumes the probe differently from a freshly restored one, and would
/// report a divergence that is really the documented cost of cutting
/// mid-sequence. Callers holding a possibly-mid-sequence parser — the fixture
/// corpus splits its input there on purpose — want
/// [`assert_screen_roundtrips`] instead.
///
/// The caller's parser is switched to the other grid and back, so it is left
/// showing what it showed on entry. The one lasting effect is that looking at
/// the alternate grid allocates its row storage, which is inherent to looking
/// at it; tests that care about that allocation compare checkpoints directly
/// instead of coming through here.
pub fn assert_roundtrips(original: &mut Parser) {
    let mut restored = roundtrip(original);
    assert_state_preserved(original, &restored);

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

/// The corpus oracle: parse `input` and require the screen it produced to
/// survive the round trip. Deliberately the no-probe check — see
/// [`assert_roundtrips`].
pub fn assert_screen_roundtrips(input: &[u8]) {
    let mut parser = Parser::default();
    parser.process(input);
    let restored = roundtrip(&parser);
    assert_state_preserved(&parser, &restored);
}
