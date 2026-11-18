//! The Connect step: what the Chat drawer's modal slot shows for a row whose hub answers `unconfigured`, or after `Connect
//! Slack again` in the picker, or a person's post the hub refused for want of their token, one screen at a time (the start,
//! the refused tokens' screen, the wrong app's screen, or the post-as-you screen; then
//! steps 1 to 3, with Left and Right between them; then connecting and connected). A pasted token is held here as a
//! `ChatToken`, whose `Debug` prints no byte of it, and is drawn as its five-character prefix and its length alone; the
//! three tokens leave in one `chat.connect`. A refused token sends the person back to its step and the others are kept; any
//! other reply drops all three. The drawer ends the
//! step as it ends any modal screen, dropping whatever it holds.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use sot_protocol::chat::{slack_app_manifest, slack_app_name, slack_app_url};
use sot_protocol::ops::{ChatCode, ChatConnectRes, ChatToken, ChatUnconfigured, ChatWorkspaceInfo, ChatWorkspacesRes};

use crate::net::dial::HostKey;
use crate::net::transport::{ChatReply, OutgoingReq};
use crate::ui::input::keypress::KeyPress;
use crate::ui::*;
use sot_platform::host::env::Env;

use super::owner::Out;
use super::picker::{workspace_label, workspace_words, Body};
use crate::ui::chrome::cell_row;
use super::row_channel::{computer, log_words};
use super::{prompt_step, PromptStep};

/// Slack's list of the person's apps, which the refused tokens' screen opens.
const SLACK_APPS: &str = "https://api.slack.com/apps";
/// How often, and how many times, the connected screen asks `chat.workspaces` while its entry does not yet say which mapped
/// channels the Slack app connected can read: the hub's reads take at most 10 s after its reply (`UNREAD_WAIT`).
const UNREAD_PACE: Duration = Duration::from_secs(1);
const UNREAD_ASKS: u8 = 12;
const BOT_PREFIX: &str = "xoxb-";
const USER_PREFIX: &str = "xoxp-";
const APP_PREFIX: &str = "xapp-";
/// The copy and paste chords as this computer's keyboard names them.
const PRIMARY: &str = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };
/// What the person makes in step 1, as a screen names it at its first use there; later it is `your Slack app`.
const APP: &str = "your Slack app (the bot that reads the channel and posts for your agents)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Screen {
    /// Screen 1, or for tokens Slack no longer accepts, screen 8, or for an app-level token of another app, the wrong app's.
    First,
    Make,
    /// Step 2: the user token and the bot token, from one page, in either order.
    Tokens,
    /// Step 3: the app-level token.
    App,
    Connecting,
    /// The workspace the hub's reply named, and its name when the reply gave one.
    Connected(String, Option<String>),
}

/// Which token Slack refused, for its step's words until a new one is pasted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refused {
    Bot,
    User,
    App,
}

/// What a key asks of the window besides the step's own change.
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::ui) struct Asks {
    /// An address for this computer's browser (it holds no secret).
    pub open: Option<String>,
    /// Text for the clipboard: the app's manifest.
    pub copy: Option<String>,
    /// Escape: the drawer closes, which ends the step.
    pub close: bool,
    /// Enter on the connected screen: the step is done, and the project's channel is chosen next.
    pub done: bool,
    /// Ctrl+Shift+k on the first screen of a row whose workspace failed: the channel list of another workspace.
    pub away: bool,
    pub notice: Option<String>,
}

#[derive(Debug)]
pub(super) struct Connect {
    host: HostKey,
    reason: ChatUnconfigured,
    /// The step began from the hub's `unconfigured`, so another answer ends it while the person is still pasting; a step a
    /// person asked for (the picker's last entry) or a refused post brought stays until they leave it.
    follows: bool,
    /// The step began from a person's post into this workspace, which the hub refused `no_user_token`: its first screen is
    /// the post-as-you screen, with the refused tokens' keys.
    post_in: Option<String>,
    /// What `chat.workspaces` last said of the workspace the screen is about (the post's, or the one connected) while it
    /// runs: the Slack app it is connected to, which the post-as-you screen names and opens as the one to keep, and the
    /// mapped channels that app cannot read, which the connected screen names.
    about: Option<ChatWorkspaceInfo>,
    /// While the connected screen waits for `about` to say which mapped channels its Slack app cannot read: when it next asks
    /// `chat.workspaces` (none: at once) and how many asks are left.
    asking: Option<(Option<Instant>, u8)>,
    screen: Screen,
    bot: Option<ChatToken>,
    user: Option<ChatToken>,
    app: Option<ChatToken>,
    refused: Option<Refused>,
    /// Why the last connect failed for another cause: the first screen leads with these words.
    failed: Option<String>,
    /// The login the app is named for (`slack_app_name`), read when the step began.
    login: String,
    /// The workspace of the row's channel, when the hub named it as the one whose connection failed, and its name once known.
    failed_in: Option<(String, Option<String>)>,
    /// Another workspace runs on the hub, so the project's channel can be moved there (`on_workspaces`).
    elsewhere: bool,
}

