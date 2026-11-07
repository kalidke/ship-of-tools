//! Tests of `watch`: the model alone, driven the way its callers will drive it. No clock, no network.

use std::cmp::Ordering;

use sot_protocol::chat::{ChatAuthor, ChatRecord, Origin, Ts};

use crate::normalize::{ts_cmp, Normalized};
use crate::socket::{Gap, GapCause};
use crate::watch::{
    Loaded, State, WatchError, Watches, MAX_CHANNELS, MAX_KEPT, MAX_OVERLAY,
};

const C: &str = "C0AAAA";

fn ts(s: &str) -> Ts {
    Ts::parse(s).unwrap()
}

fn rec_in(channel: &str, at: &str, thread: &str, text: &str) -> ChatRecord {
    ChatRecord {
        workspace: "T0TEAM".into(),
        channel: channel.into(),
        ts: ts(at),
        thread_ts: ts(thread),
        author: ChatAuthor { id: "U0AAAA".into(), name: String::new() },
        origin: Origin::Member,
        text: text.into(),
        truncated: false,
        reply_count: 0,
        files: Vec::new(),
        latest_reply: None,
        limited: false,
        omitted: false,
        files_omitted: 0,
    }
}

fn rec(at: &str, text: &str) -> ChatRecord {
    rec_in(C, at, at, text)
}

fn new(at: &str, text: &str) -> Normalized {
    Normalized::New(rec(at, text))
}

fn edited(at: &str, text: &str) -> Normalized {
    Normalized::Edited(rec(at, text))
}

fn deleted(at: &str) -> Normalized {
    Normalized::Deleted { workspace: "T0TEAM".into(), channel: C.into(), ts: ts(at) }
}

fn texts(w: &Watches, channel: &str) -> Vec<String> {
    w.messages(channel).iter().map(|r| r.text.clone()).collect()
}

fn gap(seq: u64) -> Gap {
    Gap::for_test(seq, GapCause::Reconnect)
}

/// A watched channel with its snapshot begun.
fn pending(w: &mut Watches) -> u64 {
    w.watch(C).unwrap();
    w.begin(C).unwrap()
}

#[test]
fn timestamps_order_numerically_on_validated_values() {
    let cmp = |a: &str, b: &str| ts_cmp(&ts(a), &ts(b));
    // A string order puts "10" before "9".
    assert_eq!(cmp("9.000000", "10.000000"), Ordering::Less);
    // Leading zeros are one value, and so one message.
    assert_eq!(cmp("007.000000", "7.000000"), Ordering::Equal);
    assert_eq!(cmp("0.000000", "000.000000"), Ordering::Equal);
    // Equal seconds: the six digits decide, and they are digits, not a count.
    assert_eq!(cmp("5.000009", "5.000010"), Ordering::Less);
    assert_eq!(cmp("5.100000", "5.099999"), Ordering::Greater);
    // The largest values: twenty digits of seconds fit and order.
    let max = format!("{}.999999", "9".repeat(20));
    let below = format!("{}.999999", "9".repeat(19) + "8");
    assert_eq!(cmp(&max, &below), Ordering::Greater);
    assert_eq!(cmp(&max, &max), Ordering::Equal);
    assert_eq!(cmp(&max, "0.000000"), Ordering::Greater);
    // The model keeps one message for two spellings of one timestamp.
    let mut w = Watches::new();
    let t = pending(&mut w);
    assert_eq!(w.loaded(C, t, vec![rec("007.000000", "old")]), Loaded::Complete);
    assert!(w.apply(&edited("7.000000", "new")));
    assert_eq!(texts(&w, C), ["new"]);
}

#[test]
fn watch_snapshot_overlap_and_lag_resynchronize() {
    let mut w = Watches::new();
    let t = pending(&mut w);
    assert_eq!(w.state(C), Some(State::Pending));
    // Live changes during the snapshot: an edit and a delete of messages the snapshot holds, and a message it lacks.
    assert!(w.apply(&edited("1.000000", "edited")));
    assert!(w.apply(&deleted("2.000000")));
    assert!(w.apply(&new("4.000000", "arrived")));
    // The snapshot is older than all three.
    let snapshot = vec![rec("1.000000", "stale"), rec("2.000000", "deleted"), rec("3.000000", "kept")];
    assert_eq!(w.loaded(C, t, snapshot), Loaded::Complete);
    assert_eq!(texts(&w, C), ["edited", "kept", "arrived"]);
    assert_eq!(w.state(C), Some(State::Complete));

    // A new message never replaces one held, whatever order it comes in.
    assert!(w.apply(&new("3.000000", "again")));
    assert_eq!(texts(&w, C), ["edited", "kept", "arrived"]);

    // An overlay one change past its bound is lost, so the snapshot is refused and a new one is owed.
    let mut w = Watches::new();
    let t = pending(&mut w);
    for n in 0..=MAX_OVERLAY {
        assert!(w.apply(&edited(&format!("{}.000000", n + 1), "x")));
    }
    assert_eq!(w.loaded(C, t, vec![rec("1.000000", "snap")]), Loaded::Resync);
    assert_eq!(w.state(C), Some(State::Needed));
    assert!(w.messages(C).is_empty(), "a refused snapshot kept content");
    // The next snapshot, with room in its overlay, completes.
    let t2 = w.begin(C).unwrap();
    assert!(w.apply(&edited("1.000000", "live")));
    assert_eq!(w.loaded(C, t2, vec![rec("1.000000", "snap")]), Loaded::Complete);
    assert_eq!(texts(&w, C), ["live"]);
    // Exactly the bound fits.
    let mut w = Watches::new();
    let t = pending(&mut w);
    for n in 0..MAX_OVERLAY {
        w.apply(&new(&format!("{}.000000", n + 1), "x"));
    }
    assert_eq!(w.loaded(C, t, vec![]), Loaded::Complete);
}

