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
    input_runs: Vec<String>, run: String,
}
#[derive(Default)]
struct Step {
    status: Option<&'static str>, checkpoint: bool, dead: bool,
    outcome: Option<InputOutcome>, output: &'static [u8],
    copy: Option<&'static [u8]>, backlog: bool, mid_sequence: bool,
    screen_run: Option<&'static str>,
}
type Handle = Rc<RefCell<Wire>>;
struct Scripted {
    wire: Handle, parser: vt100::Parser, status: String,
    checkpoint: bool, dead: bool, outcome: Option<InputOutcome>,
    copy: Option<Vec<u8>>, drained: bool, ground: bool,
    screen_run: String,
}
impl Scripted {
    fn new(text: &[u8], ready: bool) -> (Self, Handle) { Self::at_size(text, ready, 80, 24) }
    fn at_size(text: &[u8], ready: bool, cols: u16, rows: u16) -> (Self, Handle) {
        let wire = Rc::new(RefCell::new(Wire { run: "first".into(), ..Default::default() }));
        let mut parser = vt100::Parser::new(rows, cols, 100);
        parser.process(text);
        (Self { wire: wire.clone(), parser, status: if ready { "attached" } else { "connecting…" }.into(),
            checkpoint: ready, dead: false, outcome: None, copy: None, drained: true, ground: true,
            screen_run: if ready { "first" } else { "" }.into() }, wire)
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
        if s.checkpoint { self.screen_run = s.screen_run.unwrap_or("first").into(); }
        self.dead |= s.dead;
        if s.outcome.is_some() { self.outcome = s.outcome; }
        self.parser.process(s.output);
        if let Some(copy) = s.copy { self.copy = Some(copy.to_vec()); }
        self.drained = !s.backlog;
        self.ground = !s.mid_sequence;
        true
    }
    fn last_input_outcome(&self) -> Option<InputOutcome> { self.outcome }
    fn inputs_discarded(&self) -> usize { 0 }
    fn screen_run(&self) -> &str { &self.screen_run }
    fn run(&self) -> String { self.wire.borrow().run.clone() }
    fn send_input_in_run(&mut self, bytes: &[u8], run: &str) {
        let mut wire = self.wire.borrow_mut();
        wire.inputs.push(bytes.to_vec());
        wire.input_runs.push(run.into());
    }
    fn resize(&mut self, cols: u16, rows: u16) {
        self.wire.borrow_mut().sizes.push((cols, rows));
        self.parser.screen_mut().set_size(rows, cols);
    }
    fn reset_scroll(&mut self) { self.wire.borrow_mut().resets += 1; self.parser.screen_mut().set_scrollback(0); }
    fn scroll(&mut self, rows: i32) { self.wire.borrow_mut().scrolls.push(rows); scroll_ring(self.parser.screen_mut(), rows); }
    fn mouse_tracking_on(&self) -> bool { !matches!(self.screen().mouse_protocol_mode(), vt100::MouseProtocolMode::None) }
    fn set_viewed(&self, viewed: bool) { self.wire.borrow_mut().viewed.push(viewed); }
    fn take_copy(&mut self) -> Option<Vec<u8>> { self.copy.take() }
    fn backlog_drained(&self) -> bool { self.drained }
    fn at_ground(&self) -> bool { self.ground }
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
    p.prepare_attach(target)
}
fn start(
    p: &mut Pane, plan: AttachPlan<Scripted>,
    factory: impl FnOnce(&HostKey, &str, (u16, u16)) -> std::result::Result<Scripted, AttachFailure>,
    now: impl FnMut() -> Instant,
) -> AttachEffect {
    install_pane_attach(p, plan, factory, now, |_| {})
}
fn install(p: &mut Pane, h: &str, n: &str, c: Scripted, t: Instant) -> AttachEffect {
    let (plan, retired) = prepare(p, key(h, n));
    drop(retired);
    start(p, plan, |_, _, _| Ok(c), || t)
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
fn present(p: &mut Pane, at: Instant) {
    let receipt = candidate(p, (80, 24));
    p.live_frame_presented(at);
    if let Some(receipt) = receipt { p.presented(receipt, at); }
}
fn held_key(p: &mut Pane, key: NamedKey, repeat: bool, at: Instant) -> String {
    p.held_key(&Key::Named(key), repeat, true, at).expect("held key consumed")
}
fn pixels(p: &mut Pane) -> (PaneScreen, String) {
    let choice = p.view(None).0;
    (choice, p.painted_screen(choice).map(|s| s.contents()).unwrap_or_default())
}
fn failure(p: &mut Pane, h: &str, kind: AttachFailure, t: Instant) -> AttachEffect {
    let (plan, retired) = prepare(p, key(h, "row"));
    drop(retired);
    start(p, plan, |_, _, _| Err(kind), || t)
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
    p.send_input(b"held");
    assert!(old.borrow().inputs.is_empty());
    assert!(p.view(None).1.iter().any(|s| s.contains("held, not sent (1 keys): held")));
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
    assert!(b.borrow().inputs.is_empty(), "input before a checkpoint stays local");
    assert!(p.view(None).1.iter().any(|s| s.contains("pendingbefore checkpoint")));
    step(&mut p, &b, Step { status: Some("attached"), ..Default::default() }, t);
    assert!(!p.view(None).1.iter().any(|s| s.contains("discarded")));
    assert!(b.borrow().inputs.is_empty());
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
    assert!(b.borrow().inputs.is_empty());
    assert!(p.view(None).1.iter().any(|s| s.contains("unresolvedselected")));
}

#[test]
fn keys_typed_while_a_row_connects_are_held_and_sent_only_on_an_armed_enter() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    select(&mut p, "h", "row", t, 2);
    p.send_input(b"pending");
    held_key(&mut p, NamedKey::Enter, false, t);
    let (c, wire) = Scripted::new(b"", false);
    install(&mut p, "h", "row", c, t);
    p.send_input(b"installed");
    held_key(&mut p, NamedKey::Enter, false, t);
    assert!(wire.borrow().inputs.is_empty());
    assert!(p.view(None).1.iter().any(|s| s == "held, not sent (4 keys): pending⏎installed⏎"));
    step(&mut p, &wire, Step { checkpoint: true, status: Some("attached"), ..Default::default() }, t);
    // A draw that never reaches present cannot make Enter send, even after the dwell.
    drop(candidate(&mut p, (80, 24)));
    held_key(&mut p, NamedKey::Enter, false, after(t, 2000));
    assert!(wire.borrow().inputs.is_empty());
    present(&mut p, after(t, 2000));
    assert!(wire.borrow().inputs.is_empty(), "presentation never flushes held input");
    p.send_input(b"later");
    p.send_input(b"\x1b[200~paste\x1b[201~");
    assert!(held_key(&mut p, NamedKey::Enter, false, after(t, 2999)).contains("wait"));
    assert!(wire.borrow().inputs.is_empty());
    held_key(&mut p, NamedKey::Enter, false, after(t, 3000));
    assert_eq!(wire.borrow().inputs, vec![b"pending\rinstalled\r\rlater\x1b[200~paste\x1b[201~".to_vec()]);
    assert_eq!(wire.borrow().input_runs, vec!["first"]);
    assert!(!p.view(None).1.iter().any(|s| s.starts_with("held, not sent")));
    p.send_input(b"plain");
    assert_eq!(wire.borrow().inputs, vec![b"pending\rinstalled\r\rlater\x1b[200~paste\x1b[201~".to_vec(), b"plain".to_vec()]);
    assert_eq!(wire.borrow().input_runs, vec!["first", "first"]);
    assert!(p.held_key(&Key::Named(NamedKey::Enter), false, true, after(t, 3001)).is_none());
}

