use std::time::{Duration, Instant};

use cosmic_text::FontSystem;
use sot_protocol::ops::{ChatChannel, ChatChannelsRes, ChatCode, ChatPairRes};

use super::compose::Edit;
use super::drawer::ChatDrawer;
use super::owner::Out;
use super::drawer::never_written;
use super::picker::{link_channel, Body};
use super::row_channel_tests::answer;
use super::thread_tests::{body, channel, joined, shown, top};
use super::{prompt_step, PromptStep};
use crate::net::dial::HostKey;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use crate::ui::input::keybindings::Action;
use winit::keyboard::{Key, NamedKey};

fn host() -> HostKey {
    "a".to_string()
}

fn id(s: &str) -> ChatChannel {
    ChatChannel::try_from(s.to_string()).unwrap()
}

/// A drawer whose shown row `ROW1` answered `unmapped`, the instant of the answer, and what the answer asked for.
fn unmapped() -> (ChatDrawer, Instant, Vec<Out>) {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.set_focused(true);
    d.on_connection(&host(), true, false, t0);
    d.tick(shown("ROW1"), t0);
    d.tick(shown("ROW1"), t0 + Duration::from_millis(200));
    let at = t0 + Duration::from_millis(210);
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), at);
    // The frame after the answer opens the picker by itself, and asks for the list.
    let outs = d.tick(shown("ROW1"), at + Duration::from_millis(1)).0;
    (d, at, outs)
}

fn channels_reply(more: bool) -> ChatEvt {
    let res: ChatChannelsRes = serde_json::from_value(serde_json::json!({ "channels": [
        { "id": "C4", "name": "random", "private": false, "member": true },
        { "id": "G3", "name": "lab-papers", "private": true, "member": true },
        { "id": "C2", "name": "lab-microscopes", "private": false, "member": false },
        { "id": "C1", "name": "lab-general", "private": false, "member": true },
    ], "more": more })).unwrap();
    ChatEvt::Channels { reply: ChatReply::Done(res) }
}

fn pairs(outs: &[Out]) -> Vec<(String, Option<String>)> {
    outs.iter().filter_map(|(_, r)| match r {
        OutgoingReq::ChatPair { row, channel } => Some((row.clone(), channel.as_ref().map(|c| c.as_str().to_string()))),
        _ => None,
    }).collect()
}

fn lists(outs: &[Out]) -> usize {
    outs.iter().filter(|(_, r)| matches!(r, OutgoingReq::ChatChannels)).count()
}

fn paired(row: &str, channel: Option<&str>, reply: ChatReply<ChatPairRes>) -> ChatEvt {
    ChatEvt::Paired { row: row.to_string(), channel: channel.map(id), reply }
}

fn shown_lines(d: &ChatDrawer) -> Vec<String> {
    d.body().expect("a body").lines(80, 24)
}

fn asks_resolve(d: &mut ChatDrawer, at: Instant) -> bool {
    d.tick(shown("ROW1"), at).0.iter().any(|(_, r)| matches!(r, OutgoingReq::ChatResolve { .. }))
}

