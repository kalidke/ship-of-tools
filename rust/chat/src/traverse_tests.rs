//! Tests of `traverse`'s channel and thread walks against the fake Slack's `History`, which plays the measured forms
//! (its own header names the sections of SlackClaw's facts note and says what it infers). Every id, name and timestamp
//! is synthetic.

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sot_protocol::chat::Ts;

use crate::api::{Api, ApiError, Identity, MAX_PAGE};
use crate::fake_slack::{FakeSlack, History, Identity as Trust, Reply};
use crate::http::Secret;
use crate::normalize::ts_key;
use crate::state::State;
use crate::traverse::{pass, walk, Order, Outcome, Passed, MARGIN, MAX_PASS_MAIL};

const WS: &str = "T0TEAM";
const CHANNEL: &str = "C0CHAN";
const FLOOR: &str = "1700000000.000000";
/// The pass's upper bound in most tests; a post made after it is the next pass's.
const U: &str = "1700001000.000000";
const NO_PACE: Duration = Duration::ZERO;

fn ts(s: &str) -> Ts {
    Ts::parse(s).unwrap()
}

/// The synthetic ts `n` seconds after the floor, with a fraction no second-aligned bound shares.
fn at(n: u64) -> String {
    format!("{}.000001", 1_700_000_000 + n)
}

async fn bounded<T>(body: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(90), body)
        .await
        .expect("the test exceeded its bound")
}

/// The fake serving `script` for anything but `auth.test`, and an `Api` that reads from it.
async fn start(script: impl Fn(&crate::fake_slack::Seen) -> Reply + Send + Sync + 'static) -> (FakeSlack, Api) {
    let fake = FakeSlack::start(Trust::Trusted, move |seen, _| script(seen)).await;
    let api = Api::new(fake.http(), Secret::new("sentinel-bot-token-not-a-credential"), WS)
        .unwrap()
        .with_base(fake.url("slack.com", "/api/"))
        .with_identity(Identity { user_id: "U0BOT".into(), bot_id: Some("B0BOT".into()) });
    (fake, api)
}

async fn serve(history: History) -> (FakeSlack, Api, Arc<History>) {
    let history = Arc::new(history);
    let served = history.clone();
    let (fake, api) = start(move |seen| served.answer(seen)).await;
    (fake, api, history)
}

/// A channel's order: its boundary (the floor's unless `boundary` is given), and the greatest event `ts` handed over.
fn order(boundary: Option<&str>, filed: &[String], seen: Option<&str>) -> Order {
    let absent = std::env::temp_dir().join("sot-chat-traverse-has-no-state-file");
    let mut state = State::load(&absent, WS, CHANNEL, &ts(FLOOR)).unwrap();
    state.commit(&ts(boundary.unwrap_or(FLOOR)));
    for f in filed {
        state.insert(&ts(f));
    }
    Order { state, floor: ts(FLOOR), seen: seen.map(ts) }
}

fn posts(h: History, range: std::ops::Range<u64>) -> History {
    range.fold(h, |h, n| h.post(&at(n)))
}

fn mail_ts(got: &Passed) -> Vec<String> {
    got.mail.iter().map(|r| r.ts.as_str().to_string()).collect()
}

fn history_reads(fake: &FakeSlack) -> Vec<crate::fake_slack::Seen> {
    fake.seen().into_iter().filter(|s| s.path() == "/api/conversations.history").collect()
}

fn replies_reads(fake: &FakeSlack) -> Vec<crate::fake_slack::Seen> {
    fake.seen().into_iter().filter(|s| s.path() == "/api/conversations.replies").collect()
}

