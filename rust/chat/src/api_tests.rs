//! Behavioral tests of `api` against the scripted fake Slack over real TLS. Every id, name and timestamp is synthetic.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sot_protocol::chat::Origin;

use crate::api::{
    identify_at, tag_of, Api, ApiError, Bounds, Channel, Continuation, FileFetch, FileOutcome, FileVariant, HistoryQuery,
    Identified, JoinOutcome, PostKind, PostOutcome, ScanQuery, ASSUMED_RETRY, MAX_CHAT_FILE, MAX_CURSOR, MAX_LISTED,
    MAX_NAMES, MAX_RETRY, MAX_SCAN, MAX_THUMB,
};
use crate::fake_slack::{eventually, FakeSlack, Files, Identity, Reply, Seen, Upload};
use crate::http::{HttpError, Secret};
use crate::normalize::MAX_TEXT;

const TOKEN: &str = "sentinel-bot-token-not-a-credential";
/// The person's user token, a different bearer from the bot's so a test can tell which one a request carried.
const PERSON: &str = "sentinel-person-token-not-a-credential";
const WS: &str = "T0TEAM";
const OWN_USER: &str = "U0BOT";
const OWN_BOT: &str = "B0BOT";
/// Retry headers a limited answer carries, the wait they mean, and whether it was assumed.
type Case<'a> = (&'a [(&'a str, &'a str)], Duration, bool);

const T0: &str = "1700000000.000000";

/// Every test body runs under one bound: a hung fixture fails here instead of waiting.
async fn bounded<T>(body: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(90), body)
        .await
        .expect("the test exceeded its bound")
}

/// An `Api` holding the bot's token and the person's.
fn new_api(fake: &FakeSlack) -> Api {
    new_api_with(fake, Some(PERSON))
}

fn new_api_with(fake: &FakeSlack, person: Option<&str>) -> Api {
    Api::new(fake.http(), Secret::new(TOKEN), WS)
        .unwrap()
        .with_person(person.map(Secret::new))
        .with_base(fake.url("slack.com", "/api/"))
}

/// An `Api` that has read the bot identity.
async fn ready(fake: &FakeSlack) -> Api {
    let api = new_api(fake);
    api.auth_test().await.expect("auth.test");
    api
}

/// `auth.test`, answered for every script.
fn auth(seen: &Seen) -> Option<Reply> {
    (seen.path() == "/api/auth.test").then(|| {
        Reply::json(&json!({"ok": true, "team_id": WS, "user_id": OWN_USER, "bot_id": OWN_BOT}))
    })
}

fn status(code: u16, headers: &[(&str, &str)], body: &Value) -> Reply {
    Reply::Status {
        code,
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        body: body.to_string().into_bytes(),
    }
}

fn calls(fake: &FakeSlack, method: &str) -> usize {
    let path = format!("/api/{method}");
    fake.seen().iter().filter(|s| s.path() == path).count()
}

fn msg(ts: &str) -> Value {
    json!({"ts": ts, "user": "U0MEM", "text": format!("text {ts}")})
}

fn page(messages: Vec<Value>, more: Option<&str>) -> Value {
    json!({"ok": true, "messages": messages, "has_more": more.is_some(),
           "response_metadata": {"next_cursor": more.unwrap_or("")}})
}

fn query(channel: &str, bounds: Bounds, limit: u16) -> HistoryQuery {
    HistoryQuery::new(channel, None, bounds, limit).unwrap()
}

fn tss(page: &crate::api::Page) -> Vec<&str> {
    page.records.iter().map(|r| r.ts.as_str()).collect()
}

#[tokio::test]
async fn history_pages_exact_query_without_silent_completion() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            let channel = seen.param("channel").unwrap_or_default();
            let thread = seen.path() == "/api/conversations.replies";
            match (channel.as_str(), seen.param("cursor").as_deref()) {
                ("C0CHAN", None) => Reply::json(&page(
                    vec![msg("1700000300.000001"), msg("1700000200.000002")],
                    Some("c1"),
                )),
                ("C0CHAN", Some("c1")) => Reply::json(&page(vec![msg("1700000100.000003")], None)),
                // has_more with an empty cursor: incomplete, never the last page.
                ("C0EMPTY", _) => Reply::json(&page(vec![msg("1700000100.000003")], Some(""))),
                ("C0NOKEY", _) => {
                    Reply::json(&json!({"ok": true, "messages": [], "has_more": true}))
                }
                // The cursor sent comes straight back.
                ("C0LOOP", _) => Reply::json(&page(vec![msg("1700000100.000003")], Some("same"))),
                ("C0BIG", _) => Reply::json(&page(
                    vec![msg("1700000100.000003")],
                    Some(&"x".repeat(2000)),
                )),
                ("C0ERR", _) => Reply::json(&json!({"ok": false, "error": "channel_not_found"})),
                ("C0NOARR", _) => {
                    Reply::json(&json!({"ok": true, "messages": {}, "has_more": false}))
                }
                ("C0NOMORE", _) => Reply::json(&json!({"ok": true, "messages": []})),
                ("C0MANY", _) => Reply::json(&page(
                    vec![
                        msg("1700000100.000001"),
                        msg("1700000100.000002"),
                        msg("1700000100.000003"),
                    ],
                    None,
                )),
                ("C0JUNK", _) => Reply::json(&page(vec![json!({"text": "no ts"})], None)),
                ("C0LIM", _) => {
                    let mut v = page(vec![msg("1700000100.000001")], None);
                    v["is_limited"] = json!(true);
                    Reply::json(&v)
                }
                // Slack's edge inclusivity is not trusted: the client rechecks every message.
                // Edges that differ only in the fraction.
                ("C0FRAC", _) => Reply::json(&page(
                    vec![
                        msg("1700000100.000002"),
                        msg("1700000100.000001"),
                        msg("1700000100.000000"),
                    ],
                    None,
                )),
                ("C0EDGE", _) => Reply::json(&page(
                    vec![
                        msg("1700000500.000000"),
                        msg("1700000400.000000"),
                        msg("1700000300.000000"),
                        msg("1700000100.000000"),
                        msg("1700000099.999999"),
                    ],
                    None,
                )),
                ("C0THR", _) if thread => {
                    let mut parent = msg("1700000100.000000");
                    parent["thread_ts"] = json!("1700000100.000000");
                    let reply = |ts: &str| {
                        let mut m = msg(ts);
                        m["thread_ts"] = json!("1700000100.000000");
                        m
                    };
                    Reply::json(&page(
                        vec![
                            parent,
                            reply("1700000101.000000"),
                            reply("1700000102.000000"),
                        ],
                        Some("t1"),
                    ))
                }
                ("C0THRX", _) if thread => Reply::json(&page(
                    (1..=4)
                        .map(|i| msg(&format!("1700000{i}00.000000")))
                        .collect(),
                    None,
                )),
                _ => Reply::json(&json!({"ok": false, "error": "unexpected"})),
            }
        })
        .await;
        let api = ready(&fake).await;

        // The exact query, and a two-page walk to the end.
        let bounds =
            Bounds::new(Some("1699999000.000000"), Some("1700001000.000000"), None).unwrap();
        let q = query("C0CHAN", bounds, 2);
        let first = api.history(&q, None).await.unwrap();
        assert_eq!(tss(&first), ["1700000300.000001", "1700000200.000002"]);
        assert!(!first.limited);
        let asked = &fake.seen()[1];
        assert_eq!(asked.method, "GET");
        assert_eq!(asked.path(), "/api/conversations.history");
        for (k, v) in [
            ("channel", "C0CHAN"),
            ("limit", "2"),
            ("include_all_metadata", "true"),
            ("oldest", "1699999000.000000"),
            ("latest", "1700001000.000000"),
            ("inclusive", "false"),
        ] {
            assert_eq!(asked.param(k).as_deref(), Some(v), "{k}");
        }
        assert_eq!(asked.param("cursor"), None);
        assert_eq!(
            asked.authorization.as_deref(),
            Some(&*format!("Bearer {TOKEN}"))
        );
        let cont = first.next.clone().expect("more pages");
        assert!(cont.as_str().len() <= MAX_CURSOR);
        let second = api.history(&q, Some(&cont)).await.unwrap();
        assert_eq!(tss(&second), ["1700000100.000003"]);
        assert!(second.next.is_none(), "the last page says so");
        assert_eq!(fake.seen()[2].param("cursor").as_deref(), Some("c1"));

        // `until` is inclusive, `after` alone sends no upper bound.
        let until = Bounds::new(None, None, Some("1700000500.000000")).unwrap();
        let _ = api.history(&query("C0LIM", until, 5), None).await.unwrap();
        let seen = fake.seen();
        assert_eq!(
            seen.last().unwrap().param("inclusive").as_deref(),
            Some("true")
        );
        assert_eq!(seen.last().unwrap().param("oldest"), None);

        // A continuation cannot be reused under another query, and nothing is sent when it is refused. Each row
        // differs from the query it was made for (`q`) in exactly one component.
        let before = fake.seen().len();
        let (a0, b0) = ("1699999000.000000", "1700001000.000000");
        let row =
            |channel: &str, thread: Option<&str>, after: &str, upper: (&str, &str), limit: u16| {
                let (before, until) = match upper {
                    ("before", t) => (Some(t), None),
                    (_, t) => (None, Some(t)),
                };
                let bounds = Bounds::new(Some(after), before, until).unwrap();
                HistoryQuery::new(channel, thread, bounds, limit).unwrap()
            };
        assert_eq!(
            row("C0CHAN", None, a0, ("before", b0), 2),
            q,
            "the table's base"
        );
        let one_off = [
            ("channel", row("C0OTHR", None, a0, ("before", b0), 2)),
            (
                "thread",
                row("C0CHAN", Some("1700000100.000000"), a0, ("before", b0), 2),
            ),
            (
                "after",
                row("C0CHAN", None, "1699999000.000001", ("before", b0), 2),
            ),
            ("bound kind", row("C0CHAN", None, a0, ("until", b0), 2)),
            (
                "bound value",
                row("C0CHAN", None, a0, ("before", "1700001000.000001"), 2),
            ),
            ("limit", row("C0CHAN", None, a0, ("before", b0), 3)),
        ];
        for (what, other) in &one_off {
            assert_eq!(
                api.history(other, Some(&cont)).await.unwrap_err(),
                ApiError::Altered,
                "only the {what} differs"
            );
        }
        // Only the workspace differs: a second `Api` for another workspace given the first one's continuation.
        let elsewhere = Api::new(fake.http(), Secret::new(TOKEN), "T0OTHR")
            .unwrap()
            .with_base(fake.url("slack.com", "/api/"))
            .with_identity(crate::api::Identity {
                user_id: OWN_USER.into(),
                bot_id: Some(OWN_BOT.into()),
                app_id: None,
            });
        assert_eq!(
            elsewhere.history(&q, Some(&cont)).await.unwrap_err(),
            ApiError::Altered,
            "only the workspace differs"
        );
        for text in ["not json", "[]", "[1,2]", "{}"] {
            let junk = Continuation::parse(text.into()).unwrap();
            assert_eq!(
                api.history(&q, Some(&junk)).await.unwrap_err(),
                ApiError::Altered
            );
        }
        assert_eq!(
            Continuation::parse("x".repeat(MAX_CURSOR + 1)).unwrap_err(),
            ApiError::Altered
        );
        assert_eq!(fake.seen().len(), before, "refused before sending");

        // Incomplete, repeated, oversized and malformed pages fail explicitly.
        let plain = |c: &str| query(c, Bounds::default(), 2);
        for c in ["C0EMPTY", "C0NOKEY"] {
            assert_eq!(
                api.history(&plain(c), None).await.unwrap_err(),
                ApiError::Missing,
                "{c}"
            );
        }
        let looped = plain("C0LOOP");
        let first = api.history(&looped, None).await.unwrap();
        let again = api.history(&looped, first.next.as_ref()).await;
        assert_eq!(again.unwrap_err(), ApiError::Repeated);
        assert_eq!(
            api.history(&plain("C0BIG"), None).await.unwrap_err(),
            ApiError::TooLarge
        );
        assert_eq!(
            api.history(&plain("C0ERR"), None).await.unwrap_err(),
            ApiError::Slack("channel_not_found".into())
        );
        for c in ["C0NOARR", "C0NOMORE", "C0MANY", "C0JUNK"] {
            assert_eq!(
                api.history(&plain(c), None).await.unwrap_err(),
                ApiError::Malformed,
                "{c}"
            );
        }

        // The horizon is passed on, not an error.
        let limited = api.history(&plain("C0LIM"), None).await.unwrap();
        assert!(limited.limited && limited.records.iter().all(|r| r.limited));

        // The client rechecks the bounds itself: after 1700000100 exclusive, before 1700000400 exclusive.
        let b = Bounds::new(Some("1700000100.000000"), Some("1700000400.000000"), None).unwrap();
        let edge = api.history(&query("C0EDGE", b, 5), None).await.unwrap();
        assert_eq!(tss(&edge), ["1700000300.000000"]);
        let b = Bounds::new(Some("1700000100.000000"), None, Some("1700000400.000000")).unwrap();
        let edge = api.history(&query("C0EDGE", b, 5), None).await.unwrap();
        assert_eq!(tss(&edge), ["1700000400.000000", "1700000300.000000"]);

        // The recheck reads the fraction, not whole seconds.
        let frac = |after: Option<&str>, before: Option<&str>, until: Option<&str>| {
            query("C0FRAC", Bounds::new(after, before, until).unwrap(), 5)
        };
        let t = "1700000100.000001";
        let got = api.history(&frac(Some(t), None, None), None).await.unwrap();
        assert_eq!(tss(&got), ["1700000100.000002"]);
        let got = api.history(&frac(None, Some(t), None), None).await.unwrap();
        assert_eq!(tss(&got), ["1700000100.000000"]);
        let got = api.history(&frac(None, None, Some(t)), None).await.unwrap();
        assert_eq!(tss(&got), ["1700000100.000001", "1700000100.000000"]);

        // A thread: replies, the parent may make 21 records (here 3 for limit 2); more is malformed.
        let thr = |c: &str| {
            HistoryQuery::new(c, Some("1700000100.000000"), Bounds::default(), 2).unwrap()
        };
        let replies = api.history(&thr("C0THR"), None).await.unwrap();
        assert_eq!(replies.records.len(), 3);
        // A thread's continuation belongs to that thread alone.
        let other =
            HistoryQuery::new("C0THR", Some("1700000100.000001"), Bounds::default(), 2).unwrap();
        assert_eq!(
            api.history(&other, replies.next.as_ref())
                .await
                .unwrap_err(),
            ApiError::Altered,
            "only the thread differs"
        );
        let asked = fake
            .seen()
            .into_iter()
            .rev()
            .find(|s| s.path() == "/api/conversations.replies")
            .unwrap();
        assert_eq!(asked.param("ts").as_deref(), Some("1700000100.000000"));
        // The parent is exempt from an `after` bound that would exclude it.
        let b = Bounds::new(Some("1700000100.000000"), None, None).unwrap();
        let q = HistoryQuery::new("C0THR", Some("1700000100.000000"), b, 2).unwrap();
        assert_eq!(api.history(&q, None).await.unwrap().records.len(), 3);
        // Only the parent is exempt: a reply outside the bounds is dropped.
        let b = Bounds::new(Some("1700000101.000000"), None, None).unwrap();
        let q = HistoryQuery::new("C0THR", Some("1700000100.000000"), b, 2).unwrap();
        let got = api.history(&q, None).await.unwrap();
        assert_eq!(tss(&got), ["1700000100.000000", "1700000102.000000"]);
        assert_eq!(
            api.history(&thr("C0THRX"), None).await.unwrap_err(),
            ApiError::Malformed
        );
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_bound_without_exactly_six_fraction_digits_is_refused_before_sending() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            auth(seen).unwrap_or_else(|| Reply::json(&page(vec![], None)))
        })
        .await;
        let api = ready(&fake).await;
        let after_auth = fake.seen().len();
        for bad in [
            "1700000000.12345",
            "1700000000.1234567",
            "1700000000.1234567890",
            "1700000000",
            "1700000000.",
            ".123456",
            "1700000000.12345a",
            "-1.123456",
            " 1700000000.123456",
            "",
        ] {
            assert_eq!(
                Bounds::new(Some(bad), None, None),
                Err(ApiError::BadBound),
                "{bad:?}"
            );
            assert_eq!(
                Bounds::new(None, Some(bad), None),
                Err(ApiError::BadBound),
                "{bad:?}"
            );
            assert_eq!(
                Bounds::new(None, None, Some(bad)),
                Err(ApiError::BadBound),
                "{bad:?}"
            );
            assert_eq!(
                HistoryQuery::new("C0CHAN", Some(bad), Bounds::default(), 5),
                Err(ApiError::BadBound),
                "thread {bad:?}"
            );
        }
        // Before and until together name two upper bounds.
        assert_eq!(
            Bounds::new(None, Some(T0), Some(T0)),
            Err(ApiError::BadBound)
        );
        // Other inputs.
        for limit in [0, 21] {
            assert_eq!(
                HistoryQuery::new("C0CHAN", None, Bounds::default(), limit),
                Err(ApiError::BadRequest)
            );
        }
        assert_eq!(
            HistoryQuery::new("bad channel", None, Bounds::default(), 5),
            Err(ApiError::BadRequest)
        );
        assert_eq!(fake.seen().len(), after_auth, "the fake saw nothing");
        // The control: exactly six digits go out.
        let ok = Bounds::new(Some("1700000000.123456"), None, None).unwrap();
        api.history(&query("C0CHAN", ok, 5), None).await.unwrap();
        assert_eq!(fake.seen().len(), after_auth + 1);
        assert_eq!(
            fake.seen().last().unwrap().param("oldest").as_deref(),
            Some("1700000000.123456")
        );
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn lost_post_answer_is_unknown_and_never_resent() {
    bounded(async {
        // The fake reads the whole body and closes with no answer.
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            auth(seen).unwrap_or(Reply::Drop)
        })
        .await;
        let api = Arc::new(ready(&fake).await);
        let posts = |fake: &FakeSlack| calls(fake, "chat.postMessage");

        let outcome = api
            .post("C0CHAN", None, PostKind::Person, "Ada Test", "hello")
            .await
            .unwrap();
        assert_eq!(outcome, PostOutcome::Unknown);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(posts(&fake), 1, "submitted once, never resent");

        // A caller that stops waiting does not cause a second submission either.
        let task = {
            let api = api.clone();
            tokio::spawn(async move {
                api.post("C0CHAN", None, PostKind::Row, "build-7", "second")
                    .await
            })
        };
        eventually("the second post", || posts(&fake) == 2).await;
        task.abort();
        let _ = task.await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(posts(&fake), 2, "cancellation resent nothing");
        // Still only the two posts' connections plus auth's.
        assert_eq!(fake.connections(), 3);
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_post_carries_its_author_and_every_unconfirmed_answer_is_unknown() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            let body = seen.json().unwrap_or_default();
            match body["channel"].as_str().unwrap_or("") {
                "C0OK" => Reply::json(&json!({"ok": true, "ts": "1700000700.000100"})),
                "C0REJ" => Reply::json(&json!({"ok": false, "error": "channel_not_found"})),
                "C0INT" => Reply::json(&json!({"ok": false, "error": "internal_error"})),
                "C0ODD" => Reply::json(&json!({"ok": false, "error": "Has Spaces https://x"})),
                "C0NOTS" => Reply::json(&json!({"ok": true})),
                "C0BADTS" => Reply::json(&json!({"ok": true, "ts": "17.5"})),
                "C0NOOK" => Reply::json(&json!({"ts": "1700000700.000100"})),
                "C0TEXT" => Reply::ok(b"not json"),
                "C0FIVE" => status(503, &[], &json!({})),
                "C0FOUR" => status(403, &[], &json!({})),
                "C0RATE" => status(429, &[], &json!({})),
                "C0RATE2" => Reply::json(&json!({"ok": false, "error": "ratelimited"})),
                _ => Reply::Drop,
            }
        })
        .await;
        let api = ready(&fake).await;
        let post = |c: &'static str| api.post(c, None, PostKind::Row, "tester Test", "hello");

        assert!(matches!(post("C0OK").await, Ok(PostOutcome::Posted(ts)) if ts.as_str() == "1700000700.000100"));
        let sent = fake.seen().into_iter().find(|s| s.path() == "/api/chat.postMessage").unwrap();
        assert_eq!(sent.method, "POST");
        assert_eq!(sent.authorization.as_deref(), Some(&*format!("Bearer {TOKEN}")));
        let body = sent.json().unwrap();
        assert_eq!(body["text"], "hello", "no text prefix of any form");
        assert_eq!(body["username"], "tester Test (row via Ship of Tools)");
        assert_eq!(
            body["metadata"],
            json!({"event_type": "sot_chat_post", "event_payload": {"kind": "row", "name": "tester Test"}})
        );
        assert!(body.get("thread_ts").is_none());

        assert_eq!(post("C0REJ").await, Ok(PostOutcome::Refused("channel_not_found".into())));
        assert_eq!(post("C0FOUR").await, Ok(PostOutcome::Refused("http_403".into())));
        for unconfirmed in ["C0INT", "C0ODD", "C0NOTS", "C0BADTS", "C0NOOK", "C0TEXT", "C0FIVE", "C0DROP"] {
            assert_eq!(post(unconfirmed).await, Ok(PostOutcome::Unknown), "{unconfirmed}");
        }
        for limited in ["C0RATE", "C0RATE2"] {
            assert!(
                matches!(post(limited).await, Ok(PostOutcome::Limited { assumed: true, .. })),
                "{limited}"
            );
        }
        // Thirteen posts, thirteen requests: none repeated.
        assert_eq!(calls(&fake, "chat.postMessage"), 13);

        // A reply carries its thread.
        let _ = api
            .post("C0OK", Some("1700000100.000000"), PostKind::Row, "build-7", "re")
            .await;
        let last = fake.seen().pop().unwrap().json().unwrap();
        assert_eq!(last["thread_ts"], "1700000100.000000");
        assert_eq!(last["username"], "build-7 (row via Ship of Tools)");

        // Input a post cannot carry is refused before sending.
        let before = fake.seen().len();
        let big = "x".repeat(MAX_TEXT + 1);
        for (thread, name, text) in [
            (None, "", "hi"),
            (None, "bad\nname", "hi"),
            (None, " padded", "hi"),
            (None, "Ada", ""),
            (None, "Ada", big.as_str()),
            (Some("1700000100.12345"), "Ada", "hi"),
        ] {
            let r = api.post("C0OK", thread, PostKind::Row, name, text).await;
            assert!(r.is_err(), "{name:?} {text:.5}");
        }
        assert!(api.post("bad id", None, PostKind::Row, "tester", "hi").await.is_err());
        assert_eq!(fake.seen().len(), before);
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn retry_header_paces_reads_and_preserves_mutation_draft() {
    bounded(async {
        let history_calls = Arc::new(AtomicUsize::new(0));
        let counter = history_calls.clone();
        let fake = FakeSlack::start(Identity::Trusted, move |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            match seen.path().as_str() {
                "/api/conversations.history" => {
                    if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                        // Mixed case: the header name is matched in any case.
                        status(429, &[("rEtRy-AfTeR", "1")], &json!({}))
                    } else {
                        Reply::json(&page(vec![msg("1700000100.000001")], None))
                    }
                }
                "/api/chat.postMessage" => status(429, &[("RETRY-AFTER", "7")], &json!({})),
                _ => Reply::json(&json!({"ok": false, "error": "unexpected"})),
            }
        })
        .await;
        let api = ready(&fake).await;
        let q = query("C0CHAN", Bounds::default(), 5);

        let first = api.history(&q, None).await.unwrap_err();
        assert_eq!(
            first,
            ApiError::Limited { retry_after: Duration::from_secs(1), assumed: false }
        );
        // While the wait runs, a read is refused without a request.
        let second = api.history(&q, None).await.unwrap_err();
        assert!(matches!(second, ApiError::Limited { assumed: false, retry_after } if retry_after <= Duration::from_secs(1)));
        assert!(api.auth_test().await.is_err(), "every read waits, not one method");
        assert_eq!(history_calls.load(Ordering::SeqCst), 1);

        // A mutation is neither gated by the reads' wait nor repeated; the draft is the caller's, untouched.
        let draft = String::from("my draft");
        let outcome = api.post("C0CHAN", None, PostKind::Person, "Ada Test", &draft).await;
        assert_eq!(
            outcome,
            Ok(PostOutcome::Limited { retry_after: Duration::from_secs(7), assumed: false })
        );
        assert_eq!(draft, "my draft");
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(calls(&fake, "chat.postMessage"), 1);

        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(api.history(&q, None).await.unwrap().records.len(), 1);
        assert_eq!(history_calls.load(Ordering::SeqCst), 2);
        fake.shutdown().await;

        // Missing, invalid and hostile headers: an explicit conservative state, or a capped one.
        let long = (MAX_RETRY.as_secs() * 10).to_string();
        let cases: [Case; 7] = [
            (&[], ASSUMED_RETRY, true),
            (&[("Retry-After", "abc")], ASSUMED_RETRY, true),
            (&[("Retry-After", "-3")], ASSUMED_RETRY, true),
            (&[("Retry-After", "")], ASSUMED_RETRY, true),
            (&[("Retry-After", "99999999999999999999999")], ASSUMED_RETRY, true),
            (&[("retry-after", &long)], MAX_RETRY, false),
            (&[("Retry-After", "3")], Duration::from_secs(3), false),
        ];
        for (headers, want, assumed) in cases {
            let headers: Vec<(String, String)> =
                headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            let fake = FakeSlack::start(Identity::Trusted, move |_, _| Reply::Status {
                code: 429,
                headers: headers.clone(),
                body: b"{}".to_vec(),
            })
            .await;
            let got = new_api(&fake).auth_test().await.unwrap_err();
            assert_eq!(got, ApiError::Limited { retry_after: want, assumed }, "{want:?}");
            fake.shutdown().await;
        }

        // ok:false "ratelimited" in a 200 reads the same header.
        let fake = FakeSlack::start(Identity::Trusted, |_, _| {
            status(200, &[("Retry-After", "4")], &json!({"ok": false, "error": "ratelimited"}))
        })
        .await;
        assert_eq!(
            new_api(&fake).auth_test().await.unwrap_err(),
            ApiError::Limited { retry_after: Duration::from_secs(4), assumed: false }
        );
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn names_fall_back_to_ids_without_unbounded_cache() {
    bounded(async {
        let failing = Arc::new(AtomicBool::new(true));
        let flag = failing.clone();
        let fake = FakeSlack::start(Identity::Trusted, move |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            let user = seen.param("user").unwrap_or_default();
            match seen.path().as_str() {
                "/api/users.info" if flag.load(Ordering::SeqCst) && (user == "U0FLAKY" || user == "U0GONE") => {
                    Reply::json(&json!({"ok": false, "error": "user_not_found"}))
                }
                "/api/users.info" => {
                    let (display, real, name) = match user.as_str() {
                        "U0REAL" => ("", "Real Name", "login"),
                        "U0NAME" => ("  ", "", "login"),
                        "U0NONE" => ("", "", ""),
                        _ => ("", "", ""),
                    };
                    let display = if user.starts_with("U0N") || user == "U0REAL" || user == "U0NONE" {
                        display.to_string()
                    } else {
                        format!("Name-{user}")
                    };
                    Reply::json(&json!({"ok": true, "user": {"name": name,
                        "profile": {"display_name": display, "real_name": real}}}))
                }
                "/api/conversations.history" => Reply::json(&page(
                    vec![
                        json!({"ts": "1700000100.000001", "user": "U0GONE", "text": "a"}),
                        json!({"ts": "1700000100.000002", "user": "U0ONE", "text": "b"}),
                        json!({"ts": "1700000100.000003", "user": "U0ONE", "text": "c"}),
                        json!({"ts": "1700000100.000004", "bot_id": "B0OTHER", "username": "other", "text": "d"}),
                    ],
                    None,
                )),
                _ => Reply::json(&json!({"ok": false, "error": "unexpected"})),
            }
        })
        .await;
        let api = ready(&fake).await;

        // A failed lookup shows the id and is not remembered: the next call asks again.
        assert_eq!(api.author_name("U0FLAKY").await, "U0FLAKY");
        assert_eq!(api.author_name("U0FLAKY").await, "U0FLAKY");
        assert_eq!(calls(&fake, "users.info"), 2);
        assert_eq!(api.names_cached(), 0);
        failing.store(false, Ordering::SeqCst);
        let name = api.author_name("U0FLAKY").await;
        assert_eq!(name, "Name-U0FLAKY", "the failure was not a permanent success");
        assert_eq!(api.author_name("U0FLAKY").await, name);
        assert_eq!(calls(&fake, "users.info"), 3, "a success is cached");

        // The order: display name, real name, login name; none gives the id. A bad id is never sent.
        assert_eq!(api.author_name("U0REAL").await, "Real Name");
        assert_eq!(api.author_name("U0NAME").await, "login");
        assert_eq!(api.author_name("U0NONE").await, "U0NONE");
        let before = fake.seen().len();
        assert_eq!(api.author_name("not an id").await, "not an id");
        assert_eq!(fake.seen().len(), before);

        // Excess distinct authors cannot grow the cache: driven directly, as 1,100 handshakes prove nothing more.
        for i in 0..(MAX_NAMES + 76) {
            api.remember(&format!("U1X{i}"), "n");
        }
        assert_eq!(api.names_cached(), MAX_NAMES);
        assert!(!api.is_cached("U1X75"), "the oldest entries left");
        assert!(api.is_cached("U1X76") && api.is_cached(&format!("U1X{}", MAX_NAMES + 75)));
        api.remember("U1X500", "again");
        assert_eq!(api.names_cached(), MAX_NAMES, "a known id evicts nothing");
        assert!(api.is_cached("U1X76") && api.is_cached(&format!("U1X{}", MAX_NAMES + 75)));

        // Records get names, and an author that cannot be looked up shows the id. Bots are left alone.
        failing.store(true, Ordering::SeqCst);
        let q = query("C0CHAN", Bounds::default(), 5);
        let mut records = api.history(&q, None).await.unwrap().records;
        api.fill_names(&mut records).await;
        let names: Vec<&str> = records.iter().map(|r| r.author.name.as_str()).collect();
        assert_eq!(names, ["U0GONE", "Name-U0ONE", "Name-U0ONE", "other"]);
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn every_operation_ends_at_its_deadline() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |_, _| Reply::Hang).await;
        let api = new_api(&fake).with_deadlines(
            Duration::from_millis(300),
            Duration::from_millis(300),
            Duration::from_millis(300),
        );
        let start = Instant::now();
        assert_eq!(
            api.auth_test().await,
            Err(ApiError::Http(HttpError::Timeout))
        );
        assert_eq!(
            api.post("C0CHAN", None, PostKind::Person, "Ada", "hi")
                .await,
            Ok(PostOutcome::Unknown)
        );
        let mut records = vec![member_record("U0HANG")];
        api.fill_names(&mut records).await;
        assert_eq!(records[0].author.name, "U0HANG");
        assert!(start.elapsed() < Duration::from_secs(10));
        fake.shutdown().await;
    })
    .await;
}