#[test]
fn held_keys_stay_with_their_row_across_a_switch() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let a = ready(&mut p, "h", "a", b"a", t);
    p.send_input(b"for a");
    let b = ready(&mut p, "h", "b", b"b", t);
    p.send_input(b"for b");
    assert!(p.view(None).1.iter().any(|s| s.ends_with("for b")));
    select(&mut p, "h", "a", t, 2);
    let (plan, _) = prepare(&mut p, key("h", "a"));
    start(&mut p, plan, |_, _, _| panic!("warm a"), || t);
    assert!(p.view(None).1.iter().any(|s| s.ends_with("for a")));
    assert!(a.borrow().inputs.is_empty() && b.borrow().inputs.is_empty());
    present(&mut p, t);
    held_key(&mut p, NamedKey::Enter, false, after(t, 1000));
    assert_eq!(a.borrow().inputs, vec![b"for a".to_vec()]);
    assert!(b.borrow().inputs.is_empty());
    select(&mut p, "h", "b", t, 2);
    let (plan, _) = prepare(&mut p, key("h", "b"));
    start(&mut p, plan, |_, _, _| panic!("warm b"), || t);
    assert!(p.view(None).1.iter().any(|s| s.ends_with("for b")));
    held_key(&mut p, NamedKey::Enter, false, after(t, 2000));
    assert!(b.borrow().inputs.is_empty(), "a warm screen must be presented again before sending");
}

#[test]
fn held_keys_are_not_sent_into_a_run_the_screen_did_not_show() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let wire = ready(&mut p, "h", "row", b"first", t);
    p.send_input(b"held");
    present(&mut p, t);
    wire.borrow_mut().run = "replacement".into();
    assert!(held_key(&mut p, NamedKey::Enter, false, after(t, 1000)).contains("restarted"));
    assert!(wire.borrow().inputs.is_empty());
    assert!(p.view(None).1.iter().any(|s| s.ends_with("held")));
    // Applying the replacement checkpoint is insufficient without presentation and a new dwell.
    step(&mut p, &wire, Step { checkpoint: true, screen_run: Some("replacement"), ..Default::default() }, after(t, 1100));
    held_key(&mut p, NamedKey::Enter, false, after(t, 2000));
    assert!(wire.borrow().inputs.is_empty());
    present(&mut p, after(t, 2000));
    held_key(&mut p, NamedKey::Enter, false, after(t, 2999));
    assert!(wire.borrow().inputs.is_empty());
    held_key(&mut p, NamedKey::Enter, false, after(t, 3000));
    assert_eq!(wire.borrow().inputs, vec![b"held\r".to_vec()]);
    assert_eq!(wire.borrow().input_runs, vec!["replacement"]);
}

