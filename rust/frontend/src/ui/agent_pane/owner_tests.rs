//! Characterizations through the production owner operations and a process-free client port.
use super::*;
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;
use sot_capsule::attach_client::client::InputOutcome;

type Pane = AgentPane<Scripted>;
#[derive(Default)]
struct Wire {
    inputs: Vec<Vec<u8>>, sizes: Vec<(u16, u16)>, viewed: Vec<bool>,
    scrolls: Vec<i32>, resets: usize, dead: bool, steps: VecDeque<Step>,
}
#[derive(Default)]
struct Step {
    status: Option<&'static str>, checkpoint: bool, dead: bool,
    outcome: Option<InputOutcome>, output: &'static [u8],
}
type Handle = Rc<RefCell<Wire>>;
struct Scripted {
    wire: Handle, parser: vt100::Parser, status: String,
    checkpoint: bool, dead: bool, outcome: Option<InputOutcome>,
}
impl Scripted {
    fn new(text: &[u8], ready: bool) -> (Self, Handle) { Self::at_size(text, ready, 80, 24) }
    fn at_size(text: &[u8], ready: bool, cols: u16, rows: u16) -> (Self, Handle) {
        let wire = Rc::new(RefCell::new(Wire::default()));
        let mut parser = vt100::Parser::new(rows, cols, 100);
        parser.process(text);
        (Self { wire: wire.clone(), parser, status: if ready { "attached" } else { "connecting…" }.into(),
            checkpoint: ready, dead: false, outcome: None }, wire)
    }
}
impl PaneClient for Scripted {
    fn screen(&self) -> &vt100::Screen { self.parser.screen() }
    fn is_checkpointed(&self) -> bool { self.checkpoint }
    fn is_dead(&mut self) -> bool { self.dead || self.wire.borrow().dead }
    fn status_line(&self) -> &str { &self.status }
    fn pump(&mut self) -> bool {
        let Some(s) = self.wire.borrow_mut().steps.pop_front() else { return false };
        if let Some(status) = s.status { self.status = status.into(); }
        self.checkpoint |= s.checkpoint;
        self.dead |= s.dead;
        if s.outcome.is_some() { self.outcome = s.outcome; }
        self.parser.process(s.output);
        true
    }
    fn last_input_outcome(&self) -> Option<InputOutcome> { self.outcome }
    fn inputs_discarded(&self) -> usize { 0 }
    fn send_input(&mut self, bytes: &[u8]) { self.wire.borrow_mut().inputs.push(bytes.to_vec()); }
    fn resize(&mut self, cols: u16, rows: u16) {
        self.wire.borrow_mut().sizes.push((cols, rows));
        self.parser.screen_mut().set_size(rows, cols);
    }
    fn reset_scroll(&mut self) { self.wire.borrow_mut().resets += 1; self.parser.screen_mut().set_scrollback(0); }
    fn scroll(&mut self, rows: i32) { self.wire.borrow_mut().scrolls.push(rows); scroll_ring(self.parser.screen_mut(), rows); }
    fn mouse_tracking_on(&self) -> bool { !matches!(self.screen().mouse_protocol_mode(), vt100::MouseProtocolMode::None) }
    fn set_viewed(&self, viewed: bool) { self.wire.borrow_mut().viewed.push(viewed); }
}
fn key(h: &str, n: &str) -> WsKey { (h.into(), n.into()) }
fn after(t: Instant, ms: u64) -> Instant { t + Duration::from_millis(ms) }
fn select(p: &mut Pane, h: &str, n: &str, t: Instant, bound: usize) -> Vec<Scripted> {
    let switch = p.begin_switch(key(h, n), Some(t), bound);
    if let Some(open) = switch.open { assert!(p.finish_open(open, true).is_some()); }
    switch.retired
}
fn prepare(p: &mut Pane, key: WsKey) -> (AttachPlan<Scripted>, Vec<Scripted>) {
    let DirectReply::Attach(target) = p.direct_reply(&key.0, &key.0, Some(key.1.clone())) else { panic!("admitted reply") };
    let (request, mut retired) = p.prepare_attach(target);
    let (plan, dead) = p.prepare_warm(request);
    retired.extend(dead);
    (plan, retired)
}
fn install(p: &mut Pane, h: &str, n: &str, c: Scripted, t: Instant) -> AttachEffect {
    let (plan, retired) = prepare(p, key(h, n));
    drop(retired);
    p.start_attach(plan, |_, _, _| Ok(c), || t)
}
fn ready(p: &mut Pane, h: &str, n: &str, text: &[u8], t: Instant) -> Handle {
    select(p, h, n, t, 16);
    let (c, wire) = Scripted::new(text, true);
    install(p, h, n, c, t);
    wire
}
fn step(p: &mut Pane, wire: &Handle, s: Step, at: Instant) -> PumpEffect {
    wire.borrow_mut().steps.push_back(s);
    p.pump(|| at)
}
fn candidate(p: &mut Pane, area: (u16, u16)) -> Option<PresentationCandidate> {
    let claim = p.view(None).2?;
    p.painted_candidate(claim, area)
}
fn pixels(p: &mut Pane) -> (PaneScreen, String) {
    let choice = p.view(None).0;
    (choice, p.painted_screen(choice).map(|s| s.contents()).unwrap_or_default())
}
fn failure(p: &mut Pane, h: &str, kind: AttachFailure, t: Instant) -> AttachEffect {
    let (plan, retired) = prepare(p, key(h, "row"));
    drop(retired);
    p.start_attach(plan, |_, _, _| Err(kind), || t)
}

