//! Restore is fail-closed: every malformed or oversize payload is refused, never panicked on.

use super::*;

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
        Err(CheckpointError::Malformed("not a checkpoint: bad magic"))
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
    for (rows, cols) in [
        (0_u16, 40_u16),
        (12, 0),
        // Below `grid::MIN_ROWS` / `MIN_COLS`: refused, not clamped, because
        // clamping would hand back a screen the payload does not describe.
        (1, 40),
        (12, 1),
        (257, 40),
        (12, 513),
        (600, 600),
    ] {
        let mut bytes = sample_checkpoint();
        bytes[10..12].copy_from_slice(&rows.to_le_bytes());
        bytes[12..14].copy_from_slice(&cols.to_le_bytes());
        assert!(
            matches!(
                restore(&bytes),
                Err(CheckpointError::Malformed(
                    "the payload announces terminal dimensions outside the \
                     supported range"
                ))
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
        Err(CheckpointError::Malformed(
            "screen modes carry undefined bits"
        ))
    ));
}

#[test]
fn rejects_unknown_mouse_tags() {
    let mut bytes = sample_checkpoint();
    bytes[MODES_OFFSET + 1] = 42;
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::Malformed(
            "undefined mouse protocol mode tag"
        ))
    ));

    let mut bytes = sample_checkpoint();
    bytes[MODES_OFFSET + 2] = 42;
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::Malformed(
            "undefined mouse protocol encoding tag"
        ))
    ));
}

/// Bold and dim together is a state the parser cannot reach — every setter
/// clears the intensity bits before setting one. Restore refuses it because
/// the check is cheap and certain, which is the actual rule here; the
/// contract is not "only screens the parser could produce", since three
/// unreachable shapes are knowingly accepted (see `Row::check_invariants`).
#[test]
fn rejects_simultaneous_bold_and_dim() {
    let mut bytes = sample_checkpoint();
    // The sample's current attributes are default, so both colors encode as
    // a bare tag and the text-mode byte follows them.
    bytes[MODES_OFFSET + 5] = 0b0000_0011;
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::Malformed(
            "bold and dim set together"
        ))
    ));
}

#[test]
fn rejects_trailing_bytes() {
    let mut bytes = sample_checkpoint();
    bytes.push(0);
    assert!(matches!(
        restore(&bytes),
        Err(CheckpointError::Malformed("bytes remained after the checkpoint"))
    ));
}

#[test]
fn rejects_every_truncation() {
    let bytes = sample_checkpoint();
    for len in 0..bytes.len() {
        // Every prefix must fail for the SAME reason — it ran out of bytes —
        // and not because a shorter payload happened to trip some other
        // rule on the way.
        assert_eq!(
            restore(&bytes[..len]).err(),
            Some(CheckpointError::Malformed(
                "payload ended before the checkpoint was complete"
            )),
            "a {len}-byte prefix of a {}-byte checkpoint",
            bytes.len()
        );
    }
}

/// Restore takes bytes off a transport. Whatever it accepts it must accept
/// consistently — never panic, and never round-trip to something different
/// from what it was handed.
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

    // `is_err` on purpose, and the only one left in this file: the property
    // under test is that ANY failure leaves the parser untouched, so pinning
    // which error would narrow the test rather than strengthen it.
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

/// The FE-side fix for the attach-time misalignment this fact causes: a
/// capsule's checkpoint carries the CAPSULE's own PTY size (its
/// `build_run_command` creation default of 80x24 until actually resized),
/// which need not match the pane the attaching frontend renders into.
/// `restore_screen` alone leaves the client's screen at the checkpoint's
/// size (proven above by `restore_adopts_the_checkpoints_dimensions`), so
/// `sot_log::fe_client_io`'s `pump()` calls `Screen::set_size` to the
/// pane's own known rect immediately after every restore. This proves
/// that step, at the exact crate boundary the fix calls: every cell the
/// checkpoint described keeps its content and position, and every cell in
/// the pane's rect resolves to `Some` rather than the `None` that made
/// `rust/frontend/src/ui/drawer/terminal/vt.rs`'s `paint_terminal` skip a cell and leave a
/// previous frame's content sitting in the ratatui buffer — the observed
/// "garbled" first paint.
#[test]
fn restore_then_resize_reflows_to_the_pane_without_misalignment() {
    // Mirrors the observed defect exactly: a freshly created capsule's run
    // child starts at `--cols 80 --rows 24`, while the attaching FE's pane
    // is already at its own, larger rect.
    let (checkpoint_rows, checkpoint_cols): (u16, u16) = (24, 80);
    let (pane_rows, pane_cols): (u16, u16) = (76, 203);

    let mut source = Parser::new(checkpoint_rows, checkpoint_cols, 0);
    source.process(b"\x1b[31mtop-left content\x1b[m");
    source.process(b"\x1b[23;70Hbottom-right corner");

    // `FeAttachClient::attach` creates the parser at the pane's rect.
    let mut target = Parser::new(pane_rows, pane_cols, 0);
    target.restore_screen(&checkpoint(&source)).unwrap();
    // The root cause, inline: restore alone left the screen mismatched
    // against the pane it will be painted into.
    assert_eq!(target.screen().size(), (checkpoint_rows, checkpoint_cols));

    // The fix: reflow to the pane's rect right after restore.
    target.screen_mut().set_size(pane_rows, pane_cols);
    assert_eq!(target.screen().size(), (pane_rows, pane_cols));

    // Every cell the checkpoint described survives, unmoved and
    // unmodified — `Grid::set_size`'s pad/clip only appends rows/columns,
    // it never reflows the ones that were already there.
    for row in 0..checkpoint_rows {
        for col in 0..checkpoint_cols {
            assert_eq!(
                target.screen().cell(row, col),
                source.screen().cell(row, col),
                "cell ({row}, {col}) moved or changed on reflow"
            );
        }
    }

    // Every cell in the pane's own rect now resolves — this is what stops
    // the render loop from hitting the `None` arm for most of the pane.
    for row in 0..pane_rows {
        for col in 0..pane_cols {
            assert!(
                target.screen().cell(row, col).is_some(),
                "cell ({row}, {col}) missing after reflow to the pane size"
            );
        }
    }

    // The padding beyond the checkpoint's own bounds is blank, not
    // garbage — `Grid::set_size`'s new cells are `Cell::new()`.
    assert_eq!(
        target
            .screen()
            .cell(pane_rows - 1, pane_cols - 1)
            .unwrap()
            .contents(),
        ""
    );
}

/// The reverse direction: a reattach whose pane rect SHRANK while
/// disconnected must clip, not leave any cell in the new (smaller) rect
/// undefined — the same unconditional `Screen::set_size` call the fix
/// makes after every restore, regardless of which way the size moved.
#[test]
fn restore_then_resize_clips_when_the_pane_shrank() {
    let mut source = Parser::new(50, 120, 0);
    source.process(b"\x1b[10;10Hstill here after the shrink");

    let mut target = Parser::new(50, 120, 0);
    target.restore_screen(&checkpoint(&source)).unwrap();
    target.screen_mut().set_size(20, 60);
    assert_eq!(target.screen().size(), (20, 60));
    for row in 0..20 {
        for col in 0..60 {
            assert!(target.screen().cell(row, col).is_some());
        }
    }
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