#[test]
fn held_keys_past_the_bound_are_refused_and_counted() {
    use sot_capsule::attach_client::attach::HELD_INPUT_CAP;
    let t = Instant::now();
    let mut p = Pane::new(None);
    select(&mut p, "h", "row", t, 2);
    p.send_input(&vec![b'x'; HELD_INPUT_CAP + 1]);
    assert!(p.view(None).1.iter().any(|s| s.starts_with("1 keystroke")));
    assert!(!p.view(None).1.iter().any(|s| s.starts_with("held, not sent")));
    p.send_input(&vec![b'x'; HELD_INPUT_CAP - 1]);
    p.send_input(b"\x1b[200~paste\x1b[201~");
    p.send_input(b"\r"); // Exactly the cap is kept; the paste was refused whole.
    p.send_input(b"y");
    assert!(p.view(None).1.iter().any(|s| s == "3 keystrokes discarded, not sent"));
    let wire = Scripted::new(b"", true);
    let handle = wire.1;
    install(&mut p, "h", "row", wire.0, t);
    present(&mut p, t);
    held_key(&mut p, NamedKey::Enter, false, after(t, 1000));
    let mut expected = vec![b'x'; HELD_INPUT_CAP - 1];
    expected.push(b'\r');
    assert_eq!(handle.borrow().inputs, vec![expected]);
}

#[test]
fn esc_drops_and_backspace_removes_the_last_held_key() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let wire = ready(&mut p, "h", "row", b"", t);
    p.send_input(b"first");
    p.send_input(b"\x1b[200~last paste\x1b[201~");
    assert!(p.held_key(&Key::Named(NamedKey::Escape), false, false, t).is_none(), "a chord follows the ordinary path");
    held_key(&mut p, NamedKey::Backspace, false, t);
    assert!(p.view(None).1.iter().any(|s| s == "held, not sent (1 keys): first"));
    present(&mut p, t);
    assert!(held_key(&mut p, NamedKey::Escape, false, t).contains("1 held"));
    assert!(wire.borrow().inputs.is_empty());
    p.send_input(b"ordinary");
    assert_eq!(wire.borrow().inputs, vec![b"ordinary".to_vec()]);
    for key in [NamedKey::Enter, NamedKey::Escape, NamedKey::Backspace] {
        assert!(p.held_key(&Key::Named(key), false, true, t).is_none());
    }
    step(&mut p, &wire, Step { status: Some("redialing…"), ..Default::default() }, t);
    p.send_input(b"last");
    held_key(&mut p, NamedKey::Backspace, false, t);
    assert!(!p.view(None).1.iter().any(|s| s.starts_with("held, not sent")));
}

#[test]
fn a_repeated_enter_never_sends_held_keys() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let wire = ready(&mut p, "h", "row", b"", t);
    p.send_input(b"held");
    present(&mut p, t);
    assert!(held_key(&mut p, NamedKey::Enter, true, after(t, 1000)).contains("release"));
    assert!(wire.borrow().inputs.is_empty());
    held_key(&mut p, NamedKey::Enter, false, after(t, 1001));
    assert_eq!(wire.borrow().inputs, vec![b"held".to_vec()]);
}

#[test]
fn a_wheel_while_connecting_holds_nothing() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    select(&mut p, "h", "row", t, 2);
    assert!(matches!(p.wheel(3), WheelEffect::Pending));
    let (c, wire) = Scripted::new(b"\x1b[?1000h", false);
    install(&mut p, "h", "row", c, t);
    assert!(matches!(p.wheel(3), WheelEffect::Redraw));
    assert_eq!(wire.borrow().scrolls, vec![3]);
    assert!(wire.borrow().inputs.is_empty());
    assert!(!p.view(None).1.iter().any(|s| s.starts_with("held, not sent")));
    step(&mut p, &wire, Step { checkpoint: true, status: Some("attached"), ..Default::default() }, t);
    p.wheel(2);
    assert!(wire.borrow().inputs.is_empty(), "mouse tracking waits for a presented screen too");
    present(&mut p, t);
    p.wheel(-1);
    assert_eq!(wire.borrow().inputs, vec![b"\x1b[<65;1;1M".to_vec()]);
    assert_eq!(wire.borrow().input_runs, vec!["first"]);
}

