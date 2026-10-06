//! Lease tests: leaving, and what the daemon is told on the way out.

use super::grant_tests::{bind, read_handoff, Bound};
use super::*;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// A daemon that grants (once `gate` has signalled the request and been
/// opened, when given), then logs each frame the window writes, and
/// `eof` the moment the stream ends, concurrently with its replies. It
/// answers each leave whose intent is `reply_to`, after `delay`, with the
/// sentinel `not_ended: 7`, and marks it replied only once that is written.
fn leave_fake(
    listener: Bound,
    gate: Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>,
    reply_to: Option<&'static str>,
    delay: Duration,
) -> (Arc<std::sync::Mutex<Vec<String>>>, Arc<std::sync::atomic::AtomicBool>, tokio::task::JoinHandle<()>) {
    let log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let replied = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (l, r) = (log.clone(), replied.clone());
    let task = tokio::spawn(async move {
        let conn = listener.accept().await.unwrap();
        let (rx, mut tx) = conn.split();
        let mut rx = codec::buffered(rx);
        let (_, req) = read_handoff(&mut rx, &mut tx).await.expect("a handoff");
        if let Some((asked, open)) = gate {
            asked.send(()).unwrap();
            open.await.unwrap();
        }
        let granted = serde_json::json!({"outcome": "granted"});
        codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, granted), None).await.unwrap();
        let (want_tx, mut want_rx) = mpsc::unbounded_channel::<u64>();
        let reader = async move {
            while let Ok((f, _)) = codec::read_frame(&mut rx).await {
                l.lock().unwrap().push(format!("{} {}", f.op, f.payload));
                if reply_to.is_some_and(|i| f.payload["intent"] == i) {
                    want_tx.send(f.id).unwrap();
                }
            }
            l.lock().unwrap().push("eof".to_string());
        };
        let writer = async move {
            while let Some(id) = want_rx.recv().await {
                tokio::time::sleep(delay).await;
                let res = Frame::res(id, op::FE_LEAVING, serde_json::json!({"not_ended": 7}));
                codec::write_frame(&mut tx, &res, None).await.unwrap();
                r.store(true, Ordering::SeqCst);
            }
        };
        tokio::join!(reader, writer);
    });
    (log, replied, task)
}

/// Poll the way the event loop does until the step is not a wait (3 s at most).
async fn poll_out(leaving: &mut Leaving) -> LeaveStep {
    for _ in 0..150 {
        let step = leaving.poll(Instant::now());
        if !matches!(step, LeaveStep::Wait(_)) {
            return step;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    leaving.poll(Instant::now())
}

/// The fake's log once it has `n` entries (1 s at most).
async fn logged(log: &Arc<std::sync::Mutex<Vec<String>>>, n: usize) -> Vec<String> {
    for _ in 0..100 {
        if log.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    log.lock().unwrap().clone()
}

fn is_leave(entry: &str, intent: &str) -> bool {
    entry.starts_with(op::FE_LEAVING) && entry.contains(&format!(r#""intent":"{intent}""#))
}

/// Join the fake once the window's side is dropped (a panic in it fails
/// the test); its whole log, which ends with `eof`.
async fn finish(fake: tokio::task::JoinHandle<()>, log: &Arc<std::sync::Mutex<Vec<String>>>) -> Vec<String> {
    tokio::time::timeout(Duration::from_secs(5), fake).await.expect("the fake never saw eof").unwrap();
    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.last().map(String::as_str), Some("eof"), "{seen:?}");
    seen
}

#[tokio::test]
async fn keep_reaches_the_daemon_before_eof() {
    let (listener, path) = bind("keepleave");
    let (log, replied, fake) = leave_fake(listener, None, Some("keep"), Duration::from_millis(300));
    let host = "local".to_string();
    let leases = Leases::new(false, vec![host.clone()]);
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
    let mut leaving = leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).unwrap();
    assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)));
    let seen = logged(&log, 1).await;
    assert!(seen.len() == 1 && is_leave(&seen[0], "keep"), "the leave line reaches the daemon before any eof: {seen:?}");
    assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "no reply yet");
    assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show);
    assert!(replied.load(Ordering::SeqCst), "done only after the reply");
    assert_eq!(leaving.line(), not_ended_line(7), "the window received the reply's own count");
    drop((leaving, leases));
    assert_eq!(finish(fake, &log).await.len(), 2);
}

