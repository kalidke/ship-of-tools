//! The screen file's disk I/O never reaches the output path (D2, review round).

use super::*;
use std::sync::atomic::AtomicU32;

/// Drives a shell under a leg with a replaced screen write, types two commands, and returns whether the second one's
/// output reached a viewer before `finish` was called.
fn second_output_arrives(write: Box<dyn FnMut(&std::path::Path, &[u8]) -> std::io::Result<()> + Send>) -> bool {
    let dir = tempfile::tempdir().unwrap();
    capsule::set_screen_hook_for_test(write, Duration::from_millis(100));
    let side = rowside::RowSide::new();
    let mut cfg = config(dir.path(), "screenio1", vec![SHELL_ARGV.to_string()], 80, 25);
    cfg.signals = Some(Arc::clone(&side.signals));
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || capsule::run::<P>(cfg, rx));
    let mut driver = side.driver(80, 25);
    driver.send_input(b"echo SOT_SCREEN_ONE_71\r\n").expect("the driver types");
    // Let the first screen write start (and, in the stalled case, block).
    std::thread::sleep(Duration::from_millis(600));
    driver.send_input(b"echo SOT_SCREEN_TWO_72\r\n").expect("the driver types");
    // The terminal echoes the command and the shell prints it again: the marker shows twice.
    let arrived = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rowside::read_until(&mut driver, "the second command's output", Duration::from_secs(5), |seen| {
            seen.matches("SOT_SCREEN_TWO_72").count() >= 2
        })
    }))
    .is_ok();
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

/// A run over a state folder that holds an earlier run's screen starts its parser from it: a viewer that attaches to the
/// new run is shown the old screen with a restart line below it, before the agent has written a byte.
#[test]
fn a_run_over_a_saved_screen_shows_it_above_a_restart_line() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let marker = "SOT_SAVED_SCREEN_81";
    let first = config(dir.path(), "restart1", shell_command(&format!("echo {marker}")), 80, 25);
    let (_tx, rx) = mpsc::channel();
    capsule::run::<P>(first, rx).expect("the first run ends on its own");
    assert!(saved_screen_text(dir.path(), "restart1").contains(marker), "the first run saved its screen");

    let side = rowside::RowSide::new();
    let mut second = config(dir.path(), "restart1", vec![SHELL_ARGV.to_string()], 80, 25);
    second.signals = Some(Arc::clone(&side.signals));
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || capsule::run::<P>(second, rx));
    let mut viewer = side.watcher(80, 25);
    let seen = rowside::read_until(&mut viewer, "the old screen above the restart line", Duration::from_secs(20), |seen| {
        seen.contains("[restarted") && seen.contains(marker)
    });
    assert!(seen.find(marker).unwrap() < seen.find("[restarted").unwrap(), "the restart line is below the old screen: {seen:?}");
    tx.send(Command::Kill).unwrap();
    let _ = wait_for_join(handle, Duration::from_secs(30)).expect("the second run ends");
}
