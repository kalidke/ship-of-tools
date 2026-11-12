//! Tests of `delivery`: the filing queue, the schedule of passes and the commit. Pure, as the module is: instants are
//! offsets from one start, and every id, name and timestamp is synthetic.

use std::path::Path;
use std::time::{Duration, Instant};

use sot_protocol::chat::{ChatAuthor, ChatRecord, Origin, Ts};

use crate::delivery::{Action, Delivery, Timing, Verdict, FILE_GAP, MAX_QUEUED};
use crate::mail::Filed;
use crate::state::{State, MAX_FILED};
use crate::traverse::{Outcome, Passed};
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
    Arrived { records, gap: 0 }
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
    fresh_from(t0, None)
}

/// A delivery of `CH` and `C0BBBB` whose channel `CH` has committed its boundary to `n` seconds after the floor.
fn fresh_from(t0: Instant, boundary: Option<u64>) -> Delivery {
    fresh_of(t0, boundary, &[CH, "C0BBBB"])
}

fn fresh_of(t0: Instant, boundary: Option<u64>, channels: &[&str]) -> Delivery {
    // No state file exists in the folder, so each state is its channel's floor.
    let dir = Path::new("/nonexistent-sot-chat-delivery-tests");
    let state = |channel: &str| {
        let mut state = State::load(dir, WS, channel, &ts(FLOOR)).unwrap();
        if let (CH, Some(n)) = (channel, boundary) {
            state.commit(&ts(&at(n)));
        }
        (state, ts(FLOOR))
    };
    Delivery::new(Timing::PRODUCT, t0, channels.iter().map(|c| state(c)))
}

/// A pass's result: `mail` found, `horizon` seconds after the floor.
fn passed(outcome: Outcome, horizon: u64, mail: &[u64]) -> Passed {
    Passed { horizon: ts(&at(horizon)), mail: mail.iter().map(|n| post(*n)).collect(), outcome, reads: 1, limited: false }
}

/// The first socket hello, which owes every channel its start's pass.
fn hello() -> Arrived {
    Arrived { records: Vec::new(), gap: 1 }
}

/// Files what the delivery asks for until it asks for anything else, which is returned with the time it was asked; a
/// filing's `ts` in `refuse` returns `Refused`, any other `Filed`, each a `FILE_GAP` after the last.
fn run(d: &mut Delivery, mut now: Instant, refuse: &[u64]) -> (Vec<String>, Action, Instant) {
    let mut files = Vec::new();
    loop {
        match d.poll(now) {
            Action::File(f) => {
                files.push(f.record.ts.as_str().to_string());
                now += FILE_GAP;
                let refused = refuse.iter().any(|n| at(*n) == f.record.ts.as_str());
                d.filed(now, if refused { Verdict::Refused } else { Verdict::Filed });
                now += FILE_GAP;
            }
            other => return (files, other, now),
        }
    }
}

/// A started delivery handed the hello, and the pass it asks for (of `CH`): the time it was asked.
fn in_pass(t0: Instant) -> (Delivery, Instant) {
    in_pass_of(started(t0), t0)
}

/// The same for a delivery of `CH` alone, whose other channel's passes cannot come between a test and its own.
fn in_solo(t0: Instant) -> (Delivery, Instant) {
    let mut d = fresh_of(t0, None, &[CH]);
    d.start(&[]);
    in_pass_of(d, t0)
}

fn in_pass_of(mut d: Delivery, t0: Instant) -> (Delivery, Instant) {
    d.hand_over(hello());
    let (files, action, now) = run(&mut d, t0, &[]);
    assert!(files.is_empty() && matches!(action, Action::Pass { .. }), "{action:?}");
    (d, now)
}