#[tokio::test]
async fn close_waits_for_its_reply() {
    let (listener, path) = bind("closeleave");
    let (log, replied, fake) = leave_fake(listener, None, Some("close"), Duration::from_millis(300));
    let host = "local".to_string();
    let leases = Leases::new(false, vec![host.clone()]);
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
    let mut leaving = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
    assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)));
    let seen = logged(&log, 1).await;
    assert!(seen.len() == 1 && is_leave(&seen[0], "close"), "the leave line reaches the daemon before any eof: {seen:?}");
    assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "no reply yet");
    assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show);
    assert!(replied.load(Ordering::SeqCst), "done only after the reply");
    assert_eq!(leaving.line(), not_ended_line(7), "the window received the reply's own count");
    drop((leaving, leases));
    assert_eq!(finish(fake, &log).await.len(), 2);

    // A withheld reply ends the wait at the close-ack bound itself.
    let (listener, path) = bind("closenoreply");
    let (log, replied, fake) = leave_fake(listener, None, None, Duration::ZERO);
    let leases = Leases::new(false, vec![host.clone()]);
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
    let t0 = Instant::now();
    let mut leaving = leases.leave_all(LeaveIntent::Close, 0, t0).unwrap();
    let seen = logged(&log, 1).await;
    assert!(seen.len() == 1 && is_leave(&seen[0], "close"), "{seen:?}");
    let ms = Duration::from_millis;
    assert!(matches!(leaving.poll(t0 + lease::CLOSE_ACK_WAIT - ms(1)), LeaveStep::Wait(_)), "waits inside the bound");
    assert_eq!(leaving.poll(t0 + lease::CLOSE_ACK_WAIT), LeaveStep::Show, "the bound ends the wait");
    assert_eq!(leaving.line().as_deref(), Some(LEAVE_UNCONFIRMED_CLOSE), "and says the close was not confirmed");
    assert!(!replied.load(Ordering::SeqCst));
    drop((leaving, leases));
    assert_eq!(finish(fake, &log).await.len(), 2);
}

#[tokio::test]
async fn close_after_keep_supersedes() {
    let (listener, path) = bind("supersede");
    // The keep's reply is withheld; the close's comes after 300 ms.
    let (log, _replied, fake) = leave_fake(listener, None, Some("close"), Duration::from_millis(300));
    let host = "local".to_string();
    let leases = Leases::new(false, vec![host.clone()]);
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
    let keep = leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).unwrap();
    assert_eq!(logged(&log, 1).await.len(), 1);
    // The X during the keep's ack wait (`exit_intent`'s Supersede).
    let mut leaving = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
    drop(keep);
    assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "the close waits for its reply");
    let seen = logged(&log, 2).await;
    assert!(seen.len() == 2 && is_leave(&seen[0], "keep") && is_leave(&seen[1], "close"), "keep, then close: {seen:?}");
    assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show);
    assert_eq!(leaving.line(), not_ended_line(7), "the close's own reply");
    drop((leaving, leases));
    assert_eq!(finish(fake, &log).await.len(), 3, "keep, close, then eof");
}