/// The names' group deadline is its own: each lookup would wait 20 s, five of them are cut at 300 ms.
#[tokio::test]
async fn name_lookups_together_end_at_the_names_deadline() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |_, _| Reply::Hang).await;
        let api = new_api(&fake).with_deadlines(
            Duration::from_secs(20),
            Duration::from_secs(20),
            Duration::from_millis(300),
        );
        let ids = ["U0H1", "U0H2", "U0H3", "U0H4", "U0H5"];
        let mut records: Vec<_> = ids.iter().map(|id| member_record(id)).collect();
        let start = Instant::now();
        api.fill_names(&mut records).await;
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
        let names: Vec<&str> = records.iter().map(|r| r.author.name.as_str()).collect();
        assert_eq!(names, ids);
        assert!(
            calls(&fake, "users.info") <= 1,
            "the first hung lookup used the whole group's time"
        );
        fake.shutdown().await;
    })
    .await;
}

/// A mention's token for `n` distinct people, `U0M00`....
fn mentions(n: usize) -> String {
    (0..n).map(|i| format!("<@U0M{i:02}>")).collect::<Vec<_>>().join(" ")
}

/// The ids of the records' `names`, in order.
fn named(r: &sot_protocol::chat::ChatRecord) -> Vec<&str> {
    r.names.iter().map(|n| n.id.as_str()).collect()
}

#[tokio::test]
async fn fill_names_resolves_mentions_within_the_deadline_and_the_cache() {
    use sot_protocol::chat::MAX_MENTIONS;
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            let user = seen.param("user").unwrap_or_default();
            match seen.path().as_str() {
                "/api/users.info" if user == "U0BAD" => Reply::json(&json!({"ok": false, "error": "user_not_found"})),
                "/api/users.info" => Reply::json(&json!({"ok": true, "user": {"profile": {"display_name": format!("Name-{user}")}}})),
                "/api/conversations.info" => Reply::json(&json!({"ok": true, "channel": {"name": "chan", "is_private": false}})),
                "/api/conversations.history" => Reply::json(&page(
                    vec![
                        json!({"ts": "1700000100.000001", "user": "U0AUTH", "text": "hi <@U0AAA> and <@U0BAD> and <@U0AAA|label>, <@W0WWW>, <@U0AUTH>"}),
                        json!({"ts": "1700000100.000002", "user": "U0AUTH", "text": mentions(20)}),
                        json!({"ts": "1700000100.000003", "user": "U0AUTH", "text": "a bot's <@B0NOPE> and a channel <#C0CHAN> and an open <@U0OPEN"}),
                    ],
                    None,
                )),
                _ => Reply::json(&json!({"ok": false, "error": "unexpected"})),
            }
        })
        .await;
        let api = ready(&fake).await;
        let q = query("C0CHAN", Bounds::default(), 5);
        let mut records = api.history(&q, None).await.unwrap().records;
        assert!(records.iter().all(|r| r.names.is_empty()), "the page itself carries no names");
        api.fill_names(&mut records).await;
        // A name for each mention that could be looked up, in the text's order, one per id; a failed lookup adds none.
        assert_eq!(named(&records[0]), ["U0AAA", "W0WWW", "U0AUTH"]);
        assert_eq!(records[0].names[0].name, "Name-U0AAA");
        assert_eq!(records[0].author.name, "Name-U0AUTH");
        // At most sixteen per record, and no lookup past them.
        assert_eq!(records[1].names.len(), MAX_MENTIONS);
        assert_eq!(named(&records[1])[15], "U0M15");
        let asked = |id: &str| fake.seen().iter().any(|s| s.param("user").as_deref() == Some(id));
        assert!(asked("U0M15") && !asked("U0M16"), "a lookup past the sixteenth of a record");
        // `users.info` is asked only for a person's token: not a bot, a channel or an open token. A label-less channel token
        // is asked for with `conversations.info` (`a_channel_mention_without_a_label_is_looked_up_once`).
        assert!(!asked("B0NOPE") && !asked("U0OPEN") && !asked("C0CHAN"));
        assert_eq!((named(&records[2]), calls(&fake, "conversations.info")), (vec!["C0CHAN"], 1));

        // The second page asks again only for what failed; the cache answers the rest, and a record cached names alone
        // takes them with no request.
        let before = calls(&fake, "users.info");
        let mut again = api.history(&q, None).await.unwrap().records;
        api.fill_names(&mut again).await;
        assert_eq!(calls(&fake, "users.info"), before + 1, "a cached name was asked for again");
        assert_eq!(again, records);
        let mut cold = api.history(&q, None).await.unwrap().records;
        let before = calls(&fake, "users.info");
        api.cached_names(&mut cold);
        assert_eq!(calls(&fake, "users.info"), before, "a cache read made a request");
        assert_eq!((named(&cold[0]), cold[0].author.name.as_str()), (["U0AAA", "W0WWW", "U0AUTH"].to_vec(), "Name-U0AUTH"));
        fake.shutdown().await;

        // Lookups past the names' deadline add nothing: the record shows its label or id, and the fill ends on time.
        let fake = FakeSlack::start(Identity::Trusted, |_, _| Reply::Hang).await;
        let api = new_api(&fake).with_deadlines(Duration::from_secs(20), Duration::from_secs(20), Duration::from_millis(300));
        let mut records = vec![member_record("U0AUTH"), member_record("U0AUTH")];
        records[1].text = mentions(5);
        let start = Instant::now();
        api.fill_names(&mut records).await;
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        assert!(records[1].names.is_empty() && records[1].author.name == "U0AUTH");
        fake.shutdown().await;
    })
    .await;
}

/// A fake whose `users.info` hangs for a mention (`U0M..`) and names every other user at once.
async fn slow_mentions() -> FakeSlack {
    FakeSlack::start(Identity::Trusted, |seen, _| {
        if let Some(r) = auth(seen) {
            return r;
        }
        let user = seen.param("user").unwrap_or_default();
        if user.starts_with("U0M") {
            return Reply::Hang;
        }
        Reply::json(&json!({"ok": true, "user": {"profile": {"display_name": format!("Name-{user}")}}}))
    })
    .await
}

/// Authors are looked up before mentions, so a mention that hangs the names' deadline cannot cost an author its name.
#[tokio::test]
async fn fill_names_looks_up_authors_before_mentions() {
    bounded(async {
        let fake = slow_mentions().await;
        // One names deadline covers the whole lookup group by design, so the author's own (answered) lookup must finish
        // inside it: a lookup takes about 44 ms here, and 1 s leaves 20 times that.
        let api = new_api(&fake).with_deadlines(Duration::from_secs(20), Duration::from_secs(20), Duration::from_millis(1000));
        let mut records = vec![member_record("U0AUTH")];
        records[0].text = mentions(5);
        api.fill_names(&mut records).await;
        assert_eq!(records[0].author.name, "Name-U0AUTH");
        assert!(records[0].names.is_empty());
        fake.shutdown().await;
    })
    .await;
}

/// A fill sets names from what its own lookups found, not from the cache, which holds `MAX_NAMES` entries and drops its
/// oldest: when other lookups push the fill's own first names out before it ends, it keeps them.
#[tokio::test]
async fn fill_names_keeps_what_it_found_when_the_cache_cannot() {
    bounded(async {
        let slot: Arc<Mutex<Option<Arc<Api>>>> = Arc::new(Mutex::new(None));
        let held = slot.clone();
        let fake = FakeSlack::start(Identity::Trusted, move |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            let user = seen.param("user").unwrap_or_default();
            if user == "U0LAST" {
                // While the last lookup is being answered, other reads fill the cache and push out the fill's first names.
                let api = held.lock().unwrap().clone().unwrap();
                for i in 0..MAX_NAMES + 10 {
                    api.remember(&format!("U1X{i}"), "n");
                }
            }
            Reply::json(&json!({"ok": true, "user": {"profile": {"display_name": format!("Name-{user}")}}}))
        })
        .await;
        let api = Arc::new(new_api(&fake));
        *slot.lock().unwrap() = Some(api.clone());
        let mut records = vec![member_record("U0FIRST")];
        records[0].text = "<@U0ONE> <@U0LAST>".into();
        api.fill_names(&mut records).await;
        assert!(!api.is_cached("U0FIRST") && !api.is_cached("U0ONE"), "the cache did not push them out");
        assert_eq!(records[0].author.name, "Name-U0FIRST");
        assert_eq!(named(&records[0]), ["U0ONE", "U0LAST"]);
        fake.shutdown().await;
    })
    .await;
}

/// A fill ends its channel lookups at the first answer no other channel escapes (a missing scope or a rate limit), so a
/// scope Slack has not granted costs one request a fill and a wait is never earned by channel names; a channel Slack
/// does not know is only its own failure, and the rest are still asked.
#[tokio::test]
async fn a_fill_stops_its_channel_lookups_at_a_scope_error_or_a_rate_limit() {
    bounded(async {
        // The answer to `conversations.info`, by case, and the requests a fill makes with it: all the channels the record's sixteen names leave room for, fifteen after its one person.
        for (case, asked) in [("missing_scope", 1), ("ratelimited", 1), ("channel_not_found", 15)] {
            let fake = FakeSlack::start(Identity::Trusted, move |seen, _| {
                if seen.path() == "/api/conversations.info" {
                    let body = json!({"ok": false, "error": case});
                    return if case == "ratelimited" { status(429, &[("Retry-After", "30")], &body) } else { Reply::json(&body) };
                }
                let user = seen.param("user").unwrap_or_default();
                Reply::json(&json!({"ok": true, "user": {"profile": {"display_name": format!("Name-{user}")}}}))
            })
            .await;
            let api = new_api(&fake);
            let mut records = vec![member_record("U0AUTH")];
            records[0].text = format!("<@U0ONE> {}", (0..16).map(|i| format!("<#C0K{i:02}>")).collect::<String>());
            api.fill_names(&mut records).await;
            assert_eq!(calls(&fake, "conversations.info"), asked, "{case}");
            // The people and the author were looked up first, and keep their names.
            assert_eq!((records[0].author.name.as_str(), named(&records[0])), ("Name-U0AUTH", vec!["U0ONE"]));
            fake.shutdown().await;
        }
    })
    .await;
}

