//! Tests of `watch`: the model alone, driven the way its callers will drive it. No clock, no network.

use std::cmp::Ordering;
use std::sync::Arc;
use std::time::Duration;

use sot_protocol::chat::{ChatAuthor, ChatRecord, Origin, Ts};

use crate::api::Api;
use crate::http::{Http, Secret};
use crate::normalize::{ts_key, Normalized};
use crate::service::Service;
use crate::socket::{Gap, GapCause, Taken};
use crate::watch::{
    Loaded, State, Wake, WatchError, Watches, MAX_CHANNELS, MAX_KEPT, MAX_OVERLAY,
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
    w.begin_taken(C, Taken::default()).unwrap()
}

#[test]
fn timestamps_order_numerically_on_validated_values() {
    let cmp = |a: &str, b: &str| ts_key(&ts(a)).cmp(&ts_key(&ts(b)));
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
    assert_eq!(w.loaded_at(C, t, vec![rec("007.000000", "old")], None), Loaded::Complete);
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
    assert_eq!(w.loaded_at(C, t, snapshot, None), Loaded::Complete);
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
    assert_eq!(w.loaded_at(C, t, vec![rec("1.000000", "snap")], None), Loaded::Resync);
    assert_eq!(w.state(C), Some(State::Needed));
    assert!(w.messages(C).is_empty(), "a refused snapshot kept content");
    // The next snapshot, with room in its overlay, completes.
    let t2 = w.begin_taken(C, Taken::default()).unwrap();
    assert!(w.apply(&edited("1.000000", "live")));
    assert_eq!(w.loaded_at(C, t2, vec![rec("1.000000", "snap")], None), Loaded::Complete);
    assert_eq!(texts(&w, C), ["live"]);
    // Exactly the bound fits.
    let mut w = Watches::new();
    let t = pending(&mut w);
    for n in 0..MAX_OVERLAY {
        w.apply(&new(&format!("{}.000000", n + 1), "x"));
    }
    assert_eq!(w.loaded_at(C, t, vec![], None), Loaded::Complete);
}

#[test]
fn a_gap_makes_every_watch_incomplete_until_a_later_snapshot_and_clears_only_then() {
    let mut w = Watches::new();
    w.watch(C).unwrap();
    w.watch("C0BBBB").unwrap();
    w.sync(Some(gap(1)));
    let (a, b) = (w.begin_taken(C, Taken::default()).unwrap(), w.begin_taken("C0BBBB", Taken::default()).unwrap());
    assert_eq!(w.loaded_at(C, a, vec![rec("1.000000", "a")], None), Loaded::Complete);
    // One watch is still pending: the gap is not cleared, and `clear` is not asked.
    assert!(!w.reconcile_with(|_| panic!("asked to clear with a watch pending")));
    // A second gap during the resync: the pending snapshot began before it, and so does the finished one.
    w.sync(Some(gap(2)));
    assert_eq!(w.state(C), Some(State::Needed));
    assert_eq!(w.loaded_at("C0BBBB", b, vec![], None), Loaded::Resync);
    assert_eq!(w.state("C0BBBB"), Some(State::Needed));
    // Resynced from snapshots begun after the second gap: the gap clears, once, with the value read.
    let (a, b) = (w.begin_taken(C, Taken::default()).unwrap(), w.begin_taken("C0BBBB", Taken::default()).unwrap());
    assert_eq!(w.loaded_at(C, a, vec![], None), Loaded::Complete);
    assert_eq!(w.loaded_at("C0BBBB", b, vec![], None), Loaded::Complete);
    let mut given = None;
    assert!(w.reconcile_with(|g| {
        given = Some(g);
        true
    }));
    assert_eq!(given, Some(gap(2)));
    assert!(!w.reconcile_with(|_| panic!("a cleared gap is cleared again")));
    // A loss during the resync, seen by the socket but not yet by the model: `clear` refuses and the gap stays.
    w.sync(Some(gap(3)));
    let a = w.begin_taken(C, Taken::default()).unwrap();
    w.loaded_at(C, a, vec![], None);
    let b = w.begin_taken("C0BBBB", Taken::default()).unwrap();
    w.loaded_at("C0BBBB", b, vec![], None);
    assert!(!w.reconcile_with(|_| false));
    // The loss is seen: every watch owes a snapshot again.
    w.sync(Some(gap(4)));
    assert_eq!((w.state(C), w.state("C0BBBB")), (Some(State::Needed), Some(State::Needed)));
    // A gap latched while a snapshot is pending: the answer is refused.
    let a = w.begin_taken(C, Taken::default()).unwrap();
    w.sync(Some(gap(5)));
    assert_eq!(w.loaded_at(C, a, vec![rec("1.000000", "x")], None), Loaded::Resync);
}