#[test]
fn a_gap_makes_every_watch_incomplete_until_a_later_snapshot_and_clears_only_then() {
    let mut w = Watches::new();
    w.watch(C).unwrap();
    w.watch("C0BBBB").unwrap();
    w.sync(Some(gap(1)));
    let (a, b) = (w.begin(C).unwrap(), w.begin("C0BBBB").unwrap());
    assert_eq!(w.loaded(C, a, vec![rec("1.000000", "a")]), Loaded::Complete);
    // One watch is still pending: the gap is not cleared, and `clear` is not asked.
    assert!(!w.reconcile(|_| panic!("asked to clear with a watch pending")));
    // A second gap during the resync: the pending snapshot began before it, and so does the finished one.
    w.sync(Some(gap(2)));
    assert_eq!(w.state(C), Some(State::Needed));
    assert_eq!(w.loaded("C0BBBB", b, vec![]), Loaded::Resync);
    assert_eq!(w.state("C0BBBB"), Some(State::Needed));
    // Resynced from snapshots begun after the second gap: the gap clears, once, with the value read.
    let (a, b) = (w.begin(C).unwrap(), w.begin("C0BBBB").unwrap());
    assert_eq!(w.loaded(C, a, vec![]), Loaded::Complete);
    assert_eq!(w.loaded("C0BBBB", b, vec![]), Loaded::Complete);
    let mut given = None;
    assert!(w.reconcile(|g| {
        given = Some(g);
        true
    }));
    assert_eq!(given, Some(gap(2)));
    assert!(!w.reconcile(|_| panic!("a cleared gap is cleared again")));
    // A loss during the resync, seen by the socket but not yet by the model: `clear` refuses and the gap stays.
    w.sync(Some(gap(3)));
    let a = w.begin(C).unwrap();
    w.loaded(C, a, vec![]);
    let b = w.begin("C0BBBB").unwrap();
    w.loaded("C0BBBB", b, vec![]);
    assert!(!w.reconcile(|_| false));
    // The loss is seen: every watch owes a snapshot again.
    w.sync(Some(gap(4)));
    assert_eq!((w.state(C), w.state("C0BBBB")), (Some(State::Needed), Some(State::Needed)));
    // A gap latched while a snapshot is pending: the answer is refused.
    let a = w.begin(C).unwrap();
    w.sync(Some(gap(5)));
    assert_eq!(w.loaded(C, a, vec![rec("1.000000", "x")]), Loaded::Resync);
}

#[test]
fn a_gap_with_nothing_watched_clears_and_a_watch_begun_after_it_needs_no_resync() {
    let mut w = Watches::new();
    w.sync(Some(gap(1)));
    assert!(w.reconcile(|g| g == gap(1)));
    let t = pending(&mut w);
    assert_eq!(w.loaded(C, t, vec![]), Loaded::Complete);
}

