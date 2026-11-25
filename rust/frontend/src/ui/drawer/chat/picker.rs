//! The channel picker: the list a person chooses a project's channel from (`chat.workspaces`, then `chat.channels` of each
//! Slack workspace a person opens, then one `chat.pair`), narrowed by typing. Every running workspace is a row that opens in
//! place into its channels, as the nav pane's trees do (Right opens, Left closes); each list is kept from its ask. It holds
//! the lists and the cursor (a `TreeView`, its rows drawn as the nav pane draws its tree rows) and sends nothing;
//! `ChatDrawer` sends what it returns, holds the one pair in flight and parks the channel's view behind it, as behind any of
//! its modal screens. Also here: `Body`, what a modal screen draws.

use sot_protocol::ops::{ChatChannel, ChatChannelInfo, ChatChannelsRes, ChatCode, ChatWorkspaceInfo, ChatWorkspacesRes};

use std::collections::{HashMap, HashSet};

use sot_protocol::TreeNode;

use super::compose::Edit;
use super::row_channel::{computer, log_words};
use crate::net::transport::ChatReply;
use crate::ui::chrome::{cell_row, wrap_status};
use crate::ui::nav::{format_tree_row, TreeRow, TreeView};
use crate::ui::ResultRowIdentity;

/// How a person invites a Slack app (`name`, the one the hub names, else `slack_app_name`'s) to a channel.
pub(super) fn invite(name: &str) -> String {
    format!("in that channel in Slack, type /invite @{name}")
}
/// Why a private channel is missing from a list and what to type in Slack (`name`, the person's app; `workspace`, which
/// workspace's app that is when several run): Slack lists a private channel only to an app that is in it.
fn private_words(name: &str, workspace: Option<&str>) -> String {
    let place = workspace.map_or_else(|| ":".to_string(), |w| format!(". For {w}:"));
    format!("A private channel is listed only once someone in it invites your Slack app{place} {}, then press Esc and open this list again.", invite(name))
}
/// The guide.
const GUIDE: &str = "Type to narrow the channels of the open workspaces, Up and Down to choose, Right to open a Slack workspace and Left to close it, Enter to pick, Esc to close. Picking a public channel your Slack app is not in makes it join, and Slack then shows that it joined.";
/// What an older computer's picker says: whose channels these are, that its Ship of Tools connects one workspace and who can
/// change that, and what "your Slack app" reads, with what it is unless the head's first words said so (`glossed`).
fn head_one(c: &str, glossed: bool) -> String {
    let app = if glossed { "Your Slack app (the bot that reads the channel and posts for your agents)" } else { "Your Slack app" };
    format!("These are the channels of your connected Slack workspace. Ship of Tools on {c} is too old to connect more than one Slack workspace: whoever runs Ship of Tools there can update it. \
{app} reads only the channels it is in.")
}
/// Where an older computer's guide says to read how to move the chat to another workspace.
const MOVE: &str = " To move the chat to another workspace, see the manual page \"Setting up Slack\": press F1, select \"Choose channel\" in the list, and press Enter; the page opens in your browser.";
/// What your Slack app is, at its first use in the head of a list of the connected workspaces' channels.
const APP_IS: &str = "your Slack app (the bot you make for yourself in each Slack workspace, which reads the channel and posts for your agents)";
/// What your Slack app reads, on such a list.
const APP_IN: &str = "Your Slack app reads only the channels it is in; picking a public channel adds it there.";

/// A Slack workspace as a title names it: its name, or `workspace <ID>` when none is known.
pub(super) fn workspace_label(id: &str, name: Option<&str>) -> String {
    match name {
        Some(n) => cell_row(n).into_owned(),
        None => format!("workspace {id}"),
    }
}

/// A Slack workspace as a sentence names it: `the Slack workspace <name>`, or `the Slack workspace with ID <ID>` when no
/// name is known (a workspace whose start failed since the daemon started has none).
pub(super) fn workspace_words(id: &str, name: Option<&str>) -> String {
    match name {
        Some(n) => format!("the Slack workspace {}", cell_row(n)),
        None => format!("the Slack workspace with ID {id}"),
    }
}

/// What Enter on an entry does. A pair names the workspace it pairs in (`None`: an older computer's one).
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Pick {
    Channel(ChatChannel, Option<String>),
    /// The project has no channel again.
    Unpair(Option<String>),
    /// The Connect step: a workspace added, or one given new Slack codes.
    Reconnect,
}

