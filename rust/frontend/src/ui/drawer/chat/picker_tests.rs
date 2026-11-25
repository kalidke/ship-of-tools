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
    shown_lines(d).into_iter().skip_while(|l| !is_row(l)).collect()
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
    assert_eq!(tree(&d)[..2], ["> v your connected Slack workspace", "    . reading its channels…"]);
    d.on_evt(&host(), channels_reply(false), at);
    let lines = listed(&d);
    assert!(lines[1].starts_with(">   . #lab-general"), "{lines:?}");
    assert!(lines[2].starts_with("    . #lab-microscopes") && lines[2].ends_with("your Slack app joins it when you pick it"), "{lines:?}");
    assert!(lines[3].starts_with("    . #lab-papers") && lines[3].ends_with("private; your Slack app is in it"), "{lines:?}");
    assert!(lines[4] == "    . #random", "a public channel the app is in has no mark: {lines:?}");
    assert!(lines[5].starts_with("  . Connect Slack again") && lines[5].ends_with("connect again with new Slack codes (same workspace)"), "{lines:?}");
    let guide = lines[6..].join(" ");
    assert!(guide.contains("Esc to close") && guide.contains("Picking a public channel your Slack app is not in makes it join") && !guide.contains("paste a channel's link"), "{guide}");
    // An older computer's head names the one connected workspace and says who can update it, what the Slack app is, and
    // why a private channel may be missing, with what to type in Slack.
    let head = shown_lines(&d).join(" ");
    assert!(head.contains("A private channel is listed only once someone in it invites your Slack app: in that channel in Slack, type /invite @Ship of Tools, then press Esc and open this list again."), "{head}");
    assert!(head.contains("channels of your connected Slack workspace") && head.contains("Ship of Tools on a is too old to connect more than one Slack workspace"), "{head}");
    assert!(head.contains("Your Slack app (the bot that reads the channel and posts for your agents)"), "{head}");
    assert!(guide.contains("To move the chat to another workspace, see the manual page \"Setting up Slack\": press F1, select \"Choose channel\" in the list, and press Enter"), "{guide}");
    // A list cut at the bound says to paste a link.
    d.on_evt(&host(), channels_reply(true), at);
    assert!(said(&d).contains("paste a channel's link"));
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
    assert!(said(&d).starts_with("could not list the channels: the chat is not available here"));
    assert_eq!(tree(&d), ["> v your connected Slack workspace", "    . could not list its channels", "  . Connect Slack again · connect again with new Slack codes (same workspace)"]);
    d.on_evt(&host(), ChatEvt::Channels { workspace: None, reply: ChatReply::Unsupported }, at);
    assert!(said(&d).starts_with("Ship of Tools on a is too old to list channels: update it"));
    (0..2).for_each(|_| d.picker_step(true));
    assert!(d.picker_enter().is_empty() && d.in_setup(), "Connect Slack again sends nothing and starts the Connect step");
}

/// The name of the picker's last entry, as the list shows it.
fn last_entry(lines: &[String]) -> String {
    let line = lines.iter().rfind(|l| l.trim_start_matches(['>', '.', ' ']).starts_with("Connect")).expect("a Connect entry");
    line.trim_start_matches(['>', '.', ' ']).split(" · ").next().unwrap().to_string()
}

/// The words of a post refused for want of a Slack code that posts as the person name the picker's last entry as the
/// person will see it: `Connect a Slack workspace`, or `Connect Slack again` against an older computer's daemon, and
/// the wire cannot tell which, so the words carry both.
#[test]
fn the_refused_posts_words_name_the_last_entry_the_picker_shows() {
    let words = ChatCode::NoUserToken.text();
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(false), at);
    let older = last_entry(&listed(&d));
    assert_eq!(older, "Connect Slack again");
    assert!(words.contains(&older), "{words}");

    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.set_focused(true);
    d.on_connection(&host(), true, false, t0);
    d.tick(shown("ROW1"), t0);
    d.tick(shown("ROW1"), t0 + Duration::from_millis(200));
    let at = t0 + Duration::from_millis(210);
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), at);
    d.tick(shown("ROW1"), at);
    d.on_evt(&host(), workspaces("TAAAA1111", &[("TAAAA1111", Some("Alpha Lab"), true)]), at);
    d.on_evt(&host(), listed_in("TAAAA1111", false), at);
    let newer = last_entry(&listed(&d));
    assert_eq!(newer, "Connect a Slack workspace");
    assert!(words.contains(&newer), "{words}");
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
    assert!(tree(&d)[1].starts_with(">   . #lab-general") && tree(&d).len() > 4);
    d.picker_edit(Edit::Backspace);
    d.picker_edit(Edit::Text("LAB"));
    assert_eq!(d.title(), " chat · choose this project's channel  filter: LAB ");
    let lines = tree(&d);
    assert!(lines[1].starts_with(">   . #lab-general") && lines[3].starts_with("    . #lab-papers") && !lines.iter().any(|l| l.contains("#random")), "{lines:?}");
    d.picker_step(true);
    d.picker_step(true);
    d.picker_step(true);
    assert!(tree(&d)[3].starts_with(">   . #lab-papers"), "the cursor stops at the last entry");
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
    assert!(tree(&d).contains(&"    . no channel matches".to_string()), "{:?}", tree(&d));
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
fn the_setup_chord_offers_no_channel_after_the_workspaces_on_a_mapped_row() {
    let (mut d, at) = channel(&[]);
    let outs = d.open_picker(shown("ROW1").as_ref()).unwrap();
    assert_eq!(lists(&outs), 1);
    older(&mut d, at);
    d.on_evt(&host(), channels_reply(false), at);
    let lines = tree(&d);
    assert!(lines[1].starts_with(">   . #lab-general") && lines[5].starts_with("  . No channel for this project"), "{lines:?}");
    // A narrowed list is channels alone, so typing never leaves Enter on the unpairing.
    d.picker_edit(Edit::Text("lab"));
    assert!(!shown_lines(&d).iter().any(|l| l.contains("No channel for")));
    (0..3).for_each(|_| d.picker_edit(Edit::Backspace));
    assert!(tree(&d)[1].starts_with(">   . #lab-general"), "the cursor returns to the first channel");
    // `No channel for this project` unpairs, and the row then stays unpicked until a person asks.
    (0..4).for_each(|_| d.picker_step(true));
    assert_eq!(cursor_row(&d), "> . No channel for this project");
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
    older(&mut d, at);
    d.on_evt(&host(), channels_reply(false), at);
    assert!(tree(&d)[1].starts_with(">   . #lab-general") && !shown_lines(&d).iter().any(|l| l.contains("No channel for")));
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
    assert_eq!(tree(&d)[..2], ["> v your connected Slack workspace", "    . reading its channels…"]);
}

