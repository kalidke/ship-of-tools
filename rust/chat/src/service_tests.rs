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

/// `credentials` with a user token, built at run time like the others.
fn credentials_with_user(config: &Config) -> Credentials {
    let text = format!(
        "workspace = \"{WS}\"\nbot_token = \"{}\"\napp_token = \"{}\"\nuser_token = \"{}\"\n",
        ["xoxb", "-sentinel-bot-aaaa"].concat(),
        ["xapp", "-sentinel-app-bbbb"].concat(),
        ["xoxp", "-sentinel-user-cccc"].concat()
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
            crate::service::End::detached(),
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
        let ticket = watches.begin("C0AAAA", &service).0.unwrap();
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
        let ticket = watches.begin("C0AAAA", &first).0.unwrap();
        assert_eq!(watches.loaded("C0AAAA", ticket, Vec::new(), &first), Loaded::Complete);
        let second = start(&fake, &dir.0);
        // The lock is the first service's until it stops.
        assert_eq!(second.await.err(), Some(ServiceError::Held));
        first.stop(Duration::from_secs(3)).await;
        // A stopped service's content is not current, whatever the watch is called against, or never called.
        assert_eq!(watches.state("C0AAAA"), Some(State::Needed));
        watches.watch("C0BBBB").unwrap();
        assert_eq!(watches.begin("C0BBBB", &first).0, None, "a snapshot begun against a stopped service");
        assert!(!watches.reconcile(&first));
        // The next service's gap is not this watch's to clear.
        let second = start(&fake, &dir.0).await.expect("second");
        eventually("the second hello", || second.gap().is_some()).await;
        let _ = watches.pump(&second);
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
    use sot_protocol::chat::{ChatAuthor, ChatRecord, Ts};
    crate::normalize::Normalized::Edited(ChatRecord {
        workspace: WS.into(),
        channel: "C0AAAA".into(),
        ts: Ts::parse("1.000000").unwrap(),
        thread_ts: Ts::parse("1.000000").unwrap(),
        author: ChatAuthor { id: "U0AAAA".into(), name: String::new() },
        text: "x".into(),
        ..ChatRecord::fixture()
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
        let _ = watches.pump(&service);
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
        let _ = watches.pump(&service);
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
        let _ = watches.pump(&service);
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
        let t = watches.begin("C0AAAA", &service).0.unwrap();
        watches.loaded("C0AAAA", t, Vec::new(), &service);
        assert_eq!(watches.state("C0AAAA"), Some(State::Complete));
        // A loss with no event after it: only the wake can tell the pump to look.
        let (woke, ()) = tokio::join!(watches.wait(&service, Duration::from_secs(20)), async {
            tokio::task::yield_now().await;
            service.lose(GapCause::Failed);
        });
        assert_eq!(woke, Wake::Look, "the pump was not woken for a gap");
        let _ = watches.pump(&service);
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
        let t = watches.begin("C0AAAA", &service).0.unwrap();
        service.lose(GapCause::Failed);
        assert_eq!(watches.loaded("C0AAAA", t, Vec::new(), &service), Loaded::Resync);
        // A snapshot pending at the stop is answered after it: not complete.
        let t = watches.begin("C0AAAA", &service).0.unwrap();
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
        let t = watches.begin("C0AAAA", &service).0.unwrap();
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
        let t = watches.begin("C0AAAA", &service).0.unwrap();
        assert_eq!(watches.loaded("C0AAAA", t, Vec::new(), &service), Loaded::Complete);
        drop(service);
        assert_eq!(watches.state("C0AAAA"), Some(State::Needed), "a watch of a dropped service served as complete");
    })
    .await;
}

/// The one function that makes the service's `Api` from its credentials hands it the user token, so a person's post
/// has a token to go out under (the product's start and the test's are the same call).
#[tokio::test]
async fn the_service_api_holds_the_credentials_user_token() {
    bounded(async {
        let dir = Scratch::new("person");
        let fake = holding_fake(WS).await;
        let config = config();
        let url = fake.url("slack.com", "/api/");
        let with = Service::start_for_test(&config, credentials_with_user(&config), fake.http(), &dir.0, url.clone(), timing())
            .await
            .expect("service with a user token");
        // The person's post is made (Slack's answer is the fake's refusal) under the credentials' user token, not unsent.
        let sent = with.api().unwrap().post("C0AAAA", None, crate::api::PostKind::Person, "tester", "hi").await;
        assert!(sent.is_ok(), "{sent:?}");
        let by = fake.seen().into_iter().find(|s| s.path().ends_with("/chat.postMessage")).and_then(|s| s.authorization);
        assert_eq!(by.as_deref(), Some(&*format!("Bearer {}", ["xoxp", "-sentinel-user-cccc"].concat())), "the person's bearer");
        with.stop(Duration::from_secs(3)).await;
        let without = Service::start_for_test(&config, credentials(&config), fake.http(), &dir.0, url, timing())
            .await
            .expect("service without one");
        let unsent = without.api().unwrap().post("C0AAAA", None, crate::api::PostKind::Person, "tester", "hi").await;
        assert_eq!(unsent, Err(crate::api::ApiError::NoPersonToken));
        assert_eq!(fake.seen().iter().filter(|s| s.path().ends_with("/chat.postMessage")).count(), 1, "nothing sent without a token");
        without.stop(Duration::from_secs(3)).await;
        fake.shutdown().await;
    })
    .await;
}

/// A fake for a service's own ends: `auth.test` as `WS`, `bots.info` naming the app `A0OWN`, a socket whose hello names
/// `hello_app` once `go` is raised, and `conversations.list`, `chat.postMessage`, `conversations.join` and a file's first
/// step answering Slack's code `refusal` while one is set (`http_403`: a bare 403 with no code).
async fn ends_fake(
    hello_app: &'static str,
    go: std::sync::Arc<std::sync::atomic::AtomicBool>,
    refusal: std::sync::Arc<std::sync::Mutex<Option<&'static str>>>,
) -> FakeSlack {
    use std::sync::atomic::Ordering;
    FakeSlack::start(Identity::Trusted, move |seen, port| {
        let refused = *refusal.lock().unwrap();
        match (seen.path().as_str(), refused) {
            ("/api/auth.test", _) => {
                return Reply::json(&json!({"ok": true, "team_id": WS, "user_id": "U0BOT", "bot_id": "B0BOT"}))
            }
            ("/api/bots.info", _) => return Reply::json(&json!({"ok": true, "bot": {"id": "B0BOT", "app_id": "A0OWN"}})),
            ("/api/apps.connections.open", _) => {
                let url = format!("wss://wss-primary.slack.com:{port}/link/?ticket=sentinel-ticket");
                return Reply::json(&json!({"ok": true, "url": url}));
            }
            ("/api/conversations.list" | "/api/chat.postMessage" | "/api/conversations.join" | "/api/files.getUploadURLExternal", Some(code)) => {
                return if code == "http_403" {
                    Reply::Status { code: 403, headers: Vec::new(), body: b"{}".to_vec() }
                } else {
                    Reply::json(&json!({"ok": false, "error": code}))
                };
            }
            ("/api/conversations.list", None) => {
                return Reply::json(&json!({"ok": true, "channels": [], "response_metadata": {"next_cursor": ""}}))
            }
            ("/api/chat.postMessage", None) => return Reply::json(&json!({"ok": true, "ts": "1700000000.000100"})),
            _ => {}
        }
        if seen.upgrade {
            let go = go.clone();
            return Reply::socket(move |mut peer| {
                let go = go.clone();
                async move {
                    while !go.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    peer.send(&json!({"type": "hello", "num_connections": 1, "connection_info": {"app_id": hello_app}})).await;
                    peer.drain().await;
                }
            });
        }
        Reply::json(&json!({"ok": false, "error": "unexpected"}))
    })
    .await
}

/// C15, rule 4: a bot-token call Slack refuses with a code of `TOKEN_REFUSALS` (a read, a row's post) ends the service
/// `bot_token_refused`, for good; `missing_scope`, a person's token refused, and a client closed by a stop end nothing.
/// The bot's app is read at the start (`app_unchecked` is `None`), and unknown when `bots.info` fails.
#[tokio::test]
async fn a_bot_token_refusal_ends_the_service_and_a_missing_scope_does_not() {
    use crate::api::{ApiError, PostKind, PostOutcome};
    use crate::service::BOT_TOKEN_REFUSED;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    bounded(async {
        let dir = Scratch::new("refused");
        // The socket says hello at once, as the bot's own app, so a stop's close is answered.
        let (go, refusal) = (Arc::new(AtomicBool::new(true)), Arc::new(Mutex::new(None)));
        let fake = ends_fake("A0OWN", go.clone(), refusal.clone()).await;
        let config = config();
        let url = fake.url("slack.com", "/api/");
        let start_with_user = || {
            Service::start_for_test(&config, credentials_with_user(&config), fake.http(), &dir.0, url.clone(), timing())
        };

        let service = start_with_user().await.expect("service");
        assert_eq!(service.app_unchecked(), None, "the bot's app was not read at the start");
        let api = service.api().unwrap();
        *refusal.lock().unwrap() = Some("missing_scope");
        assert_eq!(api.channels().await, Err(ApiError::Slack("missing_scope".into())));
        *refusal.lock().unwrap() = Some("invalid_auth");
        let person = api.post("C0AAAA", None, PostKind::Person, "tester", "hi").await;
        assert_eq!(person, Ok(PostOutcome::Refused("invalid_auth".into())));
        // A bare 403, with no Slack code, names nothing (PS11).
        *refusal.lock().unwrap() = Some("http_403");
        assert_eq!(api.channels().await, Err(ApiError::Status(403)));
        assert_eq!(api.post("C0AAAA", None, PostKind::Row, "row-a", "hi").await, Ok(PostOutcome::Refused("http_403".into())));
        assert_eq!((service.ended(), service.api().is_ok()), (None, true), "a scope, a person's token or a bare 403 ended the service");
        *refusal.lock().unwrap() = Some("account_inactive");
        let row = api.post("C0AAAA", None, PostKind::Row, "row-a", "hi").await;
        assert_eq!(row, Ok(PostOutcome::Refused("account_inactive".into())));
        assert_eq!(service.ended(), Some(BOT_TOKEN_REFUSED));
        assert_eq!(service.api().err(), Some(ServiceError::Stopped));
        assert_eq!(api.channels().await, Err(ApiError::Closed), "a handle of an ended service still reached Slack");
        eventually("stopped", || service.status() == Status::Stopped).await;
        drop(service);

        // A read refused the same way ends the next service.
        *refusal.lock().unwrap() = None;
        let service = start_with_user().await.expect("the lock was freed by the end");
        *refusal.lock().unwrap() = Some("token_revoked");
        assert_eq!(service.api().unwrap().channels().await, Err(ApiError::Slack("token_revoked".into())));
        assert_eq!(service.ended(), Some(BOT_TOKEN_REFUSED));
        service.stop(Duration::from_secs(3)).await;

        // A client closed by a stop answers `closed`, and the stop is no end.
        *refusal.lock().unwrap() = None;
        let service = start_with_user().await.expect("third");
        let api = service.api().unwrap();
        service.stop(Duration::from_secs(3)).await;
        assert_eq!(api.channels().await, Err(ApiError::Closed));
        assert_eq!(service.ended(), None, "a stop was read as an end");
        fake.shutdown().await;

        // `bots.info` refused: the app is unknown, said by a fixed code, and the service runs.
        let other = holding_fake(WS).await;
        let service = start(&other, &dir.0).await.expect("service");
        assert_eq!((service.app_unchecked(), service.ended()), (Some("slack"), None));
        service.stop(Duration::from_secs(3)).await;
        other.shutdown().await;
    })
    .await;
}

/// C15, rule 5: a service ended on its own (here a hello of another app) answers as a stopped one: a wait in flight
/// answers `Stopped`, its channels owe a snapshot, nothing is taken, no new `Watches` is given, and its lock is freed
/// with the listener's task, so a second service of the workspace starts.
#[tokio::test]
async fn an_ended_service_ends_its_watches_wait_and_frees_its_lock() {
    use crate::service::WRONG_APP;
    use crate::watch::{State, Wake};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    bounded(async {
        let dir = Scratch::new("ended");
        let go = Arc::new(AtomicBool::new(false));
        let fake = ends_fake("A0OTHER", go.clone(), Arc::new(Mutex::new(None))).await;
        let service = Arc::new(start(&fake, &dir.0).await.expect("service"));
        let mut watches = service.watches().unwrap();
        watches.watch("C0AAAA").unwrap();
        let s = service.clone();
        let waiting = tokio::spawn(async move {
            let wake = watches.wait(&s, Duration::from_secs(20)).await;
            (watches, wake)
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!waiting.is_finished(), "the wait was meant to be in flight");
        go.store(true, Ordering::SeqCst);
        let (mut watches, wake) = waiting.await.unwrap();
        assert_eq!(wake, Wake::Stopped);
        assert_eq!(service.ended(), Some(WRONG_APP));
        assert_eq!(service.api().err(), Some(ServiceError::Stopped));
        assert_eq!(watches.state("C0AAAA"), Some(State::Needed));
        let (ticket, arrived) = watches.begin("C0AAAA", &service);
        assert_eq!((ticket, arrived.records.len()), (None, 0));
        assert_eq!(watches.wait(&service, Duration::from_secs(20)).await, Wake::Stopped);
        drop(watches);
        assert!(service.watches().is_none(), "an ended service gave a Watches");
        eventually("stopped", || service.status() == Status::Stopped).await;
        // The lock went with the listener's task.
        let next = holding_fake(WS).await;
        let second = start(&next, &dir.0).await.expect("the lock was freed by the end");
        second.stop(Duration::from_secs(3)).await;
        service.stop(Duration::from_secs(3)).await;
        assert_eq!(service.status(), Status::Stopped);
        next.shutdown().await;
        fake.shutdown().await;
    })
    .await;
}

/// Guarantee 22's two other bot-token calls: a join and a row's file step refused with a code of `TOKEN_REFUSALS` each
/// end the service `bot_token_refused`.
#[tokio::test]
async fn a_refused_join_or_file_step_ends_the_service() {
    use crate::api::{FileOutcome, JoinOutcome, PostKind};
    use crate::service::BOT_TOKEN_REFUSED;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    bounded(async {
        let dir = Scratch::new("join-file");
        let refusal = Arc::new(Mutex::new(None));
        let fake = ends_fake("A0OWN", Arc::new(AtomicBool::new(true)), refusal.clone()).await;
        let service = start(&fake, &dir.0).await.expect("service");
        *refusal.lock().unwrap() = Some("invalid_auth");
        assert_eq!(service.api().unwrap().join("C0AAAA").await, Ok(JoinOutcome::Refused("invalid_auth".into())));
        assert_eq!(service.ended(), Some(BOT_TOKEN_REFUSED), "a refused join");
        service.stop(Duration::from_secs(3)).await;

        *refusal.lock().unwrap() = None;
        let service = start(&fake, &dir.0).await.expect("second");
        let api = service.api().unwrap();
        *refusal.lock().unwrap() = Some("token_expired");
        let posted = api.post_file(api.transfer().unwrap(), "C0AAAA", None, PostKind::Row, "row-a", "a.png", vec![1, 2, 3], None).await;
        assert_eq!(posted, Ok(FileOutcome::Refused("token_expired".into())));
        assert_eq!(service.ended(), Some(BOT_TOKEN_REFUSED), "a refused file step");
        service.stop(Duration::from_secs(3)).await;
        fake.shutdown().await;
    })
    .await;
}

/// The claim is the one arbiter between a stop and an end: once it is ended (a stop began, or an end came first), no
/// end sets a code or replaces the first.
#[test]
fn an_end_after_the_claim_ended_is_no_end_and_the_first_end_stays() {
    use crate::service::{End, ENDED, WRONG_APP};
    use std::sync::atomic::AtomicU8;
    use std::sync::Arc;
    let stopped = End::new(Arc::new(AtomicU8::new(ENDED)));
    stopped.end(WRONG_APP);
    assert_eq!(stopped.code(), None, "an end after a stop began");
    let claim = Arc::new(AtomicU8::new(crate::service::HELD));
    let end = End::new(claim.clone());
    end.end(WRONG_APP);
    end.end("bot_token_refused");
    assert_eq!((end.code(), claim.load(std::sync::atomic::Ordering::SeqCst)), (Some(WRONG_APP), ENDED));
}