/// What a workspace not asked yet shows.
static READING: List = List::Reading;

/// The row id of workspace `ws` (empty for an older computer's one); a channel's row id continues it after a `/`.
fn ws_id(ws: &Option<String>) -> String {
    format!("chat-ws:{}", ws.as_deref().unwrap_or_default())
}

/// A row's node payload: the workspace and the channel it names, when it names them.
fn payload(ws: Option<&str>, channel: Option<&str>) -> serde_json::Map<String, serde_json::Value> {
    [("workspace", ws), ("channel", channel)].into_iter().filter_map(|(k, v)| Some((k.to_string(), v?.into()))).collect()
}

/// One row of the picker's tree at `depth`.
fn row(id: String, label: String, kind: &str, depth: usize, payload: serde_json::Map<String, serde_json::Value>) -> TreeRow {
    TreeRow { node: TreeNode { id, label, kind: kind.into(), has_children: false, badges: Vec::new(), payload }, depth, expanded: false }
}

/// The workspace a row names (`None`: an older computer's one, or a row of none).
fn workspace_of(row: &TreeRow) -> Option<String> {
    row.node.payload.get("workspace").and_then(|w| w.as_str()).map(str::to_string)
}

enum List {
    Reading,
    Listed { channels: Vec<ChatChannelInfo>, more: bool },
    Failed(String),
}

/// Which workspaces the picker can show.
enum Places {
    /// `chat.workspaces` is asked.
    Asking,
    /// The computer's Ship of Tools predates several workspaces: its one list, named by no workspace.
    Older,
    /// The running workspaces, in the daemon's order (by name).
    Known(Vec<ChatWorkspaceInfo>),
    /// None runs, or the list of them could not be read: the words, and the Connect step alone.
    Nowhere(String),
}

pub(super) struct Picker {
    pub(super) row: ResultRowIdentity,
    /// The row has a channel: the list offers `No channel for this project` after its workspaces.
    mapped: bool,
    filter: String,
    /// The rows as the list draws them (workspaces, their channels and notes, then the project's own rows), and the cursor on one.
    tree: TreeView,
    /// The list of the workspace the picker opened on, when it first arrives, moves the cursor to its first channel, unless a
    /// key moved it first.
    land: bool,
    /// The workspaces shown open (`None`: an older computer's one).
    open: HashSet<Option<String>>,
    /// Each workspace's list from the moment it was asked (`None`: an older computer's one), kept while the picker is open.
    lists: HashMap<Option<String>, List>,
    places: Places,
    /// The workspace to open on, when it runs: the row's, or the one just connected; once the workspaces are known, the one
    /// the picker opened on.
    start: Option<String>,
    /// The Connect step of a row whose workspace failed opened it, to move the project into another workspace.
    away: bool,
    /// This picker sent the pair in flight (`ChatDrawer::pair_sent`), so its reply is this picker's to close on, and the
    /// workspace it went to (`None`: an older computer's), which the reply's words name wherever the list has moved since.
    sent: Option<Option<String>>,
    /// The name the setup gives the person's Slack app (`slack_app_name`): an invite says it only when the workspace's
    /// entry names no Slack app (`app_of`).
    app: String,
    /// The row's channel now, for a mapped row: its workspace, its id and its label (`#name`, else the id).
    current: Option<(String, ChatChannel, String)>,
}

/// The team and the channel a pasted link names: the channel is its last path segment, when it has Slack's id shape (`C` or
/// `G`, then capitals and digits, eight characters at least), and the team the segment before it, when that has a team's
/// (`T` or `E`, the same rule: the browser's `app.slack.com/client/T.../C...`; Slack's Copy link,
/// `<domain>.slack.com/archives/C...`, names none); an id alone is a channel. Any other paste (a word, a token) names nothing.
pub(super) fn link_target(text: &str) -> Option<(Option<String>, ChatChannel)> {
    let path = text.trim().split(['?', '#']).next()?;
    let mut segments = path.trim_end_matches('/').rsplit('/');
    let shaped = |s: &str, first: [char; 2]| s.len() >= 8 && s.starts_with(first) && s.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let segment = segments.next()?;
    let channel = shaped(segment, ['C', 'G']).then(|| ChatChannel::try_from(segment.to_string()).ok()).flatten()?;
    let team = segments.next().filter(|t| shaped(t, ['T', 'E'])).map(str::to_string);
    Some((team, channel))
}