#[test]
fn switches_and_replies_keep_the_host_and_attachment_pair() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    ready(&mut p, "a", "same", b"a", t);
    let switch = p.begin_switch(key("b", "same"), Some(t), 2);
    let open = switch.open.expect("another host is a switch");
    assert_eq!(open.host(), "b");
    p.finish_open(open, true);
    assert!(matches!(p.direct_reply(&"a".into(), &"b".into(), Some("same".into())), DirectReply::Ignore));
    assert!(matches!(p.direct_reply(&"b".into(), &"a".into(), Some("same".into())), DirectReply::Ignore));
    assert!(matches!(p.direct_reply(&"b".into(), &"b".into(), None), DirectReply::Targetless));
    for (h, n) in [("a", Some("same")), ("b", None), ("b", Some("other"))] {
        assert!(p.open_failed(&h.into(), &"b".into(), n, "late").is_none());
    }
    let (c, _) = Scripted::new(b"b", true);
    install(&mut p, "b", "same", c, t);
    assert!(matches!(p.direct_reply(&"b".into(), &"b".into(), Some("same".into())), DirectReply::Ignore));
    assert!(p.open_failed(&"b".into(), &"b".into(), Some("same"), "selected").is_some());
    assert!(p.view(None).1.iter().any(|s| s.contains("selected")));
    assert_eq!(p.snapshot_target().as_deref(), Some("same"));
    p.restore_snapshot_target("snapshot-host".into(), Some("stored".into()));
    assert_eq!(p.target(), Some(key("snapshot-host", "stored")));
    p.restore_snapshot_target("snapshot-host".into(), None);
    assert_eq!(p.target(), None);
}

#[test]
fn a_failed_open_send_does_not_commit_the_target() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let old = ready(&mut p, "a", "row", b"old", t);
    let switch = p.begin_switch(key("b", "row"), Some(after(t, 1)), 2);
    assert!(switch.retired.is_empty());
    assert!(p.finish_open(switch.open.unwrap(), false).is_none(), "failure has no persist/redraw success effect");
    assert_eq!(p.target(), Some(key("a", "row")));
    assert_eq!(pixels(&mut p), (PaneScreen::Hold, "old".into()));
    p.send_input(b"lost");
    assert!(old.borrow().inputs.is_empty());
    assert!(p.view(None).1.iter().any(|s| s.starts_with("1 keystroke")));
    let switch = p.begin_switch(key("b", "row"), Some(after(t, 2)), 2);
    assert!(p.finish_open(switch.open.unwrap(), true).is_some());
    assert_eq!(p.target(), Some(key("b", "row")));
}

#[test]
fn pane_input_reaches_only_the_selected_client_or_is_counted() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    p.send_input(b"many bytes");
    p.send_input(b"\x1b[A");
    assert_eq!(p.view(None).1, vec!["2 keystrokes discarded, not sent"]);
    let a = ready(&mut p, "a", "one", b"a", t);
    select(&mut p, "a", "two", t, 2);
    p.send_input(b"pending");
    let (c, b) = Scripted::new(b"", false);
    install(&mut p, "a", "two", c, t);
    p.send_input(b"before checkpoint");
    assert!(a.borrow().inputs.is_empty());
    assert_eq!(b.borrow().inputs, vec![b"before checkpoint".to_vec()], "no Pending input is replayed");
    assert!(p.view(None).1.iter().any(|s| s.starts_with("1 keystroke")));
    step(&mut p, &b, Step { status: Some("attached"), ..Default::default() }, t);
    assert!(!p.view(None).1.iter().any(|s| s.contains("discarded")));
}

