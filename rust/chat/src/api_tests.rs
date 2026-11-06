//! Behavioral tests of `api` against the scripted fake Slack over real TLS. Every id, name and timestamp is synthetic.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sot_protocol::chat::Origin;

use crate::api::{
    Api, ApiError, Bounds, Continuation, HistoryQuery, PostKind, PostOutcome, ASSUMED_RETRY,
    MAX_CURSOR, MAX_NAMES, MAX_RETRY,
};
use crate::fake_slack::{eventually, FakeSlack, Identity, Reply, Seen};
use crate::http::{HttpError, Secret};
use crate::normalize::MAX_TEXT;

const TOKEN: &str = "sentinel-bot-token-not-a-credential";
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

fn new_api(fake: &FakeSlack) -> Api {
    Api::new(fake.http(), Secret::new(TOKEN), WS)
        .unwrap()
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
        let post = |c: &'static str| api.post(c, None, PostKind::Person, "Ada Test", "hello");

        assert!(matches!(post("C0OK").await, Ok(PostOutcome::Posted(ts)) if ts.as_str() == "1700000700.000100"));
        let sent = fake.seen().into_iter().find(|s| s.path() == "/api/chat.postMessage").unwrap();
        assert_eq!(sent.method, "POST");
        assert_eq!(sent.authorization.as_deref(), Some(&*format!("Bearer {TOKEN}")));
        let body = sent.json().unwrap();
        assert_eq!(body["text"], "hello", "no text prefix of any form");
        assert_eq!(body["username"], "Ada Test (person via Ship of Tools)");
        assert_eq!(
            body["metadata"],
            json!({"event_type": "sot_chat_post", "event_payload": {"kind": "person", "name": "Ada Test"}})
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
            let r = api.post("C0OK", thread, PostKind::Person, name, text).await;
            assert!(r.is_err(), "{name:?} {text:.5}");
        }
        assert!(api.post("bad id", None, PostKind::Person, "Ada", "hi").await.is_err());
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
                    // As Slack stores it: the app's bot id and user, the custom username, the metadata.
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
        assert_eq!(
            (person.origin, person.author.name.as_str()),
            (Origin::Drawer, "Ada Test")
        );
        assert_eq!(person.text, "from a person");
        let row = by_ts(&posted[1]);
        assert_eq!(
            (row.origin, row.author.name.as_str()),
            (Origin::Row, "build-7")
        );
        // Every request carried the bot authorization and nothing else did.
        assert!(fake
            .seen()
            .iter()
            .all(|s| s.authorization.as_deref() == Some(&*format!("Bearer {TOKEN}"))));
        fake.shutdown().await;
    })
    .await;
}
