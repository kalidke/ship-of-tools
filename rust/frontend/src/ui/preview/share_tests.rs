use super::*;
use sot_protocol::ops::SelectionState;

fn host() -> HostKey { "h".to_string() }

fn want_in(host_name: &str, row: &str, node: &str, text: &str) -> Option<Want> {
    Want::new(host_name.to_string(), row.to_string(), node.to_string(), ((0, 0), (0, text.len())), text)
}

fn want(text: &str) -> Option<Want> { want_in("h", "r1", "files:a.txt", text) }

fn up() -> SelectionShare {
    let mut s = SelectionShare::default();
    s.on_connection(&host(), true);
    s
}

fn req(out: Option<(HostKey, OutgoingReq)>) -> SelectionSetReq {
    match out {
        Some((_, OutgoingReq::SelectionSet { req })) => req,
        _ => panic!("one selection.set expected"),
    }
}

/// Runs `reconcile` with a send that succeeds and returns what it was handed.
fn go(s: &mut SelectionShare, desired: Option<Want>) -> Option<(HostKey, OutgoingReq)> {
    let mut out = None;
    s.reconcile(desired, &[], |h, r| {
        out = Some((h.clone(), r));
        Ok::<(), ()>(())
    });
    out
}

fn done() -> SelectionReply { SelectionReply::Done(SelectionState::Shared) }

/// R10. The orderings of the share plan that a text selection has: 1 (a released range sends), 2 (the range gone sends a
/// clear), 5 (a row switch clears the old row, then sends the new), 6 (only the latest at the reply), 7 (a refusal is
/// not retried), 8 (a failed send waits for the reconnect), 9 and 10 (a connection's end forgets, a stale reply is
/// dropped), and an ended reply shows nothing and is sent again after `Connected`. Ordering 11, nothing during a drag,
/// is `flowed_selection_tests.rs::the_released_range_is_ordered_and_absent_during_a_drag`.
#[test]
fn the_window_shares_its_selection_and_withdraws_it() {
    // Nothing desired, nothing sent; a host that is down is not written to.
    let mut s = SelectionShare::default();
    assert!(go(&mut s, want("abc")).is_none());
    s.on_connection(&host(), true);

    // 1: a released range sends once; the same state again sends nothing, and the reply names the status line.
    let first = req(go(&mut s, want("abc")));
    assert_eq!(first.selection.as_ref().unwrap().len, 3);
    assert!(go(&mut s, want("abc")).is_none(), "one request in flight");
    assert_eq!(s.on_reply(&host(), &first, done()).as_deref(), Some("selection shared: the agent sees it at its next prompt"));
    assert!(go(&mut s, want("abc")).is_none());

    // 6: changes while a request is in flight send only the latest, at the reply.
    let second = req(go(&mut s, want("abcd")));
    assert!(go(&mut s, want("abcde")).is_none());
    assert!(go(&mut s, None).is_none());
    assert!(go(&mut s, want("xy")).is_none());
    s.on_reply(&host(), &second, done());
    let third = req(go(&mut s, want("xy")));
    assert_eq!(third.selection.as_ref().unwrap().len, 2);
    s.on_reply(&host(), &third, done());

    // 2: the range gone sends one clear, which earns no status line.
    let clear = req(go(&mut s, None));
    assert!(clear.selection.is_none() && clear.workspace_id == "r1");
    assert!(s.on_reply(&host(), &clear, done()).is_none());
    assert!(go(&mut s, None).is_none());

    // 5: a row switch clears the row that held the share, then shares the new row's.
    let held = req(go(&mut s, want("abc")));
    s.on_reply(&host(), &held, SelectionReply::Done(SelectionState::Held));
    let other = want_in("h", "r2", "files:b.txt", "q");
    let clear = req(go(&mut s, other.clone()));
    assert!(clear.selection.is_none() && clear.workspace_id == "r1");
    assert!(go(&mut s, other.clone()).is_none());
    s.on_reply(&host(), &clear, done());
    assert_eq!(req(go(&mut s, other)).workspace_id, "r2");
}