/// A record's sixteen names go to its people before its channels, whatever the text's order.
#[tokio::test]
async fn people_come_before_channels_under_the_cap_of_names() {
    use sot_protocol::chat::MAX_MENTIONS;
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            let (user, channel) = (seen.param("user").unwrap_or_default(), seen.param("channel").unwrap_or_default());
            match seen.path().as_str() {
                "/api/conversations.info" => Reply::json(&json!({"ok": true, "channel": {"name": format!("n-{channel}"), "is_private": false}})),
                _ => Reply::json(&json!({"ok": true, "user": {"profile": {"display_name": format!("Name-{user}")}}})),
            }
        })
        .await;
        let api = new_api(&fake);
        let mut records = vec![member_record("U0AUTH")];
        records[0].text = format!("<#C0FIRST> {} <#C0LAST>", mentions(MAX_MENTIONS));
        api.fill_names(&mut records).await;
        assert!(named(&records[0]).iter().all(|id| id.starts_with("U0M")) && records[0].names.len() == MAX_MENTIONS);
        assert_eq!(calls(&fake, "conversations.info"), 0, "a channel past the sixteen was asked for");
        records[0].text = format!("<#C0FIRST> {} <#C0LAST>", mentions(MAX_MENTIONS - 1));
        api.fill_names(&mut records).await;
        assert_eq!(named(&records[0]).last(), Some(&"C0FIRST"), "{:?}", named(&records[0]));
        assert_eq!(records[0].names.len(), MAX_MENTIONS);
        fake.shutdown().await;
    })
    .await;
}

/// `cached_name` answers from the cache alone: a channel is in it once `author_name` found it, a private one never.
#[tokio::test]
async fn a_cached_channel_name_is_read_with_no_request() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            let private = seen.param("channel").as_deref() == Some("C0PRIV");
            Reply::json(&json!({"ok": true, "channel": {"name": "general", "is_private": private}}))
        })
        .await;
        let api = new_api(&fake);
        assert_eq!(api.cached_name("C0PUB"), None);
        assert_eq!(calls(&fake, "conversations.info"), 0, "a cache read asked Slack");
        api.author_name("C0PUB").await;
        api.author_name("C0PRIV").await;
        let before = calls(&fake, "conversations.info");
        assert_eq!((api.cached_name("C0PUB").as_deref(), api.cached_name("C0PRIV")), (Some("general"), None));
        assert_eq!(calls(&fake, "conversations.info"), before);
        fake.shutdown().await;
    })
    .await;
}

/// The start's lookup of the configured channels' names: distinct channel ids only, at most `MAX_CHANNELS`, each asked
/// once and cached, a mapped private channel's included; a failure is returned with its fixed code.
#[tokio::test]
async fn the_configured_channels_names_are_looked_up_once() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            let channel = seen.param("channel").unwrap_or_default();
            Reply::json(&json!({"ok": true, "channel": {"name": format!("n-{channel}"), "is_private": channel == "C0PRIV"}}))
        })
        .await;
        let api = new_api(&fake);
        // A channel mapped twice is asked for, and returned, once; a failure is not cached, so a duplicate of one would repeat.
        let mut ids: Vec<String> = ["C0AAA", "C0AAA", "U0USER", "D0DM", "C0PRIV", "G0GRP", "C0PRIV"].map(String::from).to_vec();
        assert!(api.channel_names(&ids).await.is_empty(), "a user or a DM is not asked for");
        assert_eq!((api.cached_name("C0AAA").as_deref(), api.cached_name("G0GRP").as_deref()), (Some("n-C0AAA"), Some("n-G0GRP")));
        assert_eq!(api.cached_name("C0PRIV").as_deref(), Some("n-C0PRIV"));
        assert_eq!(calls(&fake, "conversations.info"), 3, "each distinct channel once");
        api.channel_names(&ids).await;
        assert_eq!(calls(&fake, "conversations.info"), 3, "a cached name is not asked for again");

        // At most `MAX_CHANNELS` distinct channels, in the order given.
        ids = (0..40).map(|i| format!("C0K{i:02}")).collect();
        let before = calls(&fake, "conversations.info");
        assert!(api.channel_names(&ids).await.is_empty());
        assert_eq!(calls(&fake, "conversations.info"), before + crate::watch::MAX_CHANNELS);
        assert!(api.cached_name("C0K31").is_some() && api.cached_name("C0K32").is_none());
        fake.shutdown().await;
    })
    .await;
}

/// A mapped channel is named public or private, and a failure of one is returned with its code and not remembered; a
/// mention of a channel that is not mapped keeps the public-only rule, and once a private channel is mapped and named its
/// name is in the cache, so a mention of it shows the name too.
#[tokio::test]
async fn a_mapped_private_channel_is_named_and_an_unmapped_one_is_not() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            match seen.param("channel").unwrap_or_default().as_str() {
                "C0PRIV" => Reply::json(&json!({"ok": true, "channel": {"name": "secret", "is_private": true}})),
                "C0OTHER" => Reply::json(&json!({"ok": true, "channel": {"name": "hidden", "is_private": true}})),
                "C0NOFLAG" => Reply::json(&json!({"ok": true, "channel": {"name": "unsure"}})),
                "C0EMPTY" => Reply::json(&json!({"ok": true, "channel": {"name": " \u{7} ", "is_private": true}})),
                "C0GONE" => Reply::json(&json!({"ok": false, "error": "channel_not_found"})),
                _ => Reply::json(&json!({"ok": true, "channel": {"name": "open", "is_private": false}})),
            }
        })
        .await;
        let api = new_api(&fake);
        // A mention reads before the mapped lookup: the private channel gets no name, and is asked for again by the lookup.
        let mut records = vec![member_record("U0AUTH")];
        records[0].text = "<#C0PRIV> <#C0OTHER> <#C0NOFLAG>".into();
        api.fill_names(&mut records).await;
        assert!(records[0].names.is_empty(), "{:?}", records[0].names);
        assert!(api.cached_name("C0PRIV").is_none() && api.cached_name("C0OTHER").is_none());
        let ids: Vec<String> = ["C0PRIV", "C0PUB", "C0NOFLAG", "C0EMPTY", "C0GONE"].map(String::from).to_vec();
        let failed = api.channel_names(&ids).await;
        assert_eq!(failed, [("C0EMPTY".to_string(), "malformed"), ("C0GONE".to_string(), "slack")], "a name nothing is left of is no name");
        assert_eq!(
            (api.cached_name("C0PRIV").as_deref(), api.cached_name("C0PUB").as_deref(), api.cached_name("C0NOFLAG").as_deref()),
            (Some("secret"), Some("open"), Some("unsure")),
            "a mapped channel is named whether Slack says it is private, public or nothing"
        );
        assert!(api.cached_name("C0OTHER").is_none() && api.cached_name("C0EMPTY").is_none(), "only the mapped channels were named");
        // The unmapped private channel still gets none; the mapped one's name, now cached, shows on a mention of it.
        api.fill_names(&mut records).await;
        assert_eq!(named(&records[0]), ["C0PRIV", "C0NOFLAG"]);
        assert_eq!(records[0].names[0].name, "secret");
        fake.shutdown().await;
    })
    .await;
}

/// The lookup ends at the first answer no other channel escapes (the scope is missing, or Slack is limiting reads), and a
/// stalled one ends at the names' deadline, the channels it did not reach returned as `timeout`.
#[tokio::test]
async fn the_lookup_stops_at_a_scope_error_and_at_the_deadline() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            auth(seen).unwrap_or_else(|| Reply::json(&json!({"ok": false, "error": "missing_scope"})))
        })
        .await;
        let api = new_api(&fake);
        let ids: Vec<String> = ["C0AAA", "C0BBB", "C0CCC"].map(String::from).to_vec();
        assert_eq!(api.channel_names(&ids).await, [("C0AAA".to_string(), "slack")]);
        assert_eq!(calls(&fake, "conversations.info"), 1, "the doomed rest was not asked for");
        fake.shutdown().await;

        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            auth(seen).unwrap_or_else(|| status(429, &[("Retry-After", "30")], &json!({})))
        })
        .await;
        let api = new_api(&fake);
        assert_eq!(api.channel_names(&ids).await, [("C0AAA".to_string(), "limited")]);
        assert_eq!(calls(&fake, "conversations.info"), 1, "a rate limit ended the lookup");
        fake.shutdown().await;

        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            auth(seen).unwrap_or(Reply::Hang)
        })
        .await;
        let api = new_api(&fake).with_deadlines(Duration::from_secs(20), Duration::from_secs(20), Duration::from_millis(300));
        let start = Instant::now();
        let failed = api.channel_names(&ids).await;
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        assert_eq!(failed.iter().map(|(id, code)| (id.as_str(), *code)).collect::<Vec<_>>(), [("C0AAA", "timeout"), ("C0BBB", "timeout"), ("C0CCC", "timeout")]);
        fake.shutdown().await;
    })
    .await;
}

/// A label-less `<#C…>` or `<#G…>` token is looked up once with `conversations.info`, through the cache and within the
/// names' deadline, after the authors and the people; a labeled one, a direct-message id and a failure are not, and a
/// record's names stay within `MAX_MENTIONS` whatever their kind.
#[tokio::test]
async fn a_channel_mention_without_a_label_is_looked_up_once() {
    use sot_protocol::chat::MAX_MENTIONS;
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            let user = seen.param("user").unwrap_or_default();
            let channel = seen.param("channel").unwrap_or_default();
            match seen.path().as_str() {
                "/api/users.info" => Reply::json(&json!({"ok": true, "user": {"profile": {"display_name": format!("Name-{user}")}}})),
                "/api/conversations.info" if channel == "C0BAD" => Reply::json(&json!({"ok": false, "error": "channel_not_found"})),
                "/api/conversations.info" if channel == "C0CTRL" => Reply::json(&json!({"ok": true, "channel": {"name": " la\u{7}b ", "is_private": false}})),
                // A name nothing is left of after cleaning is no name.
                "/api/conversations.info" if channel == "C0EMPTY" => Reply::json(&json!({"ok": true, "channel": {"name": " \u{7} ", "is_private": false}})),
                // A private channel's name is not added, nor one whose reply does not say it is public.
                "/api/conversations.info" if channel == "C0PRIV" => Reply::json(&json!({"ok": true, "channel": {"name": "secret", "is_private": true}})),
                "/api/conversations.info" if channel == "C0NOFLAG" => Reply::json(&json!({"ok": true, "channel": {"name": "unsure"}})),
                "/api/conversations.info" => Reply::json(&json!({"ok": true, "channel": {"name": format!("name-{channel}"), "is_private": false}})),
                _ => Reply::json(&json!({"ok": false, "error": "unexpected"})),
            }
        })
        .await;
        let api = new_api(&fake);
        let asked = |param: &str, id: &str| fake.seen().iter().any(|s| s.param(param).as_deref() == Some(id));
        let mut records = vec![member_record("U0AUTH"), member_record("U0AUTH")];
        records[0].text = "<#C0AAA> <#G0GRP|> <#C0LAB|label> <#C0BAD> <#D0DM> <#C0CTRL> <#C0AAA> <@C0NOT> <#U0USER> <#C0EMPTY> <#C0PRIV> <#C0NOFLAG>".into();
        // Ten people and ten channels: the first sixteen tokens in the text's order get an entry, and no lookup past them.
        records[1].text = format!("{} {}", mentions(10), (0..10).map(|i| format!("<#C0K{i:02}>")).collect::<String>());
        api.fill_names(&mut records).await;
        assert_eq!(named(&records[0]), ["C0AAA", "G0GRP", "C0CTRL"]);
        assert_eq!(records[0].names[0].name, "name-C0AAA");
        assert_eq!(records[0].names[2].name, "lab", "a name is cleaned");
        assert!(!asked("channel", "C0LAB") && !asked("channel", "D0DM") && !asked("channel", "C0NOT") && !asked("channel", "U0USER"));
        assert_eq!(records[1].names.len(), MAX_MENTIONS);
        assert_eq!((named(&records[1])[9], named(&records[1])[15]), ("U0M09", "C0K05"));
        assert!(asked("channel", "C0K05") && !asked("channel", "C0K06"), "a lookup past the sixteenth of a record");
        assert_eq!(calls(&fake, "conversations.info"), 7 + 6, "a channel was asked for twice or not at all");
        assert!(asked("channel", "C0PRIV") && asked("channel", "C0EMPTY"), "a channel that gives no name was not asked for");
        // The second fill asks again only for what failed; a cache read asks for nothing.
        let before = calls(&fake, "conversations.info");
        api.fill_names(&mut records).await;
        assert_eq!(calls(&fake, "conversations.info"), before + 4, "a cached channel was asked for again, or a failed one not");
        let mut cold = vec![member_record("U0AUTH")];
        cold[0].text = records[0].text.clone();
        let (before, users) = (calls(&fake, "conversations.info"), calls(&fake, "users.info"));
        api.cached_names(&mut cold);
        assert_eq!((calls(&fake, "conversations.info"), calls(&fake, "users.info")), (before, users));
        assert_eq!(named(&cold[0]), ["C0AAA", "G0GRP", "C0CTRL"]);
        fake.shutdown().await;

        // A channel lookup that hangs costs the deadline, after the author and the people have their names.
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            if seen.path() == "/api/conversations.info" {
                return Reply::Hang;
            }
            let user = seen.param("user").unwrap_or_default();
            Reply::json(&json!({"ok": true, "user": {"profile": {"display_name": format!("Name-{user}")}}}))
        })
        .await;
        // As above: the group's one deadline covers the answered author and person lookups (two round trips) and the
        // hung channel lookups.
        let api = new_api(&fake).with_deadlines(Duration::from_secs(20), Duration::from_secs(20), Duration::from_millis(1000));
        let mut records = vec![member_record("U0AUTH")];
        records[0].text = "<#C0AAA> <@U0ONE> <#C0BBB>".into();
        let start = Instant::now();
        api.fill_names(&mut records).await;
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        assert_eq!((records[0].author.name.as_str(), named(&records[0])), ("Name-U0AUTH", vec!["U0ONE"]));
        fake.shutdown().await;
    })
    .await;
}

fn member_record(id: &str) -> sot_protocol::chat::ChatRecord {
    let ctx = crate::normalize::Context {
        workspace: WS,
        own_bot_id: None,
        own_user_id: None,
        limited: false,
    };
    match crate::normalize::normalize(
        &ctx,
        "C0CHAN",
        &json!({"ts": "1.000001", "user": id, "text": "x"}),
    ) {
        crate::normalize::Normalized::New(r) => r,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn slack_text_and_identity_failures_reach_no_caller_raw() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| match seen.param("mode").as_deref() {
            Some("echo") => Reply::json(&json!({
                "ok": false,
                "error": format!("invalid auth https://files.slack.com/private/xoxb-sentinel {TOKEN}")
            })),
            Some("html") => Reply::ok(format!("<html>{TOKEN}</html>").as_bytes()),
            Some("server") => status(500, &[], &json!({"error": TOKEN})),
            Some("team") => Reply::json(&json!({"ok": true, "team_id": "T0OTHER", "user_id": "U1"})),
            Some("bare") => Reply::json(&json!({"ok": true, "team_id": WS})),
            _ => Reply::Drop,
        })
        .await;
        // The mode rides in the base so that `auth.test` carries it.
        let with_mode = |mode: &str| {
            Api::new(fake.http(), Secret::new(TOKEN), WS)
                .unwrap()
                .with_base(fake.url("slack.com", &format!("/api/?mode={mode}&m=")))
        };
        let shown = |e: &ApiError| format!("{e} {e:?} {}", e.code());
        let e = with_mode("echo").auth_test().await.unwrap_err();
        assert_eq!(e, ApiError::Slack("unknown_error".into()));
        assert_eq!(with_mode("html").auth_test().await.unwrap_err(), ApiError::Malformed);
        assert_eq!(with_mode("server").auth_test().await.unwrap_err(), ApiError::Status(500));
        assert_eq!(with_mode("team").auth_test().await.unwrap_err(), ApiError::WrongWorkspace);
        assert_eq!(with_mode("bare").auth_test().await.unwrap_err(), ApiError::Malformed);
        let dropped = with_mode("drop").auth_test().await.unwrap_err();
        assert!(matches!(dropped, ApiError::Http(_)));
        for e in [e, dropped] {
            let text = shown(&e);
            for secret in [TOKEN, "xoxb", "private", "files.slack.com", "127.0.0.1"] {
                assert!(!text.contains(secret), "{text}");
            }
        }
        // History before the identity is known would misjudge own posts: it is refused unsent.
        let fresh = with_mode("none");
        let before = fake.seen().len();
        let q = query("C0CHAN", Bounds::default(), 5);
        assert_eq!(fresh.history(&q, None).await.unwrap_err(), ApiError::NoIdentity);
        assert_eq!(fake.seen().len(), before);
        fake.shutdown().await;
    })
    .await;
}

/// The setup's `auth.test`: any workspace is answered, the read rules hold, and nothing shown carries the token.
#[tokio::test]
async fn identify_answers_the_tokens_workspace_and_no_error_carries_a_token() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| match seen.path().as_str() {
            "/api/ok/auth.test" => Reply::json(
                &json!({"ok": true, "team_id": "T0OTHER", "user_id": OWN_USER, "bot_id": OWN_BOT}),
            ),
            "/api/nobot/auth.test" => {
                Reply::json(&json!({"ok": true, "team_id": WS, "user_id": OWN_USER, "bot_id": "not an id"}))
            }
            "/api/noteam/auth.test" => Reply::json(&json!({"ok": true, "user_id": OWN_USER})),
            "/api/badteam/auth.test" => {
                Reply::json(&json!({"ok": true, "team_id": "T0 BAD", "user_id": OWN_USER}))
            }
            "/api/nouser/auth.test" => Reply::json(&json!({"ok": true, "team_id": WS})),
            "/api/refused/auth.test" => Reply::json(&json!({"ok": false, "error": "invalid_auth"})),
            "/api/echo/auth.test" => Reply::json(&json!({
                "ok": false,
                "error": format!("invalid auth https://files.slack.com/private/xoxb-sentinel {TOKEN}")
            })),
            "/api/html/auth.test" => Reply::ok(format!("<html>{TOKEN}</html>").as_bytes()),
            "/api/server/auth.test" => status(500, &[], &json!({"error": TOKEN})),
            "/api/limited/auth.test" => status(429, &[("Retry-After", "7")], &json!({"ok": false})),
            _ => Reply::Drop,
        })
        .await;
        let http = fake.http();
        let identify = |mode: &str| {
            identify_at(&http, Secret::new(TOKEN), fake.url("slack.com", &format!("/api/{mode}/")))
        };
        assert_eq!(
            identify("ok").await.unwrap(),
            Identified {
                workspace: "T0OTHER".into(),
                own: crate::api::Identity {
                    user_id: OWN_USER.into(),
                    bot_id: Some(OWN_BOT.into()),
                    app_id: None,
                },
                name: None,
            }
        );
        let ok = fake.seen();
        assert_eq!(ok.len(), 1, "one request");
        assert_eq!(ok[0].authorization.as_deref(), Some(&*format!("Bearer {TOKEN}")));
        assert_eq!(ok[0].target, "/api/ok/auth.test", "no workspace is sent");
        assert_eq!(identify("nobot").await.unwrap().own.bot_id, None);
        let mut errors = Vec::new();
        for (mode, want) in [
            ("noteam", ApiError::Malformed),
            ("badteam", ApiError::Malformed),
            ("nouser", ApiError::Malformed),
            ("html", ApiError::Malformed),
            ("refused", ApiError::Slack("invalid_auth".into())),
            ("echo", ApiError::Slack("unknown_error".into())),
            ("server", ApiError::Status(500)),
            ("limited", ApiError::Limited { retry_after: Duration::from_secs(7), assumed: false }),
        ] {
            let got = identify(mode).await.unwrap_err();
            assert_eq!(got, want, "{mode}");
            errors.push(got);
        }
        let dropped = identify("drop").await.unwrap_err();
        assert!(matches!(dropped, ApiError::Http(_)), "drop");
        errors.push(dropped);
        for e in &errors {
            let text = format!("{e} {e:?} {}", e.code());
            for secret in [TOKEN, "xoxb", "private", "files.slack.com", "127.0.0.1"] {
                assert!(!text.contains(secret), "an error of identify shows a secret or a URL ({})", e.code());
            }
        }
        // Each call is its own: a rate limit answered to one does not refuse the next.
        assert!(identify("ok").await.is_ok());
        fake.shutdown().await;
    })
    .await;
}

