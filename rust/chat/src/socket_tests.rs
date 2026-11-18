//! Behavioral tests of `socket` against the scripted fake Slack over real TLS and the selected library's websocket
//! server handshake. Every id, name, ticket and token is synthetic; every wait is bounded.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use sot_protocol::chat::Origin;
use tokio::time::Instant;

use crate::api::{Api, Identity as Bot};
use crate::fake_slack::{eventually, FakeSlack, Flood, Identity, Reply, Seen, WsPeer};
use crate::http::{HttpError, Secret};
use crate::normalize::Normalized;
use crate::socket::{
    check_app_token_at, GapCause, Probe, Socket, SocketError, Timing, MAX_MESSAGE, MAX_PENDING, MAX_PENDING_BYTES,
};

const APP: &str = "sentinel-app-token-not-a-credential";
const WS: &str = "T0TEAM";
const TICKET: &str = "sentinel-ticket-not-a-secret";
const SEC: Duration = Duration::from_secs(2);

/// Every test body runs under one bound: a hung fixture fails here instead of waiting.
async fn bounded<T>(body: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), body)
        .await
        .expect("the test exceeded its bound")
}

/// Runs a test that holds a link or a measured duration against a bound. A host that pauses the whole process for about
/// as long as the bound's margin (a loaded or descheduled runner) lengthens every measured interval and makes the client
/// see a silent peer, which the rule rightly retires. Each attempt runs beside a ticker, and an attempt that failed
/// while the ticker saw a pause of `margin` is run again. An attempt that fails on a host that did not pause fails the
/// test, naming the longest tick seen, and so does a host that paused every attempt.
async fn steady<F, Fut>(margin: Duration, attempt: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let mut longest_ever = 0;
    for _ in 0..5 {
        let longest = Arc::new(AtomicUsize::new(0));
        let seen = longest.clone();
        let ticker = tokio::spawn(async move {
            loop {
                let began = Instant::now();
                tokio::time::sleep(Duration::from_millis(20)).await;
                seen.fetch_max(began.elapsed().as_millis() as usize, Ordering::SeqCst);
            }
        });
        let result = attempt().await;
        ticker.abort();
        let longest = longest.load(Ordering::SeqCst);
        longest_ever = longest_ever.max(longest);
        let paused = longest as u128 >= margin.as_millis();
        match result {
            Ok(()) => return,
            Err(why) if !paused => panic!(
                "{why} (longest tick {longest} ms, a pause counts from {} ms)",
                margin.as_millis()
            ),
            Err(_) => {}
        }
    }
    panic!(
        "the host paused during every attempt (longest tick {longest_ever} ms, a pause counts from {} ms)",
        margin.as_millis()
    );
}

/// Short waits: pacing of tens of milliseconds, a close bound of 300 ms.
fn timing() -> Timing {
    Timing {
        open: Duration::from_secs(5),
        close: Duration::from_millis(300),
        write: SEC,
        idle: Duration::from_secs(60),
        ping: Duration::from_secs(30),
        backoff_first: Duration::from_millis(50),
        backoff_max: Duration::from_millis(200),
        stop: Duration::from_secs(3),
    }
}

fn start(fake: &FakeSlack, timing: Timing) -> Socket {
    start_of(fake, timing, None)
}

/// `start` for a bot whose app is `app` (`None`: unknown, so no frame's app is checked).
fn start_of(fake: &FakeSlack, timing: Timing, app: Option<&str>) -> Socket {
    Socket::start_for_test(
        fake.http(),
        Secret::new(APP),
        Bot {
            user_id: "U0BOT".into(),
            bot_id: Some("B0BOT".into()),
            app_id: app.map(String::from),
        },
        WS,
        fake.url("slack.com", "/api/"),
        timing,
    )
    .expect("a socket")
}

/// A fake whose `apps.connections.open` names the websocket host and whose upgrades run `script(index, peer)`, the
/// index counting connections from zero. `other` answers any other request first.
async fn fake_with<F, Fut>(
    other: impl Fn(&Seen) -> Option<Reply> + Send + Sync + 'static,
    script: F,
) -> FakeSlack
where
    F: Fn(usize, WsPeer) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let script = Arc::new(script);
    let index = Arc::new(AtomicUsize::new(0));
    FakeSlack::start(Identity::Trusted, move |seen, port| {
        if let Some(reply) = other(seen) {
            return reply;
        }
        if seen.path() == "/api/apps.connections.open" {
            let url = format!("wss://wss-primary.slack.com:{port}/link/?ticket={TICKET}");
            return Reply::json(&json!({"ok": true, "url": url}));
        }
        if seen.upgrade {
            let i = index.fetch_add(1, Ordering::SeqCst);
            let script = script.clone();
            return Reply::socket(move |peer| {
                let script = script.clone();
                async move { script(i, peer).await }
            });
        }
        Reply::json(&json!({"ok": false, "error": "unexpected"}))
    })
    .await
}

async fn fake<F, Fut>(script: F) -> FakeSlack
where
    F: Fn(usize, WsPeer) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    fake_with(|_| None, script).await
}

fn hello() -> Value {
    json!({"type": "hello", "num_connections": 1})
}

fn disconnect(reason: &str) -> Value {
    json!({"type": "disconnect", "reason": reason})
}

/// A message envelope with the given extra size in an unrelated field.
fn envelope(id: &str, n: usize, text: &str, pad: usize) -> Value {
    json!({
        "type": "events_api",
        "envelope_id": id,
        "payload": {
            "team_id": WS,
            "pad": "x".repeat(pad),
            "event": {
                "type": "message",
                "channel": "C0CHAN",
                "user": "U0MEMBER",
                "ts": format!("17000{n:05}.000100"),
                "text": text,
            },
        },
    })
}

async fn expect_ack(peer: &mut WsPeer, id: &str) -> bool {
    peer.recv(SEC).await == Some(json!({ "envelope_id": id }))
}

