//! The Connect step: what the Chat drawer's modal slot shows for a row whose hub answers `unconfigured`, or after `Connect
//! Slack again` in the picker, one screen at a time (the start, or the refused tokens' screen; then steps 1 to 3, with Left and
//! Right between them; then connecting and connected). A pasted token is held here as a `ChatToken`, whose `Debug` prints no
//! byte of it, and is drawn as its five-character prefix and its length alone; both tokens leave in one `chat.connect`. A
//! refused token sends the person back to its step and the other is kept; any other reply drops both. The drawer ends the
//! step as it ends any modal screen, dropping whatever it holds.

use std::sync::OnceLock;

use sot_protocol::chat::{slack_app_manifest, slack_app_name, slack_app_url};
use sot_protocol::ops::{ChatCode, ChatConnectRes, ChatToken, ChatUnconfigured};

use crate::net::dial::HostKey;
use crate::net::transport::{ChatReply, OutgoingReq};
use crate::ui::input::keypress::KeyPress;
use crate::ui::*;
use sot_platform::host::env::Env;

use super::owner::Out;
use super::picker::Body;
use super::{prompt_step, PromptStep};

/// Slack's list of the person's apps, which the refused tokens' screen opens.
const SLACK_APPS: &str = "https://api.slack.com/apps";
const BOT_PREFIX: &str = "xoxb-";
const APP_PREFIX: &str = "xapp-";
/// The copy and paste chords as this computer's keyboard names them.
const PRIMARY: &str = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Screen {
    /// Screen 1, or for tokens Slack no longer accepts, screen 8.
    First,
    Make,
    Bot,
    App,
    Connecting,
    /// The workspace the hub's reply named.
    Connected(String),
}

/// Which token Slack refused, for its step's words until a new one is pasted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refused {
    Bot,
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
    pub notice: Option<String>,
}

#[derive(Debug)]
pub(super) struct Connect {
    host: HostKey,
    reason: ChatUnconfigured,
    /// The step began from the hub's `unconfigured`, so another answer ends it while the person is still pasting; a step a
    /// person asked for (`Connect Slack again`) stays until they leave it.
    follows: bool,
    screen: Screen,
    bot: Option<ChatToken>,
    app: Option<ChatToken>,
    refused: Option<Refused>,
    /// Why the last connect failed for another cause: the first screen leads with these words.
    failed: Option<String>,
    /// The login the app is named for (`slack_app_name`), read when the step began.
    login: String,
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
        Connect { host, reason, follows, screen: Screen::First, bot: None, app: None, refused: None, failed: None, login: login.to_string() }
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
    pub(super) fn held(&self) -> (Option<&str>, Option<&str>) {
        (self.bot.as_ref().map(ChatToken::expose), self.app.as_ref().map(ChatToken::expose))
    }

    /// Whether another answer for the row ends the step: one begun from `unconfigured`, while the person is still pasting. A
    /// connect in flight or done waits for the person.
    pub(super) fn ends_on_another_answer(&self) -> bool {
        self.follows && !matches!(self.screen, Screen::Connecting | Screen::Connected(_))
    }

    pub(super) fn connecting(&self) -> bool {
        self.screen == Screen::Connecting
    }

    /// The hub's reason, read again: the first screen follows it.
    pub(super) fn reason(&mut self, reason: ChatUnconfigured) {
        self.reason = reason;
    }

    fn name(&self) -> String {
        slack_app_name(&self.login)
    }

    /// A key, by its action, with the clipboard's text for a paste; `busy`: an earlier connect is still unanswered, so none
    /// is sent now. The request it sends, if any, and what it asks of the window.
    pub(super) fn key(&mut self, action: Option<Action>, paste: Option<&str>, busy: bool) -> (Option<Out>, Asks) {
        let mut asks = Asks::default();
        let refused = self.reason == ChatUnconfigured::TokensRefused;
        match (action, &self.screen) {
            (Some(Action::ReturnNav), _) => asks.close = true,
            (Some(Action::ChatSend), Screen::Connected(_)) => asks.done = true,
            (_, Screen::Connecting | Screen::Connected(_)) => {}
            (Some(Action::Paste), _) => return self.paste(paste.unwrap_or_default(), busy),
            // Step 1 offers the manifest to copy, for a Slack page that came up empty.
            (Some(Action::ChatCopySelection), Screen::Make) => {
                asks.copy = Some(slack_app_manifest(&self.name()));
                asks.notice = Some("copied the app's description: paste it in Slack's From a manifest".into());
            }
            // The refused tokens' screen: Enter opens the person's apps for steps 2 and 3, Left makes the app again.
            (Some(Action::ChatSend), Screen::First) if refused => (asks.open, self.screen) = (Some(SLACK_APPS.into()), Screen::Bot),
            (Some(Action::ChatThreadClose), Screen::First) if refused => (asks.open, self.screen) = (Some(slack_app_url(&self.name())), Screen::Make),
            (Some(Action::ChatSend), Screen::First) => (asks.open, self.screen) = (Some(slack_app_url(&self.name())), Screen::Make),
            (Some(Action::ChatThreadOpen), Screen::First) if !refused => self.screen = Screen::Bot,
            (Some(Action::ChatSend | Action::ChatThreadOpen), Screen::Make) => self.screen = Screen::Bot,
            (Some(Action::ChatThreadClose), Screen::Make) => self.screen = Screen::First,
            (Some(Action::ChatThreadOpen), Screen::Bot) => self.screen = Screen::App,
            (Some(Action::ChatThreadClose), Screen::Bot) => self.screen = Screen::Make,
            (Some(Action::ChatThreadClose), Screen::App) => self.screen = Screen::Bot,
            // Both tokens held while an earlier connect was unanswered: Enter sends them once it is.
            (Some(Action::ChatSend), Screen::Bot | Screen::App) => return (self.send(busy, &mut asks), asks),
            _ => {}
        }
        (None, asks)
    }

