use super::*;
use serde_json::json;

fn msg(eval_id: u64, frame: serde_json::Value) -> ReplFrameMsg {
    ReplFrameMsg { eval_id, workspace_id: Some("row".into()), frame, opener: Default::default() }
}

fn kind_and_id(m: Option<ReplFrameMsg>) -> Option<(u64, String)> {
    m.map(|m| (m.eval_id, m.frame["kind"].as_str().unwrap_or("").to_string()))
}

/// A frame-bus subscription to read `running` against.
fn bus() -> broadcast::Receiver<ReplFrameMsg> {
    broadcast::channel(1).1
}

fn done(n: u64) -> ReplFrameMsg {
    msg(n, json!({ "kind": "done", "eval_id": n, "elapsed_ms": 1 }))
}

/// Two windows that both call their run 1 get distinct daemon numbers; each is written its own run under 1,
/// without its `started`, and the other's run under the daemon's number, `started` first.
#[test]
fn each_window_sees_its_own_run_under_its_own_number_and_the_others_under_the_daemons() {
    let (a, b) = (CallerNumbers::default(), CallerNumbers::default());
    let (na, nb) = (a.number(Some(1)), b.number(Some(1)));
    assert!(na >= 1 << 40 && nb >= 1 << 40 && na != nb, "{na} {nb}");
    let started = |n| msg(n, json!({ "kind": "started", "run_id": format!("exec-{n}"), "origin": "window", "display": "x" }));
    assert_eq!(kind_and_id(a.outgoing(started(na))), None, "a window is never announced its own run");
    assert_eq!(kind_and_id(b.outgoing(started(na))), Some((na, "started".into())));
    let out = msg(na, json!({ "kind": "stdout", "text": "from a" }));
    assert_eq!(kind_and_id(a.outgoing(out.clone())), Some((1, "stdout".into())));
    assert_eq!(kind_and_id(b.outgoing(out)), Some((na, "stdout".into())));
    let mine = a.outgoing(done(na)).expect("a's own done");
    assert_eq!((mine.eval_id, mine.frame["eval_id"].clone()), (1, json!(1)), "the done names the window's number too");
    assert_eq!(b.outgoing(done(na)).expect("b sees a's done").eval_id, na);
    assert_eq!(kind_and_id(b.outgoing(msg(nb, json!({ "kind": "stdout", "text": "from b" })))), Some((1, "stdout".into())));
}

/// Each connection of a window writes the run's `done`: one that writes it first (a new connection, or an old one
/// winding down) leaves the run for the others, so each writes it under the window's number.
#[test]
fn every_connection_of_a_window_writes_its_runs_done_under_its_number() {
    let window = CallerNumbers::default();
    let (old, new) = (window.clone(), window.clone());
    let n = window.number(Some(3));
    for conn in [&old, &new] {
        let written = conn.outgoing(done(n)).expect("the done");
        assert_eq!((written.eval_id, written.frame["eval_id"].clone()), (3, json!(3)));
    }
}

/// A window's number names one run: a later run it numbers the same (a window relaunched under the same instance
/// counts from 1 again) takes the number, and the earlier run's frames reach it under the daemon's number.
#[test]
fn a_windows_number_names_one_run() {
    let window = CallerNumbers::default();
    let before = window.number(Some(1));
    let after = window.number(Some(1));
    let out = |n| msg(n, json!({ "kind": "stdout", "text": "x" }));
    assert_eq!(kind_and_id(window.outgoing(out(after))), Some((1, "stdout".into())));
    assert_eq!(kind_and_id(window.outgoing(out(before))), Some((before, "stdout".into())));
    assert_eq!(window.daemon_numbers(&[1]), vec![after]);
}

/// A run the daemon refused its window is written to that window not at all, its `done` included (the reply said
/// why), and to every other window as any run is.
#[test]
fn a_refused_run_is_written_to_its_window_not_at_all() {
    let (a, b) = (CallerNumbers::default(), CallerNumbers::default());
    let n = a.number(Some(4));
    a.refused(n);
    for frame in [
        json!({ "kind": "started", "run_id": format!("exec-{n}"), "origin": "window", "display": "x" }),
        json!({ "kind": "error", "message": "the REPL's queue is full", "stacktrace": [] }),
        json!({ "kind": "done", "eval_id": n, "elapsed_ms": 0 }),
        json!({ "kind": "stdout", "text": "after its done" }),
    ] {
        assert_eq!(kind_and_id(a.outgoing(msg(n, frame.clone()))), None, "{frame}");
        assert_eq!(b.outgoing(msg(n, frame)).map(|m| m.eval_id), Some(n));
    }
}