#[tokio::test]
async fn late_grant_gets_the_leave() {
    let (la, pa) = bind("latea");
    let (lb, pb) = bind("lateb");
    let (lc, pc) = bind("latec");
    let (log_a, replied_a, fake_a) = leave_fake(la, None, Some("keep"), Duration::ZERO);
    let (asked_tx, asked) = oneshot::channel();
    let (open, open_rx) = oneshot::channel();
    let (log_b, _, fake_b) = leave_fake(lb, Some((asked_tx, open_rx)), Some("keep"), Duration::ZERO);
    let (a, b, c) = ("a".to_string(), "b".to_string(), "c".to_string());
    let mut leases = Leases::new(false, vec![a.clone(), b.clone(), c.clone()]);
    Arc::get_mut(&mut leases).unwrap().reply_wait = Duration::from_secs(2);
    assert_eq!(leases.before_data_connection(&a, &pa, None).await.unwrap(), 0);
    let in_flight = {
        let leases = leases.clone();
        tokio::spawn(async move { leases.before_data_connection(&b, &pb, None).await })
    };
    asked.await.unwrap();
    let mut leaving = leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).unwrap();
    // Polled before the grant, once a's reply is in.
    answered(&replied_a).await;
    assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "the handshake in flight holds the leave");
    open.send(()).unwrap();
    assert_eq!(in_flight.await.unwrap().unwrap(), 0);
    // A handshake started after the leave is refused, and never connects.
    assert!(leases.before_data_connection(&c, &pc, None).await.is_err());
    assert!(tokio::time::timeout(Duration::from_millis(200), lc.accept()).await.is_err(), "a lease after the leave");
    // Both acks are in the one wait: 7 from each, and their sum shows.
    assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show);
    assert_eq!(leaving.line(), not_ended_line(14));
    drop((leaving, leases));
    let seen_b = finish(fake_b, &log_b).await;
    assert!(seen_b.len() == 2 && is_leave(&seen_b[0], "keep"), "the late lease got the keep before eof: {seen_b:?}");
    assert_eq!(finish(fake_a, &log_a).await.len(), 2);
}

/// Until a fake has written its reply, then a moment for the holder to
/// pass it on.
async fn answered(replied: &std::sync::atomic::AtomicBool) {
    for _ in 0..100 {
        if replied.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
}

/// A daemon that grants, answers the first leave with `payload` under the
/// leave's id plus `shift`, then reads until EOF.
fn odd_reply_fake(
    listener: Bound,
    shift: u64,
    payload: serde_json::Value,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let conn = listener.accept().await.unwrap();
        let (rx, mut tx) = conn.split();
        let mut rx = codec::buffered(rx);
        let (_, req) = read_handoff(&mut rx, &mut tx).await.expect("a handoff");
        let granted = serde_json::json!({"outcome": "granted"});
        codec::write_frame(&mut tx, &Frame::res(req.id, op::FE_LEASE, granted), None).await.unwrap();
        let (leave, _) = codec::read_frame(&mut rx).await.unwrap();
        codec::write_frame(&mut tx, &Frame::res(leave.id + shift, op::FE_LEAVING, payload), None).await.unwrap();
        while codec::read_frame(&mut rx).await.is_ok() {}
    })
}

#[tokio::test]
async fn inflight_handshake_gets_the_leave() {
    // One host, its handshake still in flight when the window leaves.
    let (listener, path) = bind("inflight");
    let (asked_tx, asked) = oneshot::channel();
    let (open, open_rx) = oneshot::channel();
    let (log, _, fake) = leave_fake(listener, Some((asked_tx, open_rx)), Some("keep"), Duration::ZERO);
    let host = "local".to_string();
    let leases = Leases::new(false, vec![host.clone()]);
    let in_flight = {
        let (leases, host) = (leases.clone(), host.clone());
        tokio::spawn(async move { leases.before_data_connection(&host, &path, None).await })
    };
    asked.await.unwrap();
    let mut leaving =
        leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).expect("a handshake in flight is a lease to leave");
    assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "polled before the grant");
    open.send(()).unwrap();
    assert_eq!(in_flight.await.unwrap().unwrap(), 0);
    assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show);
    assert_eq!(leaving.line(), not_ended_line(7));
    drop((leaving, leases));
    let seen = finish(fake, &log).await;
    assert!(seen.len() == 2 && is_leave(&seen[0], "keep"), "the keep reached the lease before eof: {seen:?}");

    // Two hosts: a granted one acks at once, the other is in flight.
    let (la, pa) = bind("inflighta");
    let (lb, pb) = bind("inflightb");
    let (log_a, replied_a, fake_a) = leave_fake(la, None, Some("close"), Duration::ZERO);
    let (asked_tx, asked) = oneshot::channel();
    let (open, open_rx) = oneshot::channel();
    let (log_b, _, fake_b) = leave_fake(lb, Some((asked_tx, open_rx)), Some("close"), Duration::ZERO);
    let (a, b) = ("a".to_string(), "b".to_string());
    let leases = Leases::new(false, vec![a.clone(), b.clone()]);
    assert_eq!(leases.before_data_connection(&a, &pa, None).await.unwrap(), 0);
    let in_flight = {
        let leases = leases.clone();
        tokio::spawn(async move { leases.before_data_connection(&b, &pb, None).await })
    };
    asked.await.unwrap();
    let mut leaving = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
    answered(&replied_a).await;
    assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "a's reply is in; the leave waits for b");
    open.send(()).unwrap();
    assert_eq!(in_flight.await.unwrap().unwrap(), 0);
    assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show);
    drop((leaving, leases));
    let seen_b = finish(fake_b, &log_b).await;
    assert!(seen_b.len() == 2 && is_leave(&seen_b[0], "close"), "b got the close before eof: {seen_b:?}");
    assert_eq!(finish(fake_a, &log_a).await.len(), 2);
}

