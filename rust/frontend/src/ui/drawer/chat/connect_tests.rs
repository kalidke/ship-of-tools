use std::time::{Duration, Instant};

use cosmic_text::FontSystem;
use sot_protocol::chat::{slack_app_manifest, slack_app_url};
use sot_protocol::ops::{ChatChannelsRes, ChatCode, ChatConnectRes, ChatUnconfigured};

use super::compose::Edit;
use super::connect::{Asks, Connect, Screen};
use super::drawer::ChatDrawer;
use super::owner::Out;
use super::row_channel_tests::answer;
use super::thread_tests::{body, channel, shown, top};
use crate::net::dial::HostKey;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use crate::ui::input::keybindings::Action;

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

fn app() -> String {
    format!("{}{}", "xapp-", "VKW".repeat(16))
}

/// Whether `text` holds any two letters of a fixture token's tail; names the place, never the text.
fn leaks(text: &str) -> bool {
    ["QJ", "JX", "XQ", "VK", "KW", "WV"].iter().any(|pair| text.contains(pair))
}

fn unconfigured(reason: &str) -> ChatEvt {
    answer("ROW1", serde_json::json!({ "state": "unconfigured", "reason": reason }))
}

fn resolves(outs: &[Out]) -> usize {
    outs.iter().filter(|(_, r)| matches!(r, OutgoingReq::ChatResolve { .. })).count()
}

fn lists(outs: &[Out]) -> usize {
    outs.iter().filter(|(_, r)| matches!(r, OutgoingReq::ChatChannels)).count()
}

fn connects(outs: &[Out]) -> Vec<(String, String)> {
    outs.iter()
        .filter_map(|(_, r)| match r {
            OutgoingReq::ChatConnect { bot_token, app_token } => Some((bot_token.expose().to_string(), app_token.expose().to_string())),
            _ => None,
        })
        .collect()
}

fn connected(workspace: &str) -> ChatEvt {
    ChatEvt::Connect { reply: ChatReply::Done(ChatConnectRes { workspace: workspace.into() }) }
}

fn refused(code: ChatCode) -> ChatEvt {
    ChatEvt::Connect { reply: ChatReply::Refused(code) }
}

