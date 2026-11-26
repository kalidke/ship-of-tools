use std::time::{Duration, Instant};

use cosmic_text::FontSystem;
use sot_protocol::chat::{slack_app_manifest, slack_app_url};
use sot_protocol::ops::{ChatChannelsRes, ChatCode, ChatConnectRes, ChatOtherApp, ChatUnconfigured};

use super::compose::Edit;
use super::drawer::never_written;
use super::view::Step;
use super::connect::{Asks, Connect, Screen};
use super::drawer::ChatDrawer;
use super::owner::Out;
use super::row_channel_tests::answer;
use super::thread_tests::{body, channel, shown, top};
use crate::net::dial::HostKey;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use crate::ui::input::keybindings::{key_name_capitals, Action};

const LOGIN: &str = "lab-user";
const NAME: &str = "Ship of Tools (lab-user)";
const ENTER: Option<Action> = Some(Action::ChatSend);
const ESC: Option<Action> = Some(Action::ReturnNav);
const LEFT: Option<Action> = Some(Action::ChatThreadClose);
const RIGHT: Option<Action> = Some(Action::ChatThreadOpen);
const PASTE: Option<Action> = Some(Action::Paste);
const COPY: Option<Action> = Some(Action::ChatCopySelection);

fn host() -> HostKey {
    "a".to_string()
}

/// Fixture tokens, built at run time so no token literal is in the source; the tail after the prefix is letters that no
/// screen's words hold in a row.
fn bot() -> String {
    format!("{}{}", "xoxb-", "QJX".repeat(16))
}

fn user() -> String {
    format!("{}{}", "xoxp-", "HNZ".repeat(16))
}

fn app() -> String {
    format!("{}{}", "xapp-", "VKW".repeat(16))
}

/// Whether `text` holds any two letters of a fixture token's tail; names the place, never the text.
fn leaks(text: &str) -> bool {
    ["QJ", "JX", "XQ", "VK", "KW", "WV", "HN", "NZ", "ZH"].iter().any(|pair| text.contains(pair))
}

fn unconfigured(reason: &str) -> ChatEvt {
    answer("ROW1", serde_json::json!({ "state": "unconfigured", "reason": reason }))
}

fn resolves(outs: &[Out]) -> usize {
    outs.iter().filter(|(_, r)| matches!(r, OutgoingReq::ChatResolve { .. })).count()
}

/// How many times the picker opened and asked which workspaces run, the first of its reads.
fn lists(outs: &[Out]) -> usize {
    outs.iter().filter(|(_, r)| matches!(r, OutgoingReq::ChatWorkspaces)).count()
}

/// The connects among `outs` that do not say to replace the Slack app connected now, by their tokens.
fn connects(outs: &[Out]) -> Vec<(String, String, String)> {
    connects_saying(outs, false)
}

/// The connects among `outs` that say to replace the Slack app connected now, by their tokens.
fn replacing(outs: &[Out]) -> Vec<(String, String, String)> {
    connects_saying(outs, true)
}

/// The connects among `outs` whose `replace_app` is `replace`, by their tokens.
fn connects_saying(outs: &[Out], replace: bool) -> Vec<(String, String, String)> {
    outs.iter()
        .filter_map(|(_, r)| match r {
            OutgoingReq::ChatConnect { bot_token, user_token, app_token, replace_app } if *replace_app == replace => {
                Some((bot_token.expose().to_string(), user_token.expose().to_string(), app_token.expose().to_string()))
            }
            _ => None,
        })
        .collect()
}

fn connected(workspace: &str) -> ChatEvt {
    ChatEvt::Connect { reply: ChatReply::Done(ChatConnectRes { workspace: workspace.into(), name: None }) }
}

fn refused(code: ChatCode) -> ChatEvt {
    ChatEvt::Connect { reply: ChatReply::Refused(code) }
}

/// A drawer whose shown row ROW1 answered `unconfigured` for `reason`, with the step begun, and the instant after.
fn in_step(reason: &str) -> (ChatDrawer, Instant) {
    in_step_as(LOGIN, reason)
}

/// `in_step` for a person who signs in as `login`.
fn in_step_as(login: &'static str, reason: &str) -> (ChatDrawer, Instant) {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.set_login(login);
    d.on_connection(&host(), true, false, t0);
    d.tick(shown("ROW1"), t0);
    assert_eq!(resolves(&d.tick(shown("ROW1"), t0 + Duration::from_millis(200)).0), 1);
    d.on_evt(&host(), unconfigured(reason), t0 + Duration::from_millis(210));
    d.tick(shown("ROW1"), t0 + Duration::from_millis(220));
    assert!(d.in_setup(), "unconfigured begins the step");
    (d, t0 + Duration::from_millis(230))
}

fn key(d: &mut ChatDrawer, action: Option<Action>) -> (Vec<Out>, Asks) {
    d.setup_key(action, None)
}

fn paste(d: &mut ChatDrawer, text: &str) -> (Vec<Out>, Asks) {
    d.setup_key(PASTE, Some(text))
}

/// Everything the drawer hands the chrome to draw at a drawer of `width` x `height` cells: the title, the body's rows and
/// any compose rows.
fn drawn_at(d: &ChatDrawer, width: usize, height: usize) -> String {
    let mut all = vec![d.title()];
    all.extend(d.body().map(|b| b.lines(width, height)).unwrap_or_default());
    all.extend(d.compose_text(true).map(|c| super::compose::compose_rows(&c, width, 10).join("\n")));
    all.join("\n")
}

fn drawn(d: &ChatDrawer) -> String {
    drawn_at(d, 400, 60)
}

fn step(reason: ChatUnconfigured) -> Connect {
    Connect::new(host(), reason, LOGIN, true)
}

/// A mapped row ROW1 with a person's post in flight, and that post's request.
fn post_in_flight() -> (ChatDrawer, Instant, sot_protocol::ops::ChatPostReq) {
    let (mut d, at) = channel(&[top(1)]);
    d.set_login(LOGIN);
    let r = shown("ROW1");
    d.edit(r.as_ref(), Edit::Text("hello")).unwrap();
    let req = match d.send(r.as_ref(), Some(LOGIN), at).unwrap().as_slice() {
        [(_, OutgoingReq::ChatPost { req })] => req.clone(),
        other => panic!("one chat.post expected, got {other:?}"),
    };
    (d, at, req)
}

/// The hub's answer that it holds no token that posts as the person.
fn no_user_token() -> ChatReply<sot_protocol::ops::ChatPostRes> {
    ChatReply::Done(sot_protocol::ops::ChatPostRes::Refused { code: ChatCode::NoUserToken })
}

/// A mapped row whose person's post the hub refused `no_user_token`: the Connect step it brings, and the instant after.
fn after_a_refused_post() -> (ChatDrawer, Instant) {
    let (mut d, at, req) = post_in_flight();
    d.on_evt(&host(), ChatEvt::Posted { req, reply: no_user_token() }, at);
    (d, at)
}

/// The hub's `chat.workspaces`: `WS1` runs, connected to the Slack app `A0NEW` whose bot Slack calls `app`.
fn connected_app(app: &str) -> ChatReply<sot_protocol::ops::ChatWorkspacesRes> {
    ChatReply::Done(serde_json::from_value(serde_json::json!({ "default": "WS1", "workspaces": [{ "id": "WS1", "name": "Alpha Lab", "running": true, "app_name": app, "app_id": "A0NEW" }] })).unwrap())
}

/// CHAT-CONNECT-MAKES-A-NEW-APP: a person's post refused because the hub holds no code that posts as them asks which Slack
/// app the workspace is connected to. Until the hub answers, and from a hub that gives no app, the screen opens the
/// person's list of Slack apps and says to look there for the app connected before, whatever its name, before making a new
/// one, which would be in none of the private channels. Once the hub gives the app, the screen says first to keep it, with
/// the name Slack calls it, and Enter opens that app's own page; the why follows, and Left makes a new app only if it is
/// gone. In a panel 60 cells by 12 the action is in view at once.
#[test]
fn the_post_as_you_screen_names_the_connected_slack_app_to_keep() {
    let (mut d, at, req) = post_in_flight();
    d.set_login("");
    let outs = d.on_evt(&host(), ChatEvt::Posted { req, reply: no_user_token() }, at);
    assert!(outs.iter().any(|(h, r)| h == &host() && matches!(r, OutgoingReq::ChatWorkspaces)), "the step asks which Slack app is connected: {outs:?}");
    assert_eq!(d.title(), " chat · post as you ");
    let warning = "If Ship of Tools is not in the list, look there for the Slack app you connected this workspace with before, whatever its name, and click that one. Press Left to make a new one (step 1) only if there is none: a new Slack app is in none of your private channels.";
    let before = drawn(&d).replace('\n', " ");
    assert!(before.contains("Press Enter: your list of Slack apps opens in your browser. In it, click Ship of Tools, your Slack app (the bot that reads the channel and posts for your agents), and copy its codes again, as steps 2 and 3 show (they follow here)."), "{before}");
    assert!(before.contains(warning), "until the hub answers: {before}");
    assert!(before.ends_with("Enter  open your Slack apps    Left  make a new Slack app    Esc  close"), "{before}");
    d.on_evt(&host(), ChatEvt::Workspaces { reply: connected_app(NAME) }, at);
    let body = drawn(&d).replace('\n', " ");
    let keep = format!("Keep your Slack app (the bot that reads the channel and posts for your agents), the one a is connected to now (Slack calls it {NAME}). Press Enter to open its page in Slack, add the permissions to post as you and install it again (step 2 shows how), then paste its codes here. Do not make a new Slack app: it would be in none of your private channels. Press Left to make one only if that Slack app is gone.");
    assert!(body.contains(&format!("{keep}  Why: Ship of Tools posts your messages in Slack as you, and a holds no Slack code that works for that: a was connected to Slack before")), "{body}");
    assert!(!body.contains("click Ship of Tools"), "the name a new app would have is not offered: {body}");
    assert!(body.ends_with("Enter  open your Slack app's page    Left  make a new Slack app    Esc  close"), "{body}");
    let rows = d.body().unwrap().lines(60, 12);
    assert!(rows[0].starts_with("Keep your Slack app") && rows[..6].join(" ").contains("Press Enter to open its page in Slack"), "{rows:#?}");
    // Enter opens that app's own permissions page, then step 2.
    let (_, asks) = key(&mut d, ENTER);
    assert_eq!(asks.open.as_deref(), Some("https://api.slack.com/apps/A0NEW/oauth"));
    assert_eq!(d.title(), " chat · set up Slack · step 2 of 3: the first two Slack codes ");
    // An app whose bot Slack names nothing is kept by its page alone.
    let mut c = Connect::for_post(host(), LOGIN, "WS1".into());
    c.on_workspaces(&ChatReply::Done(serde_json::from_value(serde_json::json!({ "default": "WS1", "workspaces": [{ "id": "WS1", "running": true, "app_id": "A0NEW" }] })).unwrap()));
    assert!(c.body().head.starts_with("Keep your Slack app (the bot that reads the channel and posts for your agents), the one a is connected to now. Press Enter to open its page in Slack"), "{}", c.body().head);
    // Another workspace's entry, one that does not run, and an older hub give no app: the list, with the warning.
    for reply in [
        ChatReply::Done(serde_json::from_value(serde_json::json!({ "default": "WS2", "workspaces": [{ "id": "WS2", "running": true, "app_name": NAME, "app_id": "A0NEW" }] })).unwrap()),
        ChatReply::Done(serde_json::from_value(serde_json::json!({ "default": "WS1", "workspaces": [{ "id": "WS1", "running": false, "app_name": NAME, "app_id": "A0NEW" }] })).unwrap()),
        ChatReply::Unsupported,
    ] {
        let mut c = Connect::for_post(host(), LOGIN, "WS1".into());
        c.on_workspaces(&reply);
        let head = c.body().head;
        assert!(head.contains(&format!("In it, click {NAME}, your Slack app")) && head.contains("a new Slack app is in none of your private channels."), "{reply:?}: {head}");
        assert_eq!(c.key(ENTER, None, false).1.open.as_deref(), Some("https://api.slack.com/apps"), "{reply:?}");
    }
}

/// The hub's `other_app` refusal: the tokens are for another Slack app, which Slack calls `Ship of Tools (lab-user)`, than
/// the one `Alpha Lab` is connected to now, `Alpha Lab chat`.
fn other_app() -> ChatOtherApp {
    ChatOtherApp {
        workspace: Some("TAAAA1".into()),
        name: Some("Alpha Lab".into()),
        app_id: Some("A0OLD".into()),
        app_name: Some("Alpha Lab chat".into()),
        new_app_known: true,
        new_app_name: Some(NAME.into()),
    }
}

/// The hub's `other_app` refusal when Slack did not say which Slack app the tokens are for.
fn untold() -> ChatOtherApp {
    ChatOtherApp { new_app_known: false, new_app_name: None, ..other_app() }
}

/// The new Slack app's tokens, as `asked_with` pastes them, and the connected app's, which differ in their tails.
fn connected_bot() -> String {
    bot().replace("QJX", "QJQ")
}

fn connected_user() -> String {
    user().replace("HNZ", "HNH")
}

fn connected_app_token() -> String {
    app().replace("VKW", "VKV")
}

/// A step whose three tokens the hub refused `other_app` with `asked`.
fn asked_with(asked: ChatOtherApp) -> Connect {
    let mut c = step(ChatUnconfigured::NotConnected);
    for t in [bot(), user(), app()] {
        c.key(PASTE, Some(&t), false);
    }
    assert_eq!(c.screen(), &Screen::Connecting);
    c.asked(asked);
    c
}

/// The label of `chat.thread_open`'s default key, as the screen names it (on a Mac too).
fn right_label() -> String {
    crate::ui::input::keybindings::KeyBindings::defaults().first_label(Action::ChatThreadOpen)
}

/// The rows of `c`'s screen above its pinned keys, at a panel 60 cells by 12, joined; the keys, which begin `first`, must
/// be in view.
fn above(c: &Connect, first: &str) -> String {
    let rows = c.body().lines(60, 12);
    let pin_at = rows.iter().position(|r| r.starts_with(first)).unwrap_or_else(|| panic!("the keys are not pinned in view: {rows:#?}"));
    rows[..pin_at].join(" ")
}

/// `above` for the other Slack app's screen, whose keys begin with Enter.
fn above_the_keys(c: &Connect) -> String {
    above(c, "Enter  ")
}