#[tokio::test]
async fn failed_leave_send_is_not_confirmed() {
    // a and b reply 7 each; c's handshake ends with a holder already gone,
    // so the leave cannot be sent to it; b, in flight, keeps the leave open.
    let (la, pa) = bind("deadsenda");
    let (lb, pb) = bind("deadsendb");
    let (log_a, _, fake_a) = leave_fake(la, None, Some("keep"), Duration::ZERO);
    let (asked_tx, asked) = oneshot::channel();
    let (open, open_rx) = oneshot::channel();
    let (log_b, _, fake_b) = leave_fake(lb, Some((asked_tx, open_rx)), Some("keep"), Duration::ZERO);
    let (a, b, c) = ("a".to_string(), "b".to_string(), "c".to_string());
    let leases = Leases::new(false, vec![a.clone(), b.clone(), c.clone()]);
    assert_eq!(leases.before_data_connection(&a, &pa, None).await.unwrap(), 0);
    let in_flight = {
        let leases = leases.clone();
        tokio::spawn(async move { leases.before_data_connection(&b, &pb, None).await })
    };
    asked.await.unwrap();
    let mut leaving = leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).unwrap();
    let (dead, _) = mpsc::unbounded_channel::<HolderCmd>();
    leases.set(&c, Standing::Granted { state_root: None }, Some(dead));
    open.send(()).unwrap();
    assert_eq!(in_flight.await.unwrap().unwrap(), 0);
    assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show);
    let line = leaving.line().unwrap();
    assert!(line.contains(LEAVE_UNCONFIRMED_KEEP) && line.contains(&not_ended_line(14).unwrap()), "{line}");
    drop((leaving, leases));
    assert_eq!(finish(fake_a, &log_a).await.len(), 2);
    assert_eq!(finish(fake_b, &log_b).await.len(), 2);
}

#[tokio::test]
async fn inflight_handshake_ended_unanswered_is_not_confirmed() {
    // The daemon never answers the lease, and its connection ends after
    // the window began to close.
    let (listener, path) = bind("inflightnogrant");
    let (asked_tx, asked) = oneshot::channel();
    let (open, open_rx) = oneshot::channel();
    let (_, _, fake) = leave_fake(listener, Some((asked_tx, open_rx)), Some("close"), Duration::ZERO);
    let host = "local".to_string();
    let mut leases = Leases::new(false, vec![host.clone()]);
    Arc::get_mut(&mut leases).unwrap().reply_wait = Duration::from_millis(300);
    let in_flight = {
        let (leases, host) = (leases.clone(), host.clone());
        tokio::spawn(async move { leases.before_data_connection(&host, &path, None).await })
    };
    asked.await.unwrap();
    let mut leaving = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
    fake.abort();
    assert!(in_flight.await.unwrap().is_err(), "the connection ended unanswered");
    assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show, "a handshake in flight yields an outcome");
    assert_eq!(leaving.line().as_deref(), Some(LEAVE_UNCONFIRMED_CLOSE));
    drop(open);
}