#[test]
fn held_keys_show_safe_text_and_the_tail_that_fits() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    select(&mut p, "h", "row", t, 2);
    p.send_input(b"\x03");
    p.send_input(b"\x1b");
    p.send_input(b"\r");
    p.send_input("\x1b[200~paste\n\t\u{85}\u{2028}\u{2029}\u{202e}\u{2066}尾\x1b[201~".as_bytes());
    let overlay = p.view(None).1;
    let held = overlay.iter().find(|s| s.starts_with("held, not sent")).unwrap();
    assert!(held.contains("^C^[⏎paste^J^I"));
    assert!(!held.contains("[200~") && !held.contains("[201~"));
    assert!(!held.contains(|c: char| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202e}' | '\u{2066}')));
    let prefix = "held, not sent (4 keys): ";
    assert_eq!(pane_overlay_row(held, prefix.len() + 2), format!("{prefix}尾"));
    assert_eq!(pane_overlay_row(held, prefix.len() + 1), prefix);
}

#[test]
fn a_redial_or_new_screen_run_requires_another_presented_frame() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let wire = ready(&mut p, "h", "row", b"", t);
    present(&mut p, t); // The receipt is consumed, but later frames still certify liveness.
    step(&mut p, &wire, Step { status: Some("redialing…"), ..Default::default() }, t);
    p.send_input(b"during redial");
    step(&mut p, &wire, Step { status: Some("attached"), ..Default::default() }, t);
    held_key(&mut p, NamedKey::Enter, false, after(t, 1000));
    assert!(wire.borrow().inputs.is_empty());
    present(&mut p, after(t, 1000));
    // A different checkpoint run resets the dwell even without a status edge.
    wire.borrow_mut().run = "replacement".into();
    step(&mut p, &wire, Step { checkpoint: true, screen_run: Some("replacement"), ..Default::default() }, after(t, 1100));
    p.view(None);
    held_key(&mut p, NamedKey::Enter, false, after(t, 2100));
    assert!(wire.borrow().inputs.is_empty());
    present(&mut p, after(t, 2100));
    held_key(&mut p, NamedKey::Enter, false, after(t, 3100));
    assert_eq!(wire.borrow().inputs, vec![b"during redial\r\r".to_vec()]);
    assert_eq!(wire.borrow().input_runs, vec!["replacement"]);
}

#[test]
fn ordinary_input_and_wheel_name_the_shown_run() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let wire = ready(&mut p, "h", "row", b"\x1b[?1000h", t);
    present(&mut p, t);
    wire.borrow_mut().run = "unshown".into();
    p.send_input(b"key");
    p.send_input(b"\x1b[200~paste\x1b[201~");
    p.wheel(1);
    assert_eq!(wire.borrow().inputs, vec![b"key".to_vec(), b"\x1b[200~paste\x1b[201~".to_vec(), b"\x1b[<64;1;1M".to_vec()]);
    assert_eq!(wire.borrow().input_runs, vec!["first"; 3]);
}

#[test]
fn pruning_drops_only_the_removed_hosts_held_rows() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    select(&mut p, "a", "same", t, 2);
    p.send_input(b"removed");
    select(&mut p, "b", "same", t, 2);
    p.send_input(b"kept");
    p.prune_warm(&"a".into(), &HashSet::new());
    p.prune_warm(&"b".into(), &HashSet::from(["same".into()]));
    assert!(p.view(None).1.iter().any(|s| s.ends_with("kept")));
    select(&mut p, "a", "same", t, 2);
    assert!(!p.view(None).1.iter().any(|s| s.starts_with("held, not sent")));
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
    start(&mut p, plan, |_, _, _| panic!("warm reuse must precede the factory"), || t);
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
    start(&mut p, plan, |_, _, _| panic!("warm"), || t);
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
    let e = start(&mut p, plan, |_, _, _| panic!("warm"), || after(t, 350));
    assert!(matches!(e.trace, AttachTrace::Warm(50)));
    let e = p.pump(|| after(t, 400));
    assert_eq!((e.since_request_ms, e.since_client_ms), (100, 50));
}

#[test]
fn attach_logging_precedes_client_start_and_warm_resume() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    select(&mut p, "h", "a", t, 2);
    let (c, a) = Scripted::new(b"a", true);
    let (plan, _) = prepare(&mut p, key("h", "a"));
    let clock = std::cell::Cell::new(after(t, 100));
    let effect = install_pane_attach(&mut p, plan, |_, _, _| Ok(c), || clock.get(), |trace| {
        assert!(matches!(trace, AttachTrace::Cold));
        clock.set(after(t, 150)); // Time spent in the coordinator's cold-attach log.
    });
    assert!(matches!(effect.trace, AttachTrace::Cold));
    assert_eq!(p.client_open_elapsed(), Some(Duration::from_millis(150)));
    let e = p.pump(|| after(t, 200));
    assert_eq!((e.since_request_ms, e.since_client_ms), (200, 50));

    ready(&mut p, "h", "b", b"b", after(t, 220));
    select(&mut p, "h", "a", after(t, 300), 2);
    let (plan, _) = prepare(&mut p, key("h", "a"));
    clock.set(after(t, 350));
    let effect = install_pane_attach(&mut p, plan, |_, _, _| panic!("warm reuse"), || clock.get(), |trace| {
        assert!(matches!(trace, AttachTrace::Warm(50)));
        assert_eq!(a.borrow().viewed, vec![false], "the warm client resumed before its attach log");
        clock.set(after(t, 370)); // The request sample precedes logging; the client sample follows it.
    });
    assert!(matches!(effect.trace, AttachTrace::Warm(50)));
    assert_eq!(a.borrow().viewed, vec![false, true]);
    assert_eq!(p.client_open_elapsed(), Some(Duration::from_millis(70)));
    let e = p.pump(|| after(t, 400));
    assert_eq!((e.since_request_ms, e.since_client_ms), (100, 30));
}

