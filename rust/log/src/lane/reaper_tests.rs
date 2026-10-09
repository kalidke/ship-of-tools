//! `PendingJoins` against real threads: completion, panic, once-only expiry and ownership after expiry; and the
//! shutdown deadline's shared state, which a wake that cannot be queued does not lose.

use super::pending::{signal_shutdown, PendingJoins, ReaperMsg, Worker};
use super::test_progress::Progress;
use std::sync::{mpsc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// A thread that ends only when told to, and one that has already ended.
fn held() -> (mpsc::Sender<()>, JoinHandle<()>) {
    let (release, wait) = mpsc::channel::<()>();
    (
        release,
        thread::spawn(move || {
            let _ = wait.recv();
        }),
    )
}

fn ended_when_polled(handle: &JoinHandle<()>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "the thread never ended");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_finished_worker_is_joined_while_its_peer_is_still_running() {
    let (release, reader) = held();
    let writer = thread::spawn(|| {});
    ended_when_polled(&writer);
    let mut joins = PendingJoins::new(reader, writer, Instant::now() + Duration::from_secs(60));
    let first = joins.poll(Instant::now());
    assert_eq!(first.joined, vec![(Worker::Writer, true)]);
    assert!(!first.done && first.expired.is_none());
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let poll = joins.poll(Instant::now());
        if poll.done {
            assert_eq!(poll.joined, vec![(Worker::Reader, true)]);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the released reader never joined"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_completed_panic_is_reported_apart_from_the_unfinished_peer_expiry() {
    let (release, reader) = held();
    let writer = thread::spawn(|| std::panic::resume_unwind(Box::new("injected")));
    ended_when_polled(&writer);
    let mut joins = PendingJoins::new(reader, writer, Instant::now() - Duration::from_millis(1));
    let poll = joins.poll(Instant::now());
    assert_eq!(poll.joined, vec![(Worker::Writer, false)]);
    assert_eq!(
        poll.expired,
        Some(vec![Worker::Reader]),
        "only the unfinished peer is named"
    );
    assert!(poll.panicked() && !poll.done);
    release.send(()).unwrap();
}

#[test]
fn expiry_is_reported_once_and_the_unfinished_workers_stay_owned() {
    let (release_reader, reader) = held();
    let (release_writer, writer) = held();
    let mut joins = PendingJoins::new(reader, writer, Instant::now() - Duration::from_millis(1));
    assert_eq!(
        joins.poll(Instant::now()).expired,
        Some(vec![Worker::Reader, Worker::Writer])
    );
    let again = joins.poll(Instant::now());
    assert!(again.expired.is_none() && !again.done && again.joined.is_empty());
    release_reader.send(()).unwrap();
    release_writer.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !joins.poll(Instant::now()).done {
        assert!(
            Instant::now() < deadline,
            "the released workers never joined"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_worker_that_finished_by_the_deadline_is_not_an_expiry() {
    let reader = thread::spawn(|| {});
    let writer = thread::spawn(|| {});
    ended_when_polled(&reader);
    ended_when_polled(&writer);
    let mut joins = PendingJoins::new(reader, writer, Instant::now() - Duration::from_secs(1));
    let poll = joins.poll(Instant::now());
    assert!(poll.done && poll.expired.is_none() && !poll.panicked());
}

#[test]
fn tightening_only_moves_the_deadline_forward() {
    let (release_reader, reader) = held();
    let (release_writer, writer) = held();
    let mut joins = PendingJoins::new(reader, writer, Instant::now() + Duration::from_secs(60));
    assert!(joins.poll(Instant::now()).expired.is_none());
    joins.tighten(Instant::now() + Duration::from_secs(120));
    assert!(
        joins.poll(Instant::now()).expired.is_none(),
        "a later deadline must not apply"
    );
    joins.tighten(Instant::now() - Duration::from_millis(1));
    assert_eq!(joins.poll(Instant::now()).expired.map(|w| w.len()), Some(2));
    release_reader.send(()).unwrap();
    release_writer.send(()).unwrap();
}

#[test]
fn the_shutdown_deadline_survives_a_wake_that_cannot_be_queued() {
    let (wake, inbox) = mpsc::sync_channel(1);
    wake.try_send(ReaperMsg::Wake).unwrap();
    let shutdown = OnceLock::new();
    let progress = Progress::default();
    let first = Instant::now() + Duration::from_secs(20);
    signal_shutdown(&shutdown, &wake, &progress, first);
    assert_eq!(
        shutdown.get(),
        Some(&first),
        "the full inbox lost the deadline"
    );
    signal_shutdown(&shutdown, &wake, &progress, first + Duration::from_secs(5));
    assert_eq!(shutdown.get(), Some(&first), "a repeat moved the deadline");
    assert!(
        matches!(inbox.try_recv(), Ok(ReaperMsg::Wake)),
        "the earlier message is still queued"
    );
}