/// A post made by this client, read back through history in the fake, normalizes to the origin it was posted as.
#[tokio::test]
async fn a_post_read_back_through_history_keeps_its_origin() {
    bounded(async {
        let stored: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let store = stored.clone();
        let fake = FakeSlack::start(Identity::Trusted, move |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            match seen.path().as_str() {
                "/api/chat.postMessage" => {
                    let body = seen.json().unwrap();
                    let mut store = store.lock().unwrap();
                    let ts = format!("1700000{:03}.000100", store.len() + 1);
                    // As Slack stores it: a person's token makes the person's own message (their user and the app's
                    // `app_id`, no bot id), the bot's the app's, with the custom username and the metadata.
                    if seen.authorization.as_deref() == Some(&*format!("Bearer {PERSON}")) {
                        store.push(json!({"ts": ts, "user": "U0PERSON", "app_id": "A0APP", "text": body["text"]}));
                    } else {
                        store.push(json!({
                            "ts": ts, "bot_id": OWN_BOT, "user": OWN_USER, "text": body["text"],
                            "username": body["username"], "metadata": body["metadata"],
                        }));
                    }
                    Reply::json(&json!({"ok": true, "ts": ts}))
                }
                "/api/conversations.history" => {
                    let mut all = store.lock().unwrap().clone();
                    all.reverse();
                    Reply::json(&page(all, None))
                }
                _ => Reply::json(&json!({"ok": false, "error": "unexpected"})),
            }
        })
        .await;
        let api = ready(&fake).await;
        let a = api
            .post(
                "C0CHAN",
                None,
                PostKind::Person,
                "Ada Test",
                "from a person",
            )
            .await;
        let b = api
            .post("C0CHAN", None, PostKind::Row, "build-7", "from a row")
            .await;
        let posted: Vec<String> = [a, b]
            .into_iter()
            .map(|o| match o {
                Ok(PostOutcome::Posted(ts)) => ts.as_str().to_string(),
                other => panic!("{other:?}"),
            })
            .collect();
        let page = api
            .history(&query("C0CHAN", Bounds::default(), 5), None)
            .await
            .unwrap();
        let by_ts = |ts: &str| page.records.iter().find(|r| r.ts.as_str() == ts).unwrap();
        let person = by_ts(&posted[0]);
        assert_eq!((person.origin, person.author.id.as_str()), (Origin::Member, "U0PERSON"), "an ordinary message of theirs");
        assert_eq!(person.text, "from a person");
        let row = by_ts(&posted[1]);
        assert_eq!(
            (row.origin, row.author.name.as_str()),
            (Origin::Row, "build-7")
        );
        // The person's post carried their token and every other request the bot's.
        let (person_post, bot) = (format!("Bearer {PERSON}"), format!("Bearer {TOKEN}"));
        let posts: Vec<Seen> = fake.seen().into_iter().filter(|s| s.path() == "/api/chat.postMessage").collect();
        assert_eq!(posts[0].authorization.as_deref(), Some(&*person_post));
        let by = |a: &str| fake.seen().iter().filter(|s| s.authorization.as_deref() == Some(a)).count();
        assert_eq!((by(&person_post), by(&bot)), (1, fake.seen().len() - 1), "every request had one of the two");
        fake.shutdown().await;
    })
    .await;
}

/// The one door escapes every text: the fake receives Slack's text, a post reads back as escaped markup that
/// `text_segments` decodes, and the bound is on the escaped form (8 KiB exactly is sent, one more `&` is refused unsent).
#[tokio::test]
async fn a_message_text_is_sent_escaped_and_bounded_escaped() {
    bounded(async {
        let stored: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let store = stored.clone();
        let fake = FakeSlack::start(Identity::Trusted, move |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            match seen.path().as_str() {
                "/api/chat.postMessage" => {
                    let body = seen.json().unwrap();
                    let mut store = store.lock().unwrap();
                    let ts = format!("1700000{:03}.000100", store.len() + 1);
                    store.push(json!({
                        "ts": ts, "bot_id": OWN_BOT, "user": OWN_USER, "text": body["text"],
                        "username": body["username"], "metadata": body["metadata"],
                    }));
                    Reply::json(&json!({"ok": true, "ts": ts}))
                }
                "/api/conversations.history" => {
                    let mut all = store.lock().unwrap().clone();
                    all.reverse();
                    Reply::json(&page(all, None))
                }
                _ => Reply::json(&json!({"ok": false, "error": "unexpected"})),
            }
        })
        .await;
        let api = ready(&fake).await;
        let post = |text: String| {
            let api = &api;
            async move { api.post("C0CHAN", None, PostKind::Person, "Ada Test", &text).await }
        };
        let sent = |fake: &FakeSlack| -> Vec<Value> {
            fake.seen().iter().filter(|s| s.path().as_str() == "/api/chat.postMessage").map(|s| s.json().unwrap()).collect()
        };

        // The fake receives Slack's text.
        assert!(matches!(post("if a < b && c > d".into()).await, Ok(PostOutcome::Posted(_))));
        assert_eq!(sent(&fake)[0]["text"], "if a &lt; b &amp;&amp; c &gt; d");
        // The body carries no key that changes how Slack reads the text (`link_names`, `parse`, ...).
        let first = sent(&fake).remove(0);
        let mut keys: Vec<&str> = first.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["channel", "text"]);

        // A mention token typed by a person is text, not a ping.
        assert!(matches!(post("<!channel> <@U0BOT>".into()).await, Ok(PostOutcome::Posted(_))));
        assert_eq!(sent(&fake)[1]["text"], "&lt;!channel&gt; &lt;@U0BOT&gt;");

        // Read back through history: escaped markup, and `text_segments` gives the typed text back.
        let ts = match post("a < b & c".into()).await {
            Ok(PostOutcome::Posted(ts)) => ts.as_str().to_string(),
            other => panic!("{other:?}"),
        };
        let page = api.history(&query("C0CHAN", Bounds::default(), 5), None).await.unwrap();
        let rec = page.records.iter().find(|r| r.ts.as_str() == ts).unwrap();
        assert_eq!(rec.text, "a &lt; b &amp; c");
        let back: String = sot_protocol::chat::text_segments(&rec.text)
            .iter()
            .map(|s| match s {
                sot_protocol::chat::TextSegment::Text(t) => t.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(back, "a < b & c");

        // The bound is on the escaped form: exactly 8 KiB is sent, one more `&` is refused and nothing is sent.
        let exact = format!("{}aa", "&".repeat(1638));
        assert_eq!(sot_protocol::chat::escape_text(&exact).len(), MAX_TEXT);
        assert!(matches!(post(exact).await, Ok(PostOutcome::Posted(_))));
        let before = fake.seen().len();
        let over = format!("{}aa", "&".repeat(1639));
        assert_eq!(post(over).await.unwrap_err(), ApiError::BadRequest);
        assert_eq!(fake.seen().len(), before);
        fake.shutdown().await;
    })
    .await;
}

const SCAN_AFTER: &str = "1699999000.000000";
const SCAN_BEFORE: &str = "1800000000.000000";
const PARENT: &str = "1700000100.000000";

fn scan_query(channel: &str, thread: Option<&str>, limit: u16) -> ScanQuery {
    let bounds = Bounds::new(Some(SCAN_AFTER), Some(SCAN_BEFORE), None).unwrap();
    ScanQuery::new(channel, thread, bounds, limit).unwrap()
}

/// A scan page as Slack answers it: the raw messages, `has_more`, and `is_limited` when given.
fn scan_reply(messages: Vec<Value>, more: bool, limited: Option<bool>) -> Reply {
    let mut v = json!({"ok": true, "messages": messages, "has_more": more});
    if let Some(l) = limited {
        v["is_limited"] = json!(l);
    }
    Reply::json(&v)
}

/// An answer a test expects to be a page: a failure is an assertion that prints the error.
fn ok<T: std::fmt::Debug>(got: Result<T, ApiError>) -> T {
    assert!(matches!(got, Ok(_)), "{got:?}");
    got.unwrap()
}

fn ts(s: &str) -> sot_protocol::chat::Ts {
    sot_protocol::chat::Ts::parse(s).unwrap()
}

fn reply_msg(ts: &str) -> Value {
    let mut m = msg(ts);
    m["thread_ts"] = json!(PARENT);
    m
}

#[tokio::test]
async fn a_scan_reads_999_and_its_bound_is_its_own() {
    bounded(async {
        let big = |text: usize| {
            (0..999)
                .map(|i| {
                    let mut m = msg(&format!("{}.000001", 1_700_000_000 + i));
                    m["text"] = json!("x".repeat(text));
                    m
                })
                .collect::<Vec<_>>()
        };
        let fake = FakeSlack::start(Identity::Trusted, move |seen, _| {
            auth(seen).unwrap_or_else(|| match seen.param("channel").as_deref() {
                Some("C0SMALL") => scan_reply(big(10), false, None),
                _ => scan_reply(big(2100), false, None),
            })
        })
        .await;
        let api = ready(&fake).await;

        let got = ok(api.scan(&scan_query("C0SMALL", None, 999)).await);
        assert_eq!(got.records.len(), 999);
        let asked = fake.seen().pop().unwrap();
        assert_eq!(asked.path(), "/api/conversations.history");
        for (k, v) in [
            ("channel", "C0SMALL"),
            ("limit", "999"),
            ("include_all_metadata", "true"),
            ("oldest", SCAN_AFTER),
            ("latest", SCAN_BEFORE),
            ("inclusive", "false"),
        ] {
            assert_eq!(asked.param(k).as_deref(), Some(v), "{k}");
        }
        assert_eq!(asked.param("cursor"), None, "a scan never sends a cursor");

        // A body of over 2 MiB passes a scan and fails the display page's 1 MiB bound.
        let got = ok(api.scan(&scan_query("C0BULKY", None, 999)).await);
        assert_eq!(got.records.len(), 999);
        let plain = query("C0BULKY", Bounds::default(), 20);
        assert_eq!(
            api.history(&plain, None).await,
            Err(ApiError::Http(HttpError::TooLarge))
        );
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_scan_page_names_its_raw_extremes_and_every_parents_latest_reply() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            let join = |ts: &str| json!({"ts": ts, "subtype": "channel_join", "user": "U0MEM"});
            match seen.param("channel").unwrap().as_str() {
                // Joins only: no record, and still the page's extremes (newest first).
                "C0JOINS" => scan_reply(
                    vec![join("1700000300.000000"), join("1700000200.000000")],
                    true,
                    None,
                ),
                // A deleted parent, and a parent whose last reply came while the read was in flight.
                "C0TOMB" => {
                    let tomb = json!({"ts": "1700000300.000000", "subtype": "tombstone",
                        "thread_ts": "1700000300.000000", "latest_reply": "1700000900.000100"});
                    let mut live = msg("1700000200.000000");
                    live["latest_reply"] = json!("1800000000.000270");
                    scan_reply(vec![tomb, live, msg("1700000100.000000")], false, None)
                }
                "C0HIST" => scan_reply(
                    vec![
                        msg("1700000400.000000"),
                        msg("1700000300.000000"),
                        msg("1700000200.000000"),
                    ],
                    false,
                    None,
                ),
                // A replies page, oldest first, the parent first and older than every reply.
                "C0REPL" => {
                    let mut parent = msg(PARENT);
                    parent["thread_ts"] = json!(PARENT);
                    parent["latest_reply"] = json!("1700000400.000000");
                    scan_reply(
                        vec![
                            parent,
                            reply_msg("1700000200.000000"),
                            reply_msg("1700000300.000000"),
                            reply_msg("1700000400.000000"),
                        ],
                        false,
                        None,
                    )
                }
                "C0NOTS" => scan_reply(vec![json!({"subtype": "channel_join"})], false, None),
                _ => {
                    let mut m = msg("1700000200.000000");
                    m["latest_reply"] = json!("soon");
                    scan_reply(vec![m], false, None)
                }
            }
        })
        .await;
        let api = ready(&fake).await;

        let got = ok(api.scan(&scan_query("C0JOINS", None, 10)).await);
        assert!(got.records.is_empty() && got.threads.is_empty() && got.has_more);
        assert_eq!(got.oldest, Some(ts("1700000200.000000")));
        assert_eq!(got.newest, Some(ts("1700000300.000000")));

        let got = ok(api.scan(&scan_query("C0TOMB", None, 10)).await);
        assert_eq!(tss_of(&got), ["1700000200.000000", "1700000100.000000"]);
        assert_eq!(
            got.threads,
            [
                (ts("1700000300.000000"), ts("1700000900.000100")),
                (ts("1700000200.000000"), ts("1800000000.000270")),
            ],
            "a tombstone's latest_reply, and one above the read's latest, as Slack sent them"
        );

        // The same extremes from a page newest first and one oldest first, and the parent is not the oldest.
        let hist = ok(api.scan(&scan_query("C0HIST", None, 10)).await);
        let replies = ok(api.scan(&scan_query("C0REPL", Some(PARENT), 10)).await);
        assert_eq!(hist.oldest, Some(ts("1700000200.000000")));
        assert_eq!(hist.newest, Some(ts("1700000400.000000")));
        assert_eq!(
            (&replies.oldest, &replies.newest),
            (&hist.oldest, &hist.newest)
        );
        assert_eq!(replies.records.len(), 4, "the parent is inside the bounds");
        assert_eq!(replies.threads, [(ts(PARENT), ts("1700000400.000000"))]);

        // A replies scan's records are the messages inside its bounds: a parent below `after` is no record (it is
        // still in `threads` with its latest_reply) and is not the page's oldest.
        let bounds = Bounds::new(Some("1700000150.000000"), Some(SCAN_BEFORE), None).unwrap();
        let q = ScanQuery::new("C0REPL", Some(PARENT), bounds, 10).unwrap();
        let got = ok(api.scan(&q).await);
        assert_eq!(tss_of(&got).len(), 3);
        assert_eq!(got.records[0].ts, ts("1700000200.000000"));
        assert_eq!(got.threads, [(ts(PARENT), ts("1700000400.000000"))]);
        assert_eq!(got.oldest, Some(ts("1700000200.000000")));

        // A message with no valid ts, or a latest_reply that is not one, fails the page.
        for c in ["C0NOTS", "C0BADLR"] {
            let got = api.scan(&scan_query(c, None, 10)).await;
            assert_eq!(got, Err(ApiError::Malformed), "{c}");
        }
        fake.shutdown().await;
    })
    .await;
}

fn tss_of(page: &crate::api::ScanPage) -> Vec<&str> {
    page.records.iter().map(|r| r.ts.as_str()).collect()
}

#[tokio::test]
async fn a_limited_page_is_a_page_with_its_flag() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            auth(seen).unwrap_or_else(|| match seen.param("channel").unwrap().as_str() {
                "C0LAST" => scan_reply(vec![msg("1700000100.000001")], false, Some(true)),
                _ => scan_reply(vec![msg("1700000100.000001")], true, Some(true)),
            })
        })
        .await;
        let api = ready(&fake).await;
        for (c, more) in [("C0LAST", false), ("C0MORE", true)] {
            let got = ok(api.scan(&scan_query(c, None, 10)).await);
            assert!(got.limited && got.has_more == more, "{c}");
            assert!(got.records.iter().all(|r| r.limited), "{c}");
        }
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_replies_scan_at_999_may_hold_1000_with_its_parent() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            auth(seen).unwrap_or_else(|| {
                let held = match seen.param("channel").unwrap().as_str() {
                    "C0ENOUGH" => 1000,
                    _ => 1001,
                };
                let mut messages = vec![msg(PARENT)];
                messages
                    .extend((1..held).map(|i| reply_msg(&format!("{}.000001", 1_700_000_200 + i))));
                scan_reply(messages, false, None)
            })
        })
        .await;
        let api = ready(&fake).await;
        let got = api
            .scan(&scan_query("C0ENOUGH", Some(PARENT), MAX_SCAN))
            .await;
        assert_eq!(ok(got).records.len(), 1000);
        let got = api
            .scan(&scan_query("C0TOOMANY", Some(PARENT), MAX_SCAN))
            .await;
        assert_eq!(got, Err(ApiError::Malformed));
        // A channel read has no parent to add: 1,000 at 999 is too many.
        let got = api.scan(&scan_query("C0ENOUGH", None, MAX_SCAN)).await;
        assert_eq!(got, Err(ApiError::Malformed));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_replies_page_holding_only_its_parent_with_no_more_is_an_end() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            auth(seen).unwrap_or_else(|| match seen.param("channel").unwrap().as_str() {
                "C0END" => scan_reply(vec![msg(PARENT)], false, None),
                // More behind it, and nothing to walk back from.
                "C0LIE" => scan_reply(vec![msg(PARENT)], true, None),
                "C0EMPTY" => scan_reply(vec![], true, None),
                // A page shorter than its limit with more behind it is a page.
                _ => scan_reply(vec![msg("1700000200.000000")], true, None),
            })
        })
        .await;
        let api = ready(&fake).await;
        let thread = |c: &'static str| {
            let api = &api;
            async move { api.scan(&scan_query(c, Some(PARENT), 999)).await }
        };
        let end = ok(thread("C0END").await);
        assert!(!end.has_more && end.oldest.is_none() && end.newest.is_none());
        assert_eq!(end.records.len(), 1);
        assert_eq!(thread("C0LIE").await, Err(ApiError::Malformed));
        let got = api.scan(&scan_query("C0EMPTY", None, 999)).await;
        assert_eq!(got, Err(ApiError::Malformed));
        let short = ok(api.scan(&scan_query("C0SHORT", None, 999)).await);
        assert!(short.has_more && short.records.len() == 1);
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_scan_rechecks_its_bounds_and_refuses_a_limit_over_999() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            auth(seen).unwrap_or_else(|| {
                scan_reply(
                    vec![
                        msg("1700000500.000000"),
                        msg("1700000400.000000"),
                        msg("1700000300.000000"),
                        msg("1700000100.000000"),
                        msg("1700000099.999999"),
                    ],
                    false,
                    None,
                )
            })
        })
        .await;
        let api = ready(&fake).await;
        // Slack's edges are not trusted: `oldest` and `latest` are both exclusive, and the page's raw extremes stay.
        let b = Bounds::new(Some("1700000100.000000"), Some("1700000400.000000"), None).unwrap();
        let q = ScanQuery::new("C0EDGE", None, b, 10).unwrap();
        let got = ok(api.scan(&q).await);
        assert_eq!(tss_of(&got), ["1700000300.000000"]);
        assert_eq!(got.oldest, Some(ts("1700000099.999999")));
        assert_eq!(got.newest, Some(ts("1700000500.000000")));

        // Refused unsent: a limit of 0 or over 999, a missing bound, an inclusive one, an empty or inverted window,
        // a bad id or thread.
        let sent = fake.seen().len();
        let both = || Bounds::new(Some(SCAN_AFTER), Some(SCAN_BEFORE), None).unwrap();
        let bounds =
            |a: Option<&str>, b: Option<&str>, u: Option<&str>| Bounds::new(a, b, u).unwrap();
        let window =
            |a: &str, b: &str| ScanQuery::new("C0EDGE", None, bounds(Some(a), Some(b), None), 10);
        let refused = [
            ScanQuery::new("C0EDGE", None, both(), 0),
            ScanQuery::new("C0EDGE", None, both(), 1000),
            ScanQuery::new("C0EDGE", None, bounds(None, Some(SCAN_BEFORE), None), 10),
            ScanQuery::new("C0EDGE", None, bounds(Some(SCAN_AFTER), None, None), 10),
            ScanQuery::new(
                "C0EDGE",
                None,
                bounds(Some(SCAN_AFTER), None, Some(SCAN_BEFORE)),
                10,
            ),
            ScanQuery::new("not an id", None, both(), 10),
            window(SCAN_AFTER, SCAN_AFTER),
            window(SCAN_BEFORE, SCAN_AFTER),
        ];
        for r in refused {
            assert_eq!(r, Err(ApiError::BadRequest));
        }
        let junk = ScanQuery::new("C0EDGE", Some("17000"), both(), 10);
        assert_eq!(junk, Err(ApiError::BadBound));
        assert!(ScanQuery::new("C0EDGE", None, both(), MAX_SCAN).is_ok());
        assert_eq!(fake.seen().len(), sent, "nothing was sent");
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_thread_continuation_under_a_lower_bound_alone_is_refused_unsent() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            auth(seen).unwrap_or_else(|| {
                let next = seen.param("cursor").is_none().then_some("c1");
                Reply::json(&page(vec![msg("1700000200.000000")], next))
            })
        })
        .await;
        let api = ready(&fake).await;
        let after = Some("1700000100.000000");
        let thread =
            |channel: &str, b: Bounds| HistoryQuery::new(channel, Some(PARENT), b, 2).unwrap();
        let lower_only = thread("C0THR", Bounds::new(after, None, None).unwrap());
        let first = ok(api.history(&lower_only, None).await);
        let cont = first.next.expect("more pages");
        let sent = fake.seen().len();
        assert_eq!(
            api.history(&lower_only, Some(&cont)).await,
            Err(ApiError::BadRequest)
        );
        assert_eq!(fake.seen().len(), sent, "refused before anything was sent");

        // The same query's first page, and every other form's continuation, are sent as ever.
        let upper = Some("1700001000.000000");
        let forms = [
            thread("C0A", Bounds::new(after, upper, None).unwrap()),
            thread("C0B", Bounds::new(after, None, upper).unwrap()),
            thread("C0C", Bounds::new(None, None, None).unwrap()),
            thread("C0D", Bounds::new(None, upper, None).unwrap()),
            query("C0E", Bounds::new(after, None, None).unwrap(), 2),
        ];
        for form in &forms {
            let first = ok(api.history(form, None).await);
            let sent = fake.seen().len();
            let second = ok(api.history(form, first.next.as_ref()).await);
            assert!(second.next.is_none());
            assert_eq!(fake.seen().len(), sent + 1, "{form:?}");
        }
        fake.shutdown().await;
    })
    .await;
}

