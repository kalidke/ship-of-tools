//! The REPL's port announcements: a `browser` frame's port is granted through the injected capability before the frame
//! is forwarded, a refusal is visible at the run, and a new handle's generations start above the last.

use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use super::*;
use crate::sidecars::ports::{PortEvent, PortGrants, PortRefused, PortSink};
#[cfg(unix)]
use crate::sidecars::contract_tests::{isolated, OwnedRoot};
#[cfg(unix)]
use serde_json::{json, Value};
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
const BODY: Duration = Duration::from_secs(120);

/// A `julia` stand-in that answers every request with a `browser` frame for eval 7.
#[cfg(unix)]
const BROWSER_STUB: &str = "#!/bin/sh\nwhile IFS= read -r line; do\n  printf '%s\\n' '{\"id\":0,\"kind\":\"evt\",\"op\":\"repl.frame\",\"payload\":{\"eval_id\":7,\"frame\":{\"kind\":\"browser\",\"url\":\"http://127.0.0.1:45621/page\"}}}'\ndone\n";

/// A receiver that keeps every event, says whether the frame was already on the bus when each arrived, and answers with
/// `refuse` when set.
pub(super) struct Recorder {
    events: Mutex<Vec<(PortEvent, bool)>>,
    bus: Mutex<broadcast::Receiver<ReplFrameMsg>>,
    refuse: Option<PortRefused>,
}

impl Recorder {
    pub(super) fn new(bus: broadcast::Receiver<ReplFrameMsg>, refuse: Option<PortRefused>) -> Arc<Self> {
        Arc::new(Self { events: Mutex::new(Vec::new()), bus: Mutex::new(bus), refuse })
    }

    pub(super) fn events(&self) -> Vec<PortEvent> {
        self.events.lock().unwrap().iter().map(|(e, _)| e.clone()).collect()
    }

    pub(super) fn allows(&self) -> Vec<(PortEvent, bool)> {
        self.events.lock().unwrap().iter().filter(|(e, _)| matches!(e, PortEvent::Allow { .. })).cloned().collect()
    }
}

impl PortGrants for Recorder {
    fn apply(&self, event: PortEvent) -> Result<(), PortRefused> {
        let mut forwarded = false;
        let mut bus = self.bus.lock().unwrap();
        while let Ok(msg) = bus.try_recv() {
            forwarded |= msg.frame["kind"] == "browser";
        }
        self.events.lock().unwrap().push((event, forwarded));
        self.refuse.clone().map_or(Ok(()), Err)
    }
}

/// A REPL over the stub, owned by a private root, with its frame bus.
#[cfg(unix)]
struct Fixture {
    owned: OwnedRoot,
    repl: Repl,
    bus: broadcast::Receiver<ReplFrameMsg>,
}

#[cfg(unix)]
fn fixture(refuse: Option<PortRefused>) -> (Fixture, Arc<Recorder>) {
    let owned = OwnedRoot::private();
    let root = owned.path().to_path_buf();
    let stub = root.join("julia");
    sot_platform::test_exec::write_executable(&stub, BROWSER_STUB.to_owned());
    let resources = root.join("resources");
    std::fs::create_dir_all(resources.join("julia").join("repl")).expect("create resources");
    std::env::set_var("SOT_RESOURCE_ROOT", &resources);
    std::env::set_var("SOT_JULIA_BIN", &stub);
    let project = root.join("project");
    std::fs::create_dir(&project).expect("create project");
    let (tx, bus) = broadcast::channel(256);
    let recorder = Recorder::new(tx.subscribe(), refuse);
    let repl = Repl::new(tx, Some("ws-ports".into()), project, owned.sig()).with_ports(recorder.clone());
    (Fixture { owned, repl, bus }, recorder)
}

#[cfg(unix)]
/// The frames of eval 7 up to and including the first `browser` or `stderr` one, as kinds with their text.
async fn first_frame_of_the_page(fx: &mut Fixture) -> (Vec<String>, Value) {
    let mut kinds = Vec::new();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(30), fx.bus.recv()).await.expect("a frame in time").expect("bus");
        if msg.eval_id != 7 {
            continue;
        }
        let kind = msg.frame["kind"].as_str().unwrap_or("").to_string();
        kinds.push(kind.clone());
        if kind == "browser" || kind == "stderr" {
            return (kinds, msg.frame);
        }
    }
}

