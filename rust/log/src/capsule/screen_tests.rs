use super::*;

/// A volume with no room: every screen write fails, output keeps flowing, the failure is noted once for the streak,
/// and the next write that can succeed does.
#[test]
fn a_full_volume_never_stops_output() {
    let dir = std::env::temp_dir();
    let calls = Arc::new(Mutex::new(0u32));
    let full = Arc::new(AtomicBool::new(true));
    let writer = {
        let (calls, full) = (Arc::clone(&calls), Arc::clone(&full));
        Box::new(move |_: &std::path::Path, _: &[u8]| {
            *calls.lock().unwrap() += 1;
            if full.load(Ordering::SeqCst) {
                Err(std::io::Error::from(std::io::ErrorKind::StorageFull))
            } else {
                Ok(())
            }
        })
    };
    let t0 = Instant::now();
    let mut file = ScreenFile::with_writer(dir.join("screen"), t0, writer);
    let mut parser = vt100_ctt::Parser::new(24, 80, CAPSULE_SCROLLBACK_ROWS);
    for i in 1..=4u32 {
        parser.process(format!("line {i}\r\n").as_bytes());
        file.output_arrived();
        file.tick(&parser, t0 + SCREEN_PACE * i);
    }
    assert_eq!(*calls.lock().unwrap(), 4, "every pace tried the write again");
    assert_eq!(file.notes, 1, "one note for the whole failing streak");

    full.store(false, Ordering::SeqCst);
    parser.process(b"after\r\n");
    file.output_arrived();
    file.tick(&parser, t0 + SCREEN_PACE * 5);
    assert_eq!(*calls.lock().unwrap(), 5);
    file.output_arrived();
    file.tick(&parser, t0 + SCREEN_PACE * 5 + Duration::from_millis(10));
    assert_eq!(*calls.lock().unwrap(), 5, "inside the pace nothing is written");
}

fn text_of(parser: &vt100_ctt::Parser) -> String {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    (0..rows)
        .map(|r| (0..cols).filter_map(|c| screen.cell(r, c).map(|cell| cell.contents().to_string())).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
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
    file.flush(&parser, t0);

    let mut next = vt100_ctt::Parser::new(24, 80, CAPSULE_SCROLLBACK_ROWS);
    assert!(restore(&path, &mut next, 24, 80, UNIX_EPOCH + Duration::from_secs(1_700_000_000)));
    let text = text_of(&next);
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
    assert!(text_of(&parser).trim().is_empty());
}
