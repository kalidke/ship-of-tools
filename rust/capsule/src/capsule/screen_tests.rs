use super::*;

/// The screen's text with its history, oldest page first: one rule on every system, since on Windows `restore`
/// scrolls the retained screen and the restart line into the history.
fn text_of(parser: &mut vt100_ctt::Parser) -> String {
    let screen = parser.screen_mut();
    screen.set_scrollback(usize::MAX);
    let mut offset = screen.scrollback();
    let (rows, cols) = screen.size();
    let mut out = String::new();
    loop {
        screen.set_scrollback(offset);
        for r in 0..rows {
            out.extend((0..cols).filter_map(|c| screen.cell(r, c).map(|cell| cell.contents().to_string())));
            out.push('\n');
        }
        if offset == 0 {
            break;
        }
        offset = offset.saturating_sub(usize::from(rows));
    }
    screen.set_scrollback(0);
    out
}

/// The screen a leg saved is the screen the next leg starts with, under a restart line.
#[test]
fn a_saved_screen_restores_above_the_restart_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("screen");
    let t0 = Instant::now();
    let mut file = ScreenFile::new(path.clone(), t0);
    let mut parser = vt100_ctt::Parser::new(24, 80, CAPSULE_SCROLLBACK_ROWS);
    parser.process(b"first leg output\r\n");
    file.output_arrived();
    file.finish(&parser, t0, Duration::from_secs(5));

    let mut next = vt100_ctt::Parser::new(24, 80, CAPSULE_SCROLLBACK_ROWS);
    assert!(restore(&path, &mut next, 24, 80, UNIX_EPOCH + Duration::from_secs(1_700_000_000)));
    let text = text_of(&mut next);
    let old = text.find("first leg output").expect("the old screen is there");
    let line = text.find("[restarted 2023-11-14 22:13:20 UTC]").expect("the restart line is there");
    assert!(old < line, "the restart line is below the old screen: {text:?}");
}

/// A screen file that does not restore is removed and the screen starts empty.
#[test]
fn a_torn_screen_file_is_removed_and_the_screen_starts_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("screen");
    std::fs::write(&path, b"not a checkpoint").unwrap();
    let mut parser = vt100_ctt::Parser::new(24, 80, CAPSULE_SCROLLBACK_ROWS);
    assert!(!restore(&path, &mut parser, 24, 80, SystemTime::now()));
    assert!(!path.exists());
    assert!(text_of(&mut parser).trim().is_empty());
}

/// An agent that left its cursor at the top of the screen (or scrolling margins, or the alternate screen) does not
/// get the restart line written over its retained content: the line lands below it.
#[test]
fn the_restart_line_never_overwrites_retained_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("screen");
    let t0 = Instant::now();
    let mut file = ScreenFile::new(path.clone(), t0);
    let mut parser = vt100_ctt::Parser::new(10, 40, CAPSULE_SCROLLBACK_ROWS);
    parser.process(b"top row\r\nsecond row\r\nthird row\x1b[3;5r\x1b[H");
    file.output_arrived();
    file.finish(&parser, t0, Duration::from_secs(5));

    let mut next = vt100_ctt::Parser::new(10, 40, CAPSULE_SCROLLBACK_ROWS);
    assert!(restore(&path, &mut next, 10, 40, UNIX_EPOCH + Duration::from_secs(1_700_000_000)));
    let text = text_of(&mut next);
    for kept in ["top row", "second row", "third row"] {
        assert!(text.contains(kept), "{kept:?} was overwritten: {text:?}");
    }
    assert!(text.find("third row").unwrap() < text.find("[restarted").unwrap(), "{text:?}");
}

/// The input modes a program turned on (mouse reporting in any tracking mode and encoding, bracketed paste,
/// application cursor keys and keypad, a hidden cursor, the alternate screen) are not the next program's: a run over
/// its screen starts with them as a fresh terminal has them, in its own parser and in the checkpoint a viewer is
/// attached with, while the retained content and the restart line stay.
#[test]
fn a_new_run_starts_with_the_input_modes_reset() {
    for (tracking, encoding) in [("9", "1005"), ("1000", "1006"), ("1002", "1006"), ("1003", "1005")] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("screen");
        let t0 = Instant::now();
        let mut file = ScreenFile::new(path.clone(), t0);
        let mut parser = vt100_ctt::Parser::new(10, 40, CAPSULE_SCROLLBACK_ROWS);
        parser.process(b"kept row\r\n");
        parser.process(format!("\x1b[?{tracking}h\x1b[?{encoding}h\x1b[?2004h\x1b[?1h\x1b=\x1b[?25l\x1b[?1049h").as_bytes());
        let old = parser.screen();
        assert_ne!(old.mouse_protocol_mode(), vt100_ctt::MouseProtocolMode::None, "{tracking}: the old run tracks the mouse");
        assert_ne!(old.mouse_protocol_encoding(), vt100_ctt::MouseProtocolEncoding::Default, "{encoding}: the old run encodes it");
        assert!(old.bracketed_paste() && old.application_cursor() && old.application_keypad() && old.hide_cursor());
        file.output_arrived();
        file.finish(&parser, t0, Duration::from_secs(5));

        let mut next = vt100_ctt::Parser::new(10, 40, CAPSULE_SCROLLBACK_ROWS);
        assert!(restore(&path, &mut next, 10, 40, UNIX_EPOCH + Duration::from_secs(1_700_000_000)));
        let attached = {
            let mut viewer = vt100_ctt::Parser::new(10, 40, CAPSULE_SCROLLBACK_ROWS);
            viewer.restore_screen(&next.screen().checkpoint().unwrap()).unwrap();
            viewer
        };
        for (who, parser) in [("the new run's parser", &mut next), ("the checkpoint an attach is answered with", &mut { attached })] {
            let screen = parser.screen();
            assert_eq!(screen.mouse_protocol_mode(), vt100_ctt::MouseProtocolMode::None, "{tracking}/{encoding}: {who}: mouse reporting");
            assert_eq!(
                screen.mouse_protocol_encoding(),
                vt100_ctt::MouseProtocolEncoding::Default,
                "{tracking}/{encoding}: {who}: mouse encoding"
            );
            assert!(!screen.bracketed_paste(), "{tracking}/{encoding}: {who}: bracketed paste");
            assert!(!screen.application_cursor(), "{tracking}/{encoding}: {who}: application cursor keys");
            assert!(!screen.application_keypad(), "{tracking}/{encoding}: {who}: application keypad");
            assert!(!screen.hide_cursor(), "{tracking}/{encoding}: {who}: cursor hidden");
            assert!(!screen.alternate_screen(), "{tracking}/{encoding}: {who}: alternate screen");
            let text = text_of(parser);
            assert!(text.contains("kept row") && text.contains("[restarted 2023-11-14 22:13:20 UTC]"), "{who}: {text:?}");
        }
    }
}
