//! The chat drawer's model: the owner, the row it follows and the reads in flight. State sends what its methods return.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use sot_protocol::chat::{Ts, MAX_NAME};
use sot_protocol::chat::slack_app_name;
use sot_protocol::ops::{ChatChannel, ChatCode, ChatHistoryReq, ChatPairRes, ChatPostKind, ChatPostReq, ChatPostRes, ChatResolveRes, ChatTarget, ChatUnconfigured};

use crate::net::dial::HostKey;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use crate::ui::ResultRowIdentity;
use crate::ui::render::quad::ScreenRect;

use super::compose::{Compose, ComposeText, Edit};
use super::connect::{Asks, Connect};
use super::owner::{dead, ChatOwner, Fill, Out, PostShown};
use super::picker::{pair_words, Body, Pick, Picker};
use super::row_channel::{RowChannel, Stand};
use super::thread::Thread;
use super::view::{ChatHit, ChatView, Step};
use crate::ui::input::keybindings::Action;
use crate::ui::motion::ScrollMotion;

/// A `chat.history` request in flight. `orphan`: its reader gave it up, so its reply is dropped.
struct Read {
    req: ChatHistoryReq,
    orphan: bool,
}

/// Every read in flight, per host, so two equal reads are never in flight together (replies are matched by value and
/// may come back in either order). A read equal to one in flight waits in `held`.
#[derive(Default)]
struct Flight {
    reads: HashMap<HostKey, Vec<Read>>,
    held: Vec<Out>,
}

impl Flight {
    /// What may go out now; a read equal to one in flight is held, and a read that goes is in flight.
    fn send(&mut self, outs: Vec<Out>) -> Vec<Out> {
        let mut go = Vec::new();
        for out in outs {
            let (host, OutgoingReq::ChatHistory { req }) = &out else { go.push(out); continue };
            let reads = self.reads.entry(host.clone()).or_default();
            if reads.iter().any(|r| &r.req == req) {
                self.held.push(out);
            } else {
                reads.push(Read { req: req.clone(), orphan: false });
                go.push(out);
            }
        }
        go
    }

    /// A reply came: its one equal entry leaves; whether that entry was an orphan, and so the reply is dropped.
    fn answered(&mut self, host: &HostKey, req: &ChatHistoryReq) -> bool {
        let Some(reads) = self.reads.get_mut(host) else { return false };
        reads.iter().position(|r| &r.req == req).is_some_and(|i| reads.remove(i).orphan)
    }

    /// A reader gave the read up: a read still held is cancelled, one in flight stays as an orphan.
    fn abandon(&mut self, host: &HostKey, req: &ChatHistoryReq) {
        let held = |o: &Out| matches!(o, (h, OutgoingReq::ChatHistory { req: r }) if h == host && r == req);
        if let Some(i) = self.held.iter().position(held) {
            self.held.remove(i);
        } else if let Some(r) = self.reads.get_mut(host).and_then(|v| v.iter_mut().find(|r| &r.req == req)) {
            r.orphan = true;
        }
    }

    /// A link changed: nothing of that host is in flight, and no reply will come. Another host's reads stay. A read of
    /// that host still held is cancelled by `owner_outs` and, for the thread, `give_up_read` (a link change is a give-up).
    fn clear(&mut self, host: &HostKey) {
        self.reads.remove(host);
    }
}

#[derive(Default)]
pub(in crate::ui) struct ChatDrawer {
    owner: ChatOwner,
    rows: RowChannel,
    flight: Flight,
    view: ChatView,
    /// The open thread, which holds the channel's view meanwhile: the one switch for which collection the drawer shows.
    thread: Option<Thread>,
    compose: Compose,
    /// How many compose rows the last frame drew at the drawer's foot: the records' body is that much shorter.
    foot: u16,
    /// The words of the last post's outcome, until the status line takes them.
    notice: Option<String>,
    /// The name being typed at the drawer's foot, while Enter found none set: the keys edit it, not the draft.
    asking: Option<String>,
    /// The one modal screen (the Connect step or the channel picker), which holds the channel's view meanwhile, and the row
    /// whose picker Escape closed: the picker opens by itself for an unmapped row, once, until the row changes.
    modal: Option<Modal>,
    dismissed: Option<ResultRowIdentity>,
    /// The one `chat.connect` in flight, whatever has become of the step that sent it: no step sends another until its
    /// reply, so a reply is only ever the connecting step's.
    connect_sent: Option<HostKey>,
    /// The login the person's app is named for (`connect::login`, set by State).
    login: &'static str,
    /// How far the last frame's modal body could scroll, and the rows it had: the scroll's bound and its page.
    body_end: (u16, u16),
    /// The one `chat.pair` in flight, whatever has become of the picker that sent it: nothing is sent until its reply.
    pair_sent: Option<ResultRowIdentity>,
    /// The drawer has the keys (State sets it each frame): only then does an unmapped row open the picker by itself.
    focused: bool,
    /// The hosts whose hello said the daemon posts a person's chat post under their own token; the hint says so only for these.
    posts_as_person: std::collections::HashSet<HostKey>,
}

/// What holds the drawer's body in place of the records, one at a time. Whatever it is, the drawer parks the channel's view
/// behind it, scrolls its body, routes the keys to it first, and ends it at a row change, a hidden drawer and a link change
/// of its host.
struct Modal {
    kind: Kind,
    parked: ChatView,
    /// The body's rows scrolled past, and the eased motion toward what PageUp and PageDown asked.
    scroll: u16,
    motion: ScrollMotion,
}