#[test]
fn a_held_screen_is_never_an_input_destination() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let a = ready(&mut p, "h", "a", b"departed", t);
    select(&mut p, "h", "b", t, 2);
    assert_eq!(pixels(&mut p), (PaneScreen::Hold, "departed".into()));
    p.send_input(b"unresolved");
    let (c, b) = Scripted::new(b"new", false);
    install(&mut p, "h", "b", c, t);
    assert_eq!(pixels(&mut p).0, PaneScreen::Hold);
    p.send_input(b"selected");
    assert!(a.borrow().inputs.is_empty());
    assert_eq!(b.borrow().inputs, vec![b"selected".to_vec()]);
}

#[test]
fn unknown_delivery_is_reported_by_the_pump() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let wire = ready(&mut p, "h", "row", b"", t);
    for (outcome, words) in [(InputOutcome::DeliveryUnknown, "delivery unknown"), (InputOutcome::RunEnded, "run ended"),
                             (InputOutcome::Recorded, "attached"), (InputOutcome::RefusedStale, "attached")] {
        let effect = step(&mut p, &wire, Step { outcome: Some(outcome), ..Default::default() }, t);
        assert!(effect.status.unwrap().contains(words));
        assert!(p.pump(|| t).status.is_none());
    }
}

#[test]
fn holds_end_at_checkpoint_or_a_dead_end() {
    let t = Instant::now();
    for terminal in [false, true] {
        let mut p = Pane::new(None);
        ready(&mut p, "h", "a", b"old", t);
        select(&mut p, "h", "b", t, 2);
        let (c, wire) = Scripted::new(b"new", false);
        install(&mut p, "h", "b", c, t);
        assert_eq!(pixels(&mut p), (PaneScreen::Hold, "old".into()));
        assert!(candidate(&mut p, (80, 24)).is_none());
        step(&mut p, &wire, Step { checkpoint: !terminal, dead: terminal,
            status: Some(if terminal { "ended" } else { "attached" }), ..Default::default() }, t);
        if terminal {
            assert_eq!(pixels(&mut p), (PaneScreen::Empty, "".into()));
            assert!(candidate(&mut p, (80, 24)).is_none());
        } else {
            assert_eq!(pixels(&mut p), (PaneScreen::Client, "new".into()));
            assert!(candidate(&mut p, (80, 24)).is_some());
            step(&mut p, &wire, Step { dead: true, status: Some("ended"), ..Default::default() }, t);
            assert_eq!(pixels(&mut p), (PaneScreen::Client, "new".into()));
        }
    }
    for kind in [AttachFailure::MissingConfig, AttachFailure::Unresolved, AttachFailure::Unusable, AttachFailure::Startup("no worker".into())] {
        let mut p = Pane::new(None);
        ready(&mut p, "h", "a", b"old", t);
        select(&mut p, "h", "row", t, 2);
        failure(&mut p, "h", kind, t);
        assert_eq!(pixels(&mut p).0, PaneScreen::Empty);
    }
    let mut p = Pane::new(None);
    ready(&mut p, "h", "a", b"warm", t);
    ready(&mut p, "h", "b", b"other", t);
    select(&mut p, "h", "a", t, 2);
    let (plan, _) = prepare(&mut p, key("h", "a"));
    p.start_attach(plan, |_, _, _| panic!("warm reuse must precede the factory"), || t);
    assert_eq!(pixels(&mut p), (PaneScreen::Client, "warm".into()));
}

