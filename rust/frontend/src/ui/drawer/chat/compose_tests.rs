use std::time::{Duration, Instant};

use sot_protocol::ops::{ChatCode, ChatPostFileReq, ChatPostFileRes, ChatPostKind, ChatPostReq, ChatPostRes, ChatTarget, ChatUnlinked, ChatUnlinkedWhy};
use winit::keyboard::{Key, NamedKey};

use super::compose::{compose_rows, Compose, Edit};
use super::{apply_name, prompt_step, PromptStep};
use crate::ui::persist::settings::Settings;
use sot_protocol::chat::MAX_NAME;
use super::drawer::{ChatDrawer, Previewed};
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

/// A connected drawer on row `R`, its channel `C1` resolved and watched, on a hub whose hello says it does not post as the
/// person.
fn watching() -> (ChatDrawer, Instant) {
    watching_on(false)
}

fn watching_on(as_person: bool) -> (ChatDrawer, Instant) {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.on_connection(&"a".to_string(), true, as_person, t0);
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
    assert_eq!(d.edit(None, Edit::Text("a")), Err("no project is selected to write to"));

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
    assert_eq!(d.send(None, Some("Ada"), at).unwrap_err(), "no project is selected to write to");
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
    assert!(d.take_notice().unwrap().contains("the chat name is not valid"));
    assert_eq!(d.draft(), Some(("a < b", "C1")));
    // Any other refusal: kept, with its reason.
    let req = posted(d.send(Some(&r), Some("Ada"), at).unwrap());
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Refused { code: ChatCode::Busy }), at);
    assert_eq!(d.take_notice().as_deref(), Some("not posted: the chat is busy; try again in a moment"));
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
    assert_eq!(d.take_notice().as_deref(), Some("not posted: update Ship of Tools on a, then post again"));
    assert_eq!(d.draft(), Some(("a < b", "C1")));

    // A reply for a post that is not waiting changes nothing; the reply that proves the post clears that draft alone.
    let req = posted(d.send(Some(&r), Some("Ada"), at + Duration::from_secs(40)).unwrap());
    let other = ChatPostReq { text: "other".into(), ..req.clone() };
    reply(&mut d, &other, ChatReply::Done(ChatPostRes::Posted { ts: serde_json::from_value(serde_json::json!("1700000000.000100")).unwrap(), unlinked: Vec::new(), mentioned: Vec::new() }), at);
    assert_eq!(d.draft(), Some(("a < b", "C1")));
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Posted { ts: serde_json::from_value(serde_json::json!("1700000000.000100")).unwrap(), unlinked: Vec::new(), mentioned: Vec::new() }), at);
    assert_eq!(d.draft(), None);
    assert_eq!(d.take_notice(), None);
    // A second reply to that post, arriving after the person began another draft, clears nothing.
    d.edit(Some(&r), Edit::Text("next")).unwrap();
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Posted { ts: serde_json::from_value(serde_json::json!("1700000000.000100")).unwrap(), unlinked: Vec::new(), mentioned: Vec::new() }), at);
    assert_eq!(d.draft(), Some(("next", "C1")));

    // A link that drops while the post waits leaves it unknown, and the status line says so.
    let _ = posted(d.send(Some(&r), Some("Ada"), at + Duration::from_secs(60)).unwrap());
    d.on_connection(&"a".to_string(), false, false, at);
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
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Posted { ts: serde_json::from_value(serde_json::json!("1700000000.000100")).unwrap(), unlinked: Vec::new(), mentioned: Vec::new() }), at);
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
    assert_eq!(foot(&d, 80), ["Your name for chat posts (saved in settings.toml): █"]);

    // The keys edit the name, not the draft.
    for e in [Edit::Text("Ad"), Edit::Text("x"), Edit::Backspace, Edit::Text("a"), Edit::Space, Edit::Newline] {
        d.edit_name(e).unwrap();
    }
    assert_eq!(d.asking(), Some("Ada "));
    assert_eq!(d.draft(), Some(("hello", "C1")));
    assert_eq!(foot(&d, 80), ["Your name for chat posts (saved in settings.toml): Ada █"]);
    assert_eq!(foot(&d, 54), ["Your name for chat posts (saved in settings.toml): a █"], "a narrow foot keeps the end of the name");
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
    assert!(d.send(Some(&r), None, at).is_err(), "the prompt holds the drawer: no post key posts");
    assert_eq!(d.asking(), Some("Ad"), "Enter again keeps a name half typed");

    // Saved (apply_name's part), the next Enter posts under the name.
    d.cancel_name();
    let req = posted(d.send(Some(&r), Some("Ada"), at).unwrap());
    assert_eq!((req.name.as_str(), req.text.as_str()), ("Ada", "hello"));
    assert_eq!(d.asking(), None);
}