#[tokio::test]
async fn socket_ack_precedes_slow_api_and_dispatch() {
    bounded(async {
        let acked = Arc::new(Mutex::new(Vec::<String>::new()));
        let missed = Arc::new(AtomicBool::new(false));
        let (a, m) = (acked.clone(), missed.clone());
        let fake = fake_with(
            |seen| (seen.path() == "/api/users.info").then_some(Reply::Hang),
            move |_, mut peer| {
                let (a, m) = (a.clone(), m.clone());
                async move {
                    peer.send(&hello()).await;
                    // Nothing more is sent until the previous envelope's own acknowledgement is in.
                    for i in 0..3 {
                        let id = format!("E{i}");
                        peer.send(&envelope(&id, i, "hi", 0)).await;
                        if !expect_ack(&mut peer, &id).await {
                            m.store(true, Ordering::SeqCst);
                            return;
                        }
                        a.lock().unwrap().push(id);
                    }
                    peer.drain().await;
                }
            },
        )
        .await;
        let socket = start(&fake, timing());
        let api = Api::new(
            fake.http(),
            Secret::new("sentinel-bot-token-not-a-credential"),
            WS,
        )
        .unwrap()
        .with_base(fake.url("slack.com", "/api/"));
        // The dispatcher takes the first event and stalls on a name lookup that never answers.
        let Some(Normalized::New(record)) = socket.next(Duration::from_secs(5)).await else {
            panic!("the first event");
        };
        assert_eq!(record.channel, "C0CHAN");
        let lookup = tokio::spawn(async move { api.author_name("U0MEMBER").await });
        eventually("the lookup to be in flight", || {
            fake.seen().iter().any(|s| s.path() == "/api/users.info")
        })
        .await;
        eventually("every envelope acknowledged", || {
            acked.lock().unwrap().len() == 3 || missed.load(Ordering::SeqCst)
        })
        .await;
        assert!(
            !missed.load(Ordering::SeqCst),
            "an envelope was not acknowledged by its id within the bound"
        );
        assert_eq!(*acked.lock().unwrap(), ["E0", "E1", "E2"]);
        assert!(
            !lookup.is_finished(),
            "the lookup was meant to stay blocked"
        );
        assert_eq!(
            socket.pending().0,
            2,
            "the other two wait for the stalled dispatcher"
        );
        // The app token rode the open call and not the websocket upgrade.
        let seen = fake.seen();
        let open = seen
            .iter()
            .find(|s| s.path() == "/api/apps.connections.open")
            .unwrap();
        assert_eq!(
            open.authorization.as_deref(),
            Some(&*format!("Bearer {APP}"))
        );
        assert!(seen
            .iter()
            .filter(|s| s.upgrade)
            .all(|s| s.authorization.is_none()));
        lookup.abort();
        socket.stop().await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn disconnect_refresh_and_silent_close_keep_one_socket() {
    bounded(steady(Duration::from_millis(1000), || async move {
        let fake = fake(|i, mut peer| async move {
            peer.send(&hello()).await;
            match i {
                0 => {
                    peer.send(&disconnect("warning")).await;
                    peer.drain().await;
                }
                2 => {} // the peer vanishes: no close frame
                3 => {
                    peer.send(&disconnect("link_disabled")).await;
                    peer.ignore_until_gone().await;
                }
                // From here on every socket asks to be replaced, so the client is always mid-cycle.
                _ => {
                    peer.send(&disconnect("refresh_requested")).await;
                    peer.drain().await;
                }
            }
        })
        .await;
        let socket = start(&fake, timing());
        eventually("six sockets", || fake.sockets().2 >= 6).await;
        let stopping = Instant::now();
        socket.stop().await;
        let stopped_in = stopping.elapsed();
        let (_, peak, total) = fake.sockets();
        assert_eq!(peak, 1, "two sockets were open at once");
        // Shutdown prevents another connect.
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(fake.sockets().2, total, "a socket was opened after stop");
        eventually("the last socket to be gone", || fake.sockets().0 == 0).await;
        fake.shutdown().await;
        (stopped_in < Duration::from_millis(1500))
            .then_some(())
            .ok_or_else(|| {
                format!("stop is bounded by the close bound, not by the cycle: {stopped_in:?}")
            })
    }))
    .await;
}

#[tokio::test]
async fn a_peer_that_ignores_close_is_forced_down_within_the_bound() {
    bounded(steady(Duration::from_millis(1000), || async move {
        let times = Arc::new(Mutex::new(Vec::<(&str, Instant)>::new()));
        let t = times.clone();
        let fake = fake(move |i, mut peer| {
            let t = t.clone();
            async move {
                peer.send(&hello()).await;
                if i == 0 {
                    peer.send(&disconnect("refresh_requested")).await;
                    t.lock().unwrap().push(("asked", Instant::now()));
                    peer.ignore_until_gone().await;
                    t.lock().unwrap().push(("gone", Instant::now()));
                } else {
                    peer.drain().await;
                }
            }
        })
        .await;
        let socket = start(&fake, timing());
        eventually("the replacement", || fake.sockets().2 >= 2).await;
        eventually("the first socket gone", || times.lock().unwrap().len() == 2).await;
        let times = times.lock().unwrap().clone();
        let held = times[1].1 - times[0].1;
        assert!(
            held >= Duration::from_millis(250),
            "the close was not waited for: {held:?}"
        );
        assert_eq!(fake.sockets().1, 1);
        socket.stop().await;
        fake.shutdown().await;
        (held < Duration::from_millis(1500))
            .then_some(())
            .ok_or_else(|| format!("the close was not bounded: {held:?}"))
    }))
    .await;
}

#[tokio::test]
async fn a_silent_socket_is_replaced_after_the_idle_bound() {
    bounded(steady(Duration::from_millis(1000), || async move {
        // Socket 0's span from its hello to the client being gone: idle plus the close wait, no handshakes.
        let span = Arc::new(Mutex::new(None));
        let seen = span.clone();
        let fake = fake(move |i, mut peer| {
            let seen = seen.clone();
            async move {
                peer.send(&hello()).await;
                if i == 0 {
                    let sent = Instant::now();
                    peer.ignore_until_gone().await;
                    *seen.lock().unwrap() = Some(sent.elapsed());
                } else {
                    peer.drain().await;
                }
            }
        })
        .await;
        let socket = start(
            &fake,
            Timing {
                idle: Duration::from_millis(500),
                // The product's shape: our ping is shorter than the idle bound and must not re-arm it.
                ping: Duration::from_millis(100),
                ..timing()
            },
        );
        eventually("the replacement", || fake.sockets().2 >= 2).await;
        // Within the bound, not merely eventually: no sooner than idle (500 ms) plus the close wait (300 ms), and
        // under 2.8 s, which leaves 2 s of slack on a span that holds no handshake.
        let span = span.lock().unwrap().expect("socket 0 ended");
        assert!(span >= Duration::from_millis(800), "{span:?}");
        assert_eq!(fake.sockets().1, 1);
        assert!(socket.gap().is_some_and(|g| g.cause == GapCause::Reconnect));
        socket.stop().await;
        fake.shutdown().await;
        (span < Duration::from_millis(2800))
            .then_some(())
            .ok_or_else(|| format!("the silent socket was replaced late: {span:?}"))
    }))
    .await;
}

/// Acknowledge-and-count script: send `batch` first, then (when `more` is set) one more, all without waiting.
fn flood(
    batch: Vec<Value>,
    more: Option<(Arc<AtomicBool>, Value)>,
    acked: Arc<AtomicUsize>,
) -> impl Fn(usize, WsPeer) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static
{
    move |_, mut peer| {
        let (batch, more, acked) = (batch.clone(), more.clone(), acked.clone());
        Box::pin(async move {
            peer.send(&hello()).await;
            for v in &batch {
                peer.send(v).await;
            }
            for _ in &batch {
                if peer.recv(SEC).await.is_some() {
                    acked.fetch_add(1, Ordering::SeqCst);
                }
            }
            if let Some((go, extra)) = more {
                let end = Instant::now() + Duration::from_secs(10);
                while !go.load(Ordering::SeqCst) && Instant::now() < end {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                peer.send(&extra).await;
                if peer.recv(SEC).await.is_some() {
                    acked.fetch_add(1, Ordering::SeqCst);
                }
            }
            peer.drain().await;
        })
    }
}

#[tokio::test]
async fn overflow_latches_gap_without_blocking_ack() {
    bounded(async {
        // A failed event: acknowledged, and a gap that stays until it is cleared.
        let acked = Arc::new(AtomicUsize::new(0));
        let mut bad = envelope("EB", 1, "hi", 0);
        bad["payload"]["event"]["ts"] = json!("not-a-timestamp");
        let fake_a = fake(flood(vec![bad], None, acked.clone())).await;
        let socket = start(&fake_a, timing());
        eventually("the bad event acknowledged", || {
            acked.load(Ordering::SeqCst) == 1
        })
        .await;
        let gap = socket.gap().expect("a failed event leaves a gap");
        assert_eq!(gap.cause, GapCause::Failed);
        assert!(socket.next(Duration::from_millis(100)).await.is_none());
        assert_eq!(
            socket.gap(),
            Some(gap),
            "the gap outlives reading the queue"
        );
        assert!(socket.reconciled(gap));
        assert_eq!(socket.gap(), None);
        socket.stop().await;
        fake_a.shutdown().await;

        // A saturated count: every envelope is acknowledged, the 257th and later are dropped into a gap.
        let acked = Arc::new(AtomicUsize::new(0));
        let go = Arc::new(AtomicBool::new(false));
        let batch: Vec<Value> = (0..MAX_PENDING + 44)
            .map(|i| envelope(&format!("E{i}"), i, "hi", 0))
            .collect();
        let more = Some((go.clone(), envelope("EX", 999, "late", 0)));
        let n = batch.len();
        let fake_b = fake(flood(batch, more, acked.clone())).await;
        let socket = start(&fake_b, timing());
        eventually("all acknowledged", || acked.load(Ordering::SeqCst) == n).await;
        assert_eq!(socket.pending().0, MAX_PENDING);
        let first = socket.gap().expect("an overflow leaves a gap");
        assert_eq!(first.cause, GapCause::Overflow);
        // Something lost after the gap was read: the reconciliation that began at `first` does not clear it.
        go.store(true, Ordering::SeqCst);
        eventually("the extra event acknowledged", || {
            acked.load(Ordering::SeqCst) == n + 1
        })
        .await;
        assert!(!socket.reconciled(first));
        let latest = socket.gap().expect("the newer loss stays");
        assert_ne!(latest, first);
        // Reading the queue empty does not clear it either.
        let mut read = 0;
        while let Some(event) = socket.next(Duration::from_millis(100)).await {
            assert!(matches!(event, Normalized::New(_)));
            read += 1;
        }
        assert_eq!(read, MAX_PENDING);
        assert_eq!(socket.gap(), Some(latest));
        assert!(socket.reconciled(latest));
        assert_eq!(socket.gap(), None);
        socket.stop().await;
        fake_b.shutdown().await;

        // A saturated byte budget: ten events of 900,000 bytes are acknowledged; the bytes cap the queue, far
        // under the count.
        let acked = Arc::new(AtomicUsize::new(0));
        let batch: Vec<Value> = (0..10)
            .map(|i| envelope(&format!("E{i}"), i, "hi", 900_000))
            .collect();
        let fake_c = fake(flood(batch, None, acked.clone())).await;
        let socket = start(&fake_c, timing());
        eventually("all acknowledged", || acked.load(Ordering::SeqCst) == 10).await;
        let (count, bytes) = socket.pending();
        assert!((1..MAX_PENDING).contains(&count), "{count}");
        assert!(bytes <= MAX_PENDING_BYTES, "{bytes}");
        assert!(
            count < 10,
            "the ninth or tenth was dropped for bytes: {count}"
        );
        let gap = socket.gap().expect("a byte overflow leaves a gap");
        assert_eq!(gap.cause, GapCause::Overflow);
        socket.stop().await;
        fake_c.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_message_over_one_mib_is_refused_before_it_is_read_whole() {
    bounded(async {
        let fake = fake(|i, mut peer| async move {
            peer.send(&hello()).await;
            if i == 0 {
                // A valid event that is one byte over the bound: if it were read whole it would be delivered.
                let mut env = envelope("EBIG", 1, "big", 0);
                let len = env.to_string().len();
                env["payload"]["pad"] = json!("x".repeat(MAX_MESSAGE + 1 - len));
                assert_eq!(env.to_string().len(), MAX_MESSAGE + 1);
                peer.send(&env).await;
            } else {
                // Exactly the bound passes.
                let mut env = envelope("EOK", 2, "exact", 0);
                let len = env.to_string().len();
                env["payload"]["pad"] = json!("x".repeat(MAX_MESSAGE - len));
                assert_eq!(env.to_string().len(), MAX_MESSAGE);
                peer.send(&env).await;
            }
            peer.drain().await;
        })
        .await;
        let socket = start(&fake, timing());
        let Some(Normalized::New(record)) = socket.next(Duration::from_secs(5)).await else {
            panic!("the exact-size event");
        };
        assert_eq!(
            record.text, "exact",
            "the oversize message was delivered or an order broke"
        );
        assert!(
            fake.sockets().2 >= 2,
            "the oversize connection was not replaced"
        );
        assert!(socket.gap().is_some());
        socket.stop().await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_foreign_websocket_url_fails_visibly_and_leaks_nothing() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, port| {
            if seen.path() == "/api/apps.connections.open" {
                let url = format!("wss://evilslack.com:{port}/link/?ticket={TICKET}");
                return Reply::json(&json!({"ok": true, "url": url}));
            }
            Reply::json(&json!({"ok": false}))
        })
        .await;
        let socket = start(&fake, timing());
        eventually("the refusal to show", || socket.status().error.is_some()).await;
        let status = socket.status();
        assert_eq!(
            status.error,
            Some(SocketError::Http(HttpError::Destination))
        );
        assert!(!status.connected);
        let shown = format!(
            "{status:?} {} {:?}",
            SocketError::Http(HttpError::Destination),
            socket.gap()
        );
        assert!(
            !shown.contains(TICKET) && !shown.contains("evilslack"),
            "{shown}"
        );
        assert!(
            fake.seen().iter().all(|s| !s.upgrade),
            "a foreign host was dialed"
        );
        assert_eq!(fake.sockets().2, 0);
        socket.stop().await;

        // The same policy for every other spelling.
        let http = fake.http();
        let port = fake.url("x", "").rsplit(':').next().unwrap().to_string();
        for url in [
            format!("wss://evilslack.com:{port}/l"),
            format!("wss://slack.com.evil.example:{port}/l"),
            format!("ws://wss-primary.slack.com:{port}/l"),
            format!("https://wss-primary.slack.com:{port}/l"),
            format!("wss://user:pw@wss-primary.slack.com:{port}/l"),
            "wss://wss-primary.slack.com:1/l".to_string(),
            "wss://127.0.0.1/l".to_string(),
            "not a url".to_string(),
        ] {
            let result = http.websocket(&url, SEC).await;
            assert!(
                matches!(result, Err(HttpError::Destination)),
                "{url} was not refused as a destination"
            );
        }
        assert_eq!(fake.sockets().2, 0);
        fake.shutdown().await;
    })
    .await;
}

/// A probe's short waits: its first frame within 2 s, its close within 300 ms.
fn probe() -> Probe {
    Probe { hello: SEC, close: Duration::from_millis(300) }
}

/// The setup's check of an app token: one `apps.connections.open`, its URL judged by the websocket policy before anything
/// is dialed; the socket's codes; nothing shown carries the token, the ticket or the host. A URL that passes is dialed
/// once, and the app its `hello` names is the answer, with the socket gone when the check returns.
#[tokio::test]
async fn an_app_token_check_tells_the_app_by_one_socket_and_refuses_a_foreign_url() {
    bounded(async {
        let fake = FakeSlack::start(Identity::Trusted, |seen, port| {
            let url = |u: String| Reply::json(&json!({"ok": true, "url": u}));
            if seen.upgrade {
                return Reply::socket(|mut peer| async move {
                    peer.send(&json!({"type": "hello", "num_connections": 1, "connection_info": {"app_id": "A0APP"}})).await;
                    peer.drain().await;
                });
            }
            match seen.path().as_str() {
                "/api/good/apps.connections.open" => {
                    url(format!("wss://wss-primary.slack.com:{port}/link/?ticket={TICKET}"))
                }
                "/api/foreign/apps.connections.open" => {
                    url(format!("wss://evilslack.com:{port}/link/?ticket={TICKET}"))
                }
                "/api/plain/apps.connections.open" => {
                    url(format!("ws://wss-primary.slack.com:{port}/link/?ticket={TICKET}"))
                }
                "/api/long/apps.connections.open" => url(format!(
                    "wss://wss-primary.slack.com:{port}/link/?ticket={}",
                    "t".repeat(2048)
                )),
                "/api/nourl/apps.connections.open" => Reply::json(&json!({"ok": true})),
                "/api/html/apps.connections.open" => Reply::ok(format!("<html>{APP}</html>").as_bytes()),
                "/api/refused/apps.connections.open" => {
                    Reply::json(&json!({"ok": false, "error": "invalid_auth", "warning": APP}))
                }
                "/api/limited/apps.connections.open" => {
                    Reply::json(&json!({"ok": false, "error": format!("ratelimited {APP}")}))
                }
                "/api/down/apps.connections.open" => Reply::Status {
                    code: 503,
                    headers: Vec::new(),
                    body: APP.as_bytes().to_vec(),
                },
                "/api/status/apps.connections.open" => Reply::Status {
                    code: 401,
                    headers: Vec::new(),
                    body: APP.as_bytes().to_vec(),
                },
                _ => Reply::Drop,
            }
        })
        .await;
        let http = fake.http();
        let check = |mode: &str| {
            let base = fake.url("slack.com", &format!("/api/{mode}/"));
            let http = http.clone();
            async move { check_app_token_at(&http, Secret::new(APP), &base, probe()).await }
        };
        assert_eq!(check("good").await, Ok(Some("A0APP".to_string())));
        // The probe's socket is retired before the check answers: the fake's script ends once its peer is gone.
        eventually("the probe's socket gone", || fake.sockets().0 == 0).await;
        assert_eq!(fake.sockets(), (0, 1, 1), "one socket, opened once");
        let seen = fake.seen();
        assert_eq!(seen.len(), 2, "one request and one upgrade");
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].authorization.as_deref(), Some(&*format!("Bearer {APP}")));
        assert!(seen[1].upgrade && seen[1].authorization.is_none(), "the upgrade carries the ticket alone");
        let mut errors = Vec::new();
        for (mode, want) in [
            ("foreign", SocketError::Http(HttpError::Destination)),
            ("plain", SocketError::Http(HttpError::Destination)),
            ("long", SocketError::Malformed),
            ("nourl", SocketError::Malformed),
            ("html", SocketError::Malformed),
            ("refused", SocketError::Rejected),
            ("status", SocketError::Rejected),
            ("limited", SocketError::Unavailable),
            ("down", SocketError::Unavailable),
        ] {
            let got = check(mode).await.unwrap_err();
            assert_eq!(got, want, "{mode}");
            errors.push(got);
        }
        let dropped = check("drop").await.unwrap_err();
        assert!(matches!(dropped, SocketError::Http(_)), "drop");
        errors.push(dropped);
        for e in &errors {
            let text = format!("{e} {e:?} {}", e.code());
            for secret in [APP, TICKET, "evilslack", "wss-primary", "127.0.0.1"] {
                assert!(!text.contains(secret), "a check's error shows a secret or a URL ({})", e.code());
            }
        }
        // No refused answer was dialed, a foreign URL least of all: the one socket is the good check's.
        assert_eq!(fake.seen().iter().filter(|s| s.upgrade).count(), 1, "a refused check dialed a websocket");
        assert_eq!(fake.sockets().2, 1);
        assert_eq!(fake.seen().len(), 12, "one request a check, and the good check's upgrade");
        fake.shutdown().await;
    })
    .await;
}

/// What the fake's socket does for a probe, by the upgrade's index.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Says {
    /// An envelope of the app first, then the hello: a message Slack handed this connection.
    EnvelopeFirst,
    /// The hello, then an envelope at once.
    EnvelopeAfter,
    /// A hello that names no app.
    NoApp,
    /// A first text frame that is not JSON.
    NotJson,
    /// Nothing, and no answer to a close.
    Silent,
}

/// A probe socket never acknowledges: an envelope Slack hands it, before its hello or after, gets no frame back but the
/// close, so Slack can send it again to a connection of its app; the app is the first text frame's (an envelope's
/// `payload.api_app_id` as a hello's `connection_info.app_id`). A socket that names no app, says nothing in time or does
/// not open tells no app, within the probe's two waits, and is no error.
#[tokio::test]
async fn a_probe_socket_acknowledges_no_envelope_and_an_untold_app_is_none_within_its_waits() {
    bounded(async {
        let order = [Says::EnvelopeFirst, Says::EnvelopeAfter, Says::NoApp, Says::NotJson, Says::Silent];
        let got: Arc<Mutex<Vec<(Says, Option<Value>)>>> = Arc::default();
        let (heard, upgrades) = (got.clone(), Arc::new(AtomicUsize::new(0)));
        let fake = FakeSlack::start(Identity::Trusted, move |seen, port| {
            if seen.path() == "/api/apps.connections.open" {
                return Reply::json(&json!({"ok": true, "url": format!("wss://wss-primary.slack.com:{port}/link/?ticket={TICKET}")}));
            }
            if !seen.upgrade {
                return Reply::json(&json!({"ok": false, "error": "unexpected"}));
            }
            // The sixth dial is refused: a socket that does not open.
            let Some(says) = order.get(upgrades.fetch_add(1, Ordering::SeqCst)).copied() else { return Reply::Drop };
            let heard = heard.clone();
            Reply::socket(move |mut peer| {
                let heard = heard.clone();
                async move {
                    let named = json!({"type": "hello", "num_connections": 1, "connection_info": {"app_id": "A0APP"}});
                    match says {
                        Says::EnvelopeFirst => {
                            peer.send(&envelope_for("E1", 1, "A0ENV")).await;
                            peer.send(&named).await;
                        }
                        Says::EnvelopeAfter => {
                            peer.send(&named).await;
                            peer.send(&envelope_for("E2", 2, "A0APP")).await;
                        }
                        Says::NoApp => peer.send(&hello()).await,
                        Says::NotJson => peer.send_text("hello".into()).await,
                        Says::Silent => {
                            peer.ignore_until_gone().await;
                            heard.lock().unwrap().push((says, None));
                            return;
                        }
                    }
                    // Every text frame the client writes until it is gone: a frame that acknowledges would be one.
                    let frame = peer.recv(Duration::from_secs(5)).await;
                    heard.lock().unwrap().push((says, frame));
                }
            })
        })
        .await;
        let (http, base) = (fake.http(), fake.url("slack.com", "/api/"));
        let mut told = Vec::new();
        for n in 0..6 {
            // Each check within the probe's two waits, the fifth (a silent peer) at their end.
            let waits = probe().hello + probe().close;
            let checked = tokio::time::timeout(waits + SEC, check_app_token_at(&http, Secret::new(APP), &base, probe())).await;
            assert!(checked.is_ok(), "check {n}: a probe socket outlasted its waits of {waits:?}");
            told.extend(checked);
            // Its socket is gone before the next check, as before a connect's write.
            eventually("the probe's socket gone", || fake.sockets().0 == 0).await;
        }
        let app = |a: &str| Ok(Some(a.to_string()));
        assert_eq!(told, [app("A0ENV"), app("A0APP"), Ok(None), Ok(None), Ok(None), Ok(None)]);
        eventually("every script's end", || got.lock().unwrap().len() == 5).await;
        for (says, frame) in got.lock().unwrap().iter() {
            assert_eq!(*frame, None, "{says:?}: the probe wrote a text frame, which is how an envelope is acknowledged");
        }
        assert_eq!(fake.sockets(), (0, 1, 5), "never two sockets, and the refused dial opened none");
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn an_events_message_keeps_its_origin_through_the_socket() {
    bounded(async {
        let acked = Arc::new(AtomicUsize::new(0));
        let mut own = envelope("E1", 1, "from the bot", 0);
        own["payload"]["event"]["bot_id"] = json!("B0BOT");
        own["payload"]["event"]["user"] = json!("U0BOT");
        let mut edit = envelope("E2", 2, "", 0);
        edit["payload"]["event"] = json!({
            "type": "message", "subtype": "message_deleted", "channel": "C0CHAN",
            "deleted_ts": "1700000001.000100", "ts": "1700000009.000100"
        });
        let fake = fake(flood(vec![own, edit], None, acked.clone())).await;
        let socket = start(&fake, timing());
        let Some(Normalized::New(record)) = socket.next(Duration::from_secs(5)).await else {
            panic!("the own post");
        };
        assert_eq!(record.origin, Origin::Bot);
        assert!(matches!(
            socket.next(Duration::from_secs(5)).await,
            Some(Normalized::Deleted { .. })
        ));
        socket.stop().await;
        fake.shutdown().await;
    })
    .await;
}

/// Record the instant of every `apps.connections.open` request; `refuse` answers it `ok: false` for the first `n`, with
/// a code of Slack's own state (a token's refusal would end the listener).
fn opens(
    times: &Arc<Mutex<Vec<Instant>>>,
    refuse: usize,
) -> impl Fn(&Seen) -> Option<Reply> + Send + Sync + 'static {
    let times = times.clone();
    move |seen| {
        if seen.path() != "/api/apps.connections.open" {
            return None;
        }
        let mut t = times.lock().unwrap();
        t.push(Instant::now());
        (t.len() <= refuse).then(|| Reply::json(&json!({"ok": false, "error": "internal_error"})))
    }
}

#[tokio::test]
async fn a_gap_opened_by_an_outage_survives_a_reconciliation_made_during_it() {
    bounded(async {
        let times = Arc::new(Mutex::new(Vec::new()));
        // The first open succeeds, the second is refused, the third succeeds.
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let recorder = opens(&times, 0);
        let fake = fake_with(
            move |seen| {
                recorder(seen);
                (seen.path() == "/api/apps.connections.open"
                    && c.fetch_add(1, Ordering::SeqCst) == 1)
                    .then(|| Reply::json(&json!({"ok": false, "error": "internal_error"})))
            },
            |i, mut peer| async move {
                peer.send(&hello()).await;
                if i == 0 {
                    peer.send(&disconnect("refresh_requested")).await;
                }
                peer.drain().await;
            },
        )
        .await;
        let slow = Timing {
            backoff_first: Duration::from_millis(400),
            backoff_max: Duration::from_millis(800),
            ..timing()
        };
        let socket = start(&fake, slow);
        eventually("the refused reopen", || {
            socket.status().error == Some(SocketError::Unavailable)
        })
        .await;
        let gap = socket.gap().expect("the lost socket left a gap");
        assert!(!socket.status().connected);
        // History works during the outage, so a reconciliation finishes and clears what it covered.
        assert!(socket.reconciled(gap));
        assert_eq!(socket.gap(), None);
        eventually("the new hello", || socket.status().connected).await;
        let gap = socket.gap().expect("the new hello brings the gap back");
        assert_eq!(gap.cause, GapCause::Reconnect);
        socket.stop().await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_failed_event_or_a_lost_socket_during_a_reconciliation_keeps_the_gap() {
    bounded(async {
        let (bad_now, drop_now) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (b, d) = (bad_now.clone(), drop_now.clone());
        let fake = fake(move |i, mut peer| {
            let (b, d) = (b.clone(), d.clone());
            async move {
                peer.send(&hello()).await;
                if i == 0 {
                    let wait = |f: Arc<AtomicBool>| async move {
                        let end = Instant::now() + Duration::from_secs(10);
                        while !f.load(Ordering::SeqCst) && Instant::now() < end {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    };
                    wait(b).await;
                    let mut bad = envelope("EB", 1, "hi", 0);
                    bad["payload"]["event"]["ts"] = json!("not-a-timestamp");
                    peer.send(&bad).await;
                    let _ = peer.recv(SEC).await;
                    wait(d).await;
                    peer.send(&disconnect("refresh_requested")).await;
                }
                peer.drain().await;
            }
        })
        .await;
        let socket = start(&fake, timing());
        eventually("the first gap", || socket.gap().is_some()).await;
        // A reconciliation begins at `first`; a failed event arrives during it.
        let first = socket.gap().unwrap();
        bad_now.store(true, Ordering::SeqCst);
        eventually("the failed event", || {
            socket.gap().is_some_and(|g| g.cause == GapCause::Failed)
        })
        .await;
        assert!(
            !socket.reconciled(first),
            "a failed event during a reconciliation was cleared by it"
        );
        // Another begins at `second`; the socket is lost during it.
        let second = socket.gap().unwrap();
        assert_eq!(second.cause, GapCause::Failed);
        drop_now.store(true, Ordering::SeqCst);
        eventually("the lost socket", || socket.gap() != Some(second)).await;
        assert!(
            !socket.reconciled(second),
            "a lost socket during a reconciliation was cleared by it"
        );
        assert!(socket.gap().is_some());
        socket.stop().await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn reconnects_are_paced_after_a_refusal_and_after_a_hello() {
    bounded(steady(Duration::from_millis(100), || async move {
        // A refusing Slack is asked at 0, 200, 600 and 1400 ms: the pause doubles.
        let times = Arc::new(Mutex::new(Vec::new()));
        let fake_a = fake_with(opens(&times, usize::MAX), |_, _| async {}).await;
        let paced = Timing {
            backoff_first: Duration::from_millis(200),
            backoff_max: Duration::from_millis(800),
            ..timing()
        };
        let socket = start(&fake_a, paced);
        eventually("four attempts", || times.lock().unwrap().len() >= 4).await;
        let t = times.lock().unwrap().clone();
        assert!(
            t[1] - t[0] >= Duration::from_millis(180),
            "{:?}",
            t[1] - t[0]
        );
        assert!(
            t[2] - t[1] >= Duration::from_millis(380),
            "{:?}",
            t[2] - t[1]
        );
        assert!(
            t[3] - t[2] >= Duration::from_millis(780),
            "{:?}",
            t[3] - t[2]
        );
        socket.stop().await;
        fake_a.shutdown().await;

        // A socket that says hello and is then replaced is back after the first pause, no sooner.
        let times = Arc::new(Mutex::new(Vec::new()));
        let gone = Arc::new(Mutex::new(Vec::new()));
        let gone_at = gone.clone();
        let fake_b = fake_with(opens(&times, 0), move |_, mut peer| {
            let gone_at = gone_at.clone();
            async move {
                peer.send(&hello()).await;
                peer.send(&disconnect("refresh_requested")).await;
                peer.drain().await;
                gone_at.lock().unwrap().push(Instant::now());
            }
        })
        .await;
        let socket = start(&fake_b, paced);
        eventually("three connections", || times.lock().unwrap().len() >= 3).await;
        // From the peer seeing the socket gone to the next open request: the pause and nothing else. The hello
        // reset it, so the second reopen did not wait the doubled 400 ms.
        let t = times.lock().unwrap().clone();
        let g = gone.lock().unwrap().clone();
        let pauses: Vec<Duration> = (0..2).map(|i| t[i + 1] - g[i]).collect();
        for pause in &pauses {
            assert!(*pause >= Duration::from_millis(180), "{pause:?}");
        }
        socket.stop().await;
        fake_b.shutdown().await;
        pauses
            .iter()
            .all(|p| *p < Duration::from_millis(300))
            .then_some(())
            .ok_or_else(|| format!("the pause after a hello was not reset: {pauses:?}"))
    }))
    .await;
}

#[tokio::test]
async fn stop_during_the_reconnect_pause_does_not_wait_for_it() {
    bounded(steady(Duration::from_millis(250), || async move {
        let fake = fake(|_, mut peer| async move {
            peer.send(&hello()).await;
            peer.send(&disconnect("refresh_requested")).await;
            peer.drain().await;
        })
        .await;
        // The pause (5 s) is longer than the stop bound (1 s).
        let socket = start(
            &fake,
            Timing {
                backoff_first: Duration::from_secs(5),
                backoff_max: Duration::from_secs(5),
                stop: Duration::from_secs(1),
                ..timing()
            },
        );
        eventually("the first socket gone", || {
            let (open, _, total) = fake.sockets();
            total == 1 && open == 0
        })
        .await;
        let began = Instant::now();
        socket.stop().await;
        let took = began.elapsed();
        assert_eq!(fake.sockets().2, 1);
        fake.shutdown().await;
        (took < Duration::from_millis(500))
            .then_some(())
            .ok_or_else(|| format!("stop waited for the pause: {took:?}"))
    }))
    .await;
}

#[tokio::test]
async fn a_link_that_answers_is_kept_past_the_idle_bound() {
    let idle = Duration::from_millis(500);
    bounded(steady(idle / 2, || async move {
        let fake = fake(|_, mut peer| async move {
            peer.send(&hello()).await;
            peer.drain().await;
        })
        .await;
        let socket = start(
            &fake,
            Timing {
                idle,
                ping: Duration::from_millis(100),
                ..timing()
            },
        );
        eventually("hello", || socket.status().connected).await;
        let g = socket.gap().expect("hello gap");
        assert_eq!(g.cause, GapCause::Reconnect);
        assert!(socket.reconciled(g));
        assert_eq!(socket.gap(), None);
        tokio::time::sleep(Duration::from_millis(1800)).await;
        let kept = fake.sockets().2 == 1 && socket.gap().is_none() && socket.status().connected;
        socket.stop().await;
        fake.shutdown().await;
        kept.then_some(())
            .ok_or_else(|| "a healthy link was replaced or reported a gap".to_string())
    }))
    .await;
}

#[tokio::test]
async fn a_chatty_peer_that_never_answers_keeps_its_link() {
    let idle = Duration::from_millis(500);
    bounded(steady(idle / 2, || async move {
        // The peer is chatty for 1.8 s, past three idle bounds, then goes silent; the link is judged when the peer has
        // sent its last frame, not at a time the test guesses.
        let done = Arc::new(AtomicBool::new(false));
        let finished = done.clone();
        let fake = fake(move |_, mut peer| {
            let finished = finished.clone();
            async move {
                peer.send(&hello()).await;
                for _ in 0..18 {
                    peer.send(&json!({"type": "slash_commands_noop"})).await;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                finished.store(true, Ordering::SeqCst);
                peer.ignore_until_gone().await;
            }
        })
        .await;
        let socket = start(
            &fake,
            Timing {
                idle,
                ping: Duration::from_millis(100),
                ..timing()
            },
        );
        eventually("the frames sent", || done.load(Ordering::SeqCst)).await;
        let kept = fake.sockets().2 == 1;
        socket.stop().await;
        fake.shutdown().await;
        kept.then_some(())
            .ok_or_else(|| "chatty link replaced".to_string())
    }))
    .await;
}

#[tokio::test]
async fn a_reconciliation_begun_before_a_hello_is_refused_by_it() {
    bounded(async {
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let fake = fake_with(
            move |seen| {
                (seen.path() == "/api/apps.connections.open"
                    && c.fetch_add(1, Ordering::SeqCst) == 1)
                    .then(|| Reply::json(&json!({"ok": false, "error": "internal_error"})))
            },
            |i, mut peer| async move {
                peer.send(&hello()).await;
                if i == 0 {
                    peer.send(&disconnect("refresh_requested")).await;
                }
                peer.drain().await;
            },
        )
        .await;
        let slow = Timing {
            backoff_first: Duration::from_millis(400),
            backoff_max: Duration::from_millis(800),
            ..timing()
        };
        let socket = start(&fake, slow);
        eventually("the refused reopen", || {
            socket.status().error == Some(SocketError::Unavailable)
        })
        .await;
        // A reconciliation begins here, during the outage, and is still running when the hello arrives.
        let gap = socket.gap().expect("the lost socket left a gap");
        eventually("the new hello", || socket.status().connected).await;
        assert!(
            !socket.reconciled(gap),
            "a reconciliation begun before the hello cleared it"
        );
        assert!(socket.gap().is_some());
        socket.stop().await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn every_slack_ping_is_answered_with_its_payload_while_the_consumer_stalls_and_the_queue_is_full(
) {
    bounded(async {
        let pongs = Arc::new(Mutex::new(Vec::<(&str, bool)>::new()));
        let seen = pongs.clone();
        let fake = fake(move |_, mut peer| {
            let seen = seen.clone();
            async move {
                peer.send(&hello()).await;
                // A consumer that never reads, queue not full: the ping is still answered.
                for i in 0..3 {
                    peer.send(&envelope(&format!("S{i}"), i, "hi", 0)).await;
                    assert!(expect_ack(&mut peer, &format!("S{i}")).await);
                }
                // An envelope right behind the ping: its acknowledgement must come after the pong.
                peer.ping("Ping from applink-1").await;
                peer.send(&envelope("S3", 3, "hi", 0)).await;
                let ok = peer.expect_pong_first("Ping from applink-1", SEC).await;
                assert!(expect_ack(&mut peer, "S3").await);
                seen.lock().unwrap().push(("stalled", ok));
                // The queue overflowing: every envelope acknowledged, the rest dropped; the ping is still answered.
                for i in 0..MAX_PENDING + 20 {
                    peer.send(&envelope(&format!("F{i}"), 100 + i, "hi", 0))
                        .await;
                }
                for _ in 0..MAX_PENDING + 20 {
                    assert!(peer.recv(SEC).await.is_some());
                }
                peer.ping("Ping from applink-2").await;
                peer.send(&envelope("G0", 999, "hi", 0)).await;
                let ok = peer.expect_pong_first("Ping from applink-2", SEC).await;
                assert!(expect_ack(&mut peer, "G0").await);
                seen.lock().unwrap().push(("full", ok));
                peer.drain().await;
            }
        })
        .await;
        let socket = start(&fake, timing());
        eventually("both pongs seen", || pongs.lock().unwrap().len() == 2).await;
        assert_eq!(
            *pongs.lock().unwrap(),
            vec![("stalled", true), ("full", true)]
        );
        assert_eq!(socket.pending().0, MAX_PENDING, "the queue was not full");
        socket.stop().await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_peer_that_only_pings_keeps_its_link() {
    let idle = Duration::from_millis(700);
    bounded(steady(idle / 2, || async move {
        // The peer pings every 40 ms for 1.4 s, twice the idle bound (17 times its own gap), and never reads, so
        // never answers ours.
        let done = Arc::new(AtomicBool::new(false));
        let finished = done.clone();
        let fake = fake(move |_, mut peer| {
            let finished = finished.clone();
            async move {
                peer.send(&hello()).await;
                for _ in 0..35 {
                    peer.ping("Ping from applink-1").await;
                    tokio::time::sleep(Duration::from_millis(40)).await;
                }
                finished.store(true, Ordering::SeqCst);
                peer.ignore_until_gone().await;
            }
        })
        .await;
        let socket = start(
            &fake,
            Timing {
                idle,
                ping: Duration::from_millis(100),
                ..timing()
            },
        );
        eventually("the pings sent", || done.load(Ordering::SeqCst)).await;
        let kept = fake.sockets().2 == 1;
        socket.stop().await;
        fake.shutdown().await;
        kept.then_some(())
            .ok_or_else(|| "a pinging link was replaced".to_string())
    }))
    .await;
}

#[tokio::test]
async fn a_bare_end_of_stream_is_replaced_within_the_pacing_and_latches_a_gap() {
    bounded(steady(Duration::from_millis(1000), || async move {
        // Socket 0 says hello and vanishes with no close and no disconnect; socket 1 says nothing. The idle bound
        // (60 s) is far past the test, so only the end of stream can have ended socket 0.
        let go = Arc::new(AtomicBool::new(false));
        let times = Arc::new(Mutex::new(Vec::<Instant>::new()));
        let (flag, at) = (go.clone(), times.clone());
        let fake = fake(move |i, mut peer| {
            let (flag, at) = (flag.clone(), at.clone());
            async move {
                if i == 0 {
                    peer.send(&hello()).await;
                    while !flag.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    at.lock().unwrap().push(Instant::now());
                } else {
                    at.lock().unwrap().push(Instant::now());
                    peer.drain().await;
                }
            }
        })
        .await;
        let socket = start(&fake, timing());
        eventually("hello", || socket.status().connected).await;
        let g = socket.gap().expect("hello gap");
        assert!(socket.reconciled(g));
        assert_eq!(socket.gap(), None);
        go.store(true, Ordering::SeqCst);
        eventually("the replacement", || times.lock().unwrap().len() == 2).await;
        let t = times.lock().unwrap().clone();
        let replaced_in = t[1] - t[0];
        assert!(socket.gap().is_some_and(|g| g.cause == GapCause::Reconnect));
        socket.stop().await;
        fake.shutdown().await;
        (replaced_in < Duration::from_secs(5))
            .then_some(())
            .ok_or_else(|| format!("replaced by the idle bound, not by the end: {replaced_in:?}"))
    }))
    .await;
}

#[test]
fn the_product_numbers_are_the_measured_ones() {
    // Slack's quiet-socket ping gap is 10.3 s at most and it drops a silent peer at 45 s.
    assert_eq!(Timing::PRODUCT.idle, Duration::from_secs(45));
    assert_eq!(Timing::PRODUCT.ping, Duration::from_secs(30));
    assert_eq!(Timing::PRODUCT.write, Duration::from_secs(5));
    assert_eq!(Timing::PRODUCT.open, Duration::from_secs(15));
    assert_eq!(Timing::PRODUCT.close, Duration::from_secs(2));
    assert_eq!(Timing::PRODUCT.backoff_first, Duration::from_secs(1));
    assert_eq!(Timing::PRODUCT.backoff_max, Duration::from_secs(30));
    assert_eq!(Timing::PRODUCT.stop, Duration::from_secs(5));
}

#[tokio::test]
async fn a_write_that_cannot_finish_retires_the_socket_within_the_write_bound() {
    bounded(steady(Duration::from_millis(1000), || async move {
        // The peer sends acknowledged frames and never reads, so the client's acknowledgements fill the buffers
        // and one write stalls. Idle is 60 s, so only the write bound can end the socket; the peer times from its
        // stalled send, if any, to the connection dropping.
        let ended = Arc::new(Mutex::new(None));
        let seen = ended.clone();
        let fake = fake(move |i, mut peer| {
            let seen = seen.clone();
            async move {
                if i == 0 {
                    peer.send(&hello()).await;
                    let id = "a".repeat(128);
                    let frame = json!({"type": "noop", "envelope_id": id}).to_string();
                    let out = peer
                        .send_until_dropped(
                            &frame,
                            Duration::from_millis(100),
                            400_000,
                            Duration::from_secs(6),
                        )
                        .await;
                    *seen.lock().unwrap() = Some(out);
                } else {
                    peer.drain().await;
                }
            }
        })
        .await;
        let socket = start(
            &fake,
            Timing {
                write: Duration::from_millis(200),
                ..timing()
            },
        );
        eventually("the flood to end", || ended.lock().unwrap().is_some()).await;
        let out = ended.lock().unwrap().take().unwrap();
        // The write bound (200 ms) and the close wait (300 ms), with 2.5 s of slack for a slow host. An unbounded
        // write never drops the connection, so the flood ends held, stalled.
        let late = match out {
            Flood::Dropped(stalled) => stalled.filter(|s| *s >= Duration::from_millis(3000)),
            Flood::Held {
                stalled: true,
                frames,
            } => panic!("a stalled write held the socket: {frames} frames, never dropped"),
            Flood::Held { frames, .. } => {
                panic!("the flood never stalled a write within its caps: {frames} frames")
            }
        };
        eventually("the replacement", || fake.sockets().2 >= 2).await;
        socket.stop().await;
        fake.shutdown().await;
        late.map_or(Ok(()), |s| {
            Err(format!("the stalled write was dropped late: {s:?}"))
        })
    }))
    .await;
}

#[tokio::test]
async fn take_hands_over_every_pending_event_and_the_gap_and_frees_the_budget() {
    bounded(async {
        // Ten events of 900,000 bytes: the budget holds nine and the tenth is dropped into a gap.
        let acked = Arc::new(AtomicUsize::new(0));
        let batch: Vec<Value> = (0..10).map(|i| envelope(&format!("E{i}"), i, "hi", 900_000)).collect();
        let go = Arc::new(AtomicBool::new(false));
        let more = Some((go.clone(), envelope("EX", 99, "late", 900_000)));
        let fake = fake(flood(batch, more, acked.clone())).await;
        let socket = start(&fake, timing());
        eventually("all acknowledged", || acked.load(Ordering::SeqCst) == 10).await;
        let (count, _) = socket.pending();
        let gap = socket.gap().expect("a byte overflow leaves a gap");
        let taken = socket.take();
        assert_eq!(taken.events.len(), count, "take dropped pending events");
        assert_eq!(taken.gap, Some(gap));
        assert_eq!(socket.pending(), (0, 0), "take left the byte budget charged");
        assert_eq!(socket.gap(), Some(gap), "take cleared the gap");
        // The budget is free again: one more big event is queued, not dropped.
        go.store(true, Ordering::SeqCst);
        eventually("the extra event acknowledged", || acked.load(Ordering::SeqCst) == 11).await;
        assert_eq!(socket.pending().0, 1, "the event after a take was dropped");
        assert_eq!(socket.gap(), Some(gap), "no new loss");
        socket.stop().await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn wait_wakes_on_a_gap_and_on_an_event_and_takes_nothing() {
    bounded(async {
        let acked = Arc::new(AtomicUsize::new(0));
        let go = Arc::new(AtomicBool::new(false));
        let more = Some((go.clone(), envelope("EX", 99, "late", 0)));
        let fake = fake(flood(vec![], more, acked.clone())).await;
        let socket = start(&fake, timing());
        // The hello latches a gap and queues nothing: the wait wakes for the gap alone.
        assert!(socket.wait(Duration::from_secs(20)).await, "no wake for a gap");
        assert_eq!(socket.pending().0, 0);
        let taken = socket.take();
        assert!(taken.gap.is_some() && taken.events.is_empty());
        // An event that arrives while the wait is waiting wakes it (the queue is empty when the wait begins), and
        // the wait leaves the event queued.
        let (woke, ()) = tokio::join!(socket.wait(Duration::from_secs(20)), async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            go.store(true, Ordering::SeqCst);
        });
        assert!(woke, "no wake for an event");
        assert_eq!(socket.pending().0, 1, "wait took the event");
        assert_eq!(socket.take().events.len(), 1);
        socket.stop().await;
        fake.shutdown().await;
    })
    .await;
}

/// The bot's own app, and another app's, as Slack names them in a `hello` and an envelope.
const OWN_APP: &str = "A0OWN";
const OTHER_APP: &str = "A0OTHER";

/// `envelope` naming the app it was delivered for.
fn envelope_for(id: &str, n: usize, app: &str) -> Value {
    let mut e = envelope(id, n, "hi", 0);
    e["payload"]["api_app_id"] = json!(app);
    e
}

fn opened(fake: &FakeSlack) -> usize {
    fake.seen().iter().filter(|s| s.path() == "/api/apps.connections.open").count()
}

fn status_reply(code: u16, body: &Value) -> Reply {
    Reply::Status { code, headers: Vec::new(), body: body.to_string().into_bytes() }
}

/// A socket that says hello naming `hello_app` as its `connection_info.app_id` (`Null`: no `connection_info`), then
/// sends `envelopes` one after another, recording which were acknowledged by their id; then drains.
async fn app_fake(hello_app: Value, envelopes: Vec<Value>, acked: Arc<Mutex<Vec<String>>>) -> FakeSlack {
    let envelopes = Arc::new(envelopes);
    fake(move |_, mut peer| {
        let (envelopes, acked, hello_app) = (envelopes.clone(), acked.clone(), hello_app.clone());
        async move {
            let mut said = hello();
            if !hello_app.is_null() {
                said["connection_info"] = json!({"app_id": hello_app});
            }
            peer.send(&said).await;
            for e in envelopes.iter() {
                peer.send(e).await;
                let id = e["envelope_id"].as_str().unwrap().to_string();
                if peer.recv(SEC).await == Some(json!({ "envelope_id": id })) {
                    acked.lock().unwrap().push(id);
                }
            }
            peer.drain().await;
        }
    })
    .await
}

/// Guarantee 17's one exception (PS8, PS10): an envelope naming another app is not acknowledged, so Slack can give it to
/// that app's own consumer, and the listener ends for good, `wrong_app`, with no next socket. An envelope naming the
/// bot's own app, or none, is acknowledged first as before, and a bot whose app is unknown checks nothing.
#[tokio::test]
async fn an_envelope_of_another_app_is_not_acknowledged_and_ends_the_listener() {
    bounded(async {
        let acked = Arc::new(Mutex::new(Vec::new()));
        let sent = vec![
            envelope("E0", 0, "hi", 0),
            envelope_for("E1", 1, OWN_APP),
            envelope_for("E2", 2, OTHER_APP),
            envelope_for("E3", 3, OWN_APP),
        ];
        let fake = app_fake(json!(OWN_APP), sent.clone(), acked.clone()).await;
        let socket = start_of(&fake, timing(), Some(OWN_APP));
        eventually("the listener's end", || socket.ended().is_some()).await;
        assert_eq!(socket.ended(), Some("wrong_app"));
        eventually("the socket closed", || fake.sockets().0 == 0).await;
        // Well past the reconnect pause (50 ms): no next socket was asked for.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(*acked.lock().unwrap(), ["E0", "E1"], "the other app's envelope was acknowledged, or one before it was not");
        assert_eq!((fake.sockets().2, opened(&fake)), (1, 1), "a next socket was opened after the end");
        assert_eq!(socket.pending().0, 2, "only the bot's own app's events are queued");
        assert!(socket.finished(), "the listener's task outlived its end");
        socket.stop().await;
        fake.shutdown().await;

        // A bot whose app is unknown checks nothing: the other app's envelope is acknowledged and queued.
        let acked = Arc::new(Mutex::new(Vec::new()));
        let fake = app_fake(json!(OTHER_APP), sent, acked.clone()).await;
        let socket = start_of(&fake, timing(), None);
        eventually("every envelope acknowledged", || acked.lock().unwrap().len() == 4).await;
        assert_eq!((socket.ended(), socket.pending().0), (None, 4));
        socket.stop().await;
        fake.shutdown().await;
    })
    .await;
}

/// A `hello` naming another app ends the listener before it is live or reads an envelope; a `hello` naming none, or
/// naming one by a value that is no id (empty, a number), is live as before.
#[tokio::test]
async fn a_hello_naming_another_app_ends_the_listener_before_any_envelope() {
    bounded(async {
        let acked = Arc::new(Mutex::new(Vec::new()));
        let fake = app_fake(json!(OTHER_APP), vec![envelope("E0", 0, "hi", 0)], acked.clone()).await;
        let socket = start_of(&fake, timing(), Some(OWN_APP));
        eventually("the listener's end", || socket.ended().is_some()).await;
        assert_eq!(socket.ended(), Some("wrong_app"));
        eventually("the socket closed", || fake.sockets().0 == 0).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(!socket.status().connected, "a hello of another app made the socket live");
        assert!(acked.lock().unwrap().is_empty(), "an envelope after another app's hello was acknowledged");
        assert_eq!((socket.pending().0, fake.sockets().2, opened(&fake)), (0, 1, 1));
        socket.stop().await;
        fake.shutdown().await;

        for unnamed in [Value::Null, json!(""), json!(7)] {
            let acked = Arc::new(Mutex::new(Vec::new()));
            let fake = app_fake(unnamed.clone(), vec![envelope("E0", 0, "hi", 0)], acked.clone()).await;
            let socket = start_of(&fake, timing(), Some(OWN_APP));
            eventually("the envelope acknowledged", || acked.lock().unwrap().len() == 1).await;
            assert!(socket.status().connected, "{unnamed}");
            assert_eq!(socket.ended(), None, "{unnamed}");
            socket.stop().await;
            fake.shutdown().await;
        }
    })
    .await;
}

/// An `apps.connections.open` Slack refuses by its own code naming the token (`TOKEN_REFUSALS` or `missing_scope` in the
/// answer's body, at 200 or at 401 or 403: PS11) ends the listener for good with `app_token_refused`; a bare 401 or 403
/// with no such code (a proxy's, an edge's) and Slack's own state (`Unavailable`) are retried on the pace.
#[tokio::test]
async fn a_rejected_open_ends_the_listener_and_an_unavailable_one_is_retried() {
    bounded(async {
        let coded = |code: &str| json!({"ok": false, "error": code});
        for (status, body) in [(200, coded("invalid_auth")), (200, coded("missing_scope")), (403, coded("token_revoked")), (401, coded("not_authed"))] {
            let fake = fake_with(
                move |seen| (seen.path() == "/api/apps.connections.open").then(|| status_reply(status, &body)),
                |_, _| async {},
            )
            .await;
            let socket = start(&fake, timing());
            eventually("the listener's end", || socket.ended().is_some()).await;
            assert_eq!((socket.ended(), socket.status().error), (Some("app_token_refused"), Some(SocketError::Rejected)));
            tokio::time::sleep(Duration::from_millis(400)).await;
            assert_eq!(opened(&fake), 1, "a refused token was asked again");
            assert!(socket.finished());
            socket.stop().await;
            fake.shutdown().await;
        }
        for (status, body, error) in [
            (403, json!({}), SocketError::Rejected),
            (401, json!("Unauthorized"), SocketError::Rejected),
            (200, coded("internal_error"), SocketError::Unavailable),
        ] {
            let fake = fake_with(
                move |seen| (seen.path() == "/api/apps.connections.open").then(|| status_reply(status, &body)),
                |_, _| async {},
            )
            .await;
            let socket = start(&fake, timing());
            eventually("three tries", || opened(&fake) >= 3).await;
            assert_eq!((socket.ended(), socket.status().error), (None, Some(error)), "{status}");
            socket.stop().await;
            fake.shutdown().await;
        }
    })
    .await;
}

/// Guarantee 17's rule reads the app whatever the envelope's type: a `slash_commands` or an `interactive` envelope of
/// another app is not acknowledged and ends the listener, as an `events_api` one does.
#[tokio::test]
async fn an_envelope_of_any_type_naming_another_app_ends_the_listener() {
    bounded(async {
        for kind in ["slash_commands", "interactive"] {
            let acked = Arc::new(Mutex::new(Vec::new()));
            let mut theirs = envelope_for("E1", 1, OTHER_APP);
            theirs["type"] = json!(kind);
            let fake = app_fake(json!(OWN_APP), vec![envelope("E0", 0, "hi", 0), theirs], acked.clone()).await;
            let socket = start_of(&fake, timing(), Some(OWN_APP));
            eventually("the listener's end", || socket.ended().is_some()).await;
            assert_eq!(socket.ended(), Some("wrong_app"), "{kind}");
            eventually("the socket closed", || fake.sockets().0 == 0).await;
            assert_eq!(*acked.lock().unwrap(), ["E0"], "{kind}: another app's envelope was acknowledged");
            socket.stop().await;
            fake.shutdown().await;
        }
    })
    .await;
}