#[test]
fn attach_warnings_follow_edges_and_reset_per_installed_client() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let a = ready(&mut p, "h", "a", b"", t);
    let warning = |e: PumpEffect, n| assert!(matches!(e.outcomes.as_slice(), [AttachOutcome::Warning { episode, .. }] if *episode == n));
    warning(step(&mut p, &a, Step { status: Some("unreachable"), ..Default::default() }, t), 1);
    assert!(p.pump(|| t).outcomes.is_empty());
    assert!(step(&mut p, &a, Step { output: b"text", ..Default::default() }, t).outcomes.is_empty());
    assert_eq!(step(&mut p, &a, Step { status: Some("attached"), ..Default::default() }, t).outcomes, vec![AttachOutcome::Attached]);
    warning(step(&mut p, &a, Step { status: Some("refused"), ..Default::default() }, t), 2);
    select(&mut p, "h", "b", t, 2);
    let (c, b) = Scripted::new(b"", false);
    install(&mut p, "h", "b", c, t);
    warning(step(&mut p, &b, Step { status: Some("unreachable"), ..Default::default() }, t), 1);
    select(&mut p, "h", "a", t, 2);
    let (plan, _) = prepare(&mut p, key("h", "a"));
    p.start_attach(plan, |_, _, _| panic!("warm"), || t);
    warning(step(&mut p, &a, Step { status: Some("another failure"), ..Default::default() }, t), 1);
}

#[test]
fn request_and_client_clocks_keep_their_distinct_origins() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    select(&mut p, "h", "a", t, 2); // Earlier create origin.
    let (c, a) = Scripted::new(b"a", false);
    install(&mut p, "h", "a", c, after(t, 100));
    a.borrow_mut().steps.push_back(Step { status: Some("attached"), checkpoint: true, ..Default::default() });
    let mut ticks = [after(t, 150), after(t, 151)].into_iter();
    let e = p.pump(|| ticks.next().unwrap());
    assert_eq!((e.since_request_ms, e.since_client_ms), (150, 51));
    assert_eq!(p.client_open_elapsed(), Some(Duration::from_millis(100)));
    let noop = p.begin_switch(key("h", "a"), Some(after(t, 180)), 2);
    assert!(noop.open.is_none());
    let c = candidate(&mut p, (80, 24)).unwrap();
    assert_eq!(p.presented(c, after(t, 200)), Presentation::Receipt { elapsed: Duration::from_millis(200) });
    ready(&mut p, "h", "b", b"b", after(t, 220));
    select(&mut p, "h", "a", after(t, 300), 2);
    let (plan, _) = prepare(&mut p, key("h", "a"));
    let e = p.start_attach(plan, |_, _, _| panic!("warm"), || after(t, 350));
    assert!(matches!(e.trace, AttachTrace::Warm(50)));
    let e = p.pump(|| after(t, 400));
    assert_eq!((e.since_request_ms, e.since_client_ms), (100, 50));
}

#[test]
fn warm_reuse_is_bounded_and_uses_the_exact_host_target() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let a = ready(&mut p, "a", "same", b"a", t);
    let b = ready(&mut p, "b", "same", b"b", t);
    select(&mut p, "a", "same", t, 2);
    p.sync_size((90, 30));
    let (plan, _) = prepare(&mut p, key("a", "same"));
    p.start_attach(plan, |_, _, _| panic!("exact warm key"), || t);
    assert_eq!(a.borrow().viewed, vec![false, true]);
    assert_eq!(a.borrow().sizes, vec![(90, 30)]);
    p.send_input(b"a only");
    assert!(b.borrow().inputs.is_empty());
    // A worker can end while its checkpointed client is parked.
    ready(&mut p, "a", "other", b"", t);
    a.borrow_mut().dead = true;
    select(&mut p, "a", "same", t, 2);
    let (plan, retired) = prepare(&mut p, key("a", "same"));
    assert_eq!(retired.len(), 1);
    let calls = std::cell::Cell::new(0);
    p.start_attach(plan, |_, _, _| { calls.set(calls.get() + 1); Ok(Scripted::new(b"fresh", true).0) }, || t);
    assert_eq!(calls.get(), 1);
    // The cap evicts the oldest on this host, leaving the other host's exact key reusable.
    let mut capped = Pane::new(None);
    let other = ready(&mut capped, "b", "same", b"b", t);
    let mut retired = Vec::new();
    for i in 0..18 {
        retired.extend(select(&mut capped, "a", &format!("n{i}"), t, 100));
        install(&mut capped, "a", &format!("n{i}"), Scripted::new(b"", true).0, t);
    }
    retired.extend(select(&mut capped, "b", "same", t, 100));
    assert_eq!(retired.len(), 2, "eighteen departed clients leave sixteen warm clients");
    let (plan, _) = prepare(&mut capped, key("b", "same"));
    capped.start_attach(plan, |_, _, _| panic!("another host was evicted"), || t);
    assert_eq!(other.borrow().viewed.last(), Some(&true));
    // A snapshot can relabel the active slot with an already parked key.
    let mut p = Pane::new(None);
    ready(&mut p, "h", "a", b"first", t);
    ready(&mut p, "h", "b", b"second", t);
    p.restore_snapshot_target("h".into(), Some("a".into()));
    let duplicate = select(&mut p, "h", "c", t, 2);
    assert_eq!(duplicate.len(), 1);
    assert_eq!(duplicate[0].screen().contents(), "first");
    install(&mut p, "h", "c", Scripted::new(b"c", true).0, t);
    assert_eq!(select(&mut p, "h", "d", t, 0).len(), 1, "zero bound clamps to one");

}