impl Picker {
    pub(super) fn new(row: ResultRowIdentity, mapped: bool, app: String) -> Self {
        Picker { row, mapped, filter: String::new(), tree: TreeView::new(), land: false, open: HashSet::new(), lists: HashMap::new(), places: Places::Asking, start: None, away: false, sent: None, app, current: None }
    }

    /// The row's channel now (its workspace, id and label), which the head names when the list shown lacks it.
    pub(super) fn current(self, current: Option<(String, ChatChannel, String)>) -> Self {
        Picker { current, ..self }
    }

    /// The words for a row's channel the whole list of its workspace lacks: Slack lists a private channel only to an app in
    /// it, so the connected app is not in it (none when the list was cut or is not held).
    fn missing_words(&self) -> Option<String> {
        let (workspace, id, label) = self.current.as_ref()?;
        // An older computer's one list is kept under no workspace, whatever workspace the row names.
        let key = (!matches!(self.places, Places::Older)).then(|| workspace.clone());
        let Some(List::Listed { channels, more: false }) = self.lists.get(&key) else { return None };
        (!channels.iter().any(|c| &c.id == id)).then(|| {
            format!("This project's channel, {label}, is not in this list: your Slack app (the bot that reads the channel and posts for your agents) is not in it, so this panel cannot show it. To keep it, {}, then press Esc. To change it, pick another channel below.\n\n", invite(&self.app_of(key.as_deref())))
        })
    }

    /// The Slack app an invite names: the cursor's workspace's, by the name Slack gives its bot when the hub gave one, else
    /// the person's (`app`).
    fn invite_app(&self) -> String {
        self.app_of(self.here().as_deref())
    }

    /// The Slack app an invite names in workspace `ws`: the one its entry names, else the person's.
    fn app_of(&self, ws: Option<&str>) -> String {
        self.info(ws).and_then(|w| w.app_name.as_deref()).map_or_else(|| self.app.clone(), |n| cell_row(n).into_owned())
    }

    /// The workspace a pair's words name: the one the pair in flight was sent to (every caller runs with one sent).
    fn pair_workspace(&self) -> Option<String> {
        self.sent.clone().flatten()
    }

    /// The Slack app a pair's refusal tells a person to invite: its own workspace's, wherever the list has moved since.
    pub(super) fn pair_app(&self) -> String {
        self.app_of(self.pair_workspace().as_deref())
    }

    /// The row under the cursor.
    fn cursor_row(&self) -> Option<&TreeRow> {
        self.tree.rows().get(self.tree.selected())
    }

    /// The workspace the cursor is in: the one its row names, else the one the picker opened on.
    fn here(&self) -> Option<String> {
        self.cursor_row().and_then(workspace_of).or_else(|| self.start.clone())
    }

    /// The workspace `No channel for this project` unpairs in: the project's own when it runs, else the one the picker opened on.
    fn unpair_workspace(&self) -> Option<String> {
        self.current.as_ref().map(|c| c.0.clone()).filter(|w| self.info(Some(w)).is_some()).or_else(|| self.start.clone())
    }

    /// The workspace rows, in order: each running workspace by its name, or an older computer's one.
    fn spaces(&self) -> Vec<(Option<String>, String)> {
        match &self.places {
            Places::Known(running) => running.iter().map(|w| (Some(w.id.clone()), workspace_label(&w.id, w.name.as_deref()))).collect(),
            Places::Older => vec![(None, "your connected Slack workspace".to_string())],
            _ => Vec::new(),
        }
    }

    /// The picker opening on `start` when that workspace runs.
    pub(super) fn starting(self, start: Option<String>) -> Self {
        Picker { start, ..self }
    }

    /// The picker the Connect step opens to move a project out of a workspace whose connection failed.
    pub(super) fn away_from_failed(self) -> Self {
        Picker { away: true, ..self }
    }

    pub(super) fn away(&self) -> bool {
        self.away
    }

    fn c(&self) -> String {
        computer(&self.row.host)
    }

    /// The running workspace `ws`, when there is one.
    fn info(&self, ws: Option<&str>) -> Option<&ChatWorkspaceInfo> {
        match &self.places {
            Places::Known(running) => running.iter().find(|w| Some(w.id.as_str()) == ws),
            _ => None,
        }
    }

