use std::time::{Duration, Instant};

use cosmic_text::FontSystem;
use sot_protocol::ops::{ChatChannel, ChatChannelsRes, ChatCode, ChatPairRes};

use super::compose::Edit;
use super::drawer::ChatDrawer;
use super::owner::Out;
use super::drawer::never_written;
use super::picker::{link_target, Body};
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
    // The frame after the answer opens the picker by itself, and asks which workspaces run; an older computer's daemon
    // answers that it serves one, and the picker asks for its list.
    let outs = d.tick(shown("ROW1"), at + Duration::from_millis(1)).0;
    older(&mut d, at);
    (d, at, outs)
}

/// The picker's daemon is an older computer's, which serves one workspace: the picker asks for that one's list.
pub(super) fn older(d: &mut ChatDrawer, at: Instant) {
    let outs = d.on_evt(&host(), ChatEvt::Workspaces { reply: ChatReply::Unsupported }, at);
    assert_eq!(outs.iter().filter(|(_, r)| matches!(r, OutgoingReq::ChatChannels { workspace: None })).count(), 1);
}

fn channels_reply(more: bool) -> ChatEvt {
    let res: ChatChannelsRes = serde_json::from_value(serde_json::json!({ "channels": [
        { "id": "C4", "name": "random", "private": false, "member": true },
        { "id": "G3", "name": "lab-papers", "private": true, "member": true },
        { "id": "C2", "name": "lab-microscopes", "private": false, "member": false },
        { "id": "C1", "name": "lab-general", "private": false, "member": true },
    ], "more": more })).unwrap();
    ChatEvt::Channels { workspace: None, reply: ChatReply::Done(res) }
}

/// Each pair sent: its row, its channel, and the workspace it names (none: an older daemon's one, the file's).
fn pairs(outs: &[Out]) -> Vec<(String, Option<String>, Option<String>)> {
    outs.iter().filter_map(|(_, r)| match r {
        OutgoingReq::ChatPair { row, workspace, channel } => Some((row.clone(), channel.as_ref().map(|c| c.as_str().to_string()), workspace.clone())),
        _ => None,
    }).collect()
}

/// How many times the picker opened and asked which workspaces run, the first of its reads.
fn lists(outs: &[Out]) -> usize {
    outs.iter().filter(|(_, r)| matches!(r, OutgoingReq::ChatWorkspaces)).count()
}

fn paired(row: &str, channel: Option<&str>, reply: ChatReply<ChatPairRes>) -> ChatEvt {
    ChatEvt::Paired { row: row.to_string(), channel: channel.map(id), reply }
}

fn shown_lines(d: &ChatDrawer) -> Vec<String> {
    d.body().expect("a body").lines(80, 24)
}