#[test]
fn the_picker_lists_channels_by_name_with_their_marks_and_says_how_a_private_one_appears() {
    let (mut d, at, outs) = unmapped();
    // An unmapped row opens it by itself and asks for the list once; the title is the picker's.
    assert_eq!((lists(&outs), d.picking()), (1, true));
    assert_eq!(d.title(), " chat · choose this project's channel ");
    assert_eq!(shown_lines(&d), ["reading Slack's channels…"]);
    d.on_evt(&host(), channels_reply(false), at);
    let lines = shown_lines(&d);
    assert!(lines[0].starts_with("> #lab-general"), "{lines:?}");
    assert!(lines[1].starts_with("  #lab-microscopes") && lines[1].ends_with("your app joins it when you pick it"), "{lines:?}");
    assert!(lines[2].starts_with("  #lab-papers") && lines[2].ends_with("private, app invited"), "{lines:?}");
    assert!(lines[3].starts_with("  #random") && lines[3].trim_end() == "  #random", "a public channel the app is in has no mark: {lines:?}");
    let guide = lines[4..].join(" ");
    assert!(guide.contains("Esc to close") && guide.contains("/invite @") && !guide.contains("paste a channel's link"), "{guide}");
    // A list cut at the bound says to paste a link.
    d.on_evt(&host(), channels_reply(true), at);
    assert!(shown_lines(&d).join(" ").contains("paste a channel's link"));
    // A short drawer keeps the choice in view and cuts the guide, never the entries' window below three rows.
    let rows = d.body().unwrap().lines(80, 5);
    assert_eq!(rows.len(), 5);
    assert_eq!(rows.iter().filter(|l| l.starts_with("> ")).count(), 1);
    // A read that fails says why and offers only to connect Slack again; the picker does not hide.
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), ChatEvt::Channels { reply: ChatReply::Refused(ChatCode::Unavailable) }, at);
    assert_eq!(shown_lines(&d)[..2], ["could not list the channels: the chat is not available here", "> Connect Slack again  new tokens for the hub"]);
    d.on_evt(&host(), ChatEvt::Channels { reply: ChatReply::Unsupported }, at);
    assert_eq!(shown_lines(&d)[0], "the hub's daemon must be updated to list channels");
    assert!(d.picker_enter().is_empty() && d.in_setup(), "Connect Slack again sends nothing and starts the Connect step");
}

#[test]
fn typing_filters_and_enter_pairs_once() {
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(false), at);
    // The `#` the entries are drawn with may be typed.
    d.picker_edit(Edit::Text("#"));
    assert!(shown_lines(&d)[0].starts_with("> #lab-general") && shown_lines(&d).len() > 4);
    d.picker_edit(Edit::Backspace);
    d.picker_edit(Edit::Text("LAB"));
    assert_eq!(d.title(), " chat · choose this project's channel  filter: LAB ");
    let lines = shown_lines(&d);
    assert!(lines[0].starts_with("> #lab-general") && lines[2].starts_with("  #lab-papers") && !lines.iter().any(|l| l.contains("#random")), "{lines:?}");
    d.picker_step(true);
    d.picker_step(true);
    d.picker_step(true);
    assert!(shown_lines(&d)[2].starts_with("> #lab-papers"), "the choice stops at the last entry");
    d.picker_step(false);
    // Enter sends one pair for the chosen channel; a second Enter before the reply sends nothing.
    let sent = d.picker_enter();
    assert_eq!(pairs(&sent), [("ROW1".to_string(), Some("C2".to_string()))]);
    assert!(sent.iter().all(|(h, _)| *h == host()));
    assert!(d.picker_enter().is_empty());
    assert!(shown_lines(&d)[0].contains("setting this project's channel"));
    // The reply closes the picker, and the row is resolved again at once; nothing is said.
    d.on_evt(&host(), paired("ROW1", Some("C2"), ChatReply::Done(ChatPairRes {})), at);
    assert!(!d.picking() && d.take_notice().is_none());
    assert!(asks_resolve(&mut d, at + Duration::from_millis(300)));
    // Backspace widens the list again, and a filter that matches nothing says so.
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(false), at);
    d.picker_edit(Edit::Text("zzz"));
    assert_eq!(shown_lines(&d)[0], "no channel matches");
    assert!(d.picker_enter().is_empty());
    (0..3).for_each(|_| d.picker_edit(Edit::Backspace));
    assert_eq!(d.title(), " chat · choose this project's channel ");
    assert_eq!(shown_lines(&d).len() > 4, true);
}

