use std::time::{Duration, Instant};

use sot_protocol::ops::{ChatCode, ChatPostKind, ChatPostReq, ChatPostRes, ChatTarget};
use winit::keyboard::{Key, NamedKey};

use super::compose::{compose_rows, Compose, Edit};
use super::drawer::ChatDrawer;
use super::draft_edit;
use super::row_channel_tests::{answer, mapped};
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use crate::ui::input::keybindings::Action;
use crate::ui::ResultRowIdentity;

fn row(id: &str) -> ResultRowIdentity {
    ResultRowIdentity { host: "a".to_string(), workspace_id: id.to_string() }
}

fn channel(c: &str) -> ChatTarget {
    serde_json::from_value(serde_json::json!({ "row": "R", "workspace": "T1", "channel": c })).unwrap()
}

fn ch(s: &str) -> Key {
    Key::Character(s.into())
}

/// Row `R`'s draft: its text and the channel it goes to.
fn held<'a>(c: &'a Compose, r: &ResultRowIdentity) -> Option<(&'a str, &'a str)> {
    c.draft(r).map(|d| (d.text.as_str(), d.dest.channel.as_str()))
}

#[test]
fn the_draft_keeps_its_destination_and_its_escaped_bound() {
    // A chord edits the draft only where it types: AltGr does, Ctrl+a does not, and an action's key edits nothing.
    assert!(matches!(draft_edit(None, &ch("@"), (true, true, false)), Some(Edit::Text("@"))));
    assert!(draft_edit(None, &ch("a"), (true, false, false)).is_none());
    assert!(draft_edit(None, &Key::Named(NamedKey::Space), (true, false, false)).is_none());
    assert!(matches!(draft_edit(None, &Key::Named(NamedKey::Space), (false, false, false)), Some(Edit::Space)));
    assert!(matches!(draft_edit(None, &Key::Named(NamedKey::Backspace), (false, false, false)), Some(Edit::Backspace)));
    assert!(matches!(draft_edit(None, &Key::Named(NamedKey::Backspace), (true, false, false)), Some(Edit::Backspace)), "Ctrl+Backspace is Backspace");
    assert!(draft_edit(Some(Action::Paste), &ch("v"), (false, false, true)).is_none(), "a key that resolved to an action types nothing");
    assert!(draft_edit(Some(Action::ChatLatest), &Key::Named(NamedKey::End), (false, false, false)).is_none());
    assert!(draft_edit(None, &Key::Named(NamedKey::Enter), (false, false, false)).is_none(), "Enter posts at B5m, not before");

    // No channel to write to: nothing is begun.
    let (r, c1, c2) = (row("R"), channel("C1"), channel("C2"));
    let mut none = Compose::default();
    assert!(none.edit(&r, None, None, Edit::Text("a")).is_err());

    // The bound counts the escaped form: 1,638 `&` fit and the 1,639th is refused.
    let mut c = Compose::default();
    for n in 1..=1638 {
        assert!(c.edit(&r, Some(&c1), None, Edit::Text("&")).is_ok(), "the {n}th");
    }
    assert!(c.edit(&r, Some(&c1), None, Edit::Text("&")).is_err(), "the 1,639th");
    assert!(c.edit(&r, Some(&c1), None, Edit::Space).is_ok() && c.edit(&r, Some(&c1), None, Edit::Backspace).is_ok());
    assert!(c.edit(&r, Some(&c1), None, Edit::Text("&&")).is_err(), "a paste past it is refused whole");
    assert!(c.edit(&r, Some(&c1), None, Edit::Newline).is_ok(), "a byte that fits still goes in");
    assert!(c.edit(&r, Some(&c1), None, Edit::Text("a\r\nb")).is_err());
    assert_eq!(held(&c, &r).map(|(t, _)| t.to_string()), Some(format!("{}\n", "&".repeat(1638))), "a refused paste adds none of its part that would fit");
    // A first paste past the bound begins no draft, so the next channel is the one it goes to.
    let mut big = Compose::default();
    assert!(big.edit(&r, Some(&c1), None, Edit::Text(&"&".repeat(1639))).is_err());
    assert!(held(&big, &r).is_none());
    // Backspace takes one character, with or without a chord.
    assert!(c.edit(&r, Some(&c1), None, Edit::Backspace).is_ok());
    assert_eq!(held(&c, &r).map(|(t, _)| t.len()), Some(1638));

    // Controls: CR LF and CR are LF, tab stays, every other C0 control goes.
    let mut c = Compose::default();
    c.edit(&r, Some(&c1), None, Edit::Text("a\r\nb\0c\x07d")).unwrap();
    c.edit(&r, Some(&c1), None, Edit::Text("\r\te\x1b")).unwrap();
    c.edit(&r, Some(&c1), None, Edit::Text("\x08f")).unwrap();
    assert_eq!(held(&c, &r), Some(("a\nbcd\n\tef", "C1")), "LF between, tab kept, other controls gone");
    c.edit(&r, Some(&c1), None, Edit::Text("\x7fg\u{85}\u{9b}h")).unwrap();
    assert_eq!(held(&c, &r), Some(("a\nbcd\n\tefgh", "C1")), "DEL and C1 controls go too");

    // The destination is fixed at the first character: the view moves to another channel and the draft stays.
    let mut c = Compose::default();
    c.edit(&r, Some(&c1), None, Edit::Text("hi")).unwrap();
    c.edit(&r, Some(&c2), None, Edit::Text("!")).unwrap();
    assert_eq!(held(&c, &r), Some(("hi!", "C1")));
    // Emptied, the draft is dropped and follows the view again.
    for _ in 0..3 {
        c.edit(&r, Some(&c2), None, Edit::Backspace).unwrap();
    }
    assert_eq!(held(&c, &r), None);
    c.edit(&r, Some(&c2), None, Edit::Text("gone")).unwrap();
    assert_eq!(held(&c, &r), Some(("gone", "C2")));

    // A row a listing retires drops its draft, and no other.
    c.retire(&[row("S")]);
    assert_eq!(held(&c, &r), Some(("gone", "C2")));
    c.retire(&[r.clone()]);
    assert_eq!(held(&c, &r), None);
}

