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
use crate::fake_slack::{eventually, FakeSlack, Identity, Reply, Seen, WsPeer};
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
    bounded(async {
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
        assert!(
            stopping.elapsed() < Duration::from_millis(1500),
            "stop is bounded by the close bound, not by the cycle"
        );
        let (_, peak, total) = fake.sockets();
        assert_eq!(peak, 1, "two sockets were open at once");
        // Shutdown prevents another connect.
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(fake.sockets().2, total, "a socket was opened after stop");
        eventually("the last socket to be gone", || fake.sockets().0 == 0).await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_peer_that_ignores_close_is_forced_down_within_the_bound() {
    bounded(async {
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
            held >= Duration::from_millis(250) && held < Duration::from_millis(1500),
            "the close was neither waited for nor bounded: {held:?}"
        );
        assert_eq!(fake.sockets().1, 1);
        socket.stop().await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_silent_socket_is_replaced_after_the_idle_bound() {
    bounded(async {
        let fake = fake(|i, mut peer| async move {
            peer.send(&hello()).await;
            if i == 0 {
                peer.ignore_until_gone().await;
            } else {
                peer.drain().await;
            }
        })
        .await;
        let began = Instant::now();
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
        // Within the bound, not merely eventually: 500 ms of idle plus a stated slack.
        assert!(
            began.elapsed() < Duration::from_millis(1500),
            "{:?}",
            began.elapsed()
        );
        assert_eq!(fake.sockets().1, 1);
        assert!(socket.gap().is_some_and(|g| g.cause == GapCause::Reconnect));
        socket.stop().await;
        fake.shutdown().await;
    })
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
    bounded(async {
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
        let fake_b = fake_with(opens(&times, 0), |_, mut peer| async move {
            peer.send(&hello()).await;
            peer.send(&disconnect("refresh_requested")).await;
            peer.drain().await;
        })
        .await;
        let socket = start(&fake_b, paced);
        eventually("three connections", || times.lock().unwrap().len() >= 3).await;
        let t = times.lock().unwrap().clone();
        assert!(
            t[1] - t[0] >= Duration::from_millis(180),
            "{:?}",
            t[1] - t[0]
        );
        assert!(
            t[2] - t[1] >= Duration::from_millis(180),
            "{:?}",
            t[2] - t[1]
        );
        // The hello reset the pause: the second reopen did not wait the doubled 400 ms.
        assert!(
            t[2] - t[1] < Duration::from_millis(390),
            "{:?}",
            t[2] - t[1]
        );
        socket.stop().await;
        fake_b.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn stop_during_the_reconnect_pause_does_not_wait_for_it() {
    bounded(async {
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
        assert!(
            began.elapsed() < Duration::from_millis(500),
            "{:?}",
            began.elapsed()
        );
        assert_eq!(fake.sockets().2, 1);
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_link_that_answers_is_kept_past_the_idle_bound() {
    bounded(async {
        let fake = fake(|_, mut peer| async move {
            peer.send(&hello()).await;
            peer.drain().await;
        })
        .await;
        let socket = start(
            &fake,
            Timing {
                idle: Duration::from_millis(500),
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
        assert_eq!(fake.sockets().2, 1, "healthy link replaced");
        assert_eq!(socket.gap(), None, "steady link reports a gap");
        assert!(socket.status().connected);
        socket.stop().await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_chatty_peer_that_never_answers_keeps_its_link() {
    bounded(async {
        let fake = fake(|_, mut peer| async move {
            peer.send(&hello()).await;
            for _ in 0..18 {
                peer.send(&json!({"type": "slash_commands_noop"})).await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            peer.ignore_until_gone().await;
        })
        .await;
        let socket = start(
            &fake,
            Timing {
                idle: Duration::from_millis(500),
                ping: Duration::from_millis(100),
                ..timing()
            },
        );
        eventually("hello", || socket.status().connected).await;
        tokio::time::sleep(Duration::from_millis(1700)).await;
        assert_eq!(fake.sockets().2, 1, "chatty link replaced");
        socket.stop().await;
        fake.shutdown().await;
    })
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