    fn several(&self) -> bool {
        matches!(&self.places, Places::Known(running) if running.len() > 1)
    }

    /// The answer to `chat.workspaces`: the workspace whose channels are asked next (`Some(None)`: an older computer's one
    /// list), or `None` when there is none to ask. It opens on `start` when that one runs, else on the default when it runs,
    /// else on the first running.
    pub(super) fn on_workspaces(&mut self, reply: ChatReply<ChatWorkspacesRes>) -> Option<Option<String>> {
        // A late second answer (the picker closed and opened again) never moves a person off the list they are on.
        if !matches!(self.places, Places::Asking) {
            return None;
        }
        let c = self.c();
        let next = match reply {
            ChatReply::Done(res) => {
                let running: Vec<ChatWorkspaceInfo> = res.workspaces.into_iter().filter(|w| w.running).collect();
                if running.is_empty() {
                    self.places = Places::Nowhere(format!("no Slack workspace is running on {c} right now: choose Connect a Slack workspace below; {}", log_words(&c)));
                    None
                } else {
                    let at = [self.start.as_deref(), Some(res.default.as_str())]
                        .into_iter()
                        .flatten()
                        .find_map(|want| running.iter().position(|w| w.id == want))
                        .unwrap_or(0);
                    self.start = Some(running[at].id.clone());
                    self.places = Places::Known(running);
                    Some(self.start.clone())
                }
            }
            ChatReply::Unsupported => {
                self.places = Places::Older;
                self.start = None;
                Some(None)
            }
            ChatReply::Refused(code) => {
                self.places = Places::Nowhere(format!("could not read which Slack workspaces the chat on {c} is connected to: {}", code.text()));
                None
            }
        };
        // The workspace asked next is open, with its list kept from now on, reading; the cursor starts on its row.
        if let Some(ws) = &next {
            self.lists.insert(ws.clone(), List::Reading);
            self.open.insert(ws.clone());
            self.land = true;
        }
        self.install();
        if let Some(ws) = &next {
            self.select(&ws_id(ws));
        }
        next
    }

    /// The rows: each workspace, its channels beneath it when it is open (a note says what its list holds until it holds
    /// channels), then `No channel for this project` over an empty filter on a mapped row, and the Connect step last over an
    /// empty filter (alone when no workspace runs or the workspaces could not be read). Each is one tree row, its mark after
    /// its label; the filter narrows the channels of the open workspaces.
    fn rows(&self) -> Vec<TreeRow> {
        let plain = self.filter.is_empty();
        let spaces = self.spaces();
        let mut out = Vec::new();
        for (ws, label) in &spaces {
            let open = self.open.contains(ws);
            let mut r = row(ws_id(ws), label.clone(), "chat_workspace", 0, payload(ws.as_deref(), None));
            (r.node.has_children, r.expanded) = (true, open);
            out.push(r);
            if open {
                out.extend(self.children(ws));
            }
        }
        if plain && self.mapped && !spaces.is_empty() {
            out.push(row("chat:none".into(), "No channel for this project".into(), "chat_unpair", 0, payload(self.unpair_workspace().as_deref(), None)));
        }
        if plain && !matches!(self.places, Places::Asking) {
            let (label, mark) = match self.places {
                Places::Older => ("Connect Slack again", "connect again with new Slack codes (same workspace)"),
                _ => ("Connect a Slack workspace", "add a Slack workspace, or give one new Slack codes"),
            };
            out.push(row("chat:connect".into(), format!("{label} · {mark}"), "chat_connect", 0, Default::default()));
        }
        out
    }