/// A connected drawer on row `R`, its channel `C1` resolved and watched.
fn watching() -> (ChatDrawer, Instant) {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.on_connection(&"a".to_string(), true, t0);
    d.tick(Some(row("R")), t0);
    let at = t0 + Duration::from_millis(200);
    d.tick(Some(row("R")), at);
    d.on_evt(&"a".to_string(), mapped("R"), at);
    (d, at)
}

#[test]
fn the_wired_draft_is_the_shown_rows_and_begins_in_its_own_watch() {
    // chat.newline is an edit; plain Enter is not (it posts at B5m).
    assert!(matches!(draft_edit(Some(Action::ChatNewline), &Key::Named(NamedKey::Enter), (false, false, false)), Some(Edit::Newline)));

    // No shown row, no draft.
    let (mut d, at) = watching();
    assert_eq!(d.edit(None, Edit::Text("a")), Err("no row to write for"));

    // The row is the one given at the key: `S` has no watch yet (the frame has not moved it), so nothing begins in `R`'s
    // channel for it, and `R`'s own draft is untouched.
    d.edit(Some(&row("R")), Edit::Text("hi")).unwrap();
    assert_eq!(d.edit(Some(&row("S")), Edit::Text("x")), Err("no channel to write to"));
    let other_host = ResultRowIdentity { host: "b".to_string(), workspace_id: "R".to_string() };
    assert_eq!(d.edit(Some(&other_host), Edit::Text("x")), Err("no channel to write to"), "the same id on another host is another row");
    assert_eq!(d.draft(), Some(("hi", "C1")));

    // The destination is fixed at the first character: the view's channel moves, the draft stays.
    d.on_evt(&"a".to_string(), answer("R", serde_json::json!({ "state": "mapped", "workspace": "T1", "channel": "C2", "delivers": false })), at);
    d.edit(Some(&row("R")), Edit::Text("!")).unwrap();
    assert_eq!(d.draft(), Some(("hi!", "C1")));

    // A row a listing retires drops its draft.
    d.retire(&[row("R")]);
    assert_eq!(d.draft(), None);
    d.edit(Some(&row("R")), Edit::Text("again")).unwrap();
    assert_eq!(d.draft(), Some(("again", "C2")), "a new draft begins in the channel now watched");
}

/// The shown row's compose rows at a drawer `width` wide and `height` tall.
fn shown(d: &ChatDrawer, focused: bool, width: usize, height: usize) -> Vec<String> {
    compose_rows(&d.compose_text(focused).expect("a channel is known"), width, height)
}