#[test]
fn warm_overflow_retires_the_oldest_host_local_client_and_reuses_the_survivor() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let other = ready(&mut p, "other", "same", b"other host", t);
    let oldest = ready(&mut p, "h", "a", b"oldest", t);
    assert!(select(&mut p, "h", "b", t, 2).is_empty());
    let (c, survivor) = Scripted::new(b"survivor", true);
    install(&mut p, "h", "b", c, t);
    assert!(select(&mut p, "h", "c", t, 2).is_empty());
    let (c, newest) = Scripted::new(b"newest", true);
    install(&mut p, "h", "c", c, t);

    // Three parked clients overflow the intermediate bound of two, well below the cap of sixteen.
    let retired = select(&mut p, "h", "b", t, 2);
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].screen().contents(), "oldest", "eviction chose the wrong host-local client");
    let (plan, rejected) = prepare(&mut p, key("h", "b"));
    assert!(rejected.is_empty());
    start(&mut p, plan, |_, _, _| panic!("the surviving client must be reused"), || t);
    assert_eq!(pixels(&mut p), (PaneScreen::Client, "survivor".into()));
    present(&mut p, t);
    p.send_input(b"selected survivor");
    assert_eq!(survivor.borrow().viewed, vec![false, true]);
    assert_eq!(survivor.borrow().inputs, vec![b"selected survivor".to_vec()]);
    assert!(oldest.borrow().inputs.is_empty() && newest.borrow().inputs.is_empty());

    assert!(select(&mut p, "other", "same", t, 2).is_empty());
    let (plan, _) = prepare(&mut p, key("other", "same"));
    start(&mut p, plan, |_, _, _| panic!("another host's client must survive"), || t);
    assert_eq!(other.borrow().viewed, vec![false, true]);
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
    start(&mut p, plan, |_, _, _| panic!("exact warm key"), || t);
    assert_eq!(a.borrow().viewed, vec![false, true]);
    assert_eq!(a.borrow().sizes, vec![(90, 30)]);
    present(&mut p, t);
    p.send_input(b"a only");
    assert_eq!(a.borrow().inputs, vec![b"a only".to_vec()]);
    assert!(b.borrow().inputs.is_empty());
    // A worker can end while its checkpointed client is parked.
    ready(&mut p, "a", "other", b"", t);
    a.borrow_mut().dead = true;
    select(&mut p, "a", "same", t, 2);
    let (plan, retired) = prepare(&mut p, key("a", "same"));
    assert_eq!(retired.len(), 1);
    let calls = std::cell::Cell::new(0);
    start(&mut p, plan, |_, _, _| { calls.set(calls.get() + 1); Ok(Scripted::new(b"fresh", true).0) }, || t);
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
    start(&mut capped, plan, |_, _, _| panic!("another host was evicted"), || t);
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
    start(&mut p, plan, |_, _, _| { calls.set(calls.get() + 1); Ok(Scripted::new(b"new", true).0) }, || t);
    assert_eq!(calls.get(), 1);
    select(&mut p, "b", "stored", t, 2);
    let (plan, _) = prepare(&mut p, key("b", "stored"));
    start(&mut p, plan, |_, _, _| panic!("other host survives pruning"), || t);
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
    start(&mut p, plan, |_, _, size| { assert_eq!(size, (100, 40)); Ok(c) }, || t);
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
    assert_eq!(p.copy_text(5, false).as_deref(), Some("bc\ndef"));
    assert!(p.copy_text(0, false).is_none());
    p.copy_finished(false);
    assert!(p.selection().is_some());
    p.copy_finished(true);
    assert!(p.selection().is_none());
    select(&mut p, "h", "b", t, 2);
    p.press(Some((0, 0)));
    p.extend_drag(Some((0, 2)));
    assert_eq!(pixels(&mut p).0, PaneScreen::Hold);
    assert!(p.copy_text(80, false).is_none(), "blank copy does not read the hold");
    install(&mut p, "h", "b", Scripted::new(b"new", false).0, t);
    // Painting keeps the held screen's spaces; only clipboard text is trimmed.
    assert_eq!(pixels(&mut p).1, "abc  \ndefgh");
    assert_eq!(p.copy_text(80, false).as_deref(), Some("new"));
    let mut wrapped = Pane::new(None);
    select(&mut wrapped, "h", "wrapped", t, 2);
    wrapped.sync_size((5, 3));
    install(&mut wrapped, "h", "wrapped", Scripted::at_size(b"abcdefgh", true, 5, 3).0, t);
    wrapped.press(Some((0, 3)));
    wrapped.extend_drag(Some((1, 1)));
    assert_eq!(wrapped.copy_text(5, false).as_deref(), Some("defg"), "a row the terminal wrapped joins the next");
}