#[test]
fn one_pair_is_in_flight_across_escape_and_a_reopened_picker_is_never_closed_by_an_older_reply() {
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(false), at);
    assert_eq!(pairs(&d.picker_enter()).len(), 1);
    // Escape, and Enter over the empty draft: a new picker, with the first pair unanswered.
    d.dismiss_picker();
    d.send(shown("ROW1").as_ref(), Some("tester"), at).unwrap();
    older(&mut d, at);
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
    assert!(said(&d).starts_with("could not list the channels: the chat is not available here"));
    d.on_evt(&host(), channels_reply(false), at);
    d.picker_step(true);
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
    assert_eq!(tree(&d)[..2], ["> v your connected Slack workspace", "    . reading its channels…"]);
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
    let body = Body { head: String::new(), rows: (0..30).map(|i| format!("  . #c{i}")).collect(), cursor: 20, guide: "a guide that wraps over two rows".into(), ..Body::default() };
    let rows = body.lines(20, 10);
    assert!(rows.len() <= 10);
    assert!(rows.iter().any(|t| t == "  . #c20"), "{rows:?}");
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

/// The nav pane's text for a leaf at `depth` with `label`, the cursor's when `selected`.
fn tree_leaf(label: &str, selected: bool, depth: usize) -> String {
    use crate::ui::nav::{format_tree_row, TreeRow};
    let node = sot_protocol::TreeNode { id: "x".into(), label: label.into(), kind: "k".into(), has_children: false, badges: Vec::new(), payload: Default::default() };
    format_tree_row(&TreeRow { node, depth, expanded: false }, selected, false)
}

/// A channel list of `(id, name)`, none private and the app in each.
fn channels_of(channels: &[(&str, &str)]) -> ChatEvt {
    let list: Vec<_> = channels.iter().map(|(id, name)| serde_json::json!({ "id": id, "name": name, "private": false, "member": true })).collect();
    let res: ChatChannelsRes = serde_json::from_value(serde_json::json!({ "channels": list, "more": false })).unwrap();
    ChatEvt::Channels { workspace: None, reply: ChatReply::Done(res) }
}

/// `channels_of`, answering workspace `workspace`.
fn channels_of_in(workspace: &str, channels: &[(&str, &str)]) -> ChatEvt {
    match channels_of(channels) {
        ChatEvt::Channels { reply, .. } => ChatEvt::Channels { workspace: Some(workspace.to_string()), reply },
        _ => unreachable!(),
    }
}

/// The row the cursor is on, as the list draws it.
fn cursor_row(d: &ChatDrawer) -> String {
    shown_lines(d).into_iter().find(|l| l.starts_with("> ")).expect("a cursor row")
}


/// A row of the picker's tree as the list draws it.
fn is_row(l: &str) -> bool {
    l.starts_with(['>', ' ']) && ["v ", "> ", ". "].iter().any(|p| l[1..].trim_start().starts_with(p))
}

/// The tree's rows as drawn, in order.
fn tree(d: &ChatDrawer) -> Vec<String> {
    d.body().expect("a body").lines(2000, 80).into_iter().skip_while(|l| !is_row(l)).take_while(|l| is_row(l)).collect()
}

/// Everything the body says, one line.
fn said(d: &ChatDrawer) -> String {
    d.body().expect("a body").lines(2000, 80).join(" ")
}

const CONNECT: &str = "  . Connect a Slack workspace · add a Slack workspace, or give one new Slack codes";

/// A picker opened over an unmapped row on the workspaces `all` (id, name, running), opening on `default`.
fn opened(default: &str, all: &[(&str, Option<&str>, bool)]) -> (ChatDrawer, Instant) {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.set_focused(true);
    d.on_connection(&host(), true, false, t0);
    d.tick(shown("ROW1"), t0);
    d.tick(shown("ROW1"), t0 + Duration::from_millis(200));
    let at = t0 + Duration::from_millis(210);
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), at);
    assert_eq!(lists(&d.tick(shown("ROW1"), at).0), 1);
    assert_eq!(asked(&d.on_evt(&host(), workspaces(default, all), at)), [Some(default.to_string())]);
    (d, at)
}

/// An unmapped row's picker on a computer running Alpha Lab (`TAAAA1111`, the default) and Beta Lab (`TBBBB2222`), a third
/// (`TCCCC3333`) stopped, before any list is answered.
fn known_two() -> (ChatDrawer, Instant) {
    opened("TAAAA1111", &[("TAAAA1111", Some("Alpha Lab"), true), ("TBBBB2222", Some("Beta Lab"), true), ("TCCCC3333", Some("Gamma Lab"), false)])
}

/// An unmapped row's picker on two running workspaces whose entries name their Slack apps, Alpha's list answered.
fn two_apps() -> (ChatDrawer, Instant) {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.set_focused(true);
    d.on_connection(&host(), true, false, t0);
    d.tick(shown("ROW1"), t0);
    d.tick(shown("ROW1"), t0 + Duration::from_millis(200));
    let at = t0 + Duration::from_millis(210);
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), at);
    d.tick(shown("ROW1"), at);
    let res = serde_json::from_value(serde_json::json!({ "default": "TAAAA1111", "workspaces": [
        { "id": "TAAAA1111", "name": "Alpha Lab", "running": true, "app_name": "Alpha App" },
        { "id": "TBBBB2222", "name": "Beta Lab", "running": true, "app_name": "Beta App" } ] })).unwrap();
    d.on_evt(&host(), ChatEvt::Workspaces { reply: ChatReply::Done(res) }, at);
    d.on_evt(&host(), listed_in("TAAAA1111", false), at);
    (d, at)
}

/// `two_apps` with Beta opened and its list answered; the cursor on Beta's first channel.
fn both_open() -> (ChatDrawer, Instant) {
    let (mut d, at) = two_apps();
    (0..4).for_each(|_| d.picker_step(true));
    d.picker_open();
    d.on_evt(&host(), listed_in("TBBBB2222", false), at);
    d.picker_step(true);
    (d, at)
}