/// Ordering 7 and the status lines: a refusal is not retried until the state changes, and each reply kind has its line.
#[test]
fn a_refusal_is_shown_and_not_retried() {
    let mut s = up();
    let r = req(go(&mut s, want("abc")));
    let line = s.on_reply(&host(), &r, SelectionReply::Refused { code: Some("not_file_text".into()), error: "x".into() });
    assert_eq!(line.as_deref(), Some("selection not shared: this view is not the file's text"));
    assert!(go(&mut s, want("abc")).is_none());
    let r = req(go(&mut s, want("abcd")));
    let line = s.on_reply(&host(), &r, SelectionReply::Refused { code: Some("no_agent".into()), error: "x".into() });
    assert_eq!(line.as_deref(), Some("selection not shared: this row's agent takes none"));
    let r = req(go(&mut s, want("ab")));
    assert_eq!(s.on_reply(&host(), &r, SelectionReply::Refused { code: None, error: "boom".into() }).as_deref(), Some("selection not shared: boom"));
    let r = req(go(&mut s, want("abc")));
    let refused = SelectionReply::Refused { code: Some("bad_node_id".into()), error: "selection not shared: bad_node_id".into() };
    assert_eq!(s.on_reply(&host(), &r, refused).as_deref(), Some("selection not shared: bad_node_id"));
    let r = req(go(&mut s, want("ab")));
    assert_eq!(s.on_reply(&host(), &r, SelectionReply::Unsupported).as_deref(), Some("selection not shared: this host's daemon does not take selections"));
    let r = req(go(&mut s, want("abcd")));
    assert_eq!(s.on_reply(&host(), &r, SelectionReply::Done(SelectionState::Other)).as_deref(), Some("selection not shared: unknown answer"));
    let r = req(go(&mut s, want("a")));
    assert!(s.on_reply(&host(), &r, SelectionReply::Done(SelectionState::Held)).unwrap().starts_with("selection held:"));
}

/// Orderings 8 to 10 and the ended reply: nothing is replayed, the share is sent again after `Connected`.
#[test]
fn a_lost_connection_is_answered_after_it_returns() {
    // 8: a send that fails leaves nothing in flight and waits for the reconnect.
    let mut s = up();
    s.reconcile(want("abc"), &[], |_, _| Err::<(), ()>(()));
    assert!(go(&mut s, want("abc")).is_none());
    s.on_connection(&host(), true);
    let r = req(go(&mut s, want("abc")));

    // 10: a reply that does not answer the request in flight is dropped.
    let stale = SelectionSetReq { workspace_id: "r1".into(), selection: None, drawing: None };
    assert!(s.on_reply(&host(), &stale, done()).is_none());

    // The ended reply shows nothing; the host reads as down until `Connected`, then the state is sent again.
    assert!(s.on_reply(&host(), &r, SelectionReply::Ended).is_none());
    assert!(go(&mut s, want("abc")).is_none());
    s.on_connection(&host(), true);
    let r = req(go(&mut s, want("abc")));
    s.on_reply(&host(), &r, done());

    // 9: the link ends: what the daemon held is forgotten (it withdrew it), the request in flight is retired, and the
    // late reply of the ended connection is dropped.
    let r2 = req(go(&mut s, want("abcd")));
    s.on_connection(&host(), false);
    assert!(s.on_reply(&host(), &r2, done()).is_none());
    s.on_connection(&host(), true);
    assert!(go(&mut s, None).is_none(), "nothing is held after the end, so no clear");
    assert!(go(&mut s, want("abcd")).is_some());
}

/// The wire cannot describe an empty or oversized text.
#[test]
fn text_the_wire_cannot_describe_is_not_wanted() {
    assert!(want("").is_none());
    assert!(want("a").is_some());
}

/// What the daemon checks: the range, `len` and the digest of the text.
#[test]
fn the_want_carries_what_the_daemon_checks() {
    let Share::Text(w) = Want::new("h".into(), "r1".into(), "files:a.txt".into(), ((1, 2), (3, 4)), "hello").unwrap().share else { panic!() };
    assert_eq!((w.start.line, w.start.byte, w.end.line, w.end.byte), (1, 2, 3, 4));
    assert_eq!(w.len, 5);
    assert_eq!(w.digest, selection_digest("hello"));
    assert!(Want::new("h".into(), "r".into(), "n".into(), ((0, 0), (0, 0)), &"a".repeat(MAX_SELECTION_LEN as usize + 1)).is_none());
}