/// One `conversations.list` entry as Slack gives it, with the three facts the list keeps.
fn listed(id: &str, name: &str, private: bool, member: bool) -> Value {
    json!({"id": id, "name": name, "is_private": private, "is_member": member, "topic": {"value": "ignored"}})
}

fn channel_page(channels: Vec<Value>, next: &str) -> Reply {
    Reply::json(&json!({"ok": true, "channels": channels, "response_metadata": {"next_cursor": next}}))
}

/// Two hundred channels whose ids carry `page`, the size of the page the list asks for.
fn full_page(page: usize) -> Vec<Value> {
    (0..200).map(|i| listed(&format!("C{page}X{i}"), &format!("n{page}-{i}"), false, i % 2 == 0)).collect()
}

#[tokio::test]
async fn the_channel_list_follows_the_cursor_to_its_bound_and_fails_whole() {
    bounded(async {
        // The scenario the script plays, by name: the test sets it before each list.
        let mode = Arc::new(Mutex::new("two".to_string()));
        let fake = {
            let mode = mode.clone();
            FakeSlack::start(Identity::Trusted, move |seen, _| {
                let page = seen.param("cursor").map(|c| c.trim_start_matches('p').parse::<usize>().unwrap_or(99)).unwrap_or(0);
                match (mode.lock().unwrap().as_str(), page) {
                    ("two", 0) => channel_page(
                        vec![
                            listed("C0OPEN", "open", false, true),
                            listed("G0SECRET", "se\u{7}cret", true, true),
                            json!({"id": "C0BARE", "name": "bare"}),
                            json!({"name": "no id"}),
                            json!({"id": "bad id", "name": "spaces"}),
                            json!({"id": "D0DIRECT", "name": "dm"}),
                            listed("C0EMPTY", " \u{7} ", false, true),
                        ],
                        "p1",
                    ),
                    ("two", 1) => channel_page(vec![listed("C0LATE", "late", true, false)], ""),
                    // Every page names another cursor: the list stops at five pages, a thousand channels, and says more.
                    ("endless", p) => channel_page(full_page(p), &format!("p{}", p + 1)),
                    ("repeat", 0) => channel_page(full_page(0), "p1"),
                    ("repeat", _) => channel_page(full_page(1), "p1"),
                    ("huge", _) => channel_page(full_page(0), &"x".repeat(MAX_CURSOR + 1)),
                    ("fits", 0) => channel_page(full_page(0), &"p".repeat(MAX_CURSOR)),
                    ("fits", _) => channel_page(Vec::new(), ""),
                    ("fails_second", 0) => channel_page(full_page(0), "p1"),
                    ("fails_second", _) => Reply::json(&json!({"ok": false, "error": "missing_scope"})),
                    ("limited_second", 0) => channel_page(full_page(0), "p1"),
                    ("limited_second", _) => status(429, &[("Retry-After", "7")], &json!({})),
                    ("no_array", _) => Reply::json(&json!({"ok": true, "channels": {}})),
                    ("too_big", _) => channel_page((0..201).map(|i| listed(&format!("C{i}"), "n", false, true)).collect(), ""),
                    // A page of the right size whose cursor is missing is the last one.
                    ("no_cursor", _) => Reply::json(&json!({"ok": true, "channels": [listed("C0ONE", "one", false, true)]})),
                    _ => Reply::Drop,
                }
            })
            .await
        };
        let api = new_api(&fake);
        let list = |mode_name: &str| {
            *mode.lock().unwrap() = mode_name.to_string();
            api.channels()
        };

        let got = list("two").await.expect("two pages");
        assert!(!got.more);
        let want = |id: &str, name: &str, private: bool, member: bool| Channel { id: id.into(), name: name.into(), private, member };
        assert_eq!(
            got.channels,
            [
                want("C0OPEN", "open", false, true),
                want("G0SECRET", "secret", true, true),
                // No `is_private` reads as private and no `is_member` as not a member.
                want("C0BARE", "bare", true, false),
                want("C0LATE", "late", true, false),
            ],
            "an entry with no valid channel id or no name is skipped, and a name is cleaned"
        );
        let seen = fake.seen();
        assert_eq!(seen.len(), 2);
        let first = &seen[0];
        assert_eq!((first.method.as_str(), first.path().as_str()), ("GET", "/api/conversations.list"));
        assert_eq!(first.authorization.as_deref(), Some(&*format!("Bearer {TOKEN}")));
        assert_eq!(first.param("types").as_deref(), Some("public_channel,private_channel"));
        assert_eq!(first.param("exclude_archived").as_deref(), Some("true"));
        assert_eq!(first.param("limit").as_deref(), Some("200"));
        assert_eq!(first.param("cursor"), None);
        assert_eq!(seen[1].param("cursor").as_deref(), Some("p1"), "the cursor Slack gave is the one sent back");

        let before = fake.seen().len();
        let got = list("endless").await.expect("the capped list");
        assert_eq!((got.channels.len(), got.more, fake.seen().len() - before), (MAX_LISTED, true, 5), "five pages, then more");
        assert_eq!(got.channels[0].id, "C0X0");

        let before = fake.seen().len();
        let got = list("no_cursor").await.expect("a missing cursor ends the list");
        assert_eq!((got.channels.len(), got.more, fake.seen().len() - before), (1, false, 1));

        // The list is whole or an error: no page is returned alone.
        assert_eq!(list("repeat").await, Err(ApiError::Repeated));
        assert_eq!(list("huge").await, Err(ApiError::TooLarge));
        assert!(list("fits").await.is_ok(), "a cursor of exactly the bound is carried");
        assert_eq!(list("fails_second").await, Err(ApiError::Slack("missing_scope".into())));
        assert_eq!(list("no_array").await, Err(ApiError::Malformed));
        assert_eq!(list("too_big").await, Err(ApiError::Malformed), "a page over what it was asked for");
        // Last: a rate limit holds every later read.
        assert!(matches!(list("limited_second").await, Err(ApiError::Limited { assumed: false, .. })));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_channels_facts_come_from_one_read_and_the_naming_rule_reads_them() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            let id = seen.param("channel").unwrap_or_default();
            let info = |channel: Value| Reply::json(&json!({"ok": true, "channel": channel}));
            match id.as_str() {
                "C0PUB" => info(json!({"id": id, "name": "open", "is_private": false, "is_member": true})),
                "C0PUBOUT" => info(json!({"name": "  out\u{7} ", "is_private": false, "is_member": false})),
                "G0PRIV" => info(json!({"name": "secret", "is_private": true, "is_member": true})),
                "C0NOFLAGS" => info(json!({"name": "unsure"})),
                "C0EMPTY" => info(json!({"name": " \u{7} ", "is_private": false})),
                "C0NONAME" => info(json!({"is_private": false})),
                "C0GONE" => Reply::json(&json!({"ok": false, "error": "channel_not_found"})),
                _ => Reply::Drop,
            }
        })
        .await;
        let api = new_api(&fake);
        let fact = |id: &str, name: &str, private: bool, member: bool| Ok(Channel { id: id.into(), name: name.into(), private, member });

        assert_eq!(api.channel("C0PUB").await, fact("C0PUB", "open", false, true));
        assert_eq!(api.channel("C0PUBOUT").await, fact("C0PUBOUT", "out", false, false));
        assert_eq!(api.channel("G0PRIV").await, fact("G0PRIV", "secret", true, true));
        assert_eq!(api.channel("C0NOFLAGS").await, fact("C0NOFLAGS", "unsure", true, false), "a reply that does not say is private and not joined");
        assert_eq!(api.channel("C0EMPTY").await, Err(ApiError::Malformed));
        assert_eq!(api.channel("C0NONAME").await, Err(ApiError::Malformed));
        assert_eq!(api.channel("C0GONE").await, Err(ApiError::Slack("channel_not_found".into())));
        assert_eq!(calls(&fake, "conversations.info"), 7, "one read each");
        assert_eq!(api.channel("U0PERSON").await, Err(ApiError::BadRequest));
        assert_eq!(api.channel("bad id").await, Err(ApiError::BadRequest));
        assert_eq!(calls(&fake, "conversations.info"), 7, "a refused id is not sent");

        // The naming rule of guarantee 16 reads the same facts through the same function: a channel is named when it is not
        // private, or when it is mapped; every one of these asks `conversations.info`, so none has a second reader.
        let before = calls(&fake, "conversations.info");
        for (id, named) in [("C0PUB", true), ("G0PRIV", false), ("C0NOFLAGS", false), ("C0EMPTY", false)] {
            assert_eq!(api.author_name(id).await != id, named, "{id} unmapped");
        }
        assert_eq!(calls(&fake, "conversations.info"), before + 4);
        assert!(api.channel_names(&["G0PRIV".into(), "C0NOFLAGS".into()]).await.is_empty());
        assert_eq!((api.cached_name("G0PRIV").as_deref(), api.cached_name("C0NOFLAGS").as_deref()), (Some("secret"), Some("unsure")));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_join_is_sent_once_and_already_in_channel_is_joined() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            let body = seen.json().unwrap_or_default();
            match body["channel"].as_str().unwrap_or("") {
                "C0OK" => Reply::json(&json!({"ok": true, "channel": {"id": "C0OK"}})),
                "C0WARN" => Reply::json(&json!({"ok": true, "warning": "already_in_channel"})),
                "C0ALREADY" => Reply::json(&json!({"ok": false, "error": "already_in_channel"})),
                "C0SCOPE" => Reply::json(&json!({"ok": false, "error": "missing_scope"})),
                "G0PRIV" => Reply::json(&json!({"ok": false, "error": "method_not_supported_for_channel_type"})),
                "C0GONE" => Reply::json(&json!({"ok": false, "error": "channel_not_found"})),
                "C0ARCH" => Reply::json(&json!({"ok": false, "error": "is_archived"})),
                "C0FOUR" => status(403, &[], &json!({})),
                "C0INT" => Reply::json(&json!({"ok": false, "error": "internal_error"})),
                "C0ODD" => Reply::json(&json!({"ok": false, "error": "Has Spaces https://x"})),
                "C0NOOK" => Reply::json(&json!({"channel": {}})),
                "C0TEXT" => Reply::ok(b"not json"),
                "C0FIVE" => status(503, &[], &json!({})),
                "C0RATE" => status(429, &[("Retry-After", "7")], &json!({})),
                "C0RATE2" => Reply::json(&json!({"ok": false, "error": "ratelimited"})),
                _ => Reply::Drop,
            }
        })
        .await;
        let api = new_api(&fake);

        for joined in ["C0OK", "C0WARN", "C0ALREADY"] {
            assert_eq!(api.join(joined).await, Ok(JoinOutcome::Joined), "{joined}");
        }
        let sent = fake.seen().remove(0);
        assert_eq!((sent.method.as_str(), sent.path().as_str()), ("POST", "/api/conversations.join"));
        assert_eq!(sent.authorization.as_deref(), Some(&*format!("Bearer {TOKEN}")));
        assert_eq!(sent.json().unwrap(), json!({"channel": "C0OK"}));

        for refused in ["C0SCOPE", "G0PRIV", "C0GONE", "C0ARCH"] {
            let code = match refused {
                "C0SCOPE" => "missing_scope",
                "G0PRIV" => "method_not_supported_for_channel_type",
                "C0GONE" => "channel_not_found",
                _ => "is_archived",
            };
            assert_eq!(api.join(refused).await, Ok(JoinOutcome::Refused(code.into())), "{refused}");
        }
        assert_eq!(api.join("C0FOUR").await, Ok(JoinOutcome::Refused("http_403".into())));
        // Nothing here is an acknowledgement or a definite refusal: a read settles whether the app is in.
        let unknown = ["C0INT", "C0ODD", "C0NOOK", "C0TEXT", "C0FIVE", "C0RATE", "C0RATE2", "C0DROP"];
        for id in unknown {
            assert_eq!(api.join(id).await, Ok(JoinOutcome::Unknown), "{id}");
        }
        assert_eq!(calls(&fake, "conversations.join"), 3 + 4 + 1 + unknown.len(), "each join is one request, none resent");

        // An id that is no channel's is refused before anything is sent, and so is a call on a closed client.
        let before = fake.seen().len();
        for bad in ["U0PERSON", "bad id", ""] {
            assert_eq!(api.join(bad).await, Err(ApiError::BadRequest), "{bad:?}");
        }
        api.close();
        assert_eq!(api.join("C0OK").await, Err(ApiError::Closed));
        assert_eq!(fake.seen().len(), before);
        fake.shutdown().await;
    })
    .await;
}

/// A picture's bytes, synthetic.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n synthetic picture bytes";
/// The first upload step and the share.
const ASK: &str = "/api/files.getUploadURLExternal";
const SHARE: &str = "/api/files.completeUploadExternal";

/// The paths the fake saw, `auth.test` left out.
fn steps(fake: &FakeSlack) -> Vec<String> {
    fake.seen().iter().map(Seen::path).filter(|p| p != "/api/auth.test").collect()
}

fn shared(fake: &FakeSlack) -> usize {
    calls(fake, "files.completeUploadExternal")
}

#[tokio::test]
async fn a_file_is_uploaded_unauthorized_and_shared_once_with_its_label() {
    bounded(async {
        let upload = Upload::new();
        let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
            upload.reply(seen, port).unwrap_or(Reply::Drop)
        })
        .await;
        let api = new_api(&fake);
        let outcome = api
            .post_file(
                api.transfer().unwrap(),
                "C0CHAN",
                Some("1700000100.000000"),
                PostKind::Row,
                "build-7",
                "fig 1.png",
                PNG.to_vec(),
                Some("the <!channel> & its fit"),
            )
            .await;
        assert_eq!(
            outcome,
            Ok(FileOutcome::Posted { file: "F0FILE".into(), ts: Some(ts("1700000800.000100")) })
        );
        let seen = fake.seen();
        let paths: Vec<String> = seen.iter().map(Seen::path).collect();
        assert_eq!(paths, [ASK, "/upload/v1/F0FILE", SHARE], "three requests, in order, once");
        let bearer = format!("Bearer {TOKEN}");

        let (ask, bytes, share) = (&seen[0], &seen[1], &seen[2]);
        assert_eq!(ask.method, "GET");
        assert_eq!(ask.param("filename").as_deref(), Some("fig 1.png"));
        assert_eq!(ask.param("length"), Some(PNG.len().to_string()));
        assert_eq!(ask.authorization.as_deref(), Some(&*bearer));

        assert_eq!(bytes.method, "POST");
        assert!(bytes.host.starts_with("files.slack.com:"), "{}", bytes.host);
        assert_eq!(bytes.body, PNG, "the bytes sent are the bytes given");
        assert_eq!(bytes.authorization, None, "the upload carries no authorization");

        assert_eq!(share.method, "POST");
        assert_eq!(share.authorization.as_deref(), Some(&*bearer));
        let body = share.json().unwrap();
        assert_eq!(body["files"], json!([{"id": "F0FILE", "title": "fig 1.png"}]));
        assert_eq!(body["channel_id"], "C0CHAN");
        assert_eq!(body["thread_ts"], "1700000100.000000");
        assert_eq!(
            body["initial_comment"],
            "build-7 (row via Ship of Tools): the &lt;!channel&gt; &amp; its fit",
            "the label and the comment, escaped"
        );

        // A row's file with no comment and no thread: the label alone, at the channel's top level.
        let outcome = api.post_file(api.transfer().unwrap(), "C0CHAN", None, PostKind::Row, "tester Test", "a.png", PNG.to_vec(), None).await;
        assert!(matches!(outcome, Ok(FileOutcome::Posted { .. })), "{outcome:?}");
        let body = fake.seen().pop().unwrap().json().unwrap();
        assert_eq!(body["initial_comment"], "tester Test (row via Ship of Tools)");
        assert!(body.get("thread_ts").is_none());
        assert_eq!(shared(&fake), 2);

        // Input a file post cannot carry is refused before anything is sent; the bound is on the escaped label and
        // comment together.
        let before = fake.seen().len();
        let fits = "x".repeat(MAX_TEXT - "tester (row via Ship of Tools): ".len());
        let over = format!("{fits}&");
        let long_name = "x".repeat(257);
        let big = vec![0u8; MAX_CHAT_FILE + 1];
        let cases: [(&str, Option<&str>, &str, &str, &[u8], Option<&str>); 11] = [
            ("bad id", None, "Ada", "a.png", PNG, None),
            ("C0CHAN", Some("1700000100.12345"), "Ada", "a.png", PNG, None),
            ("C0CHAN", None, "", "a.png", PNG, None),
            ("C0CHAN", None, "Ada", "", PNG, None),
            ("C0CHAN", None, "Ada", "dir/a.png", PNG, None),
            ("C0CHAN", None, "Ada", "a\nb.png", PNG, None),
            ("C0CHAN", None, "Ada", &long_name, PNG, None),
            ("C0CHAN", None, "Ada", "a.png", b"", None),
            ("C0CHAN", None, "Ada", "a.png", &big, None),
            ("C0CHAN", None, "Ada", "a.png", PNG, Some("")),
            ("C0CHAN", None, "Ada", "a.png", PNG, Some(&over)),
        ];
        for (channel, thread, name, file, bytes, comment) in cases {
            let r = api.post_file(api.transfer().unwrap(), channel, thread, PostKind::Row, name, file, bytes.to_vec(), comment).await;
            assert!(r.is_err(), "{channel} {name:?} {file:.10} {} {:?}", bytes.len(), comment.map(str::len));
        }
        assert_eq!(fake.seen().len(), before, "nothing sent for a refused input");
        let exact = api.post_file(api.transfer().unwrap(), "C0CHAN", None, PostKind::Row, "tester", "a.png", PNG.to_vec(), Some(&fits)).await;
        assert!(matches!(exact, Ok(FileOutcome::Posted { .. })), "the bound itself is carried");
        let full_size = api.post_file(api.transfer().unwrap(), "C0CHAN", None, PostKind::Row, "tester", "a.png", vec![7u8; MAX_CHAT_FILE], None).await;
        assert!(matches!(full_size, Ok(FileOutcome::Posted { .. })), "a file of exactly the bound is carried");
        assert_eq!(fake.seen().iter().rev().nth(1).unwrap().body.len(), MAX_CHAT_FILE);

        let before = fake.seen().len();
        api.close();
        assert_eq!(api.transfer().err(), Some(ApiError::Closed), "a closed client grants no transfer");
        assert_eq!(fake.seen().len(), before);
        fake.shutdown().await;
    })
    .await;
}

