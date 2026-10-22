//! The screen file's disk I/O never reaches the output path (D2, review round).

use super::*;
use std::sync::atomic::AtomicU32;

/// Drives a shell under a leg with a replaced screen write, types two commands, and returns whether the second one's
/// output reached a watcher before `finish` was called.
fn second_output_arrives(write: Box<dyn FnMut(&std::path::Path, &[u8]) -> std::io::Result<()> + Send>) -> bool {
    let dir = tempfile::tempdir().unwrap();
    capsule::set_screen_hook_for_test(write, Duration::from_millis(100));
    let cfg = config(dir.path(), "screenio1", vec![SHELL_ARGV.to_string()], 80, 25);
    let transport = TestTransport::new();
    let (tx, rx) = mpsc::channel();
    let run_transport = transport.clone();
    let handle = std::thread::spawn(move || {
        let mut t = run_transport;
        capsule::run::<P>(cfg, rx, &mut t)
    });
    const A: ConnId = 1;
    transport.open(A);
    transport.feed(A, frame::hello());
    let mut watcher = FrameWatcher::new(&transport);
    watcher.wait_for("hello_ok", A, Duration::from_secs(10), |f| {
        matches!(f, wire::DecodedFrame::AttachServer(wire::AttachServer::HelloOk { .. })).then_some(())
    });
    transport.feed(A, frame::attach("driver"));
    watcher.collect_checkpoint("checkpoint", A, Duration::from_secs(10));
    transport.feed(A, frame::take("driver"));
    let epoch = watcher.wait_for("take_ok", A, Duration::from_secs(10), |f| match f {
        wire::DecodedFrame::AttachServer(wire::AttachServer::TakeOk { take_epoch }) => Some(*take_epoch),
        _ => None,
    });
    transport.feed(A, frame::input("driver", epoch, [1; 16], b"echo SOT_SCREEN_ONE_71\r\n"));
    // Let the first screen write start (and, in the stalled case, block).
    std::thread::sleep(Duration::from_millis(600));
    transport.feed(A, frame::input("driver", epoch, [2; 16], b"echo SOT_SCREEN_TWO_72\r\n"));
    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut arrived = false;
    while Instant::now() < deadline {
        let chunk = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            watcher.wait_for("output", A, Duration::from_millis(500), |f| match f {
                wire::DecodedFrame::AttachServer(wire::AttachServer::Output { bytes }) => {
                    Some(String::from_utf8_lossy(bytes).into_owned())
                }
                _ => None,
            })
        }));
        if let Ok(c) = chunk {
            seen.push_str(&c);
            if seen.matches("SOT_SCREEN_TWO_72").count() >= 2 {
                arrived = true;
                break;
            }
        }
    }
    tx.send(Command::Kill).unwrap();
    // The stalled writer (if any) is released by the caller's closure when `Kill` is sent; the leg must end.
    let _ = wait_for_join(handle, Duration::from_secs(30));
    arrived
}

/// A screen write that blocks (a stalled disk) does not stall output.
#[test]
fn a_stalled_screen_write_never_stalls_output() {
    let _serial = serial();
    let release = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicBool::new(false));
    let (r, st) = (Arc::clone(&release), Arc::clone(&started));
    let writer = Box::new(move |_: &std::path::Path, _: &[u8]| {
        st.store(true, Ordering::SeqCst);
        while !r.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    });
    let r2 = Arc::clone(&release);
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(12));
        r2.store(true, Ordering::SeqCst);
    });
    let arrived = second_output_arrives(writer);
    release.store(true, Ordering::SeqCst);
    let _ = releaser.join();
    assert!(started.load(Ordering::SeqCst), "the screen write never started");
    assert!(arrived, "output stopped while a screen write was blocked");
}

/// Every screen write fails with a full volume, and output keeps arriving; the leg keeps trying.
#[test]
fn a_full_volume_never_stops_output() {
    let _serial = serial();
    let calls = Arc::new(AtomicU32::new(0));
    let c = Arc::clone(&calls);
    let writer = Box::new(move |_: &std::path::Path, _: &[u8]| {
        c.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::from(std::io::ErrorKind::StorageFull))
    });
    let arrived = second_output_arrives(writer);
    assert!(arrived, "output stopped while every screen write failed");
    assert!(calls.load(Ordering::SeqCst) >= 2, "the leg did not keep trying: {} writes", calls.load(Ordering::SeqCst));
}