/// A hello names a window's run from its numbering to its end: not once refused (before a hello named it), not
/// once ended, and never a run the window did not number.
#[test]
fn a_window_names_its_runs_only_while_they_run() {
    let (a, bus) = (CallerNumbers::default(), bus());
    let (one, two, three) = (a.number(Some(1)), a.number(Some(2)), a.number(Some(3)));
    let theirs = a.number(None);
    assert_eq!(a.running(&bus), [1, 2, 3], "numbered, not accepted: named, for a window that redials during the start");
    for n in [one, two, theirs] {
        a.accepted(n);
    }
    a.refused(three);
    a.accepted(three);
    assert_eq!(a.running(&bus), [1, 2], "accepted, not yet written to the child: its done still to come");
    a.end_then(one, || {});
    a.accepted(one);
    assert_eq!(a.running(&bus), [2], "ended, and accepting it late does not reopen it");
    assert_eq!(kind_and_id(a.outgoing(done(one))), Some((1, "done".into())), "an ended run is still the window's");
}

/// A hello that reads a run while it is still waiting for the REPL's queue names it: the window redials during the
/// REPL's start and the run is accepted after. If the queue then refuses it, the reply went to the dropped
/// connection, so the run's reason and `done` are written to the window that was named it.
#[test]
fn a_run_named_while_waiting_and_then_refused_is_written_its_end() {
    let (a, bus) = (CallerNumbers::default(), bus());
    let named = a.number(Some(1));
    assert_eq!(a.running(&bus), [1], "a numbered run is named");
    let unnamed = a.number(Some(2));
    a.refused(unnamed);
    assert_eq!(a.running(&bus), [1], "a run refused before any hello is named by none");
    a.refused(named);
    assert!(a.running(&bus).is_empty());
    let error = msg(named, json!({ "kind": "error", "message": "no REPL", "stacktrace": [] }));
    assert_eq!(kind_and_id(a.outgoing(error)), Some((1, "error".into())));
    assert_eq!(kind_and_id(a.outgoing(done(named))), Some((1, "done".into())));
    assert_eq!(kind_and_id(a.outgoing(done(unnamed))), None, "never named: the reply said why");
}

/// A run is marked ended before its `done` is published: from inside `publish` a hello already names it no more.
#[test]
fn a_run_is_marked_ended_before_its_done_is_published() {
    let (a, bus) = (CallerNumbers::default(), bus());
    let n = a.number(Some(1));
    a.accepted(n);
    assert_eq!(a.running(&bus), [1]);
    let mut named_at_publish = None;
    a.end_then(n, || named_at_publish = Some(a.running(&bus)));
    assert_eq!(named_at_publish, Some(vec![]), "the run was still named when its done was published");
}

/// An interrupt names a window's own runs by its numbers and announced runs by the daemon's; both reach the
/// REPL as the daemon's numbers, and a number of no run stays as it is.
#[test]
fn an_interrupt_names_the_daemons_numbers() {
    let a = CallerNumbers::default();
    let mine = a.number(Some(7));
    let announced = next();
    assert_eq!(a.daemon_numbers(&[7, announced, 3]), vec![mine, announced, 3]);
}

/// A window keeps at most `REMEMBERED` of its runs: a finished one goes first, then the oldest.
#[test]
fn a_window_remembers_a_bounded_number_of_runs_finished_ones_going_first() {
    let a = CallerNumbers::default();
    let running = a.number(Some(1));
    let finished = a.number(Some(2));
    a.end_then(finished, || {});
    for own in 3..=(REMEMBERED as u64 + 1) {
        a.number(Some(own));
    }
    assert_eq!(a.runs().len(), REMEMBERED);
    assert!(!a.runs().contains_key(&finished), "the finished run went first");
    assert!(a.runs().contains_key(&running), "the run still going stays");
    a.number(Some(REMEMBERED as u64 + 2));
    assert!(!a.runs().contains_key(&running), "then the oldest");
    assert!(a.number(None) > running);
    assert_eq!(a.runs().len(), REMEMBERED, "a run the window did not number is not remembered");
}
