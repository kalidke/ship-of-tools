//! Issue-B split (2026-09-05): after a capsule checkpoint is restored into
//! the frontend's parser (and reflowed to the pane, exactly as
//! `sot_log::fe_client_win`'s `pump()` does), does ordinary newline-driven
//! output still fill the scrollback ring? The field trace could not be
//! taken (an instrumented FE build trips the build boundary), but this is a
//! deterministic property of the parser, so it is proven here instead.
use vt100_ctt::Parser;

const SCROLLBACK_ROWS: usize = 5000; // fe_client_win.rs's own capacity

fn ring_len(p: &mut Parser) -> usize {
    p.screen_mut().set_scrollback(usize::MAX);
    let n = p.screen().scrollback();
    p.screen_mut().set_scrollback(0);
    n
}

fn feed_lines(p: &mut Parser, n: usize) {
    for i in 0..n {
        p.process(format!("line {i}\r\n").as_bytes());
    }
}

#[test]
fn control_fresh_parser_ring_fills_from_newlines() {
    let mut p = Parser::new(76, 203, SCROLLBACK_ROWS);
    feed_lines(&mut p, 200);
    let n = ring_len(&mut p);
    assert!(n >= 200 - 76, "fresh parser ring_len={n}");
}

#[test]
fn ring_fills_from_newlines_after_a_restore_and_pane_reflow() {
    // The capsule side: an 80x24 run child that painted a prompt.
    let mut source = Parser::new(24, 80, SCROLLBACK_ROWS);
    source.process(b"\x1b[3;2HAccessing workspace\x1b[15;2H> No, exit\x1b[18;2HEnter to confirm");
    let checkpoint = source.screen().checkpoint().unwrap();

    // The frontend side, as pump()'s Checkpoint arm does it.
    let mut target = Parser::new(76, 203, SCROLLBACK_ROWS);
    target.restore_screen(&checkpoint).unwrap();
    target.screen_mut().set_size(76, 203);
    assert_eq!(ring_len(&mut target), 0, "restore starts with an empty ring");

    feed_lines(&mut target, 200);
    let n = ring_len(&mut target);
    assert!(n >= 200 - 76, "post-restore ring_len={n} (expected >= 124)");
}

#[test]
fn absolute_positioning_repaint_does_not_fill_the_ring() {
    // Claude's routine traffic: full-screen repaints via cursor addressing,
    // no line feeds -- by construction this never scrolls anything off.
    let mut p = Parser::new(76, 203, SCROLLBACK_ROWS);
    for i in 0..500u16 {
        let row = 1 + (i % 76);
        p.process(format!("\x1b[{row};1Hrepaint {i}\x1b[K").as_bytes());
    }
    assert_eq!(ring_len(&mut p), 0);
}

#[test]
fn checkpoint_carries_the_capsules_ring_into_the_restored_parser() {
    // The capsule side (`capsule_win.rs`: CAPSULE_SCROLLBACK_ROWS = 200)
    // after a long newline-driven reply: 100 lines scrolled off a 24-row
    // screen sit in its ring.
    let mut source = Parser::new(24, 80, 200);
    feed_lines(&mut source, 124);
    let source_ring = ring_len(&mut source);
    assert!(source_ring >= 100, "source ring_len={source_ring}");
    let checkpoint = source.screen().checkpoint().unwrap();

    let mut target = Parser::new(76, 203, SCROLLBACK_ROWS);
    target.restore_screen(&checkpoint).unwrap();
    target.screen_mut().set_size(76, 203);
    let n = ring_len(&mut target);
    assert_eq!(n, source_ring, "restored ring_len={n}, checkpoint should carry the ring");
}

#[test]
fn alternate_screen_output_never_reaches_the_ring_so_a_checkpoint_carries_none() {
    // A TUI that switches to the alternate screen (DECSET 1049) scrolls the
    // alternate grid, which has no scrollback by design; the normal grid's
    // ring stays empty and so does every checkpoint taken while it runs.
    let mut source = Parser::new(24, 80, 200);
    source.process(b"\x1b[?1049h");
    feed_lines(&mut source, 124);
    assert_eq!(ring_len(&mut source), 0, "alt-screen output must not fill the normal ring");
    let checkpoint = source.screen().checkpoint().unwrap();

    let mut target = Parser::new(76, 203, SCROLLBACK_ROWS);
    target.restore_screen(&checkpoint).unwrap();
    assert_eq!(ring_len(&mut target), 0);
}