/// How a pasted token is shown: its prefix and its length, never a byte after the prefix.
fn shown(token: Option<&ChatToken>) -> String {
    match token {
        None => "not pasted yet".into(),
        Some(t) => format!("pasted: {}…, {} characters", &t.expose()[..BOT_PREFIX.len()], t.expose().chars().count()),
    }
}

impl Connect {
    pub(super) fn new(host: HostKey, reason: ChatUnconfigured, login: &str, follows: bool) -> Connect {
        Connect {
            host,
            reason,
            follows,
            post_in: None,
            about: None,
            asking: None,
            screen: Screen::First,
            bot: None,
            user: None,
            app: None,
            refused: None,
            failed: None,
            login: login.to_string(),
            failed_in: None,
            elsewhere: false,
        }
    }

    /// The step for a row whose channel is in `workspace`, the one the hub says failed.
    pub(super) fn in_workspace(self, workspace: Option<String>) -> Connect {
        Connect { failed_in: workspace.map(|w| (w, None)), ..self }
    }

    /// The hub's running workspaces: for a refused post or a connect made, what the hub says of that workspace (its Slack
    /// app, and once known the channels that app cannot read, which ends the connected screen's asks); for a failed
    /// workspace, whether another runs, and the failed one's name. An older hub, which has no such answer, ends the asks.
    pub(super) fn on_workspaces(&mut self, reply: &ChatReply<ChatWorkspacesRes>) {
        let res = match reply {
            ChatReply::Done(res) => res,
            ChatReply::Unsupported => {
                self.asking = None;
                return;
            }
            ChatReply::Refused(_) => return,
        };
        let about = match &self.screen {
            Screen::Connected(w, _) => Some(w),
            _ => self.post_in.as_ref(),
        };
        if let Some(w) = about {
            self.about = res.workspaces.iter().find(|e| &e.id == w && e.running).cloned();
        }
        if self.about.as_ref().is_some_and(|a| a.unread.is_some()) {
            self.asking = None;
        }
        let Some((failed, name)) = self.failed_in.as_mut() else { return };
        self.elsewhere = res.workspaces.iter().any(|w| w.running && &w.id != failed);
        if let Some(w) = res.workspaces.iter().find(|w| &w.id == failed) {
            name.clone_from(&w.name);
        }
    }

    /// The workspace a connect made, once the hub answered it.
    pub(super) fn connected(&self) -> Option<&str> {
        match &self.screen {
            Screen::Connected(w, ..) => Some(w),
            _ => None,
        }
    }

    /// Whether Ctrl+Shift+k moves the project to another workspace's channel: on the first screen, for a row whose
    /// workspace failed, while another runs.
    fn can_move(&self) -> bool {
        self.screen == Screen::First && self.failed_in.is_some() && self.elsewhere
    }

    /// The step a person's refused post into `workspace` brings: the hub holds no token that posts as them, or Slack stopped
    /// accepting it. The caller asks `chat.workspaces`, whose answer names the Slack app to keep.
    pub(super) fn for_post(host: HostKey, login: &str, workspace: String) -> Connect {
        Connect { post_in: Some(workspace), ..Connect::new(host, ChatUnconfigured::NotConnected, login, false) }
    }

    /// The connected screen's next `chat.workspaces`, when one is due at `now`: at once after the reply, then each
    /// `UNREAD_PACE`, at most `UNREAD_ASKS` in all, until the answer says which channels the Slack app cannot read.
    pub(super) fn ask_unread(&mut self, now: Instant) -> Option<Out> {
        let (due, left) = self.asking.as_mut()?;
        if due.is_some_and(|d| now < d) {
            return None;
        }
        if *left == 0 {
            self.asking = None;
            return None;
        }
        (*due, *left) = (Some(now + UNREAD_PACE), *left - 1);
        Some((self.host.clone(), OutgoingReq::ChatWorkspaces))
    }

