use std::time::{Duration, Instant};

use sot_protocol::ops::{ChatCode, ChatPostKind, ChatPostReq, ChatPostRes, ChatTarget};
use winit::keyboard::{Key, NamedKey};

use super::compose::{compose_rows, Compose, Edit};
use super::{apply_name, name_step, NameStep};
use crate::ui::persist::settings::Settings;
use sot_protocol::chat::MAX_NAME;
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
    assert!(none.edit(&r, None, Edit::Text("a")).is_err());

    // The bound counts the escaped form: 1,638 `&` fit and the 1,639th is refused.
    let mut c = Compose::default();
    for n in 1..=1638 {
        assert!(c.edit(&r, Some((&c1, None)), Edit::Text("&")).is_ok(), "the {n}th");
    }
    assert!(c.edit(&r, Some((&c1, None)), Edit::Text("&")).is_err(), "the 1,639th");
    assert!(c.edit(&r, Some((&c1, None)), Edit::Space).is_ok() && c.edit(&r, Some((&c1, None)), Edit::Backspace).is_ok());
    assert!(c.edit(&r, Some((&c1, None)), Edit::Text("&&")).is_err(), "a paste past it is refused whole");
    assert!(c.edit(&r, Some((&c1, None)), Edit::Newline).is_ok(), "a byte that fits still goes in");
    assert!(c.edit(&r, Some((&c1, None)), Edit::Text("a\r\nb")).is_err());
    assert_eq!(held(&c, &r).map(|(t, _)| t.to_string()), Some(format!("{}\n", "&".repeat(1638))), "a refused paste adds none of its part that would fit");
    // A first paste past the bound begins no draft, so the next channel is the one it goes to.
    let mut big = Compose::default();
    assert!(big.edit(&r, Some((&c1, None)), Edit::Text(&"&".repeat(1639))).is_err());
    assert!(held(&big, &r).is_none());
    // Backspace takes one character, with or without a chord.
    assert!(c.edit(&r, Some((&c1, None)), Edit::Backspace).is_ok());
    assert_eq!(held(&c, &r).map(|(t, _)| t.len()), Some(1638));

    // Controls: CR LF and CR are LF, tab stays, every other C0 control goes.
    let mut c = Compose::default();
    c.edit(&r, Some((&c1, None)), Edit::Text("a\r\nb\0c\x07d")).unwrap();
    c.edit(&r, Some((&c1, None)), Edit::Text("\r\te\x1b")).unwrap();
    c.edit(&r, Some((&c1, None)), Edit::Text("\x08f")).unwrap();
    assert_eq!(held(&c, &r), Some(("a\nbcd\n\tef", "C1")), "LF between, tab kept, other controls gone");
    c.edit(&r, Some((&c1, None)), Edit::Text("\x7fg\u{85}\u{9b}h")).unwrap();
    assert_eq!(held(&c, &r), Some(("a\nbcd\n\tefgh", "C1")), "DEL and C1 controls go too");

    // The destination is fixed at the first character: the view moves to another channel and the draft stays.
    let mut c = Compose::default();
    c.edit(&r, Some((&c1, None)), Edit::Text("hi")).unwrap();
    c.edit(&r, Some((&c2, None)), Edit::Text("!")).unwrap();
    assert_eq!(held(&c, &r), Some(("hi!", "C1")));
    // Emptied, the draft is dropped and follows the view again.
    for _ in 0..3 {
        c.edit(&r, Some((&c2, None)), Edit::Backspace).unwrap();
    }
    assert_eq!(held(&c, &r), None);
    c.edit(&r, Some((&c2, None)), Edit::Text("gone")).unwrap();
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
    // The title now names C2, so the foot says where this draft goes; for C1, the title's own channel, it did not.
    assert_eq!(shown(&d, false, 40, 10), ["> hi!", "to C1"]);

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
    assert_eq!(shown(&d, false, 40, 10), ["> a", "  bcd", "   e"]);
    assert_eq!(shown(&d, true, 40, 10), ["> a", "  bcd", "   e█"], "the cursor block ends the last row while focused");

    // Two or more rows past three are counted, one is counted in the singular.
    let (mut d, _) = watching();
    d.edit(Some(&row("R")), Edit::Text("1\n2\n3\n4")).unwrap();
    assert_eq!(shown(&d, false, 40, 10), ["> 2", "  3", "  4", "1 line not shown"]);
    d.edit(Some(&row("R")), Edit::Text("\n5")).unwrap();
    assert_eq!(shown(&d, false, 40, 10), ["> 3", "  4", "  5", "2 lines not shown"]);
    d.edit(Some(&row("R")), Edit::Newline).unwrap();
    assert_eq!(shown(&d, false, 40, 10)[3], "3 lines not shown", "a trailing LF is an empty row");

    // A line wider than the drawer wraps, and its rows count toward the three and the count.
    let (mut d, _) = watching();
    d.edit(Some(&row("R")), Edit::Text("abcdefghijklmnopqrstuvwxy")).unwrap();
    assert_eq!(shown(&d, false, 12, 10), ["> abcdefghij", "  klmnopqrst", "  uvwxy"]);
    d.edit(Some(&row("R")), Edit::Text("z012345678")).unwrap();
    assert_eq!(shown(&d, false, 12, 10), ["> klmnopqrst", "  uvwxyz0123", "  45678", "1 line not shown"]);

    // A short drawer keeps fewer draft rows, counts the ones it dropped, and the first row it shows keeps its `> `; the
    // count is the last row dropped, and it is there only when something was.
    assert_eq!(shown(&d, false, 12, 4), ["> klmnopqrst", "  uvwxyz0123", "  45678", "1 line not shown"]);
    assert_eq!(shown(&d, false, 12, 3), ["> uvwxyz0123", "  45678", "2 lines not shown"]);
    assert_eq!(shown(&d, false, 12, 2), ["> 45678", "3 lines not shown"]);
    assert_eq!(shown(&d, false, 12, 1), ["4 lines not shown"]);
}