#[test]
fn the_compose_rows_show_the_draft_wrapped_to_the_drawer_and_counted() {
    // No channel known, no rows.
    assert!(ChatDrawer::default().compose_text(true).is_none());

    // The draft's own text keeps its rules (CR and tab), and its rows are what the drawer shows of them.
    let (mut d, _) = watching();
    d.edit(Some(&row("R")), Edit::Text("a\r\nb\0c\x07d")).unwrap();
    d.edit(Some(&row("R")), Edit::Text("\r\te")).unwrap();
    assert_eq!(d.draft(), Some(("a\nbcd\n\te", "C1")), "the text, not only its rows");
    assert_eq!(shown(&d, false, 40, 10), ["> a", "  bcd", "   e", "to C1"]);
    assert_eq!(shown(&d, true, 40, 10), ["> a", "  bcd", "   e█", "to C1"], "the cursor block ends the last row while focused");

    // Two or more rows past three are counted, one is counted in the singular.
    let (mut d, _) = watching();
    d.edit(Some(&row("R")), Edit::Text("1\n2\n3\n4")).unwrap();
    assert_eq!(shown(&d, false, 40, 10), ["> 2", "  3", "  4", "to C1 · 1 line not shown"]);
    d.edit(Some(&row("R")), Edit::Text("\n5")).unwrap();
    assert_eq!(shown(&d, false, 40, 10), ["> 3", "  4", "  5", "to C1 · 2 lines not shown"]);
    d.edit(Some(&row("R")), Edit::Newline).unwrap();
    assert_eq!(shown(&d, false, 40, 10)[3], "to C1 · 3 lines not shown", "a trailing LF is an empty row");

    // A line wider than the drawer wraps, and its rows count toward the three and the count.
    let (mut d, _) = watching();
    d.edit(Some(&row("R")), Edit::Text("abcdefghijklmnopqrstuvwxy")).unwrap();
    assert_eq!(shown(&d, false, 12, 10), ["> abcdefghij", "  klmnopqrst", "  uvwxy", "to C1"]);
    d.edit(Some(&row("R")), Edit::Text("z012345678")).unwrap();
    assert_eq!(shown(&d, false, 12, 10), ["> klmnopqrst", "  uvwxyz0123", "  45678", "to C1 · 1 line not shown"]);

    // A short drawer keeps fewer draft rows, counts the ones it dropped, and the first row it shows keeps its `> `; the
    // destination is the last row dropped.
    assert_eq!(shown(&d, false, 12, 4), ["> klmnopqrst", "  uvwxyz0123", "  45678", "to C1 · 1 line not shown"]);
    assert_eq!(shown(&d, false, 12, 3), ["> uvwxyz0123", "  45678", "to C1 · 2 lines not shown"]);
    assert_eq!(shown(&d, false, 12, 2), ["> 45678", "to C1 · 3 lines not shown"]);
    assert_eq!(shown(&d, false, 12, 1), ["to C1 · 4 lines not shown"]);
}

/// The one post `outs` holds.
fn posted(outs: Vec<(String, OutgoingReq)>) -> ChatPostReq {
    match outs.as_slice() {
        [(_, OutgoingReq::ChatPost { req })] => req.clone(),
        other => panic!("one chat.post expected, got {other:?}"),
    }
}

fn reply(d: &mut ChatDrawer, req: &ChatPostReq, reply: ChatReply<ChatPostRes>, at: Instant) {
    d.on_evt(&"a".to_string(), ChatEvt::Posted { req: req.clone(), reply }, at);
}

