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
