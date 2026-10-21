/// Wakes before a drain post one event; a wake after the drain posts again. Four threads then queue work and wake
/// while a loop that drains only when an event arrives drains every item: a wake lost between a queued item and
/// its drain leaves the loop waiting, and the 10 s bound fails it.
#[test]
fn main_wake_coalesces_and_never_loses_a_wake() {
    use super::MainWake;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let posts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&posts);
    let wake = MainWake::posting(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    for _ in 0..5 {
        wake.wake();
    }
    assert_eq!(
        posts.load(Ordering::SeqCst),
        1,
        "five wakes before a drain post one event"
    );
    assert!(wake.take());
    assert!(!wake.take(), "the drain cleared the wake");
    wake.wake();
    assert_eq!(
        posts.load(Ordering::SeqCst),
        2,
        "a wake after the drain posts again"
    );

    let (posted_tx, posted_rx) = std::sync::mpsc::channel::<()>();
    let posted_tx = std::sync::Mutex::new(posted_tx);
    let wake = MainWake::posting(move || {
        let _ = posted_tx.lock().unwrap().send(());
    });
    let (work_tx, work_rx) = std::sync::mpsc::channel::<u32>();
    const SENDERS: u32 = 4;
    const EACH: u32 = 10_000;
    let senders: Vec<_> = (0..SENDERS)
        .map(|_| {
            let (wake, work_tx) = (wake.clone(), work_tx.clone());
            std::thread::spawn(move || {
                for i in 0..EACH {
                    work_tx.send(i).unwrap();
                    wake.wake();
                }
            })
        })
        .collect();
    drop(work_tx);
    let mut drained = 0;
    while drained < SENDERS * EACH {
        posted_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap_or_else(|_| {
                panic!("{drained} items drained, and the rest have no wake pending")
            });
        assert!(wake.take(), "each event stands for one pending wake");
        while work_rx.try_recv().is_ok() {
            drained += 1;
        }
    }
    for sender in senders {
        sender.join().unwrap();
    }
    println!("main-wake: {drained} items from {SENDERS} threads drained");
}