/// Each step's failure by the file's name: `drop1` and `hang1` at step 1, `drop2` and `hang2` at the upload, and at
/// the share `drop3`, `hang3`, `five` (a 503) and `noid` (an `ok` without the file).
#[tokio::test]
async fn only_the_share_can_be_unknown() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, port| {
            let path = seen.path();
            if path == ASK {
                let file = seen.param("filename").unwrap_or_default();
                return match file.as_str() {
                    "drop1" => Reply::Drop,
                    "hang1" => Reply::Hang,
                    _ => Reply::json(&json!({
                        "ok": true, "file_id": "F0FILE", "upload_url": format!("https://files.slack.com:{port}/upload/{file}"),
                    })),
                };
            }
            if path == SHARE {
                let file = seen.json().and_then(|b| b["files"][0]["title"].as_str().map(String::from));
                return match file.as_deref().unwrap_or_default() {
                    "drop3" => Reply::Drop,
                    "hang3" => Reply::Hang,
                    "five" => status(503, &[], &json!({})),
                    "noid" => Reply::json(&json!({"ok": true, "files": []})),
                    _ => Reply::json(&json!({"ok": true, "files": [{"id": "F0FILE"}]})),
                };
            }
            match path.as_str() {
                "/upload/drop2" => Reply::Drop,
                "/upload/hang2" => Reply::Hang,
                _ => Reply::ok(b"OK"),
            }
        })
        .await;
        // A short deadline sits only on the step a case hangs at (the ask is a read, the upload a transfer, the share
        // a post); every other step keeps a generous one, so a slow round trip is slow and never a timeout.
        let short = Duration::from_millis(300);
        let long = Duration::from_secs(10);
        let api_for = |file: &str| match file {
            "hang1" => new_api(&fake).with_deadlines(short, long, long).with_transfer_deadline(long),
            "hang2" => new_api(&fake).with_deadlines(long, long, long).with_transfer_deadline(short),
            "hang3" => new_api(&fake).with_deadlines(long, short, long).with_transfer_deadline(long),
            _ => new_api(&fake).with_deadlines(long, long, long).with_transfer_deadline(long),
        };
        let post = |file: &'static str| {
            let api = api_for(file);
            async move { api.post_file(api.transfer().unwrap(), "C0CHAN", None, PostKind::Row, "build-7", file, PNG.to_vec(), None).await }
        };

        for before_the_share in ["drop1", "drop2"] {
            let got = post(before_the_share).await;
            assert!(matches!(got, Ok(FileOutcome::Refused(_))), "{before_the_share}: {got:?}");
        }
        for before_the_share in ["hang1", "hang2"] {
            let start = Instant::now();
            assert_eq!(post(before_the_share).await, Ok(FileOutcome::Refused("timeout".into())), "{before_the_share}");
            // Ended at its own step's 300 ms, not at the 10 s of a step that was not meant to hang.
            assert!(start.elapsed() < Duration::from_secs(5), "{before_the_share} ended at {:?}", start.elapsed());
        }
        assert!(fake.seen().iter().any(|s| s.path() == "/upload/hang2"), "hang2 timed out at the upload, not before it");
        assert_eq!(shared(&fake), 0, "no share is sent after a failure before it");

        for at_the_share in ["drop3", "hang3", "five", "noid"] {
            let start = Instant::now();
            assert_eq!(post(at_the_share).await, Ok(FileOutcome::Unknown), "{at_the_share}");
            assert!(start.elapsed() < Duration::from_secs(5), "{at_the_share} ended at {:?}", start.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(shared(&fake), 4, "each share sent once, never again");
        assert_eq!(post("good").await, Ok(FileOutcome::Posted { file: "F0FILE".into(), ts: None }));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_share_with_no_ts_is_posted_with_its_file() {
    bounded(async {
        for (upload, want) in [
            (Upload::new().no_ts(), None),
            (Upload::new().private(), Some(ts("1700000800.000100"))),
        ] {
            let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
                upload.reply(seen, port).unwrap_or(Reply::Drop)
            })
            .await;
            let api = new_api(&fake);
            let got = api.post_file(api.transfer().unwrap(), "G0PRIV", None, PostKind::Row, "build-7", "a.png", PNG.to_vec(), None).await;
            assert_eq!(got, Ok(FileOutcome::Posted { file: "F0FILE".into(), ts: want.clone() }), "{want:?}");
            fake.shutdown().await;
        }
    })
    .await;
}

/// A file this client shared, read back through history in the fake as a `file_share` message from the app with the
/// share's comment as its text, normalizes to the origin and name it was posted as, its text the comment alone.
#[tokio::test]
async fn a_file_share_read_back_through_history_keeps_its_origin() {
    bounded(async {
        let stored: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let store = stored.clone();
        let upload = Upload::new();
        let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
            if let Some(r) = auth(seen) {
                return r;
            }
            match seen.path().as_str() {
                SHARE => {
                    let body = seen.json().unwrap();
                    let mut store = store.lock().unwrap();
                    let ts = format!("1700000{:03}.000100", store.len() + 1);
                    let (channel, file) = (body["channel_id"].clone(), &body["files"][0]);
                    // As Slack stores a share: the app's bot id and user, the comment as text, the file.
                    store.push(json!({
                        "ts": ts, "subtype": "file_share", "bot_id": OWN_BOT, "user": OWN_USER,
                        "text": body["initial_comment"], "upload": false,
                        "files": [{"id": file["id"], "name": file["title"], "mimetype": "image/png", "size": PNG.len()}],
                    }));
                    let shares = json!({channel.as_str().unwrap(): [{"ts": ts}]});
                    Reply::json(&json!({"ok": true, "files": [{"id": file["id"], "shares": {"public": shares}}]}))
                }
                "/api/conversations.history" => {
                    let mut all = store.lock().unwrap().clone();
                    all.reverse();
                    Reply::json(&page(all, None))
                }
                _ => upload.reply(seen, port).unwrap_or(Reply::Drop),
            }
        })
        .await;
        let api = ready(&fake).await;
        let a = api
            .post_file(api.transfer().unwrap(), "C0CHAN", None, PostKind::Row, "build-7", "fig.png", PNG.to_vec(), Some("the <fit> & more"))
            .await;
        let b = api.post_file(api.transfer().unwrap(), "C0CHAN", None, PostKind::Row, "R&D <tester>", "b.png", PNG.to_vec(), None).await;
        let posted: Vec<String> = [a, b]
            .into_iter()
            .map(|o| match o {
                Ok(FileOutcome::Posted { ts: Some(ts), .. }) => ts.as_str().to_string(),
                other => panic!("{other:?}"),
            })
            .collect();
        let page = api.history(&query("C0CHAN", Bounds::default(), 5), None).await.unwrap();
        let by_ts = |ts: &str| page.records.iter().find(|r| r.ts.as_str() == ts).unwrap();
        let row = by_ts(&posted[0]);
        assert_eq!((row.origin, row.author.name.as_str()), (Origin::Row, "build-7"));
        assert_eq!(row.text, "the &lt;fit&gt; &amp; more", "the comment, as Slack's text");
        assert_eq!(row.files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["fig.png"]);
        let second = by_ts(&posted[1]);
        assert_eq!((second.origin, second.author.name.as_str(), second.text.as_str()), (Origin::Row, "R&D <tester>", ""));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_foreign_upload_url_is_refused_before_any_byte_and_nothing_is_shared() {
    bounded(async {
        let long = format!("/upload/{}", "x".repeat(2100));
        for (host, path, code) in [
            ("other.example", "/upload/v1/F0FILE", "destination"),
            ("evilslack.com", "/upload/v1/F0FILE", "destination"),
            ("slack.com.evil.example", "/upload/v1/F0FILE", "destination"),
            ("files.slack.com", long.as_str(), "malformed"),
        ] {
            let upload = Upload::new().upload_to(host, path);
            let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
                upload.reply(seen, port).unwrap_or(Reply::Drop)
            })
            .await;
            let api = new_api(&fake);
            let got = api.post_file(api.transfer().unwrap(), "C0CHAN", None, PostKind::Row, "build-7", "a.png", PNG.to_vec(), None).await;
            assert_eq!(got, Ok(FileOutcome::Refused(code.into())), "{host}");
            assert_eq!(steps(&fake), [ASK], "{host}: only the first step was sent");
            assert_eq!(fake.connections(), 1, "{host}: no connection to the upload host");
            fake.shutdown().await;
        }
    })
    .await;
}

#[tokio::test]
async fn a_third_transfer_is_busy_at_once_and_a_dropped_one_frees_its_permit() {
    bounded(async {
        let upload = Upload::new();
        let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
            if seen.param("filename").as_deref() == Some("hang.png") {
                return Reply::Hang;
            }
            upload.reply(seen, port).unwrap_or(Reply::Drop)
        })
        .await;
        let api = Arc::new(new_api(&fake));
        let hang = |api: Arc<Api>| {
            tokio::spawn(async move {
                api.post_file(api.transfer().unwrap(), "C0CHAN", None, PostKind::Row, "build-7", "hang.png", PNG.to_vec(), None).await
            })
        };
        let (first, second) = (hang(api.clone()), hang(api.clone()));
        eventually("two transfers under way", || fake.seen().len() == 2).await;

        let start = Instant::now();
        assert_eq!(api.transfer().err(), Some(ApiError::Http(HttpError::Busy)), "a third transfer");
        assert!(start.elapsed() < Duration::from_secs(1), "busy at once");
        assert_eq!(fake.seen().len(), 2, "the third sent nothing");

        // A caller that drops its transfer frees the permit with it; one that ends frees its own.
        first.abort();
        let _ = first.await;
        for _ in 0..2 {
            let got = api.post_file(api.transfer().unwrap(), "C0CHAN", None, PostKind::Row, "build-7", "ok.png", PNG.to_vec(), None).await;
            assert!(matches!(got, Ok(FileOutcome::Posted { .. })), "{got:?}");
        }
        second.abort();
        let _ = second.await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_file_post_shows_no_token_or_private_url_in_any_error() {
    bounded(async {
        const SECRET_PATH: &str = "/upload/v1/sentinel-private-upload";
        let echo = format!("invalid auth https://files.slack.com{SECRET_PATH} {TOKEN}");
        // Each file fails at one step by its name; every answer echoes the token or the upload URL.
        let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
            let path = seen.path();
            let title = seen.json().and_then(|b| b["files"][0]["title"].as_str().map(String::from));
            let at = |host: &str, file: &str| format!("https://{host}:{port}{SECRET_PATH}?t={TOKEN}&f={file}");
            match (path.as_str(), seen.param("filename").or(title).as_deref()) {
                (ASK, Some("echo1.png")) => Reply::json(&json!({"ok": false, "error": echo})),
                (SHARE, Some("echo3.png")) => Reply::json(&json!({"ok": false, "error": echo})),
                (_, Some("html1.png")) => Reply::ok(format!("<html>{TOKEN} {SECRET_PATH}</html>").as_bytes()),
                (_, Some("foreign.png")) => {
                    Reply::json(&json!({"ok": true, "file_id": "F0FILE", "upload_url": at("other.example", "x")}))
                }
                (ASK, Some(file)) => {
                    Reply::json(&json!({"ok": true, "file_id": "F0FILE", "upload_url": at("files.slack.com", file)}))
                }
                (SECRET_PATH, _) if seen.param("f").as_deref() == Some("refused2.png") => {
                    status(403, &[], &json!({"error": TOKEN}))
                }
                (SECRET_PATH, _) => Reply::ok(TOKEN.as_bytes()),
                _ => status(400, &[], &json!({"error": format!("{TOKEN} {SECRET_PATH}")})),
            }
        })
        .await;
        let api = new_api(&fake);
        let mut shown = Vec::new();
        for file in ["echo1.png", "html1.png", "foreign.png", "refused2.png", "echo3.png", "refused3.png", ""] {
            let got = api.post_file(api.transfer().unwrap(), "C0CHAN", None, PostKind::Row, "build-7", file, PNG.to_vec(), None).await;
            shown.push(match got {
                Ok(outcome) => format!("{outcome:?}"),
                Err(e) => format!("{e:?} {e}"),
            });
        }
        let want = [
            "Refused(\"unknown_error\")",
            "Refused(\"malformed\")",
            "Refused(\"destination\")",
            "Refused(\"http_403\")",
            "Unknown",
            "Refused(\"http_400\")",
            "BadRequest bad_request",
        ];
        assert_eq!(shown, want);
        for text in &shown {
            for secret in [TOKEN, "sentinel-private", "files.slack.com", "other.example", "127.0.0.1", "upload"] {
                assert!(!text.contains(secret), "{text}");
            }
        }
        fake.shutdown().await;

        // A fetch's errors show neither the token nor a file's private URL either: each file fails at one place by its
        // id, and every answer echoes the token or the URL.
        let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
            let at = |host: &str, file: &str| format!("https://{host}:{port}{SECRET_PATH}?t={TOKEN}&f={file}");
            let info = |url: String| Reply::json(&json!({"ok": true, "file": {
                "id": seen.param("file"), "mimetype": "image/png", "size": 9, "url_private_download": url,
                "shares": {"public": {"C0CHAN": [{"ts": "1700000800.000100"}]}}}}));
            match (seen.path().as_str(), seen.param("file").as_deref()) {
                ("/api/files.info", Some("F0ECHO")) => {
                    Reply::json(&json!({"ok": false, "error": format!("invalid auth https://files.slack.com{SECRET_PATH} {TOKEN}")}))
                }
                ("/api/files.info", Some("F0HTML")) => Reply::ok(format!("<html>{TOKEN} {SECRET_PATH}</html>").as_bytes()),
                ("/api/files.info", Some("F0FOREIGN")) => info(at("other.example", "x")),
                ("/api/files.info", Some(file)) => info(at("files.slack.com", file)),
                (SECRET_PATH, _) if seen.param("f").as_deref() == Some("F0DENIED") => {
                    status(403, &[], &json!({"error": TOKEN}))
                }
                (SECRET_PATH, _) if seen.param("f").as_deref() == Some("F0PAGE") => {
                    Reply::Status { code: 200, headers: vec![("Content-Type".into(), "text/html".into())], body: TOKEN.into() }
                }
                _ => status(400, &[], &json!({"error": format!("{TOKEN} {SECRET_PATH}")})),
            }
        })
        .await;
        let api = new_api(&fake);
        let mut shown = Vec::new();
        for file in ["F0ECHO", "F0HTML", "F0FOREIGN", "F0DENIED", "F0PAGE", "F0GONE", "bad id"] {
            let got = api.fetch_file("C0CHAN", file, FileVariant::Original).await;
            shown.push(match got {
                Ok(outcome) => format!("{outcome:?}"),
                Err(e) => format!("{e:?} {e}"),
            });
        }
        let want = [
            "Slack(\"unknown_error\") slack: unknown_error",
            "Malformed malformed",
            "Http(Destination) http: destination: the destination is not an HTTPS Slack host",
            "Status(403) status: 403",
            "NotAnImage",
            "Status(400) status: 400",
            "BadRequest bad_request",
        ];
        assert_eq!(shown, want);
        for text in &shown {
            for secret in [TOKEN, "sentinel-private", "files.slack.com", "other.example", "127.0.0.1", "upload"] {
                assert!(!text.contains(secret), "{text}");
            }
        }
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_rate_limit_refuses_a_file_post_unsent() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            if seen.path() == ASK {
                status(429, &[("Retry-After", "5")], &json!({}))
            } else {
                Reply::Drop
            }
        })
        .await;
        let api = new_api(&fake);
        let post = || api.post_file(api.transfer().unwrap(), "C0CHAN", None, PostKind::Row, "build-7", "a.png", PNG.to_vec(), None);
        assert_eq!(post().await, Ok(FileOutcome::Limited { retry_after: Duration::from_secs(5), assumed: false }));
        // While the wait runs, the first step is a read refused without a request.
        assert!(matches!(post().await, Ok(FileOutcome::Limited { assumed: false, .. })));
        assert_eq!(steps(&fake), [ASK], "one request, nothing uploaded or shared");
        fake.shutdown().await;

        // A share that is limited is returned with its wait and is not resent.
        let upload = Upload::new();
        let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
            if seen.path() == SHARE {
                return status(429, &[("Retry-After", "7")], &json!({}));
            }
            upload.reply(seen, port).unwrap_or(Reply::Drop)
        })
        .await;
        let api = new_api(&fake);
        let got = api.post_file(api.transfer().unwrap(), "C0CHAN", None, PostKind::Row, "build-7", "a.png", PNG.to_vec(), None).await;
        assert_eq!(got, Ok(FileOutcome::Limited { retry_after: Duration::from_secs(7), assumed: false }));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(shared(&fake), 1);
        fake.shutdown().await;
    })
    .await;
}

/// The fake answering one file's `files.info` and URLs, and nothing else.
async fn files_fake(files: Files) -> FakeSlack {
    FakeSlack::start(Identity::Trusted, move |seen, port| files.reply(seen, port).unwrap_or(Reply::Drop)).await
}

/// The requests that were not Slack API calls: the file URLs asked for.
fn downloads(fake: &FakeSlack) -> Vec<Seen> {
    fake.seen().into_iter().filter(|s| !s.path().starts_with("/api/")).collect()
}

fn image(bytes: &[u8]) -> Result<FileFetch, ApiError> {
    Ok(FileFetch::Image { mime: "image/png".into(), bytes: bytes.to_vec() })
}

const THUMB_BYTES: &[u8] = b"\x89PNG thumbnail bytes";
const ORIGINAL_BYTES: &[u8] = b"\x89PNG original bytes";