#[test]
fn compose_retains_refused_limited_and_unknown_drafts() {
    let r = row("R");
    let (mut d, at) = watching();

    // Nothing to post: no draft, a blank one, no name.
    assert_eq!(d.send(Some(&r), Some("Ada"), at).unwrap_err(), "nothing to post");
    d.edit(Some(&r), Edit::Space).unwrap();
    assert_eq!(d.send(Some(&r), Some("Ada"), at).unwrap_err(), "nothing to post", "blank is nothing");
    d.edit(Some(&r), Edit::Backspace).unwrap();
    d.edit(Some(&r), Edit::Text("a < b")).unwrap();
    assert!(d.send(Some(&r), None, at).unwrap_err().contains("[chat] name"));
    assert_eq!(d.send(None, Some("Ada"), at).unwrap_err(), "no row to write for");
    assert_eq!(d.draft(), Some(("a < b", "C1")));

    // The plain draft goes once, as typed, to the channel it was begun in; the draft is frozen while the post waits.
    let req = posted(d.send(Some(&r), Some("Ada"), at).unwrap());
    assert_eq!((req.text.as_str(), req.name.as_str(), req.target.channel.as_str(), req.thread.as_ref(), req.kind), ("a < b", "Ada", "C1", None, ChatPostKind::Person));
    assert_eq!(d.draft(), Some(("a < b", "C1")), "kept while it waits");
    assert_eq!(d.send(Some(&r), Some("Ada"), at).unwrap_err(), "posting…", "no second post");
    assert_eq!(d.edit(Some(&r), Edit::Text("x")), Err("posting…"));
    assert_eq!(d.edit(Some(&r), Edit::Backspace), Err("posting…"));
    assert_eq!(d.take_notice(), None);

    // Refused bad_request: kept, and the status names the setting; Enter works again.
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Refused { code: ChatCode::BadRequest }), at);
    assert!(d.take_notice().unwrap().contains("[chat] name"));
    assert_eq!(d.draft(), Some(("a < b", "C1")));
    // Any other refusal: kept, with its reason.
    let req = posted(d.send(Some(&r), Some("Ada"), at).unwrap());
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Refused { code: ChatCode::Busy }), at);
    assert_eq!(d.take_notice().as_deref(), Some("not posted: the chat service is busy"));
    d.tick(Some(r.clone()), at);
    assert_eq!(d.take_notice(), None, "a report is said once");
    assert_eq!(d.draft(), Some(("a < b", "C1")));

    // Limited: kept, the words say about N s when assumed, and Enter is refused until the wait passes.
    let req = posted(d.send(Some(&r), Some("Ada"), at).unwrap());
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Limited { retry_after_secs: 30, assumed: true }), at);
    assert!(d.take_notice().unwrap().contains("about 30 s"));
    assert!(d.send(Some(&r), Some("Ada"), at + Duration::from_secs(10)).unwrap_err().contains("about 20 s"), "the seconds left, and still a guess");
    assert_eq!(d.draft(), Some(("a < b", "C1")));
    let req = posted(d.send(Some(&r), Some("Ada"), at + Duration::from_secs(30)).expect("the wait is over at its end"));

    // Unknown: kept, and the words say to look before sending again.
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Unknown), at);
    assert!(d.take_notice().unwrap().starts_with("may have been posted"));
    assert_eq!(d.draft(), Some(("a < b", "C1")));

    // A hub that does not post plain text: the window never wrote it, the draft is kept.
    let req = posted(d.send(Some(&r), Some("Ada"), at + Duration::from_secs(40)).unwrap());
    d.on_evt(&"a".to_string(), ChatEvt::PostNotSent { req }, at);
    assert_eq!(d.take_notice().as_deref(), Some("not posted: the hub must be updated to post"));
    assert_eq!(d.draft(), Some(("a < b", "C1")));

    // A reply for a post that is not waiting changes nothing; the reply that proves the post clears that draft alone.
    let req = posted(d.send(Some(&r), Some("Ada"), at + Duration::from_secs(40)).unwrap());
    let other = ChatPostReq { text: "other".into(), ..req.clone() };
    reply(&mut d, &other, ChatReply::Done(ChatPostRes::Posted { ts: serde_json::from_value(serde_json::json!("1700000000.000100")).unwrap() }), at);
    assert_eq!(d.draft(), Some(("a < b", "C1")));
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Posted { ts: serde_json::from_value(serde_json::json!("1700000000.000100")).unwrap() }), at);
    assert_eq!(d.draft(), None);
    assert_eq!(d.take_notice(), None);
    // A second reply to that post, arriving after the person began another draft, clears nothing.
    d.edit(Some(&r), Edit::Text("next")).unwrap();
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Posted { ts: serde_json::from_value(serde_json::json!("1700000000.000100")).unwrap() }), at);
    assert_eq!(d.draft(), Some(("next", "C1")));

    // A link that drops while the post waits leaves it unknown, and the status line says so.
    let _ = posted(d.send(Some(&r), Some("Ada"), at + Duration::from_secs(60)).unwrap());
    d.on_connection(&"a".to_string(), false, at);
    assert!(d.take_notice().unwrap().starts_with("may have been posted"));
    assert_eq!(d.draft(), Some(("next", "C1")), "kept, and no longer frozen");

    // The hub is unreachable: each Enter says why, though the outcome equals the last one.
    for n in 1..=2 {
        assert!(d.send(Some(&r), Some("Ada"), at + Duration::from_secs(60)).unwrap().is_empty());
        assert_eq!(d.take_notice().as_deref(), Some("not posted: the chat is not available here"), "Enter {n}");
    }
}

/// Moves the drawer to row `to`, mapped to `C1`; the instant it settled.
fn show(d: &mut ChatDrawer, to: &str, at: Instant) -> Instant {
    d.tick(Some(row(to)), at);
    let at = at + Duration::from_millis(200);
    d.tick(Some(row(to)), at);
    d.on_evt(&"a".to_string(), mapped(to), at);
    at
}

#[test]
fn a_posted_reply_clears_the_draft_of_the_row_it_proves_and_no_other() {
    let (mut d, at) = watching();
    d.edit(Some(&row("R")), Edit::Text("from a")).unwrap();
    let req = posted(d.send(Some(&row("R")), Some("Ada"), at).unwrap());
    // The person moves to row S and types there while R's post waits (S's draft is not R's).
    let at = show(&mut d, "S", at + Duration::from_secs(1));
    d.edit(Some(&row("S")), Edit::Text("from b")).unwrap();
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Posted { ts: serde_json::from_value(serde_json::json!("1700000000.000100")).unwrap() }), at);
    assert_eq!(d.draft(), Some(("from b", "C1")), "the shown row's draft stays");
    show(&mut d, "R", at + Duration::from_secs(1));
    assert_eq!(d.draft(), None, "the row the reply proves is cleared");
}