    /// When the connected screen next asks, for the window to wake then.
    pub(super) fn next_ask(&self) -> Option<Instant> {
        self.asking.and_then(|(due, _)| due)
    }

    /// The page Enter opens on a screen whose Slack app exists: that app's own permissions page when the hub gave the id of
    /// the app the post's workspace is connected to, else the person's list of Slack apps.
    fn apps_page(&self) -> String {
        let id = self.post_in.as_ref().and(self.about.as_ref()).and_then(|a| a.app_id.as_deref());
        match id.filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric())) {
            Some(id) => format!("{SLACK_APPS}/{id}/oauth"),
            None => SLACK_APPS.into(),
        }
    }

    /// Whether any token is pasted: a late reply to an older post leaves such a step alone.
    pub(super) fn holds_token(&self) -> bool {
        self.bot.is_some() || self.user.is_some() || self.app.is_some()
    }

    pub(super) fn host(&self) -> &HostKey {
        &self.host
    }

    #[cfg(test)]
    pub(super) fn screen(&self) -> &Screen {
        &self.screen
    }

    /// The tokens held, for a test that they are dropped (a `ChatToken`'s `Debug` would say nothing either way).
    #[cfg(test)]
    pub(super) fn held(&self) -> (Option<&str>, Option<&str>, Option<&str>) {
        (self.bot.as_ref().map(ChatToken::expose), self.user.as_ref().map(ChatToken::expose), self.app.as_ref().map(ChatToken::expose))
    }

    /// Whether another answer for the row ends the step: one begun from `unconfigured`, while the person is still pasting. A
    /// connect in flight or done waits for the person.
    pub(super) fn ends_on_another_answer(&self) -> bool {
        self.follows && !matches!(self.screen, Screen::Connecting | Screen::Connected(..))
    }

    pub(super) fn connecting(&self) -> bool {
        self.screen == Screen::Connecting
    }

    /// The hub's reason, read again: the first screen follows it.
    pub(super) fn reason(&mut self, reason: ChatUnconfigured) {
        (self.reason, self.post_in) = (reason, None);
    }

    /// Whether the first screen is one whose app exists but whose tokens are copied again: its keys open the person's apps.
    fn refused_screen(&self) -> bool {
        matches!(self.reason, ChatUnconfigured::TokensRefused | ChatUnconfigured::WrongApp) || self.post_in.is_some()
    }

    fn name(&self) -> String {
        slack_app_name(&self.login)
    }

    /// A key, by its action, with the clipboard's text for a paste; `busy`: an earlier connect is still unanswered, so none
    /// is sent now. The request it sends, if any, and what it asks of the window.
    pub(super) fn key(&mut self, action: Option<Action>, paste: Option<&str>, busy: bool) -> (Option<Out>, Asks) {
        let mut asks = Asks::default();
        // Tokens Slack no longer accepts, an app-level token of another app, and a hub with no token that posts as the person,
        // take the same keys: the person's list of apps for steps 2 and 3, or the app made again.
        let refused = self.refused_screen();
        match (action, &self.screen) {
            (Some(Action::ReturnNav), _) => asks.close = true,
            (Some(Action::ChatSend), Screen::Connected(..)) => asks.done = true,
            (_, Screen::Connecting | Screen::Connected(..)) => {}
            (Some(Action::ChatSetup), Screen::First) if self.can_move() => asks.away = true,
            (Some(Action::Paste), _) => return self.paste(paste.unwrap_or_default(), busy),
            // Step 1 offers the manifest to copy, for a Slack page that came up empty.
            (Some(Action::ChatCopySelection), Screen::Make) => {
                asks.copy = Some(slack_app_manifest(&self.name()));
                asks.notice = Some("copied your Slack app's description: paste it in Slack's From a manifest".into());
            }
            // The refused tokens' screen: Enter opens the person's apps for steps 2 and 3, Left makes the app again.
            (Some(Action::ChatSend), Screen::First) if refused => (asks.open, self.screen) = (Some(self.apps_page()), Screen::Tokens),
            (Some(Action::ChatThreadClose), Screen::First) if refused => (asks.open, self.screen) = (Some(slack_app_url(&self.name())), Screen::Make),
            (Some(Action::ChatSend), Screen::First) => (asks.open, self.screen) = (Some(slack_app_url(&self.name())), Screen::Make),
            (Some(Action::ChatThreadOpen), Screen::First) if !refused => self.screen = Screen::Tokens,
            (Some(Action::ChatSend | Action::ChatThreadOpen), Screen::Make) => self.screen = Screen::Tokens,
            (Some(Action::ChatThreadClose), Screen::Make) => self.screen = Screen::First,
            (Some(Action::ChatThreadOpen), Screen::Tokens) => self.screen = Screen::App,
            (Some(Action::ChatThreadClose), Screen::Tokens) => self.screen = Screen::Make,
            (Some(Action::ChatThreadClose), Screen::App) => self.screen = Screen::Tokens,
            // All three tokens held while an earlier connect was unanswered: Enter sends them once it is.
            (Some(Action::ChatSend), Screen::Tokens | Screen::App) => return (self.send(busy, &mut asks), asks),
            _ => {}
        }
        (None, asks)
    }

    /// A paste is a token, sorted by its prefix; anything else is dropped unshown. With all three held, they go in one request.
    fn paste(&mut self, text: &str, busy: bool) -> (Option<Out>, Asks) {
        let text = text.trim();
        let token = |prefix: &str| {
            (text.len() > prefix.len() && text.starts_with(prefix) && !text.chars().any(|c| c.is_whitespace() || c.is_control()))
                .then(|| ChatToken::try_from(text.to_string()).ok())
                .flatten()
        };
        if let Some(t) = token(BOT_PREFIX) {
            self.bot = Some(t);
            self.refused = self.refused.filter(|r| *r != Refused::Bot);
        } else if let Some(t) = token(USER_PREFIX) {
            self.user = Some(t);
            self.refused = self.refused.filter(|r| *r != Refused::User);
        } else if let Some(t) = token(APP_PREFIX) {
            self.app = Some(t);
            self.refused = self.refused.filter(|r| *r != Refused::App);
        } else {
            let words = "that is not a Slack code: the \"Bot User OAuth Token\" starts xoxb-, the \"User OAuth Token\" xoxp-, the \"App-Level Token\" xapp-";
            return (None, Asks { notice: Some(words.into()), ..Asks::default() });
        }
        self.failed = None;
        // Step 2 until its two tokens are held, whichever came first; then the last token's step.
        self.screen = if self.bot.is_none() || self.user.is_none() { Screen::Tokens } else { Screen::App };
        let mut asks = Asks::default();
        (self.send(busy, &mut asks), asks)
    }

    /// One `chat.connect` with the three tokens, unless one is missing or an earlier connect is unanswered.
    fn send(&mut self, busy: bool, asks: &mut Asks) -> Option<Out> {
        let (Some(bot), Some(user), Some(app)) = (&self.bot, &self.user, &self.app) else { return None };
        if busy {
            asks.notice = Some("Slack is still checking the codes sent before: wait a few seconds for its answer, then press Enter again".into());
            return None;
        }
        let out = (self.host.clone(), OutgoingReq::ChatConnect { bot_token: bot.clone(), user_token: user.clone(), app_token: app.clone() });
        self.screen = Screen::Connecting;
        Some(out)
    }

    /// The hub's reply to the step's connect: connected drops the tokens; a refused token goes back to its step, the others
    /// kept; anything else drops all three and returns to the first screen with its words. A step that is not connecting sent no
    /// connect, so a reply changes nothing in it.
    pub(super) fn answered(&mut self, reply: ChatReply<ChatConnectRes>) {
        if self.screen != Screen::Connecting {
            return;
        }
        let words = match reply {
            ChatReply::Done(res) => {
                (self.bot, self.user, self.app) = (None, None, None);
                // Nothing is known yet of the Slack app the hub now runs: the screen asks.
                (self.screen, self.about, self.asking) = (Screen::Connected(res.workspace, res.name), None, Some((None, UNREAD_ASKS)));
                return;
            }
            ChatReply::Refused(ChatCode::BotTokenRefused) => {
                (self.bot, self.refused, self.screen) = (None, Some(Refused::Bot), Screen::Tokens);
                return;
            }
            ChatReply::Refused(ChatCode::UserTokenRefused) => {
                (self.user, self.refused, self.screen) = (None, Some(Refused::User), Screen::Tokens);
                return;
            }
            ChatReply::Refused(ChatCode::AppTokenRefused) => {
                (self.app, self.refused, self.screen) = (None, Some(Refused::App), Screen::App);
                return;
            }
            ChatReply::Unsupported => format!("Ship of Tools on {} is too old to connect Slack: update it", computer(&self.host)),
            ChatReply::Refused(code) => code.text().to_string(),
        };
        (self.bot, self.user, self.app, self.refused) = (None, None, None, None);
        (self.failed, self.screen) = (Some(format!("Not connected: {words}.")), Screen::First);
    }

    /// The drawer's title for the screen.
    pub(super) fn title(&self) -> String {
        let words = match self.screen {
            Screen::First if self.post_in.is_some() => "post as you",
            Screen::First => "set up Slack",
            Screen::Make => "set up Slack · step 1 of 3: make your Slack app",
            Screen::Tokens => "set up Slack · step 2 of 3: the first two Slack codes",
            Screen::App => "set up Slack · step 3 of 3: the last Slack code",
            Screen::Connecting => "connecting…",
            Screen::Connected(..) => "connected",
        };
        format!(" chat · {words} ")
    }

    /// The screen: its paragraphs, a blank line between, and its keys pinned at the foot.
    pub(super) fn body(&self) -> Body {
        let name = self.name();
        let p = PRIMARY;
        let c = computer(&self.host);
        let steps = "Left/Right  move between steps    Esc  close";
        let refused = self.refused_screen();
        let mut out: Vec<String> = self.failed.iter().filter(|_| self.screen == Screen::First).cloned().collect();
        // A row whose channel is in a workspace whose connection failed: which one, and why, before the step's own words.
        if let (Screen::First, Some((id, wname))) = (&self.screen, &self.failed_in) {
            let w = workspace_words(id, wname.as_deref());
            out.push(match self.reason {
                ChatUnconfigured::NotConnected => format!("This project's channel is in {w}, which Ship of Tools on {c} is not connected to."),
                ChatUnconfigured::TokensRefused | ChatUnconfigured::WrongApp => format!("This project's channel is in {w}, whose connection failed."),
                ChatUnconfigured::StartFailed => format!("This project's channel is in {w}, whose chat did not start on {c}: {}.", log_words(&c)),
                ChatUnconfigured::Other => format!("This project's channel is in {w}, whose connection failed on {c}: {}.", log_words(&c)),
            });
        }
        // Ctrl+Shift+k: the other workspaces' channels, for a project to move to.
        let away = if self.can_move() { format!("    {p}+Shift+k  open the channel list of another workspace") } else { String::new() };
        let keys = match &self.screen {
            Screen::First if refused => {
                out.push(if self.reason == ChatUnconfigured::WrongApp {
                    format!("The chat stopped: the \"App-Level Token\" it holds belongs to another Slack app than your Slack app (the bot that reads the channel and posts for your agents), so the chat would have received that other app's messages. To fix it, paste your own Slack app's codes: press Enter, and your list of Slack apps opens in your browser. Click {name}, then copy its three Slack codes again, as steps 2 and 3 show (they follow here).")
                } else if self.post_in.is_some() {
                    let why = format!("Why: Ship of Tools posts your messages in Slack as you, and {c} holds no Slack code that works for that: {c} was connected to Slack before Ship of Tools could post as you, or Slack stopped accepting the code it had.");
                    let app = self.about.as_ref().filter(|a| a.app_id.is_some());
                    let keep = match app.and_then(|a| a.app_name.as_deref()).map(cell_row) {
                        // The Slack app connected now, which keeping keeps in every channel it is in; its page opens by its id.
                        Some(named) => format!("Keep {APP}, the one {c} is connected to now (Slack calls it {named})."),
                        None if app.is_some() => format!("Keep {APP}, the one {c} is connected to now."),
                        None => String::new(),
                    };
                    if keep.is_empty() {
                        format!("Press Enter: your list of Slack apps opens in your browser. In it, click {name}, {APP}, and copy its codes again, as steps 2 and 3 show (they follow here). If {name} is not in the list, look there for the Slack app you connected this workspace with before, whatever its name, and click that one. Press Left to make a new one (step 1) only if there is none: a new Slack app is in none of your private channels.\n\n{why}")
                    } else {
                        format!("{keep} Press Enter to open its page in Slack, add the permissions to post as you and install it again (step 2 shows how), then paste its codes here. Do not make a new Slack app: it would be in none of your private channels. Press Left to make one only if that Slack app is gone.\n\n{why}")
                    }
                } else {
                    format!("Slack no longer accepts the Slack codes Ship of Tools holds. Press Enter: your list of Slack apps opens in your browser. If {name}, {APP}, is in it, click it and copy its three codes again (steps 2 and 3 follow here). If it is not, press Left here to make your Slack app again (step 1).")
                });
                match (self.post_in.is_some(), self.apps_page() == SLACK_APPS) {
                    (true, true) => format!("Enter  open your Slack apps    Left  make a new Slack app{away}    Esc  close"),
                    (true, false) => format!("Enter  open your Slack app's page    Left  make a new Slack app{away}    Esc  close"),
                    _ => format!("Enter  open your Slack apps    Left  make your Slack app again{away}    Esc  close"),
                }
            }
            Screen::First => {
                out.push("This panel shows a Slack channel beside this project: you read it and post to it here. First, connect Ship of Tools to the Slack workspace the channel is in. You do this once for each Slack workspace you use, in three steps on Slack's website. Every project with a channel in that workspace then uses that connection, and each project picks its own channel afterward.".into());
                out.push(format!("You need Slack's website open and signed in, in your web browser (the Slack desktop app is not enough). In step 1 you make {APP}. In steps 2 and 3 you copy three Slack codes (Slack calls them tokens) from Slack: one lets Ship of Tools post as you, one lets your Slack app read and post, and one lets it see new messages as they arrive. A Slack code is a long string of letters and digits, like a password: paste it only here. A Slack code you paste here is never posted. The messages you write in the chat itself, once it is set up, are posted under your own Slack name; what your agents post goes out as your Slack app. Each person makes their own Slack app in each workspace they connect, even when a colleague already has one there: two people sharing one app each miss messages, because Slack splits the messages between them."));
                format!("Enter  start    Right  I already made my Slack app: go to step 2{away}    Esc  close")
            }
            Screen::Make => {
                let named = if self.login.trim().is_empty() {
                    format!("{name} (you can change it there)")
                } else {
                    format!("{name}, after the name you sign in to this computer with (you can change it there)")
                };
                out.push(format!("Slack's website now shows a page that makes {APP}, already named {named}; Slack shows that name on the messages your Slack app posts. If Slack asks which workspace the app is for, pick the one you are connecting. Your Slack app is how Ship of Tools reaches your channels. It can list your workspace's channels, read and post in the ones it is in, and, with your own Slack code, post your messages as you; it cannot read direct messages or channels it is not in. Click Next, then Create. Then click Install to Workspace, then Allow. When Slack shows the app's page after Allow, press Enter here: this panel then shows step 2, where you copy the first two Slack codes."));
                out.push("If Slack says the app needs an administrator's approval, send the request Slack offers and wait for Slack's message that it is approved. Then, on the app's page in Slack, click Install to Workspace and Allow, and come back here. (If you closed this panel meanwhile, open it, and press Right on the first screen to come straight to step 2.)".into());
                out.push(format!("The page was empty? Copy your Slack app's description (Slack calls it a manifest) with {p}+c in this panel, then go to api.slack.com/apps, click Create New App, then From a manifest, choose the workspace you are connecting, paste it with {p}+v, Next, Create."));
                format!("Enter  next step    {steps}")
            }
            Screen::Tokens => {
                out.push(match self.refused {
                    Some(Refused::User) => format!("Slack did not accept the code you pasted. Make sure you copied the \"User OAuth Token\" (it starts with xoxp-); then copy it again, click in this panel and press {p}+v."),
                    Some(Refused::Bot) => format!("Slack did not accept the code you pasted. Make sure you copied the \"Bot User OAuth Token\" (it starts with xoxb-); then copy it again, click in this panel and press {p}+v."),
                    _ => format!("Slack now shows two long codes like passwords: the \"User OAuth Token\", which starts with xoxp- and lets Ship of Tools post your messages as you, and the \"Bot User OAuth Token\", which starts with xoxb- and lets {APP} read your channels and post in them. (Not on screen? At api.slack.com/apps, click {name}, then OAuth & Permissions in the menu on the left.) Click Copy beside one, then click in this panel and press {p}+v. Then do the same with the other, in either order."),
                });
                out.push(format!("User OAuth Token       {}\nBot User OAuth Token   {}", shown(self.user.as_ref()), shown(self.bot.as_ref())));
                out.push(format!("No \"User OAuth Token\" on the page? Your Slack app was made before Ship of Tools posted as you. On that page, under User Token Scopes, click Add an OAuth Scope and choose chat:write (post as you), then again for files:write (post a file as you). Then click Reinstall to Workspace and Allow; the \"User OAuth Token\" shows at the top. Copy it, click in this panel and press {p}+v as above."));
                format!("{p}+v  paste    {steps}")
            }
            Screen::App => {
                if self.refused == Some(Refused::App) {
                    out.push(format!("Slack did not accept the code you pasted. Make sure you copied the \"App-Level Token\" (it starts with xapp-) and that you chose connections:write (receive new messages as they arrive); then copy it again, click in this panel and press {p}+v. Copy it from the same Slack app as the two codes of step 2."));
                } else {
                    out.push("One more code, the \"App-Level Token\": it lets Ship of Tools see new messages as they arrive. Copy it from the same Slack app as the two codes of step 2. On Slack's page:".into());
                    out.push("  1. In the menu on the left, click Basic Information.\n  2. Scroll down to App-Level Tokens and click Generate Token and Scopes.\n  3. In the name box, type: messages\n  4. Click Add Scope and choose connections:write (receive new messages as they arrive) from the list.\n  5. Click Generate, then Copy.".into());
                    out.push(format!("Then click in this panel and press {p}+v."));
                }
                out.push(format!("App-Level Token   {}", shown(self.app.as_ref())));
                format!("{p}+v  paste    {steps}")
            }
            Screen::Connecting => {
                out.push("Slack is checking the three codes.".into());
                "Esc  close; the check goes on".to_string()
            }
            Screen::Connected(id, wname) => {
                let (w, label) = (workspace_words(id, wname.as_deref()), workspace_label(id, wname.as_deref()));
                let kept = format!("Ship of Tools is connected to {w}. It keeps that workspace's three Slack codes on {c}, readable only by your login there.");
                let unread = self.about.as_ref().and_then(|a| a.unread.as_deref());
                let app = self.about.as_ref().and_then(|a| a.app_name.as_deref()).map_or(name, |n| cell_row(n).into_owned());
                match unread {
                    // What a person must do first leads: invite the Slack app where it is not, and choose no other channel.
                    Some(list) if !list.is_empty() => {
                        let labels: Vec<String> = list
                            .iter()
                            .map(|u| match u.name.as_deref() {
                                Some(n) => format!("#{}", cell_row(n)),
                                // Slack's search does not find a channel by its ID: the address that opens it does.
                                None => format!("the channel with ID {0} (to find it, open app.slack.com/client/{id}/{0} in your web browser)", u.id.as_str()),
                            })
                            .collect();
                        out.push(unread_words(&labels, &app));
                        out.push(kept);
                        "Enter  open the channel list    Esc  close".to_string()
                    }
                    Some(_) => {
                        out.push(format!("{kept} Other projects keep the channels they have. Next, press Enter to choose this project's channel in {label}; each project picks its own."));
                        format!("Enter  choose this project's channel in {label}    Esc  close")
                    }
                    // Until the hub says which channels the Slack app reads, nothing offers to choose this project's channel: the
                    // person could choose away the one this connect exists to keep.
                    None if self.asking.is_some() => {
                        out.push(format!("{kept} It is now checking which of the projects' channels {APP} can read; this takes a few seconds."));
                        "Enter  open the channel list    Esc  close".to_string()
                    }
                    None => {
                        out.push(format!("{kept} Ship of Tools could not check which of the projects' channels {APP} can read. If a project's panel shows nothing, open its channel in Slack and type /invite @{app}."));
                        "Enter  open the channel list    Esc  close".to_string()
                    }
                }
            }
        };
        Body { head: out.join("\n\n"), pin: keys, ..Body::default() }
    }
}