#[test]
fn warm_pruning_uses_the_committed_host_session_names() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    ready(&mut p, "a", "stored", b"a", t);
    ready(&mut p, "b", "stored", b"b", t);
    select(&mut p, "a", "other", t, 2);
    let mut directory = RowDirectory::default();
    let mut row = ws_info("listed-slug", "stored");
    row.workspace_id = "old-id".into();
    let update = directory.prepare_replace("a".into(), vec![row.clone()]);
    directory.commit_replace(update);
    row.workspace_id = "replacement-id".into();
    let update = directory.prepare_replace("a".into(), vec![row]);
    assert_eq!(update.retired().len(), 1);
    directory.commit_replace(update);
    let live: HashSet<String> = directory.rows("a").unwrap().iter().map(|r| r.session_name.clone()).collect();
    assert!(p.prune_warm(&"a".into(), &live).is_empty(), "same stored name survives canonical replacement");
    let update = directory.prepare_replace("a".into(), vec![]);
    directory.commit_replace(update);
    let live = directory.rows("a").unwrap().iter().map(|r| r.session_name.clone()).collect();
    assert_eq!(p.prune_warm(&"a".into(), &live).len(), 1);
    select(&mut p, "a", "stored", t, 2);
    let (plan, _) = prepare(&mut p, key("a", "stored"));
    let calls = std::cell::Cell::new(0);
    p.start_attach(plan, |_, _, _| { calls.set(calls.get() + 1); Ok(Scripted::new(b"new", true).0) }, || t);
    assert_eq!(calls.get(), 1);
    select(&mut p, "b", "stored", t, 2);
    let (plan, _) = prepare(&mut p, key("b", "stored"));
    p.start_attach(plan, |_, _, _| panic!("other host survives pruning"), || t);
}

#[test]
fn dial_failures_preserve_retry_and_reason_policy() {
    let t = Instant::now();
    for (kind, persistent, words) in [
        (AttachFailure::MissingConfig, true, "no transport configured"),
        (AttachFailure::Unresolved, false, "not yet connected"),
        (AttachFailure::Unusable, true, "no usable transport"),
        (AttachFailure::Startup("no worker".into()), false, "session attach failed"),
    ] {
        let mut p = Pane::new(None);
        select(&mut p, "h", "row", t, 2);
        let effect = failure(&mut p, "h", kind, t);
        assert!(effect.status.contains(words));
        assert!(matches!(effect.trace, AttachTrace::Failed(_)));
        assert_eq!(p.reconnect_open(&"h".into()).is_none(), persistent);
        assert!(p.reconnect_open(&"other".into()).is_none());
        assert_eq!(p.view(None).1.iter().any(|s| s.contains(words)), persistent);
        assert_eq!(p.view(Some("protocol refused")).1[0], "protocol refused");
        if let Some(open) = p.reconnect_open(&"h".into()) {
            assert!(matches!(open.request(), OutgoingReq::PtyOpen { user_switch: false, cols: 80, rows: 24, .. }));
        }
        install(&mut p, "h", "row", Scripted::new(b"", true).0, t);
        assert!(p.view(None).1.is_empty());
        assert!(p.reconnect_open(&"h".into()).is_none());
    }
}

