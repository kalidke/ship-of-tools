use super::*;
use sot_platform::test_isolated::run_isolated;

#[test]
fn held_book_does_not_extend_forced_delivery() {
    if !run_isolated("lease::delivery_tests::held_book_does_not_extend_forced_delivery") { return; }
    let leases = Leases::new(true, vec![]);
    let held = leases.clone();
    let (ready, entered) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let owner = std::thread::spawn(move || {
        let _book = held.book.lock().unwrap();
        ready.send(()).unwrap();
        let _ = gate.recv_timeout(Duration::from_millis(2400));
    });
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let log = sot_log::test_log::capture();
    let start = Instant::now();
    leases.deliver_queued(LEAVE_WRITE_WAIT);
    let elapsed = start.elapsed();
    let _ = release.send(());
    owner.join().unwrap();
    assert!(elapsed < Duration::from_millis(1500), "Book acquisition exceeded total deadline: {elapsed:?}");
    assert!(log.text().contains("lease book stayed locked"), "missing lock-contention warning");
    println!("forced delivery: held Book returned within 1.5 seconds");
}

#[test]
fn frozen_worker_and_multiple_holders_share_one_deadline() {
    if !run_isolated("lease::delivery_tests::frozen_worker_and_multiple_holders_share_one_deadline") { return; }
    use super::{grant_tests::bind, leave_tests::{leave_fake, logged, is_leave, finish}};
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let leases = Leases::new(false, vec![]);
    let mut peers = Vec::new();
    runtime.block_on(async {
        for host in ["pending-a", "pending-b"] {
            let (listener, path) = bind("forcedclock");
            let (log, _, peer) = leave_fake(listener, None, None, Duration::ZERO);
            tokio::time::timeout(Duration::from_secs(5), leases.before_data_connection(&host.to_string(), &path, None, |_| {})).await.unwrap().unwrap();
            peers.push((log, peer));
        }
    });
    let fast = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let fast_peer = fast.block_on(async {
        let (listener, path) = bind("forcedready");
        let peer = leave_fake(listener, None, None, Duration::ZERO);
        tokio::time::timeout(Duration::from_secs(5), leases.before_data_connection(&"ready".to_string(), &path, None, |_| {})).await.unwrap().unwrap();
        peer
    });
    let (closed, rx) = mpsc::unbounded_channel();
    drop(rx);
    leases.set(&"closed".to_string(), Standing::Granted { state_root: None }, Some(closed));
    let (ready, entered) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    runtime.spawn(async move {
        ready.send(()).unwrap();
        let _ = gate.recv_timeout(Duration::from_secs(3));
    });
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let keep = leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).unwrap();
    let close = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
    let log = sot_log::test_log::capture();
    let start = Instant::now();
    leases.deliver_queued(LEAVE_WRITE_WAIT);
    let elapsed = start.elapsed();
    let _ = release.send(());
    runtime.block_on(async {
        for (log, _) in &peers {
            let seen = logged(log, 2).await;
            assert!(is_leave(&seen[0], "keep") && is_leave(&seen[1], "close"));
        }
    });
    drop((keep, close, leases));
    runtime.block_on(async { for (log, peer) in peers { finish(peer, &log).await; } });
    fast.block_on(async { finish(fast_peer.2, &fast_peer.0).await; });
    runtime.shutdown_timeout(LEAVE_WRITE_WAIT);
    fast.shutdown_timeout(LEAVE_WRITE_WAIT);
    assert!(elapsed < Duration::from_millis(1500), "transport worker extended forced delivery: {elapsed:?}");
    let text = log.text();
    assert!(text.contains("pending-a") && text.contains("pending-b") && text.contains("closed"), "unconfirmed holders missing: {text}");
    assert!(!text.contains("ready"), "written holder was reported unconfirmed: {text}");
    assert_eq!(text.matches("exiting before every queued leave was written").count(), 1);
    println!("forced delivery: frozen worker and mixed holders returned within 1.5 seconds");
}
