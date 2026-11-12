//! Tests of `delivery`'s filing half. Pure, as the module is: instants are offsets from one start, and every id, name
//! and timestamp is synthetic. The pass half is unbuilt.

use std::path::Path;
use std::time::{Duration, Instant};

use sot_protocol::chat::{ChatAuthor, ChatRecord, Origin, Ts};

use crate::delivery::{Action, Delivery, Verdict, FILE_GAP, MAX_QUEUED};
use crate::mail::Filed;
use crate::state::{State, MAX_FILED};
use crate::watch::Arrived;

const WS: &str = "T0AAAA";
const CH: &str = "C0AAAA";
const FLOOR: &str = "1700000000.000000";

fn ts(s: &str) -> Ts {
    Ts::parse(s).unwrap()
}

/// The synthetic ts `n` seconds after the floor.
fn at(n: u64) -> String {
    format!("{}.000100", 1_700_000_000 + n)
}

fn rec(channel: &str, origin: Origin, at: &str, thread: &str) -> ChatRecord {
    ChatRecord {
        workspace: WS.into(),
        channel: channel.into(),
        ts: ts(at),
        thread_ts: ts(thread),
        author: ChatAuthor { id: "U0ADA".into(), name: "Ada".into() },
        origin,
        text: "text".into(),
        truncated: false,
        reply_count: 0,
        files: Vec::new(),
        latest_reply: None,
        limited: false,
        omitted: false,
        files_omitted: 0,
    }
}

/// A member's top-level post `n` seconds after the floor.
fn post(n: u64) -> ChatRecord {
    rec(CH, Origin::Member, &at(n), &at(n))
}

fn arrived(records: Vec<ChatRecord>) -> Arrived {
    Arrived { records, gap: 1 }
}

fn filed(n: u64) -> Filed {
    Filed { workspace: WS.into(), channel: CH.into(), ts: ts(&at(n)) }
}

/// A delivery of the one channel `CH` (and `C0BBBB`, for a post of another channel) with empty sets, started.
fn started(t0: Instant) -> Delivery {
    let mut d = fresh(t0);
    d.start(&[]);
    d
}

fn fresh(t0: Instant) -> Delivery {
    // No state file exists in the folder, so each state is its channel's floor.
    let dir = Path::new("/nonexistent-sot-chat-delivery-tests");
    let state = |channel: &str| (State::load(dir, WS, channel, &ts(FLOOR)).unwrap(), ts(FLOOR));
    Delivery::new(t0, [state(CH), state("C0BBBB")])
}

/// Files everything the delivery asks for, each returned `Filed` a `FILE_GAP` after the last; the ts of each, in order.
fn drain(d: &mut Delivery, t0: Instant) -> Vec<String> {
    let (mut now, mut out) = (t0, Vec::new());
    while let Action::File(f) = d.poll(now) {
        out.push(f.record.ts.as_str().to_string());
        now += FILE_GAP;
        d.filed(now, Verdict::Filed);
        now += FILE_GAP;
    }
    out
}

#[test]
fn a_post_seen_live_and_by_a_pass_is_filed_once() {
    // The live halves: the same post taken twice, queued or in flight, and again after it was filed; a pass's result
    // joining the same queue is the pass half's.
    let t0 = Instant::now();
    let mut d = started(t0);
    d.hand_over(arrived(vec![post(3), post(3)]));
    assert!(matches!(d.poll(t0), Action::File(f) if f.record.ts == ts(&at(3))));
    d.hand_over(arrived(vec![post(3)]));
    assert!(matches!(d.poll(t0 + Duration::from_secs(9)), Action::Idle(None)), "in flight, none issued");
    d.filed(t0, Verdict::Filed);
    d.hand_over(arrived(vec![post(3)]));
    assert!(drain(&mut d, t0 + Duration::from_secs(9)).is_empty(), "filed, not queued again");
    // Events out of ts order, each twice, are each filed once, in the order they were taken.
    d.hand_over(arrived(vec![post(30), post(10), post(20), post(10), post(30), post(20)]));
    assert_eq!(drain(&mut d, t0 + Duration::from_secs(20)), [at(30), at(10), at(20)]);
}