/// Two hosts: one's end leaves the other's share and flight alone; a row with the same id on the other host clears the
/// first host's share before it shares; a failed send forgets that host's share, so another host can share.
#[test]
fn hosts_are_kept_apart() {
    let mut s = up();
    s.on_connection(&"g".to_string(), true);
    let r = req(go(&mut s, want("abc")));
    assert!(s.on_reply(&"g".to_string(), &r, done()).is_none(), "a reply from another host answers nothing");
    s.on_connection(&"g".to_string(), false);
    assert!(s.on_reply(&host(), &r, done()).is_some(), "the flight on h still answers");
    assert!(go(&mut s, want("abc")).is_none(), "the share on h survives g's end");
    s.on_connection(&"g".to_string(), true);
    let other = want_in("g", "r1", "files:a.txt", "abc");
    let (to, out) = go(&mut s, other.clone()).unwrap();
    assert_eq!(to, "h");
    let OutgoingReq::SelectionSet { req: clear } = out else { panic!() };
    assert!(clear.selection.is_none());
    s.on_reply(&host(), &clear, done());
    assert_eq!(go(&mut s, other).unwrap().0, "g");

    // A failed send on h, then a selection on g: g shares at once.
    let mut s = up();
    s.on_connection(&"g".to_string(), true);
    let r = req(go(&mut s, want("abc")));
    s.on_reply(&host(), &r, done());
    s.reconcile(want("abcd"), &[], |_, _| Err::<(), ()>(()));
    assert_eq!(go(&mut s, want_in("g", "r2", "files:b.txt", "q")).unwrap().0, "g");
}

/// The table of `wanted`: each missing piece gives nothing.
#[test]
fn wanted_needs_every_piece() {
    let h = host();
    let rel = || Some((((0, 0), (0, 3)), "abc".to_string()));
    assert!(wanted(true, &h, Some("r".into()), Some("n"), rel()).is_some());
    assert!(wanted(false, &h, Some("r".into()), Some("n"), rel()).is_none(), "preview not shown");
    assert!(wanted(true, &h, None, Some("n"), rel()).is_none(), "no row id");
    assert!(wanted(true, &h, Some("r".into()), None, rel()).is_none(), "no node");
    assert!(wanted(true, &h, Some("r".into()), Some("n"), None).is_none(), "mid-drag or none");
}

fn ink_want(row: &str, node: &str, revision: u64) -> Option<Want> {
    let base = InkBase::Figure { node_id: node.to_string() };
    wanted_ink(Some(base.clone()), Some(&base), true, revision, &host(), Some(row.to_string()))
}

fn strokes(n: usize) -> Vec<Vec<[u16; 2]>> { vec![vec![[1, 2], [3, 4]]; n] }

/// Runs `reconcile` with the drawing's strokes.
fn go_ink(s: &mut SelectionShare, desired: Option<Want>, n: usize) -> Option<(HostKey, OutgoingReq)> {
    let mut out = None;
    s.reconcile(desired, &strokes(n), |h, r| {
        out = Some((h.clone(), r));
        Ok::<(), ()>(())
    });
    out
}

