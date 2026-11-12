use super::*;
use sot_protocol::chat::ChatAuthor;
use sot_protocol::ops::MAX_WATCHED;
use std::sync::atomic::AtomicU32;

fn record(ts: &str, text: &str) -> ChatRecord {
    ChatRecord {
        workspace: "T0AAAA".into(),
        channel: "C0AAAA".into(),
        ts: Ts::parse(ts).unwrap(),
        thread_ts: Ts::parse(ts).unwrap(),
        author: ChatAuthor { id: "U0MEMBER".into(), name: String::new() },
        text: text.into(),
        ..ChatRecord::fixture()
    }
}

fn stamp(n: u32) -> String {
    format!("17000{n:05}.000100")
}

#[test]
fn a_diff_names_what_was_put_and_what_is_gone() {
    let (a, b, c) = (record(&stamp(1), "a"), record(&stamp(2), "b"), record(&stamp(3), "c"));
    let edited = record(&stamp(2), "b2");
    let (put, gone) = diff(&[a.clone(), b.clone()], &[b.clone(), c.clone()]);
    assert_eq!((put, gone), (vec![c.clone()], vec![a.ts.clone()]));
    let (put, gone) = diff(&[a.clone(), b], &[a.clone(), edited.clone()]);
    assert_eq!((put, gone), (vec![edited], vec![]));
    assert_eq!(diff(&[a.clone()], &[a]), (vec![], vec![]));
}

#[test]
fn a_full_model_dropping_its_oldest_is_not_a_delete() {
    let all: Vec<ChatRecord> = (1..=MAX_KEPT as u32 + 1).map(|n| record(&stamp(n), "m")).collect();
    // A new message at capacity pushes the oldest out of the model: Slack still holds it.
    let (put, gone) = diff(&all[..MAX_KEPT], &all[1..]);
    assert_eq!((put.len(), gone.len()), (1, 0));
    // A message deleted from the middle of a full model is a delete.
    let mut shorter = all[..MAX_KEPT].to_vec();
    let removed = shorter.remove(50);
    let mut full = shorter.clone();
    full.push(all[MAX_KEPT].clone());
    let (_, gone) = diff(&all[..MAX_KEPT], &full);
    assert_eq!(gone, vec![removed.ts]);
}

#[tokio::test(start_paused = true)]
async fn a_busy_snapshot_is_retried_not_dropped() {
    let tries = AtomicU32::new(0);
    let done = paced(BUSY_PACE, BUSY_ATTEMPTS, || async {
        let n = tries.fetch_add(1, Ordering::SeqCst) + 1;
        if n < 3 { Err(ChatCode::Busy) } else { Ok(n) }
    })
    .await;
    assert_eq!(done, Ok(3), "a busy snapshot was given up on before its retries");
    // Busy for good: the attempts are bounded, and then the watch is told to resync (its slot kept).
    let tries = AtomicU32::new(0);
    let done: Result<(), _> = paced(BUSY_PACE, BUSY_ATTEMPTS, || async {
        tries.fetch_add(1, Ordering::SeqCst);
        Err(ChatCode::Busy)
    })
    .await;
    assert_eq!((done, tries.load(Ordering::SeqCst)), (Err(ChatCode::Busy), BUSY_ATTEMPTS));
    // Any other refusal is final at once.
    let tries = AtomicU32::new(0);
    let done: Result<(), _> = paced(BUSY_PACE, BUSY_ATTEMPTS, || async {
        tries.fetch_add(1, Ordering::SeqCst);
        Err(ChatCode::Failed)
    })
    .await;
    assert_eq!((done, tries.load(Ordering::SeqCst)), (Err(ChatCode::Failed), 1));
}

fn target(channel: &str) -> ChatTarget {
    ChatTarget { row: "ws-x-0".into(), workspace: "T0AAAA".into(), channel: ChatChannel::try_from(channel.to_string()).unwrap() }
}

#[test]
fn a_watch_is_bounded_per_connection_and_in_all() {
    let hub = Hub::new();
    let (first, second) = (hub.connect(), hub.connect());
    let (one, two) = (first.conn.clone(), second.conn.clone());
    for n in 0..MAX_WATCHED {
        assert_eq!(one.claim(target(&format!("C{n:04}"))), Ok(()), "watch {n}");
    }
    assert_eq!(one.claim(target("C9999")), Err(ChatCode::TooManyWatches), "a watch past the connection's bound");
    // A watch it already holds is replaced, not counted again.
    assert_eq!(one.claim(target("C0000")), Ok(()));
    // The daemon's model holds that many channels in all: another connection may watch one of them, not a new one.
    assert_eq!(two.claim(target("C9999")), Err(ChatCode::TooManyWatches), "a channel past the daemon's bound");
    assert_eq!(two.claim(target("C0001")), Ok(()));
    one.release("C0001");
    assert!(two.watches("C0001") && !one.watches("C0001"), "an unwatch ended another connection's watch");
    one.release("C0002");
    assert_eq!(one.claim(target("C9999")), Ok(()), "a released slot is free again");
}