/// The picker's rows from its first entry down, under the head that names the workspace and the Slack app.
fn listed(d: &ChatDrawer) -> Vec<String> {
    let entry = |l: &String| l.starts_with("> ") || l.starts_with("  #") || l.starts_with("  No channel") || l.starts_with("  Connect");
    shown_lines(d).into_iter().skip_while(|l| !entry(l)).collect()
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
    let lines = listed(&d);
    assert!(lines[0].starts_with("> #lab-general"), "{lines:?}");
    assert!(lines[1].starts_with("  #lab-microscopes") && lines[1].ends_with("your Slack app joins it when you pick it"), "{lines:?}");
    assert!(lines[2].starts_with("  #lab-papers") && lines[2].ends_with("private; your Slack app is in it"), "{lines:?}");
    assert!(lines[3].starts_with("  #random") && lines[3].trim_end() == "  #random", "a public channel the app is in has no mark: {lines:?}");
    assert!(lines[4].starts_with("  Connect Slack again") && lines[4].ends_with("connect again with new Slack codes (same workspace)"), "{lines:?}");
    let guide = lines[5..].join(" ");
    assert!(guide.contains("Esc to close") && guide.contains("Picking a public channel your Slack app is not in makes it join") && !guide.contains("paste a channel's link"), "{guide}");
    // An older computer's head names the one connected workspace and says who can update it, what the Slack app is, and
    // why a private channel may be missing, with what to type in Slack.
    let head = shown_lines(&d).join(" ");
    assert!(head.contains("A private channel is listed only once someone in it invites your Slack app: in that channel in Slack, type /invite @Ship of Tools, then open this list again."), "{head}");
    assert!(head.contains("channels of your connected Slack workspace") && head.contains("Ship of Tools on a is too old to connect more than one Slack workspace"), "{head}");
    assert!(head.contains("Your Slack app (the bot that reads the channel and posts for your agents)"), "{head}");
    assert!(guide.contains("To move the chat to another workspace, see the manual page \"Setting up Slack\": press F1, select \"Choose channel\" in the list, and press Enter"), "{guide}");
    // A list cut at the bound says to paste a link.
    d.on_evt(&host(), channels_reply(true), at);
    assert!(shown_lines(&d).join(" ").contains("paste a channel's link"));
    // A short drawer keeps the choice in view and cuts the guide and then the head, never the entries' window below three rows:
    // from four rows up, on the first entry and on the last.
    for last in [false, true] {
        if last {
            (0..5).for_each(|_| d.picker_step(true));
        }
        for height in 4..=9 {
            let rows = d.body().unwrap().lines(80, height);
            assert_eq!(rows.len(), height, "last {last}, {height} rows");
            assert_eq!(rows.iter().filter(|l| l.starts_with("> ")).count(), 1, "last {last}, {height} rows: {rows:?}");
        }
    }
    (0..5).for_each(|_| d.picker_step(false));
    // A read that fails says why and offers only to connect Slack again; the picker does not hide.
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), ChatEvt::Channels { workspace: None, reply: ChatReply::Refused(ChatCode::Unavailable) }, at);
    assert_eq!(shown_lines(&d)[..2], ["could not list the channels: the chat is not available here", "> Connect Slack again  connect again with new Slack codes (same workspace)"]);
    d.on_evt(&host(), ChatEvt::Channels { workspace: None, reply: ChatReply::Unsupported }, at);
    assert_eq!(shown_lines(&d)[0], "Ship of Tools on a is too old to list channels: update it");
    assert!(d.picker_enter().is_empty() && d.in_setup(), "Connect Slack again sends nothing and starts the Connect step");
}

/// An older computer's listed picker says whose channels these are, that Ship of Tools there connects one Slack workspace
/// and who can update it, and where the manual says how to move it; a filter that matches nothing keeps the line and adds its own.
#[test]
fn the_picker_says_the_one_connected_workspace_and_that_another_cannot_be_added_yet() {
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(false), at);
    let said = shown_lines(&d).join(" ");
    assert!(said.contains("These are the channels of your connected Slack workspace. Ship of Tools on a is too old to connect more than one Slack workspace: whoever runs Ship of Tools there can update it."), "{said}");
    assert!(said.contains("manual page \"Setting up Slack\": press F1, select \"Choose channel\" in the list, and press Enter; the page opens in your browser."), "{said}");
    // Typing narrows the entries and leaves the line; a filter with no match says so under it.
    d.picker_edit(Edit::Text("zzz"));
    let said = shown_lines(&d).join(" ");
    assert!(said.contains("whoever runs Ship of Tools there") && said.contains("no channel matches"), "{said}");
}

