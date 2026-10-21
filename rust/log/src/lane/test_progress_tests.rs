use super::*;

/// An ordinary holder (another thread's checkpoint, a polling snapshot) only delays a checkpoint, however long it is
/// descheduled: the record is kept and nothing is skipped. Fails while a busy ring is given up after a bounded few
/// attempts, which lost the checkpoints a test then waited for.
#[test]
fn an_ordinary_busy_ring_delays_a_checkpoint_and_never_drops_it() {
    let progress = std::sync::Arc::new(Progress::default());
    let held = progress.ring.lock().unwrap();
    let producer = std::thread::spawn({
        let progress = std::sync::Arc::clone(&progress);
        move || progress.note(Some(18), "sent.enqueue", "ok marker=18")
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    drop(held);
    producer.join().unwrap();
    let snapshot = progress.snapshot();
    assert_eq!(
        snapshot.skipped, 0,
        "an ordinary holder made the checkpoint skip\n{snapshot}"
    );
    assert!(
        snapshot.records.iter().any(|r| r.result == "ok marker=18"),
        "{snapshot}"
    );
}

#[test]
fn a_fixture_hold_skips_and_counts_checkpoints() {
    let progress = std::sync::Arc::new(Progress::default());
    progress.note(Some(1), "registered", "ok");
    let before = progress.snapshot().to_string();
    let held = progress.hold();
    let (entered, entry) = std::sync::mpsc::channel();
    let (finished, completion) = std::sync::mpsc::channel();
    let producer = std::thread::spawn({
        let progress = std::sync::Arc::clone(&progress);
        move || {
            entered.send(()).unwrap();
            for _ in 0..7 {
                progress.note(Some(2), "registered", "ok");
            }
            finished.send(()).unwrap();
        }
    });
    let observed_entry = entry
        .recv_timeout(std::time::Duration::from_secs(2))
        .is_ok();
    let completed_while_held = completion
        .recv_timeout(std::time::Duration::from_secs(2))
        .is_ok();
    drop(held);
    producer.join().unwrap();
    assert!(observed_entry, "checkpoint producer did not enter");
    assert!(
        completed_while_held,
        "checkpoint admission waited for the busy recorder"
    );
    let after = progress.snapshot().to_string();
    assert!(
        after.contains("skipped=7"),
        "known checkpoint attempts were not counted: {after}"
    );
    assert_eq!(
        before.lines().skip(1).collect::<Vec<_>>(),
        after.lines().skip(1).collect::<Vec<_>>()
    );
    assert!(
        after.contains("records=1 overwritten=0"),
        "busy admission changed the ring: {after}"
    );
    eprintln!("recorder-proof attempts=7 skipped=7 completed=while-held bodies=1");
}

#[test]
fn busy_snapshot_does_not_wait() {
    let progress = Progress::default();
    let _held = progress.ring.lock().unwrap();
    let snapshot = progress.snapshot();
    assert!(snapshot.unavailable);
    assert_eq!(
        snapshot.to_string(),
        "transport-progress snapshot unavailable skipped=0 reason=busy\n"
    );
}

#[test]
fn poisoned_snapshot_reports_unavailable_without_ring_counts() {
    let progress = Progress::default();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = progress.ring.lock().unwrap();
        panic!("poison the test-owned recorder");
    }));
    progress.note(None, "registered", "skipped");
    let snapshot = progress.snapshot();
    assert!(snapshot.unavailable);
    assert_eq!(snapshot.overwritten, None);
    assert_eq!(snapshot.skipped, 1);
    assert_eq!(
        snapshot.to_string(),
        "transport-progress snapshot unavailable skipped=1 reason=poisoned\n"
    );
}

#[test]
fn overwrite_keeps_the_last_256_even_after_a_connection_ends() {
    let progress = Progress::default();
    for id in 0..300 {
        progress.note(Some(id), "closed.enqueue", "ok");
    }
    let snapshot = progress.snapshot();
    assert_eq!(snapshot.overwritten, Some(44));
    assert_eq!(snapshot.skipped, 0);
    assert_eq!(snapshot.records.len(), 256);
    assert_eq!(snapshot.records[0].conn, Some(44));
    assert_eq!(snapshot.records[255].conn, Some(299));
}

fn fields(record: &Checkpoint) -> Vec<String> {
    record
        .to_string()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[test]
fn checkpoint_line_names_transport_caller_result_and_errno() {
    let progress = Progress::new("pipe");
    progress.note_with(
        None,
        "shutdown.result",
        "rc=-1",
        "rust/log/src/lane/socket_unix/client.rs::cancel",
        Some(107),
    );
    progress.note(Some(4), "registered", "ok");
    let snapshot = progress.snapshot();
    let first = fields(&snapshot.records[0]);
    assert_eq!(
        first[..3],
        ["transport-progress", "transport=pipe", "conn=pending"]
    );
    assert_eq!(first[3], "step=shutdown.result");
    assert!(first[4].starts_with("elapsed_ms="));
    assert_eq!(
        first[5..],
        [
            "caller=rust/log/src/lane/socket_unix/client.rs::cancel",
            "result=rc=-1",
            "errno=107"
        ]
    );
    let second = fields(&snapshot.records[1]);
    assert_eq!(second[2], "conn=4");
    assert!(
        second[5].starts_with("caller=rust/log/src/lane/test_progress.rs:"),
        "an unnamed caller is the site that noted: {}",
        second[5]
    );
    assert_eq!(second[6..], ["result=ok", "errno=none"]);
}

#[test]
fn exit_hold_blocks_once_and_panic_fires_once_at_the_exit_point() {
    let progress = Progress::default();
    let controls = Arc::new(Controls::default());
    let gate = controls.arm_exit_hold(7, Role::Reader);
    controls.arm_exit_panic(7, Role::Reader);
    let worker = std::thread::spawn({
        let controls = Arc::clone(&controls);
        move || {
            let progress = Progress::default();
            controls.exit_point(&progress, 7, Role::Reader);
        }
    });
    assert!(
        gate.wait_reached(Duration::from_secs(5)),
        "the worker never stopped at its hold"
    );
    assert!(!worker.is_finished(), "the hold did not hold");
    gate.release();
    assert!(
        worker.join().is_err(),
        "the armed panic did not fire after the release"
    );
    // Both were consumed: the same point now passes.
    controls.exit_point(&progress, 7, Role::Reader);
    controls.exit_point(&progress, 7, Role::Writer);
}

#[test]
fn barrier_is_one_shot_and_unarmed_points_pass() {
    let progress = Progress::default();
    let controls = Controls::default();
    controls.barrier_point(&progress, None, "registration.barrier");
    let gate = controls.arm_barrier("registration.barrier");
    std::thread::scope(|scope| {
        let waiting =
            scope.spawn(|| controls.barrier_point(&progress, Some(1), "registration.barrier"));
        assert!(gate.wait_reached(Duration::from_secs(5)));
        gate.release();
        waiting.join().unwrap();
    });
    controls.barrier_point(&progress, Some(2), "registration.barrier");
}