/// A daemon whose hello says it posts as the person puts no name on the post, so the window asks for none and sends none;
/// the other side, a daemon too old to, is the test above: it asks, and posts under the name it is given.
#[test]
fn enter_posts_with_no_prompt_and_no_name_when_the_daemon_posts_as_the_person() {
    let r = row("R");
    let (mut d, at) = watching_as_person();
    d.edit(Some(&r), Edit::Text("hello")).unwrap();
    let outs = d.send(Some(&r), None, at).unwrap();
    assert_eq!(d.asking(), None, "no name is asked for");
    let req = posted(outs);
    assert_eq!((req.name.as_str(), req.text.as_str(), req.kind), ("", "hello", ChatPostKind::Person));
    // A name saved earlier is not sent either: the daemon posts under the person's own Slack account.
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Refused { code: ChatCode::Busy }), at);
    let req = posted(d.send(Some(&r), Some("Ada"), at).unwrap());
    assert_eq!(req.name, "");
}

#[test]
fn enter_with_no_name_on_a_row_that_is_not_mapped_says_why_and_opens_no_prompt() {
    let (mut d, at) = watching();
    d.edit(Some(&row("R")), Edit::Text("hello")).unwrap();
    d.on_connection(&"a".to_string(), false, false, at);
    let words = d.send(Some(&row("R")), None, at).unwrap_err();
    assert!(words.starts_with("not posted: ") && words.contains("no connection to a"), "{words}");
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
    d.on_connection(&"a".to_string(), false, false, at3);
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

/// The hint says what a post will be by the connection's hello, carried in with the link's coming up: the person's own message on
/// a daemon that posts as them, the app's on one that does not, and nothing of the daemon's age with no hello read (a link
/// that is down).
#[test]
fn the_hint_says_whether_the_post_goes_out_as_the_person_or_as_the_app_by_the_hello() {
    let (mut d, at) = watching();
    let host = "a".to_string();
    let said = |d: &ChatDrawer| compose_rows(&d.compose_text(false).expect("the foot's text"), 160, 10).join(" ");
    let chord = if cfg!(target_os = "macos") { "Cmd+Shift+k" } else { "Ctrl+Shift+k" };
    // `watching` brought the link up with the word false: an older daemon.
    assert!(said(&d).contains("is too old to post as you, so what you post appears in the channel as a message from your Slack app, with your chat name. Update it to post as yourself."), "{}", said(&d));
    // A hello that says true.
    d.on_connection(&host, true, true, at);
    assert!(said(&d).contains("as your own message, from your Slack account"), "{}", said(&d));
    assert!(!said(&d).contains("a message from your Slack app"));
    // The link goes: nothing is said of the daemon, only of the channel's chord.
    d.on_connection(&host, false, false, at);
    d.tick(Some(row("R")), at + Duration::from_millis(1));
    if let Some(text) = d.compose_text(false) {
        let said = compose_rows(&text, 160, 10).join(" ");
        assert!(!said.contains("older") && !said.contains("a message from your Slack app") && !said.contains("your own message"), "{said}");
        assert!(said.contains(&format!("{chord} changes this project's channel.")), "{said}");
    }
}

#[test]
fn an_empty_draft_shows_what_to_type_and_the_chord_that_changes_the_channel() {
    use super::compose::ComposeText;
    let chord = if cfg!(target_os = "macos") { "Cmd+Shift+k" } else { "Ctrl+Shift+k" };
    let hint = |focused| compose_rows(&ComposeText::of(None, None, focused, Some(true), "a", true), 80, 10);
    assert_eq!(hint(false)[0], "> type a message and press Enter to post it");
    assert_eq!(hint(true)[0], "> █type a message and press Enter to post it");
    let said = hint(false)[1..].join(" ");
    assert_eq!(said, format!("What you post appears in the channel as your own message, from your Slack account. {chord} changes this project's channel."));
    // The prompt row comes first at any height, and a narrow drawer wraps it.
    assert_eq!(compose_rows(&ComposeText::of(None, None, false, Some(true), "a", true), 80, 1), ["> type a message and press Enter to post it"]);
    assert!(compose_rows(&ComposeText::of(None, None, false, Some(true), "a", true), 80, 0).is_empty());
    assert!(compose_rows(&ComposeText::of(None, None, false, Some(true), "a", true), 20, 10).iter().all(|r| r.chars().count() <= 20));
    // Anything typed, or a destination to name, is the draft's own foot.
    assert_eq!(compose_rows(&ComposeText::of(Some("hi"), None, false, Some(true), "a", true), 80, 10), ["> hi"]);
    assert_eq!(compose_rows(&ComposeText::of(None, Some("to C1".into()), false, Some(true), "a", true), 80, 10), ["> ", "to C1"]);
    // After this computer's first post the prompt stays alone; an older daemon's warning is not an explanation and stays.
    assert_eq!(compose_rows(&ComposeText::of(None, None, false, Some(true), "a", false), 80, 10), ["> type a message and press Enter to post it"]);
    assert_eq!(compose_rows(&ComposeText::of(None, None, false, None, "a", false), 80, 10), ["> type a message and press Enter to post it"]);
    assert!(compose_rows(&ComposeText::of(None, None, false, Some(false), "a", false), 80, 10)[1..].join(" ").contains("is too old to post as you"));
}

/// A post proved made from `d`'s row `R`: a draft, its post and the reply that proves it.
fn post_proved(d: &mut ChatDrawer, at: Instant) {
    let r = row("R");
    d.edit(Some(&r), Edit::Text("hi")).unwrap();
    let req = posted(d.send(Some(&r), Some("Ada"), at).unwrap());
    reply(d, &req, ChatReply::Done(ChatPostRes::Posted { ts: serde_json::from_value(serde_json::json!("1700000000.000100")).unwrap(), unlinked: Vec::new(), mentioned: Vec::new() }), at);
    assert_eq!(d.draft(), None, "the proof clears the draft");
}

/// The foot's explanation shows until a post is proved made, by the proof that clears the draft; a refusal or an unknown
/// outcome leaves it. The prompt is always there, and the foot has no draft or destination to name.
#[test]
fn the_foot_explains_until_the_first_proved_post() {
    let (mut d, at) = watching_as_person();
    let r = row("R");
    let said = |d: &ChatDrawer| shown(d, false, 160, 10);
    let prompt = "> type a message and press Enter to post it";
    let chord = if cfg!(target_os = "macos") { "Cmd+Shift+k" } else { "Ctrl+Shift+k" };
    assert_eq!(said(&d), [prompt.to_string(), format!("What you post appears in the channel as your own message, from your Slack account. {chord} changes this project's channel.")]);
    // A refusal and an unknown outcome prove nothing.
    d.edit(Some(&r), Edit::Text("hi")).unwrap();
    let req = posted(d.send(Some(&r), Some("Ada"), at).unwrap());
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Refused { code: ChatCode::Busy }), at);
    let req = posted(d.send(Some(&r), Some("Ada"), at).unwrap());
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Unknown), at);
    d.edit(Some(&r), Edit::Backspace).unwrap();
    d.edit(Some(&r), Edit::Backspace).unwrap();
    assert!(!d.posted_before() && !d.take_first_post());
    assert_eq!(said(&d).len(), 2, "{:?}", said(&d));
    // A proved post: the prompt alone, from then on, and State is told once.
    post_proved(&mut d, at);
    assert_eq!(said(&d), [prompt]);
    assert!(d.posted_before() && d.take_first_post() && !d.take_first_post());
    post_proved(&mut d, at);
    assert!(!d.take_first_post(), "only the first is told");
    // A file's share is a post too, in a window that has had none.
    let (mut d, at) = watching_as_person();
    d.edit(Some(&r), Edit::Text("the fit")).unwrap();
    let req = file_posted(d.send_file(Some(&r), Previewed::File("files:figs/fit.png"), at).unwrap());
    d.on_evt(&"a".to_string(), ChatEvt::FilePosted { req, reply: ChatReply::Done(ChatPostFileRes::Posted { file: "F0FILE".into(), ts: None, unlinked: Vec::new(), mentioned: Vec::new() }) }, at);
    assert_eq!(said(&d), [prompt]);
    assert!(d.take_first_post());
    // Another project's foot is the same: the fact is the computer's.
    show(&mut d, "S", at);
    assert_eq!(shown(&d, false, 160, 10), [prompt]);
}