/// The workspaces are rows that open in place, each with its channels beneath it: Alpha opens at the start and its list is
/// asked, Right opens Beta and asks its list once, Left on a channel goes to its workspace and there closes it, and Enter on
/// a channel pairs in its workspace.
#[test]
fn the_channel_list_is_a_tree_of_its_workspaces_with_their_channels_beneath() {
    let (mut d, at) = known_two();
    assert_eq!(d.title(), " chat · choose this project's channel ");
    assert_eq!(tree(&d), ["> v Alpha Lab", "    . reading its channels…", "  > Beta Lab", CONNECT]);
    d.on_evt(&host(), listed_in("TAAAA1111", false), at);
    let alpha = ["  v Alpha Lab", ">   . #lab-general", "    . #lab-microscopes · your Slack app joins it when you pick it", "    . #lab-papers · private; your Slack app is in it", "    . #random", "  > Beta Lab", CONNECT];
    assert_eq!(tree(&d), alpha);
    (0..4).for_each(|_| d.picker_step(true));
    assert_eq!(cursor_row(&d), "> > Beta Lab");
    assert_eq!(asked(&d.picker_open()), [Some("TBBBB2222".to_string())]);
    assert!(d.picker_open().is_empty(), "Right on an open workspace asks nothing");
    assert_eq!(tree(&d)[5..], ["> v Beta Lab", "    . reading its channels…", CONNECT]);
    d.on_evt(&host(), listed_in("TBBBB2222", false), at);
    d.picker_step(true);
    d.picker_close();
    assert_eq!(cursor_row(&d), "> v Beta Lab");
    d.picker_close();
    assert_eq!(tree(&d)[5..], ["> > Beta Lab", CONNECT]);
    // Up to Alpha's last channel: Enter pairs it in Alpha.
    d.picker_step(false);
    assert_eq!(pairs(&d.picker_enter()), [("ROW1".to_string(), Some("C4".to_string()), Some("TAAAA1111".to_string()))]);
}

/// The picker's keys, by action and whether the key is held (`picker_arrow`): Up and Down choose, held too; Right opens and
/// Left closes, never held; no other key is an arrow, so none falls through to the thread keys. Then each key on each kind
/// of row, as the nav pane's trees take them.
#[test]
fn the_pickers_keys_open_close_and_choose_as_the_nav_trees_do() {
    use super::{picker_arrow, PickerArrow};
    for (action, repeat, want) in [
        (Some(Action::ChatMarkOlder), false, Some(PickerArrow::Choose { down: false })),
        (Some(Action::ChatMarkNewer), true, Some(PickerArrow::Choose { down: true })),
        (Some(Action::ChatThreadClose), false, Some(PickerArrow::Close)),
        (Some(Action::ChatThreadOpen), false, Some(PickerArrow::Open)),
        (Some(Action::ChatThreadOpen), true, Some(PickerArrow::Held)),
        (Some(Action::ChatThreadClose), true, Some(PickerArrow::Held)),
        (Some(Action::ChatSend), false, None),
        (Some(Action::ChatSetup), false, None),
        (None, false, None),
    ] {
        assert_eq!(picker_arrow(action, repeat), want, "{action:?} held {repeat}");
    }
    let (mut d, _) = two_apps();
    let open = tree(&d);
    // Right on a channel does nothing; Left on a channel goes to its workspace and closes nothing.
    assert!(d.picker_open().is_empty());
    assert_eq!(tree(&d), open);
    d.picker_close();
    assert_eq!(cursor_row(&d), "> v Alpha Lab");
    assert_eq!(tree(&d).len(), open.len());
    // Left on an open workspace closes it; on a closed one at the top it does nothing.
    d.picker_close();
    assert_eq!(tree(&d), ["> > Alpha Lab", "  > Beta Lab", CONNECT]);
    d.picker_close();
    assert_eq!(tree(&d), ["> > Alpha Lab", "  > Beta Lab", CONNECT]);
    // Right on the closed workspace opens it, its list being held, and leaves the cursor on its row.
    assert!(d.picker_open().is_empty());
    assert_eq!(cursor_row(&d), "> v Alpha Lab");
    assert_eq!(tree(&d).len(), open.len());
    // Enter on an open workspace sends nothing; on a closed one it opens it and asks its list; on a note it sends nothing.
    assert!(d.picker_enter().is_empty());
    assert_eq!(tree(&d).len(), open.len());
    (0..5).for_each(|_| d.picker_step(true));
    assert_eq!(cursor_row(&d), "> > Beta Lab");
    assert_eq!(asked(&d.picker_enter()), [Some("TBBBB2222".to_string())]);
    assert_eq!(cursor_row(&d), "> v Beta Lab");
    d.picker_step(true);
    assert_eq!(cursor_row(&d), ">   . reading its channels…");
    assert!(d.picker_enter().is_empty() && !d.in_setup(), "a note picks nothing");
    // Enter on the Connect step opens it.
    d.picker_step(true);
    assert!(cursor_row(&d).contains("Connect a Slack workspace"));
    assert!(d.picker_enter().is_empty() && d.in_setup());
}

/// A list answered while its workspace is closed is kept and not shown; its opening shows it with no ask. A list this picker
/// never asked is dropped, one still being read is not asked again, and one that failed is asked again at the next opening.
#[test]
fn a_list_for_a_closed_workspace_is_kept_and_shown_at_its_opening_with_no_ask() {
    let (mut d, at) = two_apps();
    // An answer for Beta, which this picker never asked, is dropped: opening Beta asks, and shows it reading.
    d.on_evt(&host(), listed_in("TBBBB2222", false), at);
    (0..4).for_each(|_| d.picker_step(true));
    assert_eq!(asked(&d.picker_open()), [Some("TBBBB2222".to_string())]);
    assert_eq!(tree(&d)[5..], ["> v Beta Lab", "    . reading its channels…", CONNECT]);
    // Closed and opened again while it is being read: nothing is asked.
    d.picker_close();
    assert!(d.picker_open().is_empty());
    // Closed, its answer comes: kept, not shown; its opening shows it with no ask.
    d.picker_close();
    d.on_evt(&host(), channels_of_in("TBBBB2222", &[("C9", "beta-only")]), at);
    assert_eq!(tree(&d)[5..], ["> > Beta Lab", CONNECT]);
    assert!(d.picker_open().is_empty());
    assert_eq!(tree(&d)[5..], ["> v Beta Lab", "    . #beta-only", CONNECT]);
    // A list that failed says so, leads the head with the words, and is asked again at the next opening.
    let (mut d, at) = two_apps();
    (0..4).for_each(|_| d.picker_step(true));
    d.picker_open();
    d.on_evt(&host(), ChatEvt::Channels { workspace: Some("TBBBB2222".into()), reply: ChatReply::Refused(ChatCode::Unavailable) }, at);
    assert_eq!(tree(&d)[5..], ["> v Beta Lab", "    . could not list its channels", CONNECT]);
    assert!(said(&d).starts_with("could not list the channels of the Slack workspace Beta Lab: the chat is not available here"), "{}", said(&d));
    d.picker_close();
    assert_eq!(asked(&d.picker_open()), [Some("TBBBB2222".to_string())]);
}