#[test]
fn events_nobody_keeps_and_answers_nobody_wants() {
    let mut w = Watches::new();
    // A channel nobody watches: not kept, and no state appears for it.
    assert!(!w.apply(&new("1.000000", "x")));
    assert_eq!(w.state(C), None);
    let t = pending(&mut w);
    // A thread reply and a non-message are not the channel's.
    assert!(!w.apply(&Normalized::New(rec_in(C, "3.000000", "1.000000", "reply"))));
    assert!(!w.apply(&Normalized::Unsupported));
    // An unwatch during the snapshot: its answer is stale, even when the channel is watched again.
    w.unwatch(C);
    assert_eq!(w.loaded(C, t, vec![rec("1.000000", "x")]), Loaded::Stale);
    w.watch(C).unwrap();
    assert!(w.messages(C).is_empty());
    let t2 = w.begin(C).unwrap();
    assert_eq!(w.loaded(C, t, vec![rec("1.000000", "x")]), Loaded::Stale);
    assert_eq!(w.state(C), Some(State::Pending));
    // A snapshot that fails, or whose service stopped, leaves a new one owed; a late failure of an old one does not.
    w.failed(C, t);
    assert_eq!(w.state(C), Some(State::Pending));
    w.failed(C, t2);
    assert_eq!(w.state(C), Some(State::Needed));
    assert_eq!(w.loaded(C, t2, vec![]), Loaded::Stale);
    let t3 = w.begin(C).unwrap();
    assert_eq!(w.begin(C), None, "a second snapshot begun over a pending one");
    // A delete of a message never held, and an edit of one older than the page, change nothing.
    let page: Vec<_> = (10..13).map(|n| rec(&format!("{n}.000000"), "p")).collect();
    assert_eq!(w.loaded(C, t3, page), Loaded::Complete);
    assert!(w.apply(&deleted("11.500000")));
    assert!(w.apply(&edited("5.000000", "older")));
    assert_eq!(texts(&w, C), ["p", "p", "p"]);
    // An edit of a message not held is never added, newer or older than the page.
    assert!(w.apply(&edited("13.000000", "late")));
    assert_eq!(texts(&w, C), ["p", "p", "p"]);
}

#[test]
fn the_watch_is_bounded_in_channels_ids_and_messages() {
    let mut w = Watches::new();
    for n in 0..MAX_CHANNELS {
        w.watch(&format!("C{n:05}")).unwrap();
    }
    assert_eq!(w.watch("C99999"), Err(WatchError::Full));
    assert_eq!(w.watch("C00001"), Ok(()), "a watched channel is not a new one");
    assert_eq!(w.watch(&"C".repeat(65)), Err(WatchError::Invalid));
    assert_eq!(w.watch(""), Err(WatchError::Invalid));
    w.unwatch("C00001");
    assert_eq!(w.watch("C99999"), Ok(()));
    // Kept messages: the oldest leave, and an edit of one that left is not brought back.
    let mut w = Watches::new();
    let t = pending(&mut w);
    let page: Vec<_> = (1..=MAX_KEPT + 5).map(|n| rec(&format!("{n}.000000"), &n.to_string())).collect();
    assert_eq!(w.loaded(C, t, page), Loaded::Complete);
    assert_eq!(w.messages(C).len(), MAX_KEPT);
    assert_eq!(w.messages(C)[0].text, "6");
    assert!(w.apply(&edited("2.000000", "back")));
    assert_eq!(w.messages(C)[0].text, "6");
    assert!(w.apply(&new("1000.000000", "live")));
    assert_eq!(w.messages(C).len(), MAX_KEPT);
    assert_eq!(w.messages(C)[0].text, "7");
    // With room made by a delete, the edit of an evicted message still stays out.
    assert!(w.apply(&deleted("1000.000000")));
    assert!(w.apply(&edited("3.000000", "back")));
    assert_eq!(w.messages(C).len(), MAX_KEPT - 1);
    assert_eq!(w.messages(C)[0].text, "7");
}

#[test]
fn a_snapshot_keeps_only_this_channels_top_level_messages() {
    let mut w = Watches::new();
    let t = pending(&mut w);
    let page = vec![rec("1.000000", "top"), rec_in(C, "3.000000", "1.000000", "broadcast"), rec_in("C0BBBB", "2.000000", "2.000000", "other")];
    assert_eq!(w.loaded(C, t, page), Loaded::Complete);
    assert_eq!(texts(&w, C), ["top"]);
    // A reply is as absent live as it is in the snapshot.
    assert!(!w.apply(&Normalized::Edited(rec_in(C, "3.000000", "1.000000", "edited"))));
}

#[test]
fn syncing_a_gap_already_known_keeps_a_complete_watch() {
    let mut w = Watches::new();
    w.sync(Some(gap(1)));
    let t = pending(&mut w);
    assert_eq!(w.loaded(C, t, vec![]), Loaded::Complete);
    w.sync(Some(gap(1)));
    assert_eq!(w.state(C), Some(State::Complete));
}

#[test]
fn events_applied_before_begin_are_replaced_by_the_snapshot() {
    // The caller's order: apply what the socket holds, sync, then begin. Older queued events then lose to the snapshot.
    let mut w = Watches::new();
    w.watch(C).unwrap();
    w.sync(Some(gap(1)));
    assert!(w.apply(&edited("1.000000", "queued, older")));
    let t = w.begin(C).unwrap();
    assert_eq!(w.loaded(C, t, vec![rec("1.000000", "newer")]), Loaded::Complete);
    assert_eq!(texts(&w, C), ["newer"]);
    assert!(w.reconcile(|g| g == gap(1)));
}
