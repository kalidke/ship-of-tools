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
    s.reconcile(desired, |h, r| {
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
    s.reconcile(want("abc"), |_, _| Err::<(), ()>(()));
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
    let w = Want::new("h".into(), "r1".into(), "files:a.txt".into(), ((1, 2), (3, 4)), "hello").unwrap().selection;
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
    s.reconcile(want("abcd"), |_, _| Err::<(), ()>(()));
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