/// Each workspace's rows say what its list holds: reading, refused (with its words leading the head), a computer too old
/// to list, no channel the Slack app can see, no match for the filter, and a list cut at its bound.
#[test]
fn a_workspaces_rows_say_what_its_list_holds() {
    let (mut d, at) = known_two();
    assert_eq!(tree(&d)[1], "    . reading its channels…");
    d.on_evt(&host(), channels_of_in("TAAAA1111", &[]), at);
    assert_eq!(tree(&d)[..2], ["> v Alpha Lab", "    . no channel your Slack app can see"]);
    d.picker_edit(Edit::Text("zzz"));
    assert_eq!(tree(&d), ["> v Alpha Lab", "    . no channel your Slack app can see", "  > Beta Lab"], "a filter hides the unpairing and Connect");
    let (mut d, at) = two_apps();
    d.picker_edit(Edit::Text("zzz"));
    assert_eq!(tree(&d), ["> v Alpha Lab", "    . no channel matches", "  > Beta Lab"]);
    d.picker_edit(Edit::Backspace);
    d.picker_edit(Edit::Backspace);
    d.picker_edit(Edit::Backspace);
    // Beta is open and its list is cut at its bound: its last row says to paste a link.
    (0..4).for_each(|_| d.picker_step(true));
    d.picker_open();
    d.on_evt(&host(), listed_in("TBBBB2222", true), at);
    assert_eq!(tree(&d).last().map(String::as_str), Some(CONNECT));
    assert!(tree(&d).contains(&"    . More channels exist than are listed: paste a channel's link to pick one.".to_string()), "{:?}", tree(&d));
    d.picker_edit(Edit::Text("zzz"));
    assert!(tree(&d).iter().filter(|r| r.ends_with("no channel matches")).count() == 2, "{:?}", tree(&d));
    // A computer too old to list channels says so, in the head and on the row.
    let (mut d, at) = two_apps();
    (0..4).for_each(|_| d.picker_step(true));
    d.picker_open();
    d.on_evt(&host(), ChatEvt::Channels { workspace: Some("TBBBB2222".into()), reply: ChatReply::Unsupported }, at);
    assert!(said(&d).starts_with("Ship of Tools on a is too old to list channels: update it"), "{}", said(&d));
    // An older computer's one workspace names no workspace in its words.
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), ChatEvt::Channels { workspace: None, reply: ChatReply::Refused(ChatCode::Unavailable) }, at);
    assert_eq!(tree(&d)[..2], ["> v your connected Slack workspace", "    . could not list its channels"]);
    assert!(said(&d).starts_with("could not list the channels: the chat is not available here"), "{}", said(&d));
}

/// The cursor starts on the opened workspace's row and, when its list first arrives, goes to its first channel unless a key
/// moved it; a row that went sends it to its workspace's row; an edit puts it on the first channel shown, else on the first
/// row; it is never on `No channel for this project` unless a key put it there.
#[test]
fn the_cursor_lands_on_a_channel_and_never_falls_on_no_channel_for_this_project() {
    let all = [("WS1", Some("Alpha Lab"), true), ("WS2", Some("Beta Lab"), true)];
    let mapped = || {
        let (mut d, at) = channel(&[]);
        d.open_picker(shown("ROW1").as_ref()).unwrap();
        d.on_evt(&host(), workspaces("WS1", &all), at);
        (d, at)
    };
    let (mut d, at) = mapped();
    assert_eq!(tree(&d), ["> v Alpha Lab", "    . reading its channels…", "  > Beta Lab", "  . No channel for this project", CONNECT]);
    d.on_evt(&host(), listed_in("WS1", false), at);
    assert_eq!(cursor_row(&d), ">   . #lab-general");
    // Closed and its list answered again: the cursor is on its row and stays there (the landing is spent).
    d.picker_close();
    d.on_evt(&host(), listed_in("WS1", false), at);
    assert_eq!(cursor_row(&d), "> v Alpha Lab");
    // The filter: the first channel shown, else the first row; emptied, the first channel again.
    d.picker_step(true);
    (0..3).for_each(|_| d.picker_step(true));
    assert_eq!(cursor_row(&d), ">   . #random");
    d.picker_edit(Edit::Text("lab"));
    assert_eq!(cursor_row(&d), ">   . #lab-general");
    assert!(!tree(&d).iter().any(|r| r.contains("No channel for") || r.contains("Connect a")), "a filter hides the unpairing and Connect: {:?}", tree(&d));
    d.picker_edit(Edit::Text("zzz"));
    assert_eq!(cursor_row(&d), "> v Alpha Lab");
    (0..6).for_each(|_| d.picker_edit(Edit::Backspace));
    assert_eq!(cursor_row(&d), ">   . #lab-general");
    // A key moved the cursor off its row before the list came: the landing does not happen, and the note's row going sends it
    // to its workspace's row.
    let (mut d, at) = mapped();
    d.picker_step(true);
    assert_eq!(cursor_row(&d), ">   . reading its channels…");
    d.on_evt(&host(), listed_in("WS1", false), at);
    assert_eq!(cursor_row(&d), "> v Alpha Lab");
    // A workspace other than the first: its note's row going sends the cursor to its row, not to the first row.
    d.picker_step(true);
    (0..3).for_each(|_| d.picker_step(true));
    d.picker_step(true);
    assert_eq!(cursor_row(&d), "> > Beta Lab");
    d.picker_open();
    d.picker_step(true);
    d.on_evt(&host(), listed_in("WS2", false), at);
    assert_eq!(cursor_row(&d), "> v Beta Lab");
    // A key that moves nothing (Up on the first row) does not spend the landing.
    let (mut d, at) = mapped();
    d.picker_step(false);
    d.on_evt(&host(), listed_in("WS1", false), at);
    assert_eq!(cursor_row(&d), ">   . #lab-general");
    // An edit before the list came spends the landing.
    let (mut d, at) = mapped();
    d.picker_edit(Edit::Text("lab"));
    d.on_evt(&host(), listed_in("WS1", false), at);
    assert_eq!(cursor_row(&d), "> v Alpha Lab");
    // The cursor starts on the row of the workspace the picker opened on, wherever it stands in the list.
    let (d, _) = opened("TBBBB2222", &[("TAAAA1111", Some("Alpha Lab"), true), ("TBBBB2222", Some("Beta Lab"), true)]);
    assert_eq!(cursor_row(&d), "> v Beta Lab");
}