#[test]
fn forced_exit_delivers_the_queued_close() {
    // The window's runtime runs only inside its `block_on`s, as if the
    // exit came before the holder's next turn; dropping it is the exit.
    let daemon = tokio::runtime::Runtime::new().unwrap();
    let window = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let (log, _, fake, path) = daemon.block_on(async {
        let (listener, path) = bind("forcedexit");
        let (log, replied, fake) = leave_fake(listener, None, Some("keep"), Duration::ZERO);
        (log, replied, fake, path)
    });
    let host = "local".to_string();
    let leases = Leases::new(false, vec![host.clone()]);
    window.block_on(async {
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        let mut keep = leases.leave_all(LeaveIntent::Keep, 0, Instant::now()).unwrap();
        assert_eq!(poll_out(&mut keep).await, LeaveStep::Show, "the daemon accepted the keep");
    });
    // An X queues a Close; a second X exits at once.
    let _close = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
    window.block_on(leases.written(LEAVE_WRITE_WAIT));
    // The exit closes every handle, not just the runtime: on Windows the
    // pipe's halves share one handle, which stays open while any owner lives.
    drop(window);
    drop(_close);
    drop(leases);
    let seen = daemon.block_on(finish(fake, &log));
    assert!(
        seen.len() == 3 && is_leave(&seen[0], "keep") && is_leave(&seen[1], "close"),
        "the queued close reaches the daemon before eof: {seen:?}"
    );
}

#[tokio::test]
async fn invalid_leave_reply_is_not_confirmed() {
    let host = "local".to_string();
    // Unparsable, an error, no count: each is a failure at once, never a 0.
    let bad = [
        serde_json::json!({"not_ended": "invalid"}),
        serde_json::json!({"error": "unknown op: fe.leaving"}),
        serde_json::json!({}),
    ];
    for (i, payload) in bad.into_iter().enumerate() {
        let (listener, path) = bind(&format!("badreply{i}"));
        let fake = odd_reply_fake(listener, 0, payload.clone());
        let leases = Leases::new(false, vec![host.clone()]);
        assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
        let mut leaving = leases.leave_all(LeaveIntent::Close, 0, Instant::now()).unwrap();
        assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show, "{payload}");
        assert_eq!(leaving.line().as_deref(), Some(LEAVE_UNCONFIRMED_CLOSE), "{payload}");
        drop((leaving, leases));
        tokio::time::timeout(Duration::from_secs(5), fake).await.unwrap().unwrap();
    }
    // A reply under another id answers no leave: not confirmed at the bound.
    let (listener, path) = bind("wrongid");
    let fake = odd_reply_fake(listener, 100, serde_json::json!({"not_ended": 7}));
    let leases = Leases::new(false, vec![host.clone()]);
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
    let t0 = Instant::now();
    let mut leaving = leases.leave_all(LeaveIntent::Close, 0, t0).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(matches!(leaving.poll(Instant::now()), LeaveStep::Wait(_)), "a stray reply answers nothing");
    assert_eq!(leaving.poll(t0 + lease::CLOSE_ACK_WAIT), LeaveStep::Show);
    assert_eq!(leaving.line().as_deref(), Some(LEAVE_UNCONFIRMED_CLOSE));
    drop((leaving, leases));
    tokio::time::timeout(Duration::from_secs(5), fake).await.unwrap().unwrap();
}

#[test]
fn mixed_outcomes_show_the_sum() {
    let t0 = Instant::now();
    let (a, b, c) = ("a".to_string(), "b".to_string(), "c".to_string());
    let (ta, ra) = oneshot::channel();
    let (tb, rb) = oneshot::channel::<LeaveOutcome>();
    let (tc, rc) = oneshot::channel();
    let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(a.clone(), ra), (b, rb), (c.clone(), rc)], t0);
    ta.send(LeaveOutcome::Replied(7)).unwrap();
    drop(tb);
    tc.send(LeaveOutcome::Replied(3)).unwrap();
    assert_eq!(l.poll(t0), LeaveStep::Show);
    // Both lines: the sum of the counts, hidden by neither the failure
    // nor the other count, then the failure's own line.
    assert_eq!(l.line(), Some(format!("{}\n{LEAVE_UNCONFIRMED_CLOSE}", not_ended_line(10).unwrap())));
    // Each count is acked once a frame draws the line whole.
    assert_eq!(l.presented(t0), vec![(a.clone(), 7), (c.clone(), 3)]);

    // Two daemons reply 7 and 3: the line shows 10, and both are acked.
    let (ta, ra) = oneshot::channel();
    let (tc, rc) = oneshot::channel();
    let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(a.clone(), ra), (c.clone(), rc)], t0);
    ta.send(LeaveOutcome::Replied(7)).unwrap();
    tc.send(LeaveOutcome::Replied(3)).unwrap();
    assert_eq!(l.poll(t0), LeaveStep::Show);
    assert_eq!(l.line(), not_ended_line(10));
    assert_eq!(l.presented(t0), vec![(a, 7), (c, 3)]);
}

