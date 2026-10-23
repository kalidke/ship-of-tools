#![cfg(unix)]
//! A driver under an output flood (pass 6c-1a2 review): the row resyncs a viewer that cannot keep up, with a fresh
//! snapshot, and never drops an answer to its input, so the driver keeps its connection and every key is answered. The
//! client drops, rather than keeps without bound, the output that arrives while a key waits, and says so.

use sot_log::row::{socket_path_in, RowClient, RowEvent, RowRequest, RowRun, RowServer};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[test]
fn a_driver_that_pumps_slowly_under_a_flood_keeps_its_connection_and_gets_every_reply() {
    let dir = tempfile::tempdir().unwrap();
    // The challenge refuses a peer that started in the clock tick it is dialed in; a test binary is both ends.
    std::thread::sleep(Duration::from_millis(50));
    let path = socket_path_in(dir.path(), "0123456789abcdef").expect("a short path fits");
    let run = RowRun::new("run-one", "ready");
    let parser = Arc::new(Mutex::new(vt100_ctt::Parser::new(24, 80, 200)));
    let stop = Arc::new(AtomicBool::new(false));
    let looped = {
        let (run, parser, stop) = (Arc::clone(&run), Arc::clone(&parser), Arc::clone(&stop));
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let mut p = parser.lock().unwrap();
                run.tick(&p);
                for req in run.take_requests(16) {
                    match req {
                        RowRequest::Input { bytes, reply, .. } => {
                            let _ = reply.send(Ok(bytes.len()));
                        }
                        RowRequest::Resize { reply, .. } => {
                            let _ = reply.send(Ok(()));
                        }
                    }
                }
                // The flood: as much output as the loop can produce.
                // (The parser is not fed: only the row's fan-out is under test, and a debug parser is slower than a viewer.)
                let chunk = vec![b'x'; 60_000];
                run.output(&chunk, &p);
                drop(p);
                std::thread::sleep(Duration::from_micros(50));
            }
        })
    };
    let server = RowServer::bind(&path, Arc::clone(&run)).expect("bind");
    let mut driver = RowClient::connect(&path, "test", Duration::from_secs(5)).expect("connect");
    driver.attach(80, 24, true).expect("attach as the driver");
    let mut snapshots = 0;
    let started = Instant::now();
    let mut answered = 0;
    while started.elapsed() < Duration::from_secs(10) {
        // Pumps slowly, well under the flood's rate: some events, then a key.
        for _ in 0..40 {
            match driver.next_event(Duration::from_millis(5)) {
                Ok(Some(RowEvent::Snapshot { last: true, .. })) => snapshots += 1,
                Ok(_) => {}
                Err(e) => panic!("the row cut a driver that was typing, after {answered} answers: {e}"),
            }
        }
        std::thread::sleep(Duration::from_millis(150));
        match driver.send_input(b"k") {
            Ok(1) => answered += 1,
            other => panic!("a key was not answered, after {answered} answers: {other:?}"),
        }
        // As the client's worker does: output dropped while the key waited is made good by asking for the screen again.
        if driver.take_lost_output() {
            driver.attach(80, 24, true).expect("attach again for the screen as it is");
        }
    }
    assert!(answered >= 30, "{answered}");
    assert!(snapshots >= 2, "a viewer that fell behind is resynced with a fresh snapshot ({snapshots} seen, the attach's included)");
    stop.store(true, Ordering::Release);
    looped.join().unwrap();
    server.shutdown();
}
