use std::time::{Duration, Instant};

use cosmic_text::FontSystem;
use sot_protocol::ops::{ChatChannel, ChatChannelsRes, ChatCode, ChatPairRes};

use super::compose::Edit;
use super::drawer::ChatDrawer;
use super::owner::Out;
use super::picker::{link_channel, Body, Tone};
use super::row_channel_tests::answer;
use super::thread_tests::{body, channel, joined, shown, top};
use super::{pick_step, PickStep};
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
    d.on_connection(&host(), true, t0);
    d.tick(shown("ROW1"), t0);
    d.tick(shown("ROW1"), t0 + Duration::from_millis(200));
    let at = t0 + Duration::from_millis(210);
    let outs = d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), at);
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
    d.body().expect("a body").lines(80, 24).into_iter().map(|(t, _)| t).collect()
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
    assert!(rows.iter().filter(|(_, t)| *t == Tone::Chosen).count() == 1);
    // A read that fails says why and offers nothing to pick; the picker does not hide.
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), ChatEvt::Channels { reply: ChatReply::Refused(ChatCode::Unavailable) }, at);
    assert_eq!(shown_lines(&d)[0], "could not list the channels: the chat is not available here");
    assert!(d.picker_enter().is_empty());
    d.on_evt(&host(), ChatEvt::Channels { reply: ChatReply::Unsupported }, at);
    assert_eq!(shown_lines(&d)[0], "the hub's daemon must be updated to list channels");
}

#[test]
fn typing_filters_and_enter_pairs_once() {
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(false), at);
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
fn the_setup_chord_offers_no_channel_and_reconnect_on_a_mapped_row() {
    let (mut d, at) = channel(&[]);
    let outs = d.open_picker(shown("ROW1").as_ref(), true).unwrap();
    assert_eq!(lists(&outs), 1);
    d.on_evt(&host(), channels_reply(false), at);
    let lines = shown_lines(&d);
    assert!(lines[0].starts_with("  No channel for this project") && lines[1].starts_with("> #lab-general"), "{lines:?}");
    assert!(lines[5].starts_with("  Connect Slack again"), "{lines:?}");
    // A narrowed list is channels alone.
    d.picker_edit(Edit::Text("lab"));
    assert!(!shown_lines(&d).iter().any(|l| l.contains("No channel for") || l.contains("Connect Slack")));
    (0..3).for_each(|_| d.picker_edit(Edit::Backspace));
    // `Connect Slack again` leaves the picker and calls the Connect step (a stub: the status line says so); nothing is sent.
    (0..5).for_each(|_| d.picker_step(true));
    assert!(d.picker_enter().is_empty());
    assert!(!d.picking());
    assert!(d.take_notice().unwrap().starts_with("Connect Slack again"));
    // `No channel for this project` unpairs, and the row then stays unpicked until a person asks.
    d.open_picker(shown("ROW1").as_ref(), true).unwrap();
    d.on_evt(&host(), channels_reply(false), at);
    d.picker_step(false);
    let sent = d.picker_enter();
    assert_eq!(pairs(&sent), [("ROW1".to_string(), None)]);
    d.on_evt(&host(), paired("ROW1", None, ChatReply::Done(ChatPairRes {})), at);
    assert!(!d.picking());
    let answered = d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), at + Duration::from_secs(1));
    assert_eq!((lists(&answered), d.picking()), (0, false), "an unpaired row does not reopen the picker");
    assert_eq!(d.title(), " chat · no channel ");
    assert_eq!(shown_lines(&d), ["this project has no channel: press Enter to pick one"]);
    // Enter over it, with nothing typed, asks for the channels.
    let outs = d.send(shown("ROW1").as_ref(), Some("tester"), at + Duration::from_secs(2)).unwrap();
    assert_eq!((lists(&outs), d.picking()), (1, true));
    // An unmapped row's chord has no `No channel` entry, and any other stand says its words.
    let (mut d, at, _) = unmapped();
    d.dismiss_picker();
    d.open_picker(shown("ROW1").as_ref(), true).unwrap();
    d.on_evt(&host(), channels_reply(false), at);
    assert!(shown_lines(&d)[0].starts_with("> #lab-general") && shown_lines(&d)[4].starts_with("  Connect Slack again"));
    let mut off = ChatDrawer::default();
    off.tick(shown("ROW1"), at);
    assert_eq!(off.open_picker(shown("ROW1").as_ref(), true).unwrap_err(), "not connected");
    off.on_connection(&host(), true, at);
    off.tick(shown("ROW1"), at + Duration::from_millis(200));
    off.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "invalid" })), at + Duration::from_millis(210));
    assert!(off.open_picker(shown("ROW1").as_ref(), true).unwrap_err().starts_with("the hub has no usable chat configuration"));
    assert_eq!(off.open_picker(None, true).unwrap_err(), "no row to choose a channel for");
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
fn the_picker_belongs_to_its_row_and_link_and_opens_by_itself_once() {
    // Escape closes it, and the same row's answer does not open it again; a link change does.
    let (mut d, at, _) = unmapped();
    d.dismiss_picker();
    assert!(!d.picking());
    let again = d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), at + Duration::from_secs(1));
    assert_eq!(lists(&again), 0);
    d.on_connection(&host(), false, at + Duration::from_secs(2));
    d.on_connection(&host(), true, at + Duration::from_secs(3));
    d.tick(shown("ROW1"), at + Duration::from_secs(4));
    let outs = d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), at + Duration::from_secs(5));
    assert_eq!((lists(&outs), d.picking()), (1, true));
    // A link change closes it; so does a row change, which restores nothing of the old row.
    d.on_connection(&host(), false, at + Duration::from_secs(6));
    assert!(!d.picking());
    let (mut d, at, _) = unmapped();
    d.tick(shown("ROW2"), at + Duration::from_secs(1));
    assert!(!d.picking());
    // A list read for no picker is nothing, and a reply from another host does not fill it.
    let (mut d, at, _) = unmapped();
    d.on_evt(&"b".to_string(), channels_reply(false), at);
    assert_eq!(shown_lines(&d), ["reading Slack's channels…"]);
}