#[test]
fn a_pasted_channel_link_pairs_its_id_and_any_other_paste_goes_nowhere() {
    for (text, want) in [
        ("https://app.slack.com/client/T0123ABCD/C0456EFGH", Some("C0456EFGH")),
        ("  https://lab.slack.com/archives/G0456EFGH/?thread_ts=1.2#x\n", Some("G0456EFGH")),
        ("C0456EFGH", Some("C0456EFGH")),
        ("https://app.slack.com/client/T0123ABCD", None),
        ("hello there", None),
        ("C04 56", None),
        ("", None),
        // Words and secrets that merely start with C or G are not channels, and are sent nowhere.
        ("General", None),
        ("Contact", None),
        ("Customers", None),
        ("Cats", None),
        ("C", None),
        ("GOCSPX-abcdefghijklmnop", None),
        ("Gx_secret-token-1234567890", None),
        ("https://github.com/foo/Capital", None),
        ("C0456EF", None),
        ("c0456efgh", None),
    ] {
        assert_eq!(link_channel(text).as_ref().map(ChatChannel::as_str), want, "{text:?}");
    }
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(true), at);
    assert_eq!(d.picker_paste("hello there").unwrap_err(), "not pasted: that is not a Slack channel link");
    assert_eq!(d.title(), " chat · choose this project's channel ");
    let sent = d.picker_paste("https://app.slack.com/client/T1/C0456EFGH").unwrap();
    assert_eq!(pairs(&sent), [("ROW1".to_string(), Some("C0456EFGH".to_string()))]);
    assert_eq!(d.picker_paste("C9999999999").unwrap_err(), "setting this project's channel…", "one pair at a time");
}

#[test]
fn the_setup_chord_offers_no_channel_first_on_a_mapped_row() {
    let (mut d, at) = channel(&[]);
    let outs = d.open_picker(shown("ROW1").as_ref()).unwrap();
    assert_eq!(lists(&outs), 1);
    d.on_evt(&host(), channels_reply(false), at);
    let lines = shown_lines(&d);
    assert!(lines[0].starts_with("  No channel for this project") && lines[1].starts_with("> #lab-general"), "{lines:?}");
    // A narrowed list is channels alone, so typing never leaves Enter on the unpairing.
    d.picker_edit(Edit::Text("lab"));
    assert!(!shown_lines(&d).iter().any(|l| l.contains("No channel for")));
    (0..3).for_each(|_| d.picker_edit(Edit::Backspace));
    assert!(shown_lines(&d)[1].starts_with("> #lab-general"), "the choice returns to the first channel");
    // `No channel for this project` unpairs, and the row then stays unpicked until a person asks.
    d.picker_step(false);
    let sent = d.picker_enter();
    assert_eq!(pairs(&sent), [("ROW1".to_string(), None)]);
    d.on_evt(&host(), paired("ROW1", None, ChatReply::Done(ChatPairRes {})), at);
    assert!(!d.picking());
    d.set_focused(true);
    let answered = Duration::from_secs(1);
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), at + answered);
    let outs = d.tick(shown("ROW1"), at + answered * 2).0;
    assert_eq!((lists(&outs), d.picking()), (0, false), "an unpaired row does not reopen the picker");
    assert_eq!(d.title(), " chat · no channel ");
    assert_eq!(shown_lines(&d), ["this project has no channel: press Enter to pick one"]);
    // Enter over it, with nothing typed, asks for the channels.
    let outs = d.send(shown("ROW1").as_ref(), Some("tester"), at + answered * 3).unwrap();
    assert_eq!((lists(&outs), d.picking()), (1, true));
    // An unmapped row's chord opens the same list, and any other stand says its words.
    let (mut d, at, _) = unmapped();
    d.dismiss_picker();
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    d.on_evt(&host(), channels_reply(false), at);
    assert!(shown_lines(&d)[0].starts_with("> #lab-general") && !shown_lines(&d).iter().any(|l| l.contains("No channel for")));
    let mut off = ChatDrawer::default();
    off.tick(shown("ROW1"), at);
    assert_eq!(off.open_picker(shown("ROW1").as_ref()).unwrap_err(), "not connected");
    off.on_connection(&host(), true, false, at);
    off.tick(shown("ROW1"), at + Duration::from_millis(200));
    off.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "invalid" })), at + Duration::from_millis(210));
    assert!(off.open_picker(shown("ROW1").as_ref()).unwrap_err().starts_with("the hub has no usable chat configuration"));
    assert_eq!(off.open_picker(None).unwrap_err(), "no row to choose a channel for");
}