#[test]
fn a_gap_with_nothing_watched_clears_and_a_watch_begun_after_it_needs_no_resync() {
    let mut w = Watches::new();
    w.sync(Some(gap(1)));
    assert!(w.reconcile_with(|g| g == gap(1)));
    let t = pending(&mut w);
    assert_eq!(w.loaded_at(C, t, vec![], None), Loaded::Complete);
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
    assert_eq!(w.loaded_at(C, t, vec![rec("1.000000", "x")], None), Loaded::Stale);
    w.watch(C).unwrap();
    assert!(w.messages(C).is_empty());
    let t2 = w.begin_taken(C, Taken::default()).unwrap();
    assert_eq!(w.loaded_at(C, t, vec![rec("1.000000", "x")], None), Loaded::Stale);
    assert_eq!(w.state(C), Some(State::Pending));
    // A snapshot that fails, or whose service stopped, leaves a new one owed; a late failure of an old one does not.
    w.failed(C, t);
    assert_eq!(w.state(C), Some(State::Pending));
    w.failed(C, t2);
    assert_eq!(w.state(C), Some(State::Needed));
    assert_eq!(w.loaded_at(C, t2, vec![], None), Loaded::Stale);
    let t3 = w.begin_taken(C, Taken::default()).unwrap();
    assert_eq!(w.begin_taken(C, Taken::default()), None, "a second snapshot begun over a pending one");
    // A delete of a message never held, and an edit of one older than the page, change nothing.
    let page: Vec<_> = (10..13).map(|n| rec(&format!("{n}.000000"), "p")).collect();
    assert_eq!(w.loaded_at(C, t3, page, None), Loaded::Complete);
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
    assert_eq!(w.loaded_at(C, t, page, None), Loaded::Complete);
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
    assert_eq!(w.loaded_at(C, t, page, None), Loaded::Complete);
    assert_eq!(texts(&w, C), ["top"]);
    // A reply is as absent live as it is in the snapshot.
    assert!(!w.apply(&Normalized::Edited(rec_in(C, "3.000000", "1.000000", "edited"))));
}

#[test]
fn syncing_a_gap_already_known_keeps_a_complete_watch() {
    let mut w = Watches::new();
    w.sync(Some(gap(1)));
    let t = pending(&mut w);
    assert_eq!(w.loaded_at(C, t, vec![], None), Loaded::Complete);
    w.sync(Some(gap(1)));
    assert_eq!(w.state(C), Some(State::Complete));
}

#[test]
fn a_snapshot_replaces_what_is_held_and_a_queued_event_never_wins_over_it() {
    // A message deleted during a gap leaves the model: the snapshot replaces, it does not merge.
    let mut w = Watches::new();
    let t = pending(&mut w);
    assert_eq!(w.loaded_at(C, t, vec![rec("1.000000", "a"), rec("2.000000", "b")], None), Loaded::Complete);
    w.sync(Some(gap(1)));
    let t = w.begin_taken(C, Taken::default()).unwrap();
    assert_eq!(w.loaded_at(C, t, vec![rec("1.000000", "a2")], None), Loaded::Complete);
    assert_eq!(texts(&w, C), ["a2"]);
}