enum Kind {
    Connect(Connect),
    Picker(Picker),
}

impl Modal {
    fn host(&self) -> &HostKey {
        match &self.kind {
            Kind::Connect(c) => c.host(),
            Kind::Picker(p) => &p.row.host,
        }
    }
}

/// What a request that could not be queued is told, as if the hub had refused it `unavailable`: a post, a pair or a channel
/// list never written is not left waiting for a reply that cannot come (`State::with_chat` feeds it to `on_evt`).
pub(super) fn never_written(req: &OutgoingReq) -> Option<ChatEvt> {
    let refused = ChatCode::Unavailable;
    Some(match req {
        OutgoingReq::ChatPost { req } => ChatEvt::Posted { req: req.clone(), reply: ChatReply::Done(ChatPostRes::Refused { code: refused }) },
        OutgoingReq::ChatPair { row, channel } => ChatEvt::Paired { row: row.clone(), channel: channel.clone(), reply: ChatReply::Refused(refused) },
        OutgoingReq::ChatChannels => ChatEvt::Channels { reply: ChatReply::Refused(refused) },
        OutgoingReq::ChatConnect { .. } => ChatEvt::Connect { reply: ChatReply::Refused(refused) },
        _ => return None,
    })
}

/// What the status line says of a post's outcome; a post made says nothing (its draft is gone).
fn post_words(shown: &PostShown) -> Option<String> {
    Some(match shown {
        PostShown::Posted(_) => return None,
        PostShown::Refused(ChatCode::BadRequest) => "not posted: the request is not valid; check [chat] name in settings.toml".into(),
        PostShown::Refused(code) => format!("not posted: {}", code.text()),
        PostShown::Limited(secs, assumed) => wait_words(*secs, *assumed),
        PostShown::Unknown => "may have been posted: check the channel before sending it again".into(),
        PostShown::Outdated => "not posted: the hub must be updated to post".into(),
    })
}

/// The status line's words for a rate limit: the seconds left, with `about` when the wait is a guess.
fn wait_words(secs: u64, assumed: bool) -> String {
    format!("not posted: rate limited; wait {}{secs} s", if assumed { "about " } else { "" })
}

impl ChatDrawer {
    /// The frame: follow the wanted row (see `RowChannel`) and pace the owner. The flag asks for another frame.
    pub(super) fn tick(&mut self, wanted: Option<ResultRowIdentity>, now: Instant) -> (Vec<Out>, bool) {
        let step = self.rows.tick(wanted, now);
        let mut outs = Vec::new();
        // The prompt belongs to the draft being posted and ends where its thread ends: a changed row, a hidden drawer and an
        // unmapped row all leave the row unmapped for the frame, so a message typed later is never typed into the name.
        if self.rows.stand() != Stand::Mapped {
            self.cancel_name();
        }
        if step.changed {
            // Another row's records are not the reader's place, and its thread and its modal screen are no one's.
            self.close_thread();
            self.close_modal();
            self.dismissed = None;
            self.view.reset();
            outs = self.owner_outs(ChatOwner::stop);
        }
        outs.extend(step.resolve);
        self.follow_setup();
        // An unmapped row opens the picker by itself while the drawer has the keys, once per row (Escape keeps it closed).
        if self.focused && self.modal.is_none() && self.rows.stand() == Stand::Unmapped && self.dismissed.as_ref() != self.rows.row() {
            let row = self.rows.row().cloned();
            outs.extend(self.open_picker(row.as_ref()).unwrap_or_default());
        }
        outs.extend(self.owner_outs(|o| o.tick(now)));
        outs.extend(self.thread_outs(|t| t.tick(now)));
        (outs, step.settling)
    }

    /// The Connect step follows the row's stand: `unconfigured` begins it (in place of the picker) or tells it the reason
    /// again; another answer ends one begun from `unconfigured` while the person is still pasting (`Connect::ends_on_another_answer`).
    fn follow_setup(&mut self) {
        match self.rows.stand() {
            Stand::Unconfigured(reason) => match self.modal.as_mut().map(|m| &mut m.kind) {
                Some(Kind::Connect(c)) => c.reason(reason),
                _ => {
                    if let Some(host) = self.rows.row().map(|r| r.host.clone()) {
                        self.open_modal(Kind::Connect(Connect::new(host, reason, self.login, true)));
                    }
                }
            },
            Stand::Resolving => {}
            _ => {
                if self.connect().is_some_and(Connect::ends_on_another_answer) {
                    self.close_modal();
                }
            }
        }
    }

    /// The modal screen takes the body: the channel's view (or a modal's before it) is set aside, the thread closed and the name
    /// prompt ended.
    fn open_modal(&mut self, kind: Kind) {
        self.close_modal();
        self.close_thread();
        self.cancel_name();
        let parked = self.parked_view();
        self.modal = Some(Modal { kind, parked, scroll: 0, motion: ScrollMotion::default() });
    }

    /// The modal screen ends, dropping what it holds, and the channel's view returns.
    fn close_modal(&mut self) -> Option<Kind> {
        let m = self.modal.take()?;
        self.view = m.parked;
        Some(m.kind)
    }

    fn connect(&self) -> Option<&Connect> {
        match self.modal.as_ref().map(|m| &m.kind) {
            Some(Kind::Connect(c)) => Some(c),
            _ => None,
        }
    }

    fn picker(&self) -> Option<&Picker> {
        match self.modal.as_ref().map(|m| &m.kind) {
            Some(Kind::Picker(p)) => Some(p),
            _ => None,
        }
    }