/// The destination row is there only when it has something to say, and the count of hidden rows is true at every height,
/// for a draft of one, two or three rows with no destination.
#[test]
fn a_foot_with_no_destination_has_a_row_only_for_what_it_cannot_show() {
    let rows = |n: usize, height: usize| {
        let lines = ["a", "b", "c"][..n].iter().map(|s| s.to_string()).collect();
        compose_rows(&super::compose::ComposeText::Draft { lines, to: None }, 40, height)
    };
    assert_eq!(rows(3, 3), ["> a", "  b", "  c"]);
    assert_eq!(rows(3, 9), ["> a", "  b", "  c"]);
    assert_eq!(rows(3, 2), ["> c", "2 lines not shown"]);
    assert_eq!(rows(3, 1), ["3 lines not shown"]);
    assert_eq!(rows(2, 3), ["> a", "  b"]);
    assert_eq!(rows(2, 2), ["> a", "  b"]);
    assert_eq!(rows(2, 1), ["2 lines not shown"]);
    assert_eq!(rows(1, 3), ["> a"]);
    assert_eq!(rows(1, 1), ["> a"]);
    assert!(rows(1, 0).is_empty() && rows(3, 0).is_empty());
    // With a destination the row is always there, and the count joins it.
    let with = |n: usize, height: usize| {
        let lines = ["a", "b", "c"][..n].iter().map(|s| s.to_string()).collect();
        compose_rows(&super::compose::ComposeText::Draft { lines, to: Some("to C1".into()) }, 40, height)
    };
    assert_eq!(with(3, 4), ["> a", "  b", "  c", "to C1"]);
    assert_eq!(with(3, 3), ["> b", "  c", "to C1 · 1 line not shown"]);
    assert_eq!(with(1, 1), ["to C1 · 1 line not shown"]);
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

/// The prompt open over row R's draft.
fn asking() -> (ChatDrawer, Instant) {
    let (mut d, at) = watching();
    d.edit(Some(&row("R")), Edit::Text("hello")).unwrap();
    d.ask_name();
    assert_eq!(d.asking(), Some(""));
    (d, at)
}

#[test]
fn the_name_prompt_edits_the_name_not_the_draft_and_escape_keeps_it() {
    let (mut d, _) = asking();
    let foot = |d: &ChatDrawer, w| compose_rows(&d.compose_text(true).unwrap(), w, 10);
    assert_eq!(foot(&d, 40), ["Name for your chat posts: █"]);

    // The keys edit the name, not the draft.
    for e in [Edit::Text("Ad"), Edit::Text("x"), Edit::Backspace, Edit::Text("a"), Edit::Space, Edit::Newline] {
        d.edit_name(e).unwrap();
    }
    assert_eq!(d.asking(), Some("Ada "));
    assert_eq!(d.draft(), Some(("hello", "C1")));
    assert_eq!(foot(&d, 40), ["Name for your chat posts: Ada █"]);
    assert_eq!(foot(&d, 29), ["Name for your chat posts: a █"], "a narrow foot keeps the end of the name");
    d.ask_name();
    assert_eq!(d.asking(), Some("Ada "), "asking again keeps a name half typed");

    // Escape cancels and keeps the draft; the foot is the draft's again.
    d.cancel_name();
    assert_eq!(d.asking(), None);
    assert_eq!(d.draft(), Some(("hello", "C1")));
    assert_eq!(foot(&d, 40), ["> hello█"]);
}

#[test]
fn enter_with_no_name_opens_the_prompt_and_sends_nothing_until_the_next_enter() {
    let r = row("R");
    let (mut d, at) = watching();
    // No draft: nothing to post, and no prompt.
    assert_eq!(d.send(Some(&r), None, at).unwrap_err(), "nothing to post");
    assert_eq!(d.asking(), None);

    d.edit(Some(&r), Edit::Text("hello")).unwrap();
    assert!(d.send(Some(&r), None, at).unwrap().is_empty(), "no post is sent");
    assert_eq!(d.asking(), Some(""));
    d.edit_name(Edit::Text("Ad")).unwrap();
    assert!(d.send(Some(&r), None, at).unwrap().is_empty());
    assert_eq!(d.asking(), Some("Ad"), "Enter again keeps a name half typed");

    // Saved (apply_name's part), the next Enter posts under the name.
    d.cancel_name();
    let req = posted(d.send(Some(&r), Some("Ada"), at).unwrap());
    assert_eq!((req.name.as_str(), req.text.as_str()), ("Ada", "hello"));
    assert_eq!(d.asking(), None);
}

#[test]
fn enter_with_no_name_on_a_row_that_is_not_mapped_says_why_and_opens_no_prompt() {
    let (mut d, at) = watching();
    d.edit(Some(&row("R")), Edit::Text("hello")).unwrap();
    d.on_connection(&"a".to_string(), false, at);
    let words = d.send(Some(&row("R")), None, at).unwrap_err();
    assert!(words.starts_with("not posted: ") && words.contains("not connected"), "{words}");
    assert_eq!(d.asking(), None);
}

#[test]
fn the_prompt_ends_where_the_view_it_asked_in_ends_and_keeps_the_draft() {
    // The same row and mapping: it stays, and Enter again does not empty a name half typed.
    let (mut d, at) = asking();
    d.edit_name(Edit::Text("Ad")).unwrap();
    d.tick(Some(row("R")), at + Duration::from_secs(1));
    d.ask_name();
    assert_eq!(d.asking(), Some("Ad"));

    // Another row is shown.
    d.tick(Some(row("S")), at + Duration::from_secs(2));
    assert_eq!(d.asking(), None, "a row change");
    assert_eq!(d.draft(), None, "row S has no draft");
    // The drawer is hidden (a frame with no wanted row, and the hide call).
    let (mut d, _) = asking();
    d.hide();
    assert_eq!(d.asking(), None, "a hide");
    let (mut d, at2) = asking();
    d.tick(None, at2 + Duration::from_secs(1));
    assert_eq!(d.asking(), None, "no wanted row");
    // The link changes.
    let (mut d, at3) = asking();
    d.on_connection(&"a".to_string(), false, at3);
    assert_eq!(d.asking(), None, "a link change");
    // The row's mapping is gone: unmapped.
    let (mut d, at4) = asking();
    d.on_evt(&"a".to_string(), answer("R", serde_json::json!({ "state": "unmapped" })), at4);
    d.tick(Some(row("R")), at4 + Duration::from_secs(1));
    assert_eq!(d.asking(), None, "an unmapped row");
    // The draft was kept through each.
    let (mut d, _) = asking();
    d.hide();
    assert_eq!(d.draft(), Some(("hello", "C1")));
}

#[test]
fn a_pasted_control_character_or_a_name_past_the_bound_is_refused_whole() {
    let (mut d, _) = asking();
    d.edit_name(Edit::Text("Ada")).unwrap();
    assert!(d.edit_name(Edit::Text("a\nb")).unwrap_err().contains("control"));
    assert!(d.edit_name(Edit::Text("a\tb")).is_err());
    assert_eq!(d.asking(), Some("Ada"), "nothing joined");
    d.edit_name(Edit::Text(&"n".repeat(MAX_NAME - 3))).unwrap();
    assert_eq!(d.asking().map(str::len), Some(MAX_NAME));
    assert_eq!(d.edit_name(Edit::Space), Err("name is full: nothing added"));
    assert!(d.edit_name(Edit::Text("é")).is_err());
    d.edit_name(Edit::Backspace).unwrap();
    assert!(d.edit_name(Edit::Text("é")).is_err(), "the bound counts bytes");
    d.edit_name(Edit::Text("x")).unwrap();
    assert_eq!(d.asking().map(str::len), Some(MAX_NAME));
}

#[test]
fn the_name_prompt_row_shows_the_cursor_only_while_focused_and_fits_the_height() {
    let (mut d, _) = asking();
    d.edit_name(Edit::Text("Ada")).unwrap();
    assert_eq!(compose_rows(&d.compose_text(false).unwrap(), 40, 10), ["Name for your chat posts: Ada"], "no block unfocused");
    assert_eq!(compose_rows(&d.compose_text(true).unwrap(), 40, 10), ["Name for your chat posts: Ada█"]);
    assert!(compose_rows(&d.compose_text(true).unwrap(), 40, 0).is_empty(), "never more rows than the height");
}

fn step(action: Option<Action>, key: Key, repeat: bool, mods: (bool, bool, bool)) -> Option<NameStep<'static>> {
    // The keys here are characters or named keys: `Edit::Text` borrows its text from the key, so the table leaks it.
    let key: &'static Key = Box::leak(Box::new(key));
    name_step(action, key, repeat, mods)
}