#[test]
fn a_burst_after_the_events_were_taken_refuses_the_snapshot() {
    // The events were queued before the gap's loss; a newer edit was dropped after the take. The snapshot holds it.
    let mut w = Watches::new();
    let t = pending(&mut w);
    w.loaded_at(C, t, vec![rec("1.000000", "v0")], None);
    let taken = Taken { gap: Some(gap(1)), events: vec![edited("1.000000", "stale")] };
    let t = w.begin_taken(C, taken).unwrap();
    assert_eq!(texts(&w, C), ["stale"], "a taken event was not applied before the snapshot began");
    // A loss after the take is a gap the model has not seen: the fresh read at `loaded` finds it.
    assert_eq!(w.loaded_at(C, t, vec![rec("1.000000", "fresh")], Some(gap(2))), Loaded::Resync);
    assert_eq!(w.state(C), Some(State::Needed));
    assert!(!w.reconcile_with(|_| panic!("a gap was cleared over a refused snapshot")));
    // Without a loss the same snapshot completes, and the taken event is older than it.
    let taken = Taken { gap: Some(gap(2)), events: vec![edited("1.000000", "stale")] };
    let t = w.begin_taken(C, taken).unwrap();
    assert_eq!(w.loaded_at(C, t, vec![rec("1.000000", "fresh")], Some(gap(2))), Loaded::Complete);
    assert_eq!(texts(&w, C), ["fresh"]);
}

#[test]
fn begin_applies_the_events_it_is_given_to_every_channel_whether_or_not_it_begins() {
    let other = "C0BBBB";
    let mut w = Watches::new();
    let t = pending(&mut w);
    w.loaded_at(C, t, vec![rec("1.000000", "a")], None);
    w.watch(other).unwrap();
    // The begun channel is another one: the edit of the first is applied, and the begun one goes pending.
    let taken = Taken { gap: None, events: vec![edited("1.000000", "a2")] };
    assert!(w.begin_taken(other, taken).is_some());
    assert_eq!(texts(&w, C), ["a2"], "an event for another channel was not applied");
    // No snapshot begins (already pending), and the events are applied all the same.
    let taken = Taken { gap: None, events: vec![edited("1.000000", "a3")] };
    assert_eq!(w.begin_taken(other, taken), None);
    assert_eq!(texts(&w, C), ["a3"], "events given to a refused begin were thrown away");
    // Nor for an unwatched channel.
    let taken = Taken { gap: None, events: vec![edited("1.000000", "a4")] };
    assert_eq!(w.begin_taken("C0CCCC", taken), None);
    assert_eq!(texts(&w, C), ["a4"]);
}

#[test]
fn a_gap_read_before_a_newer_one_was_seen_cannot_roll_the_model_back() {
    let mut w = Watches::new();
    w.watch(C).unwrap();
    w.sync(Some(gap(1)));
    let t = w.begin_taken(C, Taken::default()).unwrap();
    // Another task's pump saw a newer gap; the snapshot's answer carries the value read before it.
    w.sync(Some(gap(2)));
    assert_eq!(w.loaded_at(C, t, vec![rec("1.000000", "old")], Some(gap(1))), Loaded::Resync);
    assert_eq!(w.state(C), Some(State::Needed));
    // A cleared gap is not seen again from an old read.
    let t = w.begin_taken(C, Taken::default()).unwrap();
    assert_eq!(w.loaded_at(C, t, vec![], Some(gap(2))), Loaded::Complete);
    assert!(w.reconcile_with(|g| g == gap(2)));
    w.sync(Some(gap(1)));
    w.sync(Some(gap(2)));
    assert_eq!(w.state(C), Some(State::Complete), "a gap already seen and cleared came back");
}

/// One consumer, one producer, every order, through the real socket state: a `Service` over a socket with no task,
/// where the source's edits are queued and its loss is latched, and the product's own take and wake run (`pump`,
/// `begin`, `gap`, `reconciled`, `wait`). The orders are every interleaving of a snapshot task (begin, fetch, read the
/// gap, answer), a pump task (two pumps, a reconcile; one more pump when `wait` says look at the end) and the source
/// (two edits, then a third that is either delivered or lost into a gap). Each step is atomic. A complete channel
/// never holds a text the world no longer has, once the last events are pumped, and a gap is never cleared over a
/// stale model.
#[derive(Clone, Copy, Debug)]
enum Op {
    Begin,
    Fetch,
    ReadNow,
    Loaded,
    Pump,
    Reconcile,
    Edit1,
    Edit2,
    Edit3,
}