/// CHAT-CONNECT-MAKES-A-NEW-APP: tokens of another Slack app than the one the workspace is connected to now bring one
/// screen, whose first rows, in a panel 60 cells by 12, say whose tokens these are (the app connected now named), how to
/// keep everything, and that the new app is in no channel, so each project's channel shows nothing until someone invites
/// it there; the keys say Enter keeps the app connected now and `chat.thread_open`'s key, by its binding's label glossed
/// as the flow names it, connects the new one anyway. A name the hub gave none of is left out.
#[test]
fn the_other_slack_apps_screen_says_whose_codes_these_are_and_how_to_keep_everything() {
    let c = asked_with(other_app());
    assert_eq!(c.title(), " chat · a different Slack app ");
    let b = c.body();
    let first = "These Slack codes belong to a different Slack app than Alpha Lab chat, the Slack app connected to Alpha Lab now. To keep everything as it is, press Enter and paste the codes of Alpha Lab chat instead. The new Slack app is in no channel yet, so if you connect it, each project's channel shows nothing until someone invites the new Slack app to it in Slack; the next screen names each channel and what to type.";
    let right = right_label();
    let then = format!("Enter opens the page of Alpha Lab chat in your browser, where its codes are; steps 2 and 3 show which (they follow here). To connect the new Slack app anyway, press {right} (the Right arrow key).");
    assert_eq!(b.head, format!("{first}\n\n{then}"));
    assert_eq!(b.pin, format!("Enter  keep the Slack app connected now\n{right} (Right)  connect the new Slack app anyway    Esc  close"));
    let rows = b.lines(60, 12);
    assert_eq!(rows[10..], [String::from("Enter  keep the Slack app connected now"), format!("{right} (Right)  connect the new Slack app anyway    Esc  close")], "each key whole on its row");
    let above = above_the_keys(&c);
    for said in ["These Slack codes belong to a different Slack app than Alpha Lab chat, the Slack app connected to Alpha Lab now.", "To keep everything as it is, press Enter and paste the codes of Alpha Lab chat instead.", "The new Slack app is in no channel yet", "shows nothing until someone invites the new Slack app to it in Slack; the next screen names each channel and what to type."] {
        assert!(above.contains(said), "{said:?} is not in the first rows at 60 by 12: {above}");
    }
    // Nothing named but the workspace: the words leave the names out.
    let c = asked_with(ChatOtherApp { workspace: Some("TAAAA1".into()), new_app_known: true, ..ChatOtherApp::default() });
    let head = c.body().head;
    assert!(head.starts_with("These Slack codes belong to a different Slack app than the Slack app connected to workspace TAAAA1 now. To keep"), "{head}");
    assert!(head.contains("press Enter and paste the codes of the Slack app connected now instead."), "{head}");
    assert!(head.contains("\n\nEnter opens your list of Slack apps in your browser: click the one connected now there"), "no id: {head}");
    let c = asked_with(ChatOtherApp { new_app_known: true, ..ChatOtherApp::default() });
    assert!(c.body().head.starts_with("These Slack codes belong to a different Slack app than the Slack app connected to this Slack workspace now."), "{}", c.body().head);
    // An app with a page and no name: Enter's page is the Slack app connected now's.
    let c = asked_with(ChatOtherApp { app_name: None, ..other_app() });
    assert!(c.body().head.contains("\n\nEnter opens the page of the Slack app connected now in your browser, where its codes are;"), "{}", c.body().head);
}

/// Neither question screen names a Slack app to invite, whatever names the hub gave: this flow names every app it makes
/// alike, so a name here could be the connected app's. The screen after the connect names the app and each channel.
#[test]
fn the_question_screens_name_no_slack_app_to_invite() {
    for asked in [other_app(), untold()] {
        for (app_name, new_app_name) in [(Some(NAME), Some("Lab bot")), (Some("Alpha Lab chat"), None), (None, Some(NAME)), (None, None)] {
            let c = asked_with(ChatOtherApp { app_name: app_name.map(Into::into), new_app_name: new_app_name.map(Into::into), ..asked.clone() });
            let head = c.body().head;
            assert!(!head.contains("/invite"), "{app_name:?} {new_app_name:?}: the question names an app to invite: {head}");
            assert!(above_the_keys(&c).contains("until someone invites the new Slack app to it in Slack"), "{app_name:?} {new_app_name:?}");
        }
    }
}

/// When Slack did not say which Slack app the tokens are for, the screen says so truly, names the app connected now, and
/// offers to ask again: Enter sends the same three with no word to replace, nothing dropped and nothing remembered, and
/// `chat.thread_open`'s key connects these codes anyway, which the screen offers for when it comes back and the codes
/// were copied from the connected app's own page. In a panel 60 cells by 12 all of it is above the keys. No step
/// then names the connected app as the one whose codes to paste: a code Slack refuses goes back to its step as in any
/// connect.
#[test]
fn the_screen_says_when_ship_of_tools_could_not_tell_whose_codes_these_are() {
    let c = asked_with(untold());
    assert_eq!(c.title(), " chat · a different Slack app ");
    let b = c.body();
    let right = right_label();
    let first = format!("Ship of Tools could not tell whether these Slack codes belong to Alpha Lab chat, the Slack app connected to Alpha Lab now: Slack did not say. Press Enter to ask Slack again. If this screen comes back and you copied the codes from that Slack app's own page, press {right} (the Right arrow key) to connect them; if they are a new Slack app's, it is in no channel yet, so each project's channel shows nothing until someone invites the new Slack app to it in Slack; the next screen names each channel and what to type.");
    assert_eq!(b.head, first);
    assert_eq!(b.pin, format!("Enter  ask Slack again\n{right} (Right)  connect these codes anyway    Esc  close"));
    let rows = b.lines(60, 12);
    assert_eq!(rows[10..], [String::from("Enter  ask Slack again"), format!("{right} (Right)  connect these codes anyway    Esc  close")], "each key whole on its row");
    assert_eq!(above_the_keys(&c).split_whitespace().collect::<Vec<_>>(), first.split_whitespace().collect::<Vec<_>>(), "the whole screen is above the keys at 60 by 12");
    // A bare refusal is the weaker claim: an app not told, nothing named.
    assert!(asked_with(ChatOtherApp::default()).body().head.starts_with("Ship of Tools could not tell whether these Slack codes belong to the Slack app connected to this Slack workspace now: Slack did not say. Press Enter to ask Slack again. If this screen comes back and you copied the codes from that Slack app's own page, press "));
    // The other key connects the held three, saying to replace.
    let mut c = asked_with(untold());
    assert!(replacing(&c.key(RIGHT, None, false).0.into_iter().collect::<Vec<_>>()) == [(bot(), user(), app())], "the other key sent no replacing connect of the three held");
    // Enter asks Slack again: the same three, no word to replace, once; an earlier connect unanswered waits.
    let mut c = asked_with(untold());
    let (out, asks) = c.key(ENTER, None, true);
    assert!(out.is_none() && asks.notice.is_some_and(|n| n.starts_with("Slack is still checking")));
    assert_eq!(c.screen(), &Screen::OtherApp);
    let (out, asks) = c.key(ENTER, None, false);
    assert!(connects(&out.into_iter().collect::<Vec<_>>()) == [(bot(), user(), app())], "Enter did not ask again with the same three codes");
    assert!(asks.open.is_none(), "asking again opened a page");
    assert_eq!(c.screen(), &Screen::Connecting);
    assert!(c.held() == (Some(bot().as_str()), Some(user().as_str()), Some(app().as_str())), "asking again dropped a code");
    for k in [ENTER, RIGHT] {
        assert!(c.key(k, None, false).0.is_none(), "{k:?} sent a second connect");
    }
    // Slack still does not say: the same screen. Then a code Slack refuses: its step, which names the login's app, not the
    // connected one, and takes any code, the ones asked about too.
    c.asked(untold());
    assert_eq!(c.screen(), &Screen::OtherApp);
    c.key(ENTER, None, false);
    c.answered(ChatReply::Refused(ChatCode::BotTokenRefused));
    assert_eq!(c.screen(), &Screen::Tokens);
    assert!(c.body().head.starts_with("Slack did not accept the code you pasted.") && !c.body().head.contains("connected now"), "{}", c.body().head);
    let (out, asks) = c.key(PASTE, Some(&bot()), false);
    assert!(asks.notice.is_none(), "a paste of a code asked about was refused");
    assert!(connects(&out.into_iter().collect::<Vec<_>>()) == [(bot(), user(), app())]);
    // Another code pasted over the screen takes its place among the three, which are sent again with no word to replace.
    let mut c = asked_with(untold());
    let (out, _) = c.key(PASTE, Some(&connected_app_token()), false);
    assert!(connects(&out.into_iter().collect::<Vec<_>>()) == [(bot(), user(), connected_app_token())], "a paste over the screen did not send the three again");
}

/// The other Slack app's screen's keys. Enter, the key a hurried person presses, replaces nothing: it opens the page of
/// the app connected now and goes to step 2 with the three held tokens dropped (`not pasted yet`, which is true), and
/// steps 2 and 3 name the app connected now as the one to click and copy from; the connected app's three then go in one
/// connect, which does not say to replace. `chat.thread_open`'s key sends one connect of the three held, saying to replace,
/// and nothing more; a token of the app connected now pasted over the screen keeps that app as Enter does; Escape closes;
/// the hub's word for a step that sent nothing changes nothing.
#[test]
fn the_other_slack_apps_keys_keep_the_codes_or_connect_anyway_once() {
    let mut c = asked_with(other_app());
    let (out, asks) = c.key(ENTER, None, false);
    assert!(out.is_none(), "Enter sent a connect");
    assert_eq!(asks.open.as_deref(), Some("https://api.slack.com/apps/A0OLD/oauth"), "Enter opens the connected app's page");
    assert_eq!(c.screen(), &Screen::Tokens);
    assert!(c.held() == (None, None, None), "the new app's codes are not dropped");
    let head = c.body().head;
    assert!(head.contains("(Not on screen? At api.slack.com/apps, click Alpha Lab chat, the Slack app connected now, then OAuth & Permissions in the menu on the left.)"), "{head}");
    assert!(!head.contains(&format!("click {NAME}")), "step 2 names the login's app, which is the new one's name here: {head}");
    assert!(head.contains("User OAuth Token       not pasted yet\nBot User OAuth Token   not pasted yet"), "{head}");
    assert!(c.key(PASTE, Some(&connected_user()), false).0.is_none(), "one token of the app connected now sent a connect");
    assert!(c.key(PASTE, Some(&connected_bot()), false).0.is_none(), "two tokens of the app connected now sent a connect");
    assert_eq!(c.screen(), &Screen::App);
    let head = c.body().head;
    assert!(head.contains("App-Level Token   not pasted yet"), "{head}");
    assert!(head.contains("Copy it from Alpha Lab chat, the same Slack app as the two codes of step 2. On Slack's page:"), "step 3 does not name the app to copy from: {head}");
    let (out, _) = c.key(PASTE, Some(&connected_app_token()), false);
    assert!(connects(&out.into_iter().collect::<Vec<_>>()) == [(connected_bot(), connected_user(), connected_app_token())], "the third did not send one connect of the app connected now");
    // With no name from the hub, steps 2 and 3 name no app.
    let mut c = asked_with(ChatOtherApp { app_name: None, ..other_app() });
    c.key(ENTER, None, false);
    assert!(c.body().head.contains("(Not on screen? At api.slack.com/apps, click the Slack app connected now, then OAuth & Permissions"), "{}", c.body().head);
    c.key(PASTE, Some(&connected_user()), false);
    c.key(PASTE, Some(&connected_bot()), false);
    assert!(c.body().head.contains("Copy it from the same Slack app as the two codes of step 2. On Slack's page:"), "{}", c.body().head);

    // Connect anyway: one connect of the three held, saying to replace; then nothing, whatever is pressed.
    let mut c = asked_with(other_app());
    let (out, _) = c.key(RIGHT, None, false);
    assert!(replacing(&out.into_iter().collect::<Vec<_>>()) == [(bot(), user(), app())], "the other key sent no one connect of the three held, saying to replace");
    assert_eq!(c.screen(), &Screen::Connecting);
    for k in [RIGHT, ENTER, RIGHT] {
        assert!(c.key(k, None, false).0.is_none(), "{k:?} sent a second connect");
    }
    // A token Slack then refuses goes back to its step, which names the login's app again and takes the new app's codes.
    c.answered(ChatReply::Refused(ChatCode::AppTokenRefused));
    assert_eq!(c.screen(), &Screen::App);
    assert!(connects(&c.key(PASTE, Some(&app()), false).0.into_iter().collect::<Vec<_>>()) == [(bot(), user(), app())], "once chosen, the new app's codes are not sent as its own");
    // Then today's flow: the hub's reply is the connected screen.
    c.answered(ChatReply::Done(ChatConnectRes { workspace: "TAAAA1".into(), name: Some("Alpha Lab".into()) }));
    assert!(matches!(c.screen(), Screen::Connected(..)), "{:?}", c.screen());
    // An earlier connect unanswered: anyway waits, as any connect does.
    let mut c = asked_with(other_app());
    let (out, asks) = c.key(RIGHT, None, true);
    assert!(out.is_none() && asks.notice.is_some_and(|n| n.starts_with("Slack is still checking")));
    assert_eq!(c.screen(), &Screen::OtherApp);

    // A token of the app connected now pasted over the screen keeps that app: the held ones go, the paste is held.
    let mut c = asked_with(other_app());
    assert!(c.key(PASTE, Some(&connected_bot()), false).0.is_none());
    assert!(c.held() == (Some(connected_bot().as_str()), None, None), "the held codes are not the paste alone");
    assert_eq!(c.screen(), &Screen::Tokens, "the step of the next code");
    // Left does nothing there, Escape closes.
    let mut c = asked_with(other_app());
    let (out, asks) = c.key(LEFT, None, false);
    assert!(out.is_none() && asks == Asks::default());
    assert_eq!(c.screen(), &Screen::OtherApp);
    assert!(c.key(ESC, None, false).1.close);
    // The hub's word for a step that is not connecting changes nothing.
    let mut c = step(ChatUnconfigured::NotConnected);
    c.key(PASTE, Some(&bot()), false);
    c.asked(other_app());
    assert_eq!(c.screen(), &Screen::Tokens);
}

/// A set the hub refused `mixed_apps` (its App-Level Token is another Slack app's than its bot token's; nothing was saved)
/// goes back to step 3 with that code dropped and the other two kept. Its first rows, in a panel 60 cells by 12, are the
/// wire's words and what to do, with the row of keys in view; a new App-Level Token then sends the three once more, and
/// the words go. After the other Slack app's screen named the app to keep, the step names it as the one to copy from.
#[test]
fn a_set_of_two_slack_apps_codes_goes_back_to_the_app_level_token_with_the_other_two_kept() {
    let wire = "the \"App-Level Token\" belongs to a different Slack app than the \"Bot User OAuth Token\", so nothing was changed; all three codes must come from one Slack app";
    assert_eq!(ChatCode::MixedApps.text(), wire);
    let mut c = step(ChatUnconfigured::NotConnected);
    for t in [bot(), user(), app()] {
        c.key(PASTE, Some(&t), false);
    }
    c.answered(ChatReply::Refused(ChatCode::MixedApps));
    assert_eq!((c.screen(), c.title().as_str()), (&Screen::App, " chat · set up Slack · step 3 of 3: the last Slack code "));
    assert!(c.held() == (Some(bot().as_str()), Some(user().as_str()), None), "the odd code is held, or one of the other two is not");
    let b = c.body();
    let lead = format!("Not connected: {wire}. Copy the \"App-Level Token\" again, from the same Slack app as the two codes of step 2; or, if the two codes of step 2 came from the wrong Slack app, press Left, paste the right two there, then paste this one again. On Slack's page:");
    assert!(b.head.starts_with(&format!("{lead}\n\n  1. In the menu on the left, click Basic Information.")), "{}", b.head);
    assert!(b.head.ends_with("App-Level Token   not pasted yet"), "{}", b.head);
    assert_eq!(b.pin, format!("{P}+v  paste    Left/Right  move between steps    Esc  close"));
    let keys = format!("{P}+v  paste");
    // The keys are in view whole, at the panel's foot (this step's wrap onto a second row where `Ctrl` is the chord).
    let rows = b.lines(60, 12);
    let pin_at = rows.iter().position(|r| r.starts_with(&keys)).unwrap_or_else(|| panic!("the keys are not in view at 60 by 12: {rows:#?}"));
    assert_eq!(rows[pin_at..].join(" ").split_whitespace().collect::<Vec<_>>(), b.pin.split_whitespace().collect::<Vec<_>>(), "the keys are cut at 60 by 12: {rows:#?}");
    let first = above(&c, &keys);
    for said in [format!("Not connected: {wire}."), "Copy the \"App-Level Token\" again, from the same Slack app as the two codes of step 2".to_string(), "press Left, paste the right two there, then paste this one again.".to_string()] {
        assert!(first.contains(&said), "{said:?} is not in the first rows at 60 by 12: {first}");
    }
    assert!(!leaks(&b.head), "the screen draws a held code");
    // Step 2 keeps its two, and a new App-Level Token sends the three once; the words then go.
    assert!(c.key(LEFT, None, false).0.is_none());
    assert!(c.body().head.contains("User OAuth Token       pasted: xoxp-…") && c.body().head.contains("Bot User OAuth Token   pasted: xoxb-…"), "{}", c.body().head);
    let (out, _) = c.key(PASTE, Some(&connected_app_token()), false);
    assert!(connects(&out.into_iter().collect::<Vec<_>>()) == [(bot(), user(), connected_app_token())], "the new App-Level Token did not send the three once");
    c.answered(ChatReply::Refused(ChatCode::AppTokenRefused));
    assert!(!c.body().head.contains("Not connected"), "the mixed set's words outlive its App-Level Token: {}", c.body().head);
    // After the other Slack app's screen named the app to keep: the reflex paste of the new app's App-Level Token beside the
    // connected app's two, refused by the hub, names that app.
    let mut c = asked_with(other_app());
    c.key(ENTER, None, false);
    for t in [app(), connected_user(), connected_bot()] {
        c.key(PASTE, Some(&t), false);
    }
    assert_eq!(c.screen(), &Screen::Connecting);
    c.answered(ChatReply::Refused(ChatCode::MixedApps));
    assert!(c.held() == (Some(connected_bot().as_str()), Some(connected_user().as_str()), None), "the connected app's two are not kept");
    assert!(c.body().head.contains("Copy the \"App-Level Token\" again, from Alpha Lab chat, the same Slack app as the two codes of step 2; or"), "{}", c.body().head);
}