#[test]
fn name_step_says_what_each_key_is_to_the_prompt() {
    let none = (false, false, false);
    assert_eq!(step(Some(Action::ChatSend), Key::Named(NamedKey::Enter), false, none), Some(NameStep::Save));
    assert_eq!(step(Some(Action::ReturnNav), Key::Named(NamedKey::Escape), false, none), Some(NameStep::Cancel));
    assert_eq!(step(Some(Action::Paste), ch("v"), false, (true, false, false)), Some(NameStep::Paste));
    assert_eq!(step(Some(Action::ChatNewline), Key::Named(NamedKey::Enter), false, none), Some(NameStep::Edit(Edit::Newline)));
    assert_eq!(step(None, ch("a"), false, none), Some(NameStep::Edit(Edit::Text("a"))));
    assert_eq!(step(None, Key::Named(NamedKey::Space), false, none), Some(NameStep::Edit(Edit::Space)));
    assert_eq!(step(None, Key::Named(NamedKey::Backspace), false, none), Some(NameStep::Edit(Edit::Backspace)));
    // A held Enter, Escape or paste is taken and does nothing.
    for a in [Action::ChatSend, Action::ReturnNav, Action::Paste] {
        assert_eq!(step(Some(a), ch("x"), true, none), Some(NameStep::Held));
    }
    // A reading key is not the prompt's, so it still scrolls, marks and opens a thread.
    for a in [Action::ScrollPageUp, Action::ChatLatest, Action::ChatMarkOlder, Action::ChatThreadOpen] {
        assert_eq!(step(Some(a), Key::Named(NamedKey::ArrowUp), false, none), None);
    }
}

