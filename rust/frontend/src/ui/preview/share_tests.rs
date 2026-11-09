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
    assert!(s.reconcile(want("abc")).is_none());
    s.on_connection(&host(), true);

    // 1: a released range sends once; the same state again sends nothing, and the reply names the status line.
    let first = req(s.reconcile(want("abc")));
    assert_eq!(first.selection.as_ref().unwrap().len, 3);
    assert!(s.reconcile(want("abc")).is_none(), "one request in flight");
    assert_eq!(s.on_reply(&host(), &first, done()).as_deref(), Some("selection shared: the agent sees it at its next prompt"));
    assert!(s.reconcile(want("abc")).is_none());

    // 6: changes while a request is in flight send only the latest, at the reply.
    let second = req(s.reconcile(want("abcd")));
    assert!(s.reconcile(want("abcde")).is_none());
    assert!(s.reconcile(None).is_none());
    assert!(s.reconcile(want("xy")).is_none());
    s.on_reply(&host(), &second, done());
    let third = req(s.reconcile(want("xy")));
    assert_eq!(third.selection.as_ref().unwrap().len, 2);
    s.on_reply(&host(), &third, done());

    // 2: the range gone sends one clear, which earns no status line.
    let clear = req(s.reconcile(None));
    assert!(clear.selection.is_none() && clear.workspace_id == "r1");
    assert!(s.on_reply(&host(), &clear, done()).is_none());
    assert!(s.reconcile(None).is_none());

    // 5: a row switch clears the row that held the share, then shares the new row's.
    let held = req(s.reconcile(want("abc")));
    s.on_reply(&host(), &held, SelectionReply::Done(SelectionState::Held));
    let other = want_in("h", "r2", "files:b.txt", "q");
    let clear = req(s.reconcile(other.clone()));
    assert!(clear.selection.is_none() && clear.workspace_id == "r1");
    assert!(s.reconcile(other.clone()).is_none());
    s.on_reply(&host(), &clear, done());
    assert_eq!(req(s.reconcile(other)).workspace_id, "r2");
}

/// Ordering 7 and the status lines: a refusal is not retried until the state changes, and each reply kind has its line.
#[test]
fn a_refusal_is_shown_and_not_retried() {
    let mut s = up();
    let r = req(s.reconcile(want("abc")));
    let line = s.on_reply(&host(), &r, SelectionReply::Refused { code: Some("not_file_text".into()), error: "x".into() });
    assert_eq!(line.as_deref(), Some("selection not shared: this view is not the file's text"));
    assert!(s.reconcile(want("abc")).is_none());
    let r = req(s.reconcile(want("abcd")));
    let line = s.on_reply(&host(), &r, SelectionReply::Refused { code: Some("no_agent".into()), error: "x".into() });
    assert_eq!(line.as_deref(), Some("selection not shared: this row's agent takes none"));
    let r = req(s.reconcile(want("ab")));
    assert_eq!(s.on_reply(&host(), &r, SelectionReply::Refused { code: None, error: "boom".into() }).as_deref(), Some("selection not shared: boom"));
    let r = req(s.reconcile(want("a")));
    assert!(s.on_reply(&host(), &r, SelectionReply::Done(SelectionState::Held)).unwrap().starts_with("selection held:"));
}

/// Orderings 8 to 10 and the ended reply: nothing is replayed, the share is sent again after `Connected`.
#[test]
fn a_lost_connection_is_answered_after_it_returns() {
    // 8: a send that fails leaves nothing in flight and waits for the reconnect.
    let mut s = up();
    req(s.reconcile(want("abc")));
    s.send_failed(&host());
    assert!(s.reconcile(want("abc")).is_none());
    s.on_connection(&host(), true);
    let r = req(s.reconcile(want("abc")));

    // 10: a reply that does not answer the request in flight is dropped.
    let stale = SelectionSetReq { workspace_id: "r1".into(), selection: None, drawing: None };
    assert!(s.on_reply(&host(), &stale, done()).is_none());

    // The ended reply shows nothing; the host reads as down until `Connected`, then the state is sent again.
    assert!(s.on_reply(&host(), &r, SelectionReply::Ended).is_none());
    assert!(s.reconcile(want("abc")).is_none());
    s.on_connection(&host(), true);
    let r = req(s.reconcile(want("abc")));
    s.on_reply(&host(), &r, done());

    // 9: the link ends: what the daemon held is forgotten (it withdrew it), the request in flight is retired, and the
    // late reply of the ended connection is dropped.
    let r2 = req(s.reconcile(want("abcd")));
    s.on_connection(&host(), false);
    assert!(s.on_reply(&host(), &r2, done()).is_none());
    s.on_connection(&host(), true);
    assert!(s.reconcile(None).is_none(), "nothing is held after the end, so no clear");
    assert!(s.reconcile(want("abcd")).is_some());
}

/// The wire cannot describe an empty or oversized text.
#[test]
fn text_the_wire_cannot_describe_is_not_wanted() {
    assert!(want("").is_none());
    assert!(want("a").is_some());
}