/// Brings ROW1 back to mapped on WS1/CH1 from `t`: the row resolves again and is answered.
fn back_to_mapped(d: &mut ChatDrawer, t: Instant) -> Instant {
    d.tick(shown("ROW1"), t);
    d.tick(shown("ROW1"), t + Duration::from_millis(200));
    let mapped = serde_json::json!({ "state": "mapped", "workspace": "WS1", "channel": "CH1", "delivers": false });
    d.on_evt(&host(), answer("ROW1", mapped), t + Duration::from_millis(210));
    d.tick(shown("ROW1"), t + Duration::from_millis(220));
    t + Duration::from_millis(230)
}

/// The channel list's `Connect a Slack workspace`, chosen on a mapped ROW1.
fn connect_from_the_list(d: &mut ChatDrawer, at: Instant) {
    d.open_picker(shown("ROW1").as_ref()).unwrap();
    let res = serde_json::from_value(serde_json::json!({ "default": "WS1", "workspaces": [{ "id": "WS1", "running": false }] })).unwrap();
    d.on_evt(&host(), ChatEvt::Workspaces { reply: ChatReply::Done(res) }, at);
    assert!(d.picker_enter().is_empty() && d.in_setup(), "Connect a Slack workspace starts the step");
}

/// The window keeps no memory of the codes the hub asked about: the hub writes no set of two Slack apps' codes, so the
/// window sends what the person pasted and shows the hub's answer. Each way a person comes to paste the new Slack app's
/// App-Level Token (still on the clipboard) beside the connected app's two codes, a paste over the question screen, one
/// after Enter there, and one in a step opened again after Escape, after a change of session and after a lost link, sends
/// one connect of that set with no word to replace and no status-line refusal; the hub's `mixed_apps` then brings step 3
/// with the wire's words and the connected app's two kept, and with the step closed meanwhile the status line says them.
#[test]
fn every_way_to_a_mixed_set_sends_it_once_and_shows_the_hubs_refusal() {
    type Way = fn(&mut ChatDrawer, Instant) -> Instant;
    let reopened = |d: &mut ChatDrawer, at: Instant| {
        let at = back_to_mapped(d, at + Duration::from_millis(10));
        connect_from_the_list(d, at);
        at
    };
    let ways: [(&str, Way); 5] = [
        ("a paste over the question screen", |_, at| at),
        ("after Enter", |d, at| {
            key(d, ENTER);
            at
        }),
        ("after Escape", |d, at| {
            assert!(key(d, ESC).1.close);
            d.hide();
            at
        }),
        ("after a change of session", |d, at| {
            drop(d.tick(shown("ROW2"), at));
            at
        }),
        ("after a lost link", |d, at| {
            drop(d.on_connection(&host(), false, false, at));
            drop(d.on_connection(&host(), true, false, at + Duration::from_millis(1)));
            at
        }),
    ];
    for (n, (why, way)) in ways.into_iter().enumerate() {
        let (mut d, at) = channel(&[top(1)]);
        d.set_login(LOGIN);
        connect_from_the_list(&mut d, at);
        for t in [bot(), user(), app()] {
            paste(&mut d, &t);
        }
        d.on_evt(&host(), ChatEvt::OtherApp(other_app()), at);
        assert_eq!(d.title(), " chat · a different Slack app ", "{why}");
        let mut at = way(&mut d, at);
        if n >= 2 {
            assert!(!d.in_setup(), "{why}: the step did not end");
            at = reopened(&mut d, at);
        }
        let mut outs: Vec<Out> = Vec::new();
        for t in [app(), connected_user(), connected_bot()] {
            let (o, asks) = paste(&mut d, &t);
            assert!(asks.notice.is_none(), "{why}: the window refused a paste");
            outs.extend(o);
        }
        assert!(connects(&outs) == [(connected_bot(), connected_user(), app())], "{why}: not one connect of the pasted set");
        assert!(replacing(&outs).is_empty(), "{why}: a paste said to replace the Slack app connected now");
        // The hub wrote nothing and says which code is the odd one.
        d.on_evt(&host(), refused(ChatCode::MixedApps), at);
        assert_eq!(d.title(), " chat · set up Slack · step 3 of 3: the last Slack code ", "{why}");
        let seen = drawn(&d);
        assert!(seen.contains(&format!("Not connected: {}.", ChatCode::MixedApps.text())), "{why}: {seen}");
        assert!(seen.contains("App-Level Token   not pasted yet") && !leaks(&seen), "{why}: the odd code is still held, or a code is drawn");
        assert!(key(&mut d, LEFT).0.is_empty());
        let seen = drawn(&d);
        assert!(seen.contains("User OAuth Token       pasted: xoxp-…") && seen.contains("Bot User OAuth Token   pasted: xoxb-…"), "{why}: the other two are not kept: {seen}");
        assert_eq!(d.take_notice(), None, "{why}: a step that shows the refusal also wrote the status line");
    }
    // The step closed while the hub checked: the status line says the wire's words.
    let (mut d, at) = channel(&[top(1)]);
    d.set_login(LOGIN);
    connect_from_the_list(&mut d, at);
    for t in [connected_bot(), connected_user(), app()] {
        paste(&mut d, &t);
    }
    assert!(key(&mut d, ESC).1.close);
    d.hide();
    d.on_evt(&host(), refused(ChatCode::MixedApps), at);
    assert_eq!(d.take_notice(), Some(format!("Slack not connected: the \"App-Level Token\" belongs to a different Slack app than the \"Bot User OAuth Token\", so nothing was changed; all three codes must come from one Slack app. Open the channel list ({P}+Shift+k), choose Connect a Slack workspace and paste the three codes of one Slack app")));
}

/// The drawer hands the hub's `other_app` to the step that is connecting, and the step's next key sends again; with the
/// step closed while it connected, the status line says nothing changed. Nothing was saved, so the row is not resolved
/// again.
#[test]
fn the_drawer_gives_the_other_slack_app_to_its_connecting_step() {
    let (mut d, t) = in_step("not_connected");
    for token in [bot(), user(), app()] {
        paste(&mut d, &token);
    }
    let outs = d.on_evt(&host(), ChatEvt::OtherApp(other_app()), t);
    assert_eq!(resolves(&outs), 0);
    assert_eq!(resolves(&d.tick(shown("ROW1"), t + Duration::from_millis(10)).0), 0, "the row was resolved again");
    assert_eq!(d.title(), " chat · a different Slack app ");
    assert!(replacing(&key(&mut d, RIGHT).0) == [(bot(), user(), app())], "the connect was not freed for the next one");
    // Closed while it connected: the status line says why nothing changed.
    let (_, asks) = key(&mut d, ESC);
    assert!(asks.close);
    d.hide();
    d.on_evt(&host(), ChatEvt::OtherApp(other_app()), t);
    assert_eq!(d.take_notice(), Some(super::drawer::not_replaced_words()));
    assert_eq!(super::drawer::not_replaced_words(), format!("Slack not connected: those codes may belong to a different Slack app than the one connected now, and nothing changed. To decide, open the channel list ({P}+Shift+k), choose Connect a Slack workspace and paste the three codes again"));
}

#[test]
fn the_connect_step_says_each_reason_and_its_next_action() {
    for reason in ["not_connected", "start_failed"] {
        let (d, _) = in_step(reason);
        assert_eq!(d.title(), " chat · set up Slack ", "{reason}");
        let body = drawn(&d);
        assert!(body.contains("First, connect Ship of Tools to the Slack workspace the channel is in. You do this once for each Slack workspace you use"), "{reason}: the start screen");
        assert!(drawn_at(&d, 2000, 60).contains("Each person makes their own Slack app in each workspace they connect, even when a colleague already has one there: two people sharing one app each miss messages, because Slack splits the messages between them."), "{reason}: one app per person");
        assert!(body.contains("A Slack code you paste here is never posted. The messages you write in the chat itself, once it is set up, are posted under your own Slack name; what your agents post goes out as your Slack app."), "{reason}: whose posts are whose");
        assert!(body.contains("Enter  start    Right  I already made my Slack app: go to step 2    Esc  close"), "{reason}");
    }
    // The refused tokens' screen names the person's own app at its first frame.
    let (d, _) = in_step("tokens_refused");
    let body = drawn(&d);
    assert!(body.contains("Slack no longer accepts the Slack codes Ship of Tools holds. Press Enter: your list of Slack apps opens"));
    assert!(body.contains(&format!("If {NAME}, your Slack app (the bot that reads the channel and posts for your agents), is in it")), "screen 8 names the app with the login at once");
    assert!(body.contains("copy its three codes again (steps 2 and 3 follow here)"));
    assert!(body.contains("Enter  open your Slack apps    Left  make your Slack app again    Esc  close"));
    // A hub with no token that posts as the person: screen 9, with the same keys.
    let (d, _) = after_a_refused_post();
    assert_eq!(d.title(), " chat · post as you ");
    let body = drawn(&d).replace('\n', " ");
    assert!(body.contains("Why: Ship of Tools posts your messages in Slack as you, and a holds no Slack code that works for that: a was connected to Slack before Ship of Tools could post as you, or Slack stopped accepting the code it had."), "both causes are said");
    assert!(body.contains("Press Enter: your list of Slack apps opens in your browser."));
    assert!(body.contains(&format!("If {NAME} is not in the list, look there for the Slack app you connected this workspace with before, whatever its name, and click that one.")));
    assert!(body.contains("Enter  open your Slack apps    Left  make a new Slack app    Esc  close"));

    // Each step names itself and its keys.
    let mut c = step(ChatUnconfigured::NotConnected);
    c.key(ENTER, None, false);
    assert_eq!(c.title(), " chat · set up Slack · step 1 of 3: make your Slack app ");
    let make = c.body();
    assert!(make.head.contains(&format!("already named {NAME}, after the name you sign in to this computer with")));
    assert_eq!(make.pin, "Enter  next step    Left/Right  move between steps    Esc  close");
    c.key(ENTER, None, false);
    assert_eq!(c.title(), " chat · set up Slack · step 2 of 3: the first two Slack codes ");
    let two = c.body();
    assert!(two.head.contains("the \"User OAuth Token\", which starts with xoxp- and lets Ship of Tools post your messages as you, and the \"Bot User OAuth Token\", which starts with xoxb- and lets your Slack app (the bot that reads the channel and posts for your agents) read your channels and post in them"));
    assert!(two.head.contains("User OAuth Token       not pasted yet\nBot User OAuth Token   not pasted yet"));
    assert!(two.head.contains("No \"User OAuth Token\" on the page? Your Slack app was made before Ship of Tools posted as you."));
    assert!(two.head.contains("under User Token Scopes, click Add an OAuth Scope and choose chat:write (post as you), then again for files:write (post a file as you)"));
    c.key(RIGHT, None, false);
    assert_eq!(c.title(), " chat · set up Slack · step 3 of 3: the last Slack code ");
    assert!(c.body().head.contains("  4. Click Add Scope and choose connections:write (receive new messages as they arrive) from the list."));
    assert!(c.body().head.contains("App-Level Token   not pasted yet"));
    // No login names the app plainly, and says nothing of a login.
    let mut c = Connect::new(host(), ChatUnconfigured::NotConnected, "", true);
    c.key(ENTER, None, false);
    assert!(c.body().head.contains("already named Ship of Tools (you can change it there)"));
}

/// An app-level token of another app than the bot's: the hub ended the chat rather than take that app's messages, and the
/// first screen says so, names the person's app and takes the refused tokens' keys; step 3 says its token comes from the
/// same app as step 2's.
#[test]
fn the_wrong_app_screen_names_the_app_and_opens_the_apps() {
    let (mut d, _) = in_step("wrong_app");
    assert_eq!(d.title(), " chat · set up Slack ");
    let body = drawn(&d).replace('\n', " ");
    let words = format!("The chat stopped: the \"App-Level Token\" it holds belongs to another Slack app than your Slack app (the bot that reads the channel and posts for your agents), so the chat would have received that other app's messages. To fix it, paste your own Slack app's codes: press Enter, and your list of Slack apps opens in your browser. Click {NAME}, then copy its three Slack codes again, as steps 2 and 3 show (they follow here).");
    assert!(body.contains(&words), "the wrong app's words");
    assert!(body.contains("Enter  open your Slack apps    Left  make your Slack app again    Esc  close"));
    let (_, asks) = key(&mut d, ENTER);
    assert_eq!(asks.open.as_deref(), Some("https://api.slack.com/apps"));
    assert_eq!(d.title(), " chat · set up Slack · step 2 of 3: the first two Slack codes ", "the apps' list, then step 2");
    key(&mut d, RIGHT);
    assert!(drawn(&d).contains("Copy it from the same Slack app as the two codes of step 2."), "step 3 says the app");

    let mut c = step(ChatUnconfigured::WrongApp);
    let (_, asks) = c.key(LEFT, None, false);
    assert_eq!((asks.open, c.screen()), (Some(slack_app_url(NAME)), &Screen::Make), "Left makes the app again");
    assert_eq!(step(ChatUnconfigured::WrongApp).key(RIGHT, None, false).1, Asks::default(), "the screen has no Right");
}

#[test]
fn left_and_right_move_between_steps_and_the_refused_screen_opens_its_pages() {
    let mut c = step(ChatUnconfigured::NotConnected);
    // Right on the first screen goes straight to step 2 and opens nothing.
    assert_eq!(c.key(RIGHT, None, false).1, Asks::default());
    assert_eq!(c.screen(), &Screen::Tokens);
    c.key(LEFT, None, false);
    assert_eq!(c.screen(), &Screen::Make, "Left from step 2 is step 1, with no page opened again");
    c.key(LEFT, None, false);
    assert_eq!(c.screen(), &Screen::First);
    c.key(LEFT, None, false);
    assert_eq!(c.screen(), &Screen::First, "nothing before the first screen");

    let mut c = step(ChatUnconfigured::TokensRefused);
    let (_, asks) = c.key(ENTER, None, false);
    assert_eq!(asks.open.as_deref(), Some("https://api.slack.com/apps"));
    assert_eq!(c.screen(), &Screen::Tokens, "the apps' list, then step 2");
    let mut c = Connect::for_post(host(), LOGIN, "WS1".into());
    let (_, asks) = c.key(ENTER, None, false);
    assert_eq!((asks.open.as_deref(), c.screen()), (Some("https://api.slack.com/apps"), &Screen::Tokens), "screen 9: the apps' list, then step 2");
    let mut c = Connect::for_post(host(), LOGIN, "WS1".into());
    let (_, asks) = c.key(LEFT, None, false);
    assert_eq!((asks.open, c.screen()), (Some(slack_app_url(NAME)), &Screen::Make), "screen 9: make the app again");
    assert_eq!(Connect::for_post(host(), LOGIN, "WS1".into()).key(RIGHT, None, false).1, Asks::default(), "screen 9 has no Right");
    let mut c = step(ChatUnconfigured::TokensRefused);
    let (_, asks) = c.key(LEFT, None, false);
    assert_eq!(asks.open, Some(slack_app_url(NAME)));
    assert_eq!(c.screen(), &Screen::Make, "make the app again");
}