#[tokio::test]
async fn notice_seen_reply_never_swallows_a_handover_reply() {
    let (listener, path) = bind("seenhandover");
    let (log, replied, fake) = leave_fake(listener, None, Some("handover"), Duration::from_millis(200));
    let host = "local".to_string();
    let leases = Leases::new(false, vec![host.clone()]);
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
    // A relaunch; then the owed count's ack goes out while the handover
    // awaits its reply (the fake never answers the ack).
    let mut leaving = leases.leave_all(LeaveIntent::Handover, 75, Instant::now()).unwrap();
    leases.notice_seen(&host, 1);
    assert_eq!(poll_out(&mut leaving).await, LeaveStep::Show, "the handover's own reply");
    assert!(replied.load(Ordering::SeqCst));
    assert_eq!(leaving.line(), not_ended_line(7));
    drop((leaving, leases));
    let seen = finish(fake, &log).await;
    assert!(seen.len() == 3 && is_leave(&seen[0], "handover") && seen[1].starts_with(op::FE_NOTICE_SEEN), "{seen:?}");
}

#[tokio::test]
async fn ended_lease_gives_eof_while_window_lives() {
    // The window drops its lease and runs on: the daemon reads EOF
    // within the bound, on Windows too, where a pipe's halves share one
    // handle.
    let (listener, path) = bind("endeof");
    let (log, _, fake) = leave_fake(listener, None, None, Duration::ZERO);
    let host = "local".to_string();
    let leases = Leases::new(false, vec![host.clone()]);
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
    drop(leases);
    assert_eq!(finish(fake, &log).await, vec!["eof".to_string()]);
}

#[tokio::test]
async fn spawned_child_does_not_hold_the_lease() {
    // After the grant, a child spawned through the window's std `Command`
    // path (`open_url_in_browser`); then every owner of the lease stream
    // goes, as when the window is killed. The daemon reads EOF while the
    // child still runs: no child inherits the lease handle.
    let (listener, path) = bind("child");
    let (log, _, fake) = leave_fake(listener, None, None, Duration::ZERO);
    let host = "local".to_string();
    let leases = Leases::new(false, vec![host.clone()]);
    assert_eq!(leases.before_data_connection(&host, &path, None).await.unwrap(), 0);
    #[cfg(windows)]
    let mut cmd = std::process::Command::new("ping");
    #[cfg(windows)]
    cmd.args(["-n", "60", "127.0.0.1"]);
    #[cfg(not(windows))]
    let mut cmd = std::process::Command::new("sleep");
    #[cfg(not(windows))]
    cmd.arg("60");
    let mut child = cmd.stdout(std::process::Stdio::null()).spawn().unwrap();
    drop(leases);
    let eof = tokio::time::timeout(Duration::from_secs(5), fake).await;
    let alive = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let _ = child.wait();
    assert!(eof.is_ok(), "a child holds the killed window's lease: the fake never saw eof");
    assert!(alive, "the child must still run when the daemon reads eof");
    assert_eq!(log.lock().unwrap().clone(), vec!["eof".to_string()]);
}