#[test]
fn typing_filters_and_enter_pairs_once() {
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(false), at);
    // The `#` the entries are drawn with may be typed.
    d.picker_edit(Edit::Text("#"));
    assert!(listed(&d)[0].starts_with("> #lab-general") && listed(&d).len() > 4);
    d.picker_edit(Edit::Backspace);
    d.picker_edit(Edit::Text("LAB"));
    assert_eq!(d.title(), " chat · choose this project's channel  filter: LAB ");
    let lines = listed(&d);
    assert!(lines[0].starts_with("> #lab-general") && lines[2].starts_with("  #lab-papers") && !lines.iter().any(|l| l.contains("#random")), "{lines:?}");
    d.picker_step(true);
    d.picker_step(true);
    d.picker_step(true);
    assert!(listed(&d)[2].starts_with("> #lab-papers"), "the choice stops at the last entry");
    d.picker_step(false);
    // Enter sends one pair for the chosen channel; a second Enter before the reply sends nothing.
    let sent = d.picker_enter();
    assert_eq!(pairs(&sent), [("ROW1".to_string(), Some("C2".to_string()), None)], "an older daemon's pick names no workspace");
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
    assert!(shown_lines(&d).iter().any(|l| l == "no channel matches"));
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
        assert_eq!(link_target(text).map(|(_, c)| c).as_ref().map(ChatChannel::as_str), want, "{text:?}");
    }
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(true), at);
    assert_eq!(d.picker_paste("hello there").unwrap_err(), "not pasted: that is not a Slack channel link");
    assert_eq!(d.title(), " chat · choose this project's channel ");
    let sent = d.picker_paste("https://app.slack.com/client/T1/C0456EFGH").unwrap();
    assert_eq!(pairs(&sent), [("ROW1".to_string(), Some("C0456EFGH".to_string()), None)], "an older daemon's paste names no workspace");
    assert_eq!(d.picker_paste("C9999999999").unwrap_err(), "setting this project's channel…", "one pair at a time");
}

#[test]
fn the_setup_chord_offers_no_channel_first_on_a_mapped_row() {
    let (mut d, at) = channel(&[]);
    let outs = d.open_picker(shown("ROW1").as_ref()).unwrap();
    assert_eq!(lists(&outs), 1);
    older(&mut d, at);
    d.on_evt(&host(), channels_reply(false), at);
    let lines = listed(&d);
    assert!(lines[0].starts_with("  No channel for this project") && lines[1].starts_with("> #lab-general"), "{lines:?}");
    // A narrowed list is channels alone, so typing never leaves Enter on the unpairing.
    d.picker_edit(Edit::Text("lab"));
    assert!(!shown_lines(&d).iter().any(|l| l.contains("No channel for")));
    (0..3).for_each(|_| d.picker_edit(Edit::Backspace));
    assert!(listed(&d)[1].starts_with("> #lab-general"), "the choice returns to the first channel");
    // `No channel for this project` unpairs, and the row then stays unpicked until a person asks.
    d.picker_step(false);
    let sent = d.picker_enter();
    assert_eq!(pairs(&sent), [("ROW1".to_string(), None, None)], "an older daemon's unpair names no workspace");
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
    assert!(listed(&d)[0].starts_with("> #lab-general") && !shown_lines(&d).iter().any(|l| l.contains("No channel for")));
    let mut off = ChatDrawer::default();
    off.tick(shown("ROW1"), at);
    assert_eq!(off.open_picker(shown("ROW1").as_ref()).unwrap_err(), "no connection to a");
    off.on_connection(&host(), true, false, at);
    off.tick(shown("ROW1"), at + Duration::from_millis(200));
    off.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "invalid" })), at + Duration::from_millis(210));
    assert!(off.open_picker(shown("ROW1").as_ref()).unwrap_err().starts_with("the chat on a is not set up correctly"));
    assert_eq!(off.open_picker(None).unwrap_err(), "no project is selected to choose a channel for");
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
        (ChatReply::Unsupported, "not set: Ship of Tools on a is too old to set a channel: update it"),
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
    assert!(matches!(never_written(&OutgoingReq::ChatPair { row: "ROW1".into(), workspace: None, channel: Some(id("C1")) }),
        Some(ChatEvt::Paired { reply: ChatReply::Refused(ChatCode::Unavailable), .. })));
    assert!(matches!(never_written(&OutgoingReq::ChatChannels { workspace: None }), Some(ChatEvt::Channels { workspace: None, reply: ChatReply::Refused(ChatCode::Unavailable) })));
    assert!(matches!(never_written(&OutgoingReq::ChatChannels { workspace: Some("WS1".into()) }), Some(ChatEvt::Channels { workspace: Some(w), .. }) if w == "WS1"));
    assert!(matches!(never_written(&OutgoingReq::ChatWorkspaces), Some(ChatEvt::Workspaces { reply: ChatReply::Refused(ChatCode::Unavailable) })));
    assert!(never_written(&OutgoingReq::ChatResolve { row: "ROW1".into() }).is_none());
    // Told so, the picker is not left waiting: its list says why, and a pair that was never queued is refused and frees the next.
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), never_written(&OutgoingReq::ChatChannels { workspace: None }).unwrap(), at);
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
    assert_eq!(d.body().unwrap().lines(80, 5), ["reading which Slack workspaces the chat on a is connected to…"]);
    older(&mut d, Instant::now());
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