/// A drawer whose shown row ROW1 answered `unconfigured` for `reason`, with the step begun, and the instant after.
fn in_step(reason: &str) -> (ChatDrawer, Instant) {
    let t0 = Instant::now();
    let mut d = ChatDrawer::default();
    d.set_login(LOGIN);
    d.on_connection(&host(), true, t0);
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

#[test]
fn the_connect_step_says_each_reason_and_its_next_action() {
    for reason in ["not_connected", "start_failed"] {
        let (d, _) = in_step(reason);
        assert_eq!(d.title(), " chat · set up Slack ", "{reason}");
        let body = drawn(&d);
        assert!(body.contains("First, connect Ship of Tools to your Slack workspace"), "{reason}: the start screen");
        assert!(body.contains("Each person makes their own app"), "{reason}: one app per person");
        assert!(body.contains("Enter  start    Right  I made the app in an earlier try: go to step 2    Esc  close"), "{reason}");
    }
    // The refused tokens' screen names the person's own app at its first frame.
    let (d, _) = in_step("tokens_refused");
    let body = drawn(&d);
    assert!(body.contains("Slack no longer accepts Ship of Tools' tokens. Press Enter: your list of Slack apps opens"));
    assert!(body.contains(&format!("If {NAME} is in it")), "screen 8 names the app with the login at once");
    assert!(body.contains("Enter  open your Slack apps    Left  make the app again    Esc  close"));

    // Each step names itself and its keys.
    let mut c = step(ChatUnconfigured::NotConnected);
    c.key(ENTER, None, false);
    assert_eq!(c.title(), " chat · set up Slack · step 1 of 3: make the app ");
    let make = c.body();
    assert!(make.head.contains(&format!("already named {NAME}, after the name you sign in to this computer with")));
    assert_eq!(make.pin, "Enter  next step    Left/Right  move between steps    Esc  close");
    c.key(ENTER, None, false);
    assert_eq!(c.title(), " chat · set up Slack · step 2 of 3: the first token ");
    assert!(c.body().head.contains("Bot User OAuth Token   not pasted yet"));
    c.key(RIGHT, None, false);
    assert_eq!(c.title(), " chat · set up Slack · step 3 of 3: the second token ");
    assert!(c.body().head.contains("  4. Click Add Scope and choose connections:write from the list."));
    assert!(c.body().head.contains("App-Level Token   not pasted yet"));
    // No login names the app plainly, and says nothing of a login.
    let mut c = Connect::new(host(), ChatUnconfigured::NotConnected, "", true);
    c.key(ENTER, None, false);
    assert!(c.body().head.contains("already named Ship of Tools (you can change it there)"));
}

#[test]
fn left_and_right_move_between_steps_and_the_refused_screen_opens_its_pages() {
    let mut c = step(ChatUnconfigured::NotConnected);
    // Right on the first screen goes straight to step 2 and opens nothing.
    assert_eq!(c.key(RIGHT, None, false).1, Asks::default());
    assert_eq!(c.screen(), &Screen::Bot);
    c.key(LEFT, None, false);
    assert_eq!(c.screen(), &Screen::Make, "Left from step 2 is step 1, with no page opened again");
    c.key(LEFT, None, false);
    assert_eq!(c.screen(), &Screen::First);
    c.key(LEFT, None, false);
    assert_eq!(c.screen(), &Screen::First, "nothing before the first screen");

    let mut c = step(ChatUnconfigured::TokensRefused);
    let (_, asks) = c.key(ENTER, None, false);
    assert_eq!(asks.open.as_deref(), Some("https://api.slack.com/apps"));
    assert_eq!(c.screen(), &Screen::Bot, "the apps' list, then step 2");
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
    assert!(!leaks(&drawn(&d)), "step 3 after the first paste draws a token byte past its prefix");
    key(&mut d, LEFT);
    let body = drawn(&d);
    assert!(!leaks(&body), "step 2 draws a token byte past its prefix");
    assert!(body.contains(&format!("Bot User OAuth Token   pasted: xoxb-…, {} characters", bot().len())));
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
    d.on_connection(&host(), false, at);
    d.on_connection(&host(), true, at);
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
fn the_tokens_leave_with_the_request_and_nothing_keeps_them() {
    let mut c = step(ChatUnconfigured::NotConnected);
    assert!(c.key(PASTE, Some(&bot()), false).0.is_none());
    let (out, _) = c.key(PASTE, Some(&app()), false);
    assert_eq!(connects(&[out.expect("both tokens held: one connect")]), [(bot(), app())], "one request carries both tokens");
    // While Slack checks them the step keeps both, so a refused one can send the person back with the other kept (5c).
    assert_eq!(c.held(), (Some(bot().as_str()), Some(app().as_str())));
    for k in [ENTER, PASTE, RIGHT] {
        assert!(c.key(k, Some(&app()), false).0.is_none(), "{k:?} while connecting");
    }
    c.answered(ChatReply::Done(ChatConnectRes { workspace: "T1".into() }));
    assert_eq!(c.screen(), &Screen::Connected("T1".into()));
    assert_eq!(c.held(), (None, None), "a connected step keeps no token");
}

#[test]
fn a_paste_that_is_no_slack_token_is_dropped_unshown() {
    let mut c = step(ChatUnconfigured::NotConnected);
    c.key(RIGHT, None, false);
    let before = c.body().lines(100, 60);
    for bad in ["QJXQJX", "xoxb-", "xoxp-QJXQJX", &format!("{} {}", bot(), app()), &format!("xoxb-{}", "Q".repeat(600))] {
        let (out, asks) = c.key(PASTE, Some(bad), false);
        assert!(out.is_none());
        assert_eq!(asks.notice.as_deref(), Some("that is not a Slack token: the bot token starts xoxb-, the app-level token xapp-"));
        assert_eq!(c.body().lines(100, 60), before, "a refused paste changes nothing drawn");
        assert_eq!(c.held(), (None, None), "a refused paste is held nowhere");
    }
}

#[test]
fn enter_opens_the_page_until_both_tokens_are_held_then_connects_once() {
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
    assert_eq!(d.title(), " chat · set up Slack · step 2 of 3: the first token ");
    // The tokens in either order: the app-level token first, then the bot token, connects once.
    assert!(paste(&mut d, &app()).0.is_empty());
    assert_eq!(d.title(), " chat · set up Slack · step 2 of 3: the first token ", "the missing token's step");
    let (outs, _) = paste(&mut d, &bot());
    assert_eq!(connects(&outs), [(bot(), app())]);
    assert_eq!(outs[0].0, host(), "to the host that answered unconfigured");
    for k in [ENTER, PASTE] {
        assert!(d.setup_key(k, Some(&bot())).0.is_empty(), "{k:?} sends no second connect");
    }
}

#[test]
fn a_refused_token_goes_back_to_its_step_and_the_other_is_kept() {
    let mut c = step(ChatUnconfigured::NotConnected);
    c.key(PASTE, Some(&bot()), false);
    c.key(PASTE, Some(&app()), false);
    c.answered(ChatReply::Refused(ChatCode::AppTokenRefused));
    assert_eq!(c.screen(), &Screen::App);
    assert!(c.body().head.starts_with("Slack did not accept the token you pasted. Make sure you copied the App-Level Token"));
    assert_eq!(c.held(), (Some(bot().as_str()), None));
    assert!(c.key(PASTE, Some(&app()), false).0.is_some(), "the first token was kept: a new second token connects again");

    c.answered(ChatReply::Refused(ChatCode::BotTokenRefused));
    assert_eq!(c.screen(), &Screen::Bot);
    assert!(c.body().head.starts_with("Slack did not accept the token you pasted. Make sure you copied the Bot User OAuth Token"));
    assert_eq!(c.held(), (None, Some(app().as_str())));
    assert!(c.key(PASTE, Some(&bot()), false).0.is_some(), "the second token was kept");

    // Any other answer drops both and says why on the first screen.
    c.answered(ChatReply::Refused(ChatCode::OtherWorkspace));
    assert_eq!(c.screen(), &Screen::First);
    assert!(c.body().head.starts_with("Not connected: the chat is set up for another Slack workspace."));
    assert_eq!(c.held(), (None, None));
    c.key(PASTE, Some(&bot()), false);
    c.key(PASTE, Some(&app()), false);
    c.answered(ChatReply::Unsupported);
    assert!(c.body().head.starts_with("Not connected: the hub's daemon must be updated to connect Slack."));
    // A step that sent nothing is not changed by a reply.
    let mut c = step(ChatUnconfigured::NotConnected);
    c.key(PASTE, Some(&bot()), false);
    c.answered(ChatReply::Refused(ChatCode::BotTokenRefused));
    assert_eq!((c.screen(), c.held().0), (&Screen::App, Some(bot().as_str())), "a reply to no connect of this step's");
}

/// Escape stops nothing at the hub: the keys line says so, and the older connect's reply never acts on a step begun since,
/// which sends nothing until that reply.
#[test]
fn an_older_connects_reply_never_acts_on_a_newer_step() {
    let (mut d, t) = in_step("not_connected");
    paste(&mut d, &bot());
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
    let (outs, asks) = paste(&mut d, &app());
    assert!(outs.is_empty(), "no second connect while the first is unanswered");
    assert!(asks.notice.unwrap().starts_with("Slack is still checking the tokens sent before"));
    // The first connect's refusal of a bot token is not this step's.
    d.on_evt(&host(), refused(ChatCode::BotTokenRefused), t + Duration::from_millis(230));
    assert_eq!(d.title(), " chat · set up Slack · step 3 of 3: the second token ");
    assert!(!drawn(&d).contains("did not accept"));
    // Now Enter sends the held tokens, once.
    assert_eq!(connects(&key(&mut d, ENTER).0), [(bot(), app())]);
}

#[test]
fn a_modal_screen_ends_at_a_row_change_a_hidden_drawer_and_a_link_change() {
    type End = fn(&mut ChatDrawer, Instant);
    let ends: [(&str, End); 3] = [
        ("row change", |d, t| drop(d.tick(shown("ROW2"), t))),
        ("hidden drawer", |d, _| d.hide()),
        ("link change", |d, t| drop(d.on_connection(&host(), false, t))),
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
    let listed: ChatChannelsRes = serde_json::from_value(serde_json::json!({ "channels": [
        { "id": "C1", "name": "lab-general", "private": false, "member": true } ] })).unwrap();
    d.on_evt(&host(), ChatEvt::Channels { reply: ChatReply::Done(listed) }, at);
    let rows = d.body().unwrap().lines(100, 30);
    assert!(rows.iter().any(|r| r.contains(&format!("/invite @{NAME}"))), "the picker's invite names the person's app");
    assert!(rows.iter().any(|r| r.trim_start_matches(['>', ' ']).starts_with("Connect Slack again")), "Connect Slack again is offered");
    (0..3).for_each(|_| d.picker_step(true));
    assert!(d.picker_enter().is_empty(), "Connect Slack again sends nothing");
    assert!(d.in_setup() && !d.picking());
    paste(&mut d, &bot());
    paste(&mut d, &app());
    d.on_evt(&host(), connected("T1"), at);
    d.tick(shown("ROW1"), at + Duration::from_millis(10));
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "mapped", "workspace": "WS1", "channel": "CH1", "delivers": false })), at);
    d.tick(shown("ROW1"), at + Duration::from_millis(20));
    d.layout(&mut fonts, body(), 1.0, body());
    assert_eq!(d.title(), " chat · connected ");
    assert!(d.placed_text().is_empty(), "the connected screen draws over no record");
    assert!(drawn(&d).contains("on the computer that runs the chat"));
}