/// R-D17. The orderings of the share plan for a drawing: 1 (a new revision sends), 2 (no strokes or no shown base sends
/// a clear), 4 (the base shown again sends again), 5 (a row switch clears, then shares the new row's), 6 (only the latest
/// at the reply), 7 (the status lines, no retry), 8 to 10 (a failed send, a connection's end, a stale reply). Orderings 3, 11
/// and 12 are the decider (`wanted_ink`, tried first in `desired_selection`): the shown base and the committed strokes only.
#[test]
fn the_window_shares_its_drawing_and_withdraws_it() {
    let mut s = up();
    // 1: a drawing sends with its strokes and no selection; the same revision sends nothing.
    let first = req(go_ink(&mut s, ink_want("r1", "files:a.png", 1), 2));
    assert!(first.selection.is_none() && first.drawing.as_ref().unwrap().strokes.len() == 2);
    assert_eq!(first.drawing.as_ref().unwrap().base, InkBase::Figure { node_id: "files:a.png".into() });
    assert!(go_ink(&mut s, ink_want("r1", "files:a.png", 1), 2).is_none(), "one request in flight");
    assert_eq!(s.on_reply(&host(), &first, done()).as_deref(), Some("drawing shared: the agent sees it at its next prompt"));
    assert!(go_ink(&mut s, ink_want("r1", "files:a.png", 1), 2).is_none());

    // 1: an undo that leaves strokes is a new revision; 6: changes in flight send only the latest at the reply.
    let second = req(go_ink(&mut s, ink_want("r1", "files:a.png", 2), 1));
    assert!(go_ink(&mut s, ink_want("r1", "files:a.png", 3), 3).is_none());
    assert!(go_ink(&mut s, None, 0).is_none());
    assert!(go_ink(&mut s, ink_want("r1", "files:a.png", 4), 4).is_none());
    s.on_reply(&host(), &second, done());
    assert_eq!(req(go_ink(&mut s, ink_want("r1", "files:a.png", 4), 4)).drawing.unwrap().strokes.len(), 4);
    let last = s.flight.as_ref().unwrap().req.clone();
    s.on_reply(&host(), &last, done());

    // 2 and 3: no strokes, or the base hidden, is one clear with no status line; 4: shown again sends again.
    let clear = req(go_ink(&mut s, None, 0));
    assert!(clear.selection.is_none() && clear.drawing.is_none() && clear.workspace_id == "r1");
    assert!(s.on_reply(&host(), &clear, done()).is_none());
    assert!(go_ink(&mut s, None, 0).is_none());
    let again = req(go_ink(&mut s, ink_want("r1", "files:a.png", 4), 4));
    s.on_reply(&host(), &again, SelectionReply::Done(SelectionState::Held));

    // 5: a row switch clears the row that held the drawing, then shares the new row's.
    let clear = req(go_ink(&mut s, ink_want("r2", "files:b.png", 5), 1));
    assert!(clear.drawing.is_none() && clear.workspace_id == "r1");
    s.on_reply(&host(), &clear, done());
    assert_eq!(req(go_ink(&mut s, ink_want("r2", "files:b.png", 5), 1)).workspace_id, "r2");
    let last = s.flight.as_ref().unwrap().req.clone();

    // 7: each reply kind has its line, and a refusal is not retried until the revision changes.
    let line = |r: SelectionReply| { let mut s = up(); let q = req(go_ink(&mut s, ink_want("r1", "n", 1), 1)); s.on_reply(&host(), &q, r) };
    let refused = |c: &str| SelectionReply::Refused { code: Some(c.into()), error: "x".into() };
    assert!(line(SelectionReply::Done(SelectionState::Held)).unwrap().starts_with("drawing held:"));
    assert_eq!(line(refused("too_large")).as_deref(), Some("drawing not shared: the image is over 40 megapixels"));
    assert_eq!(line(refused("not_an_image")).as_deref(), Some("drawing not shared: the daemon cannot read this image"));
    assert_eq!(line(refused("no_agent")).as_deref(), Some("drawing not shared: this row's agent takes none"));
    assert_eq!(line(refused("boom")).as_deref(), Some("drawing not shared: boom"));
    assert_eq!(line(SelectionReply::Unsupported).as_deref(), Some("drawing not shared: this daemon cannot take a drawing; update it"));
    s.on_reply(&host(), &last, refused("no_agent"));
    assert!(go_ink(&mut s, ink_want("r2", "files:b.png", 5), 1).is_none());
    assert!(go_ink(&mut s, ink_want("r2", "files:b.png", 6), 1).is_some());

    // 8: a failed send leaves nothing in flight; the reconnect sends. 9 and 10: the link's end forgets, a late reply is dropped.
    let mut s = up();
    s.reconcile(ink_want("r1", "n", 1), &strokes(1), |_, _| Err::<(), ()>(()));
    assert!(go_ink(&mut s, ink_want("r1", "n", 1), 1).is_none());
    s.on_connection(&host(), true);
    let r = req(go_ink(&mut s, ink_want("r1", "n", 1), 1));
    s.on_connection(&host(), false);
    assert!(s.on_reply(&host(), &r, done()).is_none());
    assert!(go_ink(&mut s, None, 0).is_none(), "nothing is held after the end, so no clear");
    s.on_connection(&host(), true);
    assert!(go_ink(&mut s, ink_want("r1", "n", 1), 1).is_some());
    // The ended reply shows nothing; the drawing is sent again after `Connected`.
    let mut s = up();
    let r = req(go_ink(&mut s, ink_want("r1", "n", 1), 1));
    assert!(s.on_reply(&host(), &r, SelectionReply::Ended).is_none());
    s.on_connection(&host(), true);
    assert!(go_ink(&mut s, ink_want("r1", "n", 1), 1).is_some());

    // The decider: the shown base must be the drawn one and have strokes; the canvas is a base; a drawing beats text.
    let (fig, canvas) = (InkBase::Figure { node_id: "n".into() }, InkBase::Canvas);
    let w = |shown: Option<InkBase>, drawn: Option<&InkBase>, has: bool, row: Option<&str>| wanted_ink(shown, drawn, has, 1, &host(), row.map(String::from));
    assert!(w(Some(fig.clone()), Some(&fig), true, Some("r")).is_some());
    assert!(w(Some(canvas.clone()), Some(&canvas), true, Some("r")).is_some());
    assert!(w(None, Some(&fig), true, Some("r")).is_none(), "base hidden");
    assert!(w(Some(canvas.clone()), Some(&fig), true, Some("r")).is_none(), "another base shown");
    assert!(w(Some(fig.clone()), Some(&fig), false, Some("r")).is_none(), "no committed stroke");
    assert!(w(Some(fig.clone()), Some(&fig), true, None).is_none(), "no row id");
}