/// A `chat.workspaces` answer: `default`, then each workspace's id, name and whether it runs.
fn workspaces(default: &str, all: &[(&str, Option<&str>, bool)]) -> ChatEvt {
    let list: Vec<serde_json::Value> = all.iter().map(|(id, name, running)| serde_json::json!({ "id": id, "name": name, "running": running })).collect();
    let res = serde_json::from_value(serde_json::json!({ "default": default, "workspaces": list })).unwrap();
    ChatEvt::Workspaces { reply: ChatReply::Done(res) }
}

/// The workspaces whose channels were asked for.
fn asked(outs: &[Out]) -> Vec<Option<String>> {
    outs.iter().filter_map(|(_, r)| match r {
        OutgoingReq::ChatChannels { workspace } => Some(workspace.clone()),
        _ => None,
    }).collect()
}

fn listed_in(workspace: &str, more: bool) -> ChatEvt {
    match channels_reply(more) {
        ChatEvt::Channels { reply, .. } => ChatEvt::Channels { workspace: Some(workspace.to_string()), reply },
        _ => unreachable!(),
    }
}

/// An unmapped row's picker on a computer running two workspaces (a third stopped): it opens on the default's list, one
/// workspace at a time, says so with Left and Right, moves between them without wrapping, and pairs in the one shown.
#[test]
fn the_picker_shows_one_workspace_at_a_time_and_left_and_right_move_between_them() {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.set_focused(true);
    d.on_connection(&host(), true, false, t0);
    d.tick(shown("ROW1"), t0);
    d.tick(shown("ROW1"), t0 + Duration::from_millis(200));
    let at = t0 + Duration::from_millis(210);
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), at);
    assert_eq!(lists(&d.tick(shown("ROW1"), at).0), 1);
    let all = [("TAAAA1111", Some("Alpha Lab"), true), ("TBBBB2222", Some("Beta Lab"), true), ("TCCCC3333", Some("Gamma Lab"), false)];
    assert_eq!(asked(&d.on_evt(&host(), workspaces("TAAAA1111", &all), at)), [Some("TAAAA1111".to_string())]);
    assert_eq!(d.title(), " chat · choose this project's channel · Alpha Lab (1 of 2) ");
    assert_eq!(shown_lines(&d), ["reading the channels of the Slack workspace Alpha Lab…"]);
    // Another workspace's late list is not this one's.
    d.on_evt(&host(), listed_in("TBBBB2222", false), at);
    assert_eq!(shown_lines(&d), ["reading the channels of the Slack workspace Alpha Lab…"]);
    d.on_evt(&host(), listed_in("TAAAA1111", false), at);
    let said = d.body().unwrap().lines(2000, 60).join(" ");
    assert!(said.contains("These are the channels of the Slack workspace Alpha Lab that your Slack app (the bot you make for yourself in each Slack workspace, which reads the channel and posts for your agents) can see, one of the 2 Slack workspaces the chat on a is connected to. The list shows one workspace at a time: Left and Right move between them, and Connect a Slack workspace is at the end of each."), "{said}");
    assert!(said.contains("picking a public channel adds it there") && said.contains("Left and Right for another Slack workspace"), "{said}");
    assert!(!said.contains("To move the chat to another workspace"), "{said}");
    let lines = listed(&d);
    assert!(lines[4].starts_with("  Connect a Slack workspace") && lines[4].ends_with("add a Slack workspace, or give one new Slack codes"), "{lines:?}");
    // Left at the first does nothing; Right shows the second and asks its list; Right at the last does nothing.
    assert!(d.picker_move(false).unwrap().is_empty());
    assert_eq!(asked(&d.picker_move(true).unwrap()), [Some("TBBBB2222".to_string())]);
    assert_eq!(d.title(), " chat · choose this project's channel · Beta Lab (2 of 2) ");
    assert!(d.picker_move(true).unwrap().is_empty());
    d.on_evt(&host(), listed_in("TBBBB2222", false), at);
    // Enter pairs in the workspace shown.
    let sent = d.picker_enter();
    assert!(matches!(sent.as_slice(), [(_, OutgoingReq::ChatPair { workspace: Some(w), channel: Some(c), .. })] if w == "TBBBB2222" && c.as_str() == "C1"), "{sent:?}");
    // A channel that is not in it says which workspace and that the others are a key away.
    d.on_evt(&host(), paired("ROW1", Some("C1"), ChatReply::Refused(ChatCode::NoSuchChannel)), at);
    assert_eq!(d.take_notice().as_deref(), Some("not set: that channel is not in the Slack workspace Beta Lab; Left and Right show the channels of the other Slack workspaces; if it is a private channel, your Slack app is not in it: in that channel in Slack, type /invite @Ship of Tools, then pick it again"));
    assert!(d.picking());
    // A pasted link pairs in the workspace it names when that one runs; one the computer is not connected to is refused.
    let sent = d.picker_paste("https://app.slack.com/client/TAAAA1111/C0456EFGH").unwrap();
    assert!(matches!(sent.as_slice(), [(_, OutgoingReq::ChatPair { workspace: Some(w), .. })] if w == "TAAAA1111"), "{sent:?}");
    d.on_evt(&host(), paired("ROW1", Some("C0456EFGH"), ChatReply::Refused(ChatCode::NotInChannel)), at);
    assert_eq!(
        d.picker_paste("https://app.slack.com/client/TCCCC3333/C0456EFGH").unwrap_err(),
        "not pasted: that channel is in a Slack workspace the chat on a is not connected to; to connect it, choose Connect a Slack workspace at the end of this list"
    );
    let sent = d.picker_paste("C0456EFGH").unwrap();
    assert!(matches!(sent.as_slice(), [(_, OutgoingReq::ChatPair { workspace: Some(w), .. })] if w == "TBBBB2222"), "an id alone pairs in the one shown: {sent:?}");
}