/// An older daemon's warning is not an explanation: it shows after a post, as before.
#[test]
fn the_too_old_warning_still_shows_after_a_post() {
    let (mut d, at) = watching();
    post_proved(&mut d, at);
    assert!(d.posted_before());
    let said = shown(&d, false, 160, 10);
    assert!(said[0].starts_with("> type a message"), "{said:?}");
    assert!(said[1..].join(" ").starts_with("Ship of Tools on a is too old to post as you,"), "{said:?}");
}

/// The first post is kept in the per-host state file, with its other keys as they were, and a window that starts later reads
/// it: its foot has the prompt alone. A file nothing can be read from or written to changes neither the post nor the draft:
/// the window then explains again at its next start.
#[test]
fn the_first_post_is_kept_and_a_new_window_reads_it() {
    use crate::ui::persist::resume::{load_from, record_chat_posted_at, save_to, GlobalState};
    let dir = Folder::new("sot-chat-posted");
    let file = dir.0.join("sot").join("state-test.toml");
    let fresh = |file: &std::path::Path| {
        let (mut d, _) = watching_as_person();
        d.set_posted_before(load_from(file).chat_posted == Some(true));
        d
    };
    let explains = |d: &ChatDrawer| shown(d, false, 160, 10).len() == 2;
    assert!(explains(&fresh(&file)), "no file: the explanation shows");

    let (mut d, at) = watching_as_person();
    save_to(&file, &GlobalState { last_mode: Some("sessions".into()), nav_scroll: Some(7), ..Default::default() }).unwrap();
    post_proved(&mut d, at);
    assert!(d.take_first_post());
    record_chat_posted_at(&file).unwrap();
    let kept = load_from(&file);
    assert_eq!((kept.chat_posted, kept.last_mode.as_deref(), kept.nav_scroll), (Some(true), Some("sessions"), Some(7)));
    assert!(!explains(&fresh(&file)), "a new window reads the fact");

    // A file that is not a file to read, or a folder that is not one to write in: the post and the draft are as ever.
    let unreadable = dir.0.join("a-folder");
    std::fs::create_dir_all(&unreadable).unwrap();
    let not_a_folder = dir.0.join("a-file");
    std::fs::write(&not_a_folder, "x").unwrap();
    for bad in [unreadable, not_a_folder.join("state.toml")] {
        let (mut d, at) = watching_as_person();
        let r = row("R");
        d.edit(Some(&r), Edit::Text("keep")).unwrap();
        let req = posted(d.send(Some(&r), Some("Ada"), at).unwrap());
        assert_eq!(load_from(&bad).chat_posted, None);
        reply(&mut d, &req, ChatReply::Done(ChatPostRes::Posted { ts: serde_json::from_value(serde_json::json!("1700000000.000100")).unwrap(), unlinked: Vec::new(), mentioned: Vec::new() }), at);
        assert_eq!(d.draft(), None);
        assert!(d.take_first_post());
        assert!(record_chat_posted_at(&bad).is_err(), "{bad:?}");
        assert!(explains(&fresh(&bad)), "{bad:?}: the next start explains again");
        // The session itself does not explain again, and the next post still posts.
        assert!(!explains(&d));
        d.edit(Some(&r), Edit::Text("more")).unwrap();
        posted(d.send(Some(&r), Some("Ada"), at).unwrap());
    }
}