struct World {
    service: Service,
    w: Watches,
    text: String,
    ticket: Option<u64>,
    page: Option<String>,
    now: Option<Gap>,
    lost: bool,
}

/// A wait that leaves nothing owed: the permit a push or latch left, if any, is consumed. After a take every one is
/// spurious (the take saw all of it), so the next `wait` is true only for a change after the take.
async fn settle(w: &mut Watches, service: &Service) -> bool {
    w.wait(service, Duration::ZERO).await == Wake::Look
}

impl World {
    async fn step(&mut self, op: Op) {
        match op {
            Op::Begin => {
                self.ticket = self.w.begin(C, &self.service);
                settle(&mut self.w, &self.service).await;
            }
            Op::Fetch => self.page = Some(self.text.clone()),
            Op::ReadNow => self.now = self.service.gap(),
            Op::Loaded => {
                if let (Some(t), Some(page)) = (self.ticket, self.page.clone()) {
                    self.w.loaded_at(C, t, vec![rec("1.000000", &page)], self.now);
                }
            }
            Op::Pump => {
                self.w.pump(&self.service);
                settle(&mut self.w, &self.service).await;
            }
            Op::Reconcile => {
                let service = &self.service;
                self.w.reconcile(service);
            }
            Op::Edit1 | Op::Edit2 => {
                let v = if matches!(op, Op::Edit1) { "v1" } else { "v2" };
                self.text = v.into();
                self.service.feed(edited("1.000000", v));
            }
            Op::Edit3 => {
                self.text = "v3".into();
                if self.lost {
                    self.service.lose(GapCause::Failed);
                } else {
                    self.service.feed(edited("1.000000", "v3"));
                }
            }
        }
    }

    /// The model's text of M, when it holds one.
    fn held(&self) -> Option<String> {
        self.w.messages(C).first().map(|r| r.text.clone())
    }
}

fn interleavings(seqs: &[&[Op]], at: &mut Vec<usize>, run: &mut Vec<Op>, out: &mut Vec<Vec<Op>>) {
    if at.iter().zip(seqs).all(|(i, s)| *i == s.len()) {
        out.push(run.clone());
        return;
    }
    for k in 0..seqs.len() {
        if at[k] < seqs[k].len() {
            run.push(seqs[k][at[k]]);
            at[k] += 1;
            interleavings(seqs, at, run, out);
            at[k] -= 1;
            run.pop();
        }
    }
}

fn test_api() -> Arc<Api> {
    Arc::new(Api::new(Http::new().unwrap(), Secret::new("xoxb-test"), "T0TEAM").unwrap())
}

#[tokio::test]
async fn no_interleaving_of_a_snapshot_a_pump_and_the_source_ends_complete_over_a_lost_event() {
    let api = test_api();
    let snapshot = [Op::Begin, Op::Fetch, Op::ReadNow, Op::Loaded];
    let pump = [Op::Pump, Op::Pump, Op::Reconcile];
    let source = [Op::Edit1, Op::Edit2, Op::Edit3];
    let mut runs = Vec::new();
    interleavings(&[&snapshot, &pump, &source], &mut vec![0; 3], &mut Vec::new(), &mut runs);
    assert_eq!(runs.len(), 4200);
    for held_at_start in [false, true] {
        for lost in [false, true] {
            for run in &runs {
                // The channel starts needing a snapshot (a hello's gap not yet cleared) or complete and clean.
                let service = Service::inert(api.clone());
                service.lose(GapCause::Reconnect);
                let mut w = service.watches().unwrap();
                w.watch(C).unwrap();
                w.pump(&service);
                if held_at_start {
                    let t = w.begin(C, &service).unwrap();
                    w.loaded(C, t, vec![rec("1.000000", "v0")], &service);
                    assert!(w.reconcile(&service));
                }
                settle(&mut w, &service).await;
                let mut m = World {
                    service,
                    w,
                    text: "v0".into(),
                    ticket: None,
                    page: None,
                    now: None,
                    lost,
                };
                for op in run {
                    m.step(*op).await;
                }
                let what = format!("{run:?} held_at_start={held_at_start} lost={lost}");
                // A gap cleared over a model that is not the world's is a silent loss: the clear happened with
                // the loss latched, so the service still holds a gap unless the model had seen it.
                if lost && run.iter().any(|o| matches!(o, Op::Edit3)) && m.service.gap().is_none() {
                    assert_eq!(m.held().as_deref(), Some("v3"), "gap cleared over a stale model: {what}");
                }
                // The pump runs once more only when the wake says look: a quiet channel is never healed by luck.
                if settle(&mut m.w, &m.service).await {
                    m.w.pump(&m.service);
                }
                if m.w.state(C) == Some(State::Complete) {
                    assert_eq!(m.held(), Some(m.text.clone()), "complete and stale: {what}");
                }
            }
        }
    }
}