#[test]
fn a_pair_reply_resolves_the_row_again_and_not_in_channel_says_to_invite() {
    for (pick, words) in [("G3", ["private channel", "/invite @"]), ("C2", ["could not join", "/invite @"])] {
        let (mut d, at, _) = unmapped();
        d.on_evt(&host(), channels_reply(false), at);
        d.picker_edit(Edit::Text(if pick == "G3" { "papers" } else { "micro" }));
        assert_eq!(pairs(&d.picker_enter())[0].1.as_deref(), Some(pick));
        d.on_evt(&host(), paired("ROW1", Some(pick), ChatReply::Refused(ChatCode::NotInChannel)), at);
        let said = d.take_notice().unwrap();
        assert!(words.iter().all(|w| said.contains(w)), "{said}");
        // The list stays, and the row is resolved again; the next Enter may pair again.
        assert!(d.picking() && asks_resolve(&mut d, at + Duration::from_millis(300)));
        assert_eq!(pairs(&d.picker_enter()).len(), 1);
    }
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(false), at);
    for (reply, said) in [
        (ChatReply::Refused(ChatCode::ChannelTaken), "not set: another project already uses that channel"),
        (ChatReply::Refused(ChatCode::NotSaved), "not set: the chat settings could not be saved"),
        (ChatReply::Unsupported, "not set: the hub's daemon must be updated to set a channel"),
    ] {
        d.picker_enter();
        d.on_evt(&host(), paired("ROW1", Some("C1"), reply), at);
        assert_eq!(d.take_notice().as_deref(), Some(said));
        assert!(d.picking());
    }
    // A pair that may have been made closes the picker and says to look at the title.
    d.picker_enter();
    d.on_evt(&host(), ChatEvt::PairUnknown { row: "ROW1".into(), channel: Some(id("C1")) }, at);
    assert!(!d.picking());
    assert!(d.take_notice().unwrap().starts_with("the channel may have been set"));
}

#[test]
fn the_picker_belongs_to_its_row_and_link_and_opens_by_itself_once_while_the_drawer_has_the_keys() {
    // A drawer that does not have the keys asks Slack for nothing: the person scanning rows spends no list read.
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.on_connection(&host(), true, false, t0);
    d.tick(shown("ROW1"), t0);
    d.tick(shown("ROW1"), t0 + Duration::from_millis(200));
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), t0 + Duration::from_millis(210));
    let at = t0 + Duration::from_secs(1);
    assert_eq!((lists(&d.tick(shown("ROW1"), at).0), d.picking()), (0, false));
    assert_eq!(d.title(), " chat · no channel ");
    // With the keys it opens, once; Escape closes it, and neither the same row's next answer nor a link change opens it again.
    d.set_focused(true);
    assert_eq!((lists(&d.tick(shown("ROW1"), at + Duration::from_millis(16)).0), d.picking()), (1, true));
    d.dismiss_picker();
    assert!(!d.picking());
    assert_eq!(lists(&d.tick(shown("ROW1"), at + Duration::from_millis(32)).0), 0);
    d.on_connection(&host(), false, false, at + Duration::from_secs(1));
    d.on_connection(&host(), true, false, at + Duration::from_secs(2));
    d.tick(shown("ROW1"), at + Duration::from_secs(3));
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), at + Duration::from_secs(4));
    assert_eq!((lists(&d.tick(shown("ROW1"), at + Duration::from_secs(5)).0), d.picking()), (0, false));
    // A row change forgets the Escape: back on the row, it opens again.
    d.tick(shown("ROW2"), at + Duration::from_secs(6));
    d.tick(shown("ROW1"), at + Duration::from_secs(7));
    d.tick(shown("ROW1"), at + Duration::from_secs(8));
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), at + Duration::from_secs(9));
    assert_eq!((lists(&d.tick(shown("ROW1"), at + Duration::from_secs(10)).0), d.picking()), (1, true));
    // A link change closes it; so does a row change.
    d.on_connection(&host(), false, false, at + Duration::from_secs(11));
    assert!(!d.picking());
    let (mut d, at, _) = unmapped();
    d.tick(shown("ROW2"), at + Duration::from_secs(1));
    assert!(!d.picking());
    // A reply from another host does not fill it.
    let (mut d, at, _) = unmapped();
    d.on_evt(&"b".to_string(), channels_reply(false), at);
    assert_eq!(shown_lines(&d), ["reading Slack's channels…"]);
}