fn copy_of(feed: &[u8], from: (u16, u16), to: (u16, u16)) -> Option<String> {
    copy_at(5, feed, from, to)
}

fn copy_at(cols: u16, feed: &[u8], from: (u16, u16), to: (u16, u16)) -> Option<String> {
    copy_with(cols, feed, from, to, false)
}

fn copy_line_at(cols: u16, feed: &[u8], from: (u16, u16), to: (u16, u16)) -> Option<String> {
    copy_with(cols, feed, from, to, true)
}

fn copy_with(cols: u16, feed: &[u8], from: (u16, u16), to: (u16, u16), one_line: bool) -> Option<String> {
    let t = Instant::now();
    let mut p = Pane::new(None);
    select(&mut p, "h", "w", t, 2);
    p.sync_size((cols, 12));
    install(&mut p, "h", "w", Scripted::at_size(feed, true, cols, 12).0, t);
    p.press(Some(from));
    p.extend_drag(Some(to));
    p.copy_text(cols, one_line)
}

#[test]
fn copy_joins_a_wrapped_row_and_breaks_where_the_program_did() {
    assert_eq!(copy_of(b"abcdefgh\r\nxy", (0, 0), (2, 1)).as_deref(), Some("abcdefgh\nxy"));
}

#[test]
fn a_row_that_exactly_fills_the_width_and_ends_in_a_newline_is_not_joined() {
    assert_eq!(copy_of(b"abcde\r\nxy", (0, 0), (1, 1)).as_deref(), Some("abcde\nxy"));
}

#[test]
fn a_wrapped_row_keeps_the_spaces_at_its_wrap_point() {
    assert_eq!(copy_of(b"ab   cd", (0, 0), (1, 1)).as_deref(), Some("ab   cd"));
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
    present(&mut p, t);
    assert!(!p.page(true, 24), "alternate-screen page falls through to raw key input");
    p.send_input(b"\x1b[5~");
    step(&mut p, &a, Step { output: b"\x1b[?1000h", ..Default::default() }, t);
    p.wheel(20);
    p.wheel(-1);
    assert_eq!(a.borrow().inputs[0], b"\x1b[5~");
    assert_eq!(a.borrow().inputs[1..9], vec![b"\x1b[<64;1;1M".to_vec(); 8]);
    assert_eq!(a.borrow().inputs[9], b"\x1b[<65;1;1M");
    assert_eq!(a.borrow().input_runs, vec!["first"; 10]);
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
    p.send_input(b"held");
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
    start(&mut p, plan, |_, _, _| panic!("kept outcome preserves warm storage"), || t);
    assert_eq!(a.borrow().viewed.last(), Some(&true));
    select(&mut p, "h", "c", t, 2);
    assert!(p.view(None).1.iter().any(|s| s.contains("held, not sent (1 keys): held")), "kept-row invalidation keeps held keys");
    // Native kept-default controls separately prove State retains its ledger, view and snapshots.
}

/// A copy the selected row asks for reaches the effect once the installed client has drained what waited for it; one
/// read from that backlog (what the row said while off screen, a warm client's earlier view) is dropped, and so is one
/// in the pump that drains it. A reinstalled warm client starts quiet again.
#[test]
fn a_copy_is_taken_only_after_the_installed_clients_backlog() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let wire = ready(&mut p, "a", "row", b"", t);
    let stale = Step { copy: Some(b"b2xk"), backlog: true, ..Step::default() };
    assert_eq!(step(&mut p, &wire, stale, t).copy, None, "read from the backlog");
    let draining = Step { copy: Some(b"b2xkZXI="), ..Step::default() };
    assert_eq!(step(&mut p, &wire, draining, t).copy, None, "the pump that drains the backlog");
    let fresh = Step { copy: Some(b"bmV3"), ..Step::default() };
    assert_eq!(step(&mut p, &wire, fresh, t).copy.as_deref(), Some(&b"bmV3"[..]), "asked for once on screen");
    assert_eq!(step(&mut p, &wire, Step::default(), t).copy, None, "taken once");
    let (c, wire) = Scripted::new(b"", true);
    select(&mut p, "a", "other", t, 16);
    install(&mut p, "a", "other", c, t);
    let after_switch = Step { copy: Some(b"b2xk"), ..Step::default() };
    assert_eq!(step(&mut p, &wire, after_switch, t).copy, None, "a newly installed client starts quiet");

    // The backlog drains in the middle of a copy's sequence: the copy that ends next began off screen.
    let (c, wire) = Scripted::new(b"", true);
    select(&mut p, "a", "third", t, 16);
    install(&mut p, "a", "third", c, t);
    let mid = Step { mid_sequence: true, ..Step::default() };
    assert_eq!(step(&mut p, &wire, mid, t).copy, None);
    let tail = Step { copy: Some(b"c3RhbGU="), ..Step::default() };
    assert_eq!(step(&mut p, &wire, tail, t).copy, None, "the tail of a copy begun in the backlog");
    let next = Step { copy: Some(b"bmV4dA=="), ..Step::default() };
    assert_eq!(step(&mut p, &wire, next, t).copy.as_deref(), Some(&b"bmV4dA=="[..]), "the next copy is the person's");
}

