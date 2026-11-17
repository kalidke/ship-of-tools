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
    target_in("T0AAAA", channel)
}

fn target_in(workspace: &str, channel: &str) -> ChatTarget {
    ChatTarget { row: "ws-x-0".into(), workspace: workspace.into(), channel: ChatChannel::try_from(channel.to_string()).unwrap() }
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
    one.release("T0AAAA", "C0001");
    assert!(two.watches("T0AAAA", "C0001") && !one.watches("T0AAAA", "C0001"), "an unwatch ended another connection's watch");
    one.release("T0AAAA", "C0002");
    assert_eq!(one.claim(target("C9999")), Ok(()), "a released slot is free again");
}

#[tokio::test]
async fn ending_a_delivery_whose_owner_is_gone_returns_at_once() {
    let end = |hub: Arc<Hub>| async move { tokio::time::timeout(Duration::from_secs(2), hub.end_mail("T0AAAA")).await.expect("end_mail waited for an owner that is gone") };
    // No owner has run: nothing to wait for.
    end(Hub::new()).await;
    // An owner that dropped its receiver, and one that took the request and died before answering.
    let hub = Hub::new();
    let (ask, gone) = mpsc::unbounded_channel();
    lock(&hub.desks).entry("T0AAAA".into()).or_default().ending = Some(ask);
    drop(gone);
    end(hub).await;
    let hub = Hub::new();
    let (ask, mut owner) = mpsc::unbounded_channel::<oneshot::Sender<()>>();
    lock(&hub.desks).entry("T0AAAA".into()).or_default().ending = Some(ask);
    drop(tokio::spawn(async move { drop(owner.recv().await) }));
    end(hub).await;
}

/// C7: one channel id watched in two workspaces is two slots, each released, held and stated on its own.
#[test]
fn one_channel_id_in_two_workspaces_is_two_slots() {
    let hub = Hub::new();
    let watcher = hub.connect();
    let conn = watcher.conn.clone();
    conn.claim(target_in("T0AAAA", "C0SAME")).unwrap();
    conn.claim(target_in("T0BBBB", "C0SAME")).unwrap();
    assert_eq!(hub.slots().len(), 2, "a channel id in a second workspace replaced the first's slot");
    lock(&hub.table).states.insert(("T0AAAA".into(), "C0SAME".into()), ChatWatchState::Complete);
    assert_eq!((conn.state("T0AAAA", "C0SAME"), conn.state("T0BBBB", "C0SAME")), (ChatWatchState::Complete, ChatWatchState::Pending));
    conn.release("T0AAAA", "C0SAME");
    assert!(!conn.watches("T0AAAA", "C0SAME") && conn.watches("T0BBBB", "C0SAME"), "a release ended the other workspace's watch");
}

/// C7: the daemon's watch bound counts every workspace's channels together, so the watched records' memory is one bound
/// whatever the number of workspaces.
#[test]
fn the_watch_bound_counts_every_workspace() {
    let hub = Hub::new();
    let conn = hub.connect().conn.clone();
    for n in 0..MAX_WATCHED {
        let workspace = if n % 2 == 0 { "T0AAAA" } else { "T0BBBB" };
        assert_eq!(conn.claim(target_in(workspace, &format!("C{n:04}"))), Ok(()), "watch {n}");
    }
    assert_eq!(conn.claim(target_in("T0CCCC", "C9999")), Err(ChatCode::TooManyWatches), "a third workspace's watch past the daemon's bound");
}

/// C7, L6 rule 3: a workspace's end tells and clears only its own slots and states; another workspace's watches stay.
#[test]
fn an_end_tells_and_clears_only_its_workspaces_slots() {
    let hub = Hub::new();
    let (one, two) = (hub.connect(), hub.connect());
    one.conn.claim(target_in("T0AAAA", "C0AAAA01")).unwrap();
    one.conn.claim(target_in("T0BBBB", "C0BBBB01")).unwrap();
    two.conn.claim(target_in("T0BBBB", "C0BBBB01")).unwrap();
    {
        let mut table = lock(&hub.table);
        table.states.insert(("T0AAAA".into(), "C0AAAA01".into()), ChatWatchState::Complete);
        table.states.insert(("T0BBBB".into(), "C0BBBB01".into()), ChatWatchState::Complete);
    }
    let told = lock(&hub.table).end_of("T0AAAA", ChatService::Stopped);
    assert_eq!(told.len(), 1, "a connection with no slot in the ended workspace was told: {}", told.len());
    assert_eq!(told[0].1.resync.iter().map(ChatChannel::as_str).collect::<Vec<_>>(), ["C0AAAA01"]);
    assert!(!one.conn.watches("T0AAAA", "C0AAAA01"));
    assert!(one.conn.watches("T0BBBB", "C0BBBB01") && two.conn.watches("T0BBBB", "C0BBBB01"), "an end cleared another workspace's slots");
    assert_eq!(one.conn.state("T0BBBB", "C0BBBB01"), ChatWatchState::Complete, "an end cleared another workspace's state");
}