#[test]
fn a_pasted_token_is_never_drawn() {
    let (mut d, _) = in_step("not_connected");
    key(&mut d, RIGHT);
    paste(&mut d, &format!("  {}\n", bot()));
    assert!(!leaks(&drawn(&d)), "step 2 after the first paste draws a token byte past its prefix");
    let body = drawn(&d);
    assert!(body.contains(&format!("Bot User OAuth Token   pasted: xoxb-…, {} characters", bot().len())));
    paste(&mut d, &user());
    let body = drawn(&d);
    assert!(!leaks(&body), "step 3 after the second paste draws a token byte past its prefix");
    key(&mut d, LEFT);
    let body = drawn(&d);
    assert!(!leaks(&body), "step 2 draws a token byte past its prefix");
    assert!(body.contains(&format!("User OAuth Token       pasted: xoxp-…, {} characters", user().len())));
    paste(&mut d, &app());
    assert_eq!(d.title(), " chat · connecting… ");
    assert!(!leaks(&drawn(&d)), "connecting draws a token byte");
    d.on_evt(&host(), refused(ChatCode::AppTokenRefused), Instant::now());
    let body = drawn(&d);
    assert!(!leaks(&body), "the refused screen draws a token byte");
    assert!(body.contains("App-Level Token   not pasted yet"));
}

#[test]
fn a_paste_in_the_connect_step_never_reaches_the_draft() {
    // A draft begun while the row was mapped stays the row's; then the hub goes `unconfigured`.
    let (mut d, at) = channel(&[top(1)]);
    d.set_login(LOGIN);
    d.edit(shown("ROW1").as_ref(), Edit::Text("a")).unwrap();
    assert_eq!(d.draft(), Some(("a", "CH1")));
    d.on_connection(&host(), false, false, at);
    d.on_connection(&host(), true, false, at);
    assert_eq!(resolves(&d.tick(shown("ROW1"), at + Duration::from_millis(10)).0), 1);
    d.on_evt(&host(), unconfigured("not_connected"), at);
    d.tick(shown("ROW1"), at + Duration::from_millis(20));
    assert!(d.in_setup());
    // The one route a paste could reach the draft by refuses, so the draft Enter would post is as it was.
    assert_eq!(d.edit(shown("ROW1").as_ref(), Edit::Text(&bot())), Err("connect Slack first"));
    paste(&mut d, &bot());
    assert_eq!(d.draft(), Some(("a", "CH1")), "the draft holds no paste");
    assert!(d.compose_text(true).is_none(), "no compose row is drawn under the step");
}

#[test]
fn all_three_tokens_go_in_one_request_and_none_is_kept_once_connected() {
    let mut c = step(ChatUnconfigured::NotConnected);
    assert!(c.key(PASTE, Some(&bot()), false).0.is_none());
    assert!(c.key(PASTE, Some(&user()), false).0.is_none());
    let (out, _) = c.key(PASTE, Some(&app()), false);
    assert_eq!(connects(&[out.expect("all three tokens held: one connect")]), [(bot(), user(), app())], "one request carries them all");
    // While Slack checks them the step keeps all three, so a refused one can send the person back with the others kept (5c).
    assert_eq!(c.held(), (Some(bot().as_str()), Some(user().as_str()), Some(app().as_str())));
    for k in [ENTER, PASTE, RIGHT] {
        assert!(c.key(k, Some(&app()), false).0.is_none(), "{k:?} while connecting");
    }
    c.answered(ChatReply::Done(ChatConnectRes { workspace: "T1".into(), name: None }));
    assert_eq!(c.screen(), &Screen::Connected("T1".into(), None));
    assert_eq!(c.held(), (None, None, None), "a connected step keeps no token");
}

/// Step 2 takes the user token and the bot token in either order, moving to step 3 once both are held, and one connect goes
/// out when the app-level token follows; every cell drawn after each paste holds no byte past a prefix.
#[test]
fn step_two_takes_the_user_and_bot_tokens_in_either_order_and_one_connect_carries_all_three() {
    for order in [[user(), bot()], [bot(), user()]] {
        let (mut d, _) = in_step("not_connected");
        key(&mut d, RIGHT);
        assert_eq!(d.title(), " chat · set up Slack · step 2 of 3: the first two Slack codes ");
        let mut sent = Vec::new();
        for (n, token) in order.iter().enumerate() {
            sent.extend(connects(&paste(&mut d, token).0));
            assert!(!leaks(&drawn(&d)), "a cell holds a token byte after paste {n}");
            let want = if n == 0 { " chat · set up Slack · step 2 of 3: the first two Slack codes " } else { " chat · set up Slack · step 3 of 3: the last Slack code " };
            assert_eq!(d.title(), want, "after paste {n}");
        }
        assert!(sent.is_empty(), "no connect before the app-level token");
        sent.extend(connects(&paste(&mut d, &app()).0));
        assert_eq!(sent, [(bot(), user(), app())], "one request, with each token in its own place");
        assert!(!leaks(&drawn(&d)));
    }
    // The app-level token first: the step stays at step 2 until its two tokens are held, then the last waits for nothing.
    let (mut d, _) = in_step("not_connected");
    key(&mut d, RIGHT);
    assert!(paste(&mut d, &app()).0.is_empty());
    assert_eq!(d.title(), " chat · set up Slack · step 2 of 3: the first two Slack codes ");
    assert!(paste(&mut d, &bot()).0.is_empty());
    assert_eq!(connects(&paste(&mut d, &user()).0), [(bot(), user(), app())]);
}

#[test]
fn a_paste_that_is_no_slack_token_is_dropped_unshown() {
    let mut c = step(ChatUnconfigured::NotConnected);
    c.key(RIGHT, None, false);
    let before = c.body().lines(100, 60);
    for bad in ["QJXQJX", "xoxb-", "xoxp-", "xoxa-QJXQJX", &format!("{} {}", bot(), app()), &format!("xoxb-{}", "Q".repeat(600)), &format!("xoxp-{}", "H".repeat(600))] {
        let (out, asks) = c.key(PASTE, Some(bad), false);
        assert!(out.is_none());
        assert_eq!(asks.notice.as_deref(), Some("that is not a Slack code: the \"Bot User OAuth Token\" starts xoxb-, the \"User OAuth Token\" xoxp-, the \"App-Level Token\" xapp-"));
        assert_eq!(c.body().lines(100, 60), before, "a refused paste changes nothing drawn");
        assert_eq!(c.held(), (None, None, None), "a refused paste is held nowhere");
    }
}

#[test]
fn enter_opens_the_page_until_the_tokens_are_held_then_connects_once() {
    let (mut d, _) = in_step("not_connected");
    let (_, asks) = key(&mut d, COPY);
    assert_eq!(asks.copy, None, "the manifest is step 1's to copy, not the clipboard's on every screen");
    let (outs, asks) = key(&mut d, ENTER);
    assert!(outs.is_empty());
    assert_eq!(asks.open, Some(slack_app_url(NAME)), "Enter opens the page that makes the person's own app");
    let (_, asks) = key(&mut d, COPY);
    assert_eq!(asks.copy, Some(slack_app_manifest(NAME)), "the copy is the same app's description");
    let (outs, asks) = key(&mut d, ENTER);
    assert!(outs.is_empty() && asks.open.is_none(), "Enter on step 1 opens nothing more");
    assert_eq!(d.title(), " chat · set up Slack · step 2 of 3: the first two Slack codes ");
    // The tokens in any order: the app-level token first, then the bot token, then the user token, connects once.
    assert!(paste(&mut d, &app()).0.is_empty());
    assert_eq!(d.title(), " chat · set up Slack · step 2 of 3: the first two Slack codes ", "the missing token's step");
    assert!(paste(&mut d, &bot()).0.is_empty());
    let (outs, _) = paste(&mut d, &user());
    assert_eq!(connects(&outs), [(bot(), user(), app())]);
    assert_eq!(outs[0].0, host(), "to the host that answered unconfigured");
    for k in [ENTER, PASTE] {
        assert!(d.setup_key(k, Some(&bot())).0.is_empty(), "{k:?} sends no second connect");
    }
}

#[test]
fn a_refused_token_goes_back_to_its_step_and_the_others_are_kept() {
    let mut c = step(ChatUnconfigured::NotConnected);
    for t in [bot(), user(), app()] {
        c.key(PASTE, Some(&t), false);
    }
    c.answered(ChatReply::Refused(ChatCode::AppTokenRefused));
    assert_eq!(c.screen(), &Screen::App);
    assert!(c.body().head.starts_with("Slack did not accept the code you pasted. Make sure you copied the \"App-Level Token\""));
    assert_eq!(c.held(), (Some(bot().as_str()), Some(user().as_str()), None));
    assert!(c.key(PASTE, Some(&app()), false).0.is_some(), "the other two were kept: a new last token connects again");

    c.answered(ChatReply::Refused(ChatCode::BotTokenRefused));
    assert_eq!(c.screen(), &Screen::Tokens);
    assert!(c.body().head.starts_with("Slack did not accept the code you pasted. Make sure you copied the \"Bot User OAuth Token\""));
    assert_eq!(c.held(), (None, Some(user().as_str()), Some(app().as_str())));
    assert!(c.key(PASTE, Some(&bot()), false).0.is_some(), "the other two were kept");

    c.answered(ChatReply::Refused(ChatCode::UserTokenRefused));
    assert_eq!(c.screen(), &Screen::Tokens);
    assert!(c.body().head.starts_with("Slack did not accept the code you pasted. Make sure you copied the \"User OAuth Token\" (it starts with xoxp-); then copy it again, click in this panel and press"));
    assert_eq!(c.held(), (Some(bot().as_str()), None, Some(app().as_str())));
    assert!(!leaks(&c.body().lines(100, 60).join("\n")));
    assert!(c.key(PASTE, Some(&user()), false).0.is_some(), "the other two were kept");

    // Any other answer drops all three and says why on the first screen.
    c.answered(ChatReply::Refused(ChatCode::OtherWorkspace));
    assert_eq!(c.screen(), &Screen::First);
    assert!(c.body().head.starts_with(&format!("Not connected: {}.", ChatCode::OtherWorkspace.text())));
    assert_eq!(c.held(), (None, None, None));
    for t in [bot(), user(), app()] {
        c.key(PASTE, Some(&t), false);
    }
    c.answered(ChatReply::Unsupported);
    assert!(c.body().head.starts_with("Not connected: Ship of Tools on a is too old to connect Slack: update it."));
    // A step that sent nothing is not changed by a reply.
    let mut c = step(ChatUnconfigured::NotConnected);
    c.key(PASTE, Some(&bot()), false);
    c.answered(ChatReply::Refused(ChatCode::BotTokenRefused));
    assert_eq!((c.screen(), c.held().0), (&Screen::Tokens, Some(bot().as_str())), "a reply to no connect of this step's");
}

#[test]
fn a_refused_user_token_returns_to_step_two_and_keeps_the_others() {
    let (mut d, t) = in_step("not_connected");
    for token in [bot(), user(), app()] {
        paste(&mut d, &token);
    }
    d.on_evt(&host(), refused(ChatCode::UserTokenRefused), t);
    for ms in [10, 20] {
        d.tick(shown("ROW1"), t + Duration::from_millis(ms));
    }
    assert_eq!(d.title(), " chat · set up Slack · step 2 of 3: the first two Slack codes ");
    assert!(drawn(&d).contains("Make sure you copied the \"User OAuth Token\" (it starts with xoxp-)"));
    assert_eq!(d.setup_held(), Some((Some(bot()), None, Some(app()))), "the bot and app tokens are kept through the frames");
    assert_eq!(connects(&paste(&mut d, &user()).0), [(bot(), user(), app())], "a new user token connects again");
}

/// Escape stops nothing at the hub: the keys line says so, and the older connect's reply never acts on a step begun since,
/// which sends nothing until that reply.
#[test]
fn an_older_connects_reply_never_acts_on_a_newer_step() {
    let (mut d, t) = in_step("not_connected");
    paste(&mut d, &bot());
    paste(&mut d, &user());
    assert_eq!(connects(&paste(&mut d, &app()).0).len(), 1);
    assert_eq!(d.body().unwrap().pin, "Esc  close; the check goes on");
    let (_, asks) = key(&mut d, ESC);
    assert!(asks.close);
    d.hide();
    assert!(!d.in_setup(), "a hidden drawer ends the step");
    // The drawer opens again; the hub still says `unconfigured`, so a new step begins, and its two tokens wait.
    d.tick(None, t);
    d.tick(shown("ROW1"), t + Duration::from_millis(10));
    d.tick(shown("ROW1"), t + Duration::from_millis(200));
    d.on_evt(&host(), unconfigured("not_connected"), t + Duration::from_millis(210));
    d.tick(shown("ROW1"), t + Duration::from_millis(220));
    paste(&mut d, &bot());
    paste(&mut d, &user());
    let (outs, asks) = paste(&mut d, &app());
    assert!(outs.is_empty(), "no second connect while the first is unanswered");
    assert!(asks.notice.unwrap().starts_with("Slack is still checking the codes sent before"));
    // The first connect's refusal of a bot token is not this step's.
    d.on_evt(&host(), refused(ChatCode::BotTokenRefused), t + Duration::from_millis(230));
    assert_eq!(d.title(), " chat · set up Slack · step 3 of 3: the last Slack code ");
    assert!(!drawn(&d).contains("did not accept"));
    // Now Enter sends the held tokens, once.
    assert_eq!(connects(&key(&mut d, ENTER).0), [(bot(), user(), app())]);
}

#[test]
fn a_modal_screen_ends_at_a_row_change_a_hidden_drawer_and_a_link_change() {
    type End = fn(&mut ChatDrawer, Instant);
    let ends: [(&str, End); 3] = [
        ("row change", |d, t| drop(d.tick(shown("ROW2"), t))),
        ("hidden drawer", |d, _| d.hide()),
        ("link change", |d, t| drop(d.on_connection(&host(), false, false, t))),
    ];
    for (why, end) in ends {
        // The Connect step, holding a token.
        let (mut d, t) = in_step("not_connected");
        paste(&mut d, &bot());
        end(&mut d, t);
        assert!(!d.in_setup(), "{why} ends the Connect step");
        // The picker, the same way.
        let (mut d, t) = channel(&[top(1)]);
        d.open_picker(shown("ROW1").as_ref()).unwrap();
        end(&mut d, t);
        assert!(!d.picking(), "{why} ends the picker");
    }
    // Back on the row, the step begins again with nothing held.
    let (mut d, t) = in_step("not_connected");
    paste(&mut d, &bot());
    d.tick(None, t);
    d.tick(shown("ROW1"), t + Duration::from_millis(10));
    d.tick(shown("ROW1"), t + Duration::from_millis(200));
    d.on_evt(&host(), unconfigured("not_connected"), t + Duration::from_millis(210));
    d.tick(shown("ROW1"), t + Duration::from_millis(220));
    key(&mut d, RIGHT);
    assert!(drawn(&d).contains("Bot User OAuth Token   not pasted yet"));
    // Another answer while the person pastes ends a step begun from `unconfigured`.
    let (mut d, t) = in_step("not_connected");
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), t);
    d.tick(shown("ROW1"), t);
    assert!(!d.in_setup(), "another window connected the hub meanwhile");
}