/// A mapped row's picker opens on its own workspace; one workspace's list names it and says how to add another; a
/// workspace with no name is said by its ID; none running leaves only the Connect step; a list that fails names its workspace.
#[test]
fn the_picker_opens_on_the_rows_workspace_and_says_each_place() {
    let (mut d, at) = channel(&[]);
    assert_eq!(lists(&d.open_picker(shown("ROW1").as_ref()).unwrap()), 1);
    let all = [("TAAAA1", Some("Alpha Lab"), true), ("WS1", None, true)];
    assert_eq!(asked(&d.on_evt(&host(), workspaces("TAAAA1", &all), at)), [Some("WS1".to_string())], "the row's own, not the default");
    assert_eq!(d.title(), " chat · choose this project's channel · workspace WS1 (2 of 2) ");
    d.on_evt(&host(), ChatEvt::Channels { workspace: Some("WS1".into()), reply: ChatReply::Refused(ChatCode::Unavailable) }, at);
    assert!(d.body().unwrap().lines(2000, 9)[0].starts_with("could not list the channels of the Slack workspace with ID WS1: the chat is not available here"));
    // Unpairing names the workspace shown.
    d.on_evt(&host(), listed_in("WS1", false), at);
    (0..2).for_each(|_| d.picker_step(false));
    assert!(matches!(d.picker_enter().as_slice(), [(_, OutgoingReq::ChatPair { workspace: Some(w), channel: None, .. })] if w == "WS1"));

    let (mut d, at) = channel(&[]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    d.on_evt(&host(), workspaces("WS1", &[("WS1", Some("Alpha Lab"), true)]), at);
    assert_eq!(d.title(), " chat · choose this project's channel · Alpha Lab ");
    d.on_evt(&host(), listed_in("WS1", false), at);
    let said = d.body().unwrap().lines(2000, 60).join(" ");
    // The row's channel is not in the list, so its words lead, and they say what your Slack app is.
    assert!(said.starts_with("This project's channel, CH1, is not in this list: your Slack app (the bot that reads the channel and posts for your agents) is not in it"), "{said}");
    assert!(said.contains("These are the channels of the Slack workspace Alpha Lab that your Slack app can see. Your Slack app reads only the channels it is in; picking a public channel adds it there. A private channel is listed only once someone in it invites your Slack app: in that channel in Slack, type /invite @Ship of Tools, then open this list again. To add another Slack workspace, choose Connect a Slack workspace at the end of this list."), "{said}");
    assert!(said.contains("Type to narrow the list, Up and Down to choose, Enter to pick"), "one workspace's guide has no Left and Right: {said}");
    assert!(d.picker_move(true).unwrap().is_empty());

    let (mut d, at) = channel(&[]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    assert!(asked(&d.on_evt(&host(), workspaces("WS1", &[("WS1", None, false)]), at)).is_empty());
    let lines = shown_lines(&d);
    assert!(lines.join(" ").starts_with("no Slack workspace is running on a right now: choose Connect a Slack workspace below; the log of Ship of Tools' service on a says why"), "{lines:?}");
    assert_eq!(listed(&d)[0], "> Connect a Slack workspace  add a Slack workspace, or give one new Slack codes");
    assert!(d.picker_enter().is_empty() && d.in_setup(), "the Connect step takes the picker's place");

    let (mut d, at) = channel(&[]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    d.on_evt(&host(), ChatEvt::Workspaces { reply: ChatReply::Refused(ChatCode::Busy) }, at);
    assert!(shown_lines(&d)[0].starts_with("could not read which Slack workspaces the chat on a is connected to: "), "{:?}", shown_lines(&d));
}

/// An unmapped row's picker on a computer running Alpha Lab (`TAAAA1111`, the default) and Beta Lab (`TBBBB2222`), before
/// any list is answered.
fn known_two() -> (ChatDrawer, Instant) {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.set_focused(true);
    d.on_connection(&host(), true, false, t0);
    d.tick(shown("ROW1"), t0);
    d.tick(shown("ROW1"), t0 + Duration::from_millis(200));
    let at = t0 + Duration::from_millis(210);
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), at);
    assert_eq!(lists(&d.tick(shown("ROW1"), at).0), 1);
    let all = [("TAAAA1111", Some("Alpha Lab"), true), ("TBBBB2222", Some("Beta Lab"), true)];
    assert_eq!(asked(&d.on_evt(&host(), workspaces("TAAAA1111", &all), at)), [Some("TAAAA1111".to_string())]);
    (d, at)
}

/// The picker's arrows, by action and whether the key is held (`picker_arrow`): Up and Down choose, held too; Left and
/// Right move between workspaces, never held; no other key is an arrow, so none falls through to the thread keys.
#[test]
fn the_pickers_arrows_choose_and_move_and_a_held_one_moves_nothing() {
    use super::{picker_arrow, PickerArrow};
    for (action, repeat, want) in [
        (Some(Action::ChatMarkOlder), false, Some(PickerArrow::Choose { down: false })),
        (Some(Action::ChatMarkNewer), true, Some(PickerArrow::Choose { down: true })),
        (Some(Action::ChatThreadClose), false, Some(PickerArrow::Workspace { next: false })),
        (Some(Action::ChatThreadOpen), false, Some(PickerArrow::Workspace { next: true })),
        (Some(Action::ChatThreadOpen), true, Some(PickerArrow::Held)),
        (Some(Action::ChatThreadClose), true, Some(PickerArrow::Held)),
        (Some(Action::ChatSend), false, None),
        (Some(Action::ChatSetup), false, None),
        (None, false, None),
    ] {
        assert_eq!(picker_arrow(action, repeat), want, "{action:?} held {repeat}");
    }
}

/// A move shows the new workspace's list as reading until it is answered: nothing of the old list can be picked under the
/// new title.
#[test]
fn a_move_reads_its_list_before_any_pick() {
    let (mut d, at) = known_two();
    d.on_evt(&host(), listed_in("TAAAA1111", false), at);
    assert_eq!(asked(&d.picker_move(true).unwrap()), [Some("TBBBB2222".to_string())]);
    assert_eq!(shown_lines(&d), ["reading the channels of the Slack workspace Beta Lab…"]);
    assert!(d.picker_enter().is_empty(), "a pick before the new list was answered");
}

/// Left and Right wait for a pair in flight, so its answer's words name the workspace it was sent to.
#[test]
fn left_and_right_wait_for_a_pair_in_flight() {
    let (mut d, at) = known_two();
    d.on_evt(&host(), listed_in("TAAAA1111", false), at);
    assert_eq!(pairs(&d.picker_enter()), [("ROW1".to_string(), Some("C1".to_string()), Some("TAAAA1111".to_string()))]);
    assert_eq!(d.picker_move(true).unwrap_err(), "setting this project's channel…");
    assert_eq!(d.title(), " chat · choose this project's channel · Alpha Lab (1 of 2) ");
    d.on_evt(&host(), paired("ROW1", Some("C1"), ChatReply::Refused(ChatCode::NoSuchChannel)), at);
    assert_eq!(d.take_notice().as_deref(), Some("not set: that channel is not in the Slack workspace Alpha Lab; Left and Right show the channels of the other Slack workspaces; if it is a private channel, your Slack app is not in it: in that channel in Slack, type /invite @Ship of Tools, then pick it again"));
    assert!(d.picker_move(true).is_ok(), "the answer frees the keys");
}

/// A paste waits until the workspaces are known (a link's workspace could not be told from the one shown before), and is
/// refused with none running; Slack's Copy link, which names its workspace by domain alone, pairs in the one shown.
#[test]
fn a_paste_waits_for_the_workspaces_and_a_copy_link_pairs_in_the_one_shown() {
    let (mut d, _) = channel(&[]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    assert_eq!(
        d.picker_paste("https://app.slack.com/client/TBBBB2222/C0456EFGH").unwrap_err(),
        "not pasted: still reading which Slack workspaces the chat on a is connected to; paste it again in a moment"
    );
    let (mut d, at) = channel(&[]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    d.on_evt(&host(), workspaces("WS1", &[("WS1", None, false)]), at);
    assert_eq!(d.picker_paste("C0456EFGH").unwrap_err(), "not pasted: no Slack workspace is running on a right now");

    let (mut d, at) = known_two();
    d.picker_move(true).unwrap();
    d.on_evt(&host(), listed_in("TBBBB2222", false), at);
    let sent = d.picker_paste("https://lab-beta.slack.com/archives/C0456EFGH").unwrap();
    assert_eq!(pairs(&sent), [("ROW1".to_string(), Some("C0456EFGH".to_string()), Some("TBBBB2222".to_string()))]);
}