/// C7: a workspace's status names only its own connections and its own channels.
#[test]
fn a_workspaces_status_names_only_its_own_connections_and_channels() {
    let hub = Hub::new();
    let (one, two) = (hub.connect(), hub.connect());
    one.conn.claim(target_in("T0AAAA", "C0AAAA01")).unwrap();
    two.conn.claim(target_in("T0BBBB", "C0BBBB01")).unwrap();
    let table = lock(&hub.table);
    let of: BTreeSet<String> = ["C0AAAA01".to_string(), "C0BBBB01".to_string()].into();
    let told = table.status("T0AAAA", ChatService::Running, false, &of);
    assert_eq!(told.len(), 1);
    assert!(one.conn.is(told[0].0), "another workspace's connection was told");
    assert_eq!(told[0].1.resync.iter().map(ChatChannel::as_str).collect::<Vec<_>>(), ["C0AAAA01"]);
}

/// C8, L6 rule 5: the daemon's filings take one slot, `FILE_GAP` apart whatever their workspaces: the second filing of
/// another desk's job begins only `FILE_GAP` after the first returned.
#[tokio::test(start_paused = true)]
async fn the_daemons_filings_take_one_slot_file_gap_apart() {
    let slot = Arc::new(FilingSlot::new());
    let starts = Arc::new(Mutex::new(Vec::new()));
    let job = |slot: Arc<FilingSlot>, starts: Arc<Mutex<Vec<tokio::time::Instant>>>| async move {
        let at = Mutex::new(Slot::Waiting);
        slot.file(&at, async {
            lock(&starts).push(tokio::time::Instant::now());
            tokio::time::sleep(Duration::from_millis(30)).await;
        })
        .await
    };
    let (a, b) = (tokio::spawn(job(slot.clone(), starts.clone())), tokio::spawn(job(slot.clone(), starts.clone())));
    assert_eq!((a.await.unwrap(), b.await.unwrap()), (Some(()), Some(())));
    let at = lock(&starts).clone();
    assert_eq!(at.len(), 2);
    assert!(at[1] - at[0] >= Duration::from_millis(30) + sot_chat::delivery::FILE_GAP, "the second filing began {:?} after the first", at[1] - at[0]);
}

/// The slot's own rule (`FilingSlot::file`): a filing marked dropped while it waits files nothing, and the slot's holder
/// is not held up by it. The owner's use of it, `end_delivery`, is
/// `chat_mail_tests::a_filing_waiting_behind_another_workspaces_ends_at_once_with_its_delivery`.
#[tokio::test(start_paused = true)]
async fn a_dropped_filing_waiting_at_the_slot_files_nothing() {
    let slot = Arc::new(FilingSlot::new());
    let (go, gate) = tokio::sync::oneshot::channel::<()>();
    let holder = {
        let slot = slot.clone();
        tokio::spawn(async move { slot.file(&Mutex::new(Slot::Waiting), async { drop(gate.await) }).await })
    };
    tokio::task::yield_now().await;
    let waiting = Arc::new(Mutex::new(Slot::Waiting));
    let ran = Arc::new(AtomicU32::new(0));
    let second = {
        let (slot, waiting, ran) = (slot.clone(), waiting.clone(), ran.clone());
        tokio::spawn(async move { slot.file(&waiting, async move { ran.fetch_add(1, Ordering::SeqCst) }).await })
    };
    tokio::task::yield_now().await;
    assert!(*lock(&waiting) == Slot::Waiting, "the second filing took the held slot");
    // The delivery's end drops it while it waits.
    *lock(&waiting) = Slot::Dropped;
    go.send(()).unwrap();
    assert_eq!(holder.await.unwrap(), Some(()));
    assert_eq!(second.await.unwrap(), None);
    assert_eq!(ran.load(Ordering::SeqCst), 0, "a dropped filing was filed");
}