#[test]
fn live_mail_is_filed_once_and_a_refusal_owes_a_pass() {
    let t0 = Instant::now();
    let mut d = started(t0);
    let reply_to_old_thread = rec(CH, Origin::Member, &at(5), "1699999990.000000");
    let other_channel = rec("C0ZZZZ", Origin::Member, &at(6), &at(6));
    let below_floor = rec(CH, Origin::Member, "1699999999.000000", "1699999999.000000");
    let people = [rec(CH, Origin::Member, &at(1), &at(1)), rec(CH, Origin::Drawer, &at(2), &at(2))];
    let not_mail = [Origin::Row, Origin::Bot, Origin::Other].map(|o| rec(CH, o, &at(7), &at(7)));
    d.hand_over(arrived([reply_to_old_thread, other_channel, below_floor].into_iter().chain(not_mail).chain(people).collect()));
    let got = drain(&mut d, t0);
    assert_eq!(got.len(), 2, "only the member's and the drawer's posts above the floor: {got:?}");
    assert!(!d.owed(CH), "a filed post owes nothing");

    // A refusal owes the channel's pass, and the post, not in the set, is filed again when it is handed over again.
    d.hand_over(arrived(vec![post(40), post(40)]));
    let now = t0 + Duration::from_secs(5);
    assert!(matches!(d.poll(now), Action::File(_)));
    d.filed(now, Verdict::Refused);
    assert!(d.owed(CH) && !d.owed("C0BBBB"));
    // The post was queued once, so its refusal leaves nothing to try again until a pass finds it.
    assert!(matches!(d.poll(now + Duration::from_secs(1)), Action::Idle(None)));
    d.hand_over(arrived(vec![post(40)]));
    assert_eq!(drain(&mut d, now + Duration::from_secs(5)), [at(40)]);
}

#[test]
fn an_omitted_live_record_owes_a_pass_and_is_not_filed() {
    let t0 = Instant::now();
    let mut d = started(t0);
    let mut cut = post(2);
    cut.omitted = true;
    let mut cut_bot = rec(CH, Origin::Bot, &at(3), &at(3));
    cut_bot.omitted = true;
    d.hand_over(arrived(vec![cut_bot]));
    assert!(!d.owed(CH), "an omitted post that is not mail owes nothing");
    d.hand_over(arrived(vec![cut]));
    assert!(drain(&mut d, t0).is_empty());
    assert!(d.owed(CH));
}

#[test]
fn filings_are_at_least_file_gap_apart_from_a_return() {
    let t0 = Instant::now();
    let mut d = started(t0);
    d.hand_over(arrived(vec![post(1), post(2), post(3)]));
    assert!(matches!(d.poll(t0), Action::File(_)));
    // The first filing took a second; the gap runs from its return, not from its start.
    let back = t0 + Duration::from_secs(1);
    d.filed(back, Verdict::Filed);
    assert!(matches!(d.poll(back), Action::Idle(Some(w)) if w == back + FILE_GAP));
    assert!(matches!(d.poll(back + FILE_GAP - Duration::from_millis(1)), Action::Idle(Some(_))));
    assert!(matches!(d.poll(back + FILE_GAP), Action::File(f) if f.record.ts == ts(&at(2))));
    // A refusal paces the next filing the same way, and nothing is issued while one is in flight.
    assert!(matches!(d.poll(back + FILE_GAP * 9), Action::Idle(None)));
    let back = back + FILE_GAP * 10;
    d.filed(back, Verdict::Refused);
    assert!(matches!(d.poll(back + FILE_GAP - Duration::from_millis(1)), Action::Idle(Some(_))));
    assert!(matches!(d.poll(back + FILE_GAP), Action::File(f) if f.record.ts == ts(&at(3))));
}

#[test]
fn nothing_is_filed_or_passed_before_the_starts_read_is_in() {
    let t0 = Instant::now();
    let mut d = fresh(t0);
    d.hand_over(arrived(vec![post(1), post(2)]));
    assert!(matches!(d.poll(t0 + Duration::from_secs(60)), Action::Idle(None)));
    // The start's read finds one of them filed already (by a run before a restart): it is not filed again, and a filing of
    // another channel or workspace changes nothing.
    let mut other = filed(2);
    other.channel = "C0BBBB".into();
    assert!(!d.start(&[filed(1), other]));
    assert_eq!(drain(&mut d, t0 + Duration::from_secs(60)), [at(2)]);
}

#[test]
fn the_queue_and_a_channels_filed_set_are_bounded_and_a_dropped_post_owes_a_pass() {
    let t0 = Instant::now();
    let mut d = started(t0);
    d.hand_over(arrived((0..MAX_QUEUED as u64 + 1).map(|n| post(1 + n)).collect()));
    assert!(d.owed(CH), "the post past the queue's bound is dropped and owes a pass");
    assert_eq!(drain(&mut d, t0).len(), MAX_QUEUED);

    // A set at its bound: a live post waits in Slack, whatever the queue holds; the start itself reports going past it.
    let mut d = fresh(t0);
    let full: Vec<Filed> = (0..MAX_FILED as u64).map(|n| filed(1 + n)).collect();
    assert!(!d.start(&full));
    d.hand_over(arrived(vec![post(MAX_FILED as u64 + 10)]));
    assert!(d.owed(CH) && drain(&mut d, t0).is_empty());
    assert!(d.start(&[filed(MAX_FILED as u64 + 1)]), "one more takes the set past its bound");
}