#[test]
fn the_name_prompt_row_shows_the_cursor_only_while_focused_and_fits_the_height() {
    let (mut d, _) = asking();
    d.edit_name(Edit::Text("Ada")).unwrap();
    assert_eq!(compose_rows(&d.compose_text(false).unwrap(), 80, 10), ["Your name for chat posts (saved in settings.toml): Ada"], "no block unfocused");
    assert_eq!(compose_rows(&d.compose_text(true).unwrap(), 80, 10), ["Your name for chat posts (saved in settings.toml): Ada█"]);
    assert!(compose_rows(&d.compose_text(true).unwrap(), 40, 0).is_empty(), "never more rows than the height");
}

fn step(action: Option<Action>, key: Key, repeat: bool, mods: (bool, bool, bool)) -> Option<PromptStep<'static>> {
    // The keys here are characters or named keys: `Edit::Text` borrows its text from the key, so the table leaks it.
    let key: &'static Key = Box::leak(Box::new(key));
    prompt_step(action, key, repeat, mods)
}

#[test]
fn prompt_step_says_what_each_key_is_to_a_prompt() {
    let none = (false, false, false);
    assert_eq!(step(Some(Action::ChatSend), Key::Named(NamedKey::Enter), false, none), Some(PromptStep::Enter));
    assert_eq!(step(Some(Action::ReturnNav), Key::Named(NamedKey::Escape), false, none), Some(PromptStep::Escape));
    assert_eq!(step(Some(Action::Paste), ch("v"), false, (true, false, false)), Some(PromptStep::Paste));
    assert_eq!(step(Some(Action::ChatNewline), Key::Named(NamedKey::Enter), false, none), Some(PromptStep::Edit(Edit::Newline)));
    assert_eq!(step(None, ch("a"), false, none), Some(PromptStep::Edit(Edit::Text("a"))));
    assert_eq!(step(None, Key::Named(NamedKey::Space), false, none), Some(PromptStep::Edit(Edit::Space)));
    assert_eq!(step(None, Key::Named(NamedKey::Backspace), false, none), Some(PromptStep::Edit(Edit::Backspace)));
    // A held Enter, Escape or paste is taken and does nothing.
    for a in [Action::ChatSend, Action::ReturnNav, Action::Paste] {
        assert_eq!(step(Some(a), ch("x"), true, none), Some(PromptStep::Held));
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

/// `watching`, on a hub whose hello says it posts as the person.
fn watching_as_person() -> (ChatDrawer, Instant) {
    watching_on(true)
}

/// The one file post `outs` holds.
fn file_posted(outs: Vec<(String, OutgoingReq)>) -> ChatPostFileReq {
    match outs.as_slice() {
        [(_, OutgoingReq::ChatPostFile { req })] => req.clone(),
        other => panic!("one chat.post_file expected, got {other:?}"),
    }
}

/// The previewed file goes once, as the person, with the draft as its comment and into the draft's channel; it holds the
/// one post slot, and only the reply that proves the share clears the draft. With no draft it goes to the watched channel
/// with no comment.
#[test]
fn the_previewed_file_posts_once_with_the_draft_as_its_comment() {
    let r = row("R");
    let (mut d, at) = watching_as_person();
    d.edit(Some(&r), Edit::Text("the fit")).unwrap();
    let req = file_posted(d.send_file(Some(&r), Previewed::File("files:figs/fit.png"), at).unwrap());
    assert_eq!((req.node_id.as_str(), req.comment.as_deref(), req.kind), ("files:figs/fit.png", Some("the fit"), ChatPostKind::Person));
    assert_eq!((req.target.channel.as_str(), req.thread.as_ref(), req.name.as_str()), ("C1", None, ""));
    assert_eq!(d.send_file(Some(&r), Previewed::File("files:figs/fit.png"), at).unwrap_err(), "posting…", "no second post");
    assert_eq!(d.send(Some(&r), Some("tester"), at).unwrap_err(), "posting…", "nor a text post beside it");
    assert_eq!(d.edit(Some(&r), Edit::Text("x")), Err("posting…"));

    // Refused or unknown: the draft stays, with the words a text post's outcome has.
    let answer = |d: &mut ChatDrawer, req: &ChatPostFileReq, reply| d.on_evt(&"a".to_string(), ChatEvt::FilePosted { req: req.clone(), reply }, at);
    answer(&mut d, &req, ChatReply::Done(ChatPostFileRes::Refused { code: ChatCode::NotAFile }));
    assert_eq!(d.take_notice().as_deref(), Some(&*format!("not posted: {}", ChatCode::NotAFile.text())));
    assert_eq!(d.draft(), Some(("the fit", "C1")));
    let req = file_posted(d.send_file(Some(&r), Previewed::File("files:figs/fit.png"), at).unwrap());
    answer(&mut d, &req, ChatReply::Done(ChatPostFileRes::Unknown));
    assert!(d.take_notice().unwrap().starts_with("may have been posted"));
    assert_eq!(d.draft(), Some(("the fit", "C1")));

    // Shared, even with no `ts`: the draft is gone, and the status line says what went where.
    let req = file_posted(d.send_file(Some(&r), Previewed::File("files:figs/fit.png"), at).unwrap());
    answer(&mut d, &req, ChatReply::Done(ChatPostFileRes::Posted { file: "F0FILE".into(), ts: None, unlinked: Vec::new(), mentioned: Vec::new() }));
    assert_eq!(d.draft(), None);
    assert_eq!(d.take_notice().as_deref(), Some("shared fit.png to C1"));
    // A file post's `bad_request` is the file's name, never `[chat] name`, which it does not send.
    let req = file_posted(d.send_file(Some(&r), Previewed::File("files:a.png"), at).unwrap());
    answer(&mut d, &req, ChatReply::Done(ChatPostFileRes::Refused { code: ChatCode::BadRequest }));
    assert_eq!(d.take_notice().as_deref(), Some("not posted: the file's name is not one the chat can send"));

    // No draft: the watched channel, and no comment.
    let req = file_posted(d.send_file(Some(&r), Previewed::File("files:a.png"), at).unwrap());
    assert_eq!((req.target.channel.as_str(), req.comment), ("C1", None));
}

/// A post made with `@` names says on the status line whom it mentioned and which names it left as text, each with its
/// reason and, for several members, the members it could be, every entry printed as the daemon made it (a double quote is
/// structure) and joined by `, `; a text post with neither says nothing, and a file's share ends its words with them.
#[test]
fn a_post_made_names_whom_it_mentioned_and_what_was_not_linked() {
    let r = row("R");
    let (mut d, at) = watching_as_person();
    let ts: sot_protocol::chat::Ts = serde_json::from_value(serde_json::json!("1700000000.000100")).unwrap();
    let entry = |name: &str, why, candidates: &[&str]| ChatUnlinked { name: name.into(), why, candidates: candidates.iter().map(|c| c.to_string()).collect() };
    let unlinked = vec![
        entry(r#""@Zed""#, ChatUnlinkedWhy::NoMember, &[]),
        entry(r#""@Ben""#, ChatUnlinkedWhy::SeveralMembers, &[r#""Ben Smith" @ben.smith"#, r#""Ben Sample" @ben.sample"#]),
        entry(r#""@Cy""#, ChatUnlinkedWhy::SeveralMembers, &[r#""Cy One" @c1"#, r#""Cy Two" @c2"#, r#""Cy Three" @c3"#, r#""Cy Four" @c4"#, ""]),
    ];
    // The reviewer's forged pair, as the daemon now sends it: one entry, whatever the names hold.
    let mentioned = vec![r#""Ada please" ("x), Ada (Ada Lovelace") @mal"#.to_string(), r#""Ben Sample" @ben.sample"#.to_string()];
    d.edit(Some(&r), Edit::Text("hi @Ada please, @Ben Sample, @Zed, @Ben and @Cy")).unwrap();
    let req = posted(d.send(Some(&r), Some("Ada"), at).unwrap());
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Posted { ts: ts.clone(), unlinked, mentioned }), at);
    assert_eq!(d.draft(), None, "a post made clears its draft");
    assert_eq!(
        d.take_notice().as_deref(),
        Some(concat!(
            r#"posted; mentioned 2: "Ada please" ("x), Ada (Ada Lovelace") @mal, "Ben Sample" @ben.sample; not linked: "@Zed" (no member), "#,
            r#""@Ben" (several members, 2: "Ben Smith" @ben.smith, "Ben Sample" @ben.sample), "#,
            r#""@Cy" (several members, 4 and more: "Cy One" @c1, "Cy Two" @c2, "Cy Three" @c3, "Cy Four" @c4, and more)"#
        ))
    );
    d.edit(Some(&r), Edit::Text("hi")).unwrap();
    let req = posted(d.send(Some(&r), Some("Ada"), at).unwrap());
    reply(&mut d, &req, ChatReply::Done(ChatPostRes::Posted { ts: ts.clone(), unlinked: Vec::new(), mentioned: vec![r#""Ada Example" @ada.example"#.into()] }), at);
    assert_eq!(d.take_notice().as_deref(), Some(r#"posted; mentioned 1: "Ada Example" @ada.example"#), "a mention made is always told");
    d.edit(Some(&r), Edit::Text("the fit @Zed")).unwrap();
    let req = file_posted(d.send_file(Some(&r), Previewed::File("files:figs/fit.png"), at).unwrap());
    let unlinked = vec![entry(r#""@Zed""#, ChatUnlinkedWhy::MembersUnread, &[]), entry("", ChatUnlinkedWhy::More, &[])];
    let mentioned = vec![r#""Ada Example" @ada.example"#.to_string(), String::new()];
    d.on_evt(&"a".to_string(), ChatEvt::FilePosted { req, reply: ChatReply::Done(ChatPostFileRes::Posted { file: "F0FILE".into(), ts: None, unlinked, mentioned }) }, at);
    assert_eq!(
        d.take_notice().as_deref(),
        Some(r#"shared fit.png to C1; mentioned 1 and more: "Ada Example" @ada.example, and more; not linked: "@Zed" (the member list could not be read), and more"#)
    );
}

/// A picture opened from the chat in the preview is no file of the project: nothing is posted, and the status line says what
/// the preview shows, why it is not posted and what to do, naming the post key by its binding's label.
#[test]
fn a_picture_opened_from_the_chat_posts_nothing_and_says_what_to_do() {
    let r = row("R");
    let (mut d, at) = watching_as_person();
    let key = crate::ui::input::keybindings::KeyBindings::defaults().first_label(Action::ChatPostFile);
    // The label is the platform's: `Ctrl+u`, and `⌘u` on a Mac. Either ends in the lowercase letter.
    assert!(key.ends_with('u'), "the post key is written {key}");
    assert_eq!(
        d.send_file(Some(&r), Previewed::ChatPicture { key: &key }, at).unwrap_err(),
        format!("not posted: the preview shows a picture opened from the chat, which is not a file of this project; show a file in the preview, then press {key}")
    );
}

/// A preview with no node (nothing shown) posts nothing, and neither does a hub whose hello does not say it posts as the
/// person, nor a row with no channel to write to.
#[test]
fn a_preview_with_no_node_posts_nothing() {
    let r = row("R");
    let (mut d, at) = watching_as_person();
    d.edit(Some(&r), Edit::Text("the fit")).unwrap();
    assert_eq!(d.send_file(Some(&r), Previewed::Nothing, at).unwrap_err(), "not posted: the preview shows no file");
    assert_eq!(d.send_file(None, Previewed::File("files:a.png"), at).unwrap_err(), "no project is selected to post for");
    assert_eq!(d.send_file(Some(&row("S")), Previewed::File("files:a.png"), at).unwrap_err(), "not posted: no channel to write to");
    assert_eq!(d.draft(), Some(("the fit", "C1")));
    let (mut old, at) = watching();
    assert_eq!(old.send_file(Some(&r), Previewed::File("files:a.png"), at).unwrap_err(), "not posted: update Ship of Tools on a, then post again");
    // Nothing waits: a text post still goes.
    old.edit(Some(&r), Edit::Text("hi")).unwrap();
    assert!(matches!(old.send(Some(&r), Some("tester"), at).unwrap().as_slice(), [(_, OutgoingReq::ChatPost { .. })]));
}

/// Neither post key posts while a screen or the name prompt holds the drawer: the picker, the prompt; both post in the plain
/// drawer.
#[test]
fn no_post_key_posts_behind_the_picker_or_the_name_prompt() {
    let r = row("R");
    #[derive(Debug, Clone, Copy)]
    enum Held {
        Plain,
        Picker,
        Prompt,
    }
    for held in [Held::Plain, Held::Picker, Held::Prompt] {
        for file in [false, true] {
            let (mut d, at) = watching_as_person();
            d.edit(Some(&r), Edit::Text("hi")).unwrap();
            match held {
                Held::Plain => {}
                Held::Picker => {
                    d.open_picker(Some(&r)).unwrap();
                }
                Held::Prompt => d.ask_name(),
            }
            let sent = if file { d.send_file(Some(&r), Previewed::File("files:a.png"), at) } else { d.send(Some(&r), Some("tester"), at) };
            match (held, sent) {
                (Held::Plain, Ok(outs)) => assert_eq!(outs.len(), 1, "{held:?} {file}"),
                (Held::Plain, Err(e)) => panic!("{held:?} {file}: {e}"),
                (_, sent) => assert_eq!(sent.unwrap_err(), "not posted: close the screen open in the chat first", "{held:?} {file}"),
            }
        }
    }
}

/// A held key repeats only a draft edit: every chat chord that posts, opens a screen, copies or pastes is `Held` when it
/// repeats, and a chord that is not the tenant's is none.
#[test]
fn a_held_chat_chord_never_posts_again() {
    use super::{chat_chord, ChatChord};
    let chords = [
        (Action::ChatSend, ChatChord::Send),
        (Action::ChatPostFile, ChatChord::PostFile),
        (Action::ChatSetup, ChatChord::Setup),
        (Action::ChatOpenImage, ChatChord::OpenImage),
        (Action::ChatCopySelection, ChatChord::Copy),
        (Action::Paste, ChatChord::Paste),
    ];
    for (action, chord) in chords {
        assert_eq!(chat_chord(Some(action), false), Some(chord), "{action:?}");
        assert_eq!(chat_chord(Some(action), true), Some(ChatChord::Held), "{action:?} held");
    }
    for action in [None, Some(Action::ChatNewline), Some(Action::ChatLatest)] {
        assert_eq!(chat_chord(action, false), None, "{action:?}");
        assert_eq!(chat_chord(action, true), None, "{action:?} held");
    }
}

/// A rate limit says Slack asked for a pause and the seconds left, with `about` while the wait is a guess.
#[test]
fn a_wait_says_the_pause_and_the_seconds() {
    assert_eq!(super::drawer::wait_words(5, false), "not posted: Slack asked for a pause; wait 5 s");
    assert_eq!(super::drawer::wait_words(5, true), "not posted: Slack asked for a pause; wait about 5 s");
}