/// Enter on a channel pairs it in the workspace of its row, and the pair's words name that workspace whichever row the cursor
/// is on when the answer comes; a pasted id pairs in the workspace the cursor is in, a link in the workspace it names; the
/// unpairing is in the project's own workspace when it runs, else the one the picker opened on.
#[test]
fn a_pick_and_a_paste_name_their_own_workspace() {
    let (mut d, at) = both_open();
    assert_eq!(pairs(&d.picker_enter()), [("ROW1".to_string(), Some("C1".to_string()), Some("TBBBB2222".to_string()))]);
    d.picker_step(false);
    (0..6).for_each(|_| d.picker_step(false));
    assert_eq!(cursor_row(&d), "> v Alpha Lab");
    d.on_evt(&host(), paired("ROW1", Some("C1"), ChatReply::Refused(ChatCode::NoSuchChannel)), at);
    let words = d.take_notice().unwrap();
    assert!(words.contains("not in the Slack workspace Beta Lab") && words.ends_with("type /invite @Beta App, then pick it again"), "{words}");
    // An id alone pairs where the cursor is (a workspace's row or one of its channels); a link names its own workspace; a Copy
    // link, which names none, goes where the cursor is.
    assert_eq!(pairs(&d.picker_paste("C0456EFGH").unwrap())[0].2.as_deref(), Some("TAAAA1111"));
    d.on_evt(&host(), paired("ROW1", Some("C0456EFGH"), ChatReply::Refused(ChatCode::NotInChannel)), at);
    (0..6).for_each(|_| d.picker_step(true));
    assert!(cursor_row(&d).contains("Beta Lab") || cursor_row(&d).contains("#"), "{}", cursor_row(&d));
    let sent = pairs(&d.picker_paste("https://lab-beta.slack.com/archives/C0456EFGH").unwrap());
    assert_eq!(sent[0].2.as_deref(), Some("TBBBB2222"));
    d.on_evt(&host(), paired("ROW1", Some("C0456EFGH"), ChatReply::Refused(ChatCode::NotInChannel)), at);
    assert_eq!(pairs(&d.picker_paste("https://app.slack.com/client/TAAAA1111/C0456EFGH").unwrap())[0].2.as_deref(), Some("TAAAA1111"));
    d.on_evt(&host(), paired("ROW1", Some("C0456EFGH"), ChatReply::Refused(ChatCode::NotInChannel)), at);
    assert_eq!(
        d.picker_paste("https://app.slack.com/client/TCCCC3333/C0456EFGH").unwrap_err(),
        "not pasted: that channel is in a Slack workspace the chat on a is not connected to; to connect it, choose Connect a Slack workspace at the end of this list"
    );
    // Waiting for the workspaces, and with none running.
    let (mut d, _) = channel(&[]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    assert!(tree(&d).is_empty(), "no row before the workspaces are known");
    d.picker_edit(Edit::Text("a"));
    d.picker_edit(Edit::Backspace);
    assert!(tree(&d).is_empty(), "nor after an edit");
    assert_eq!(
        d.picker_paste("https://app.slack.com/client/TBBBB2222/C0456EFGH").unwrap_err(),
        "not pasted: still reading which Slack workspaces the chat on a is connected to; paste it again in a moment"
    );
    let (mut d, at) = channel(&[]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    d.on_evt(&host(), workspaces("WS1", &[("WS1", None, false)]), at);
    assert_eq!(d.picker_paste("C0456EFGH").unwrap_err(), "not pasted: no Slack workspace is running on a right now");
    // An id alone, with the cursor on the Connect step (which names no workspace), goes to the workspace the picker opened on;
    // against an older computer, which has no workspaces to name, it names none though the row knew one.
    let (mut d, _) = two_apps();
    (0..5).for_each(|_| d.picker_step(true));
    assert!(cursor_row(&d).contains("Connect a Slack workspace"), "{}", cursor_row(&d));
    assert_eq!(pairs(&d.picker_paste("C0456EFGH").unwrap())[0].2.as_deref(), Some("TAAAA1111"));
    let mut p = super::picker::Picker::new(shown("ROW1").unwrap(), false, "app".into()).starting(Some("WS9".into()));
    p.on_workspaces(ChatReply::Unsupported);
    assert_eq!(p.paste("C0456EFGH"), Ok(super::picker::Pick::Channel(id("C0456EFGH"), None)));
    // The unpairing: the project's own workspace (`WS1`) when it runs, though the picker opened on another; else the opened one.
    let current = |ws: &str| Some((ws.to_string(), id("CH1"), "#lab".to_string()));
    let unpair = |own: &str| {
        let mut p = super::picker::Picker::new(shown("ROW1").unwrap(), true, "app".into()).starting(Some("WS2".into())).current(current(own));
        p.on_workspaces(match workspaces("WS2", &[("WS1", Some("Alpha Lab"), true), ("WS2", Some("Beta Lab"), true)]) {
            ChatEvt::Workspaces { reply } => reply,
            _ => unreachable!(),
        });
        (0..2).for_each(|_| p.step(true));
        p.enter()
    };
    assert_eq!(unpair("WS1"), Some(super::picker::Pick::Unpair(Some("WS1".into()))));
    assert_eq!(unpair("GONE"), Some(super::picker::Pick::Unpair(Some("WS2".into()))));
}

/// The words: the head for several workspaces, the guide with its keys, the title with no workspace in it, the missing
/// channel said of the project's own workspace whichever row the cursor is on, the invite naming the cursor's workspace's
/// Slack app, and a refusal that points at Right.
#[test]
fn the_trees_words_name_its_workspaces_and_its_keys() {
    let (mut d, at) = known_two();
    d.on_evt(&host(), listed_in("TAAAA1111", false), at);
    let head = said(&d);
    assert!(head.contains("These are the 2 Slack workspaces the chat on a is connected to, each with the channels your Slack app (the bot you make for yourself in each Slack workspace, which reads the channel and posts for your agents) can see in it, and Connect a Slack workspace is at the end. Your Slack app reads only the channels it is in; picking a public channel adds it there. A private channel is listed only once someone in it invites your Slack app. For the Slack workspace Alpha Lab: in that channel in Slack, type /invite @Ship of Tools, then press Esc and open this list again."), "{head}");
    assert!(head.contains("Type to narrow the channels of the open workspaces, Up and Down to choose, Right to open a Slack workspace and Left to close it, Enter to pick, Esc to close."), "{head}");
    assert!(!head.contains("Left and Right") && !head.contains("one workspace at a time"), "{head}");
    d.picker_edit(Edit::Text("lab"));
    assert_eq!(d.title(), " chat · choose this project's channel  filter: lab ");
    // A refused pair says Right shows the other workspaces' channels.
    let (mut d, at) = two_apps();
    d.picker_enter();
    d.on_evt(&host(), paired("ROW1", Some("C1"), ChatReply::Refused(ChatCode::NoSuchChannel)), at);
    assert!(d.take_notice().unwrap().contains("; Right on another Slack workspace in this list shows its channels; if it is a private channel"));
    // The project's channel is in WS1; the list lacks it. The words lead while the cursor is on WS2's row or on Connect.
    let apps = serde_json::from_value(serde_json::json!({ "default": "WS1", "workspaces": [
        { "id": "WS1", "name": "Alpha Lab", "running": true, "app_name": "Ship of Tools (lab-user)" },
        { "id": "WS2", "name": "Beta Lab", "running": true } ] })).unwrap();
    let (mut d, at) = channel(&[]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    d.on_evt(&host(), ChatEvt::Workspaces { reply: ChatReply::Done(apps) }, at);
    d.on_evt(&host(), listed_in("WS1", false), at);
    let on_alpha = said(&d);
    assert!(on_alpha.starts_with("This project's channel, CH1, is not in this list") && on_alpha.contains("To keep it, in that channel in Slack, type /invite @Ship of Tools (lab-user), then press Esc."), "{on_alpha}");
    assert!(on_alpha.contains("invites your Slack app. For the Slack workspace Alpha Lab: in that channel in Slack, type /invite @Ship of Tools (lab-user), then press Esc and open this list again."), "{on_alpha}");
    (0..4).for_each(|_| d.picker_step(true));
    assert_eq!(cursor_row(&d), "> > Beta Lab");
    let on_beta = said(&d);
    assert!(on_beta.starts_with("This project's channel, CH1, is not in this list") && on_beta.contains("To keep it, in that channel in Slack, type /invite @Ship of Tools (lab-user), then press Esc."), "{on_beta}");
    assert!(on_beta.contains("invites your Slack app. For the Slack workspace Beta Lab: in that channel in Slack, type /invite @Ship of Tools, then press Esc and open this list again."), "the cursor's workspace names no app: {on_beta}");
}

/// A mapped row's picker opens on its own workspace; one workspace's list names it and says how to add another; a
/// workspace with no name is said by its ID; none running leaves only the Connect step; a list that fails names its workspace.
#[test]
fn the_picker_opens_on_the_rows_workspace_and_says_each_place() {
    let (mut d, at) = channel(&[]);
    assert_eq!(lists(&d.open_picker(shown("ROW1").as_ref()).unwrap()), 1);
    let all = [("TAAAA1", Some("Alpha Lab"), true), ("WS1", None, true)];
    assert_eq!(asked(&d.on_evt(&host(), workspaces("TAAAA1", &all), at)), [Some("WS1".to_string())], "the row's own, not the default");
    assert_eq!(d.title(), " chat · choose this project's channel ");
    d.on_evt(&host(), ChatEvt::Channels { workspace: Some("WS1".into()), reply: ChatReply::Refused(ChatCode::Unavailable) }, at);
    assert!(d.body().unwrap().lines(2000, 9)[0].starts_with("could not list the channels of the Slack workspace with ID WS1: the chat is not available here"));
    // A mapped row's refused list leaves the cursor on its workspace's row, with the unpairing and Connect below.
    assert_eq!(tree(&d), ["  > Alpha Lab", "> v workspace WS1", "    . could not list its channels", "  . No channel for this project", CONNECT]);
    // Unpairing names the project's own workspace.
    d.on_evt(&host(), listed_in("WS1", false), at);
    (0..5).for_each(|_| d.picker_step(true));
    assert_eq!(cursor_row(&d), "> . No channel for this project");
    assert!(matches!(d.picker_enter().as_slice(), [(_, OutgoingReq::ChatPair { workspace: Some(w), channel: None, .. })] if w == "WS1"));

    let (mut d, at) = channel(&[]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    d.on_evt(&host(), workspaces("WS1", &[("WS1", Some("Alpha Lab"), true)]), at);
    assert_eq!(d.title(), " chat · choose this project's channel ");
    d.on_evt(&host(), listed_in("WS1", false), at);
    let said = d.body().unwrap().lines(2000, 60).join(" ");
    // The row's channel is not in the list, so its words lead, and they say what your Slack app is.
    assert!(said.starts_with("This project's channel, CH1, is not in this list: your Slack app (the bot that reads the channel and posts for your agents) is not in it"), "{said}");
    assert!(said.contains("These are the channels of the Slack workspace Alpha Lab that your Slack app can see. Your Slack app reads only the channels it is in; picking a public channel adds it there. A private channel is listed only once someone in it invites your Slack app: in that channel in Slack, type /invite @Ship of Tools, then press Esc and open this list again. To add another Slack workspace, choose Connect a Slack workspace at the end of this list."), "{said}");
    assert!(said.contains("Up and Down to choose, Right to open a Slack workspace and Left to close it, Enter to pick"), "{said}");
    assert!(d.picker_open().is_empty());

    let (mut d, at) = channel(&[]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    assert!(asked(&d.on_evt(&host(), workspaces("WS1", &[("WS1", None, false)]), at)).is_empty());
    let lines = shown_lines(&d);
    assert!(lines.join(" ").starts_with("no Slack workspace is running on a right now: choose Connect a Slack workspace below; the log of Ship of Tools' service on a says why"), "{lines:?}");
    assert_eq!(listed(&d)[0], "> . Connect a Slack workspace · add a Slack workspace, or give one new Slack codes");
    assert!(d.picker_enter().is_empty() && d.in_setup(), "the Connect step takes the picker's place");

    let (mut d, at) = channel(&[]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    d.on_evt(&host(), ChatEvt::Workspaces { reply: ChatReply::Refused(ChatCode::Busy) }, at);
    assert!(shown_lines(&d)[0].starts_with("could not read which Slack workspaces the chat on a is connected to: "), "{:?}", shown_lines(&d));
    assert_eq!(listed(&d)[0], "> . Connect a Slack workspace · add a Slack workspace, or give one new Slack codes");
    assert!(d.picker_enter().is_empty() && d.in_setup(), "the Connect step takes the picker's place");
}

/// Left and Right move while a pair is answered, and the pair's words name the workspace it was sent to: that workspace's
/// name, its Slack app, and its list for the private-channel check, whichever workspace is shown when the answer comes.
#[test]
fn left_and_right_move_while_a_pair_is_answered_and_its_words_name_its_own_workspace() {
    // A channel Alpha's list holds, refused after the person moved to Beta: Alpha's name and Slack app.
    let (mut d, at) = two_apps();
    assert_eq!(pairs(&d.picker_enter()).len(), 1);
    (0..4).for_each(|_| d.picker_step(true));
    assert_eq!(asked(&d.picker_enter()), [Some("TBBBB2222".to_string())], "Enter opens a workspace while a pair is answered, as Right does");
    d.on_evt(&host(), paired("ROW1", Some("C1"), ChatReply::Refused(ChatCode::NoSuchChannel)), at);
    assert_eq!(d.take_notice().as_deref(), Some("not set: that channel is not in the Slack workspace Alpha Lab; Right on another Slack workspace in this list shows its channels; if it is a private channel, your Slack app is not in it: in that channel in Slack, type /invite @Alpha App, then pick it again"));
    assert!(d.picking());
    // A private channel of Alpha's list (`G3`), refused after Beta's list answered without it: still said of Alpha's.
    (0..2).for_each(|_| d.picker_step(false));
    let sent = pairs(&d.picker_enter());
    assert_eq!(sent, [("ROW1".to_string(), Some("G3".to_string()), Some("TAAAA1111".to_string()))]);
    d.on_evt(&host(), channels_of_in("TBBBB2222", &[("C9", "other")]), at);
    d.on_evt(&host(), paired("ROW1", Some("G3"), ChatReply::Refused(ChatCode::NotInChannel)), at);
    assert_eq!(d.take_notice().as_deref(), Some("not set: your Slack app is not in that private channel; in that channel in Slack, type /invite @Alpha App, then pick it again"));
    // A pasted link that names Beta, sent while the cursor is in Alpha, is said of Beta.
    let sent = pairs(&d.picker_paste("https://app.slack.com/client/TBBBB2222/C0456EFGH").unwrap());
    assert_eq!(sent, [("ROW1".to_string(), Some("C0456EFGH".to_string()), Some("TBBBB2222".to_string()))]);
    d.on_evt(&host(), paired("ROW1", Some("C0456EFGH"), ChatReply::Refused(ChatCode::NoSuchChannel)), at);
    assert!(d.take_notice().is_some_and(|n| n.contains("not in the Slack workspace Beta Lab") && n.ends_with("type /invite @Beta App, then pick it again")));
}

/// S6: the head says the project's channel is missing only from a whole list of the project's own workspace: a list cut at
/// its bound may hold it further on, and another workspace's list never holds it.
#[test]
fn the_missing_channel_is_said_only_of_a_whole_list_of_its_own_workspace() {
    let missing = "This project's channel, CH1, is not in this list";
    let head = |d: &ChatDrawer| d.body().unwrap().lines(2000, 60).join(" ");
    let opened = |all: &[(&str, Option<&str>, bool)]| {
        let (mut d, at) = channel(&[]);
        d.open_picker(shown("ROW1").as_ref()).unwrap();
        d.on_evt(&host(), workspaces("WS1", all), at);
        (d, at)
    };
    let (mut d, at) = opened(&[("WS1", Some("Alpha Lab"), true)]);
    d.on_evt(&host(), listed_in("WS1", false), at);
    assert!(head(&d).starts_with(missing), "a whole list of its own workspace: {}", head(&d));
    let (mut d, at) = opened(&[("WS1", Some("Alpha Lab"), true)]);
    d.on_evt(&host(), listed_in("WS1", true), at);
    assert!(!head(&d).contains(missing), "a list cut at its bound: {}", head(&d));
    let (mut d, at) = opened(&[("WS1", Some("Alpha Lab"), true), ("WS2", Some("Beta Lab"), true)]);
    d.on_evt(&host(), listed_in("WS1", false), at);
    assert!(head(&d).starts_with(missing), "{}", head(&d));
    (0..4).for_each(|_| d.picker_step(true));
    assert_eq!(asked(&d.picker_open()), [Some("WS2".to_string())]);
    d.on_evt(&host(), listed_in("WS2", false), at);
    assert!(head(&d).starts_with(missing), "said whichever row the cursor is on: {}", head(&d));
    // Another workspace's whole list never decides it: the project's own list was cut, so nothing is said.
    let (mut d, at) = opened(&[("WS1", Some("Alpha Lab"), true), ("WS2", Some("Beta Lab"), true)]);
    d.on_evt(&host(), listed_in("WS1", true), at);
    (0..4).for_each(|_| d.picker_step(true));
    d.picker_open();
    d.on_evt(&host(), listed_in("WS2", false), at);
    assert!(!head(&d).contains(missing), "{}", head(&d));
}

/// S2: the picker's invites name the shown workspace's Slack app as the hub names it (Slack's name for its bot), and the
/// person's (`slack_app_name` of the login) only for a workspace whose entry names none.
#[test]
fn the_pickers_invites_name_the_slack_app_the_hub_names() {
    let (mut d, at) = channel(&[]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    let res = serde_json::from_value(serde_json::json!({ "default": "WS1", "workspaces": [
        { "id": "WS1", "name": "Alpha Lab", "running": true, "app_name": "Ship of Tools (lab-user)" },
        { "id": "WS2", "name": "Beta Lab", "running": true } ] })).unwrap();
    d.on_evt(&host(), ChatEvt::Workspaces { reply: ChatReply::Done(res) }, at);
    d.on_evt(&host(), listed_in("WS1", false), at);
    let said = d.body().unwrap().lines(2000, 60).join(" ");
    assert!(said.contains("To keep it, in that channel in Slack, type /invite @Ship of Tools (lab-user), then press Esc."), "{said}");
    assert!(said.contains("A private channel is listed only once someone in it invites your Slack app. For the Slack workspace Alpha Lab: in that channel in Slack, type /invite @Ship of Tools (lab-user), then press Esc and open this list again."), "{said}");
    assert_eq!(d.picker_paste("C0456EFGH").unwrap().len(), 1);
    d.on_evt(&host(), paired("ROW1", Some("C0456EFGH"), ChatReply::Refused(ChatCode::NoSuchChannel)), at);
    assert!(d.take_notice().is_some_and(|n| n.ends_with("type /invite @Ship of Tools (lab-user), then pick it again")));
    // The cursor in another workspace whose entry names no Slack app: the person's.
    (0..4).for_each(|_| d.picker_step(true));
    assert!(d.body().unwrap().lines(2000, 60).join(" ").contains("type /invite @Ship of Tools, then press Esc and open this list again."));
}

#[test]
fn the_channel_list_draws_its_entries_as_the_nav_tree_draws_its_rows() {
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(false), at);
    let want = [
        ("#lab-general", true),
        ("#lab-microscopes · your Slack app joins it when you pick it", false),
        ("#lab-papers · private; your Slack app is in it", false),
        ("#random", false),
        ("Connect Slack again · connect again with new Slack codes (same workspace)", false),
    ];
    let rows = tree(&d);
    assert_eq!(rows, ["  v your connected Slack workspace",
        ">   . #lab-general",
        "    . #lab-microscopes · your Slack app joins it when you pick it",
        "    . #lab-papers · private; your Slack app is in it",
        "    . #random",
        "  . Connect Slack again · connect again with new Slack codes (same workspace)"]);
    let depths = [1, 1, 1, 1, 0];
    assert_eq!(rows[1..], want.iter().zip(depths).map(|((label, selected), depth)| tree_leaf(label, *selected, depth)).collect::<Vec<_>>()[..]);
}

#[test]
fn a_list_answered_again_keeps_the_cursor_on_its_channel() {
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), channels_reply(false), at);
    d.picker_step(true);
    d.picker_step(true);
    assert_eq!(cursor_row(&d), ">   . #lab-papers · private; your Slack app is in it");
    // The same list again, with a channel sorted above the cursor's: the cursor stays on its channel, now one row lower.
    d.on_evt(&host(), channels_of(&[("C0", "a-first"), ("C1", "lab-general"), ("C2", "lab-microscopes"), ("G3", "lab-papers"), ("C4", "random")]), at);
    assert_eq!(cursor_row(&d), ">   . #lab-papers");
}


/// The project's channel is missing from an older computer's one list: its words lead the head, as for any workspace, and
/// the head's second sentence does not say again what the Slack app is.
#[test]
fn an_older_computers_list_says_when_the_projects_channel_is_missing_from_it() {
    let (mut d, at) = channel(&[]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    older(&mut d, at);
    d.on_evt(&host(), channels_reply(false), at);
    let head = said(&d);
    assert!(head.starts_with("This project's channel, CH1, is not in this list: your Slack app (the bot that reads the channel and posts for your agents) is not in it"), "{head}");
    assert!(head.contains("Your Slack app reads only the channels it is in.") && !head.contains("Your Slack app (the bot"), "{head}");
}

/// An older computer's one workspace: a refused list is asked again when the workspace is opened again, by Right or by Enter,
/// and Right on it while open asks nothing.
#[test]
fn an_older_computers_failed_list_is_asked_again_when_its_workspace_is_opened_again() {
    let (mut d, at, _) = unmapped();
    d.on_evt(&host(), ChatEvt::Channels { workspace: None, reply: ChatReply::Refused(ChatCode::Unavailable) }, at);
    assert!(d.picker_open().is_empty(), "Right on the open workspace asks nothing");
    let opens: [fn(&mut ChatDrawer) -> Vec<Out>; 2] = [|d| d.picker_open(), |d| d.picker_enter()];
    for open in opens {
        d.picker_close();
        assert_eq!(tree(&d), ["> > your connected Slack workspace", "  . Connect Slack again · connect again with new Slack codes (same workspace)"]);
        assert_eq!(asked(&open(&mut d)), [None]);
        assert_eq!(tree(&d)[..2], ["> v your connected Slack workspace", "    . reading its channels…"]);
        d.on_evt(&host(), ChatEvt::Channels { workspace: None, reply: ChatReply::Refused(ChatCode::Unavailable) }, at);
        assert!(said(&d).starts_with("could not list the channels: the chat is not available here"), "{}", said(&d));
    }
}

/// Right and Left do nothing on a row that is not theirs: Right on an open workspace whose list failed, on a note, on
/// `No channel for this project` and on Connect, Left on those last two.
#[test]
fn right_and_left_do_nothing_on_rows_that_are_not_theirs() {
    let (mut d, at) = known_two();
    d.on_evt(&host(), ChatEvt::Channels { workspace: Some("TAAAA1111".into()), reply: ChatReply::Refused(ChatCode::Unavailable) }, at);
    let failed = tree(&d);
    assert!(d.picker_open().is_empty(), "an open workspace whose list failed is not asked again by Right");
    assert_eq!(tree(&d), failed);
    d.picker_step(true);
    assert_eq!(cursor_row(&d), ">   . could not list its channels");
    let on_note = tree(&d);
    assert!(d.picker_open().is_empty());
    assert_eq!(tree(&d), on_note, "Right on a note");
    // The project's rows, with the project's workspace open and then closed.
    let (mut d, at) = channel(&[]);
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    d.on_evt(&host(), workspaces("WS1", &[("WS1", Some("Alpha Lab"), true), ("WS2", Some("Beta Lab"), true)]), at);
    d.on_evt(&host(), listed_in("WS1", false), at);
    (0..5).for_each(|_| d.picker_step(true));
    assert_eq!(cursor_row(&d), "> . No channel for this project");
    let before = tree(&d);
    d.picker_close();
    assert!(d.picker_open().is_empty());
    assert_eq!(tree(&d), before, "Left and Right on the unpairing");
    d.picker_step(true);
    let before = tree(&d);
    d.picker_close();
    assert!(d.picker_open().is_empty());
    assert_eq!(tree(&d), before, "Left and Right on Connect");
    (0..7).for_each(|_| d.picker_step(false));
    d.picker_close();
    assert_eq!(cursor_row(&d), "> > Alpha Lab");
    (0..2).for_each(|_| d.picker_step(true));
    assert_eq!(cursor_row(&d), "> . No channel for this project");
    let before = tree(&d);
    assert!(d.picker_open().is_empty());
    assert_eq!(tree(&d), before, "Right on the unpairing leaves the project's closed workspace closed");
}
