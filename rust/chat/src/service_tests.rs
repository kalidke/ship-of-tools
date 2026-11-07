//! Behavioral tests of `service` against the scripted fake Slack. Every id and token is synthetic, every folder is the
//! test's own, and every wait is bounded.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;

use crate::config::{Config, Credentials};
use crate::fake_slack::{eventually, FakeSlack, Identity, Reply, WsPeer};
use crate::service::{Service, ServiceError, Status};
use crate::socket::Timing;

const WS: &str = "T0TEAM";

/// Every test body runs under one bound: a hung fixture fails here instead of waiting.
async fn bounded<T>(body: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), body)
        .await
        .expect("the test exceeded its bound")
}

/// A scratch folder that removes itself.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("sot-chat-service-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config() -> Config {
    Config::parse(&format!("workspace = \"{WS}\"\nchannels = [\"C0AAAA\"]\n"), &|_| None).unwrap()
}

/// Built at run time, so no source line holds a token-shaped literal for the secret scanner.
fn credentials(config: &Config) -> Credentials {
    let text = format!(
        "workspace = \"{WS}\"\nbot_token = \"{}\"\napp_token = \"{}\"\n",
        ["xoxb", "-sentinel-bot-aaaa"].concat(),
        ["xapp", "-sentinel-app-bbbb"].concat()
    );
    Credentials::parse(&text, config).unwrap()
}

fn timing() -> Timing {
    Timing {
        open: Duration::from_secs(5),
        close: Duration::from_secs(600),
        write: Duration::from_secs(2),
        idle: Duration::from_secs(60),
        ping: Duration::from_secs(30),
        backoff_first: Duration::from_millis(50),
        backoff_max: Duration::from_millis(200),
        stop: Duration::from_secs(3),
    }
}

/// A fake that answers `auth.test` as workspace `team`, opens a socket, says hello and then `script`.
async fn fake<F, Fut>(team: &'static str, script: F) -> FakeSlack
where
    F: Fn(WsPeer) -> Fut + Send + Sync + Clone + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    FakeSlack::start(Identity::Trusted, move |seen, port| {
        match seen.path().as_str() {
            "/api/auth.test" => {
                return Reply::json(&json!({"ok": true, "team_id": team, "user_id": "U0BOT", "bot_id": "B0BOT"}))
            }
            "/api/apps.connections.open" => {
                let url = format!("wss://wss-primary.slack.com:{port}/link/?ticket=sentinel-ticket");
                return Reply::json(&json!({"ok": true, "url": url}));
            }
            _ => {}
        }
        if seen.upgrade {
            let script = script.clone();
            return Reply::socket(move |mut peer| {
                let script = script.clone();
                async move {
                    peer.send(&json!({"type": "hello", "num_connections": 1})).await;
                    script(peer).await
                }
            });
        }
        Reply::json(&json!({"ok": false, "error": "unexpected"}))
    })
    .await
}

async fn holding_fake(team: &'static str) -> FakeSlack {
    fake(team, |mut peer| async move { peer.drain().await }).await
}

async fn start(fake: &FakeSlack, dir: &std::path::Path) -> Result<Service, ServiceError> {
    let config = config();
    Service::start_for_test(&config, credentials(&config), fake.http(), dir, fake.url("slack.com", "/api/"), timing()).await
}

fn calls(fake: &FakeSlack) -> usize {
    fake.seen().len()
}