/// What the connected screen says first of the mapped channels the Slack app connected cannot read (`labels`, each as a
/// sentence names a channel and, for one known only by its ID, how to find it; `app`, the name to invite it by): which, what
/// that costs, and what to type in Slack, before any choice of channel. Never empty.
fn unread_words(labels: &[String], app: &str) -> String {
    match labels {
        [one] => format!("{} is not in {one} yet, so the project that uses {one} shows nothing here. To keep that channel, open {one} in Slack and type /invite @{app} in its message box. Do not choose another channel here. (A new Slack app is in no channel until someone in the channel invites it.)", cap(APP)),
        [rest @ .., last] => format!("{} is not in {} and {last} yet, so the projects that use them show nothing here. To keep those channels, open each one in Slack and type /invite @{app} in its message box. Do not choose other channels here. (A new Slack app is in no channel until someone in the channel invites it.)", cap(APP), rest.join(", ")),
        [] => String::new(),
    }
}

/// `text` with its first letter a capital, for a sentence it begins.
fn cap(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// The name the person signs in to this computer with, which their app is named for: read once, by exact name. A missing
/// one names the app `Ship of Tools`.
pub(in crate::ui) fn login() -> &'static str {
    static LOGIN: OnceLock<String> = OnceLock::new();
    LOGIN.get_or_init(|| login_in(&Env::process()))
}