    /// The rows beneath the open workspace `ws`: its channels by name, or a note on what its list holds.
    fn children(&self, ws: &Option<String>) -> Vec<TreeRow> {
        let at = ws_id(ws);
        let note = |tag: &str, text: &str| row(format!("{at}/({tag})"), text.to_string(), "chat_note", 1, Default::default());
        let (channels, more) = match self.lists.get(ws).unwrap_or(&READING) {
            List::Listed { channels, more } => (channels, *more),
            List::Failed(_) => return vec![note("note", "could not list its channels")],
            List::Reading => return vec![note("note", "reading its channels…")],
        };
        let needle = self.filter.strip_prefix('#').unwrap_or(&self.filter).to_lowercase();
        let mut shown: Vec<&ChatChannelInfo> = channels.iter().filter(|c| c.name.to_lowercase().contains(&needle)).collect();
        shown.sort_by_key(|c| c.name.to_lowercase());
        let mut out: Vec<TreeRow> = shown.into_iter().map(|c| {
            let mark = match (c.private, c.member) {
                (false, true) => "",
                (false, false) => " · your Slack app joins it when you pick it",
                // Slack lists a private channel only when the app is in it.
                (true, _) => " · private; your Slack app is in it",
            };
            row(format!("{at}/{}", c.id.as_str()), format!("#{}{mark}", cell_row(&c.name)), "chat_channel", 1, payload(ws.as_deref(), Some(c.id.as_str())))
        }).collect();
        if out.is_empty() {
            out.push(note("note", if channels.is_empty() { "no channel your Slack app can see" } else { "no channel matches" }));
        }
        if more {
            out.push(note("more", "More channels exist than are listed: paste a channel's link to pick one."));
        }
        out
    }

    /// Right (or Enter) on a closed workspace opens it: the workspace to ask (`Some(None)`: an older computer's one), when it
    /// has no list yet or its list failed.
    pub(super) fn open(&mut self) -> Option<Option<String>> {
        let ws = self.cursor_row().filter(|r| r.node.kind == "chat_workspace").map(workspace_of)?;
        if !self.open.insert(ws.clone()) {
            return None;
        }
        let ask = matches!(self.lists.get(&ws), None | Some(List::Failed(_)));
        if ask {
            self.lists.insert(ws.clone(), List::Reading);
        }
        self.install();
        ask.then_some(ws)
    }

    /// Left: an open workspace closes, a row beneath one goes to its workspace's row, and a row at the top does nothing.
    pub(super) fn close(&mut self) {
        let at = self.tree.selected();
        let Some(row) = self.cursor_row() else { return };
        if row.depth > 0 {
            if let Some(parent) = self.tree.rows()[..at].iter().rposition(|r| r.depth == 0) {
                self.tree.select(parent);
            }
        } else if row.node.kind == "chat_workspace" {
            let ws = workspace_of(row);
            if self.open.remove(&ws) {
                self.install();
            }
        }
    }

    /// What Enter on `row` does.
    fn pick_of(row: &TreeRow) -> Option<Pick> {
        let payload = &row.node.payload;
        let workspace = payload.get("workspace").and_then(|w| w.as_str()).map(str::to_string);
        match row.node.kind.as_str() {
            "chat_channel" => Some(Pick::Channel(ChatChannel::try_from(payload.get("channel")?.as_str()?.to_string()).ok()?, workspace)),
            "chat_unpair" => Some(Pick::Unpair(workspace)),
            "chat_connect" => Some(Pick::Reconnect),
            _ => None,
        }
    }

    /// Put the cursor on the row with id `id`, when there is one.
    fn select(&mut self, id: &str) {
        if let Some(at) = self.tree.rows().iter().position(|r| r.node.id == id) {
            self.tree.select(at);
        }
    }

    /// The entries as they stand, drawn from `rows`: the cursor stays on its row; a row that went sends it to its workspace's
    /// row, else to the first.
    fn install(&mut self) {
        let at = self.tree.selected();
        let id = self.cursor_row().map(|r| r.node.id.clone());
        let parent = self.tree.rows().get(..=at).and_then(|up| up.iter().rev().find(|r| r.depth == 0)).map(|r| r.node.id.clone());
        self.open.iter().for_each(|ws| self.tree.mark_expanded(&ws_id(ws)));
        self.tree.set_flat(self.rows());
        if self.cursor_row().map(|r| &r.node.id) != id.as_ref() {
            parent.iter().for_each(|p| self.select(p));
        }
    }

    /// Put the cursor on the first channel shown, else on the first row.
    fn first_channel(&mut self) {
        self.tree.select_clamped(self.tree.rows().iter().position(|r| r.node.kind == "chat_channel").unwrap_or(0));
    }