#[test]
fn a_copy_decodes_to_its_text_and_bad_base64_is_refused_with_why() {
    assert_eq!(copied_text(b"aGVsbG8=").as_deref(), Ok("hello"));
    assert_eq!(copied_text(b"/w==").as_deref(), Ok("\u{fffd}"), "a byte that is not UTF-8");
    assert!(copied_text(b"").unwrap_err().contains("nothing"));
    assert!(copied_text(b"@@@").unwrap_err().contains("base64"));
}

/// The no-session state paints exactly as a new pane, whatever the last session left: an ended client with its
/// screen, a held screen, a dial error, a live client.
#[test]
fn leaving_for_a_row_with_no_attachment_paints_as_a_new_pane() {
    let t = Instant::now();
    let fresh = Pane::new(None).view(None);
    let leftovers: [(&str, fn(&mut Pane, Instant)); 4] = [
        ("ended client", |p, t| {
            let wire = ready(p, "a", "gone", b"work in progress", t);
            step(p, &wire, Step { dead: true, status: Some("ended"), ..Default::default() }, t);
        }),
        ("held screen", |p, t| {
            ready(p, "a", "gone", b"screen of gone", t);
            select(p, "a", "next", t, 16);
        }),
        ("dial error", |p, _| {
            select(p, "a", "gone", Instant::now(), 16);
            p.open_failed(&"a".to_string(), &"a".to_string(), Some("gone"), "unknown workspace: gone");
        }),
        ("live client", |p, t| { ready(p, "a", "gone", b"live", t); }),
    ];
    for (label, leave) in leftovers {
        let mut p = Pane::new(None);
        leave(&mut p, t);
        let shown = p.view(None);
        assert_ne!((shown.0, shown.1), (fresh.0, fresh.1.clone()), "{label}: the leftover must show before the switch");
        p.show_no_attachment();
        let (choice, reasons, _) = p.view(None);
        assert_eq!((choice, reasons, p.target()), (fresh.0, fresh.1.clone(), None), "{label}");
        assert_eq!(pixels(&mut p), (PaneScreen::Empty, "".into()), "{label}");
    }
}

/// The move off a destroyed row clears the pane first, so the gone session's last screen is not held while a cold
/// neighbor attaches.
#[test]
fn a_cold_neighbor_attaches_over_an_empty_pane_after_the_gone_row_is_cleared() {
    let t = Instant::now();
    let mut p = Pane::new(None);
    let wire = ready(&mut p, "a", "gone", b"the destroyed session", t);
    step(&mut p, &wire, Step { dead: true, status: Some("ended"), ..Default::default() }, t);
    p.show_no_attachment();
    select(&mut p, "a", "next", t, 16);
    assert_eq!(pixels(&mut p), (PaneScreen::Empty, "".into()));
    // Without the clear, the departing screen is held under the neighbor.
    let mut q = Pane::new(None);
    let wire = ready(&mut q, "a", "gone", b"the destroyed session", t);
    step(&mut q, &wire, Step { dead: true, status: Some("ended"), ..Default::default() }, t);
    select(&mut q, "a", "next", t, 16);
    assert_eq!(pixels(&mut q).0, PaneScreen::Hold);
}

#[test]
fn a_wide_character_copies_without_a_space_for_its_second_cell() {
    assert_eq!(copy_of("世世".as_bytes(), (0, 0), (0, 3)).as_deref(), Some("世世"));
}

/// Breaks the program made stay newlines in the default copy, whatever the next row begins with: a rule or a table
/// border is one long word, a path or a URL is one, and none of them is a word wrap.
#[test]
fn the_default_copy_keeps_every_break_the_program_made() {
    let rule = "\u{2500}".repeat(20);
    let box_rows = format!("{rule}\r\n> run the tests\r\n{rule}\r\n? for shortcuts");
    let table = "\u{2502} Name  \u{2502} Value \u{2502}\r\n\u{251c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{253c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2524}";
    for (cols, feed, to, expected) in [
        (20, box_rows.as_str(), (3, 14), box_rows.replace("\r\n", "\n")),
        (20, table, (1, 16), table.replace("\r\n", "\n")),
        (20, "/mnt/nas/lab/a/b.jl\r\n/mnt/nas/lab/c/d.jl", (1, 18), "/mnt/nas/lab/a/b.jl\n/mnt/nas/lab/c/d.jl".into()),
        (30, "https://example.org/aaaa\r\nhttps://example.org/bbbb", (1, 23), "https://example.org/aaaa\nhttps://example.org/bbbb".into()),
        (30, "$ pwd\r\n/mnt/nas/lab/results/run_01\r\n$ ", (2, 1), "$ pwd\n/mnt/nas/lab/results/run_01\n$".into()),
        (20, "aaaa bbbb cccc dddd\r\nSo? yes.", (1, 7), "aaaa bbbb cccc dddd\nSo? yes.".into()),
        (6, "ab cde\r\nxy", (1, 1), "ab cde\nxy".into()),
    ] {
        assert_eq!(copy_at(cols, feed.as_bytes(), (0, 0), to).as_deref(), Some(expected.as_str()), "{feed:?}");
    }
}