pub(super) fn login_in(env: &Env) -> String {
    let key = if cfg!(windows) { "USERNAME" } else { "USER" };
    env.get_nonempty(key).and_then(|v| v.to_str()).unwrap_or_default().to_string()
}

/// A key in the Chat tenant while the Connect step shows (`connect_action`).
pub(in crate::ui) fn connect_key(state: &mut State, key: &KeyPress<'_>, label: &str) -> bool {
    let KeyPress { event, action, ctrl, alt, super_, .. } = *key;
    connect_action(state, action, &event.logical_key, event.repeat, (ctrl, alt, super_), label)
}

/// The Connect step takes every key in the drawer: PageUp and PageDown scroll its screen, Left, Right, the copy chord and
/// `chat.setup` (which opens another workspace's channel list where the step offers it) are its own, and Enter, Escape and paste are read by the drawer's prompt machine (`prompt_step`), so a paste is a token and goes
/// nowhere else; any other key is taken and does nothing. Whether the step took it (it shows).
pub(in crate::ui) fn connect_action(state: &mut State, action: Option<Action>, key: &Key, repeat: bool, mods: (bool, bool, bool), label: &str) -> bool {
    if !state.chat.in_setup() {
        return false;
    }
    let pressed = match action {
        Some(Action::ScrollPageUp | Action::ScrollPageDown) => {
            let (now, length) = state.scroll_ask();
            state.chat.scroll(action == Some(Action::ScrollPageUp), super::Step::Page, now, length);
            None
        }
        Some(Action::ChatThreadClose | Action::ChatThreadOpen | Action::ChatCopySelection | Action::ChatSetup) => (!repeat).then_some((action, None)),
        _ => match prompt_step(action, key, repeat, mods) {
            Some(PromptStep::Enter) => Some((Some(Action::ChatSend), None)),
            Some(PromptStep::Escape) => Some((Some(Action::ReturnNav), None)),
            Some(PromptStep::Paste) => Some((Some(Action::Paste), Some(read_clipboard_text().unwrap_or_default()))),
            Some(PromptStep::Edit(_) | PromptStep::Held) | None => None,
        },
    };
    if let Some((action, paste)) = pressed {
        let mut asks = Asks::default();
        state.with_chat(|c, _| {
            let (outs, a) = c.setup_key(action, paste.as_deref());
            asks = a;
            outs
        });
        if let Some(url) = asks.open {
            if let Err(e) = crate::browser_open::open_page(&url) {
                state.status.set(format!("could not open the browser ({e}): go to api.slack.com/apps"));
            }
        }
        match asks.copy.map(write_clipboard_text) {
            Some(Err(e)) => state.status.set(format!("copy failed · {e}")),
            _ => asks.notice.into_iter().for_each(|words| state.status.set(words)),
        }
        if asks.close {
            let effect = state.layout_help.toggle_drawer(DrawerContent::Chat, state.focus_prompts.focus());
            effect.apply(state, Some(label.to_string()));
        }
    }
    state.last_key = Some(label.to_string());
    state.window.request_redraw();
    true
}