    /// A channel list's answer, kept for the workspace it answers when this picker asked it (shown when that workspace is
    /// open); one this picker never asked is dropped. The first list of the workspace the picker opened on takes the cursor
    /// to its first channel, unless a key moved it first.
    pub(super) fn on_channels(&mut self, workspace: Option<&str>, reply: ChatReply<ChatChannelsRes>) {
        let key = workspace.map(str::to_string);
        if !self.lists.contains_key(&key) {
            return;
        }
        let of = self.info(workspace).map_or_else(String::new, |w| format!(" of {}", workspace_words(&w.id, w.name.as_deref())));
        let list = match reply {
            ChatReply::Done(res) => List::Listed { channels: res.channels, more: res.more },
            ChatReply::Unsupported => List::Failed(format!("Ship of Tools on {} is too old to list channels: update it", self.c())),
            ChatReply::Refused(code) => List::Failed(format!("could not list the channels{of}: {}", code.text())),
        };
        self.lists.insert(key.clone(), list);
        self.install();
        if self.land {
            self.land = false;
            let rows = self.tree.rows();
            let first = rows.iter().position(|r| r.node.id == ws_id(&key)).map(|i| i + 1).filter(|&i| rows.get(i).is_some_and(|r| r.node.kind == "chat_channel"));
            first.iter().for_each(|&i| self.tree.select(i));
        }
    }

    pub(super) fn edit(&mut self, edit: Edit) {
        match edit {
            Edit::Text(s) if self.filter.len() + s.len() <= sot_protocol::chat::MAX_NAME => self.filter.push_str(s),
            Edit::Backspace => drop(self.filter.pop()),
            _ => return,
        }
        self.land = false;
        self.install();
        self.first_channel();
    }

    pub(super) fn step(&mut self, down: bool) {
        let from = self.tree.selected();
        if down { self.tree.move_down() } else { self.tree.move_up() }
        if self.tree.selected() != from {
            self.land = false;
        }
    }

    /// Enter: the entry under the cursor. The drawer sends it, one pair at a time.
    pub(super) fn enter(&mut self) -> Option<Pick> {
        let pick = Self::pick_of(self.cursor_row()?)?;
        // The pair goes to the workspace the picked row names.
        self.sent = match &pick {
            Pick::Channel(_, ws) | Pick::Unpair(ws) => Some(ws.clone()),
            Pick::Reconnect => None,
        };
        Some(pick)
    }

    /// A pasted channel link or id is picked as the list's entry is, in the workspace the link names when it names one the
    /// computer runs (one it is not connected to is refused, never paired elsewhere); any other paste is refused whole.
    pub(super) fn paste(&mut self, text: &str) -> Result<Pick, String> {
        let (team, channel) = link_target(text).ok_or("not pasted: that is not a Slack channel link")?;
        // Until the workspaces are known, a link's workspace could not be told from the cursor's.
        match &self.places {
            Places::Asking => return Err(format!("not pasted: still reading which Slack workspaces the chat on {} is connected to; paste it again in a moment", self.c())),
            Places::Nowhere(_) => return Err(format!("not pasted: no Slack workspace is running on {} right now", self.c())),
            _ => {}
        }
        let workspace = match (&self.places, team) {
            (Places::Known(running), Some(team)) if running.iter().any(|w| w.id == team) => Some(team),
            (Places::Known(_), Some(_)) => {
                return Err(format!(
                    "not pasted: that channel is in a Slack workspace the chat on {} is not connected to; to connect it, choose Connect a Slack workspace at the end of this list",
                    self.c()
                ))
            }
            _ => self.here(),
        };
        self.sent = Some(workspace.clone());
        Ok(Pick::Channel(channel, workspace))
    }

    /// Whether this picker sent the pair that was answered.
    pub(super) fn sent(&self) -> bool {
        self.sent.is_some()
    }

    /// The pair this picker sent was answered.
    pub(super) fn paired(&mut self) {
        self.sent = None;
    }

    /// Whether the listed channel `id` is private in the workspace the pair went to, for the words of a refusal.
    pub(super) fn is_private(&self, id: &ChatChannel) -> bool {
        matches!(self.lists.get(&self.pair_workspace()), Some(List::Listed { channels, .. }) if channels.iter().any(|c| &c.id == id && c.private))
    }

    /// The words of a pair refused `no_such_channel`: the channel is not in the workspace the pair went to, or (Slack answers
    /// both alike) it is a private channel the app is not in.
    pub(super) fn no_such_channel(&self) -> String {
        let ws = self.pair_workspace();
        let name = self.info(ws.as_deref()).map_or_else(|| "this Slack workspace".into(), |w| workspace_words(&w.id, w.name.as_deref()));
        let others = if self.several() { "; Right on another Slack workspace in this list shows its channels" } else { "" };
        format!("not set: that channel is not in {name}{others}; if it is a private channel, your Slack app is not in it: {}, then pick it again", invite(&self.pair_app()))
    }

