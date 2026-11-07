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
    GapCause, Socket, SocketError, Timing, MAX_MESSAGE, MAX_PENDING, MAX_PENDING_BYTES,
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
    Socket::start_for_test(
        fake.http(),
        Secret::new(APP),
        Bot {
            user_id: "U0BOT".into(),
            bot_id: Some("B0BOT".into()),
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

/// Record the instant of every `apps.connections.open` request; `refuse` answers it `ok: false` for the first `n`.
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
        (t.len() <= refuse).then(|| Reply::json(&json!({"ok": false, "error": "invalid_auth"})))
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
                    .then(|| Reply::json(&json!({"ok": false, "error": "invalid_auth"})))
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
            socket.status().error == Some(SocketError::Rejected)
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
                    .then(|| Reply::json(&json!({"ok": false, "error": "invalid_auth"})))
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
            socket.status().error == Some(SocketError::Rejected)
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