#[tokio::test]
async fn pump_and_begin_apply_the_services_events_in_the_call_that_takes_them() {
    let other = "C0BBBB";
    let service = Service::inert(test_api());
    let mut w = service.watches().unwrap();
    w.watch(C).unwrap();
    let t = w.begin(C, &service).unwrap();
    assert_eq!(w.loaded(C, t, vec![rec("1.000000", "a")], &service), Loaded::Complete);
    // A queued edit reaches the model through `pump`, and leaves the socket.
    service.feed(edited("1.000000", "pumped"));
    w.pump(&service);
    assert_eq!(texts(&w, C), ["pumped"], "pump dropped the service's events");
    w.pump(&service);
    assert_eq!(texts(&w, C), ["pumped"]);
    // And through `begin` of another channel.
    w.watch(other).unwrap();
    service.feed(edited("1.000000", "begun"));
    assert!(w.begin(other, &service).is_some());
    assert_eq!(texts(&w, C), ["begun"], "begin dropped the service's events");
}

#[tokio::test]
async fn a_service_has_one_watches_and_no_other_reads_its_events() {
    let api = test_api();
    let service = Service::inert(api.clone());
    let mut first = service.watches().expect("the first");
    assert!(service.watches().is_none(), "a second Watches for one service");
    first.watch(C).unwrap();
    let t = first.begin(C, &service).unwrap();
    first.loaded(C, t, vec![rec("1.000000", "a")], &service);
    // A Watches of another service takes nothing from this one.
    let elsewhere = Service::inert(api);
    let mut stranger = elsewhere.watches().unwrap();
    service.feed(edited("1.000000", "edit"));
    stranger.pump(&service);
    first.pump(&service);
    assert_eq!(texts(&first, C), ["edit"], "a Watches of another service took the events");
    // Nor is another service's gap its to read or clear.
    service.lose(GapCause::Reconnect);
    stranger.watch(C).unwrap();
    let ticket = stranger.begin(C, &elsewhere).unwrap();
    assert_eq!(stranger.loaded(C, ticket, Vec::new(), &service), Loaded::Stale, "a stranger answered with another service's gap");
    assert_eq!(stranger.state(C), Some(State::Pending));
    // Gap numbers are process-wide, so a number of its own service's is not this service's.
    let other = Service::inert(test_api());
    other.lose(GapCause::Reconnect);
    let mut empty = other.watches().unwrap();
    empty.pump(&other);
    assert!(!empty.reconcile(&service), "a stranger cleared another service's gap");
    assert!(service.gap().is_some());
    // The claim goes with the Watches.
    drop(first);
    assert!(service.watches().is_some(), "the claim outlived its Watches");
}

#[test]
fn many_threads_asking_at_once_get_one_watches() {
    let service = Service::inert(test_api());
    let barrier = std::sync::Barrier::new(8);
    for _ in 0..3000 {
        let got = std::sync::atomic::AtomicUsize::new(0);
        let held = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    barrier.wait();
                    if let Some(w) = service.watches() {
                        got.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        held.lock().unwrap().push(w);
                    }
                    barrier.wait();
                });
            }
        });
        assert_eq!(got.load(std::sync::atomic::Ordering::SeqCst), 1, "not exactly one Watches");
        held.lock().unwrap().clear();
    }
}