#[tokio::test]
async fn a_quiet_channels_one_post_made_while_the_socket_was_down_is_filed_at_the_product_margin() {
    bounded(async {
        // The channel's only new post is one second before U: inside the margin, so above H, and still the pass's mail.
        let (fake, api, _) = serve(History::new().post(&at(100)).post(&at(990))).await;
        let order = order(Some(&at(500)), &[], None);
        let got = pass(&api, &order, &ts(U), NO_PACE).await;
        assert_eq!(got.outcome, Outcome::Complete);
        assert_eq!(mail_ts(&got), [at(990)], "a post above H is a candidate; a filter to the interval at or below H drops it");
        let newest = ts_key(&ts(&at(990)));
        let h = ts_key(&got.horizon);
        assert_eq!((h.0, h.1), (newest.0 - MARGIN.as_secs() as u128, newest.1), "H is the newest ts less the margin");
        assert!(h < newest);
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn every_read_is_bounded_by_the_passs_upper_bound_and_h_is_capped_at_it() {
    bounded(async {
        // First read with both bounds. A channel of 47 posts in pages of 10: a first read with `oldest` alone gets the
        // oldest page, and the next read, `latest` lowered from it, gets an empty one, so 10 of 47 would look complete.
        let (fake, api, _) = serve(posts(History::new(), 1..48).cap(10)).await;
        let got = pass(&api, &order(None, &[], None), &ts(U), NO_PACE).await;
        assert_eq!((got.outcome.clone(), got.mail.len(), got.reads), (Outcome::Complete, 47, 5));
        let reads = history_reads(&fake);
        assert_eq!(reads[0].param("latest").as_deref(), Some(U), "the first read carries U");
        for (i, r) in reads.iter().enumerate() {
            assert_eq!(r.param("oldest").as_deref(), Some(FLOOR), "read {i}");
            assert_eq!(r.param("inclusive").as_deref(), Some("false"), "read {i}");
            assert!(ts_key(&ts(&r.param("latest").unwrap())) <= ts_key(&ts(U)), "read {i} is bounded by U");
            assert_eq!(r.param("cursor"), None, "read {i}");
        }
        fake.shutdown().await;

        // An event handed over with a ts above U raises H no higher than U.
        let (fake, api, _) = serve(History::new().post(&at(100))).await;
        let above = "1700001100.000000";
        let got = pass(&api, &order(None, &[], Some(above)), &ts(U), NO_PACE).await;
        assert_eq!(got.horizon, ts(U), "H is capped at U, however far above it an event's ts lies");
        fake.shutdown().await;

        // A parent's `latest_reply` a fraction of a second above the read's `latest`, as Slack sends it while a reply is in
        // flight: the list respects `latest`, the reply is not in the page, and H stays under U, at the page's newest value
        // less the margin.
        let newer = "1700001000.300000";
        let h = History::new().post(&at(100)).parent(&at(200), newer).reply(&at(200), newer);
        let (fake, api, _) = serve(h).await;
        let got = pass(&api, &order(None, &[], None), &ts(U), NO_PACE).await;
        assert_eq!(mail_ts(&got), [at(100), at(200)], "the reply is above U, left for the next pass, and its thread is read");
        assert_eq!(replies_reads(&fake).len(), 1);
        assert_eq!(got.horizon, ts("1700000998.300000"));
        assert!(ts_key(&got.horizon) <= ts_key(&ts(U)));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn h_is_never_below_the_boundary_and_a_floor_at_or_above_u_reads_nothing() {
    bounded(async {
        let (fake, api, _) = serve(History::new().post(&at(10))).await;
        // The channel is empty above the boundary: nothing to hand over, H stays where the boundary is.
        let got = pass(&api, &order(Some(&at(500)), &[], None), &ts(U), NO_PACE).await;
        assert_eq!((got.outcome.clone(), got.mail.len(), got.horizon.clone()), (Outcome::Complete, 0, ts(&at(500))));
        // A floor that is not below U (a clock behind) has nothing to read.
        let got = pass(&api, &order(None, &[], None), &ts(FLOOR), NO_PACE).await;
        assert_eq!((got.outcome, got.reads), (Outcome::Complete, 0));
        assert_eq!(history_reads(&fake).len(), 1);
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_boundary_at_or_above_u_reads_no_thread() {
    bounded(async {
        // The boundary is above U (a clock behind) with the floor below it: the channel is read, and a parent's
        // `latest_reply` is above the boundary, but no reply lies between the boundary and U, so the thread is not asked
        // for (a read with its bounds the wrong way round would fail the pass).
        let (fake, api, _) = serve(History::new().parent(&at(10), &at(1_600)).reply(&at(10), &at(1_600))).await;
        let got = pass(&api, &order(Some(&at(1_500)), &[], None), &ts(U), NO_PACE).await;
        assert_eq!((got.outcome.clone(), got.reads, got.mail.len()), (Outcome::Complete, 1, 0));
        assert!(replies_reads(&fake).is_empty());
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn fifty_thousand_roots_take_51_reads_and_an_old_threads_reply_is_found() {
    bounded(async {
        // All roots at or below the boundary, one parent near the floor with a reply made with no socket: the channel is
        // read 999 at a time, 51 reads, where a pass at the display page would make 2,500, and the one thread is one more.
        let big = "1700100000.000000";
        let h = posts(History::new().parent(&at(5), &at(60_000)).reply(&at(5), &at(60_000)), 10..50_000);
        let (fake, api, _) = serve(h).await;
        // The guard is 53 reads here, so a pass at the display page fails at once instead of making its 2,500.
        let got = walk(&api, &order(Some(&at(55_000)), &[], None), &ts(big), NO_PACE, 53).await;
        assert_eq!((got.outcome.clone(), mail_ts(&got), got.reads), (Outcome::Complete, vec![at(60_000)], 52));
        let limits: Vec<_> = history_reads(&fake).iter().map(|s| s.param("limit").unwrap()).collect();
        assert!(limits.iter().all(|l| l == "999"), "{limits:?} is not the display page of {MAX_PAGE}");
        let threads = replies_reads(&fake);
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].param("limit").as_deref(), Some("999"));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn recovery_finds_disconnected_untracked_old_thread_reply() {
    bounded(async {
        // A parent older than the boundary gets a reply while no socket is open, so no event names it and the filed set
        // does not hold it. The channel page shows only the parent's `latest_reply`; the thread is read for the reply. A
        // parent whose newest reply is not after the boundary is not read, and the thread's read is bounded as the
        // channel's: the boundary below, U above, `inclusive=false`, no cursor.
        let h = History::new()
            .parent(&at(10), &at(600))
            .reply(&at(10), &at(300))
            .reply(&at(10), &at(600))
            .parent(&at(20), &at(400))
            .reply(&at(20), &at(400))
            .post(&at(450));
        let (fake, api, _) = serve(h).await;
        let got = pass(&api, &order(Some(&at(500)), &[], None), &ts(U), NO_PACE).await;
        assert_eq!((got.outcome.clone(), got.reads), (Outcome::Complete, 2));
        assert_eq!(mail_ts(&got), [at(600)], "the reply made while disconnected is the pass's mail, and only it");
        let threads = replies_reads(&fake);
        assert_eq!(threads.len(), 1, "a parent whose newest reply is not after the boundary is not read");
        let t = &threads[0];
        let key = |name: &str| t.param(name);
        assert_eq!(key("ts"), Some(at(10)));
        assert_eq!((key("oldest"), key("latest"), key("inclusive")), (Some(at(500)), Some(U.into()), Some("false".into())));
        assert_eq!((key("cursor"), key("limit")), (None, Some("999".into())));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_thread_longer_than_a_page_is_read_whole() {
    bounded(async {
        // 2,500 replies after the boundary, all filed but the one in the middle page. The thread comes back newest page
        // first, 999 replies and the parent a page, `latest` lowered to each page's oldest reply and never to the parent
        // (which lies below the boundary here: a walk that took it would end at once); three pages hold the reply.
        let reply = |i: u64| at(11_000 + i);
        let h = (0..2_500).fold(History::new().parent(&at(5), &reply(2_499)), |h, i| h.reply(&at(5), &reply(i)));
        let filed: Vec<String> = (0..2_500).filter(|&i| i != 1_000).map(reply).collect();
        let (fake, api, _) = serve(h).await;
        let got = pass(&api, &order(Some(&at(10_000)), &filed, None), &ts("1700100000.000000"), NO_PACE).await;
        assert_eq!((got.outcome.clone(), mail_ts(&got), got.reads), (Outcome::Complete, vec![reply(1_000)], 4));
        let latest: Vec<_> = replies_reads(&fake).iter().map(|s| s.param("latest").unwrap()).collect();
        assert_eq!(latest, ["1700100000.000000".to_string(), reply(1_501), reply(502)], "each read starts at the page before's oldest reply");
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_walk_ends_at_the_first_page_with_no_more() {
    bounded(async {
        // 999 posts are one full page with nothing behind it: the walk does not read on to an empty page. 1,500 are two.
        for (count, reads) in [(999, 1), (1_500, 2)] {
            let (fake, api, _) = serve(posts(History::new(), 1..count + 1)).await;
            let got = pass(&api, &order(Some(&at(5_000)), &[], None), &ts("1700100000.000000"), NO_PACE).await;
            assert_eq!((got.outcome, got.reads), (Outcome::Complete, reads), "{count} posts");
            fake.shutdown().await;
        }
        // The same of a thread, whose page holds its parent on top of the limit: its 999 replies are one read, and the
        // parent alone, the read after the last, is never made. One more reply is a second page.
        for (count, reads) in [(999, 2), (1_000, 3)] {
            let h = (1..count + 1).fold(History::new().parent(&at(5), &at(9_000)), |h, n| h.reply(&at(5), &at(10 + n)));
            let (fake, api, _) = serve(h).await;
            // All filed, so the cap is not what ends the pass.
            let filed: Vec<String> = (1..count + 1).map(|n| at(10 + n)).collect();
            let got = pass(&api, &order(Some(&at(8)), &filed, None), &ts("1700100000.000000"), NO_PACE).await;
            assert_eq!((got.outcome, got.reads), (Outcome::Complete, reads), "{count} replies");
            fake.shutdown().await;
        }
    })
    .await;
}

#[tokio::test]
async fn a_channel_slack_limits_is_read_to_its_last_page_and_counts_complete() {
    bounded(async {
        // Every page of a three-page channel is `is_limited`; an old parent on the last page has three replies after the
        // boundary, two pages of its thread, each `is_limited` too.
        let h = History::new().parent(&at(1), &at(900)).post(&at(2)).post(&at(3)).post(&at(4)).post(&at(5));
        let h = [800, 850, 900].iter().fold(h, |h, n| h.reply(&at(1), &at(*n))).cap(2).limited();
        let (fake, api, _) = serve(h).await;
        let got = pass(&api, &order(Some(&at(2)), &[], None), &ts(U), NO_PACE).await;
        assert_eq!((got.outcome.clone(), got.reads, got.limited), (Outcome::Complete, 5, true));
        assert_eq!(mail_ts(&got), [at(3), at(4), at(5), at(800), at(850), at(900)]);
        // H comes from the first page, [5, 4], alone: the old parent's later `latest_reply` on page 3 does not raise it.
        assert_eq!(got.horizon, ts(&at(3)));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_page_of_joins_moves_the_walk_on_its_raw_oldest_ts() {
    bounded(async {
        // The middle page holds only joins: no record, but its raw oldest `ts` is where the walk goes on from.
        let join = |n: u64| json!({"ts": at(n), "subtype": "channel_join", "user": "U0MEM", "text": "joined"});
        let h = History::new().post(&at(1)).raw(join(2)).raw(join(3)).post(&at(4)).post(&at(5)).cap(2);
        let (fake, api, _) = serve(h).await;
        let got = pass(&api, &order(None, &[], None), &ts(U), NO_PACE).await;
        assert_eq!((got.outcome.clone(), got.reads, mail_ts(&got)), (Outcome::Complete, 3, vec![at(1), at(4), at(5)]));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_short_page_with_more_behind_it_is_read_on() {
    bounded(async {
        let (fake, api, _) = serve(posts(History::new(), 1..11).cap(3)).await;
        let got = pass(&api, &order(None, &[], None), &ts(U), NO_PACE).await;
        assert_eq!((got.outcome, got.mail.len(), got.reads), (Outcome::Complete, 10, 4));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn incomplete_recovery_keeps_its_candidates_and_reports_it() {
    bounded(async {
        let limited = Reply::Status { code: 429, headers: vec![("Retry-After".into(), "7".into())], body: Vec::new() };
        let cases: Vec<(&str, Reply, Outcome)> = vec![
            ("a 500", Reply::Status { code: 500, headers: Vec::new(), body: Vec::new() }, Outcome::ReadFailed),
            ("a 429", limited, Outcome::RateLimited { retry_after: Duration::from_secs(7) }),
            (
                "ratelimited",
                Reply::json(&json!({"ok": false, "error": "ratelimited"})),
                Outcome::RateLimited { retry_after: Duration::from_secs(30) },
            ),
            // `has_more` with a page whose oldest is `latest` again (the message the exclusive bound should have left out):
            // the walk cannot move, and never loops.
            (
                "a page that cannot move the walk",
                Reply::json(&json!({"ok": true, "has_more": true, "messages": [{"ts": at(4), "user": "U0MEM", "text": "x"}]})),
                Outcome::ReadFailed,
            ),
        ];
        for (what, second, outcome) in cases {
            let history = Arc::new(posts(History::new(), 1..6).cap(2));
            let (calls, served, second) = (AtomicUsize::new(0), history.clone(), std::sync::Mutex::new(Some(second)));
            let (fake, api) = start(move |seen| match calls.fetch_add(1, Ordering::SeqCst) {
                1 => second.lock().unwrap().take().unwrap_or_else(|| Reply::Drop),
                _ => served.answer(seen),
            })
            .await;
            let got = pass(&api, &order(None, &[], None), &ts(U), NO_PACE).await;
            assert_eq!(got.outcome, outcome, "{what}");
            assert_eq!((got.reads, mail_ts(&got)), (2, vec![at(4), at(5)]), "{what}: the first page's candidates are kept");
            fake.shutdown().await;
        }

        // The read guard, and a client the service closed.
        let (fake, api, _) = serve(posts(History::new(), 1..6).cap(1)).await;
        let got = walk(&api, &order(None, &[], None), &ts(U), NO_PACE, 2).await;
        assert_eq!((got.outcome.clone(), got.reads, mail_ts(&got)), (Outcome::TooLong, 2, vec![at(4), at(5)]));
        api.close();
        assert_eq!(pass(&api, &order(None, &[], None), &ts(U), NO_PACE).await.outcome, Outcome::Stopped);
        assert!(matches!(api.auth_test().await, Err(ApiError::Closed)));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn the_cap_applies_after_the_filed_set() {
    bounded(async {
        // 450 posts, the oldest 200 already filed: the cap keeps the next 200 of the 250 left, and says so.
        let filed: Vec<String> = (1..201).map(at).collect();
        let (fake, api, _) = serve(posts(History::new(), 1..451)).await;
        let got = pass(&api, &order(None, &filed, None), &ts(U), NO_PACE).await;
        assert_eq!(got.outcome, Outcome::Truncated);
        assert_eq!(mail_ts(&got), (201..201 + MAX_PASS_MAIL as u64).map(at).collect::<Vec<_>>());
        // One post that arrives twice (a broadcast is listed in the channel and in its thread) is one candidate.
        let again = json!({"ts": at(7), "user": "U0MEM", "text": "twice"});
        let (fake2, api2, _) = serve(History::new().raw(again.clone()).raw(again)).await;
        let got = pass(&api2, &order(None, &[], None), &ts(U), NO_PACE).await;
        assert_eq!(mail_ts(&got), [at(7)]);
        fake.shutdown().await;
        fake2.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn an_oversize_page_halves_and_is_read_again() {
    bounded(async {
        // A page past its response bound (an endless body), or past the read deadline, is asked for again at half the
        // size, down to what Slack can answer in time, and the halved limit holds for the rest of the pass: 600 messages
        // are three pages at 249. The rule is the same for the channel's roots and for one thread's replies, the two
        // shapes a read takes, so each is run through it; the fault is on that shape's method alone.
        let flood = || Reply::Flood(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec());
        for (shape, path) in [("roots", "/api/conversations.history"), ("replies", "/api/conversations.replies")] {
            for (what, fault) in [("too large", flood as fn() -> Reply), ("too slow", || Reply::Hang)] {
                let h = match shape {
                    "roots" => posts(History::new(), 1..601),
                    _ => (1..601).fold(History::new().parent(&at(0), &at(700)), |h, n| h.reply(&at(0), &at(n))),
                };
                let history = Arc::new(h);
                let served = history.clone();
                let (fake, api) = start(move |seen| {
                    let big = seen.param("limit").and_then(|l| l.parse::<u32>().ok()) > Some(300);
                    if seen.path() == path && big { fault() } else { served.answer(seen) }
                })
                .await;
                let api = api.with_deadlines(Duration::from_millis(400), Duration::from_secs(1), Duration::from_secs(1));
                // Every message but the oldest is filed, so the one found proves the last page was read.
                let filed: Vec<String> = (2..601).map(at).collect();
                let got = pass(&api, &order(Some(&at(0)), &filed, None), &ts(U), NO_PACE).await;
                let reads = if shape == "roots" { 5 } else { 6 };
                assert_eq!((got.outcome.clone(), mail_ts(&got), got.reads), (Outcome::Complete, vec![at(1)], reads), "{shape} {what}");
                let seen = fake.seen().into_iter().filter(|s| s.path() == path);
                let limits: Vec<_> = seen.map(|s| s.param("limit").unwrap()).collect();
                assert_eq!(limits, ["999", "499", "249", "249", "249"], "{shape} {what}");
                fake.shutdown().await;
            }
        }
    })
    .await;
}

#[tokio::test]
async fn a_pass_paces_its_reads() {
    bounded(async {
        let pace = Duration::from_millis(400);
        let (fake, api, history) = serve(posts(History::new(), 1..5).cap(1)).await;
        let got = pass(&api, &order(None, &[], None), &ts(U), pace).await;
        assert_eq!((got.outcome, got.reads), (Outcome::Complete, 4));
        // The first request's arrival is late by its connection's handshake, which the test does not own, so the first gap
        // is held to a quarter of the pace (a skipped pace there is one round trip, far below it) and every later gap to
        // three quarters.
        let asked = history.asked();
        for (i, gap) in asked.windows(2).map(|w| w[1] - w[0]).enumerate() {
            let least = if i == 0 { pace / 4 } else { pace * 3 / 4 };
            assert!(gap >= least, "gap {i} between reads was {gap:?}, pace {pace:?}");
        }
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_broadcast_is_filed_once_though_the_channel_and_its_thread_both_list_it() {
    bounded(async {
        // The broadcast reply is in the channel's history and in its parent's thread: one candidate, from either read.
        let h = History::new().parent(&at(10), &at(600)).broadcast(&at(10), &at(600)).reply(&at(10), &at(650));
        let (fake, api, _) = serve(h).await;
        let got = pass(&api, &order(Some(&at(500)), &[], None), &ts(U), NO_PACE).await;
        assert_eq!((got.outcome.clone(), got.reads, mail_ts(&got)), (Outcome::Complete, 2, vec![at(600), at(650)]));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_failed_thread_read_keeps_the_mail_already_read_and_the_pass_incomplete() {
    bounded(async {
        // Two parents with a reply each. The channel page lists the newer first, so the newer's thread is read on call 2
        // and the older's on call 3, which fails: the channel's mail and the newer thread's reply are kept, no reply of the
        // older one is, and the pass is not `Complete`.
        let limited = Reply::Status { code: 429, headers: vec![("Retry-After".into(), "7".into())], body: Vec::new() };
        // A thread page with more behind it whose oldest reply is not below `latest`: it cannot move the walk.
        let stuck = json!({"ok": true, "has_more": true, "messages": [
            {"ts": at(1), "thread_ts": at(1), "user": "U0MEM", "text": "x"},
            {"ts": U, "thread_ts": at(1), "user": "U0MEM", "text": "x"}]});
        let cases: Vec<(&str, Reply, Outcome)> = vec![
            ("a 500", Reply::Status { code: 500, headers: Vec::new(), body: Vec::new() }, Outcome::ReadFailed),
            ("a 429", limited, Outcome::RateLimited { retry_after: Duration::from_secs(7) }),
            (
                "ratelimited",
                Reply::json(&json!({"ok": false, "error": "ratelimited"})),
                Outcome::RateLimited { retry_after: Duration::from_secs(30) },
            ),
            ("a thread page that cannot move the walk", Reply::json(&stuck), Outcome::ReadFailed),
        ];
        for (what, third, outcome) in cases {
            let h = History::new().parent(&at(1), &at(800)).parent(&at(2), &at(900)).reply(&at(1), &at(800)).reply(&at(2), &at(900));
            let history = Arc::new(h);
            let (calls, served, third) = (AtomicUsize::new(0), history.clone(), std::sync::Mutex::new(Some(third)));
            let (fake, api) = start(move |seen| match calls.fetch_add(1, Ordering::SeqCst) {
                2 => third.lock().unwrap().take().unwrap_or_else(|| Reply::Drop),
                _ => served.answer(seen),
            })
            .await;
            let got = pass(&api, &order(None, &[], None), &ts(U), NO_PACE).await;
            assert_eq!(got.outcome, outcome, "{what}");
            assert_eq!((got.reads, mail_ts(&got)), (3, vec![at(1), at(2), at(900)]), "{what}: what was read is kept");
            fake.shutdown().await;
        }
    })
    .await;
}

#[tokio::test]
async fn a_thread_slack_says_is_not_found_reads_as_empty_and_is_counted() {
    bounded(async {
        // Two parents with a reply each; Slack has no thread for the newer parent (a deleted parent's, say) and serves the
        // older one's. The pass goes on to the older thread, ends `Complete`, hands over its reply and counts the skip.
        let h = History::new().parent(&at(1), &at(800)).parent(&at(2), &at(900)).reply(&at(1), &at(800));
        let history = Arc::new(h);
        let served = history.clone();
        let (fake, api) = start(move |seen| match (seen.path().as_str(), seen.param("ts")) {
            ("/api/conversations.replies", Some(parent)) if parent == at(2) => Reply::json(&json!({"ok": false, "error": "thread_not_found"})),
            _ => served.answer(seen),
        })
        .await;
        let got = pass(&api, &order(None, &[], None), &ts(U), NO_PACE).await;
        assert_eq!((got.outcome.clone(), got.reads, got.skipped), (Outcome::Complete, 3, 1));
        assert_eq!(mail_ts(&got), [at(1), at(2), at(800)], "the served thread's reply is handed over");
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn any_other_slack_error_on_a_thread_read_still_ends_the_pass() {
    bounded(async {
        for code in ["channel_not_found", "internal_error"] {
            let h = History::new().parent(&at(1), &at(800)).reply(&at(1), &at(800));
            let history = Arc::new(h);
            let served = history.clone();
            let (fake, api) = start(move |seen| match seen.path().as_str() {
                "/api/conversations.replies" => Reply::json(&json!({"ok": false, "error": code})),
                _ => served.answer(seen),
            })
            .await;
            let got = pass(&api, &order(None, &[], None), &ts(U), NO_PACE).await;
            assert_eq!((got.outcome.clone(), got.skipped), (Outcome::ReadFailed, 0), "{code}");
            fake.shutdown().await;
        }
    })
    .await;
}

#[tokio::test]
async fn thread_reads_are_paced_and_counted_by_the_guard() {
    bounded(async {
        // A channel page and two threads: three reads, each at least the pace after the one before, and a guard of two
        // reads ends the pass at the second thread with what the first gave.
        let h = History::new().parent(&at(1), &at(800)).parent(&at(2), &at(900)).reply(&at(1), &at(800)).reply(&at(2), &at(900));
        let (fake, api, history) = serve(h).await;
        let pace = Duration::from_millis(400);
        let got = pass(&api, &order(Some(&at(5)), &[], None), &ts(U), pace).await;
        assert_eq!((got.outcome.clone(), got.reads, mail_ts(&got)), (Outcome::Complete, 3, vec![at(800), at(900)]));
        let asked = history.asked();
        assert_eq!(asked.len(), 3);
        // The first gap is held to a quarter of the pace (the connection's handshake is not the test's), the next to
        // three quarters, as `a_pass_paces_its_reads` does.
        for (i, gap) in asked.windows(2).map(|w| w[1] - w[0]).enumerate() {
            let least = if i == 0 { pace / 4 } else { pace * 3 / 4 };
            assert!(gap >= least, "gap {i} between reads was {gap:?}, pace {pace:?}");
        }
        let got = walk(&api, &order(Some(&at(5)), &[], None), &ts(U), NO_PACE, 2).await;
        assert_eq!((got.outcome.clone(), got.reads, mail_ts(&got)), (Outcome::TooLong, 2, vec![at(900)]), "the thread read is one of the guard's");
        assert_eq!(replies_reads(&fake).len(), 3, "two threads, then the first of the second pass, and no more");
        fake.shutdown().await;
    })
    .await;
}