#[test]
fn resize_tracks_pending_geometry_without_sending() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    for size in [(0, 24), (80, 1)] { p.sync_size(size); }
    assert_eq!(p.size(), None);
    p.sync_size((90, 30));
    p.sync_size((90, 30));
    p.sync_size((100, 40));
    assert_eq!(p.size(), Some((100, 40)));
    select(&mut p, "h", "row", t, 2);
    let (plan, _) = prepare(&mut p, key("h", "row"));
    let (c, wire) = Scripted::new(b"", true);
    p.start_attach(plan, |_, _, size| { assert_eq!(size, (100, 40)); Ok(c) }, || t);
    p.sync_size((100, 40));
    assert!(wire.borrow().sizes.is_empty());
    assert_eq!(wire.borrow().resets, 0);
    p.sync_size((120, 50));
    assert_eq!(wire.borrow().sizes, vec![(120, 50)]);
    assert_eq!(wire.borrow().resets, 1);
    select(&mut p, "h", "next", t, 2);
    p.sync_size((130, 60));
    assert_eq!(wire.borrow().sizes, vec![(120, 50)], "Pending cannot resize a parked client");
    assert_eq!(p.size(), Some((130, 60)));
    let mut first = Pane::new(None);
    let w = ready(&mut first, "h", "row", b"", t);
    first.sync_size((90, 30));
    assert_eq!(w.borrow().sizes, vec![(90, 30)]);
    assert_eq!(w.borrow().resets, 0, "first geometry is not a reshape of an earlier size");
}

#[test]
fn selection_drag_preserves_press_extend_release_rules() {
    let mut p = Pane::new(None);
    assert!(!p.press(None));
    assert!(!p.extend_drag(Some((2, 3))));
    assert!(p.press(Some((1, 2))));
    assert!(p.extend_drag(Some((23, 79)))); // Adapter clamps an outside drag to this cell.
    assert_eq!(p.selection(), Some(((1, 2), (23, 79))));
    assert!(!p.extend_drag(None)); // Zero-area mapping cannot extend.
    p.release();
    assert!(!p.extend_drag(Some((0, 0))));
    assert_eq!(p.selection(), Some(((1, 2), (23, 79))));
    assert!(p.press(None));
    assert_eq!(p.selection(), None);
    assert!(p.press(Some((0, 0))));
    select(&mut p, "h", "row", Instant::now(), 2);
    p.sync_size((90, 30));
    assert_eq!(p.selection(), Some(((0, 0), (0, 0))), "switch and resize keep selection");
}

#[test]
fn copy_uses_client_or_blank_and_clears_only_after_success() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    ready(&mut p, "h", "a", b"abc  \r\ndefgh", t);
    p.press(Some((1, 2)));
    p.extend_drag(Some((0, 1)));
    p.release();
    assert_eq!(p.copy_text(5).as_deref(), Some("bc\ndef"));
    assert!(p.copy_text(0).is_none());
    p.copy_finished(false);
    assert!(p.selection().is_some());
    p.copy_finished(true);
    assert!(p.selection().is_none());
    select(&mut p, "h", "b", t, 2);
    p.press(Some((0, 0)));
    p.extend_drag(Some((0, 2)));
    assert_eq!(pixels(&mut p).0, PaneScreen::Hold);
    assert!(p.copy_text(80).is_none(), "blank copy does not read the hold");
    install(&mut p, "h", "b", Scripted::new(b"new", false).0, t);
    // Painting keeps the held screen's spaces; only clipboard text is trimmed.
    assert_eq!(pixels(&mut p).1, "abc  \ndefgh");
    assert_eq!(p.copy_text(80).as_deref(), Some("new"));
    let mut wrapped = Pane::new(None);
    select(&mut wrapped, "h", "wrapped", t, 2);
    wrapped.sync_size((5, 3));
    install(&mut wrapped, "h", "wrapped", Scripted::at_size(b"abcdefgh", true, 5, 3).0, t);
    wrapped.press(Some((0, 3)));
    wrapped.extend_drag(Some((1, 1)));
    assert_eq!(wrapped.copy_text(5).as_deref(), Some("de\nfg"));
}

#[test]
fn paging_and_wheel_preserve_terminal_modes() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    assert!(p.page(true, 24));
    assert!(matches!(p.wheel(3), WheelEffect::Pending));
    let a = ready(&mut p, "h", "a", b"1\r\n2\r\n3", t);
    assert!(p.page(true, 24));
    assert!(p.page(false, 0));
    assert!(matches!(p.wheel(-2), WheelEffect::Redraw));
    assert_eq!(a.borrow().scrolls, vec![8, -1, -2]);
    step(&mut p, &a, Step { output: b"\x1b[?1049h", ..Default::default() }, t);
    assert!(!p.page(true, 24), "alternate-screen page falls through to raw key input");
    p.send_input(b"\x1b[5~");
    step(&mut p, &a, Step { output: b"\x1b[?1000h", ..Default::default() }, t);
    p.wheel(20);
    p.wheel(-1);
    assert_eq!(a.borrow().inputs[0], b"\x1b[5~");
    assert_eq!(a.borrow().inputs[1..9], vec![b"\x1b[<64;1;1M".to_vec(); 8]);
    assert_eq!(a.borrow().inputs[9], b"\x1b[<65;1;1M");
    select(&mut p, "h", "b", t, 2);
    p.wheel(5);
    assert_eq!(a.borrow().inputs.len(), 10, "Pending wheel never reaches a parked mouse client");
}