/// Port 45621 is granted, and the grant is applied and answered before its `browser` frame is on the bus.
#[cfg(unix)]
#[tokio::test]
async fn an_allow_is_applied_before_its_browser_frame_is_forwarded() {
    if !isolated("sidecars::repl::ports_tests::an_allow_is_applied_before_its_browser_frame_is_forwarded", BODY) {
        return;
    }
    let (mut fx, recorder) = fixture(None);
    fx.repl.submit("repl.eval", json!({ "code": "x", "eval_id": 7 }), &Default::default()).await.expect("submit");
    let (_, frame) = first_frame_of_the_page(&mut fx).await;
    assert_eq!(frame["kind"], "browser", "an accepted grant forwards the frame");
    let allows = recorder.allows();
    assert_eq!(allows.len(), 1, "one announcement, one Allow: {allows:?}");
    match &allows[0] {
        (PortEvent::Allow { owner, generation, port }, forwarded_first) => {
            assert_eq!((owner.as_str(), *port), ("ws-ports", 45621));
            assert!(*generation > 0);
            assert!(!forwarded_first, "the frame was on the bus before its grant was applied");
        }
        other => panic!("not an Allow: {other:?}"),
    }
    fx.owned.end().await;
}

/// A receiver that refuses the grant keeps the frame off the bus; a `stderr` frame names the reason in its place.
#[cfg(unix)]
#[tokio::test]
async fn a_refused_allow_withholds_the_frame_and_says_why() {
    if !isolated("sidecars::repl::ports_tests::a_refused_allow_withholds_the_frame_and_says_why", BODY) {
        return;
    }
    let (mut fx, _recorder) = fixture(Some(PortRefused::Failed("the grant table is gone".into())));
    fx.repl.submit("repl.eval", json!({ "code": "x", "eval_id": 7 }), &Default::default()).await.expect("submit");
    let (kinds, frame) = first_frame_of_the_page(&mut fx).await;
    assert_eq!(frame["kind"], "stderr", "frames until the page's: {kinds:?}");
    let text = frame["text"].as_str().unwrap_or("");
    assert!(text.contains("45621") && text.contains("the grant table is gone"), "{text}");
    assert!(!kinds.contains(&"browser".to_string()), "the browser frame must not be forwarded: {kinds:?}");
    fx.owned.end().await;
}

/// An `End` the receiver refuses is logged with its reason, and the sink goes on.
#[test]
fn an_end_a_receiver_refuses_is_logged_and_changes_nothing() {
    let (_tx, rx) = broadcast::channel(1);
    let stale = Recorder::new(rx, Some(PortRefused::Stale { current: 9 }));
    let sink = PortSink::new(Some(stale.clone()), &Some("ws".into())).at(3);
    let logged = crate::sidecars::logged_by(|| sink.end());
    assert!(logged.contains("port grants not ended") && logged.contains("generation 9"), "{logged}");
    assert_eq!(stale.events.lock().unwrap().len(), 1);
}

/// With no receiver installed a grant is refused with a reason, never granted by default.
#[test]
fn a_repl_with_no_receiver_refuses_every_allow() {
    let sink = PortSink::new(None, &None).at(1);
    assert!(matches!(sink.allow(45621), Err(PortRefused::Failed(why)) if why.contains("no receiver")));
}

/// A row's later REPL handle (a fresh cell) opens above every generation an earlier one used, so a receiver that keeps
/// the newest per owner never sees the new handle's first child as stale.
#[test]
fn a_new_repl_handle_starts_above_the_last_generation() {
    let (tx, _rx) = broadcast::channel(8);
    let sink = PortSink::new(None, &None);
    let cell = || -> lifecycle::SharedLifecycle {
        Arc::new(std::sync::Mutex::new(LifecycleCell {
            gen: 0,
            state: ReplLifecycle::NotStarted,
            notice: None,
            start: None,
        }))
    };
    let (old, new) = (cell(), cell());
    let mut last = 0;
    for _ in 0..3 {
        last = lifecycle::lifecycle_begin_starting(&old, &tx, &None, &sink);
    }
    let first_of_new = lifecycle::lifecycle_begin_starting(&new, &tx, &None, &sink);
    assert!(first_of_new > last, "{first_of_new} must exceed {last}");
}