    fn picker_mut(&mut self) -> Option<&mut Picker> {
        match self.modal.as_mut().map(|m| &mut m.kind) {
            Some(Kind::Picker(p)) => Some(p),
            _ => None,
        }
    }

    /// Whether the Connect step shows: every key in the drawer is then its own (`connect::connect_action`).
    pub(in crate::ui) fn in_setup(&self) -> bool {
        self.connect().is_some()
    }

    /// The tokens the Connect step holds, for a test that a step kept or dropped them.
    #[cfg(test)]
    pub(super) fn setup_held(&self) -> Option<(Option<String>, Option<String>, Option<String>)> {
        self.connect().map(|c| {
            let (bot, user, app) = c.held();
            (bot.map(str::to_string), user.map(str::to_string), app.map(str::to_string))
        })
    }

    /// The connection's hello said whether the daemon posts a person's chat post under their own Slack token (State, at
    /// each `Connected`); gone when the link is. The compose hint says what a post will be by it.
    pub(in crate::ui) fn set_posts_as_person(&mut self, host: &HostKey, yes: bool) {
        if yes {
            self.posts_as_person.insert(host.clone());
        } else {
            self.posts_as_person.remove(host);
        }
    }

    /// The login the person's app is named for (State sets `connect::login()`; a test its own).
    pub(in crate::ui) fn set_login(&mut self, login: &'static str) {
        self.login = login;
    }

    /// A key in the Connect step (`Connect::key`). Enter on the connected screen opens the picker once the row has resolved
    /// again; until then it says so.
    pub(super) fn setup_key(&mut self, action: Option<Action>, paste: Option<&str>) -> (Vec<Out>, Asks) {
        let busy = self.connect_sent.is_some();
        let Some(Kind::Connect(c)) = self.modal.as_mut().map(|m| &mut m.kind) else { return (Vec::new(), Asks::default()) };
        let screen = c.title();
        let (out, mut asks) = c.key(action, paste, busy);
        if c.title() != screen {
            self.modal.iter_mut().for_each(|m| (m.scroll, m.motion) = (0, ScrollMotion::default()));
        }
        if let Some((host, _)) = &out {
            self.connect_sent = Some(host.clone());
        }
        let mut outs: Vec<Out> = out.into_iter().collect();
        if asks.done {
            if matches!(self.rows.stand(), Stand::Mapped | Stand::Unmapped) {
                self.close_modal();
                let row = self.rows.row().cloned();
                outs.extend(self.open_picker(row.as_ref()).unwrap_or_default());
            } else {
                asks.notice = Some("reading the hub again: press Enter in a moment".into());
            }
        }
        (outs, asks)
    }

    /// A scroll over a modal screen: its body moves a third of its rows for a page, one for a line, eased over `length`.
    fn scroll_modal(&mut self, up: bool, step: Step, now: Instant, length: Duration) {
        let page = match step {
            Step::Page => i32::from((self.body_end.1 / 3).max(1)),
            Step::Line => 1,
        };
        let end = self.body_end.0;
        if let Some(m) = self.modal.as_mut() {
            let rows = m.motion.ask_rows(if up { -page } else { page }, now, length);
            m.scroll = m.scroll.saturating_add_signed(rows as i16).min(end);
        }
    }

    /// What the chrome found the last frame's modal body to hold: how far it scrolls, and its rows.
    pub(in crate::ui) fn set_body_end(&mut self, end: u16, rows: u16) {
        self.body_end = (end, rows);
        if let Some(m) = self.modal.as_mut() {
            m.scroll = m.scroll.min(end);
        }
    }

    /// When the owner's next short-paced watch is due (`ChatOwner::next_ask`), or the wanted row is next resolved for its
    /// name (`RowChannel::next_ask`), whichever is first: the instant to wake the window for.
    pub(in crate::ui) fn next_ask(&self) -> Option<Instant> {
        [self.owner.next_ask(), self.rows.next_ask()].into_iter().flatten().min()
    }

    pub(super) fn on_connection(&mut self, host: &HostKey, up: bool, now: Instant) -> Vec<Out> {
        self.rows.on_connection(host, up);
        if !up {
            self.posts_as_person.remove(host);
        }
        self.cancel_name();
        // A link change of a modal screen's host ends it, and any token it holds.
        if self.modal.as_ref().is_some_and(|m| m.host() == host) {
            self.close_modal();
        }
        self.flight.clear(host);
        let mut outs = self.owner_outs(|o| o.on_connection(host, up, now));
        if self.thread.as_ref().is_some_and(|t| t.host() == host) {
            if up {
                outs.extend(self.restart_thread());
            } else {
                self.give_up_read();
            }
        }
        outs
    }

    pub(super) fn on_bus_lost(&mut self, host: &HostKey, bus: &str, now: Instant) -> Vec<Out> {
        self.owner_outs(|o| o.on_bus_lost(host, bus, now))
    }