    pub(super) fn title(&self) -> String {
        let filter = if self.filter.is_empty() { String::new() } else { format!("  filter: {}", cell_row(&self.filter)) };
        format!(" chat · choose this project's channel{filter} ")
    }

    /// The listed head: the words of each list refused, the row's channel when its workspace's list lacks it, whose channels
    /// these are, what your Slack app is, and why a private channel may be missing.
    fn listed_head(&self) -> String {
        // With several workspaces the invite says whose Slack app it names: the cursor's workspace's.
        let whose = self.info(self.here().as_deref()).filter(|_| self.several()).map(|w| workspace_words(&w.id, w.name.as_deref()));
        let private = private_words(&self.invite_app(), whose.as_deref());
        // `your Slack app` is said what it is at its first use: in the missing channel's words when they lead.
        let missing = self.missing_words();
        let app = if missing.is_some() { "your Slack app" } else { APP_IS };
        let refused: String = self.spaces().iter().filter_map(|(ws, _)| match self.lists.get(ws) {
            Some(List::Failed(words)) => Some(format!("{words}\n\n")),
            _ => None,
        }).collect();
        let head = match &self.places {
            Places::Known(running) if running.len() > 1 => format!(
                "These are the {} Slack workspaces the chat on {} is connected to, each with the channels {app} can see in it, and Connect a Slack workspace is at the end. {APP_IN} {private}",
                running.len(),
                self.c()
            ),
            Places::Known(running) => format!(
                "These are the channels of {} that {app} can see. {APP_IN} {private} To add another Slack workspace, choose Connect a Slack workspace at the end of this list.",
                workspace_words(&running[0].id, running[0].name.as_deref())
            ),
            _ => format!("{} {private}", head_one(&self.c(), missing.is_none())),
        };
        format!("{refused}{}{head}", missing.unwrap_or_default())
    }

    pub(super) fn body(&self) -> Body {
        let (head, guide) = match &self.places {
            Places::Asking => (format!("reading which Slack workspaces the chat on {} is connected to…", self.c()), String::new()),
            Places::Nowhere(words) => (words.clone(), "Esc to close.".to_string()),
            Places::Older => (format!("{}\n", self.listed_head()), format!("{GUIDE}{MOVE}")),
            Places::Known(_) => (format!("{}\n", self.listed_head()), GUIDE.to_string()),
        };
        let cursor = self.tree.selected();
        let rows = self.tree.rows().iter().enumerate().map(|(i, r)| format_tree_row(r, i == cursor, false)).collect();
        Body { head, rows, cursor, guide, ..Body::default() }
    }
}

/// The words of a refused `chat.pair` (`private`: the channel is one the list marked private; `app`: the person's Slack app;
/// `host`: the computer the chat runs on). `no_such_channel`'s are the picker's (`Picker::no_such_channel`), which knows the
/// workspace.
pub(super) fn pair_words(reply: &ChatReply<sot_protocol::ops::ChatPairRes>, private: bool, app: &str, host: &str) -> Option<String> {
    Some(match reply {
        ChatReply::Done(_) => return None,
        ChatReply::Unsupported => format!("not set: Ship of Tools on {} is too old to set a channel: update it", computer(host)),
        ChatReply::Refused(ChatCode::NotInChannel) if private => format!("not set: your Slack app is not in that private channel; {}, then pick it again", invite(app)),
        ChatReply::Refused(ChatCode::NotInChannel) => format!("not set: your Slack app could not join that channel; invite it: {}", invite(app)),
        ChatReply::Refused(code) => format!("not set: {}", code.text()),
    })
}

/// How a row of a body is drawn: `Words` in the default style, `Row` and `Cursor` as the nav pane draws a tree row off and
/// on its cursor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::ui) enum Tone {
    Words,
    Row,
    Cursor,
}

/// What the drawer shows in its body instead of records: a head, the rows of a list (each drawn whole) with the cursor on
/// one, a guide, and a line pinned at the foot (a screen's keys), which stays in view while the rest scrolls by `scroll` rows.
#[derive(Default)]
pub(in crate::ui) struct Body {
    pub(super) head: String,
    pub(super) rows: Vec<String>,
    pub(super) cursor: usize,
    pub(super) guide: String,
    pub(super) pin: String,
    pub(super) scroll: u16,
}