#[tokio::test]
async fn duplicate_workspace_or_app_has_one_listener() {
    bounded(async {
        let dir = Scratch::new("duplicate");
        let fake = holding_fake(WS).await;
        let first = start(&fake, &dir.0).await.expect("the first service");
        eventually("the first socket", || fake.sockets().2 == 1).await;
        let before = calls(&fake);
        // A second service for the workspace, in this process: refused with no call and no socket.
        assert_eq!(start(&fake, &dir.0).await.err(), Some(ServiceError::Held));
        assert_eq!((calls(&fake), fake.sockets().2), (before, 1), "a refused service reached Slack");
        // Stopped, the workspace is free again, and the new service is the only one.
        first.stop(Duration::from_secs(3)).await;
        let second = start(&fake, &dir.0).await.expect("start after stop");
        second.stop(Duration::from_secs(3)).await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_token_for_another_workspace_is_refused_before_serving() {
    bounded(async {
        let dir = Scratch::new("wrong");
        let fake = holding_fake("T0OTHER").await;
        assert_eq!(start(&fake, &dir.0).await.err(), Some(ServiceError::Api(crate::api::ApiError::WrongWorkspace)));
        assert_eq!(fake.sockets().2, 0, "a socket opened for the wrong workspace");
        // The refusal did not keep the lock.
        let good = holding_fake(WS).await;
        let service = start(&good, &dir.0).await.expect("the lock was released");
        service.stop(Duration::from_secs(3)).await;
        good.shutdown().await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn stop_is_bounded_final_and_repeatable_and_the_lock_waits_for_the_listener() {
    bounded(async {
        let dir = Scratch::new("stop");
        // A peer that never answers a close: the stop's bound, not the 30 s close, ends the listener.
        let fake = fake(WS, |mut peer| async move { peer.ignore_until_gone().await }).await;
        let service = std::sync::Arc::new(start(&fake, &dir.0).await.expect("service"));
        eventually("connected", || matches!(service.status(), Status::Running { socket, .. } if socket.connected)).await;
        assert!(matches!(service.status(), Status::Running { gap: true, .. }), "a hello owes a reconciliation");
        let held = service.api().expect("api while running");
        let stopping = {
            let s = service.clone();
            tokio::spawn(async move { s.stop(Duration::from_secs(3)).await })
        };
        eventually("stopping", || service.status() == Status::Stopping).await;
        assert_eq!(service.api().err(), Some(ServiceError::Stopped));
        // While the stop is under way the listener may still run, so the lock is held.
        assert_eq!(start(&fake, &dir.0).await.err(), Some(ServiceError::Held));
        stopping.await.unwrap();
        assert_eq!(service.status(), Status::Stopped);
        // Repeatable and final.
        service.stop(Duration::from_secs(3)).await;
        assert_eq!(service.api().err(), Some(ServiceError::Stopped));
        drop(held);
        // The lock is free exactly now.
        start(&fake, &dir.0).await.expect("the lock after the stop").stop(Duration::from_secs(3)).await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_stop_before_the_socket_connected_returns() {
    bounded(async {
        let dir = Scratch::new("early");
        // The socket's upgrade is never answered by a hello: the service starts, the socket does not connect.
        let fake = FakeSlack::start(Identity::Trusted, |seen, _| match seen.path().as_str() {
            "/api/auth.test" => Reply::json(&json!({"ok": true, "team_id": WS, "user_id": "U0BOT"})),
            _ => Reply::Hang,
        })
        .await;
        let service = start(&fake, &dir.0).await.expect("service");
        assert!(matches!(service.status(), Status::Running { socket, .. } if !socket.connected));
        service.stop(Duration::from_secs(3)).await;
        assert_eq!(service.status(), Status::Stopped);
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_dropped_service_releases_the_lock_only_with_its_listener() {
    bounded(async {
        let dir = Scratch::new("drop");
        let fake = holding_fake(WS).await;
        let first = start(&fake, &dir.0).await.expect("service");
        eventually("connected", || fake.sockets().0 == 1).await;
        drop(first);
        // The lock goes with the aborted task's future, so the next service is admitted and the first is gone.
        let second = start(&fake, &dir.0).await.expect("the lock after a drop");
        eventually("one socket open", || fake.sockets().0 == 1).await;
        second.stop(Duration::from_secs(3)).await;
        fake.shutdown().await;
    })
    .await;
}

/// A child process of this test binary that starts a service and waits to be killed.
#[cfg(unix)]
#[tokio::test]
async fn a_killed_holder_leaves_the_workspace_free() {
    use sot_platform::test_isolated::{enter, test_command};
    const NAME: &str = "service_tests::a_killed_holder_leaves_the_workspace_free";
    const CHILD: &str = "SOT_CHAT_SERVICE_CHILD";
    if let Some(dir) = std::env::var_os(CHILD) {
        enter(NAME);
        bounded(async {
            let dir = PathBuf::from(dir);
            let fake = holding_fake(WS).await;
            let _service = start(&fake, &dir).await.expect("the child's service");
            std::fs::write(dir.join("ready"), b"1").unwrap();
            tokio::time::sleep(Duration::from_secs(50)).await;
        })
        .await;
        return;
    }
    bounded(async {
        let dir = Scratch::new("killed");
        let (mut command, entry) = test_command(NAME);
        command.env(CHILD, &dir.0).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let mut child = command.spawn().expect("spawn the child");
        eventually("the child's service", || dir.0.join("ready").exists()).await;
        let fake = holding_fake(WS).await;
        assert_eq!(start(&fake, &dir.0).await.err(), Some(ServiceError::Held), "a second process was admitted");
        child.kill().expect("kill the holder");
        child.wait().expect("reap the holder");
        entry.assert_once(child.id());
        let service = start(&fake, &dir.0).await.expect("a killed holder left the workspace locked");
        service.stop(Duration::from_secs(3)).await;
        fake.shutdown().await;
    })
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_child_started_by_the_daemon_does_not_keep_the_lock() {
    bounded(async {
        let dir = Scratch::new("inherit");
        let fake = holding_fake(WS).await;
        let service = start(&fake, &dir.0).await.expect("service");
        // Born while the lock is held, and alive after the service ends.
        let mut child = std::process::Command::new("sleep").arg("30").spawn().expect("spawn sleep");
        service.stop(Duration::from_secs(3)).await;
        let again = start(&fake, &dir.0).await;
        child.kill().expect("kill the child");
        child.wait().expect("reap the child");
        again.expect("a child kept the lock's descriptor").stop(Duration::from_secs(3)).await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_handle_kept_across_stop_is_refused_by_every_call() {
    bounded(async {
        let dir = Scratch::new("handle");
        let fake = holding_fake(WS).await;
        let service = start(&fake, &dir.0).await.expect("service");
        let held = service.api().expect("api while running");
        service.stop(Duration::from_secs(3)).await;
        let before = calls(&fake);
        assert_eq!(held.auth_test().await.err(), Some(crate::api::ApiError::Closed));
        let post = held.post("C0AAAA", None, crate::api::PostKind::Row, "a", "t").await;
        assert_eq!(post.err(), Some(crate::api::ApiError::Closed));
        assert_eq!(calls(&fake), before, "a stopped service's handle reached Slack");
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn the_lock_is_dropped_with_the_task_before_stop_returns() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Flag(std::sync::Arc<AtomicBool>);
    impl Drop for Flag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    bounded(async {
        let fake = fake(WS, |mut peer| async move { peer.ignore_until_gone().await }).await;
        let config = config();
        let creds = credentials(&config);
        let dropped = std::sync::Arc::new(AtomicBool::new(false));
        let api = crate::api::Api::new(fake.http(), creds.bot, WS).unwrap().with_base(fake.url("slack.com", "/api/"));
        let own = api.auth_test().await.unwrap();
        let socket = crate::socket::Socket::launch(
            fake.http(), creds.app, own, WS, fake.url("slack.com", "/api/"), timing(), Box::new(Flag(dropped.clone())),
        )
        .unwrap();
        eventually("connected", || socket.status().connected).await;
        assert!(!dropped.load(Ordering::SeqCst));
        // 200 ms is the bound asked; the close bound is 600 s and the guard here 20 s, so only `within` can end it.
        tokio::time::timeout(Duration::from_secs(20), socket.stop_within(Duration::from_millis(200)))
            .await
            .expect("stop_within ignored its bound");
        assert!(dropped.load(Ordering::SeqCst), "stop returned while the task could still hold the lock");
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_lock_that_cannot_be_made_is_not_held_and_the_codes_are_fixed() {
    bounded(async {
        let fake = holding_fake(WS).await;
        let missing = std::env::temp_dir().join(format!("sot-chat-service-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);
        let err = start(&fake, &missing).await.err();
        assert_eq!(err, Some(ServiceError::Lock));
        assert_eq!(calls(&fake), 0, "a refused service reached Slack");
        assert_eq!(
            [ServiceError::Held.code(), ServiceError::Lock.code(), ServiceError::Stopped.code()],
            ["held", "lock", "stopped"]
        );
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_concurrent_stop_returns_at_once_and_the_status_reaches_stopped() {
    bounded(async {
        let dir = Scratch::new("concurrent");
        let fake = fake(WS, |mut peer| async move { peer.ignore_until_gone().await }).await;
        let service = std::sync::Arc::new(start(&fake, &dir.0).await.expect("service"));
        eventually("connected", || matches!(service.status(), Status::Running { socket, .. } if socket.connected)).await;
        let handle = service.api().expect("api while running");
        let first = {
            let s = service.clone();
            tokio::spawn(async move { s.stop(Duration::from_secs(1)).await })
        };
        eventually("stopping", || service.status() == Status::Stopping).await;
        // A handle is refused from the moment the stop began, while the listener may still run.
        let before = calls(&fake);
        assert_eq!(handle.auth_test().await.err(), Some(crate::api::ApiError::Closed));
        assert_eq!(calls(&fake), before, "a handle reached Slack during the stop");
        service.stop(Duration::from_secs(1)).await;
        assert_eq!(service.status(), Status::Stopping, "the second stop disturbed the first");
        first.await.unwrap();
        assert_eq!(service.status(), Status::Stopped);
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_cancelled_stop_frees_the_lock() {
    bounded(async {
        let dir = Scratch::new("cancel");
        let fake = fake(WS, |mut peer| async move { peer.ignore_until_gone().await }).await;
        let service = start(&fake, &dir.0).await.expect("service");
        eventually("connected", || matches!(service.status(), Status::Running { socket, .. } if socket.connected)).await;
        // The peer never answers the close and the close bound is 600 s: the stop is still waiting when it is cancelled.
        assert!(tokio::time::timeout(Duration::from_millis(200), service.stop(Duration::from_secs(30))).await.is_err());
        assert_eq!(service.status(), Status::Stopping);
        start(&fake, &dir.0).await.expect("a cancelled stop left the listener holding the lock").stop(Duration::from_secs(3)).await;
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn the_watch_clears_the_services_gap_and_a_stopped_service_clears_nothing() {
    use crate::watch::Loaded;
    bounded(async {
        let dir = Scratch::new("reconcile");
        let fake = holding_fake(WS).await;
        let service = start(&fake, &dir.0).await.expect("service");
        eventually("the hello's gap", || service.gap().is_some()).await;
        let mut watches = service.watches().unwrap();
        watches.watch("C0AAAA").unwrap();
        let ticket = watches.begin("C0AAAA", &service).unwrap();
        // The snapshot is not complete yet, so nothing clears.
        assert!(!watches.reconcile(&service));
        assert_eq!(watches.loaded("C0AAAA", ticket, Vec::new(), &service), Loaded::Complete);
        let gap = service.gap().unwrap();
        assert!(watches.reconcile(&service));
        assert!(matches!(service.status(), Status::Running { gap: false, .. }));
        service.stop(Duration::from_secs(3)).await;
        assert_eq!((service.gap(), service.reconciled(gap)), (None, false));
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_stop_makes_every_watch_owed_a_snapshot_and_a_stranger_clears_nothing() {
    use crate::watch::{Loaded, State};
    bounded(async {
        let dir = Scratch::new("outlive");
        let fake = holding_fake(WS).await;
        let first = start(&fake, &dir.0).await.expect("first");
        eventually("the first hello", || first.gap().is_some()).await;
        let mut watches = first.watches().unwrap();
        watches.watch("C0AAAA").unwrap();
        let ticket = watches.begin("C0AAAA", &first).unwrap();
        assert_eq!(watches.loaded("C0AAAA", ticket, Vec::new(), &first), Loaded::Complete);
        let second = start(&fake, &dir.0);
        // The lock is the first service's until it stops.
        assert_eq!(second.await.err(), Some(ServiceError::Held));
        first.stop(Duration::from_secs(3)).await;
        // A stopped service's content is not current, whatever the watch is called against, or never called.
        assert_eq!(watches.state("C0AAAA"), Some(State::Needed));
        watches.watch("C0BBBB").unwrap();
        assert_eq!(watches.begin("C0BBBB", &first), None, "a snapshot begun against a stopped service");
        assert!(!watches.reconcile(&first));
        // The next service's gap is not this watch's to clear.
        let second = start(&fake, &dir.0).await.expect("second");
        eventually("the second hello", || second.gap().is_some()).await;
        watches.pump(&second);
        assert_eq!(watches.state("C0AAAA"), Some(State::Needed));
        assert!(!watches.reconcile(&second));
        assert!(second.gap().is_some());
        second.stop(Duration::from_secs(3)).await;
        fake.shutdown().await;
    })
    .await;
}

fn inert() -> Service {
    use crate::api::Api;
    use crate::http::{Http, Secret};
    Service::inert(std::sync::Arc::new(Api::new(Http::new().unwrap(), Secret::new("xoxb-test"), WS).unwrap()))
}

fn edit() -> crate::normalize::Normalized {
    use sot_protocol::chat::{ChatAuthor, ChatRecord, Origin, Ts};
    crate::normalize::Normalized::Edited(ChatRecord {
        workspace: WS.into(),
        channel: "C0AAAA".into(),
        ts: Ts::parse("1.000000").unwrap(),
        thread_ts: Ts::parse("1.000000").unwrap(),
        author: ChatAuthor { id: "U0AAAA".into(), name: String::new() },
        origin: Origin::Member,
        text: "x".into(),
        truncated: false,
        reply_count: 0,
        files: Vec::new(),
        latest_reply: None,
        limited: false,
        omitted: false,
        files_omitted: 0,
    })
}

#[tokio::test]
async fn wait_wakes_for_a_gap_and_an_event_sees_what_is_queued_and_ends_with_the_stop() {
    use crate::socket::GapCause;
    use crate::watch::Wake;
    bounded(async {
        let service = inert();
        let mut watches = service.watches().unwrap();
        let short = Duration::from_millis(100);
        service.lose(GapCause::Reconnect);
        // Another service's Watches takes no wake from this one, so the owner's is still there.
        let mut stranger = inert().watches().unwrap();
        assert_eq!(stranger.wait(&service, short).await, Wake::Stopped, "a stranger waited on another service");
        assert_eq!(watches.wait(&service, Duration::from_secs(20)).await, Wake::Look, "no wake for a gap");
        service.feed(edit());
        assert_eq!(watches.wait(&service, Duration::from_secs(20)).await, Wake::Look, "no wake for an event");
        // Nothing was taken, so what is queued is still seen at once, with no new wake.
        assert_eq!(watches.wait(&service, short).await, Wake::Look, "a queued event was not seen");
        watches.pump(&service);
        service.stop(Duration::from_secs(3)).await;
        assert_eq!(watches.wait(&service, short).await, Wake::Stopped, "a stopped service did not say so");
        assert_eq!(watches.wait(&service, short).await, Wake::Stopped);
    })
    .await;
}

#[tokio::test]
async fn a_wait_in_flight_ends_at_the_stop() {
    use crate::socket::GapCause;
    use crate::watch::Wake;
    bounded(async {
        let service = inert();
        let mut watches = service.watches().unwrap();
        service.lose(GapCause::Reconnect);
        watches.pump(&service);
        // The permit the latch left is spent, so the wait below has nothing to see.
        watches.wait(&service, Duration::ZERO).await;
        // A zero bound polls the wait once and gives up: no clock decides the result.
        let mut wait = Box::pin(watches.wait(&service, Duration::from_secs(20)));
        assert!(tokio::time::timeout(Duration::ZERO, &mut wait).await.is_err(), "the wait had something to see");
        service.stop(Duration::from_secs(3)).await;
        assert_eq!(tokio::time::timeout(Duration::ZERO, &mut wait).await, Ok(Wake::Stopped), "the stop did not end the wait");
    })
    .await;
}

#[tokio::test]
async fn a_wait_woken_while_the_stop_is_under_way_answers_false() {
    use crate::watch::Wake;
    bounded(async {
        let dir = Scratch::new("stopping-wait");
        // A peer that never answers a close: the service is `Stopping` for the stop's whole bound.
        let fake = fake(WS, |mut peer| async move { peer.ignore_until_gone().await }).await;
        let service = start(&fake, &dir.0).await.expect("service");
        let mut watches = service.watches().unwrap();
        eventually("connected", || matches!(service.status(), Status::Running { socket, .. } if socket.connected)).await;
        watches.pump(&service);
        watches.wait(&service, Duration::ZERO).await;
        let (woke, ()) = tokio::join!(watches.wait(&service, Duration::from_secs(20)), async {
            tokio::task::yield_now().await;
            service.stop(Duration::from_secs(3)).await;
        });
        assert_eq!(woke, Wake::Stopped, "a wait woken by the stop answered anything else while the service was stopping");
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_pump_loop_beside_a_latched_gap_sees_it() {
    use crate::socket::GapCause;
    use crate::watch::{State, Wake};
    bounded(async {
        let service = inert();
        let mut watches = service.watches().unwrap();
        watches.watch("C0AAAA").unwrap();
        let t = watches.begin("C0AAAA", &service).unwrap();
        watches.loaded("C0AAAA", t, Vec::new(), &service);
        assert_eq!(watches.state("C0AAAA"), Some(State::Complete));
        // A loss with no event after it: only the wake can tell the pump to look.
        let (woke, ()) = tokio::join!(watches.wait(&service, Duration::from_secs(20)), async {
            tokio::task::yield_now().await;
            service.lose(GapCause::Failed);
        });
        assert_eq!(woke, Wake::Look, "the pump was not woken for a gap");
        watches.pump(&service);
        assert_eq!(watches.state("C0AAAA"), Some(State::Needed));
    })
    .await;
}


#[tokio::test]
async fn loaded_reads_its_own_services_gap_and_a_stop_ends_what_was_pending_and_the_claim() {
    use crate::socket::GapCause;
    use crate::watch::{Loaded, State};
    bounded(async {
        let service = inert();
        let mut watches = service.watches().unwrap();
        watches.watch("C0AAAA").unwrap();
        // A loss after the take, unseen by any pump: the answer reads the service's gap and is refused.
        let t = watches.begin("C0AAAA", &service).unwrap();
        service.lose(GapCause::Failed);
        assert_eq!(watches.loaded("C0AAAA", t, Vec::new(), &service), Loaded::Resync);
        // A snapshot pending at the stop is answered after it: not complete.
        let t = watches.begin("C0AAAA", &service).unwrap();
        service.stop(Duration::from_secs(3)).await;
        assert_eq!(watches.loaded("C0AAAA", t, Vec::new(), &service), Loaded::Stale);
        assert_eq!(watches.state("C0AAAA"), Some(State::Needed));
        // The claim outlives the stop and the `Watches`: a stopped service has none to give.
        assert!(service.watches().is_none(), "the stop released the claim");
        drop(watches);
        assert!(service.watches().is_none(), "a dropped Watches freed a stopped service's claim");
    })
    .await;
}

#[tokio::test]
async fn a_stop_under_way_already_ends_the_watch() {
    use crate::watch::{State, Wake};
    bounded(async {
        let dir = Scratch::new("stopping-state");
        // A peer that never answers a close, and a stop whose caller gives up: the service stays `Stopping`.
        let fake = fake(WS, |mut peer| async move { peer.ignore_until_gone().await }).await;
        let service = start(&fake, &dir.0).await.expect("service");
        let mut watches = service.watches().unwrap();
        eventually("connected", || matches!(service.status(), Status::Running { socket, .. } if socket.connected)).await;
        watches.watch("C0AAAA").unwrap();
        let t = watches.begin("C0AAAA", &service).unwrap();
        watches.loaded("C0AAAA", t, Vec::new(), &service);
        assert_eq!(watches.state("C0AAAA"), Some(State::Complete));
        assert!(tokio::time::timeout(Duration::ZERO, service.stop(Duration::from_secs(3))).await.is_err());
        assert_eq!(service.status(), Status::Stopping);
        assert_eq!(watches.state("C0AAAA"), Some(State::Needed), "a service on its way to stopped still served as complete");
        assert_eq!(watches.wait(&service, Duration::ZERO).await, Wake::Stopped);
        fake.shutdown().await;
    })
    .await;
}

#[tokio::test]
async fn a_dropped_service_ends_its_watch() {
    use crate::watch::{Loaded, State};
    bounded(async {
        let service = inert();
        let mut watches = service.watches().unwrap();
        watches.watch("C0AAAA").unwrap();
        let t = watches.begin("C0AAAA", &service).unwrap();
        assert_eq!(watches.loaded("C0AAAA", t, Vec::new(), &service), Loaded::Complete);
        drop(service);
        assert_eq!(watches.state("C0AAAA"), Some(State::Needed), "a watch of a dropped service served as complete");
    })
    .await;
}