#[test]
fn one_pair_is_in_flight_across_escape_and_a_reopened_picker_is_never_closed_by_an_older_reply() {
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(false), at);
    assert_eq!(pairs(&d.picker_enter()).len(), 1);
    // Escape, and Enter over the empty draft: a new picker, with the first pair unanswered.
    d.dismiss_picker();
    d.send(shown("ROW1").as_ref(), Some("tester"), at).unwrap();
    d.on_evt(&host(), channels_reply(false), at);
    assert!(shown_lines(&d)[0].contains("setting this project's channel"));
    assert!(d.picker_enter().is_empty(), "nothing is sent while a pair is unanswered");
    assert_eq!(d.picker_paste("C0456EFGH").unwrap_err(), "setting this project's channel…");
    // The first pair's reply does not close the new picker, and frees it to pick.
    d.on_evt(&host(), paired("ROW1", Some("C1"), ChatReply::Done(ChatPairRes {})), at);
    assert!(d.picking());
    assert_eq!(pairs(&d.picker_enter()).len(), 1);
    // A row change while a pair is out leaves it out: the next row's picker waits for the reply too.
    d.tick(shown("ROW2"), at + Duration::from_secs(1));
    assert!(!d.picking());
    d.on_evt(&host(), paired("ROW1", Some("C1"), ChatReply::Done(ChatPairRes {})), at + Duration::from_secs(2));
    assert!(d.take_notice().is_none());
}

#[test]
fn a_late_refusal_for_another_row_says_nothing() {
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(false), at);
    d.picker_enter();
    d.tick(shown("ROW2"), at + Duration::from_secs(1));
    d.on_evt(&host(), paired("ROW1", Some("C1"), ChatReply::Refused(ChatCode::NotInChannel)), at + Duration::from_secs(2));
    assert!(d.take_notice().is_none(), "the shown row is ROW2");
    // The same refusal for the shown row is said.
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(false), at);
    d.picker_enter();
    d.on_evt(&host(), paired("ROW1", Some("C1"), ChatReply::Refused(ChatCode::NotInChannel)), at);
    assert!(d.take_notice().is_some());
}

#[test]
fn a_request_that_could_not_be_queued_is_told_it_was_refused_unavailable() {
    let post = serde_json::from_value(serde_json::json!({ "target": { "row": "ROW1", "workspace": "T1", "channel": "C1" },
        "kind": "person", "name": "tester", "text": "hi" })).unwrap();
    assert!(matches!(never_written(&OutgoingReq::ChatPost { req: post }),
        Some(ChatEvt::Posted { reply: ChatReply::Done(sot_protocol::ops::ChatPostRes::Refused { code: ChatCode::Unavailable }), .. })));
    assert!(matches!(never_written(&OutgoingReq::ChatPair { row: "ROW1".into(), channel: Some(id("C1")) }),
        Some(ChatEvt::Paired { reply: ChatReply::Refused(ChatCode::Unavailable), .. })));
    assert!(matches!(never_written(&OutgoingReq::ChatChannels), Some(ChatEvt::Channels { reply: ChatReply::Refused(ChatCode::Unavailable) })));
    assert!(never_written(&OutgoingReq::ChatResolve { row: "ROW1".into() }).is_none());
    // Told so, the picker is not left waiting: its list says why, and a pair that was never queued is refused and frees the next.
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), never_written(&OutgoingReq::ChatChannels).unwrap(), at);
    assert_eq!(shown_lines(&d)[0], "could not list the channels: the chat is not available here");
    d.on_evt(&host(), channels_reply(false), at);
    let sent = d.picker_enter();
    assert!(d.picker_enter().is_empty());
    let (_, req) = &sent[0];
    d.on_evt(&host(), never_written(req).unwrap(), at);
    assert!(d.picking() && d.take_notice().unwrap().starts_with("not set: the chat is not available here"));
    assert_eq!(pairs(&d.picker_enter()).len(), 1);
}