/// What a body says while it holds more rows below than it shows.
pub(super) const MORE: &str = "PageDown for more";

/// `text` wrapped to `width`: each of its lines alone, a blank one kept, and a numbered item's own lines indented under its
/// words.
fn wrapped(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split('\n').flat_map(|line| {
        let item = line.trim_start().split_once(". ").filter(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        let indent = item.map_or(0, |(n, _)| line.len() - line.trim_start().len() + n.len() + 2);
        if line.is_empty() {
            return vec![String::new()];
        }
        let rows = wrap_status(line, width, width.saturating_sub(indent).max(1));
        rows.into_iter().enumerate().map(|(i, r)| if i == 0 { r } else { format!("{}{r}", " ".repeat(indent)) }).collect()
    }).collect()
}

impl Body {
    pub(in crate::ui) fn words(text: &str) -> Self {
        Body { head: text.to_string(), ..Body::default() }
    }

    /// A body of a head and a list, for the chrome's tests.
    #[cfg(test)]
    pub(in crate::ui) fn listing(head: &str, rows: &[&str], cursor: usize) -> Self {
        Body { head: head.to_string(), rows: rows.iter().map(|r| r.to_string()).collect(), cursor, ..Body::default() }
    }

    /// The rows for a body `width` cells wide and `height` tall, each with its tone: the head, the list (a window that keeps
    /// the cursor in view, at least three rows) and the guide, wrapped, from `scroll` rows down and cut at the height; the
    /// pinned line, wrapped, below a blank row at the foot. While rows remain below, `MORE` says so: in that blank row, or
    /// with no pinned line in the last row.
    pub(in crate::ui) fn rows(&self, width: usize, height: usize) -> Vec<(String, Tone)> {
        let (room, content, pin) = self.parts(width, height);
        let end = content.len().saturating_sub(room);
        let start = usize::from(self.scroll).min(end);
        let more = start < end;
        let words = |s: String| (s, Tone::Words);
        let mut out: Vec<(String, Tone)> = content.into_iter().skip(start).take(room).collect();
        if !pin.is_empty() {
            out.resize(room, words(String::new()));
            out.push(words(if more { MORE.to_string() } else { String::new() }));
            out.extend(pin.into_iter().map(words));
        } else if more {
            if let Some(last) = out.last_mut() {
                *last = words(MORE.to_string());
            }
        }
        out.into_iter().take(height).collect()
    }

    /// The text of `rows`, for the tests.
    #[cfg(test)]
    pub(in crate::ui) fn lines(&self, width: usize, height: usize) -> Vec<String> {
        self.rows(width, height).into_iter().map(|(text, _)| text).collect()
    }

    /// How far the body scrolls at that size: the rows of it that do not fit above the pinned line.
    pub(in crate::ui) fn scroll_end(&self, width: usize, height: usize) -> u16 {
        let (room, content, _) = self.parts(width, height);
        u16::try_from(content.len().saturating_sub(room)).unwrap_or(u16::MAX)
    }

    /// The rows above the pinned line, what scrolls in them, and the pinned rows.
    fn parts(&self, width: usize, height: usize) -> (usize, Vec<(String, Tone)>, Vec<String>) {
        let pin = wrapped(&self.pin, width);
        let room = if pin.is_empty() { height } else { height.saturating_sub(pin.len() + 1) };
        let (head, guide) = (wrapped(&self.head, width), wrapped(&self.guide, width));
        // Over a list the head gives way after the guide: the list keeps three rows (the cursor stays in view) and the row that
        // says more is below before the head keeps any, so a short drawer shows the list and a tall one the whole head.
        let head = if self.rows.is_empty() { head } else { head.into_iter().take(room.saturating_sub(self.rows.len().min(3) + 1)).collect() };
        let window = room.saturating_sub(head.len() + guide.len() + usize::from(!guide.is_empty())).max(3);
        let start = (self.cursor + 1).saturating_sub(window);
        let words = |s: String| (s, Tone::Words);
        let rows = self.rows.iter().enumerate().skip(start).take(window).map(|(i, r)| (r.clone(), if i == self.cursor { Tone::Cursor } else { Tone::Row }));
        let gap = (!guide.is_empty()).then(String::new);
        (room, head.into_iter().map(words).chain(rows).chain(gap.map(words)).chain(guide.into_iter().map(words)).collect(), pin)
    }
}
