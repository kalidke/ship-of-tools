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
                if let Err(e) = run.input(id, vec![1]) {
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
                let _ = run.input(id, vec![1]);
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