fn is_pass(a: Action) -> bool {
    matches!(a, Action::Pass { .. })
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
    // The live halves: the same post taken twice, queued or in flight, and again after it was filed.
    let t0 = Instant::now();
    let mut d = started(t0);
    d.hand_over(arrived(vec![post(3), post(3)]));
    assert!(matches!(d.poll(t0), Action::File(f) if f.record.ts == ts(&at(3))));
    d.hand_over(arrived(vec![post(3)]));
    assert!(!matches!(d.poll(t0 + Duration::from_secs(9)), Action::File(_)), "in flight, none issued");
    d.filed(t0, Verdict::Filed);
    d.hand_over(arrived(vec![post(3)]));
    assert!(drain(&mut d, t0 + Duration::from_secs(9)).is_empty(), "filed, not queued again");
    // Events out of ts order, each twice, are each filed once, in the order they were taken.
    d.hand_over(arrived(vec![post(30), post(10), post(20), post(10), post(30), post(20)]));
    assert_eq!(drain(&mut d, t0 + Duration::from_secs(20)), [at(30), at(10), at(20)]);
    // The same ts in two channels is two posts: a ts is unique within a channel only.
    d.hand_over(arrived(vec![post(50), rec("C0BBBB", Origin::Member, &at(50), &at(50))]));
    assert_eq!(drain(&mut d, t0 + Duration::from_secs(40)), [at(50), at(50)]);

    // A live event, then the pass's result holding it and one more: each is filed once, the live one as it was; a refusal
    // is not retried by a second entry (the pass is owed instead).
    let (mut d, now) = in_pass(t0);
    d.hand_over(arrived(vec![post(3)]));
    d.passed(now, passed(Outcome::Complete, 10, &[3, 4]));
    assert_eq!(run(&mut d, now, &[3]).0, [at(3), at(4)]);
    // The pass's filing in flight, then its live event, which is not queued behind it.
    let (mut d, now) = in_pass(t0);
    d.passed(now, passed(Outcome::Complete, 10, &[3]));
    assert!(matches!(d.poll(now), Action::File(f) if !f.live));
    d.hand_over(arrived(vec![post(3)]));
    d.filed(now, Verdict::Refused);
    assert!(run(&mut d, now + Duration::from_secs(1), &[]).0.is_empty());
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
    assert!(!matches!(d.poll(now + Duration::from_secs(1)), Action::File(_)));
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
    assert!(d.owed(CH));
    assert!(drain(&mut d, t0).is_empty());
    // One already settled (at or below the boundary) owes nothing: a pass would find it filed or passed.
    let mut d = fresh_from(t0, Some(10));
    d.start(&[]);
    let mut settled = post(8);
    settled.omitted = true;
    d.hand_over(arrived(vec![settled]));
    assert!(!d.owed(CH));
}

#[test]
fn a_post_at_or_below_the_boundary_is_never_queued_and_owes_nothing() {
    let t0 = Instant::now();
    let mut d = fresh_from(t0, Some(10));
    d.start(&[]);
    d.hand_over(arrived(vec![post(5), post(10), post(11)]));
    assert!(!d.owed(CH));
    assert_eq!(drain(&mut d, t0), [at(11)]);
    // Settled, it is no loss to a queue that is full: nothing is dropped, so no pass is owed.
    d.hand_over(arrived((0..MAX_QUEUED as u64).map(|n| post(20 + n)).collect()));
    d.hand_over(arrived(vec![post(5), post(10), post(11)]));
    assert!(!d.owed(CH));
}