/// A folder under the temporary one, removed when the test ends, a failed assertion included.
struct Folder(std::path::PathBuf);

impl Folder {
    fn new(name: &str) -> Folder {
        let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Folder(dir)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A settings file under a folder that goes with the test; the setting is as loaded from it.
fn settings_in(dir: &std::path::Path) -> (Settings, std::path::PathBuf) {
    let f = dir.join("settings.toml");
    std::fs::write(&f, "[trust]\nroot_prefix = \"/w\"\n").unwrap();
    (Settings { source: Some(f.clone()), ..Settings::default() }, f)
}

#[test]
fn apply_name_saves_closes_the_prompt_and_the_next_enter_posts_under_it() {
    let dir = Folder::new("sot-name-apply");
    let env = sot_platform::host::env::Env::empty();
    let (mut d, at) = asking();
    let (mut settings, file) = settings_in(&dir.0);
    // A refusal says why, leaves the prompt open, the file and the setting as they were.
    assert!(apply_name(&mut d, &mut settings, &env).contains("empty"));
    assert_eq!((d.asking(), settings.chat_name.as_deref()), (Some(""), None));
    // A file the write cannot edit is refused too.
    std::fs::write(&file, "[trust\n").unwrap();
    d.edit_name(Edit::Text("Ada")).unwrap();
    assert!(apply_name(&mut d, &mut settings, &env).contains("will not edit"));
    assert_eq!((d.asking(), settings.chat_name.as_deref()), (Some("Ada"), None));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "[trust\n");
    // A good file: saved, the prompt closed, the setting set, the draft kept, and the next Enter posts under the name.
    std::fs::write(&file, "[trust]\nroot_prefix = \"/w\"\n").unwrap();
    let words = apply_name(&mut d, &mut settings, &env);
    assert!(words.starts_with("chat name saved to"), "{words}");
    assert_eq!((d.asking(), settings.chat_name.as_deref()), (None, Some("Ada")));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "[trust]\nroot_prefix = \"/w\"\n\n[chat]\nname = \"Ada\"\n");
    let req = posted(d.send(Some(&row("R")), settings.chat_name.as_deref(), at).unwrap());
    assert_eq!((req.name.as_str(), req.text.as_str()), ("Ada", "hello"));
}