#[tokio::test]
async fn a_fetch_reads_only_a_picture_shared_in_its_channel() {
    bounded(async {
        // The thumbnail and the original of a file shared in the channel: each one `files.info` and one GET, with the
        // bearer, to the file's host.
        let fake = files_fake(Files::new()).await;
        let api = new_api(&fake);
        assert_eq!(api.fetch_file("C0CHAN", "F0PIC", FileVariant::Thumb).await, image(THUMB_BYTES));
        assert_eq!(api.fetch_file("C0CHAN", "F0PIC", FileVariant::Original).await, image(ORIGINAL_BYTES));
        let seen = fake.seen();
        let paths: Vec<String> = seen.iter().map(Seen::path).collect();
        assert_eq!(paths, ["/api/files.info", &Files::thumb_path("thumb_360"), "/api/files.info", Files::DOWNLOAD]);
        let bearer = format!("Bearer {TOKEN}");
        assert_eq!(seen[0].param("file").as_deref(), Some("F0PIC"));
        for s in &seen {
            assert_eq!((s.method.as_str(), s.authorization.as_deref()), ("GET", Some(&*bearer)), "{}", s.target);
        }
        assert!(seen[1].host.starts_with("files.slack.com:"), "{}", seen[1].host);

        // The url_private is the original's when there is no download URL.
        let fake = files_fake(Files { download: false, ..Files::new() }).await;
        assert_eq!(new_api(&fake).fetch_file("C0CHAN", "F0PIC", FileVariant::Original).await, image(ORIGINAL_BYTES));
        assert_eq!(downloads(&fake)[0].path(), Files::PRIVATE);

        // Each row: the file, the channel asked, the variant, the answer and how many URLs were fetched.
        let big = MAX_CHAT_FILE as u64 + 1;
        let (thumb, original) = (FileVariant::Thumb, FileVariant::Original);
        let cases: Vec<(&str, Files, &str, FileVariant, Result<FileFetch, ApiError>, usize)> = vec![
            ("another channel's file", Files { channels: vec!["C0OTHER".into()], ..Files::new() }, "C0CHAN", original, Err(ApiError::BadRequest), 0),
            ("a file shared nowhere", Files { channels: vec![], ..Files::new() }, "C0CHAN", thumb, Err(ApiError::BadRequest), 0),
            ("a PDF", Files { mimetype: "application/pdf".into(), ..Files::new() }, "C0CHAN", thumb, Ok(FileFetch::NotAnImage), 0),
            ("an SVG", Files { mimetype: "image/svg+xml".into(), ..Files::new() }, "C0CHAN", original, Ok(FileFetch::NotAnImage), 0),
            ("an original over the bound", Files { size: Some(big), ..Files::new() }, "C0CHAN", original, Ok(FileFetch::TooLarge), 0),
            ("a thumbnail of one", Files { size: Some(big), ..Files::new() }, "C0CHAN", thumb, image(THUMB_BYTES), 1),
            ("an original of the bound", Files { size: Some(big - 1), ..Files::new() }, "C0CHAN", original, image(ORIGINAL_BYTES), 1),
            ("the older keys", Files { older_keys: true, ..Files::new() }, "C0CHAN", thumb, image(THUMB_BYTES), 1),
            ("a group by the older keys", Files { channels: vec!["G0PRIV".into()], older_keys: true, ..Files::new() }, "G0PRIV", thumb, image(THUMB_BYTES), 1),
            ("a private share", Files { channels: vec!["C0OTHER".into(), "G0PRIV".into()], ..Files::new() }, "G0PRIV", thumb, image(THUMB_BYTES), 1),
            ("a file shared in the channel", Files::new(), "C0CHAN", thumb, image(THUMB_BYTES), 1),
            ("a JPEG", Files { mimetype: "image/jpeg".into(), ..Files::new() }, "C0CHAN", thumb, image(THUMB_BYTES), 1),
            ("a GIF", Files { mimetype: "image/gif".into(), ..Files::new() }, "C0CHAN", thumb, image(THUMB_BYTES), 1),
            ("a WebP", Files { mimetype: "image/webp".into(), ..Files::new() }, "C0CHAN", original, image(ORIGINAL_BYTES), 1),
            ("an original with no size", Files { no_size: true, ..Files::new() }, "C0CHAN", original, Err(ApiError::Malformed), 0),
            ("a thumbnail with no size", Files { no_size: true, ..Files::new() }, "C0CHAN", thumb, image(THUMB_BYTES), 1),
            ("a direct message's share", Files { channels: vec!["D0DIRECT".into()], ..Files::new() }, "C0CHAN", thumb, Err(ApiError::BadRequest), 0),
        ];
        for (what, files, channel, variant, want, gets) in cases {
            let fake = files_fake(files).await;
            assert_eq!(new_api(&fake).fetch_file(channel, "F0PIC", variant).await, want, "{what}");
            assert_eq!(downloads(&fake).len(), gets, "{what}: a file URL is fetched only after every check");
            fake.shutdown().await;
        }

        // Slack's answer is for another file, or says no; nothing is fetched.
        let fake = files_fake(Files::new()).await;
        let api = new_api(&fake);
        assert_eq!(api.fetch_file("C0CHAN", "F0NONE", FileVariant::Thumb).await, Err(ApiError::Slack("file_not_found".into())));
        let wrong = FakeSlack::start(Identity::Trusted, |seen, port| {
            let mut other = Files::new().reply(seen, port).unwrap_or(Reply::Drop);
            if let (Reply::Status { body, .. }, "/api/files.info") = (&mut other, seen.path().as_str()) {
                *body = String::from_utf8_lossy(body).replace("F0PIC", "F0ELSE").into_bytes();
            }
            other
        })
        .await;
        assert_eq!(new_api(&wrong).fetch_file("C0CHAN", "F0PIC", FileVariant::Thumb).await, Err(ApiError::Malformed));
        assert_eq!(downloads(&wrong).len(), 0);
        wrong.shutdown().await;
        // A URL over 2 KiB is none: the original has no usable one, and the thumbnail's own is still read.
        let long = FakeSlack::start(Identity::Trusted, |seen, port| {
            let mut info = Files::new().reply(seen, port).unwrap_or(Reply::Drop);
            if let (Reply::Status { body, .. }, "/api/files.info") = (&mut info, seen.path().as_str()) {
                let padded = format!("/files-pri/F0PIC/{}/", "x".repeat(2100));
                *body = String::from_utf8_lossy(body).replace("/files-pri/F0PIC/", &padded).into_bytes();
            }
            info
        })
        .await;
        let api = new_api(&long);
        assert_eq!(api.fetch_file("C0CHAN", "F0PIC", FileVariant::Original).await, Err(ApiError::Malformed));
        assert_eq!(downloads(&long).len(), 0);
        assert_eq!(api.fetch_file("C0CHAN", "F0PIC", FileVariant::Thumb).await, image(THUMB_BYTES));
        long.shutdown().await;

        // Input it cannot carry, and a closed client, send nothing.
        let before = fake.seen().len();
        for (channel, file) in [("bad id", "F0PIC"), ("C0CHAN", "bad id"), ("", "F0PIC"), ("C0CHAN", "")] {
            assert_eq!(api.fetch_file(channel, file, FileVariant::Thumb).await, Err(ApiError::BadRequest), "{channel} {file}");
        }
        api.close();
        assert_eq!(api.fetch_file("C0CHAN", "F0PIC", FileVariant::Thumb).await, Err(ApiError::Closed));
        assert_eq!(fake.seen().len(), before);
        fake.shutdown().await;

        // A client closed while `files.info` is answered sends no GET.
        let slot: Arc<Mutex<Option<Arc<Api>>>> = Arc::new(Mutex::new(None));
        let (files, closer) = (Files::new(), slot.clone());
        let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
            if seen.path() == "/api/files.info" {
                if let Some(api) = closer.lock().unwrap().as_ref() {
                    api.close();
                }
            }
            files.reply(seen, port).unwrap_or(Reply::Drop)
        })
        .await;
        let api = Arc::new(new_api(&fake));
        *slot.lock().unwrap() = Some(api.clone());
        assert_eq!(api.fetch_file("C0CHAN", "F0PIC", FileVariant::Original).await, Err(ApiError::Closed));
        assert_eq!(downloads(&fake).len(), 0);
        *slot.lock().unwrap() = None;
        fake.shutdown().await;

        // A result prints its size, never its bytes.
        let shown = format!("{:?}", FileFetch::Image { mime: "image/png".into(), bytes: vec![0; 1 << 20] });
        assert_eq!(shown, "Image { mime: \"image/png\", bytes: 1048576 }");
    })
    .await;
}

#[tokio::test]
async fn a_thumbnail_falls_back_to_a_small_original_and_never_to_a_large_one() {
    bounded(async {
        // The first of `thumb_360`, `thumb_480` and `thumb_160` that Slack names, whatever order it lists them.
        for (keys, want) in [
            (&["thumb_160", "thumb_480", "thumb_360"][..], "thumb_360"),
            (&["thumb_160", "thumb_480"], "thumb_480"),
            (&["thumb_160"], "thumb_160"),
        ] {
            let fake = files_fake(Files { thumb_keys: keys.to_vec(), ..Files::new() }).await;
            assert_eq!(new_api(&fake).fetch_file("C0CHAN", "F0PIC", FileVariant::Thumb).await, image(THUMB_BYTES), "{want}");
            assert_eq!(downloads(&fake)[0].path(), Files::thumb_path(want));
            fake.shutdown().await;
        }

        // With none, a small original stands in; one byte over `MAX_THUMB` is `NoThumbnail` (not `TooLarge`: a thumbnail may
        // come later), unsent; the original variant takes
        // it whole.
        let fake = files_fake(Files { thumb_keys: vec![], original: vec![7; MAX_THUMB], ..Files::new() }).await;
        let got = new_api(&fake).fetch_file("C0CHAN", "F0PIC", FileVariant::Thumb).await;
        assert_eq!(got, Ok(FileFetch::Image { mime: "image/png".into(), bytes: vec![7; MAX_THUMB] }));
        assert_eq!(downloads(&fake)[0].path(), Files::DOWNLOAD);
        fake.shutdown().await;
        let fake = files_fake(Files { thumb_keys: vec![], original: vec![7; MAX_THUMB + 1], ..Files::new() }).await;
        let api = new_api(&fake);
        assert_eq!(api.fetch_file("C0CHAN", "F0PIC", FileVariant::Thumb).await, Ok(FileFetch::NoThumbnail));
        assert_eq!(downloads(&fake).len(), 0, "a large original is never fetched for a thumbnail");
        assert!(matches!(api.fetch_file("C0CHAN", "F0PIC", FileVariant::Original).await, Ok(FileFetch::Image { .. })));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn an_answer_that_is_not_an_image_is_refused() {
    bounded(async {
        for (served, want) in [
            (Some("text/html; charset=utf-8"), None),
            (Some("application/octet-stream"), None),
            (Some("image/"), None),
            (Some("image/svg+xml"), None),
            (Some("image/x-icon"), None),
            (Some("image/jpg"), None),
            (None, None),
            (Some("IMAGE/PNG; q=1"), Some("image/png")),
            (Some("image/jpeg"), Some("image/jpeg")),
            (Some("image/gif"), Some("image/gif")),
            (Some("image/webp"), Some("image/webp")),
        ] {
            for variant in [FileVariant::Thumb, FileVariant::Original] {
                let fake = files_fake(Files { content_type: served.map(String::from), ..Files::new() }).await;
                let got = new_api(&fake).fetch_file("C0CHAN", "F0PIC", variant).await;
                match want {
                    Some(mime) => assert!(matches!(got, Ok(FileFetch::Image { mime: ref m, .. }) if m == mime), "{served:?} {got:?}"),
                    None => assert_eq!(got, Ok(FileFetch::NotAnImage), "{served:?} {variant:?}"),
                }
                fake.shutdown().await;
            }
        }
        // An image with no bytes is no picture either.
        let fake = files_fake(Files { original: Vec::new(), ..Files::new() }).await;
        assert_eq!(new_api(&fake).fetch_file("C0CHAN", "F0PIC", FileVariant::Original).await, Err(ApiError::Malformed));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_fetch_is_bounded_while_it_streams() {
    bounded(async {
        // `files.info` claims ten bytes; the answer is counted as it comes, so one byte over the variant's bound is too
        // large and exactly the bound passes.
        for (variant, bound) in [(FileVariant::Thumb, MAX_THUMB), (FileVariant::Original, MAX_CHAT_FILE)] {
            for (len, want_ok) in [(bound + 1, false), (bound, true)] {
                let files = Files { size: Some(10), ..Files::new() };
                let files = if variant == FileVariant::Thumb { Files { thumb: vec![3; len], ..files } } else { Files { original: vec![3; len], ..files } };
                let fake = files_fake(files).await;
                let got = new_api(&fake).fetch_file("C0CHAN", "F0PIC", variant).await;
                let want = if want_ok { Ok(FileFetch::Image { mime: "image/png".into(), bytes: vec![3; len] }) } else { Ok(FileFetch::TooLarge) };
                assert_eq!(got, want, "{variant:?} {len}");
                fake.shutdown().await;
            }
        }
    })
    .await;
}

#[tokio::test]
async fn a_fetch_redirected_off_slack_sends_nothing_there() {
    bounded(async {
        // To a foreign host: refused before any request there, so the token never reaches it.
        for host in ["other.example", "evilslack.com"] {
            for variant in [FileVariant::Thumb, FileVariant::Original] {
                let fake = files_fake(Files { redirect_to: Some(host.into()), ..Files::new() }).await;
                let got = new_api(&fake).fetch_file("C0CHAN", "F0PIC", variant).await;
                assert_eq!(got, Err(ApiError::Http(HttpError::Destination)), "{host} {variant:?}");
                for s in fake.seen() {
                    assert!(s.host.starts_with("slack.com:") || s.host.starts_with("files.slack.com:"), "{}", s.host);
                }
                fake.shutdown().await;
            }
        }
        // Within Slack the redirect is followed and the bearer rides it.
        let fake = files_fake(Files { redirect_to: Some("files.slack.com".into()), ..Files::new() }).await;
        assert_eq!(new_api(&fake).fetch_file("C0CHAN", "F0PIC", FileVariant::Original).await, image(ORIGINAL_BYTES));
        let seen = fake.seen();
        assert_eq!(seen.last().unwrap().path(), Files::PRIVATE);
        assert_eq!(seen.last().unwrap().authorization, Some(format!("Bearer {TOKEN}")));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_fetch_shares_the_transfer_permits_and_a_rate_limit_waits_the_next_read() {
    bounded(async {
        // Two originals that never answer hold both permits: a third fetch and a post are `busy` at once, sending
        // nothing; a dropped fetch frees its permit.
        let files = Files::new();
        let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
            if seen.path() == Files::DOWNLOAD {
                return Reply::Hang;
            }
            files.reply(seen, port).unwrap_or(Reply::Drop)
        })
        .await;
        let api = Arc::new(new_api(&fake));
        let hang = |api: Arc<Api>| tokio::spawn(async move { api.fetch_file("C0CHAN", "F0PIC", FileVariant::Original).await });
        let (first, second) = (hang(api.clone()), hang(api.clone()));
        eventually("two fetches under way", || downloads(&fake).len() == 2).await;
        let before = fake.seen().len();
        let start = Instant::now();
        assert_eq!(api.fetch_file("C0CHAN", "F0PIC", FileVariant::Thumb).await, Err(ApiError::Http(HttpError::Busy)));
        assert_eq!(api.transfer().err(), Some(ApiError::Http(HttpError::Busy)), "a post's transfer");
        assert!(start.elapsed() < Duration::from_secs(1), "busy at once");
        assert_eq!(fake.seen().len(), before, "neither sent anything");
        first.abort();
        let _ = first.await;
        assert_eq!(api.fetch_file("C0CHAN", "F0PIC", FileVariant::Thumb).await, image(THUMB_BYTES));
        second.abort();
        let _ = second.await;
        fake.shutdown().await;

        // A 429 on the file's URL sets the wait, so the next read is refused without a request.
        let files = Files::new();
        let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
            if seen.path() == Files::DOWNLOAD {
                return status(429, &[("Retry-After", "5")], &json!({}));
            }
            files.reply(seen, port).unwrap_or(Reply::Drop)
        })
        .await;
        let api = new_api(&fake);
        let limited = Err(ApiError::Limited { retry_after: Duration::from_secs(5), assumed: false });
        assert_eq!(api.fetch_file("C0CHAN", "F0PIC", FileVariant::Original).await, limited);
        let before = fake.seen().len();
        assert!(matches!(api.fetch_file("C0CHAN", "F0PIC", FileVariant::Thumb).await, Err(ApiError::Limited { .. })));
        assert_eq!(fake.seen().len(), before);
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn each_variant_is_read_under_its_own_deadline() {
    bounded(async {
        // A thumbnail's GET is a read (15 s) and an original's a transfer (60 s): the one that is short times out.
        let files = Files::new();
        let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
            if seen.path() == Files::DOWNLOAD || seen.path() == Files::thumb_path("thumb_360") {
                return Reply::Hang;
            }
            files.reply(seen, port).unwrap_or(Reply::Drop)
        })
        .await;
        let (short, long) = (Duration::from_millis(300), Duration::from_secs(40));
        let thumbs = new_api(&fake).with_deadlines(short, long, long);
        let originals = new_api(&fake).with_transfer_deadline(short);
        for (api, variant) in [(&thumbs, FileVariant::Thumb), (&originals, FileVariant::Original)] {
            let start = Instant::now();
            assert_eq!(api.fetch_file("C0CHAN", "F0PIC", variant).await, Err(ApiError::Http(HttpError::Timeout)), "{variant:?}");
            assert!(start.elapsed() < Duration::from_secs(10), "{variant:?} ended at its own deadline");
        }
        fake.shutdown().await;
    })
    .await;
}

/// The keys of a request's JSON body, sorted.
fn keys(seen: &Seen) -> Vec<String> {
    let mut keys: Vec<String> = seen.json().unwrap().as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

#[tokio::test]
async fn a_persons_post_goes_with_their_token_and_names_nothing_of_the_app() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            auth(seen).unwrap_or(Reply::json(&json!({"ok": true, "ts": "1700000700.000100"})))
        })
        .await;
        let api = ready(&fake).await;
        let posts = |fake: &FakeSlack| -> Vec<Seen> {
            fake.seen().into_iter().filter(|s| s.path() == "/api/chat.postMessage").collect()
        };

        let posted = api.post("C0CHAN", None, PostKind::Person, "tester Test", "a <b> & c").await;
        assert!(matches!(posted, Ok(PostOutcome::Posted(_))), "{posted:?}");
        let thread = api.post("C0CHAN", Some("1700000100.000000"), PostKind::Person, "tester Test", "re").await;
        assert!(matches!(thread, Ok(PostOutcome::Posted(_))), "{thread:?}");
        // The name is neither checked nor sent for a person's post.
        let unnamed = api.post("C0CHAN", None, PostKind::Person, "", "no name").await;
        assert!(matches!(unnamed, Ok(PostOutcome::Posted(_))), "{unnamed:?}");

        let sent = posts(&fake);
        assert_eq!(sent.len(), 3);
        for s in &sent {
            assert_eq!(s.authorization.as_deref(), Some(&*format!("Bearer {PERSON}")), "the person's bearer");
        }
        assert_eq!(keys(&sent[0]), ["channel", "text"], "no username, metadata, icon or label");
        assert_eq!(sent[0].json().unwrap()["text"], "a &lt;b&gt; &amp; c", "the text is Slack's, escaped");
        assert_eq!(keys(&sent[1]), ["channel", "text", "thread_ts"]);
        assert_eq!(sent[1].json().unwrap()["thread_ts"], "1700000100.000000");
        assert_eq!(keys(&sent[2]), ["channel", "text"]);

        // A row's post in the same client is unchanged: the bot's bearer, its author and its metadata.
        let row = api.post("C0CHAN", None, PostKind::Row, "build-7", "hi").await;
        assert!(matches!(row, Ok(PostOutcome::Posted(_))), "{row:?}");
        let row = posts(&fake).pop().unwrap();
        assert_eq!(row.authorization.as_deref(), Some(&*format!("Bearer {TOKEN}")));
        assert_eq!(keys(&row), ["channel", "metadata", "text", "username"]);
        assert_eq!(row.json().unwrap()["username"], "build-7 (row via Ship of Tools)");
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn with_no_person_token_a_persons_post_is_refused_unsent_and_a_refused_token_is_forgotten() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            match seen.json().unwrap_or_default()["channel"].as_str().unwrap_or("") {
                "C0NOTIN" => Reply::json(&json!({"ok": false, "error": "not_in_channel"})),
                "C0REVOKED" => Reply::json(&json!({"ok": false, "error": "token_revoked"})),
                "C0SCOPE" => Reply::json(&json!({"ok": false, "error": "missing_scope"})),
                "C0GONE" => Reply::json(&json!({"ok": false, "error": "channel_not_found"})),
                _ => Reply::json(&json!({"ok": true, "ts": "1700000700.000100"})),
            }
        })
        .await;
        let posts = |fake: &FakeSlack| calls(fake, "chat.postMessage");

        // None held: nothing is sent, and the bot's token is not used in its place.
        let bare = new_api_with(&fake, None);
        let got = bare.post("C0CHAN", None, PostKind::Person, "tester", "hi").await;
        assert_eq!(got, Err(ApiError::NoPersonToken));
        // So is a person's share, and the bot's token is not used for it either.
        let file = bare.post_file(bare.transfer().unwrap(), "C0CHAN", None, PostKind::Person, "tester", "a.png", PNG.to_vec(), None).await;
        assert_eq!(file, Err(ApiError::NoPersonToken));
        assert_eq!((posts(&fake), steps(&fake).len()), (0, 0), "no request of any kind");
        assert_eq!(ApiError::NoPersonToken.code(), "no_person_token");
        // A row's post is not the person's: it still goes out.
        assert!(matches!(bare.post("C0CHAN", None, PostKind::Row, "build-7", "hi").await, Ok(PostOutcome::Posted(_))));

        // Not being in the channel is the person's to fix: the token is kept.
        let api = new_api(&fake);
        // The token is held when the next person's post is made (sent, whatever Slack answers), not refused unsent.
        async fn held(api: &Api) -> bool {
            api.post("C0CHAN", None, PostKind::Person, "tester", "hi").await.is_ok()
        }
        let got = api.post("C0NOTIN", None, PostKind::Person, "tester", "hi").await;
        assert_eq!(got, Ok(PostOutcome::Refused("not_in_channel".into())));
        assert!(held(&api).await, "kept");
        // So is a channel that is not found.
        let got = api.post("C0GONE", None, PostKind::Person, "tester", "hi").await;
        assert_eq!(got, Ok(PostOutcome::Refused("channel_not_found".into())));
        assert!(held(&api).await, "kept");

        // A refusal that names the token or its scope forgets it, and the answer is still the refusal. The next post
        // is refused unsent.
        for (channel, code) in [("C0REVOKED", "token_revoked"), ("C0SCOPE", "missing_scope")] {
            let api = new_api(&fake);
            let got = api.post(channel, None, PostKind::Person, "tester", "hi").await;
            assert_eq!(got, Ok(PostOutcome::Refused(code.into())));
            let before = posts(&fake);
            let again = api.post("C0CHAN", None, PostKind::Person, "tester", "hi").await;
            assert_eq!(again, Err(ApiError::NoPersonToken), "{code}");
            assert_eq!(posts(&fake), before, "{code}: nothing sent");
        }

        // A row's refused post leaves the person's token alone.
        let api = new_api(&fake);
        let got = api.post("C0REVOKED", None, PostKind::Row, "build-7", "hi").await;
        assert_eq!(got, Ok(PostOutcome::Refused("token_revoked".into())));
        // (The bot's token refused also closes the client, guarantee 22, so the person's token is read, not posted with.)
        assert!(api.holds_person_token(), "a row's refusal is the bot token's");
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_persons_file_share_uses_their_token_and_carries_the_comment_alone() {
    bounded(async {
        let upload = Upload::new();
        let path = upload.path().to_string();
        let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
            // The upload's first step for `ask-refused.png`, and the share into `C0SCOPE`, refuse the token.
            if seen.param("filename").as_deref() == Some("ask-refused.png") {
                return Reply::json(&json!({"ok": false, "error": "invalid_auth"}));
            }
            if seen.json().is_some_and(|b| b["channel_id"] == "C0SCOPE") {
                return Reply::json(&json!({"ok": false, "error": "missing_scope"}));
            }
            upload.reply(seen, port).unwrap_or(Reply::Drop)
        })
        .await;
        let api = new_api(&fake);
        async fn share(api: &Api, channel: &str, thread: Option<&str>, file: &str, comment: Option<&str>) -> Result<FileOutcome, ApiError> {
            api.post_file(api.transfer().unwrap(), channel, thread, PostKind::Person, "", file, PNG.to_vec(), comment).await
        }
        let posted = share(&api, "C0CHAN", Some("1700000100.000000"), "fig.png", Some("the <!channel> fit")).await;
        assert!(matches!(posted, Ok(FileOutcome::Posted { .. })), "{posted:?}");
        let seen = fake.seen();
        let person = format!("Bearer {PERSON}");
        assert_eq!(seen.iter().map(Seen::path).collect::<Vec<_>>(), [ASK, path.as_str(), SHARE]);
        assert_eq!(seen[0].authorization.as_deref(), Some(&*person), "step 1 under the person's token");
        assert_eq!(seen[1].authorization, None, "the upload carries no authorization");
        assert_eq!(seen[2].authorization.as_deref(), Some(&*person), "the share under the person's token");
        let body = seen[2].json().unwrap();
        assert_eq!(keys(&seen[2]), ["channel_id", "files", "initial_comment", "thread_ts"]);
        assert_eq!(body["initial_comment"], "the &lt;!channel&gt; fit", "the comment alone, escaped, with no label");

        // With no comment there is no `initial_comment`.
        let posted = share(&api, "C0CHAN", None, "fig.png", None).await;
        assert!(matches!(posted, Ok(FileOutcome::Posted { .. })), "{posted:?}");
        assert_eq!(keys(&fake.seen().pop().unwrap()), ["channel_id", "files"]);

        // A refusal of either authorized step that names the token forgets it; the next share is refused unsent.
        for (channel, file, code) in [("C0CHAN", "ask-refused.png", "invalid_auth"), ("C0SCOPE", "fig.png", "missing_scope")] {
            let api = new_api(&fake);
            assert_eq!(share(&api, channel, None, file, None).await, Ok(FileOutcome::Refused(code.into())));
            let before = fake.seen().len();
            assert_eq!(share(&api, "C0CHAN", None, "fig.png", None).await, Err(ApiError::NoPersonToken), "{code}");
            assert_eq!(fake.seen().len(), before, "{code}: nothing sent");
        }
        fake.shutdown().await;
    })
    .await;
}

/// PS8: the bot's app is `bots.info`'s `bot.app_id` for `auth.test`'s `bot_id`, asked under the bot's token and kept in
/// the identity; any failure (Slack's refusal, a token's included, no app, an app that is no id, a transport failure)
/// leaves it unknown and ends nothing, so the check it serves fails open; a bot with no `bot_id` is not asked.
#[tokio::test]
async fn the_bots_app_is_read_at_start_and_a_failure_leaves_it_unknown() {
    bounded(async {
        let answer = Arc::new(Mutex::new(json!({"ok": true, "bot": {"id": OWN_BOT, "app_id": "A0OWN"}})));
        let a = answer.clone();
        let fake = FakeSlack::start(Identity::Trusted, move |seen, _| {
            auth(seen).unwrap_or_else(|| match seen.path().as_str() {
                "/api/bots.info" => {
                    let v = a.lock().unwrap().clone();
                    if v.is_null() { Reply::Drop } else { Reply::json(&v) }
                }
                _ => Reply::json(&json!({"ok": true, "channels": [], "response_metadata": {"next_cursor": ""}})),
            })
        })
        .await;
        let api = ready(&fake).await;
        assert_eq!(api.bot_app().await, Ok(crate::api::BotApp { id: "A0OWN".into(), name: None }));
        let asked: Vec<Seen> = fake.seen().into_iter().filter(|s| s.path() == "/api/bots.info").collect();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].param("bot").as_deref(), Some(OWN_BOT));
        assert_eq!(asked[0].authorization.as_deref(), Some(&*format!("Bearer {TOKEN}")));
        for (reply, want) in [
            (json!({"ok": false, "error": "invalid_auth"}), ApiError::Slack("invalid_auth".into())),
            (json!({"ok": false, "error": "missing_scope"}), ApiError::Slack("missing_scope".into())),
            (json!({"ok": true, "bot": {"id": OWN_BOT}}), ApiError::Malformed),
            (json!({"ok": true, "bot": {"id": OWN_BOT, "app_id": "not an id"}}), ApiError::Malformed),
        ] {
            *answer.lock().unwrap() = reply;
            assert_eq!(api.bot_app().await, Err(want));
        }
        // The name Slack gives the bot, read as a workspace's name is: cleaned, and none when it is not a usable string.
        let long = "x".repeat(crate::normalize::MAX_NAME + 1);
        for (name, want) in [
            (json!("Ship of Tools (lab-user)"), Some("Ship of Tools (lab-user)")),
            (json!("  Ship of Tools\u{7} (lab-user) "), Some("Ship of Tools (lab-user)")),
            (json!(""), None),
            (json!(7), None),
            (json!(long), None),
        ] {
            *answer.lock().unwrap() = json!({"ok": true, "bot": {"id": OWN_BOT, "app_id": "A0OWN", "name": name}});
            assert_eq!(api.bot_app().await, Ok(crate::api::BotApp { id: "A0OWN".into(), name: want.map(String::from) }), "{name}");
        }
        *answer.lock().unwrap() = Value::Null;
        assert!(matches!(api.bot_app().await, Err(ApiError::Http(_))));
        // A refused `bots.info` ended nothing: the client still reads.
        assert!(api.channels().await.is_ok(), "a failed bots.info closed the client");
        // With no bot id there is nothing to ask.
        let botless = Api::new(fake.http(), Secret::new(TOKEN), WS)
            .unwrap()
            .with_base(fake.url("slack.com", "/api/"))
            .with_identity(crate::api::Identity { user_id: OWN_USER.into(), bot_id: None, app_id: None });
        let before = calls(&fake, "bots.info");
        assert_eq!(botless.bot_app().await, Err(ApiError::NoIdentity));
        assert_eq!(calls(&fake, "bots.info"), before, "a bot with no id was asked about");
        fake.shutdown().await;
    })
    .await;
}