    pub(super) fn on_evt(&mut self, host: &HostKey, evt: ChatEvt, now: Instant) -> Vec<Out> {
        match evt {
            // The wanted row's answer decides its watch: `mapped` watches, anything else (unmapped, invalid, refused, an
            // old daemon) ends it. An answer for a row no longer wanted is nothing.
            ChatEvt::Resolved { row, reply } if self.rows.wants(host, &row) => {
                self.rows.answered(&reply, now);
                match reply {
                    ChatReply::Done(ChatResolveRes::Mapped { workspace, channel, .. }) => {
                        let target = ChatTarget { row, workspace, channel };
                        self.owner_outs(|o| o.watch(host.clone(), target, now))
                    }
                    _ => self.owner_outs(ChatOwner::stop),
                }
            }
            ChatEvt::Resolved { .. } => Vec::new(),
            ChatEvt::Channels { reply } => {
                if let Some(p) = self.picker_mut().filter(|p| &p.row.host == host) {
                    p.on_channels(reply);
                }
                Vec::new()
            }
            // Any connect reply changes what the hub answers: the row is resolved again at once. Only the connecting step sent
            // it (`connect_sent`), so only that step reads it.
            ChatEvt::Connect { reply } => {
                self.connect_sent = self.connect_sent.take().filter(|h| h != host);
                if let Some(Kind::Connect(c)) = self.modal.as_mut().map(|m| &mut m.kind) {
                    if c.host() == host {
                        c.answered(reply);
                    }
                }
                self.rows.ask_again(host);
                Vec::new()
            }
            ChatEvt::ConnectUnknown => {
                self.connect_sent = self.connect_sent.take().filter(|h| h != host);
                if self.connect().is_some_and(|c| c.host() == host && c.connecting()) {
                    self.close_modal();
                }
                self.notice = Some("Slack may be connected: the hub did not answer before the link ended; reading it again".into());
                self.rows.ask_again(host);
                Vec::new()
            }
            ChatEvt::Paired { row, channel, reply } => {
                self.on_paired(host, &row, channel.as_ref(), Some(&reply));
                Vec::new()
            }
            ChatEvt::PairUnknown { row, channel } => {
                self.on_paired(host, &row, channel.as_ref(), None);
                Vec::new()
            }
            ChatEvt::History { req, reply } => {
                let orphan = self.flight.answered(host, &req);
                let mut outs = match (orphan, req.thread.is_some()) {
                    (true, _) => Vec::new(),
                    (false, true) => self.thread_outs(|t| t.on_reply(host, &req, reply, now)),
                    (false, false) => self.owner_outs(|o| o.on_evt(host, ChatEvt::History { req, reply }, now)),
                };
                let held = std::mem::take(&mut self.flight.held);
                outs.extend(self.flight.send(held));
                outs
            }
            ChatEvt::Posted { req, reply } => {
                // A hub that holds no token that posts as the person (never held, or Slack stopped accepting it): the Connect step
                // takes the channel's place for the row shown, its post-as-you screen first. The draft stays the row's.
                let shown = self.rows.row().is_some_and(|r| &r.host == host && r.workspace_id == req.target.row);
                if shown && matches!(reply, ChatReply::Done(ChatPostRes::Refused { code: ChatCode::NoUserToken })) {
                    self.open_modal(Kind::Connect(Connect::for_post(host.clone(), self.login)));
                }
                let mut made = None;
                let mut outs = self.owner_outs(|o| {
                    made = o.on_posted(host, &req, reply, now);
                    Vec::new()
                });
                if let Some(row) = made {
                    self.compose.clear(&ResultRowIdentity { host: host.clone(), workspace_id: row });
                    // A post into the open thread: the thread is read again whole.
                    if req.thread.as_ref().is_some_and(|p| self.thread.as_ref().is_some_and(|t| t.is(host, &req.target, p))) {
                        outs.extend(self.restart_thread());
                    }
                }
                outs
            }
            evt => self.owner_outs(|o| o.on_evt(host, evt, now)),
        }
    }

    /// Enter: posts `row`'s draft once, to the channel and thread it was begun in, under `name` (`[chat] name`, `None` when unset or
    /// blank). It needs a draft with something in it, no post waiting and no `limited` wait running; with no name it asks for
    /// one (`asking`) and sends nothing. The draft stays until a reply proves the post.
    pub(in crate::ui) fn send(&mut self, row: Option<&ResultRowIdentity>, name: Option<&str>, now: Instant) -> Result<Vec<Out>, String> {
        let Some(row) = row else { return Err("no row to write for".into()) };
        // Enter over an unmapped row with nothing typed asks for its channel.
        if self.modal.is_none() && self.rows.stand() == Stand::Unmapped && self.compose.draft(row).is_none_or(|d| d.text.is_empty()) {
            return self.open_picker(Some(row));
        }
        if self.owner.posting_row().is_some() {
            return Err("posting…".into());
        }
        let Some(draft) = self.compose.draft(row).filter(|d| !d.text.trim().is_empty()) else { return Err("nothing to post".into()) };
        // No name set: the foot asks for one, the draft stays, and nothing is sent; the next Enter posts it.
        let Some(name) = name else {
            // The prompt is for a row whose channel is mapped (`tick` ends it anywhere else): any other stand says its own words.
            let stand = self.rows.stand();
            if stand != Stand::Mapped {
                return Err(format!("not posted: {}", stand.words(self.rows.host())));
            }
            self.ask_name();
            return Ok(Vec::new());
        };
        if let Some((secs, assumed)) = self.owner.waiting(now) {
            return Err(wait_words(secs, assumed));
        }
        let req = ChatPostReq { target: draft.dest.clone(), thread: draft.thread.clone(), kind: ChatPostKind::Person, name: name.to_string(), text: draft.text.clone() };
        let host = row.host.clone();
        Ok(self.owner_outs(|o| o.post(host, req).into_iter().collect()))
    }