#[test]
fn not_ended_status_line() {
    assert_eq!(not_ended_line(0), None);
    assert_eq!(not_ended_line(1).unwrap(), "1 session could not be ended and is still running");
    assert_eq!(not_ended_line(3).unwrap(), "3 sessions could not be ended and are still running");

    let h = "h".to_string();
    let t0 = Instant::now();
    let s = Duration::from_secs;

    let (tx, rx) = oneshot::channel();
    let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(h.clone(), rx)], t0);
    assert_eq!(l.line().as_deref(), Some("closing…"));
    tx.send(LeaveOutcome::Replied(2)).unwrap();
    assert_eq!(l.poll(t0), LeaveStep::Show);
    assert_eq!(l.line().unwrap(), "2 sessions could not be ended and are still running");
    assert_eq!(l.presented(t0), vec![(h.clone(), 2)]);
    assert_eq!(l.poll(t0 + s(1)), LeaveStep::Wait(t0 + s(3)));
    assert_eq!(l.poll(t0 + s(3)), LeaveStep::Exit);

    let (tx, rx) = oneshot::channel();
    let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(h.clone(), rx)], t0);
    tx.send(LeaveOutcome::Replied(0)).unwrap();
    assert_eq!(l.poll(t0), LeaveStep::Exit);

    let (_tx, rx) = oneshot::channel::<LeaveOutcome>();
    let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(h.clone(), rx)], t0);
    assert_eq!(l.poll(t0 + s(124)), LeaveStep::Wait(t0 + s(124) + LEAVE_POLL));
    assert_eq!(l.poll(t0 + s(125)), LeaveStep::Show);

    let (_tx, rx) = oneshot::channel::<LeaveOutcome>();
    let mut l = Leaving::new(LeaveIntent::Keep, 0, vec![(h.clone(), rx)], t0);
    assert_eq!(l.line().as_deref(), Some("closing…"));
    assert_eq!(l.poll(t0 + s(10)), LeaveStep::Show);
    assert_eq!(l.line().as_deref(), Some(LEAVE_UNCONFIRMED_KEEP));

    let l = Leaving::new(LeaveIntent::Handover, 75, vec![], t0);
    assert_eq!(l.line(), None);
}

#[test]
fn not_ended_holds_from_the_presented_frame() {
    let h = "h".to_string();
    let t0 = Instant::now();
    let ms = Duration::from_millis;
    let (tx, rx) = oneshot::channel();
    let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(h.clone(), rx)], t0);
    tx.send(LeaveOutcome::Replied(2)).unwrap();
    // The count arrives at t0; the step names no ack to send.
    assert_eq!(l.poll(t0), LeaveStep::Show);
    assert!(matches!(l.poll(t0 + ms(1000)), LeaveStep::Wait(_)), "no frame yet: still waiting");
    let t1 = t0 + ms(1500);
    let acks = l.presented(t1);
    assert_eq!(l.poll(t1 + NOT_ENDED_EXIT_HOLD - ms(1)), LeaveStep::Wait(t1 + NOT_ENDED_EXIT_HOLD), "held from the presented frame");
    assert_eq!(l.poll(t1 + NOT_ENDED_EXIT_HOLD), LeaveStep::Exit);
    assert_eq!(acks, vec![(h.clone(), 2)], "the ack goes with the presented frame");
    assert_eq!(l.presented(t1 + ms(10)), vec![], "and only once");

    // Nothing presented (a minimized window): exit at the bound, unacked.
    let (tx, rx) = oneshot::channel();
    let mut l = Leaving::new(LeaveIntent::Close, 0, vec![(h.clone(), rx)], t0);
    tx.send(LeaveOutcome::Replied(2)).unwrap();
    assert_eq!(l.poll(t0), LeaveStep::Show);
    assert_eq!(l.poll(t0 + NOT_ENDED_PRESENT_WAIT - ms(1)), LeaveStep::Wait(t0 + NOT_ENDED_PRESENT_WAIT));
    assert_eq!(l.poll(t0 + NOT_ENDED_PRESENT_WAIT), LeaveStep::Exit);
}