/// A hub that holds no token that posts as the person shows screen 9 in place of the channel; Enter opens the person's list of
/// Slack apps (the opener is the ask the window carries out) and goes on to step 2, where the three tokens are asked for as
/// ever.
#[test]
fn a_hub_that_holds_no_user_token_shows_post_as_you_and_enter_opens_the_apps() {
    let (mut d, _) = after_a_refused_post();
    assert_eq!(d.title(), " chat · post as you ");
    let (outs, asks) = key(&mut d, ENTER);
    assert!(outs.is_empty());
    assert_eq!(asks.open.as_deref(), Some("https://api.slack.com/apps"), "the person's apps, never the page that makes an app");
    assert_eq!(d.title(), " chat · set up Slack · step 2 of 3: the first two Slack codes ");
    for token in [user(), bot()] {
        assert!(paste(&mut d, &token).0.is_empty());
    }
    assert_eq!(connects(&paste(&mut d, &app()).0), [(bot(), user(), app())]);
}

/// A post the hub refuses `no_user_token` (it holds none, or Slack refused the one it had and the daemon forgot it) brings the
/// Connect step's post-as-you screen in place of the channel, with the draft kept and the daemon's words on the status line;
/// the row is not resolved again (the hub's resolve says nothing of the token). Any other refusal brings nothing, and neither
/// does a refusal for a row that is no longer shown.
#[test]
fn a_post_refused_for_no_user_token_brings_the_post_as_you_screen_and_keeps_the_draft() {
    let (mut d, at) = after_a_refused_post();
    assert_eq!(d.take_notice().as_deref(), Some("not posted: no Slack code that posts as you is saved; open the channel list (Ctrl+Shift+k, or Cmd+Shift+k on a Mac) and choose its last entry, Connect a Slack workspace (Connect Slack again on an older computer)"));
    assert!(d.in_setup());
    assert_eq!(d.title(), " chat · post as you ");
    assert_eq!(d.draft(), Some(("hello", "CH1")), "the draft is kept");
    assert_eq!(resolves(&d.tick(shown("ROW1"), at + Duration::from_millis(10)).0), 0, "the row is not resolved again");
    assert!(d.in_setup(), "the step is the person's to leave: a frame ends nothing");
    // A refusal that is not this one brings nothing.
    let (mut d, at) = channel(&[top(1)]);
    d.edit(shown("ROW1").as_ref(), Edit::Text("hello")).unwrap();
    let req = match d.send(shown("ROW1").as_ref(), Some(LOGIN), at).unwrap().as_slice() {
        [(_, OutgoingReq::ChatPost { req })] => req.clone(),
        other => panic!("one chat.post expected, got {other:?}"),
    };
    let busy = ChatReply::Done(sot_protocol::ops::ChatPostRes::Refused { code: ChatCode::Busy });
    d.on_evt(&host(), ChatEvt::Posted { req, reply: busy }, at);
    assert!(!d.in_setup());
    // Escape leaves the step, and the channel shows again with the draft.
    let (mut d, _) = after_a_refused_post();
    let (_, asks) = key(&mut d, ESC);
    assert!(asks.close);
}

/// A refusal that comes after the person moved to another row opens nothing over that row (the step is for the row that was
/// posted in), and one that comes while a Connect step holds a pasted token leaves the step and its token alone.
#[test]
fn a_late_refusal_never_opens_over_another_row_or_replaces_a_step_holding_a_token() {
    let (mut d, at, req) = post_in_flight();
    d.tick(shown("ROW2"), at + Duration::from_millis(10));
    d.on_evt(&host(), ChatEvt::Posted { req, reply: no_user_token() }, at + Duration::from_millis(20));
    assert!(!d.in_setup(), "the refusal was for another row's post");

    let (mut d, at, req) = post_in_flight();
    // Connect Slack again, last in the picker, brings the step over the mapped row; a bot token is pasted.
    assert_eq!(lists(&d.open_picker(shown("ROW1").as_ref()).unwrap()), 1);
    super::picker_tests::older(&mut d, at);
    let listed: ChatChannelsRes = serde_json::from_value(serde_json::json!({ "channels": [
        { "id": "C1", "name": "lab-general", "private": false, "member": true } ] })).unwrap();
    d.on_evt(&host(), ChatEvt::Channels { workspace: None, reply: ChatReply::Done(listed) }, at);
    (0..3).for_each(|_| d.picker_step(true));
    assert!(d.picker_enter().is_empty() && d.in_setup());
    paste(&mut d, &bot());
    d.on_evt(&host(), ChatEvt::Posted { req, reply: no_user_token() }, at + Duration::from_millis(30));
    assert_eq!(d.setup_held(), Some((Some(bot()), None, None)), "the pasted token is kept");
    assert_eq!(d.title(), " chat · set up Slack · step 2 of 3: the first two Slack codes ", "and the step is the one the person was in");
}

/// B1: the Connected screen, over a row whose channel the hub maps, parks the channel's records as the picker does.
#[test]
fn the_connect_step_parks_the_channels_records() {
    let mut fonts = FontSystem::new();
    let (mut d, at) = channel(&[top(1), top(2), top(3)]);
    d.set_login(LOGIN);
    d.layout(&mut fonts, body(), 1.0, body());
    assert!(d.placed_text().join("|").contains("message 3"));
    // Connect Slack again, last in the picker, starts the step over the mapped row.
    assert_eq!(lists(&d.open_picker(shown("ROW1").as_ref()).unwrap()), 1);
    super::picker_tests::older(&mut d, at);
    let listed: ChatChannelsRes = serde_json::from_value(serde_json::json!({ "channels": [
        { "id": "C1", "name": "lab-general", "private": false, "member": true } ] })).unwrap();
    d.on_evt(&host(), ChatEvt::Channels { workspace: None, reply: ChatReply::Done(listed) }, at);
    let rows = d.body().unwrap().lines(100, 30);
    assert!(rows.join(" ").contains(&format!("/invite @{NAME}")), "the picker's invite names the person's app");
    assert!(rows.iter().any(|r| r.trim_start_matches(['>', '.', ' ']).starts_with("Connect Slack again")), "Connect Slack again is offered");
    (0..3).for_each(|_| d.picker_step(true));
    assert!(d.picker_enter().is_empty(), "Connect Slack again sends nothing");
    assert!(d.in_setup() && !d.picking());
    paste(&mut d, &bot());
    paste(&mut d, &user());
    paste(&mut d, &app());
    d.on_evt(&host(), connected("T1"), at);
    d.tick(shown("ROW1"), at + Duration::from_millis(10));
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "mapped", "workspace": "WS1", "channel": "CH1", "delivers": false })), at);
    d.tick(shown("ROW1"), at + Duration::from_millis(20));
    d.layout(&mut fonts, body(), 1.0, body());
    assert_eq!(d.title(), " chat · connected ");
    assert!(d.placed_text().is_empty(), "the connected screen draws over no record");
    assert!(drawn(&d).contains("It keeps that workspace's three Slack codes on a, readable only by your login there. It is now checking which of the projects' channels"));
}

#[test]
fn a_connect_reply_resolves_the_row_again_and_enter_then_opens_the_picker() {
    let (mut d, t) = in_step("not_connected");
    paste(&mut d, &bot());
    paste(&mut d, &user());
    paste(&mut d, &app());
    d.on_evt(&host(), connected("T1"), t);
    assert_eq!(resolves(&d.tick(shown("ROW1"), t).0), 1, "the reply resolves the row at once");
    assert_eq!(d.title(), " chat · connected ");
    assert!(drawn(&d).contains("Ship of Tools is connected to the Slack workspace with ID T1."));
    // Before the row's new answer, Enter says to wait; after it, Enter opens the picker and asks for the channels.
    let (outs, asks) = key(&mut d, ENTER);
    assert!(outs.is_empty() && d.in_setup());
    assert_eq!(asks.notice.as_deref(), Some("setting up the chat: press Enter again in a few seconds to choose this project's channel"));
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), t);
    d.tick(shown("ROW1"), t);
    assert!(d.in_setup(), "the connected screen waits for the person");
    let (outs, _) = key(&mut d, ENTER);
    assert_eq!((lists(&outs), d.in_setup(), d.picking()), (1, false, true));

    // A refusal resolves the row again too, and so does a connect whose reply was lost.
    let (mut d, t) = in_step("not_connected");
    paste(&mut d, &bot());
    paste(&mut d, &user());
    paste(&mut d, &app());
    d.on_evt(&host(), refused(ChatCode::Failed), t);
    assert_eq!(resolves(&d.tick(shown("ROW1"), t).0), 1);
    let (mut d, t) = in_step("not_connected");
    paste(&mut d, &bot());
    paste(&mut d, &user());
    paste(&mut d, &app());
    d.on_evt(&host(), ChatEvt::ConnectUnknown, t);
    assert!(!d.in_setup(), "an unknown outcome ends the step: the row's new answer decides what shows");
    assert_eq!(d.take_notice().as_deref(), Some("Slack may be connected: a did not answer before the connection ended; checking again. If the panel still says Slack is not connected after a minute, connect Slack again"));
    assert_eq!(resolves(&d.tick(shown("ROW1"), t).0), 1);
}

/// S1: at a 100x30 window the drawer is about 50 cells by 11: the keys stay at the foot, and PageDown brings the rest.
#[test]
fn the_keys_stay_in_view_and_the_screen_scrolls_in_a_small_drawer() {
    let (w, h) = (50, 11);
    let (mut d, _) = in_step("not_connected");
    for (i, k) in [None, ENTER, ENTER, RIGHT].into_iter().enumerate() {
        if k.is_some() {
            key(&mut d, k);
        }
        let b = d.body().unwrap();
        let rows = b.lines(w, h);
        assert_eq!(rows.len(), h, "screen {i} fills the drawer");
        let pin = crate::ui::chrome::wrap_status(&b.pin, w, w);
        assert_eq!(rows[h - pin.len()..], pin[..], "screen {i}: the keys line is at the foot");
    }
    // Step 3 does not fit: PageDown moves its text up, the keys staying, until its last row shows.
    let end = d.body().unwrap().scroll_end(w, h);
    assert!(end > 0, "step 3 is taller than the drawer");
    d.set_body_end(end, h as u16);
    let first = d.body().unwrap().lines(w, h);
    assert!(!first.join(" ").contains("App-Level Token   not pasted yet"));
    for _ in 0..10 {
        d.scroll(false, Step::Page, Instant::now(), Duration::ZERO);
    }
    let last = d.body().unwrap().lines(w, h);
    assert!(last.join(" ").contains("App-Level Token   not pasted yet"), "PageDown reaches the field");
    assert_eq!(last[h - 1], first[h - 1], "the keys stay");
    d.scroll(true, Step::Page, Instant::now(), Duration::ZERO);
    assert_ne!(d.body().unwrap().lines(w, h), last, "PageUp moves back");
    // A new screen starts at its top.
    key(&mut d, LEFT);
    assert_eq!(d.body().unwrap().scroll, 0);
}

/// How a connect ends, each followed by what fe-net would deliver.
#[derive(Debug, Clone, Copy)]
enum End {
    Done,
    AppRefused,
    BotRefused,
    UserRefused,
    OtherWorkspace,
    Unsupported,
    Unknown,
    NeverWritten,
    LinkDrop,
}

/// Every way a connect ends frees the one-connect slot (a fresh step's two pastes send), and a refused token keeps the other
/// one through the frames that follow, while the row is resolved again.
#[test]
fn every_end_of_a_connect_frees_the_slot_and_a_refusal_keeps_the_other_token_through_the_next_frames() {
    use End::*;
    for end in [Done, AppRefused, BotRefused, UserRefused, OtherWorkspace, Unsupported, Unknown, NeverWritten, LinkDrop] {
        let (mut d, t) = in_step("not_connected");
        paste(&mut d, &bot());
        paste(&mut d, &user());
        let outs = paste(&mut d, &app()).0;
        assert_eq!(connects(&outs).len(), 1, "{end:?}: the first connect");
        match end {
            Done => drop(d.on_evt(&host(), connected("T1"), t)),
            AppRefused => drop(d.on_evt(&host(), refused(ChatCode::AppTokenRefused), t)),
            BotRefused => drop(d.on_evt(&host(), refused(ChatCode::BotTokenRefused), t)),
            UserRefused => drop(d.on_evt(&host(), refused(ChatCode::UserTokenRefused), t)),
            OtherWorkspace => drop(d.on_evt(&host(), refused(ChatCode::OtherWorkspace), t)),
            Unsupported => drop(d.on_evt(&host(), ChatEvt::Connect { reply: ChatReply::Unsupported }, t)),
            Unknown => drop(d.on_evt(&host(), ChatEvt::ConnectUnknown, t)),
            NeverWritten => {
                let evt = never_written(&outs[0].1).expect("a connect that could not be queued is told");
                drop(d.on_evt(&host(), evt, t));
            }
            LinkDrop => {
                d.on_connection(&host(), false, false, t);
                d.on_connection(&host(), true, false, t);
                drop(d.on_evt(&host(), ChatEvt::ConnectUnknown, t));
            }
        }
        // The frames after: the row is asked again (its stand reads `Resolving`), and a refused token's step keeps the other.
        for ms in [10, 20] {
            d.tick(shown("ROW1"), t + Duration::from_millis(ms));
        }
        match end {
            AppRefused => assert_eq!(d.setup_held(), Some((Some(bot()), Some(user()), None)), "{end:?}: the bot and user tokens are kept"),
            BotRefused => assert_eq!(d.setup_held(), Some((None, Some(user()), Some(app()))), "{end:?}: the user and app tokens are kept"),
            UserRefused => assert_eq!(d.setup_held(), Some((Some(bot()), None, Some(app()))), "{end:?}: the bot and app tokens are kept"),
            _ => {}
        }
        // The hub answers `unconfigured` again; a step begun now (after the drawer was hidden) sends its two tokens.
        d.hide();
        d.tick(None, t + Duration::from_millis(30));
        d.tick(shown("ROW1"), t + Duration::from_millis(40));
        d.tick(shown("ROW1"), t + Duration::from_millis(300));
        d.on_evt(&host(), unconfigured("not_connected"), t + Duration::from_millis(310));
        d.tick(shown("ROW1"), t + Duration::from_millis(320));
        assert!(d.in_setup(), "{end:?}: a new step");
        paste(&mut d, &bot());
        paste(&mut d, &user());
        assert_eq!(connects(&paste(&mut d, &app()).0).len(), 1, "{end:?}: the slot was not freed");
    }
}

/// A screen taller than the drawer says that more is below until its last row shows; the wheel and Alt+Up and Alt+Down
/// (a line step) scroll it as PageUp and PageDown do.
#[test]
fn a_clipped_screen_says_there_is_more_and_every_scroll_moves_it() {
    let (w, h) = (50, 11);
    let (mut d, _) = in_step("not_connected");
    key(&mut d, RIGHT);
    key(&mut d, RIGHT);
    let end = d.body().unwrap().scroll_end(w, h);
    assert!(end > 1);
    d.set_body_end(end, h as u16);
    assert!(d.body().unwrap().lines(w, h).contains(&super::picker::MORE.to_string()), "the top of a clipped screen says so");
    d.scroll(false, Step::Line, Instant::now(), Duration::ZERO);
    assert_eq!(d.body().unwrap().scroll, 1, "a line step moves one row");
    for _ in 0..end {
        d.scroll(false, Step::Line, Instant::now(), Duration::ZERO);
    }
    let rows = d.body().unwrap().lines(w, h);
    assert_eq!(d.body().unwrap().scroll, end);
    assert!(!rows.contains(&super::picker::MORE.to_string()), "the end of the screen says nothing more");
    // A screen that fits says nothing either.
    let (d, _) = in_step("not_connected");
    assert!(!d.body().unwrap().lines(400, 60).contains(&super::picker::MORE.to_string()));
}