    /// Opens the picker for `row` and asks for the channels: for an unmapped row, or a mapped one (which begins with `No
    /// channel for this project`). Any other stand says its words instead. The channel's view is set aside, and its thread
    /// closed (a thread's Left key would otherwise give back the channel's view under the picker).
    pub(in crate::ui) fn open_picker(&mut self, row: Option<&ResultRowIdentity>) -> Result<Vec<Out>, String> {
        let Some(row) = row.filter(|r| self.rows.row() == Some(*r)) else { return Err("no row to choose a channel for".into()) };
        let mapped = match self.rows.stand() {
            Stand::Mapped => true,
            Stand::Unmapped => false,
            other => return Err(other.words(self.rows.host())),
        };
        if self.picker().is_some() {
            return Ok(Vec::new());
        }
        self.open_modal(Kind::Picker(Picker::new(row.clone(), mapped, slack_app_name(self.login))));
        Ok(vec![(row.host.clone(), OutgoingReq::ChatChannels)])
    }

    /// The picker ends, and the channel's view returns.
    fn close_picker(&mut self) -> bool {
        self.picker().is_some() && self.close_modal().is_some()
    }

    /// Escape: the picker closes and does not open by itself again for this row until the row or its link changes.
    pub(in crate::ui) fn dismiss_picker(&mut self) {
        if self.close_picker() {
            self.dismissed = self.rows.row().cloned();
        }
    }

    pub(in crate::ui) fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub(in crate::ui) fn picking(&self) -> bool {
        self.picker().is_some()
    }

    /// The picker's keys: Up and Down choose, typing narrows, and a channel link is picked as an entry is.
    pub(in crate::ui) fn picker_step(&mut self, down: bool) {
        self.picker_mut().into_iter().for_each(|p| p.step(down));
    }

    pub(in crate::ui) fn picker_edit(&mut self, edit: Edit) {
        if let Some(p) = self.picker_mut() {
            p.edit(edit);
        }
    }

    pub(in crate::ui) fn picker_enter(&mut self) -> Vec<Out> {
        if self.pair_sent.is_some() {
            return Vec::new();
        }
        let pick = self.picker_mut().and_then(Picker::enter);
        self.picked(pick)
    }