#[test]
fn a_connect_reply_resolves_the_row_again_and_enter_then_opens_the_picker() {
    let (mut d, t) = in_step("not_connected");
    paste(&mut d, &bot());
    paste(&mut d, &app());
    d.on_evt(&host(), connected("T1"), t);
    assert_eq!(resolves(&d.tick(shown("ROW1"), t).0), 1, "the reply resolves the row at once");
    assert_eq!(d.title(), " chat · connected ");
    assert!(drawn(&d).contains("Ship of Tools is connected to the Slack workspace T1."));
    // Before the row's new answer, Enter says to wait; after it, Enter opens the picker and asks for the channels.
    let (outs, asks) = key(&mut d, ENTER);
    assert!(outs.is_empty() && d.in_setup());
    assert_eq!(asks.notice.as_deref(), Some("reading the hub again: press Enter in a moment"));
    d.on_evt(&host(), answer("ROW1", serde_json::json!({ "state": "unmapped" })), t);
    d.tick(shown("ROW1"), t);
    assert!(d.in_setup(), "the connected screen waits for the person");
    let (outs, _) = key(&mut d, ENTER);
    assert_eq!((lists(&outs), d.in_setup(), d.picking()), (1, false, true));

    // A refusal resolves the row again too, and so does a connect whose reply was lost.
    let (mut d, t) = in_step("not_connected");
    paste(&mut d, &bot());
    paste(&mut d, &app());
    d.on_evt(&host(), refused(ChatCode::Failed), t);
    assert_eq!(resolves(&d.tick(shown("ROW1"), t).0), 1);
    let (mut d, t) = in_step("not_connected");
    paste(&mut d, &bot());
    paste(&mut d, &app());
    d.on_evt(&host(), ChatEvt::ConnectUnknown, t);
    assert!(!d.in_setup(), "an unknown outcome ends the step: the row's new answer decides what shows");
    assert_eq!(d.take_notice().as_deref(), Some("Slack may be connected: the hub did not answer before the link ended; reading it again"));
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
        d.scroll_modal(false, Instant::now(), Duration::ZERO);
    }
    let last = d.body().unwrap().lines(w, h);
    assert!(last.join(" ").contains("App-Level Token   not pasted yet"), "PageDown reaches the field");
    assert_eq!(last[h - 1], first[h - 1], "the keys stay");
    d.scroll_modal(true, Instant::now(), Duration::ZERO);
    assert_ne!(d.body().unwrap().lines(w, h), last, "PageUp moves back");
    // A new screen starts at its top.
    key(&mut d, LEFT);
    assert_eq!(d.body().unwrap().scroll, 0);
}