/// While the Connect step has the keys a typed character is neither logged nor shown as the last key; elsewhere, and for
/// named keys, the key is said as always.
#[test]
fn a_typed_character_is_neither_logged_nor_shown_while_the_keys_take_secrets() {
    use crate::ui::input::keypress::key_texts;
    use winit::keyboard::{Key, NamedKey};
    let typed = Key::Character("x".into());
    assert_eq!(key_texts(&typed, true), ("<typed>".into(), "<typed>".into()));
    let (label, logged) = key_texts(&typed, false);
    assert_eq!(label, "x");
    assert!(logged.contains('x'));
    let enter = Key::Named(NamedKey::Enter);
    assert_eq!(key_texts(&enter, true), key_texts(&enter, false));
}

/// The project's own words never reach a screen: every text a person reads in the Chat drawer (each code's text, each `Stand`'s
/// words at a named, the local and an unnamed computer, every Connect screen and refusal, the picker's list and failures, a
/// refused pair, each post outcome and the foot's hints) holds none of these words. A word Slack names (its field names, its
/// headings), a file or command a person types and a key of `hosts.toml` is removed first. No text is spared `row`: a
/// person reads every code's text in the window and from `sot-fe chat post`, where the manual's word is `session`. Nor
/// does any name a computer as the one that `runs your agents`: the manual calls it the computer that runs your sessions.
#[test]
fn no_project_word_reaches_a_screen() {
    use super::compose::{compose_rows, ComposeText};
    use super::drawer::{post_words, wait_words};
    use super::owner::PostShown;
    use super::picker::{pair_words, Picker};
    use super::row_channel::Stand;
    use sot_protocol::ops::ChatPairRes;
    use sot_protocol::ops::ChatUnlinkedWhy::{ChannelWide, MembersUnread, More, NoMember, Other, SeveralMembers, TooLong, TooManyMembers};

    let codes = {
        // A match with no wildcard: a code added later fails to compile here until its text is in the table.
        let all = |c: ChatCode| match c {
            ChatCode::Unavailable | ChatCode::Unmapped | ChatCode::StaleRow | ChatCode::StaleMapping | ChatCode::BadRequest
            | ChatCode::TooManyWatches | ChatCode::Limited | ChatCode::Busy | ChatCode::BotTokenRefused | ChatCode::AppTokenRefused
            | ChatCode::UserTokenRefused | ChatCode::NoUserToken | ChatCode::NotAMember | ChatCode::OtherWorkspace
            | ChatCode::NotInChannel | ChatCode::ChannelTaken | ChatCode::NotSaved | ChatCode::NotAFile | ChatCode::TooLarge
            | ChatCode::NotAnImage | ChatCode::TooManyWorkspaces | ChatCode::NoSuchChannel | ChatCode::OtherApp | ChatCode::MixedApps
            | ChatCode::NotOwnMessage | ChatCode::AlreadyPaired | ChatCode::SeveralChannels | ChatCode::UnclearFile | ChatCode::Failed | ChatCode::Other => c,
        };
        [
            ChatCode::Unavailable, ChatCode::Unmapped, ChatCode::StaleRow, ChatCode::StaleMapping, ChatCode::BadRequest,
            ChatCode::TooManyWatches, ChatCode::Limited, ChatCode::Busy, ChatCode::BotTokenRefused, ChatCode::AppTokenRefused,
            ChatCode::UserTokenRefused, ChatCode::NoUserToken, ChatCode::NotAMember, ChatCode::OtherWorkspace,
            ChatCode::NotInChannel, ChatCode::ChannelTaken, ChatCode::NotSaved, ChatCode::NotAFile, ChatCode::TooLarge,
            ChatCode::NotAnImage, ChatCode::TooManyWorkspaces, ChatCode::NoSuchChannel, ChatCode::OtherApp, ChatCode::MixedApps,
            ChatCode::NotOwnMessage, ChatCode::AlreadyPaired, ChatCode::SeveralChannels, ChatCode::UnclearFile, ChatCode::Failed, ChatCode::Other,
        ]
        .map(all)
    };
    let mut said: Vec<String> = codes.iter().map(|c| c.text().to_string()).collect();
    for host in ["a", "local", ""] {
        let stands = [Stand::Unlisted, Stand::Offline, Stand::Resolving, Stand::Mapped, Stand::Unmapped, Stand::NoConfig, Stand::NotHub, Stand::RowGone, Stand::NoChat]
            .into_iter()
            .chain([Stand::Unconfigured(ChatUnconfigured::NotConnected), Stand::Failed(ChatCode::Busy)]);
        said.extend(stands.map(|s| s.words(host)));
        said.push(super::compose::compose_rows(&ComposeText::of(None, None, false, Some(false), host, true), 200, 10).join(" "));
        said.push(post_words(&PostShown::Outdated, true, host).unwrap_or_default());
    }
    said.extend([wait_words(5, false), wait_words(5, true)]);
    let unlinked = |why| PostShown::Posted(vec![r#""Ada" ("Ada Example")"#.into(), String::new()], vec![sot_protocol::ops::ChatUnlinked { name: r#""@Zed""#.into(), why, candidates: vec![r#""Zed One""#.into(), String::new()] }]);
    let reasons = [NoMember, SeveralMembers, MembersUnread, TooManyMembers, ChannelWide, TooLong, More, Other].map(unlinked);
    for shown in [PostShown::Posted(Vec::new(), Vec::new()), PostShown::Unknown, PostShown::Limited(3, false)].into_iter().chain(reasons) {
        said.extend(post_words(&shown, false, "a"));
    }
    for code in codes {
        said.extend(post_words(&PostShown::Refused(code), code == ChatCode::BadRequest, "a"));
    }
    said.push(compose_rows(&ComposeText::of(None, None, true, Some(true), "a", true), 200, 10).join(" "));
    // A file post with no file of the project in the preview: nothing, or a picture opened from the chat.
    let key = crate::ui::input::keybindings::KeyBindings::defaults().first_label(crate::ui::input::keybindings::Action::ChatPostFile);
    said.extend([super::drawer::NO_FILE_WORDS.to_string(), super::drawer::chat_picture_words(&key)]);
    // The Connect step: each first screen, the steps, a refused code of each kind, connecting and connected, and a post's.
    let mut screens = Vec::new();
    for reason in [ChatUnconfigured::NotConnected, ChatUnconfigured::TokensRefused, ChatUnconfigured::StartFailed, ChatUnconfigured::WrongApp, ChatUnconfigured::Other] {
        let mut c = step(reason);
        screens.push(c.title());
        screens.push(c.body().head);
        screens.push(c.body().pin);
        for k in [ENTER, ENTER, RIGHT] {
            c.key(k, None, false);
            let b = c.body();
            screens.extend([c.title(), b.head, b.pin]);
        }
        let mut c = step(reason);
        for t in [bot(), user(), app()] {
            c.key(PASTE, Some(&t), false);
        }
        screens.extend([c.title(), c.body().head, c.body().pin]);
        for reply in [ChatReply::Refused(ChatCode::BotTokenRefused), ChatReply::Refused(ChatCode::UserTokenRefused), ChatReply::Refused(ChatCode::AppTokenRefused), ChatReply::Refused(ChatCode::MixedApps), ChatReply::Unsupported, ChatReply::Refused(ChatCode::OtherWorkspace)] {
            let mut c = step(reason);
            for t in [bot(), user(), app()] {
                c.key(PASTE, Some(&t), false);
            }
            c.answered(reply);
            screens.extend([c.title(), c.body().head, c.body().pin]);
        }
        let mut c = step(reason);
        for t in [bot(), user(), app()] {
            c.key(PASTE, Some(&t), false);
        }
        c.answered(ChatReply::Done(ChatConnectRes { workspace: "T1".into(), name: None }));
        screens.extend([c.title(), c.body().head, c.body().pin]);
        screens.push(c.key(PASTE, Some("not a code"), false).1.notice.unwrap_or_default());
    }
    let mut c = Connect::for_post(host(), LOGIN, "WS1".into());
    screens.extend([c.title(), c.body().head, c.body().pin]);
    // The same screen once the hub names the Slack app the workspace is connected to.
    c.on_workspaces(&connected_app("Ship of Tools (lab-user)"));
    screens.extend([c.title(), c.body().head, c.body().pin]);
    // The other Slack app's screen, named and not, told and not; the steps after Enter (for an app told, steps 2 and 3 with
    // the app to keep named, and a set of two apps' codes refused there), after the other key and after a paste of the
    // connected app's code; the status line's words when its step was closed.
    let named = ChatOtherApp { workspace: Some("TAAAA1".into()), new_app_known: true, ..ChatOtherApp::default() };
    for asked in [other_app(), untold(), ChatOtherApp { app_id: None, new_app_name: None, ..other_app() }, named, ChatOtherApp { workspace: Some("TAAAA1".into()), ..ChatOtherApp::default() }, ChatOtherApp::default()] {
        let mut c = asked_with(asked.clone());
        screens.extend([c.title(), c.body().head, c.body().pin]);
        c.key(ENTER, None, false);
        screens.extend([c.title(), c.body().head, c.body().pin]);
        for t in [app(), connected_user(), connected_bot()] {
            screens.push(c.key(PASTE, Some(&t), false).1.notice.unwrap_or_default());
            screens.extend([c.title(), c.body().head, c.body().pin]);
        }
        c.answered(ChatReply::Refused(ChatCode::MixedApps));
        screens.extend([c.title(), c.body().head, c.body().pin]);
        let mut c = asked_with(asked.clone());
        c.key(RIGHT, None, false);
        screens.extend([c.title(), c.body().head, c.body().pin]);
        let mut c = asked_with(asked);
        c.key(PASTE, Some(&connected_bot()), false);
        screens.extend([c.title(), c.body().head, c.body().pin]);
    }
    screens.extend([super::drawer::not_replaced_words(), super::drawer::mixed_words()]);
    said.extend(screens);
    // The picker: a list, a narrowed list with no match, each failure, and a refused pair.
    let list: ChatChannelsRes = serde_json::from_value(serde_json::json!({ "channels": [
        { "id": "C1", "name": "lab-general", "private": false, "member": true },
        { "id": "C2", "name": "lab-papers", "private": true, "member": true },
        { "id": "C3", "name": "lab-new", "private": false, "member": false } ], "more": true })).unwrap();
    let mut picks: Vec<String> = Vec::new();
    for reply in [ChatReply::Done(list), ChatReply::Unsupported, ChatReply::Refused(ChatCode::Unavailable)] {
        let mut p = Picker::new(shown("ROW1").unwrap(), true, NAME.to_string());
        p.on_channels(None, reply);
        picks.push(p.title());
        picks.extend(p.body().lines(100, 40));
        p.edit(Edit::Text("zzz"));
        picks.extend(p.body().lines(100, 40));
    }
    // Several workspaces: one listed, two (moved between), none running, the list unread, a refused pair and paste, a
    // workspace with no name.
    let running = |all: &[(&str, Option<&str>, bool)]| -> ChatReply<sot_protocol::ops::ChatWorkspacesRes> {
        let list: Vec<serde_json::Value> = all.iter().map(|(id, name, running)| serde_json::json!({ "id": id, "name": name, "running": running })).collect();
        ChatReply::Done(serde_json::from_value(serde_json::json!({ "default": all[0].0, "workspaces": list })).unwrap())
    };
    let places: [&[(&str, Option<&str>, bool)]; 4] = [
        &[("TAAAA1", Some("Alpha Lab"), true)],
        &[("TAAAA1", Some("Alpha Lab"), true), ("TBBBB2", None, true)],
        &[("TAAAA1", None, false)],
        &[("TAAAA1", Some("Alpha Lab"), true), ("TBBBB2", Some("Beta Lab"), true), ("TCCCC3", None, false)],
    ];
    for (i, all) in places.into_iter().enumerate() {
        let mut p = Picker::new(shown("ROW1").unwrap(), i % 2 == 0, NAME.to_string());
        picks.push(p.title());
        picks.extend(p.body().lines(100, 40));
        let ask = p.on_workspaces(running(all));
        picks.push(p.title());
        picks.extend(p.body().lines(100, 40));
        p.on_channels(ask.flatten().as_deref(), ChatReply::Done(serde_json::from_value(serde_json::json!({ "channels": [
            { "id": "C1", "name": "lab-general", "private": false, "member": true } ], "more": true })).unwrap()));
        picks.push(p.title());
        picks.extend(p.body().lines(100, 40));
        picks.push(p.no_such_channel());
        picks.push(p.paste("https://app.slack.com/client/TZZZZ9/C0456EFGH").err().unwrap_or_default());
        // Another workspace, when one runs: step down to its row and open it, so its words are read too.
        let several = all.iter().filter(|w| w.2).count() > 1;
        let opened = (0..8).find_map(|_| {
            p.step(true);
            p.open()
        });
        assert_eq!(opened.is_some(), several, "a second running workspace's row is reached, and no other opens");
        if let Some(w) = opened {
            picks.extend(p.body().lines(100, 40));
            p.on_channels(w.as_deref(), ChatReply::Refused(ChatCode::Busy));
            picks.extend([p.title(), p.no_such_channel()]);
            picks.extend(p.body().lines(100, 40));
        }
    }
    for reply in [ChatReply::Unsupported, ChatReply::Refused(ChatCode::Busy)] {
        let mut p = Picker::new(shown("ROW1").unwrap(), false, NAME.to_string());
        p.on_workspaces(reply);
        picks.push(p.title());
        picks.extend(p.body().lines(100, 40));
    }
    // The Connect step of a row whose workspace failed, for each reason, with another workspace running; and a connect that
    // names its workspace.
    for reason in [ChatUnconfigured::NotConnected, ChatUnconfigured::TokensRefused, ChatUnconfigured::StartFailed, ChatUnconfigured::WrongApp, ChatUnconfigured::Other] {
        let mut c = step(reason).in_workspace(Some("TBBBB2".into()));
        picks.push(c.body().head);
        c.on_workspaces(&running(&[("TAAAA1", Some("Alpha Lab"), true), ("TBBBB2", Some("Beta Lab"), false)]));
        picks.extend([c.title(), c.body().head, c.body().pin]);
    }
    let mut c = step(ChatUnconfigured::NotConnected);
    for t in [bot(), user(), app()] {
        c.key(PASTE, Some(&t), false);
    }
    c.answered(ChatReply::Done(ChatConnectRes { workspace: "T1".into(), name: Some("Alpha Lab".into()) }));
    picks.extend([c.title(), c.body().head, c.body().pin]);
    // Its asks spent with no list: it could not check.
    let t0 = Instant::now();
    (0..14).for_each(|n| drop(c.ask_unread(t0 + Duration::from_secs(n))));
    picks.extend([c.body().head, c.body().pin]);
    // A connect whose Slack app reads every mapped channel, is not in one, and is not in two, one known by its ID.
    for unread in [serde_json::json!([]), serde_json::json!([{ "id": "G0PRIVATE1", "name": "lab-private" }]), serde_json::json!([{ "id": "G0PRIVATE1", "name": "lab-private" }, { "id": "C0PUBLIC1" }])] {
        let mut c = step(ChatUnconfigured::NotConnected);
        for t in [bot(), user(), app()] {
            c.key(PASTE, Some(&t), false);
        }
        c.answered(ChatReply::Done(ChatConnectRes { workspace: "T1".into(), name: None }));
        c.on_workspaces(&ChatReply::Done(serde_json::from_value(serde_json::json!({ "default": "T1", "workspaces": [{ "id": "T1", "running": true, "unread": unread }] })).unwrap()));
        picks.extend([c.title(), c.body().head, c.body().pin]);
    }
    // The post-as-you screen of an app whose bot Slack names nothing.
    let mut c = Connect::for_post(host(), LOGIN, "WS1".into());
    c.on_workspaces(&ChatReply::Done(serde_json::from_value(serde_json::json!({ "default": "WS1", "workspaces": [{ "id": "WS1", "running": true, "app_id": "A0NEW" }] })).unwrap()));
    picks.extend([c.title(), c.body().head, c.body().pin]);
    // A channel the connected Slack app cannot read, named and by its ID, and the picker of its project.
    for named in [true, false] {
        let (mut d, at) = unreadable(named);
        picks.push(drawn_at(&d, 100, 40));
        picker_of_unreadable(&mut d, at);
        picks.push(drawn_at(&d, 100, 40));
    }
    said.extend(picks);
    for reply in [ChatReply::Done(ChatPairRes {}), ChatReply::Unsupported, ChatReply::Refused(ChatCode::NotInChannel), ChatReply::Refused(ChatCode::ChannelTaken)] {
        for private in [false, true] {
            said.extend(pair_words(&reply, private, NAME, "a"));
        }
    }

    // What is taken out first, and why: Slack's own names and headings, a file's, a command's and a key's names a person types
    // or finds, and the one gloss of Slack's own word at the codes' first mention on the Connect step's first screen.
    let kept = [
        "(Slack calls them tokens)",
        "Bot User OAuth Token", "User OAuth Token", "App-Level Tokens", "App-Level Token", "User Token Scopes", "Generate Token and Scopes",
        "`hub =`", "`sotd log-path`",
    ];
    let banned = ["hub", "daemon", "token", "tokens", "row", "rows", "leg", "capsule", "sotd", "drawer"];
    // A phrase whose words are each true elsewhere (`your agents` post through your Slack app): the computer is the one
    // that runs your sessions, as the manual names it.
    let phrases = ["runs your agents"];
    let mut hits = Vec::new();
    for text in &said {
        hits.extend(phrases.iter().filter(|p| text.contains(*p)).map(|p| format!("{p:?} in {text:?}")));
        let mut rest = text.clone();
        for keep in kept.iter() {
            rest = rest.replace(keep, " ");
        }
        for word in rest.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
            if banned.contains(&word.to_lowercase().as_str()) {
                hits.push(format!("{word:?} in {text:?}"));
            }
        }
    }
    hits.dedup();
    assert!(hits.is_empty(), "{} project words on screens:\n{}", hits.len(), hits.join("\n"));
    assert_eq!(key_name_capitals(&said), Vec::<String>::new(), "a modifier followed by a capital letter key on a screen");
}

const SETUP: Option<Action> = Some(Action::ChatSetup);
const P: &str = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };

/// A drawer whose shown row ROW1 answered `unconfigured` for `reason` naming its channel's workspace `TBBBB2`, with the step
/// begun, the instant after, and what that frame sent.
fn in_failed(reason: &str) -> (ChatDrawer, Instant, Vec<Out>) {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.set_login(LOGIN);
    d.on_connection(&host(), true, false, t0);
    d.tick(shown("ROW1"), t0);
    d.tick(shown("ROW1"), t0 + Duration::from_millis(200));
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unconfigured", "reason": reason, "workspace": "TBBBB2" })), t0 + Duration::from_millis(210));
    let outs = d.tick(shown("ROW1"), t0 + Duration::from_millis(220)).0;
    assert!(d.in_setup(), "unconfigured begins the step");
    (d, t0 + Duration::from_millis(230), outs)
}

fn running(others: bool) -> ChatEvt {
    let res = serde_json::from_value(serde_json::json!({ "default": "TAAAA1", "workspaces": [
        { "id": "TAAAA1", "name": "Alpha Lab", "running": others },
        { "id": "TBBBB2", "name": "Beta Lab", "running": false } ] })).unwrap();
    ChatEvt::Workspaces { reply: ChatReply::Done(res) }
}

/// A row whose channel is in a workspace whose connection failed: the step names that workspace and why, and while another
/// runs, Ctrl+Shift+k (delta 7: inside the step's own keys) opens that one's channel list to move the project there; the
/// list stays over the row's `unconfigured`, and Escape gives the step back.
#[test]
fn a_failed_workspaces_step_names_it_and_its_chord_opens_another_workspaces_list() {
    let (mut d, at, outs) = in_failed("tokens_refused");
    assert_eq!(outs.iter().filter(|(_, r)| matches!(r, OutgoingReq::ChatWorkspaces)).count(), 1, "the step asks which workspaces run");
    assert!(drawn(&d).starts_with(" chat · set up Slack \nThis project's channel is in the Slack workspace with ID TBBBB2, whose connection failed."), "{}", drawn(&d));
    assert!(!drawn(&d).contains("Shift+k"), "no chord before another workspace is known to run");
    assert!(key(&mut d, SETUP).0.is_empty() && d.in_setup());
    d.on_evt(&host(), running(true), at);
    let shown_now = drawn(&d);
    assert!(shown_now.contains("This project's channel is in the Slack workspace Beta Lab, whose connection failed.\n\nSlack no longer accepts the Slack codes"), "{shown_now}");
    assert!(shown_now.contains(&format!("Enter  open your Slack apps    Left  make your Slack app again    {P}+Shift+k  open the channel list of another workspace    Esc  close")), "{shown_now}");
    // The chord: the picker in the step's place, asking which workspaces run; the row's stand leaves it there.
    let (outs, _) = key(&mut d, SETUP);
    assert_eq!(lists(&outs), 1);
    assert!(d.picking() && !d.in_setup());
    d.tick(shown("ROW1"), at + Duration::from_millis(10));
    assert!(d.picking(), "the row's unconfigured answer does not take the list away");
    let outs = d.on_evt(&host(), running(true), at);
    assert!(matches!(outs.as_slice(), [(_, OutgoingReq::ChatChannels { workspace: Some(w) })] if w == "TAAAA1"), "{outs:?}");
    let listed: ChatChannelsRes = serde_json::from_value(serde_json::json!({ "channels": [
        { "id": "C1", "name": "lab-general", "private": false, "member": true } ] })).unwrap();
    d.on_evt(&host(), ChatEvt::Channels { workspace: Some("TAAAA1".into()), reply: ChatReply::Done(listed) }, at);
    let sent = d.picker_enter();
    assert!(matches!(sent.as_slice(), [(_, OutgoingReq::ChatPair { workspace: Some(w), channel: Some(c), .. })] if w == "TAAAA1" && c.as_str() == "C1"), "{sent:?}");
    // Escape: the step comes back for the row's stand.
    let (mut d, at, _) = in_failed("tokens_refused");
    d.on_evt(&host(), running(true), at);
    key(&mut d, SETUP);
    d.dismiss_picker();
    d.tick(shown("ROW1"), at + Duration::from_millis(10));
    assert!(d.in_setup());
}

/// Each failure's lead says why, with the next step; with no other workspace running, or for a row with no workspace named,
/// the chord is not offered and does nothing.
#[test]
fn a_failed_workspaces_lead_says_why_and_the_chord_needs_another_running() {
    for (reason, lead) in [
        ("not_connected", "This project's channel is in the Slack workspace Beta Lab, which Ship of Tools on a is not connected to."),
        ("start_failed", "This project's channel is in the Slack workspace Beta Lab, whose chat did not start on a: the log of Ship of Tools' service on a says why; to find it, run `sotd log-path` in a terminal on a."),
        ("wrong_app", "This project's channel is in the Slack workspace Beta Lab, whose connection failed.\n\nThe chat stopped"),
    ] {
        let (mut d, at, _) = in_failed(reason);
        d.on_evt(&host(), running(false), at);
        let shown_now = drawn_at(&d, 2000, 60);
        assert!(shown_now.contains(lead), "{reason}: {shown_now}");
        assert!(!shown_now.contains("Shift+k"), "{reason}: no other workspace runs");
        assert!(key(&mut d, SETUP).0.is_empty() && d.in_setup(), "{reason}");
    }
    let (mut d, _) = in_step("tokens_refused");
    assert!(drawn(&d).starts_with(" chat · set up Slack \nSlack no longer accepts"), "a row with no workspace named has no lead");
    assert!(key(&mut d, SETUP).0.is_empty() && d.in_setup());
}

/// A connect whose reply names the workspace says its name, and Enter opens the picker on that workspace's list.
#[test]
fn the_connected_screen_names_the_workspace_and_opens_its_list() {
    let (mut d, t) = in_step("not_connected");
    paste(&mut d, &bot());
    paste(&mut d, &user());
    paste(&mut d, &app());
    d.on_evt(&host(), ChatEvt::Connect { reply: ChatReply::Done(ChatConnectRes { workspace: "TNEWW5".into(), name: Some("Delta Lab".into()) }) }, t);
    let shown_now = drawn_at(&d, 2000, 60);
    assert!(shown_now.contains("Ship of Tools is connected to the Slack workspace Delta Lab. It keeps that workspace's three Slack codes on a, readable only by your login there. It is now checking which of the projects' channels"), "{shown_now}");
    assert!(shown_now.ends_with("Enter  open the channel list    Esc  close"), "{shown_now}");
    let said = serde_json::from_value(serde_json::json!({ "default": "TNEWW5", "workspaces": [{ "id": "TNEWW5", "name": "Delta Lab", "running": true, "unread": [] }] })).unwrap();
    d.on_evt(&host(), ChatEvt::Workspaces { reply: ChatReply::Done(said) }, t);
    let shown_now = drawn_at(&d, 2000, 60);
    assert!(shown_now.contains("Other projects keep the channels they have. Next, press Enter to choose this project's channel in Delta Lab; each project picks its own."), "{shown_now}");
    assert!(shown_now.ends_with("Enter  choose this project's channel in Delta Lab    Esc  close"), "{shown_now}");
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), t);
    d.tick(shown("ROW1"), t);
    let (outs, _) = key(&mut d, ENTER);
    assert_eq!(lists(&outs), 1);
    let res = serde_json::from_value(serde_json::json!({ "default": "TAAAA1", "workspaces": [
        { "id": "TAAAA1", "name": "Alpha Lab", "running": true }, { "id": "TNEWW5", "name": "Delta Lab", "running": true } ] })).unwrap();
    let outs = d.on_evt(&host(), ChatEvt::Workspaces { reply: ChatReply::Done(res) }, t);
    assert!(matches!(outs.as_slice(), [(_, OutgoingReq::ChatChannels { workspace: Some(w) })] if w == "TNEWW5"), "{outs:?}");
}

/// Connects workspace `w`, named `name`, from the step shown: its three codes pasted and the hub's reply.
fn connect_in_step(d: &mut ChatDrawer, w: &str, name: &str, at: Instant) {
    for t in [bot(), user(), app()] {
        paste(d, &t);
    }
    d.on_evt(&host(), ChatEvt::Connect { reply: ChatReply::Done(ChatConnectRes { workspace: w.into(), name: Some(name.into()) }) }, at);
}

/// B1b review S1: from a row whose workspace failed, a person connects another workspace (in the step, or through the
/// moved-to list's `Connect a Slack workspace`); the row still answers `unconfigured` for the failed one, so Enter on the
/// connected screen opens the channel list on the workspace just connected, never "press Enter again" forever. A
/// reconnect of the failed workspace that still fails shows the step again.
#[test]
fn connecting_another_workspace_from_a_failed_row_opens_its_list() {
    for through_list in [false, true] {
        let (mut d, at, _) = in_failed("tokens_refused");
        d.on_evt(&host(), running(true), at);
        if through_list {
            key(&mut d, SETUP);
            d.on_evt(&host(), running(true), at);
            let listed: ChatChannelsRes = serde_json::from_value(serde_json::json!({ "channels": [
                { "id": "C1", "name": "lab-general", "private": false, "member": true } ] })).unwrap();
            d.on_evt(&host(), ChatEvt::Channels { workspace: Some("TAAAA1".into()), reply: ChatReply::Done(listed) }, at);
            (0..3).for_each(|_| d.picker_step(true));
            assert!(d.picker_enter().is_empty() && d.in_setup(), "Connect a Slack workspace starts the step");
        }
        connect_in_step(&mut d, "TNEWW5", "Delta Lab", at);
        assert_eq!(resolves(&d.tick(shown("ROW1"), at).0), 1, "{through_list}");
        // Before the row's new answer, Enter says to wait.
        assert_eq!(key(&mut d, ENTER).1.notice.as_deref(), Some("setting up the chat: press Enter again in a few seconds to choose this project's channel"));
        d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unconfigured", "reason": "tokens_refused", "workspace": "TBBBB2" })), at);
        d.tick(shown("ROW1"), at);
        let (outs, asks) = key(&mut d, ENTER);
        assert_eq!((lists(&outs), d.picking(), d.in_setup(), asks.notice), (1, true, false, None), "{through_list}");
        let res = serde_json::from_value(serde_json::json!({ "default": "TAAAA1", "workspaces": [
            { "id": "TAAAA1", "name": "Alpha Lab", "running": true }, { "id": "TNEWW5", "name": "Delta Lab", "running": true } ] })).unwrap();
        let outs = d.on_evt(&host(), ChatEvt::Workspaces { reply: ChatReply::Done(res) }, at);
        assert!(matches!(outs.as_slice(), [(_, OutgoingReq::ChatChannels { workspace: Some(w) })] if w == "TNEWW5"), "{through_list}: {outs:?}");
        d.tick(shown("ROW1"), at);
        assert!(d.picking(), "{through_list}: the row's unconfigured answer does not take the list away");
    }
    // The failed workspace connected again, still failing: Enter gives the step back for the row's stand.
    let (mut d, at, _) = in_failed("tokens_refused");
    connect_in_step(&mut d, "TBBBB2", "Beta Lab", at);
    d.tick(shown("ROW1"), at);
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unconfigured", "reason": "tokens_refused", "workspace": "TBBBB2" })), at);
    key(&mut d, ENTER);
    d.tick(shown("ROW1"), at);
    assert!(d.in_setup() && d.title() == " chat · set up Slack ", "{}", d.title());
}

