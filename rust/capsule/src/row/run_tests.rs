use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn driver(run: &Arc<RowRun>) -> ConnId {
    let (id, rx, slot) = run.open(Arc::new(|| {})).expect("a slot");
    std::mem::forget((rx, slot));
    run.take(id);
    id
}

#[test]
fn a_full_request_queue_refuses_input_as_busy() {
    let run = RowRun::new("r", "ready");
    run.set_bounds_for_test(Duration::from_secs(5), Duration::from_millis(400));
    let id = driver(&run);
    let busy = Arc::new(AtomicUsize::new(0));
    let workers: Vec<_> = (0..100)
        .map(|_| {
            let (run, busy) = (Arc::clone(&run), Arc::clone(&busy));
            std::thread::spawn(move || {
                if let Err(e) = run.input(id, vec![1], None) {
                    if e.contains("busy") {
                        busy.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
    assert!(busy.load(Ordering::SeqCst) >= 100 - REQUEST_QUEUE, "requests past the bound are refused as busy, not queued");
    assert!(run.take_requests(1000).len() <= REQUEST_QUEUE, "the queue never holds more than its bound");
}

#[test]
fn a_wake_is_coalesced_and_the_drain_is_under_a_quota() {
    let run = RowRun::new("r", "ready");
    run.set_bounds_for_test(Duration::from_secs(5), Duration::from_millis(100));
    let wakes = Arc::new(AtomicUsize::new(0));
    {
        let wakes = Arc::clone(&wakes);
        run.set_wake(Box::new(move || {
            wakes.fetch_add(1, Ordering::SeqCst);
        }));
    }
    let id = driver(&run);
    let workers: Vec<_> = (0..10)
        .map(|_| {
            let run = Arc::clone(&run);
            std::thread::spawn(move || {
                let _ = run.input(id, vec![1], None);
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(wakes.load(Ordering::SeqCst), 1, "ten requests, one pending wake");
    assert_eq!(run.take_requests(4).len(), 4, "a pass drains at most its quota");
    assert_eq!(wakes.load(Ordering::SeqCst), 2, "what is left arms the wake again");
    for w in workers {
        w.join().unwrap();
    }
}

#[test]
fn a_viewer_that_falls_behind_is_resynced_with_one_screen_not_cut() {
    let run = RowRun::new("r", "ready");
    let cuts = Arc::new(AtomicUsize::new(0));
    let closer = {
        let cuts = Arc::clone(&cuts);
        Arc::new(move || {
            cuts.fetch_add(1, Ordering::SeqCst);
        })
    };
    let (id, queue, slot) = run.open(closer).expect("a slot");
    let parser = vt100_ctt::Parser::new(24, 80, 0);
    run.attach(id, 2, 80, 24, false).expect("attach");
    run.tick(&parser);
    // The writer never pops: every QUEUE_FRAMES outputs resync it, far more often than ANSWER_FRAMES.
    for _ in 0..(ANSWER_FRAMES + 8) * (QUEUE_FRAMES + 1) {
        run.output(b"y\n", &parser);
    }
    assert_eq!(cuts.load(Ordering::SeqCst), 0, "a slow viewer is resynced, never cut");
    let screens = queue.lock().items.iter().filter(|(Out::Frame(f, _), _)| f.op == OP_SNAPSHOT).count();
    assert_eq!(screens, 1, "a resync supersedes the screen the writer has not begun");
    drop(slot);
}

/// A viewer is sent the screen only at a parser ground state (never inside an escape sequence), as the screen that holds
/// every byte the parser has seen, and the output after it in order.
#[test]
fn a_viewer_joins_only_at_a_ground_state_and_is_then_sent_the_output_in_order() {
    let run = RowRun::new("r", "ready");
    let (id, queue, slot) = run.open(Arc::new(|| {})).expect("a slot");
    let mut parser = vt100_ctt::Parser::new(24, 80, 0);
    // Half an escape sequence: the parser is not at a ground state.
    parser.process(b"one\r\n\x1b[3");
    assert!(!parser.is_ground(), "the premise: a split escape sequence is not a ground state");
    run.attach(id, 2, 80, 24, false).expect("attach");
    run.tick(&parser);
    let ops = |queue: &OutQueue| -> Vec<String> {
        queue.lock().items.iter().map(|(Out::Frame(f, _), _)| f.op.clone()).collect()
    };
    assert_eq!(ops(&queue), [OP_ATTACH], "no screen is cut inside an escape sequence");
    // The chunk that ends the sequence is already in the parser when the screen is cut, so it is in the screen.
    parser.process(b"1mtwo");
    run.output(b"1mtwo", &parser);
    assert_eq!(ops(&queue), [OP_ATTACH, OP_SNAPSHOT], "the screen is sent at the first ground state");
    parser.process(b"\r\nthree");
    run.output(b"\r\nthree", &parser);
    assert_eq!(ops(&queue), [OP_ATTACH, OP_SNAPSHOT, OP_OUTPUT], "the output after the screen follows in order");
    let items = queue.lock();
    let screen = items.items.iter().find_map(|(Out::Frame(f, blob), _)| (f.op == OP_SNAPSHOT).then(|| blob.clone())).flatten();
    let mut restored = vt100_ctt::Parser::new(24, 80, 0);
    restored.restore_screen(&screen.expect("the screen has its bytes")).expect("the screen restores");
    let text = restored.screen().contents();
    assert!(text.contains("one") && text.contains("two") && !text.contains("three"), "the screen is the parser's at the cut: {text:?}");
    let output = items.items.iter().find_map(|(Out::Frame(f, blob), _)| (f.op == OP_OUTPUT).then(|| blob.clone())).flatten();
    assert_eq!(output.as_deref(), Some(&b"\r\nthree"[..]));
    drop(items);
    drop(slot);
}

/// A connection whose reader has ended is still registered while its writer may be flushing, so a shutdown's
/// `close_all` reaches its stream: a peer that never reads cannot hold the supervisor's exit.
#[test]
fn a_closed_connection_stays_reachable_until_its_threads_have_ended() {
    let run = RowRun::new("r", "ready");
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = Arc::clone(&closed);
    let (id, rx, slot) = run.open(Arc::new(move || seen.store(true, Ordering::SeqCst))).expect("a slot");
    std::mem::forget((rx, slot));
    run.close(id);
    run.close_all();
    assert!(closed.load(Ordering::SeqCst), "close_all did not reach a connection whose writer has yet to end");
}

/// A management whose status the test sets.
struct Scripted(Mutex<StatusRes>);

impl Scripted {
    fn new(phase: &str, run: Option<&str>) -> Arc<Self> {
        Arc::new(Scripted(Mutex::new(StatusRes { phase: phase.into(), run: run.map(str::to_string) })))
    }
    fn set(&self, phase: &str, run: Option<&str>) {
        *self.0.lock().unwrap() = StatusRes { phase: phase.into(), run: run.map(str::to_string) };
    }
}

impl Management for Arc<Scripted> {
    fn status(&self) -> StatusRes {
        self.0.lock().unwrap().clone()
    }
    fn end_run(&self, _: &str, _: &str) -> super::super::proto::EndRunRes {
        unreachable!()
    }
    fn reset(&self) -> super::super::proto::ResetRes {
        unreachable!()
    }
    fn stop(&self, _: ConnId) -> super::super::proto::StopRes {
        unreachable!()
    }
    fn conn_gone(&self, _: ConnId) {}
}

/// The frames a watcher's queue holds, as `(op, payload)`, oldest first.
fn drained(queue: &OutQueue) -> Vec<(String, serde_json::Value)> {
    let mut frames = Vec::new();
    queue.finish();
    while let Some(Out::Frame(frame, _)) = queue.pop() {
        frames.push((frame.op, frame.payload));
    }
    frames
}

/// A watch answers the status last told; after it, one `row.phase` leaves for each report that differs, in the order the
/// reports were handed over, and none for a status that is the same.
#[test]
fn a_watch_is_told_each_change_once_and_in_order() {
    let run = RowRun::new("r", "starting");
    let status = Scripted::new("starting", None);
    run.set_management(Box::new(Arc::clone(&status)));
    let report = |phase: &str, run: Option<&str>| StatusRes { phase: phase.into(), run: run.map(str::to_string) };
    run.publish(report("starting", None));
    let (id, queue, slot) = run.open(Arc::new(|| {})).expect("a slot");
    run.watch(id, 7, &Arc::clone(&status));
    run.publish(report("starting", None));
    run.publish(report("starting", Some("run-one")));
    run.publish(report("starting", Some("run-one")));
    run.publish(report("ready", Some("run-one")));
    let frames = drained(&queue);
    let ops: Vec<_> = frames.iter().map(|(op, _)| op.as_str()).collect();
    assert_eq!(ops, ["row.watch", "row.phase", "row.phase"], "{frames:?}");
    assert_eq!(frames[0].1["phase"], "starting");
    assert_eq!(frames[1].1["run"], "run-one");
    assert_eq!(frames[2].1["phase"], "ready");
    drop(slot);
}

/// A row nobody watches asks its management for nothing: the loop's every tick costs nothing.
#[test]
fn a_tick_asks_nothing_when_nothing_watches() {
    struct Panics;
    impl Management for Panics {
        fn status(&self) -> StatusRes {
            panic!("the status was read for no watcher")
        }
        fn poll(&self) {
            panic!("the clock was read for no watcher")
        }
        fn end_run(&self, _: &str, _: &str) -> super::super::proto::EndRunRes {
            unreachable!()
        }
        fn reset(&self) -> super::super::proto::ResetRes {
            unreachable!()
        }
        fn stop(&self, _: ConnId) -> super::super::proto::StopRes {
            unreachable!()
        }
        fn conn_gone(&self, _: ConnId) {}
    }
    let run = RowRun::new("r", "ready");
    run.set_management(Box::new(Panics));
    let (_id, _queue, _slot) = run.open(Arc::new(|| {})).expect("a slot");
    run.tick(&vt100_ctt::Parser::new(24, 80, 0));
}

/// `row.exiting` is the watcher's last frame, and the server's shutdown, which aborts what is still queued, never loses
/// it: `exiting` waits, bounded, until the watcher's connection has been released.
#[test]
fn exiting_is_sent_before_the_shutdown_and_waits_for_the_watchers() {
    let run = RowRun::new("r", "ready");
    let status = Scripted::new("ready", Some("run-one"));
    run.set_management(Box::new(Arc::clone(&status)));
    let (id, queue, slot) = run.open(Arc::new(|| {})).expect("a slot");
    run.watch(id, 1, &Arc::clone(&status));
    let writer_queue = Arc::clone(&queue);
    let writer_run = Arc::clone(&run);
    let writer = std::thread::spawn(move || {
        // The connection's threads: the writer drains the queue, then the reader's end releases the connection.
        let mut ops = Vec::new();
        while let Some(Out::Frame(frame, _)) = writer_queue.pop() {
            std::thread::sleep(Duration::from_millis(100));
            ops.push((frame.op.clone(), frame.payload.clone()));
        }
        writer_run.close(id);
        writer_run.released(id);
        ops
    });
    run.exiting(76, Duration::from_secs(5));
    // The connection is gone by the time `exiting` returns, so the shutdown that follows aborts nothing of it.
    run.close_all();
    let ops = writer.join().unwrap();
    drop(slot);
    let names: Vec<_> = ops.iter().map(|(op, _)| op.as_str()).collect();
    assert_eq!(names, ["row.watch", "row.exiting"], "{ops:?}");
    assert_eq!(ops[1].1["code"], 76);
}

/// A watcher that never drains cannot hold the exit past its bound.
#[test]
fn exiting_gives_up_on_a_watcher_that_does_not_drain() {
    let run = RowRun::new("r", "ready");
    let status = Scripted::new("ready", Some("run-one"));
    run.set_management(Box::new(Arc::clone(&status)));
    let (id, _queue, slot) = run.open(Arc::new(|| {})).expect("a slot");
    run.watch(id, 1, &Arc::clone(&status));
    let started = std::time::Instant::now();
    run.exiting(0, Duration::from_millis(200));
    assert!(started.elapsed() < Duration::from_secs(2), "the exit waited past its bound");
    std::mem::forget(slot);
}

/// Once the exit code is committed, a watch that is admitted is told it: its status, then `row.exiting` with the code, then
/// the end of its stream.
#[test]
fn a_watch_admitted_during_the_exit_is_told_the_code() {
    let run = RowRun::new("r", "ready");
    let status = Scripted::new("ready", Some("run-one"));
    run.set_management(Box::new(Arc::clone(&status)));
    let (first, _first_queue, first_slot) = run.open(Arc::new(|| {})).expect("a slot");
    run.watch(first, 1, &Arc::clone(&status));
    // The first watcher never drains, so `exiting` is still waiting when the second arrives.
    let exiting = {
        let run = Arc::clone(&run);
        std::thread::spawn(move || run.exiting(76, Duration::from_secs(3)))
    };
    std::thread::sleep(Duration::from_millis(300));
    let (late, late_queue, late_slot) = run.open(Arc::new(|| {})).expect("a slot");
    run.watch(late, 2, &Arc::clone(&status));
    let frames = drained(&late_queue);
    let ops: Vec<_> = frames.iter().map(|(op, _)| op.as_str()).collect();
    assert_eq!(ops, ["row.watch", "row.exiting"], "{frames:?}");
    assert_eq!(frames[1].1["code"], 76);
    exiting.join().unwrap();
    std::mem::forget((first_slot, late_slot));
}