#[test]
fn a_mapped_channels_records_return_when_the_picker_closes() {
    let (mut d, _) = channel(&(1..4).map(top).collect::<Vec<_>>());
    let mut fonts = FontSystem::new();
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(joined(&d).contains("message 3"));
    d.open_picker(shown("ROW1").as_ref(), true).unwrap();
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(d.placed_text().is_empty(), "the picker's view holds no record");
    assert!(!d.no_messages() && d.compose_text(true).is_none());
    d.dismiss_picker();
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(joined(&d).contains("message 3"));
    assert!(d.compose_text(true).is_some());
}

#[test]
fn the_picker_takes_the_keys_that_edit_choose_and_close_it() {
    let typed = Key::Character("l".into());
    let named = |k| Key::Named(k);
    fn step<'a>(action: Option<Action>, key: &'a Key, repeat: bool) -> Option<PickStep<'a>> {
        pick_step(action, key, repeat, (false, false, false))
    }
    assert_eq!(step(Some(Action::ChatSend), &named(NamedKey::Enter), false), Some(PickStep::Pick));
    assert_eq!(step(Some(Action::ReturnNav), &named(NamedKey::Escape), false), Some(PickStep::Close));
    assert_eq!(step(Some(Action::ChatMarkOlder), &named(NamedKey::ArrowUp), false), Some(PickStep::Choose(false)));
    assert_eq!(step(Some(Action::ChatMarkNewer), &named(NamedKey::ArrowDown), true), Some(PickStep::Choose(true)));
    assert_eq!(step(Some(Action::Paste), &typed, false), Some(PickStep::Paste));
    assert_eq!(step(Some(Action::ChatSend), &named(NamedKey::Enter), true), Some(PickStep::Held));
    assert_eq!(step(Some(Action::Paste), &typed, true), Some(PickStep::Held));
    assert_eq!(step(Some(Action::ChatSetup), &typed, false), Some(PickStep::Held));
    assert_eq!(step(None, &typed, false), Some(PickStep::Edit(Edit::Text("l"))));
    assert_eq!(step(None, &named(NamedKey::Backspace), false), Some(PickStep::Edit(Edit::Backspace)));
    // A reading key the picker does not use is left to the drawer.
    assert_eq!(step(Some(Action::ScrollPageUp), &named(NamedKey::PageUp), false), None);
}

#[test]
fn the_body_draws_the_choice_in_view_and_the_guide_below_it() {
    let body = Body { head: String::new(), entries: (0..30).map(|i| (format!("#c{i}"), "")).collect(), chosen: 20, guide: "a guide that wraps over two rows".into() };
    let rows = body.lines(20, 10);
    assert!(rows.len() <= 10);
    assert!(rows.iter().any(|(t, tone)| t.starts_with("> #c20") && *tone == Tone::Chosen), "{rows:?}");
    assert!(rows.last().unwrap().0.ends_with("rows") && rows.last().unwrap().1 == Tone::Muted, "{rows:?}");
}