    /// A paste is a token, sorted by its prefix; anything else is dropped unshown. With both held, they go in one request.
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
        } else if let Some(t) = token(APP_PREFIX) {
            self.app = Some(t);
            self.refused = self.refused.filter(|r| *r != Refused::App);
        } else {
            let words = "that is not a Slack token: the bot token starts xoxb-, the app-level token xapp-";
            return (None, Asks { notice: Some(words.into()), ..Asks::default() });
        }
        self.failed = None;
        self.screen = if self.bot.is_none() { Screen::Bot } else { Screen::App };
        let mut asks = Asks::default();
        (self.send(busy, &mut asks), asks)
    }

    /// One `chat.connect` with both tokens, unless one is missing or an earlier connect is unanswered.
    fn send(&mut self, busy: bool, asks: &mut Asks) -> Option<Out> {
        let (Some(bot), Some(app)) = (&self.bot, &self.app) else { return None };
        if busy {
            asks.notice = Some("Slack is still checking the tokens sent before: press Enter once that is answered".into());
            return None;
        }
        let out = (self.host.clone(), OutgoingReq::ChatConnect { bot_token: bot.clone(), app_token: app.clone() });
        self.screen = Screen::Connecting;
        Some(out)
    }

    /// The hub's reply to the step's connect: connected drops both tokens; a refused token goes back to its step, the other
    /// kept; anything else drops both and returns to the first screen with its words. A step that is not connecting sent no
    /// connect, so a reply changes nothing in it.
    pub(super) fn answered(&mut self, reply: ChatReply<ChatConnectRes>) {
        if self.screen != Screen::Connecting {
            return;
        }
        let words = match reply {
            ChatReply::Done(res) => {
                (self.bot, self.app) = (None, None);
                self.screen = Screen::Connected(res.workspace);
                return;
            }
            ChatReply::Refused(ChatCode::BotTokenRefused) => {
                (self.bot, self.refused, self.screen) = (None, Some(Refused::Bot), Screen::Bot);
                return;
            }
            ChatReply::Refused(ChatCode::AppTokenRefused) => {
                (self.app, self.refused, self.screen) = (None, Some(Refused::App), Screen::App);
                return;
            }
            ChatReply::Unsupported => "the hub's daemon must be updated to connect Slack",
            ChatReply::Refused(code) => code.text(),
        };
        (self.bot, self.app, self.refused) = (None, None, None);
        (self.failed, self.screen) = (Some(format!("Not connected: {words}.")), Screen::First);
    }

    /// The drawer's title for the screen.
    pub(super) fn title(&self) -> String {
        let words = match self.screen {
            Screen::First => "set up Slack",
            Screen::Make => "set up Slack · step 1 of 3: make the app",
            Screen::Bot => "set up Slack · step 2 of 3: the first token",
            Screen::App => "set up Slack · step 3 of 3: the second token",
            Screen::Connecting => "connecting…",
            Screen::Connected(_) => "connected",
        };
        format!(" chat · {words} ")
    }

    /// The screen: its paragraphs, a blank line between, and its keys pinned at the foot.
    pub(super) fn body(&self) -> Body {
        let name = self.name();
        let p = PRIMARY;
        let steps = "Left/Right  move between steps    Esc  close";
        let mut out: Vec<String> = self.failed.iter().filter(|_| self.screen == Screen::First).cloned().collect();
        let keys = match &self.screen {
            Screen::First if self.reason == ChatUnconfigured::TokensRefused => {
                out.push(format!("Slack no longer accepts Ship of Tools' tokens. Press Enter: your list of Slack apps opens in your browser. If {name} is in it, click it and copy its two tokens again (steps 2 and 3 follow here). If it is not, press Left here to make the app again (step 1)."));
                "Enter  open your Slack apps    Left  make the app again    Esc  close".to_string()
            }
            Screen::First => {
                out.push("This panel shows a Slack channel beside this project: you read it and post to it here. First, connect Ship of Tools to your Slack workspace. You do it once, in three steps on Slack's website; every project then uses it.".into());
                out.push("You need Slack's website open and signed in, in your web browser (the Slack desktop app is not enough). A token, which you copy from Slack in steps 2 and 3, is a long code, like a password, that lets Ship of Tools act as the app: paste it only here. Each person makes their own app, even when a colleague already has one: two people sharing one app each miss messages.".into());
                "Enter  start    Right  I made the app in an earlier try: go to step 2    Esc  close".to_string()
            }
            Screen::Make => {
                let named = if self.login.trim().is_empty() {
                    format!("{name} (you can change it there)")
                } else {
                    format!("{name}, after the name you sign in to this computer with (you can change it there)")
                };
                out.push(format!("Slack's website now shows a page that makes your own app, already named {named}; it is the name Slack shows for your app. It is how Ship of Tools reaches your channels. It can list your workspace's channels, read and post in the ones it is in, and post your messages; it cannot read direct messages or channels it is not in. Click Next, then Create. Then click Install to Workspace, then Allow. When Slack shows the app's page after Allow, press Enter here."));
                out.push("If Slack says the app needs an administrator's approval, send the request Slack offers and wait for Slack's message that it is approved. Then, on the app's page in Slack, click Install to Workspace and Allow, and come back here. (If you closed this panel meanwhile, open it, and press Right on the first screen to come straight to step 2.)".into());
                out.push(format!("The page was empty? Press {p}+C in this panel to copy the app's description, then go to api.slack.com/apps, click Create New App, then From a manifest, choose your workspace, paste it with {p}+V, Next, Create."));
                format!("Enter  next step    {steps}")
            }
            Screen::Bot => {
                out.push(if self.refused == Some(Refused::Bot) {
                    format!("Slack did not accept the token you pasted. Make sure you copied the Bot User OAuth Token (it starts with xoxb-); then copy it again, click in this panel and press {p}+V.")
                } else {
                    format!("Slack now shows the app's Bot User OAuth Token: a long code, like a password, that starts with xoxb-. (Not on screen? At api.slack.com/apps, click {name}, then OAuth & Permissions in the menu on the left.) Click Copy beside it, then click in this panel and press {p}+V.")
                });
                out.push(format!("Bot User OAuth Token   {}", shown(self.bot.as_ref())));
                format!("{p}+V  paste    {steps}")
            }
            Screen::App => {
                if self.refused == Some(Refused::App) {
                    out.push(format!("Slack did not accept the token you pasted. Make sure you copied the App-Level Token (it starts with xapp-) and that you chose connections:write; then copy it again, click in this panel and press {p}+V."));
                } else {
                    out.push("One more token, the App-Level Token: it lets Ship of Tools see new messages as they arrive. On Slack's page:".into());
                    out.push("  1. In the menu on the left, click Basic Information.\n  2. Scroll down to App-Level Tokens and click Generate Token and Scopes.\n  3. In the name box, type: messages\n  4. Click Add Scope and choose connections:write from the list.\n  5. Click Generate, then Copy.".into());
                    out.push(format!("Then click in this panel and press {p}+V."));
                }
                out.push(format!("App-Level Token   {}", shown(self.app.as_ref())));
                format!("{p}+V  paste    {steps}")
            }
            Screen::Connecting => {
                out.push("Checking both tokens with Slack.".into());
                "Esc  close; the check goes on".to_string()
            }
            Screen::Connected(workspace) => {
                out.push(format!("Ship of Tools is connected to the Slack workspace {workspace}. It keeps the two tokens on the computer that runs the chat, readable only by your account."));
                "Enter  choose this project's channel    Esc  close".to_string()
            }
        };
        Body { head: out.join("\n\n"), pin: keys, ..Body::default() }
    }
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

/// The Connect step takes every key in the drawer: PageUp and PageDown scroll its screen, Left, Right and the copy chord are
/// its own, and Enter, Escape and paste are read by the drawer's prompt machine (`prompt_step`), so a paste is a token and goes
/// nowhere else; any other key is taken and does nothing. Whether the step took it (it shows).
pub(in crate::ui) fn connect_action(state: &mut State, action: Option<Action>, key: &Key, repeat: bool, mods: (bool, bool, bool), label: &str) -> bool {
    if !state.chat.in_setup() {
        return false;
    }
    let pressed = match action {
        Some(Action::ScrollPageUp | Action::ScrollPageDown) => {
            let (now, length) = state.scroll_ask();
            state.chat.scroll_modal(action == Some(Action::ScrollPageUp), now, length);
            None
        }
        Some(Action::ChatThreadClose | Action::ChatThreadOpen | Action::ChatCopySelection) => (!repeat).then_some((action, None)),
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
