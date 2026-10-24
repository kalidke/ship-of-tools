//! What a viewer on the row socket sees of a real run: the agent's output, a driver's input written once, a watcher's
//! refused.

use super::rowside::{read_until, read_until_contains, RowSide};
use super::*;
use sot_capsule::row::RowError;

/// A driver's input reaches the terminal and is answered as written; a watcher's is refused by name and writes
/// nothing (the shell echoes the driver's marker and never the watcher's).
#[test]
fn a_drivers_input_reaches_the_terminal_and_a_watchers_is_refused() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let side = RowSide::new();
    let mut cfg = config(dir.path(), "rowinput1", vec![SHELL_ARGV.to_string()], 80, 25);
    cfg.signals = Some(Arc::clone(&side.signals));
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || capsule::run::<P>(cfg, rx));

    let mut driver = side.driver(80, 25);
    let mut watcher = side.watcher(80, 25);
    let refused = watcher.send_input(b"echo SOT_WATCHER_INPUT_3\r").unwrap_err();
    assert!(matches!(refused, RowError::Refused(_)), "a watcher's input is refused by name: {refused:?}");
    let written = driver.send_input(b"echo SOT_DRIVER_INPUT_4\r").expect("the driver's input is answered");
    assert_eq!(written, b"echo SOT_DRIVER_INPUT_4\r".len());
    // The terminal echoes what was typed and the shell prints it again: the marker shows twice.
    let seen = read_until(&mut watcher, "the driver's marker twice", Duration::from_secs(20), |seen| {
        seen.matches("SOT_DRIVER_INPUT_4").count() >= 2
    });
    assert!(!seen.contains("SOT_WATCHER_INPUT_3"), "a watcher's refused input was written: {seen:?}");
    // The driver sees the same output.
    read_until_contains(&mut driver, "SOT_DRIVER_INPUT_4", Duration::from_secs(10));

    tx.send(Command::Kill).unwrap();
    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
}

/// An end retires the row's request queue: input sent once a requested end is in progress is answered `run ended` at
/// once, not left to time out as `delivery unknown` while the run tears down and the supervisor rests.
#[test]
fn input_sent_while_a_requested_end_is_in_progress_is_answered_run_ended() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let side = RowSide::new();
    let mut cfg = config(dir.path(), "rowend1", vec![SHELL_ARGV.to_string()], 80, 25);
    cfg.signals = Some(Arc::clone(&side.signals));
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || capsule::run::<P>(cfg, rx));

    let mut driver = side.driver(80, 25);
    driver.send_input(b"echo SOT_BEFORE_END_5\r").expect("input before the end is written");
    tx.send(Command::Kill).unwrap();
    // Input typed from here on is written until the loop sees the end, and answered `run ended` after it.
    let started = Instant::now();
    let refusal = loop {
        match driver.send_input(b"x") {
            Ok(_) => assert!(started.elapsed() < Duration::from_secs(10), "the end never closed the row to input"),
            Err(e) => break e,
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        matches!(&refusal, RowError::Refused(m) if m.contains("run ended")),
        "input at the end is answered `run ended`, got {refusal:?}"
    );
    assert!(started.elapsed() < Duration::from_millis(1500), "the answer waited out the input bound: {:?}", started.elapsed());

    let summary = wait_for_join(handle, Duration::from_secs(30))
        .expect("run did not return within the teardown bound")
        .unwrap();
    assert_eq!(summary.exit_kind, ExitKind::Requested);
}