#[test]
fn an_empty_channel_says_no_messages_in_the_body_and_the_picker_and_a_thread_say_otherwise() {
    let (mut d, _) = channel(&[]);
    assert_eq!(d.body().unwrap().lines(40, 5), ["no messages"]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    assert_eq!(d.body().unwrap().lines(40, 5), ["reading Slack's channels…"]);
    let (d, _) = channel(&[top(1)]);
    assert!(d.body().is_none(), "a channel with a record says nothing");
}

#[test]
fn a_mapped_channels_records_return_when_the_picker_closes() {
    let (mut d, _) = channel(&(1..4).map(top).collect::<Vec<_>>());
    let mut fonts = FontSystem::new();
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(joined(&d).contains("message 3"));
    // The picker closes an open thread and a name prompt: a thread's Left key would give the channel's view back under it, and
    // the prompt would come back after it.
    d.mark(true, Instant::now(), Duration::ZERO);
    d.layout(&mut fonts, body(), 1.0, body());
    d.open_thread();
    assert_eq!(d.thread_len(), 1);
    d.ask_name();
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    assert_eq!((d.thread_len(), d.asking()), (0, None));
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(d.placed_text().is_empty(), "the picker's view holds no record");
    assert!(d.compose_text(true).is_none());
    d.dismiss_picker();
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(joined(&d).contains("message 3"));
    assert!(d.compose_text(true).is_some());
}

#[test]
fn a_prompt_takes_the_keys_that_edit_and_close_it_and_leaves_the_rest_to_the_drawer() {
    let typed = Key::Character("l".into());
    let named = |k| Key::Named(k);
    fn step<'a>(action: Option<Action>, key: &'a Key, repeat: bool) -> Option<PromptStep<'a>> {
        prompt_step(action, key, repeat, (false, false, false))
    }
    assert_eq!(step(Some(Action::ChatSend), &named(NamedKey::Enter), false), Some(PromptStep::Enter));
    assert_eq!(step(Some(Action::ReturnNav), &named(NamedKey::Escape), false), Some(PromptStep::Escape));
    assert_eq!(step(Some(Action::Paste), &typed, false), Some(PromptStep::Paste));
    assert_eq!(step(Some(Action::ChatSend), &named(NamedKey::Enter), true), Some(PromptStep::Held));
    assert_eq!(step(Some(Action::Paste), &typed, true), Some(PromptStep::Held));
    assert_eq!(step(None, &typed, false), Some(PromptStep::Edit(Edit::Text("l"))));
    assert_eq!(step(None, &named(NamedKey::Backspace), false), Some(PromptStep::Edit(Edit::Backspace)));
    // The choosing keys, the chord and the reading keys are not a prompt's: the picker takes the first, `chat_key` the chord.
    for a in [Action::ChatMarkOlder, Action::ChatMarkNewer, Action::ChatSetup, Action::ScrollPageUp] {
        assert_eq!(step(Some(a), &named(NamedKey::ArrowUp), false), None, "{a:?}");
    }
}

#[test]
fn the_body_draws_the_choice_in_view_and_the_guide_below_it() {
    let body = Body { head: String::new(), entries: (0..30).map(|i| (format!("#c{i}"), "")).collect(), chosen: 20, guide: "a guide that wraps over two rows".into(), ..Body::default() };
    let rows = body.lines(20, 10);
    assert!(rows.len() <= 10);
    assert!(rows.iter().any(|t| t.starts_with("> #c20")), "{rows:?}");
    assert!(rows.last().unwrap().ends_with("rows"), "{rows:?}");
}