#[test]
fn a_returns_report_is_true_once_when_it_takes_the_set_past_its_bound() {
    let t0 = Instant::now();
    let mut d = fresh(t0);
    let almost: Vec<Filed> = (0..MAX_FILED as u64 - 1).map(|n| filed(1 + n)).collect();
    d.start(&almost);
    // Both enter below the bound; the second's return is the one that takes the set past it.
    d.hand_over(arrived(vec![post(MAX_FILED as u64 + 10), post(MAX_FILED as u64 + 11)]));
    let mut now = t0;
    let mut reports = Vec::new();
    while let Action::File(_) = d.poll(now) {
        reports.push(d.filed(now, Verdict::Filed));
        now += FILE_GAP;
    }
    assert_eq!(reports, [false, true]);
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
    assert!(!matches!(d.poll(back + FILE_GAP * 9), Action::File(_)));
    let back = back + FILE_GAP * 10;
    d.filed(back, Verdict::Refused);
    assert!(!matches!(d.poll(back + FILE_GAP - Duration::from_millis(1)), Action::File(_)));
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

#[test]
fn a_pass_is_owed_after_every_newer_gap_a_held_or_dropped_filing_and_by_the_sweep() {
    let t0 = Instant::now();
    // The sweep, with nothing filed and no gap: a pass 6 h after the start, not before.
    let mut d = started(t0);
    let sweep = Timing::PRODUCT.sweep;
    assert!(matches!(d.poll(t0), Action::Idle(Some(at)) if at == t0 + sweep));
    assert!(!is_pass(d.poll(t0 + sweep - Duration::from_millis(1))));
    assert!(is_pass(d.poll(t0 + sweep)));

    // The first hello owes each channel its start's pass, one at a time, `read_pace` apart; once a pass has completed it is
    // not owed again for the same gap, but is for the next.
    let pace = Timing::PRODUCT.read_pace;
    let (mut d, mut now) = in_pass(t0);
    for n in 0..2 {
        d.passed(now, passed(Outcome::Complete, 10, &[]));
        assert!(matches!(d.poll(now), Action::Commit { .. }));
        if n == 0 {
            assert!(!is_pass(d.poll(now + pace - Duration::from_millis(1))));
            now += pace;
            assert!(is_pass(d.poll(now)), "the other channel's");
        }
    }
    d.hand_over(hello());
    assert!(!is_pass(d.poll(now + Duration::from_secs(60))), "the same gap again");
    d.hand_over(Arrived { records: Vec::new(), gap: 2 });
    assert!(!is_pass(d.poll(now + pace - Duration::from_millis(1))));
    assert!(is_pass(d.poll(now + pace)));

    // The pass is handed the greatest `ts` taken for its channel, of any origin and in any order.
    let mut d = started(t0);
    d.hand_over(Arrived { records: vec![post(7), rec(CH, Origin::Bot, &at(9), &at(9)), post(5)], gap: 1 });
    let (_, action, _) = run(&mut d, t0, &[]);
    assert!(matches!(action, Action::Pass { order, .. } if order.seen == Some(ts(&at(9)))));

    // A refused filing, a dropped post and an omitted one each owe a pass.
    let mut d = started(t0);
    d.hand_over(arrived(vec![post(1)]));
    assert!(is_pass(run(&mut d, t0, &[1]).1));
    let mut d = started(t0);
    d.hand_over(arrived((0..MAX_QUEUED as u64 + 1).map(|n| post(1 + n)).collect()));
    let (files, action, _) = run(&mut d, t0, &[]);
    assert!(files.len() == MAX_QUEUED && is_pass(action));
    let mut d = started(t0);
    let mut cut = post(1);
    cut.omitted = true;
    d.hand_over(arrived(vec![cut]));
    assert!(is_pass(d.poll(t0)));
}

/// What the commit does after a pass that found `mail`, with `refuse` refused, at `horizon`: the boundary it commits
/// (None: none) and the delivery, which asks for its next pass at the time it returns.
fn committed(outcome: Outcome, horizon: u64, mail: &[u64], refuse: &[u64]) -> (Option<Ts>, Delivery, Instant) {
    let t0 = Instant::now();
    let (mut d, now) = in_solo(t0);
    d.passed(now, passed(outcome, horizon, mail));
    let (_, action, now) = run(&mut d, now, refuse);
    let boundary = if let Action::Commit { boundary, .. } = action { Some(boundary) } else { None };
    (boundary, d, now)
}

#[test]
fn the_commit_moves_the_boundary_to_h_or_to_the_last_filing_before_the_first_that_was_not() {
    let all = Outcome::Complete;
    // Every candidate filed: to the horizon.
    assert_eq!(committed(all.clone(), 10, &[3, 4, 5], &[]).0, Some(ts(&at(10))));
    // One refused in the middle: to the last filed before it, not to the horizon.
    assert_eq!(committed(all.clone(), 10, &[3, 4, 5], &[4]).0, Some(ts(&at(3))));
    // The oldest refused: the boundary stays, though everything after it was filed.
    assert_eq!(committed(all.clone(), 10, &[3, 4, 5], &[3]).0, None);
    // A candidate above the horizon is not judged by this commit.
    assert_eq!(committed(all, 10, &[3, 12], &[12]).0, Some(ts(&at(10))));
    // A truncated pass handed over only its oldest: no further than its last candidate, when that lies below the horizon.
    let (boundary, mut d, now) = committed(Outcome::Truncated, 10, &[3, 4, 5], &[]);
    assert_eq!(boundary, Some(ts(&at(5))));
    // And another pass of the channel is owed at once, no sooner than `read_pace` after this one.
    assert!(matches!(d.poll(now + Timing::PRODUCT.read_pace), Action::Pass { order, .. } if order.state.channel() == CH));
    // Above the horizon, the horizon is the limit.
    assert_eq!(committed(Outcome::Truncated, 10, &[3, 12], &[]).0, Some(ts(&at(10))));
}

#[test]
fn a_commit_never_reaches_a_post_still_pending() {
    // A live post of the channel is in flight and another queued, neither among the pass's candidates; the commit waits
    // until both have returned, or the recheck at issue would drop what the boundary passed.
    let t0 = Instant::now();
    let mut d = started(t0);
    d.hand_over(arrived(vec![post(5), post(6)]));
    d.hand_over(hello());
    assert!(matches!(d.poll(t0), Action::File(_)));
    assert!(is_pass(d.poll(t0)));
    d.passed(t0, passed(Outcome::Complete, 10, &[]));
    assert!(!matches!(d.poll(t0), Action::Commit { .. }), "one in flight");
    d.filed(t0, Verdict::Filed);
    assert!(!matches!(d.poll(t0), Action::Commit { .. }), "one queued");
    assert!(matches!(d.poll(t0 + FILE_GAP), Action::File(_)));
    d.filed(t0 + FILE_GAP, Verdict::Filed);
    assert!(matches!(d.poll(t0 + FILE_GAP), Action::Commit { boundary, .. } if boundary == ts(&at(10))));
}

#[test]
fn a_full_filed_set_is_drained_by_its_owed_pass() {
    let t0 = Instant::now();
    let mut d = fresh(t0);
    d.start(&(0..MAX_FILED as u64).map(|n| filed(1 + n)).collect::<Vec<_>>());
    // A live post waits in Slack, and owes a pass; the pass's candidate is queued whatever the set holds.
    let late = MAX_FILED as u64 + 10;
    d.hand_over(arrived(vec![post(late)]));
    let (files, action, now) = run(&mut d, t0, &[]);
    assert!(files.is_empty() && is_pass(action));
    d.passed(now, passed(Outcome::Complete, late + 5, &[late]));
    let (files, action, now) = run(&mut d, now, &[]);
    assert_eq!(files, [at(late)]);
    assert!(matches!(action, Action::Commit { boundary, .. } if boundary == ts(&at(late + 5))));
    // The commit pruned the set: a live post is queued again.
    d.hand_over(arrived(vec![post(late + 10)]));
    assert_eq!(run(&mut d, now, &[]).0, [at(late + 10)]);
}

#[test]
fn backoff_doubles_to_its_cap_and_a_complete_pass_resets_it() {
    let t0 = Instant::now();
    let (mut d, mut now) = in_solo(t0);
    // Failed passes commit nothing, their candidates are filed all the same, and each waits twice the last, to the cap.
    for (n, wait) in [30, 60, 120, 240, 480, 600, 600].into_iter().enumerate() {
        let wait = Duration::from_secs(wait);
        d.passed(now, Passed { outcome: Outcome::ReadFailed, ..passed(Outcome::Complete, 10, &[3 + n as u64]) });
        let (files, action, later) = run(&mut d, now, &[]);
        assert_eq!(files.len(), 1);
        assert!(!matches!(action, Action::Commit { .. }));
        assert!(!is_pass(d.poll(now + wait - Duration::from_millis(1))), "{wait:?}");
        // The filing's time passed; the wait runs from the result.
        assert!(later < now + wait);
        now += wait;
        assert!(is_pass(d.poll(now)), "{wait:?}");
    }
    // A complete pass resets it: the next failure waits the first backoff again.
    d.passed(now, passed(Outcome::Complete, 10, &[]));
    let _ = d.poll(now);
    d.hand_over(Arrived { records: Vec::new(), gap: 2 });
    now += Timing::PRODUCT.read_pace;
    assert!(is_pass(d.poll(now)));
    d.passed(now, passed(Outcome::TooLong, 10, &[]));
    let wait = Timing::PRODUCT.backoff_first;
    assert!(!is_pass(d.poll(now + wait - Duration::from_millis(1))));
    assert!(is_pass(d.poll(now + wait)));

    // A rate limit waits at least its `Retry-After`, a skipped pass (the recipient absent) the backoff, and neither reads.
    let (mut d, now) = in_solo(t0);
    d.passed(now, passed(Outcome::RateLimited { retry_after: Duration::from_secs(100) }, 10, &[]));
    assert!(!is_pass(d.poll(now + Duration::from_secs(99))));
    assert!(is_pass(d.poll(now + Duration::from_secs(100))));
    let (mut d, now) = in_solo(t0);
    d.pass_skipped(now);
    assert!(!is_pass(d.poll(now + Duration::from_secs(29))));
    assert!(is_pass(d.poll(now + Duration::from_secs(30))));
}

/// Asks for what the delivery wants until `end`, filing each filing as `Filed` and jumping to each wake: the channel of
/// every pass asked for, in order, each answered by `answer` (`passed` or `pass_skipped`).
fn pass_channels(d: &mut Delivery, mut now: Instant, end: Instant, answer: impl Fn(&str, &mut Delivery, Instant)) -> Vec<String> {
    let mut chs = Vec::new();
    for _ in 0..10_000 {
        match d.poll(now) {
            Action::Pass { order, .. } => {
                chs.push(order.state.channel().to_string());
                answer(order.state.channel(), d, now);
            }
            Action::File(_) => {
                d.filed(now, Verdict::Filed);
                now += FILE_GAP;
            }
            Action::Commit { .. } => {}
            Action::Idle(Some(at)) if at <= end => now = at,
            Action::Idle(_) => break,
        }
    }
    chs
}

fn completes(_: &str, d: &mut Delivery, now: Instant) {
    d.passed(now, passed(Outcome::Complete, 10, &[]));
}

#[test]
fn a_channels_failing_passes_never_starve_another() {
    // `CH` is first in line and its pass fails or is skipped every time; `C0BBBB`, owed its start's pass at the same
    // moment, is passed once, and `CH` keeps its own backoff meanwhile.
    for skip in [true, false] {
        let t0 = Instant::now();
        let mut d = started(t0);
        d.hand_over(hello());
        let fail = |ch: &str, d: &mut Delivery, now: Instant| match (ch, skip) {
            (CH, true) => d.pass_skipped(now),
            (CH, false) => d.passed(now, passed(Outcome::ReadFailed, 10, &[])),
            _ => completes(ch, d, now),
        };
        let chs = pass_channels(&mut d, t0, t0 + Duration::from_secs(3600), fail);
        assert_eq!(chs.iter().filter(|c| *c == "C0BBBB").count(), 1, "{chs:?}");
        // At 0, 30, 90, 210, 450, 930, 1530, 2130, 2730 and 3330 s: each after its own, doubling and capped, backoff.
        assert_eq!(chs.iter().filter(|c| *c == CH).count(), 10, "{chs:?}");
    }
}

#[test]
fn a_gap_during_a_pass_owes_another() {
    let t0 = Instant::now();
    let (mut d, now) = in_pass(t0);
    d.hand_over(Arrived { records: Vec::new(), gap: 2 });
    d.passed(now, passed(Outcome::Complete, 10, &[]));
    let chs = pass_channels(&mut d, now, now + Duration::from_secs(600), completes);
    assert_eq!(chs.iter().filter(|c| *c == CH).count(), 1, "the gap that came during the first pass owes its channel a second: {chs:?}");
}

#[test]
fn the_sweep_runs_from_the_last_completed_pass() {
    let t0 = Instant::now();
    let (mut d, now) = in_pass(t0);
    let later = now + Duration::from_secs(3600);
    d.passed(later, passed(Outcome::Complete, 10, &[]));
    assert_eq!(pass_channels(&mut d, later, later + Duration::from_secs(60), completes), ["C0BBBB"]);
    let sweep = Timing::PRODUCT.sweep;
    assert!(!is_pass(d.poll(t0 + sweep + Duration::from_secs(1))));
    assert!(is_pass(d.poll(later + sweep)));
}

#[test]
fn a_pass_owed_by_a_refusal_is_not_owed_again_once_it_has_completed() {
    let t0 = Instant::now();
    let mut d = started(t0);
    d.hand_over(arrived(vec![post(1)]));
    let (_, action, now) = run(&mut d, t0, &[1]);
    assert!(is_pass(action));
    d.passed(now, passed(Outcome::Complete, 10, &[1]));
    let (files, _, now) = run(&mut d, now, &[]);
    assert_eq!(files.len(), 1);
    assert!(!is_pass(d.poll(now + Duration::from_secs(60))));
}

#[test]
fn pass_candidates_never_take_the_queue_past_its_bound() {
    let t0 = Instant::now();
    let (mut d, now) = in_pass(t0);
    d.hand_over(arrived((0..MAX_QUEUED as u64 - 10).map(|n| rec("C0BBBB", Origin::Member, &at(1000 + n), &at(1000 + n))).collect()));
    d.passed(now, passed(Outcome::Truncated, 500, &(1..=200).collect::<Vec<u64>>()));
    let (files, action, _) = run(&mut d, now, &[]);
    assert_eq!(files.len(), MAX_QUEUED);
    // Only the first ten candidates fitted: the commit reaches the tenth.
    assert!(matches!(action, Action::Commit { boundary, .. } if boundary == ts(&at(10))));
}

#[test]
fn a_live_filing_says_it_is_live() {
    let t0 = Instant::now();
    let mut d = started(t0);
    d.hand_over(arrived(vec![post(3)]));
    assert!(matches!(d.poll(t0), Action::File(f) if f.live));
}

#[test]
fn a_commit_that_leaves_a_candidate_unfiled_backs_off() {
    let (boundary, mut d, now) = committed(Outcome::Complete, 10, &[3, 4], &[4]);
    assert_eq!(boundary, Some(ts(&at(3))));
    assert!(!is_pass(d.poll(now + Duration::from_secs(20))));
    assert!(is_pass(d.poll(now + Duration::from_secs(40))));
}

#[test]
fn a_failed_pass_keeps_read_pace_before_the_next_channels_first_read() {
    // `CH`'s pass fails; `C0BBBB`, owed its start's pass, is not asked for before `read_pace` after that pass's result.
    let t0 = Instant::now();
    let (mut d, now) = in_pass(t0);
    d.passed(now, passed(Outcome::ReadFailed, 10, &[]));
    let pace = Timing::PRODUCT.read_pace;
    assert!(!is_pass(d.poll(now)));
    assert!(!is_pass(d.poll(now + pace - Duration::from_millis(1))));
    assert!(matches!(d.poll(now + pace), Action::Pass { order, .. } if order.state.channel() == "C0BBBB"));
}

#[test]
fn a_channel_truncated_at_every_pass_does_not_hold_anothers_owed_pass() {
    // `CH` completes a truncated pass and is owed another at once, as is `C0BBBB` for its start's pass: ready together,
    // the one passed less recently goes first.
    let t0 = Instant::now();
    let (mut d, now) = in_pass(t0);
    let now = now + Duration::from_secs(10);
    d.passed(now, passed(Outcome::Truncated, 10, &[3]));
    let (files, _, now) = run(&mut d, now, &[]);
    assert_eq!(files.len(), 1);
    assert!(matches!(d.poll(now + Timing::PRODUCT.read_pace), Action::Pass { order, .. } if order.state.channel() == "C0BBBB"));
}