#[test]
fn a_wide_character_that_did_not_fit_the_last_column_copies_joined() {
    assert_eq!(copy_of("abcd世".as_bytes(), (0, 0), (1, 1)).as_deref(), Some("abcd世"));
}

/// `text` word-wrapped as Claude Code does it (wrap-ansi `trim: false, hard: true` inside a two-column indent):
/// a break where the next word does not fit, a word wider than the row cut at the last column, and the row after an
/// exactly full one starting with a space.
fn claude_wrap(text: &str, cols: usize) -> String {
    let width = cols - 2;
    let mut rows = vec![String::new()];
    for (i, word) in text.split(' ').enumerate() {
        if i > 0 {
            if rows.last().unwrap().len() >= width { rows.push(String::new()); }
            rows.last_mut().unwrap().push(' ');
        }
        let mut rest = word;
        if word.len() > width {
            let take = width.saturating_sub(rows.last().unwrap().len());
            rows.last_mut().unwrap().push_str(&rest[..take]);
            rest = &rest[take..];
            while rest.len() > width {
                rows.push(rest[..width].to_string());
                rest = &rest[width..];
            }
            if !rest.is_empty() { rows.push(String::new()); }
        } else if rows.last().unwrap().len() + word.len() > width && !rows.last().unwrap().trim().is_empty() {
            rows.push(String::new());
        }
        rows.last_mut().unwrap().push_str(rest);
    }
    rows.iter().map(|r| format!("  {r}")).collect::<Vec<_>>().join("\r\n")
}

#[test]
fn copy_as_one_line_joins_a_command_the_program_wrapped() {
    let command = "cd /srv/data && julia --project=. -e 'using Pkg; Pkg.test()' --threads=4 --output=/srv/data/results/run_2026_11_11/long/path/that/is/cut/by/the/program/at/the/width/file.txt && echo done";
    for cols in [24, 40, 60] {
        let feed = claude_wrap(command, cols);
        let rows = feed.matches("\r\n").count() as u16 + 1;
        assert_eq!(copy_line_at(cols as u16, feed.as_bytes(), (0, 2), (rows - 1, cols as u16 - 1)).as_deref(), Some(command), "{cols} columns");
        assert!(copy_at(cols as u16, feed.as_bytes(), (0, 2), (rows - 1, cols as u16 - 1)).unwrap().contains('\n'), "the default copy keeps the breaks");
    }
}

#[test]
fn copy_as_one_line_keeps_a_word_cut_at_the_last_column_whole() {
    assert_eq!(copy_line_at(20, b"aa ABCDEFGHIJKLMNOPQ\r\nRSTUVWXY", (0, 0), (1, 7)).as_deref(), Some("aa ABCDEFGHIJKLMNOPQRSTUVWXY"));
    // Terminal wraps join as in the default copy; the program's own break is one space.
    assert_eq!(copy_line_at(5, b"abcdefgh\r\nxy", (0, 0), (2, 1)).as_deref(), Some("abcdefgh xy"));
}

#[test]
fn copy_as_one_line_adds_a_space_after_a_word_that_ended_exactly_at_the_edge() {
    // A 32-character word cut over two rows, the second exactly full, then "next": wrap-ansi starts that row with a space.
    let feed = format!("  see {}\r\n  {}\r\n   next", "A".repeat(14), "A".repeat(18));
    assert_eq!(copy_line_at(20, feed.as_bytes(), (0, 2), (2, 7)).as_deref(), Some(format!("see {} next", "A".repeat(32)).as_str()));
}

#[test]
fn copy_as_one_line_skips_blank_rows_and_text_the_selection_does_not_reach() {
    assert_eq!(copy_line_at(20, b"alpha\r\n\r\nbeta", (0, 0), (2, 3)).as_deref(), Some("alpha beta"));
    // The selection ends inside the next row's indent: no trailing space.
    assert_eq!(copy_line_at(20, b"  alpha beta gamma\r\n  delta", (0, 0), (1, 1)).as_deref(), Some("  alpha beta gamma"));
    // It starts after the first row's text: no leading space.
    assert_eq!(copy_line_at(20, b"  alpha\r\n  delta", (0, 10), (1, 6)).as_deref(), Some("delta"));
    assert_eq!(copy_line_at(20, b"alpha\r\n\r\n", (0, 0), (2, 5)).as_deref(), Some("alpha"));
}