#[test]
fn pane_receipts_require_the_painted_live_checkpoint_and_present() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    assert!(candidate(&mut p, (80, 24)).is_none());
    ready(&mut p, "h", "a", b"a", t);
    let dropped = candidate(&mut p, (80, 24)).unwrap();
    drop(dropped);
    for area in [(0, 24), (80, 0)] { assert!(candidate(&mut p, area).is_none()); }
    let old = candidate(&mut p, (80, 24)).unwrap();
    select(&mut p, "h", "b", t, 2);
    assert_eq!(p.presented(old, t), Presentation::Stale);
    assert!(candidate(&mut p, (80, 24)).is_none(), "held content cannot certify the new client");
    let (c, b) = Scripted::new(b"b", false);
    install(&mut p, "h", "b", c, t);
    assert!(candidate(&mut p, (80, 24)).is_none());
    step(&mut p, &b, Step { checkpoint: true, status: Some("unreachable"), ..Default::default() }, t);
    assert!(candidate(&mut p, (80, 24)).is_none(), "checkpoint alone is insufficient");
    step(&mut p, &b, Step { status: Some("attached"), ..Default::default() }, t);
    let c = candidate(&mut p, (80, 24)).unwrap();
    assert!(candidate(&mut p, (80, 24)).is_some(), "candidate creation does not consume the receipt");
    assert!(matches!(p.presented(c, after(t, 5)), Presentation::Receipt { .. }));
    assert!(candidate(&mut p, (80, 24)).is_none());
    let mut p = Pane::new(None);
    let s = p.begin_switch(key("h", "row"), None, 2);
    p.finish_open(s.open.unwrap(), true);
    let (c, wire) = Scripted::new(b"", true);
    install(&mut p, "h", "row", c, t);
    let c = candidate(&mut p, (80, 24)).unwrap();
    assert_eq!(p.presented(c, t), Presentation::NoOrigin);
    select(&mut p, "h", "dead", t, 2);
    let (c, dead) = Scripted::new(b"dead", true);
    install(&mut p, "h", "dead", c, t);
    step(&mut p, &dead, Step { dead: true, ..Default::default() }, t);
    assert!(candidate(&mut p, (80, 24)).is_none());
    assert!(wire.borrow().inputs.is_empty());
}

#[test]
fn kept_row_invalidation_preserves_the_existing_scope() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let a = ready(&mut p, "h", "a", b"hold", t);
    ready(&mut p, "h", "b", b"selected", t);
    select(&mut p, "h", "c", t, 2);
    p.send_input(b"discard");
    let (c, current) = Scripted::new(b"not yet painted", false);
    install(&mut p, "h", "c", c, t);
    p.press(Some((0, 0)));
    p.sync_size((90, 30));
    p.invalidate_kept_attachment();
    assert_eq!(p.target(), None);
    assert_eq!(p.snapshot_target(), None);
    assert_eq!(pixels(&mut p), (PaneScreen::Hold, "selected".into()));
    assert!(p.view(None).1.is_empty(), "discard count is cleared");
    assert_eq!(p.selection(), Some(((0, 0), (0, 0))));
    assert_eq!(p.size(), Some((90, 30)));
    p.send_input(b"discard again");
    assert!(p.view(None).1.iter().any(|s| s.starts_with("1 keystroke")));
    assert!(current.borrow().inputs.is_empty(), "kept attachment cannot remain an input destination");
    select(&mut p, "h", "a", t, 2);
    let (plan, _) = prepare(&mut p, key("h", "a"));
    p.start_attach(plan, |_, _, _| panic!("kept outcome preserves warm storage"), || t);
    assert_eq!(a.borrow().viewed.last(), Some(&true));
    // Native kept-default controls separately prove State retains its ledger, view and snapshots.
}