/// The private channel of the project shown, `#lab-private` (`G0PRIVATE1` of `T0AAAA`), read whole by the Slack app first
/// connected; then a new Slack app is connected, which is not in it: the hub's reload tells the watch to resync, the row
/// resolves again (with no name when `named` is false and the window never had one), and the watch is refused
/// `not_in_channel`. The drawer and the instant after.
fn unreadable(named: bool) -> (ChatDrawer, Instant) {
    use sot_protocol::ops::{ChatChannel, ChatService, ChatStatusEvt, ChatWatchRes, ChatWatchState};
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut d = ChatDrawer::default();
    d.set_login(LOGIN);
    d.on_connection(&host(), true, true, t0);
    d.tick(shown("ROW1"), t0);
    d.tick(shown("ROW1"), at(200));
    let mapped = |name: bool| {
        let mut j = serde_json::json!({ "state": "mapped", "workspace": "T0AAAA", "channel": "G0PRIVATE1", "delivers": false });
        if name {
            j["name"] = "lab-private".into();
        }
        answer("ROW1", j)
    };
    d.on_evt(&host(), mapped(named), at(210));
    let target: sot_protocol::ops::ChatTarget = serde_json::from_value(serde_json::json!({ "row": "ROW1", "workspace": "T0AAAA", "channel": "G0PRIVATE1" })).unwrap();
    let watched = |state| ChatEvt::Watched { target: target.clone(), reply: ChatReply::Done(ChatWatchRes { state }) };
    let outs = d.on_evt(&host(), watched(ChatWatchState::Complete), at(220));
    let [(_, OutgoingReq::ChatHistory { req })] = &outs[..] else { panic!("one read expected") };
    let page = serde_json::from_value(serde_json::json!({ "records": [top(1)], "next": null })).unwrap();
    d.on_evt(&host(), ChatEvt::History { req: req.clone(), reply: ChatReply::Done(page) }, at(230));
    // The connect's reload: the service ends and the row resolves again, the new app unable to read the channel's name.
    let channel = ChatChannel::try_from("G0PRIVATE1".to_string()).unwrap();
    let mut outs = d.on_evt(&host(), ChatEvt::Status(ChatStatusEvt { service: ChatService::Stopped, gap: false, resync: vec![channel] }), at(300));
    outs.extend(d.tick(shown("ROW1"), at(310)).0);
    d.on_evt(&host(), mapped(false), at(320));
    outs.extend(d.tick(shown("ROW1"), at(330)).0);
    assert!(outs.iter().any(|(_, r)| matches!(r, OutgoingReq::ChatWatch { .. })), "the watch is sent again");
    d.on_evt(&host(), ChatEvt::Watched { target, reply: ChatReply::Refused(ChatCode::NotInChannel) }, at(340));
    (d, at(350))
}

/// The picker of `unreadable`'s project (Primary+Shift+k): one workspace running, whose list lacks the channel.
fn picker_of_unreadable(d: &mut ChatDrawer, at: Instant) {
    assert_eq!(lists(&d.open_picker(shown("ROW1").as_ref()).unwrap()), 1);
    let running = serde_json::from_value(serde_json::json!({ "default": "T0AAAA", "workspaces": [{ "id": "T0AAAA", "name": "Alpha Lab", "running": true }] })).unwrap();
    d.on_evt(&host(), ChatEvt::Workspaces { reply: ChatReply::Done(running) }, at);
    let listed: ChatChannelsRes = serde_json::from_value(serde_json::json!({ "channels": [
        { "id": "C0PUBLIC1", "name": "lab-general", "private": false, "member": false } ] })).unwrap();
    d.on_evt(&host(), ChatEvt::Channels { workspace: Some("T0AAAA".into()), reply: ChatReply::Done(listed) }, at);
}

/// A project whose channel the connected Slack app is not in (a new app, or one never invited to a private channel) says
/// so in the title, and the body says what to type in Slack in place of the old messages; the channel keeps the name the
/// window had for it; the picker opened over it says the channel is missing from the list and why; a pasted link of it
/// says it may be private. Through the hub's refusal `not_in_channel` and the list Slack gives the new app.
#[test]
fn a_channel_the_connected_slack_app_is_not_in_says_so_and_what_to_type_in_slack() {
    let (mut d, at) = unreadable(true);
    assert_eq!(d.title(), " chat · #lab-private your Slack app cannot read this channel ");
    let body = d.body().expect("the body says why").lines(400, 10).join(" ");
    assert_eq!(body, format!("Your Slack app (the bot that reads the channel and posts for your agents) cannot read #lab-private: Slack says it is not in that channel, or that the channel no longer exists. A private channel lets in only the Slack apps its members invite, and a new Slack app is in no channel until it is invited. To fix it, in that channel in Slack, type /invite @{NAME}; this panel then shows the channel within two minutes."));
    let mut fonts = FontSystem::new();
    d.layout(&mut fonts, super::thread_tests::body(), 1.0, super::thread_tests::body());
    assert!(d.placed_text().is_empty(), "the old messages give way to the words");
    // A channel the window never had a name for is given by its ID and the address that opens it.
    let (d2, _) = unreadable(false);
    assert!(d2.body().unwrap().lines(400, 10).join(" ").starts_with("Your Slack app (the bot that reads the channel and posts for your agents) cannot read the channel with ID G0PRIVATE1 (to find it, open app.slack.com/client/T0AAAA/G0PRIVATE1 in your web browser): Slack says"));
    // The picker over it: the list Slack gives the new app lacks the channel, and the head says so first.
    picker_of_unreadable(&mut d, at);
    let head = d.body().unwrap().lines(400, 40).join(" ");
    assert!(head.starts_with(&format!("This project's channel, #lab-private, is not in this list: your Slack app (the bot that reads the channel and posts for your agents) is not in it, so this panel cannot show it. To keep it, in that channel in Slack, type /invite @{NAME}, then press Esc. To change it, pick another channel below.")), "{head}");
    assert!(head.contains(&format!("These are the channels of the Slack workspace Alpha Lab that your Slack app can see. Your Slack app reads only the channels it is in; picking a public channel adds it there. A private channel is listed only once someone in it invites your Slack app: in that channel in Slack, type /invite @{NAME}, then press Esc and open this list again.")), "{head}");
    assert_eq!(head.matches("(the bot").count(), 1, "your Slack app is said what it is once, at its first use: {head}");
    // Its link pasted: Slack answers a private channel the app is not in as a channel it does not have.
    assert_eq!(d.picker_paste("https://alphalab.slack.com/archives/G0PRIVATE1").unwrap().len(), 1);
    let paired = ChatEvt::Paired { row: "ROW1".into(), channel: Some(sot_protocol::ops::ChatChannel::try_from("G0PRIVATE1".to_string()).unwrap()), reply: ChatReply::Refused(ChatCode::NoSuchChannel) };
    d.on_evt(&host(), paired, at);
    assert_eq!(d.take_notice().as_deref(), Some(format!("not set: that channel is not in the Slack workspace Alpha Lab; if it is a private channel, your Slack app is not in it: in that channel in Slack, type /invite @{NAME}, then pick it again").as_str()));
}

/// B1 and S1: the connect's reply says nothing of the channels its Slack app cannot read. The connected screen asks
/// `chat.workspaces` at once and each second after, at most 12 times, until the workspace's entry says; until then it says
/// it is checking, and neither its words nor its keys offer to choose this project's channel (a person who did would lose the
/// channel the connect exists to keep); asks that end with no list say it could not check and what to type in Slack for a
/// project whose panel shows nothing. An entry that names none says the other projects keep their channels; one that names
/// some puts first, in view in a panel 60 cells by 12, which channel cannot be read and the line to type in Slack, naming the
/// connected Slack app as the hub names it, and its keys no longer say to choose a channel.
#[test]
fn the_connected_screen_names_the_channels_the_new_slack_app_is_not_in() {
    // The login gives the person's app another name than the hub's, `Ship of Tools`.
    let (mut d, t) = in_step_as("", "not_connected");
    for token in [bot(), user(), app()] {
        paste(&mut d, &token);
    }
    let outs = d.on_evt(&host(), ChatEvt::Connect { reply: ChatReply::Done(ChatConnectRes { workspace: "T0AAAA".into(), name: Some("Alpha Lab".into()) }) }, t);
    assert_eq!(lists(&outs), 1, "the screen asks at once");
    let unknown = drawn(&d).replace('\n', " ");
    assert!(unknown.contains("readable only by your login there. It is now checking which of the projects' channels your Slack app (the bot that reads the channel and posts for your agents) can read; this takes a few seconds."), "{unknown}");
    assert!(!unknown.contains("press Enter") && !unknown.contains("Other projects keep") && !unknown.contains(" is not in "), "nothing is offered or said of the channels before it is known: {unknown}");
    assert!(unknown.ends_with("Enter  open the channel list    Esc  close"), "{unknown}");
    // An entry that does not say yet: asked again each second, 12 times in all, and never between.
    let entry = |unread: serde_json::Value| {
        let res = serde_json::from_value(serde_json::json!({ "default": "T0AAAA", "workspaces": [{ "id": "T0AAAA", "name": "Alpha Lab", "running": true, "app_name": NAME, "app_id": "A0NEW", "unread": unread }] })).unwrap();
        ChatEvt::Workspaces { reply: ChatReply::Done(res) }
    };
    d.on_evt(&host(), entry(serde_json::Value::Null), t);
    assert_eq!(d.next_ask(), Some(t + Duration::from_secs(1)));
    assert_eq!(lists(&d.tick(shown("ROW1"), t + Duration::from_millis(900)).0), 0, "not before its pace");
    let asked: usize = (1..=30).map(|n| lists(&d.tick(shown("ROW1"), t + Duration::from_secs(n)).0)).sum();
    assert_eq!(asked + 1, 12, "12 asks in all");
    // The asks ended with no list: it says it could not check, and what to type for a project whose panel shows nothing.
    let unchecked = drawn(&d).replace('\n', " ");
    assert!(unchecked.contains(&format!("readable only by your login there. Ship of Tools could not check which of the projects' channels your Slack app (the bot that reads the channel and posts for your agents) can read. If a project's panel shows nothing, open its channel in Slack and type /invite @{NAME}.")), "{unchecked}");
    assert!(!unchecked.contains("press Enter") && !unchecked.contains("checking"), "{unchecked}");
    assert!(unchecked.ends_with("Enter  open the channel list    Esc  close"), "{unchecked}");
    // The entry says the app reads every mapped channel: the other projects keep theirs.
    d.on_evt(&host(), entry(serde_json::json!([])), t);
    assert!(drawn(&d).replace('\n', " ").contains("readable only by your login there. Other projects keep the channels they have. Next, press Enter"));
    // It names one: what to do leads, in view at once, with the Slack app named as the hub names it.
    d.on_evt(&host(), entry(serde_json::json!([{ "id": "G0PRIVATE1", "name": "lab-private" }])), t);
    let body = d.body().unwrap();
    let one = format!("Your Slack app (the bot that reads the channel and posts for your agents) is not in #lab-private yet, so the project that uses #lab-private shows nothing here. To keep that channel, open #lab-private in Slack and type /invite @{NAME} in its message box. Do not choose another channel here. (A new Slack app is in no channel until someone in the channel invites it.)\n\nShip of Tools is connected to the Slack workspace Alpha Lab. It keeps that workspace's three Slack codes on a, readable only by your login there.");
    assert_eq!(body.head, one);
    assert_eq!(body.pin, "Enter  open the channel list    Esc  close");
    let rows = body.lines(60, 12);
    let shown_first = rows[..12 - 2].join(" ");
    assert!(rows[0].starts_with("Your Slack app") && shown_first.contains("#lab-private") && shown_first.contains(&format!("type /invite @{NAME} in its message box.")), "{rows:#?}");
    // Several, one known only by its ID: Slack's search does not find a channel by its ID, so the address that opens it is given.
    d.on_evt(&host(), entry(serde_json::json!([{ "id": "G0PRIVATE1", "name": "lab-private" }, { "id": "G0PRIVATE2", "name": "lab-data" }, { "id": "C0PUBLIC1" }])), t);
    assert!(d.body().unwrap().head.starts_with(&format!("Your Slack app (the bot that reads the channel and posts for your agents) is not in #lab-private, #lab-data and the channel with ID C0PUBLIC1 (to find it, open app.slack.com/client/T0AAAA/C0PUBLIC1 in your web browser) yet, so the projects that use them show nothing here. To keep those channels, open each one in Slack and type /invite @{NAME} in its message box. Do not choose other channels here.")), "{}", d.body().unwrap().head);
    let (mut d, t) = in_step_as("", "not_connected");
    for token in [bot(), user(), app()] {
        paste(&mut d, &token);
    }
    d.on_evt(&host(), ChatEvt::Connect { reply: ChatReply::Done(ChatConnectRes { workspace: "T0AAAA".into(), name: Some("Alpha Lab".into()) }) }, t);
    d.on_evt(&host(), entry(serde_json::json!([{ "id": "C0PUBLIC1" }])), t);
    let head = d.body().unwrap().head;
    assert!(head.starts_with("Your Slack app (the bot that reads the channel and posts for your agents) is not in the channel with ID C0PUBLIC1 (to find it, open app.slack.com/client/T0AAAA/C0PUBLIC1 in your web browser) yet, so the project that uses the channel with ID C0PUBLIC1 (to find it, open app.slack.com/client/T0AAAA/C0PUBLIC1 in your web browser) shows nothing here."), "{head}");
}

/// The connected screen's asks end when the entry says which channels are unreadable (a known list, empty or not) and when
/// an older hub has no such answer; each ends them at once, so nothing is asked after it however long the screen stays, and an
/// older hub's screen says it could not check.
#[test]
fn the_connected_screens_asks_end_with_a_known_list_or_an_older_hub() {
    let connect = || {
        let (mut d, t) = in_step_as("", "not_connected");
        for token in [bot(), user(), app()] {
            paste(&mut d, &token);
        }
        let outs = d.on_evt(&host(), ChatEvt::Connect { reply: ChatReply::Done(ChatConnectRes { workspace: "T0AAAA".into(), name: Some("Alpha Lab".into()) }) }, t);
        assert_eq!(lists(&outs), 1, "the screen asks at once");
        (d, t)
    };
    let entry = |unread: serde_json::Value| {
        let res = serde_json::from_value(serde_json::json!({ "default": "T0AAAA", "workspaces": [{ "id": "T0AAAA", "name": "Alpha Lab", "running": true, "unread": unread }] })).unwrap();
        ChatEvt::Workspaces { reply: ChatReply::Done(res) }
    };
    for list in [serde_json::json!([]), serde_json::json!([{ "id": "G0PRIVATE1", "name": "lab-private" }])] {
        let (mut d, t) = connect();
        assert_eq!(lists(&d.tick(shown("ROW1"), t + Duration::from_secs(1)).0), 1, "the second ask");
        d.on_evt(&host(), entry(list.clone()), t + Duration::from_secs(1));
        assert_eq!(lists(&d.tick(shown("ROW1"), t + Duration::from_secs(5)).0), 0, "a known list {list} ends the asks");
        assert_eq!(d.next_ask(), None, "{list}: and wakes nothing");
    }
    let (mut d, t) = connect();
    d.on_evt(&host(), ChatEvt::Workspaces { reply: ChatReply::Unsupported }, t);
    assert_eq!(lists(&d.tick(shown("ROW1"), t + Duration::from_secs(5)).0), 0, "an older hub ends the asks");
    assert_eq!(d.next_ask(), None);
    let said = drawn(&d).replace('\n', " ");
    assert!(said.contains("could not check which of the projects' channels") && !said.contains("checking"), "{said}");
}

/// S2: a channel the connected Slack app cannot read names that app in its invite as the hub names it: until the hub's
/// `chat.workspaces` answer the name the login gives the person's app, after it the name Slack gives the app's bot. The
/// drawer asks the hub once per link.
#[test]
fn the_unreadable_channels_invite_names_the_slack_app_the_hub_names() {
    let (mut d, at) = unreadable(true);
    d.set_login("");
    let said = |d: &ChatDrawer| d.body().unwrap().lines(400, 10).join(" ");
    assert!(said(&d).contains("type /invite @Ship of Tools; this panel then shows"), "{}", said(&d));
    assert_eq!(lists(&d.tick(shown("ROW1"), at).0), 1, "the hub is asked which Slack app it is");
    assert_eq!(lists(&d.tick(shown("ROW1"), at + Duration::from_millis(10)).0), 0, "once");
    let res = serde_json::from_value(serde_json::json!({ "default": "T0AAAA", "workspaces": [{ "id": "T0AAAA", "running": true, "app_name": NAME }] })).unwrap();
    d.on_evt(&host(), ChatEvt::Workspaces { reply: ChatReply::Done(res) }, at);
    assert!(said(&d).contains(&format!("type /invite @{NAME}; this panel then shows")), "{}", said(&d));
    assert_eq!(lists(&d.tick(shown("ROW1"), at + Duration::from_millis(20)).0), 0, "the answer is held");
}