/// The setup's question of a connect's bot token: one `bots.info` for the bot id given, under that token, read as the
/// service's own is (the app and its bot's name, `Malformed` with no app id, Slack's refusal as its code), and an id that
/// is no id asks nothing.
#[tokio::test]
async fn a_connects_bot_token_is_asked_its_app_once_under_its_own_token() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, move |seen, _| match (seen.path().as_str(), seen.param("bot").as_deref()) {
            ("/api/bots.info", Some("B0NEW")) => Reply::json(&json!({"ok": true, "bot": {"id": "B0NEW", "app_id": "A0NEW", "name": "Ship of Tools (lab-user)"}})),
            ("/api/bots.info", Some("B0BARE")) => Reply::json(&json!({"ok": true, "bot": {"id": "B0BARE"}})),
            _ => Reply::json(&json!({"ok": false, "error": "bot_not_found"})),
        })
        .await;
        let (base, http) = (fake.url("slack.com", "/api/"), fake.http());
        let ask = |bot: &'static str| crate::api::bot_app_of_for_test(&http, Secret::new(TOKEN), bot, base.clone());
        assert_eq!(ask("B0NEW").await, Ok(crate::api::BotApp { id: "A0NEW".into(), name: Some("Ship of Tools (lab-user)".into()) }));
        let asked: Vec<Seen> = fake.seen().into_iter().filter(|s| s.path() == "/api/bots.info").collect();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].authorization.as_deref(), Some(&*format!("Bearer {TOKEN}")));
        assert_eq!(ask("B0BARE").await, Err(ApiError::Malformed));
        assert_eq!(ask("B0GONE").await, Err(ApiError::Slack("bot_not_found".into())));
        let before = calls(&fake, "bots.info");
        assert_eq!(ask("not an id").await, Err(ApiError::BadRequest));
        assert_eq!(calls(&fake, "bots.info"), before, "a bot id that is no id was asked about");
        fake.shutdown().await;
    })
    .await;
}

/// L5 rule 3: `auth.test`'s `team` is the workspace's name, kept by the client and answered by `identify`; absent, not a
/// string, empty, past `MAX_NAME`, or nothing but a control character reads as no name.
#[tokio::test]
async fn the_workspaces_name_comes_from_auth_test_and_a_bad_one_is_none() {
    bounded(async {
        let long = "n".repeat(crate::normalize::MAX_NAME + 1);
        let fake = FakeSlack::start(Identity::Trusted, move |seen, _| {
            let team = match seen.path().split('/').nth(2).unwrap_or_default() {
                "named" => json!("Alpha Lab"),
                "spaced" => json!("  Alpha\u{7}Lab "),
                "empty" => json!(""),
                "control" => json!("\u{1}"),
                "long" => json!(long),
                "number" => json!(5),
                _ => Value::Null,
            };
            let mut v = json!({"ok": true, "team_id": WS, "user_id": OWN_USER, "bot_id": OWN_BOT});
            if !team.is_null() {
                v["team"] = team;
            }
            Reply::json(&v)
        })
        .await;
        for (mode, name) in [
            ("named", Some("Alpha Lab")),
            ("spaced", Some("AlphaLab")),
            ("empty", None),
            ("control", None),
            ("long", None),
            ("number", None),
            ("absent", None),
        ] {
            let base = fake.url("slack.com", &format!("/api/{mode}/"));
            let api = Api::new(fake.http(), Secret::new(TOKEN), WS).unwrap().with_base(base.clone());
            assert_eq!(api.workspace_name(), None, "no name before auth.test");
            api.auth_test().await.unwrap();
            assert_eq!(api.workspace_name().as_deref(), name, "{mode}");
            let id = identify_at(&fake.http(), Secret::new(TOKEN), base).await.unwrap();
            assert_eq!(id.name.as_deref(), name, "identify, {mode}");
            assert_eq!(api.workspace_tag().as_deref(), name, "no address: the name stands in, {mode}");
        }
        fake.shutdown().await;
    })
    .await;
}

/// The workspace's short name is the first label of its Slack address; an address that is not under `slack.com`, not
/// https or with an odd label gives the name's place to the name.
#[test]
fn the_workspace_tag_is_the_first_label_of_its_slack_address() {
    for (url, team, tag) in [
        ("https://acme.slack.com/", Some("Acme Lab"), Some("acme")),
        ("https://acme-two.slack.com", None, Some("acme-two")),
        ("https://acme.enterprise.slack.com/x", None, Some("acme")),
        ("http://acme.slack.com/", Some("Acme Lab"), Some("Acme Lab")),
        ("https://acme.example.com/", Some("Acme Lab"), Some("Acme Lab")),
        ("https://a%20b.slack.com/", None, None),
        ("https://.slack.com/", None, None),
    ] {
        let mut v = serde_json::json!({ "url": url });
        if let Some(t) = team {
            v["team"] = t.into();
        }
        assert_eq!(tag_of(&v).as_deref(), tag, "{url}");
    }
    assert_eq!(tag_of(&serde_json::json!({ "team": "Acme Lab" })).as_deref(), Some("Acme Lab"), "no address");
}

/// The Slack id of the user token's account is kept only when the start's `auth.test` under that token
/// answers this workspace with a valid `user_id`, and it goes with the token: a token of another workspace, one Slack
/// refuses and one a refused post made the client forget keep no id; an answer that names none, or none for this
/// workspace, keeps the token and no id; no token at all has none.
#[tokio::test]
async fn the_user_tokens_account_is_kept_with_the_token_and_only_then() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            let mode = seen.path().split('/').nth(2).unwrap_or_default().to_string();
            if seen.path().ends_with("/chat.postMessage") {
                return Reply::json(&json!({"ok": false, "error": "token_revoked"}));
            }
            let id = |team: &str, user: &str| Reply::json(&json!({"ok": true, "team_id": team, "user_id": user}));
            match mode.as_str() {
                "ok" => id(WS, "U0ADA"),
                "nouser" => Reply::json(&json!({"ok": true, "team_id": WS})),
                "baduser" => id(WS, "not an id"),
                "noteam" => Reply::json(&json!({"ok": true, "user_id": "U0ADA"})),
                "other" => id("T0OTHER", "U0ADA"),
                "refused" => Reply::json(&json!({"ok": false, "error": "invalid_auth"})),
                _ => status(500, &[], &json!({})),
            }
        })
        .await;
        let api_in = |mode: &str, person: Option<&str>| {
            Api::new(fake.http(), Secret::new(TOKEN), WS).unwrap().with_person(person.map(Secret::new)).with_base(fake.url("slack.com", &format!("/api/{mode}/")))
        };
        for (mode, dropped, held, id) in [
            ("ok", None, true, Some("U0ADA")),
            ("nouser", None, true, None),
            ("baduser", None, true, None),
            ("noteam", None, true, None),
            ("down", None, true, None),
            ("other", Some("user_token_other_workspace"), false, None),
            ("refused", Some("user_token_refused"), false, None),
        ] {
            let api = api_in(mode, Some(PERSON));
            assert_eq!(api.person_id(), None, "{mode}: nothing is kept before the check");
            assert_eq!(api.check_person().await, dropped, "{mode}");
            assert_eq!((api.holds_person_token(), api.person_id().as_deref()), (held, id), "{mode}");
        }
        // No token: the check asks nothing and there is no id.
        let bare = api_in("ok", None);
        assert_eq!(bare.check_person().await, None);
        assert_eq!(bare.person_id(), None);
        // The token a refused post made the client forget takes its id with it; a row's refused post leaves both.
        let api = api_in("ok", Some(PERSON));
        api.check_person().await;
        assert_eq!(api.post("C0CHAN", None, PostKind::Person, "tester", "hi").await, Ok(PostOutcome::Refused("token_revoked".into())));
        assert_eq!((api.holds_person_token(), api.person_id()), (false, None));
        fake.shutdown().await;
    })
    .await;
}

use crate::api::DeleteOutcome;

/// A delete goes out once under the bot's token, never the person's, whatever Slack answers, and each answer is the
/// outcome it says: a message the app did not post is Slack's refusal and stays, one already gone is its own outcome.
#[tokio::test]
async fn a_delete_is_made_once_under_the_bots_token_and_each_answer_has_its_outcome() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            if let Some(r) = auth(seen) {
                return r;
            }
            match seen.json().unwrap_or_default()["channel"].as_str().unwrap_or("") {
                "C0OK" => Reply::json(&json!({"ok": true, "channel": "C0OK", "ts": "1700000700.000100"})),
                "C0GONE" => Reply::json(&json!({"ok": false, "error": "message_not_found"})),
                "C0NOT" => Reply::json(&json!({"ok": false, "error": "cant_delete_message"})),
                "C0FOUR" => status(403, &[], &json!({})),
                "C0RATE" => status(429, &[], &json!({})),
                "C0RATE7" => status(429, &[("Retry-After", "7")], &json!({})),
                "C0INT" => Reply::json(&json!({"ok": false, "error": "internal_error"})),
                "C0FIVE" => status(503, &[], &json!({})),
                "C0NOOK" => Reply::json(&json!({"ts": "1700000700.000100"})),
                _ => Reply::Drop,
            }
        })
        .await;
        let api = ready(&fake).await;
        let delete = |c: &'static str| api.delete_message(c, "1700000700.000100");

        assert_eq!(delete("C0OK").await, Ok(DeleteOutcome::Deleted));
        let sent = fake.seen().into_iter().find(|s| s.path() == "/api/chat.delete").unwrap();
        assert_eq!(sent.method, "POST");
        assert_eq!(sent.authorization.as_deref(), Some(&*format!("Bearer {TOKEN}")), "the bot's token, never the person's");
        assert_eq!(sent.json().unwrap(), json!({"channel": "C0OK", "ts": "1700000700.000100"}));

        assert_eq!(delete("C0GONE").await, Ok(DeleteOutcome::Gone));
        assert_eq!(delete("C0NOT").await, Ok(DeleteOutcome::Refused("cant_delete_message".into())));
        assert_eq!(delete("C0FOUR").await, Ok(DeleteOutcome::Refused("http_403".into())));
        // A limit carries Slack's wait when its answer has one, and a guess flagged as one when it has none.
        assert_eq!(delete("C0RATE").await, Ok(DeleteOutcome::Limited { retry_after: ASSUMED_RETRY, assumed: true }));
        assert_eq!(delete("C0RATE7").await, Ok(DeleteOutcome::Limited { retry_after: Duration::from_secs(7), assumed: false }));
        for unconfirmed in ["C0INT", "C0FIVE", "C0NOOK", "C0DROP"] {
            assert_eq!(delete(unconfirmed).await, Ok(DeleteOutcome::Unknown), "{unconfirmed}");
        }
        // Ten deletes, ten requests: none repeated, and the person's token carried none of them.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(calls(&fake, "chat.delete"), 10);
        assert!(fake.seen().iter().all(|s| s.authorization.as_deref() != Some(&*format!("Bearer {PERSON}"))));

        // Input a delete cannot carry is refused before sending.
        let before = fake.seen().len();
        for (channel, ts) in [("C0OK", "1700000700.00010"), ("C0OK", "17.5"), ("C0OK", ""), ("bad id", "1700000700.000100")] {
            assert!(api.delete_message(channel, ts).await.is_err(), "{channel} {ts}");
        }
        assert_eq!(fake.seen().len(), before);

        // A client whose service has begun to stop sends nothing more (chat guarantee 22), whatever the delete's input.
        api.close();
        assert_eq!(delete("C0OK").await, Err(ApiError::Closed));
        assert_eq!(fake.seen().len(), before);
        fake.shutdown().await;
    })
    .await;
}

/// A delete Slack refuses for the bot token ends the service as every bot-token call does (guarantee 22): the refusal is
/// the delete's answer, and nothing more is sent after it.
#[tokio::test]
async fn a_delete_the_bot_token_refuses_ends_the_service() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| {
            auth(seen).unwrap_or_else(|| Reply::json(&json!({"ok": false, "error": "invalid_auth"})))
        })
        .await;
        let api = ready(&fake).await;
        assert_eq!(api.delete_message("C0OK", "1700000700.000100").await, Ok(DeleteOutcome::Refused("invalid_auth".into())));
        let sent = fake.seen().len();
        assert_eq!(api.delete_message("C0OK", "1700000700.000100").await, Err(ApiError::Closed));
        assert_eq!(fake.seen().len(), sent, "nothing is sent once the service has ended");
        fake.shutdown().await;
    })
    .await;
}