    pub(in crate::ui) fn picker_paste(&mut self, text: &str) -> Result<Vec<Out>, &'static str> {
        if self.pair_sent.is_some() {
            return Err("setting this project's channel…");
        }
        let pick = self.picker_mut().map(|p| p.paste(text)).transpose()?;
        Ok(self.picked(pick))
    }

    /// What picking an entry sends: one `chat.pair` for a channel, or for none, and nothing more until its reply.
    fn picked(&mut self, pick: Option<Pick>) -> Vec<Out> {
        let (Some(pick), Some(row)) = (pick, self.picker().map(|p| p.row.clone())) else { return Vec::new() };
        let channel = match pick {
            Pick::Channel(c) => Some(c),
            Pick::Unpair => None,
            // New tokens: the Connect step takes the picker's place, and stays until the person leaves it.
            Pick::Reconnect => {
                self.open_modal(Kind::Connect(Connect::new(row.host, ChatUnconfigured::NotConnected, self.login, false)));
                return Vec::new();
            }
        };
        self.pair_sent = Some(row.clone());
        vec![(row.host, OutgoingReq::ChatPair { row: row.workspace_id, channel })]
    }

    /// A `chat.pair` was answered (`None`: it may have been carried out). Every answer makes the row resolve again, so the
    /// title and the watch follow what the hub now has. A pair made closes the picker that sent it, as does one that may have
    /// been made; a refusal leaves the list and says why. A picker opened since never closes on an older pick's reply, and
    /// words for a row no longer shown are not said.
    fn on_paired(&mut self, host: &HostKey, row: &str, channel: Option<&ChatChannel>, reply: Option<&ChatReply<ChatPairRes>>) {
        self.pair_sent = self.pair_sent.take().filter(|r| &r.host != host || r.workspace_id != row);
        self.rows.resolve_again(host, row);
        let ours = self.picker().is_some_and(|p| p.sent() && &p.row.host == host && p.row.workspace_id == row);
        let private = ours && channel.is_some_and(|c| self.picker().is_some_and(|p| p.is_private(c)));
        let words = match reply {
            Some(reply) => pair_words(reply, private, &slack_app_name(self.login)),
            None => Some("the channel may have been set: the title shows what the hub has".into()),
        };
        if ours {
            self.picker_mut().into_iter().for_each(Picker::paired);
            if words.is_none() || reply.is_none() {
                self.close_picker();
                // A project set to no channel stays so until a person asks.
                if channel.is_none() {
                    self.dismissed = Some(ResultRowIdentity { host: host.clone(), workspace_id: row.to_string() });
                }
            }
        }
        self.notice = words.filter(|_| self.rows.wants(host, row));
    }

    /// What the body says instead of records: the picker, or, for an unmapped row, how to pick a channel, or that the channel
    /// is empty. While a pair is in flight, that it is.
    pub(in crate::ui) fn body(&self) -> Option<Body> {
        let mut body = match (self.modal.as_ref(), self.rows.stand()) {
            (Some(Modal { kind: Kind::Connect(c), scroll, .. }), _) => return Some(Body { scroll: *scroll, ..c.body() }),
            (Some(Modal { kind: Kind::Picker(p), scroll, .. }), _) => Body { scroll: *scroll, ..p.body() },
            (None, Stand::Unmapped) => Body::words("this project has no channel: press Enter to pick one"),
            (None, _) if self.thread.is_none() && self.owner.no_messages() => return Some(Body::words("no messages")),
            _ => return None,
        };
        if self.pair_sent.is_some() {
            body.head = "setting this project's channel…".into();
        }
        Some(body)
    }

    /// The foot asks for the posting name; a name half typed stays.
    pub(super) fn ask_name(&mut self) {
        self.asking.get_or_insert_with(String::new);
    }

    /// The name typed so far, while the foot asks for one.
    pub(in crate::ui) fn asking(&self) -> Option<&str> {
        self.asking.as_deref()
    }

    /// A key edits the name being asked for; nothing when the foot is not asking. A paste with a newline or another control
    /// character, or one past `MAX_NAME` bytes of name, is refused whole with words (never joined or cut), the name as it was.
    pub(in crate::ui) fn edit_name(&mut self, edit: Edit) -> Result<(), &'static str> {
        let Some(name) = self.asking.as_mut() else { return Ok(()) };
        let add = match edit {
            Edit::Text(s) if s.chars().any(char::is_control) => return Err("not pasted: a name holds no newline or other control character"),
            Edit::Text(s) => s,
            Edit::Space => " ",
            Edit::Backspace => {
                name.pop();
                return Ok(());
            }
            Edit::Newline => return Ok(()),
        };
        if name.len() + add.len() > MAX_NAME {
            return Err("name is full: nothing added");
        }
        name.push_str(add);
        Ok(())
    }

    /// The foot stops asking (Escape, or the row, the link or the view it asked in is gone); the draft was never touched.
    pub(in crate::ui) fn cancel_name(&mut self) {
        self.asking = None;
    }

    /// The words of the post outcome that has not yet been said.
    pub(in crate::ui) fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// The title: the channel, then the words of the outcome (the row's, or for a mapped row the fill's).
    pub(in crate::ui) fn title(&self) -> String {
        match self.modal.as_ref().map(|m| &m.kind) {
            Some(Kind::Connect(c)) => return c.title(),
            Some(Kind::Picker(p)) => return p.title(),
            None => {}
        }
        let host = self.rows.host();
        let words = match (self.rows.stand(), self.owner.fill()) {
            (Stand::Mapped, Some(Fill::Needed | Fill::Reading)) => Stand::Resolving.words(host),
            // The older pages are the channel's: a thread shows none of them.
            (Stand::Mapped, Some(Fill::Complete)) if self.thread.is_some() => String::new(),
            (Stand::Mapped, Some(Fill::Complete)) => self.owner.older_words().unwrap_or_default(),
            (Stand::Mapped, Some(Fill::Gap(Some(ChatCode::Unavailable)))) => "the hub's chat service is not running: its daemon log says why".into(),
            // A target refused for good is asked again only at a row or link change.
            (Stand::Mapped, Some(Fill::Gap(Some(code)))) if dead(code) => code.text().into(),
            (Stand::Mapped, Some(Fill::Gap(code))) => format!("not up to date: {}", code.map_or("no snapshot", ChatCode::text)),
            (stand, _) => stand.words(host),
        };
        let channel = self.owner.channel().map(|c| format!("{} ", self.rows.label(c))).unwrap_or_default();
        // Each phrase is its own: the thread's, then the link's and the watch's.
        let phrases: Vec<String> = self.thread.iter().map(Thread::words).chain([words]).filter(|w| !w.is_empty()).collect();
        let thread = if self.thread.is_some() { "· thread " } else { "" };
        format!(" chat · {channel}{thread}{}", if phrases.is_empty() { String::new() } else { format!("{} ", phrases.join(" · ")) })
    }

    /// Places the held records in `body` (the drawer's pixels) and shapes those that reach it, and returns the frame's
    /// rectangles for the drawer's `inside` (`view::ChatView::backs`), in painting order. They come only from this layout, so
    /// the frame cannot take the last frame's.
    pub(in crate::ui) fn layout(&mut self, fonts: &mut cosmic_text::FontSystem, body: ScreenRect, scale: f32, inside: ScreenRect) -> Vec<crate::ui::render::cells::CellQuad> {
        let now = chrono::Local::now();
        // The thread's records are its own; the channel's reader keeps the place it left.
        // A modal screen's own view holds no record; its body scrolls on its eased motion.
        if let Some(m) = self.modal.as_mut() {
            let rows = m.motion.spend_rows(std::time::Instant::now(), f32::from(self.body_end.1.max(1)));
            m.scroll = m.scroll.saturating_add_signed(rows as i16).min(self.body_end.0);
        }
        let records = if self.modal.is_some() { Vec::new() } else { self.thread.as_ref().map_or_else(|| self.owner.records(), Thread::records) };
        self.view.layout(fonts, &records, body, scale, now.timestamp(), *now.offset());
        if self.thread.is_none() && self.modal.is_none() {
            self.owner.anchored(self.view.anchor.as_ref().map(|a| a.0.clone()));
            self.owner.reaches(self.view.at_oldest);
        }
        self.view.backs(inside)
    }

    /// A key or the wheel asks to move the reader through the records (`view::ChatView::scroll`).
    /// Over a modal screen the key or the wheel scrolls its body instead (PageUp, PageDown, the wheel, Alt+Up and Alt+Down alike).
    pub(in crate::ui) fn scroll(&mut self, up: bool, step: Step, now: Instant, length: Duration) {
        if self.modal.is_some() {
            self.scroll_modal(up, step, now, length);
        } else {
            self.view.scroll(up, step, now, length);
        }
    }

    /// Whether the reader's scroll is still arriving, so frames must keep coming.
    pub(in crate::ui) fn moving(&self) -> bool {
        self.view.moving() || self.modal.as_ref().is_some_and(|m| m.motion.moving())
    }

    /// A mark key moves the mark (`view::ChatView::mark`).
    pub(in crate::ui) fn mark(&mut self, older: bool, now: Instant, length: Duration) {
        self.view.mark(older, now, length);
    }

    /// The reader asks for the newest.
    pub(in crate::ui) fn latest(&mut self) {
        self.view.latest();
    }

    /// ArrowRight: opens the marked message's thread. The channel's view is parked in it, so its position and mark return at close;
    /// the owner holds no anchor and asks for no page meanwhile. Nothing while a thread is open or none is marked.
    pub(in crate::ui) fn open_thread(&mut self) -> Vec<Out> {
        let Some(row) = self.rows.row().cloned() else { return Vec::new() };
        let (Some(mark), Some(target), None) = (self.view.mark.clone(), self.watch_for(&row), &self.thread) else { return Vec::new() };
        let Some((parent, deleted)) = self.owner.records().into_iter().find(|(r, _)| r.ts == mark) else { return Vec::new() };
        self.thread = Some(Thread::new(row.host, target.clone(), parent.clone(), deleted, self.parked_view()));
        self.owner.anchored(None);
        self.owner.reaches(false);
        self.thread_outs(Thread::start)
    }

    /// The channel's view, set aside for a thread. Nothing selectable is parked: a press made in the thread could not
    /// clear a selection held in the view parked behind it, and it would come back at close. Nothing owed is parked either.
    fn parked_view(&mut self) -> super::view::ChatView {
        let mut view = std::mem::take(&mut self.view);
        view.park();
        view
    }

    /// Escape: closes the open thread; the channel's view returns, its mark on the parent or, if that gave way, passed on.
    pub(in crate::ui) fn close_thread(&mut self) -> bool {
        let Some(t) = self.thread.take() else { return false };
        t.reading.iter().for_each(|req| self.flight.abandon(t.host(), req));
        self.view = t.parked;
        self.owner.reserve(0);
        true
    }

    /// The one restart of the open thread's walk: the read in flight is given up first (an equal reply still coming can then
    /// never be taken as the restart's answer), then the first page is asked for.
    fn restart_thread(&mut self) -> Vec<Out> {
        self.give_up_read();
        self.thread_outs(Thread::start)
    }

    /// ArrowLeft: closes the open thread as Escape does, but only while `row`'s draft is empty; whether it closed.
    pub(in crate::ui) fn leave_thread(&mut self, row: Option<&ResultRowIdentity>) -> bool {
        !row.and_then(|r| self.compose.draft(r)).is_some_and(|d| !d.text.is_empty()) && self.close_thread()
    }

    /// The open thread's read in flight is given up, so one held behind it is cancelled and its reply, if it comes, is dropped.
    fn give_up_read(&mut self) {
        if let Some((host, req)) = self.thread.as_mut().and_then(|t| t.reading.take().map(|r| (t.host().clone(), r))) {
            self.flight.abandon(&host, &req);
        }
    }

    /// Runs `f` on the open thread, tells the owner how much of the bound it holds, and sends its reads through the set.
    fn thread_outs(&mut self, f: impl FnOnce(&mut Thread) -> Vec<Out>) -> Vec<Out> {
        let outs = self.thread.as_mut().map(f).unwrap_or_default();
        self.owner.reserve(self.thread.as_ref().map_or(0, Thread::len));
        self.flight.send(outs)
    }

    #[cfg(test)]
    pub(super) fn point_at(&self, ts: &Ts, line: usize, byte: usize) -> Option<(f32, f32)> {
        self.view.point_at(ts, line, byte)
    }

    #[cfg(test)]
    pub(super) fn thread_len(&self) -> usize {
        self.thread.as_ref().map_or(0, Thread::len)
    }

    /// The chat is not shown: no record is prepared.
    pub(in crate::ui) fn hide(&mut self) {
        self.close_modal();
        self.close_thread();
        self.cancel_name();
        self.view.hide();
    }

    /// The selection's view calls: a press, the drag and the copy (`view::ChatView`).
    pub(in crate::ui) fn hit(&self, at: (f32, f32)) -> Option<ChatHit> {
        self.view.hit(at)
    }

    pub(in crate::ui) fn press(&mut self, hit: Option<ChatHit>) -> bool {
        self.view.press(hit)
    }

    pub(in crate::ui) fn extend_drag(&mut self, at: (f32, f32)) -> bool {
        self.view.extend_drag(at)
    }

    pub(in crate::ui) fn release(&mut self) {
        self.view.release();
    }

    /// What `chat.copy_selection` copies (`view::ChatView::copy_text`).
    pub(in crate::ui) fn copy_text(&self) -> Option<String> {
        self.view.copy_text()
    }

    pub(in crate::ui) fn areas(&self) -> Vec<crate::ui::render::text::ExtraArea<'_>> {
        self.view.areas()
    }

    /// The watched channel, if `row` (host and id) is the row the frame follows: a watch is ever the followed row's, so
    /// a row the frame has not yet moved to has none.
    fn watch_for(&self, row: &ResultRowIdentity) -> Option<&ChatTarget> {
        (self.rows.row() == Some(row)).then(|| self.owner.target()).flatten()
    }

    /// The person edits the draft of `row`, the row the chat shows at the key (not the last frame's, which a row switch has
    /// not yet moved). A draft begins in the watched channel only if that watch is `row`'s; a refusal is the status
    /// line's words.
    pub(in crate::ui) fn edit(&mut self, row: Option<&ResultRowIdentity>, edit: Edit) -> Result<(), &'static str> {
        let Some(row) = row else { return Err("no row to write for") };
        // A paste in the Connect step is a token: it is never typed into a draft, which Enter would post.
        if self.in_setup() {
            return Err("connect Slack first");
        }
        if self.owner.posting_row() == Some((&row.host, row.workspace_id.as_str())) {
            return Err("posting…");
        }
        let dest = self.watch_for(row).cloned();
        self.compose.edit(row, dest.as_ref().map(|t| (t, self.thread.as_ref().map(Thread::parent_ts))), edit)
    }

    /// Rows a listing retired lose their drafts.
    pub(in crate::ui) fn retire(&mut self, rows: &[ResultRowIdentity]) {
        self.compose.retire(rows);
    }

    /// What the compose rows are made of, while a channel is known to write to: the shown row's draft and where it goes
    /// (`reply in thread of h:mm AM`, the parent's time with its date before it for another day, `project::when`, then
    /// ` in <channel>` when the draft's channel is not the watched one, or
    /// `to <channel>`, spelled as the title spells it), else where the next draft would go: the open thread, or the
    /// watched channel. The foot is silent only when the destination is what the title shows: no thread in the draft,
    /// none open, and the draft's channel the watched one.
    pub(in crate::ui) fn compose_text(&self, focused: bool) -> Option<ComposeText> {
        let now = chrono::Local::now();
        self.compose_text_at(focused, now.timestamp(), *now.offset())
    }

    /// `compose_text` at a given time and offset, which the parent's time in `reply in thread of` is shown against.
    pub(super) fn compose_text_at(&self, focused: bool, now: i64, offset: chrono::FixedOffset) -> Option<ComposeText> {
        if self.modal.is_some() {
            return None;
        }
        if let Some(name) = &self.asking {
            return Some(ComposeText::asking(name, focused));
        }
        let draft = self.rows.row().and_then(|r| self.compose.draft(r));
        let dest = draft.map(|d| &d.dest).or_else(|| self.rows.row().and_then(|r| self.watch_for(r)))?;
        // With no draft, the line names where the next one would go: the open thread's parent.
        let thread = draft.map_or_else(|| self.thread.as_ref().map(Thread::parent_ts), |d| d.thread.as_ref());
        let elsewhere = self.owner.channel() != Some(dest.channel.as_str());
        let to = match thread {
            // A thread draft says its channel too when the title has moved to another one: the post goes where it was begun.
            Some(p) => {
                let at = if elsewhere { format!(" in {}", self.rows.label(dest.channel.as_str())) } else { String::new() };
                Some(format!("reply in thread of {}{at}", super::project::when(p.as_str(), now, offset)))
            }
            None if self.thread.is_none() && !elsewhere => None,
            None => Some(format!("to {}", self.rows.label(dest.channel.as_str()))),
        };
        let as_person = self.rows.row().is_some_and(|r| self.posts_as_person.contains(&r.host));
        Some(ComposeText::of(draft.map(|d| d.text.as_str()), to, focused, as_person))
    }

    pub(in crate::ui) fn set_foot(&mut self, rows: u16) {
        self.foot = rows;
    }

    pub(in crate::ui) fn foot(&self) -> u16 {
        self.foot
    }

    /// The wanted row's draft: its text and the channel it goes to.
    #[cfg(test)]
    pub(super) fn draft(&self) -> Option<(&str, &str)> {
        let d = self.compose.draft(self.rows.row()?)?;
        Some((d.text.as_str(), d.dest.channel.as_str()))
    }

    /// The anchor the owner holds for the trim.
    #[cfg(test)]
    pub(super) fn owner_anchor(&self) -> Option<sot_protocol::chat::Ts> {
        self.owner.anchor_held()
    }

    /// A test places the reader at the oldest record, as a frame's layout would.
    #[cfg(test)]
    pub(super) fn reach_oldest(&mut self, yes: bool) {
        self.owner.reaches(yes);
    }

    #[cfg(test)]
    pub(super) fn held(&self) -> usize {
        self.owner.records().len()
    }

    #[cfg(test)]
    pub(super) fn placed_text(&self) -> Vec<String> {
        self.view.placed_text()
    }

    #[cfg(test)]
    pub(super) fn marked(&self) -> Option<sot_protocol::chat::Ts> {
        self.view.mark.clone()
    }

    #[cfg(test)]
    pub(super) fn following(&self) -> bool {
        self.view.anchor.is_none()
    }

    #[cfg(test)]
    pub(super) fn fill(&self) -> Option<Fill> {
        self.owner.fill()
    }

    /// Runs `f` on the owner. A read the owner dropped (its watch stopped or replaced, or its link changed) is given up before
    /// the outs are sent, so a read `f` itself asks for is never cancelled by the same call; a read answered is already out
    /// of the set. The check compares reads by value, so an owner call must not give a read up and ask for an equal one in
    /// the same call.
    fn owner_outs(&mut self, f: impl FnOnce(&mut ChatOwner) -> Vec<Out>) -> Vec<Out> {
        let before = [self.owner.reading(), self.owner.paging()];
        let watched = self.owner.target().cloned();
        let outs = f(&mut self.owner);
        // A thread lives only under its channel's watch: a watch replaced by another channel's or stopped closes it.
        if self.owner.target() != watched.as_ref() {
            self.close_thread();
        }
        for (host, req) in before.into_iter().flatten().filter(|b| Some(b) != self.owner.reading().as_ref() && Some(b) != self.owner.paging().as_ref()) {
            self.flight.abandon(&host, &req);
        }
        if let Some(outcome) = self.owner.take_report() {
            self.notice = post_words(&outcome);
        }
        self.flight.send(outs)
    }
}