#[test]
fn leave_failure_table() {
    use LeaveIntent::*;
    use LeaveOutcome::*;
    let h = "h".to_string();
    let one = |intent, o: LeaveOutcome| leave_report(intent, &[(h.clone(), o)]);
    let failed = |line: Option<&str>, o: &LeaveOutcome| LeaveReport {
        line: line.map(str::to_string),
        acks: vec![],
        warn: vec![(h.clone(), o.clone())],
    };
    // A reply of 0 is the only quiet exit; a reply of n shows (and acks) n.
    assert_eq!(one(Close, Replied(0)), LeaveReport::default());
    assert_eq!(
        one(Close, Replied(3)),
        LeaveReport { line: not_ended_line(3), acks: vec![(h.clone(), 3)], warn: vec![] }
    );
    // A write error, EOF before the reply, and the deadline: a warn and
    // the intent's line; a handover only warns.
    let write = Failed("write: broken pipe".to_string());
    let eof = Failed("the stream ended before the reply".to_string());
    for o in [write, eof, TimedOut] {
        assert_eq!(one(Close, o.clone()), failed(Some(LEAVE_UNCONFIRMED_CLOSE), &o));
        assert_eq!(one(Keep, o.clone()), failed(Some(LEAVE_UNCONFIRMED_KEEP), &o));
        assert_eq!(one(Handover, o.clone()), failed(None, &o));
    }
    // Beside a count both lines show, and the count is acked once drawn.
    let mixed = leave_report(Close, &[("a".to_string(), Replied(2)), ("b".to_string(), TimedOut)]);
    let both = format!("{}\n{LEAVE_UNCONFIRMED_CLOSE}", not_ended_line(2).unwrap());
    assert_eq!((mixed.line, mixed.acks), (Some(both), vec![("a".to_string(), 2)]));
    // Through `poll`: a dropped reply is a failure, never a zero-count ack.
    let t0 = Instant::now();
    let (tx, rx) = oneshot::channel::<LeaveOutcome>();
    let mut l = Leaving::new(Close, 0, vec![(h.clone(), rx)], t0);
    drop(tx);
    assert_eq!(l.poll(t0), LeaveStep::Show);
    assert_eq!(l.line().as_deref(), Some(LEAVE_UNCONFIRMED_CLOSE));
}

#[test]
fn exit_intent_table() {
    use ExitReason::*;
    use ExitStep::*;
    let (keep, close, handover) = (Some(LeaveIntent::Keep), Some(LeaveIntent::Close), Some(LeaveIntent::Handover));
    assert_eq!(exit_intent(WindowClose, None), Leave { intent: LeaveIntent::Close, code: 0 });
    assert_eq!(exit_intent(QuitKey, None), Ask);
    assert_eq!(exit_intent(Relaunch(75), None), Leave { intent: LeaveIntent::Handover, code: 75 });
    assert_eq!(exit_intent(Relaunch(76), None), Leave { intent: LeaveIntent::Handover, code: 76 });
    // The user's latest intent wins: an X or OS close during a Keep closes.
    assert_eq!(exit_intent(WindowClose, keep), Supersede);
    // A second close exits 0: during a Handover it never relaunches.
    assert_eq!(exit_intent(WindowClose, close), Now { code: 0 });
    assert_eq!(exit_intent(WindowClose, handover), Now { code: 0 });
    for leaving in [keep, close, handover] {
        assert_eq!(exit_intent(QuitKey, leaving), Now { code: 0 });
        assert_eq!(exit_intent(Relaunch(75), leaving), Ignore);
    }
}

#[test]
fn second_close_during_handover_exits_zero_on_every_path() {
    use crate::lease::{LeaveOutcome, LeaveStep, Leaving};
    let t0 = std::time::Instant::now();
    let (ack, rx) = tokio::sync::oneshot::channel();
    let mut leaving = Some(Leaving::new(LeaveIntent::Handover, 75, vec![("h".to_string(), rx)], t0));
    // The second close, as `request_quit` applies it.
    let ExitStep::Now { code } = exit_intent(ExitReason::WindowClose, leaving.as_ref().map(|l| l.intent)) else {
        panic!("a second close exits at once");
    };
    assert_eq!(close_now(leaving.as_mut(), code), 0);
    // The handover's ack is then ready, and winit still runs
    // `about_to_wait`: its poll exits with the leave's code.
    ack.send(LeaveOutcome::Replied(0)).unwrap();
    assert_eq!(leaving.as_mut().map(|l| l.poll(t0)), Some(LeaveStep::Exit));
    assert_eq!(leaving.as_ref().map_or(0, |l| l.exit_code), 0, "about_to_wait's exit code after a close");
}
