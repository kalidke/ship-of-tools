//! The channel picker: the list a person chooses a project's channel from (`chat.channels`, then one `chat.pair`), narrowed by
//! typing. It holds the list and the choice and sends nothing; `ChatDrawer` sends what it returns, holds the one pair in flight
//! and parks the channel's view behind it, as behind any of its modal screens. Also here: `Body`, what a modal screen draws.

use sot_protocol::ops::{ChatChannel, ChatChannelInfo, ChatChannelsRes, ChatCode};

use super::compose::Edit;
use crate::net::transport::ChatReply;
use crate::ui::chrome::{cell_row, wrap_status};
use crate::ui::ResultRowIdentity;

/// How a person invites their app (`name`, `slack_app_name`) to a channel.
fn invite(name: &str) -> String {
    format!("in that channel in Slack, type /invite @{name}")
}
const GUIDE: &str = "Type to narrow the list, Up and Down to choose, Enter to pick, Esc to close. Your app reads only the channels it is in. \
Picking a channel it is not in makes it join; Slack then shows that your app joined. A private channel appears in this list once you invite your app: ";

/// What Enter on an entry does.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Pick {
    Channel(ChatChannel),
    /// The project has no channel again.
    Unpair,
    /// New tokens for the hub: the Connect step.
    Reconnect,
}

enum List {
    Reading,
    Listed { channels: Vec<ChatChannelInfo>, more: bool },
    Failed(String),
}

pub(super) struct Picker {
    pub(super) row: ResultRowIdentity,
    /// The row has a channel: the list begins with `No channel for this project`.
    mapped: bool,
    filter: String,
    choice: usize,
    list: List,
    /// This picker sent the pair in flight (`ChatDrawer::pair_sent`), so its reply is this picker's to close on.
    pairing: bool,
    /// The person's app, as the invite names it.
    app: String,
}

impl Picker {
    pub(super) fn new(row: ResultRowIdentity, mapped: bool, app: String) -> Self {
        Picker { row, mapped, filter: String::new(), choice: 0, list: List::Reading, pairing: false, app }
    }

    /// The entries, in order: `No channel for this project` only over an empty filter (a narrowed list never unpairs), the
    /// channels by name, and `Connect Slack again` last over an empty filter (a list that failed offers it alone). Each is its
    /// pick, its label and its mark.
    fn entries(&self) -> Vec<(Pick, String, &'static str)> {
        let plain = self.filter.is_empty();
        let again = plain.then(|| (Pick::Reconnect, "Connect Slack again".to_string(), "new tokens for the hub"));
        let channels = match &self.list {
            List::Listed { channels, .. } => channels,
            List::Failed(_) => return again.into_iter().collect(),
            List::Reading => return Vec::new(),
        };
        let needle = self.filter.strip_prefix('#').unwrap_or(&self.filter).to_lowercase();
        let mut shown: Vec<&ChatChannelInfo> = channels.iter().filter(|c| c.name.to_lowercase().contains(&needle)).collect();
        shown.sort_by_key(|c| c.name.to_lowercase());
        let mut out = Vec::new();
        if plain && self.mapped {
            out.push((Pick::Unpair, "No channel for this project".to_string(), ""));
        }
        out.extend(shown.into_iter().map(|c| {
            let mark = match (c.private, c.member) {
                (false, true) => "",
                (false, false) => "your app joins it when you pick it",
                // Slack lists a private channel only when the app is in it.
                (true, _) => "private, app invited",
            };
            (Pick::Channel(c.id.clone()), format!("#{}", cell_row(&c.name)), mark)
        }));
        out.extend(again);
        out
    }

    /// Where the choice begins: on the first channel, so a mapped row's Enter never unpairs by being pressed first.
    fn home(&self) -> usize {
        usize::from(self.filter.is_empty() && self.mapped)
    }

    pub(super) fn on_channels(&mut self, reply: ChatReply<ChatChannelsRes>) {
        self.list = match reply {
            ChatReply::Done(res) => List::Listed { channels: res.channels, more: res.more },
            ChatReply::Unsupported => List::Failed("the hub's daemon must be updated to list channels".into()),
            ChatReply::Refused(code) => List::Failed(format!("could not list the channels: {}", code.text())),
        };
        self.choice = self.home();
    }

    pub(super) fn edit(&mut self, edit: Edit) {
        match edit {
            Edit::Text(s) if self.filter.len() + s.len() <= sot_protocol::chat::MAX_NAME => self.filter.push_str(s),
            Edit::Backspace => drop(self.filter.pop()),
            _ => return,
        }
        self.choice = self.home();
    }

    pub(super) fn step(&mut self, down: bool) {
        let last = self.entries().len().saturating_sub(1);
        self.choice = if down { (self.choice + 1).min(last) } else { self.choice.saturating_sub(1) };
    }

    /// Enter: the chosen entry. The drawer sends it, one pair at a time.
    pub(super) fn enter(&mut self) -> Option<Pick> {
        let (pick, ..) = self.entries().into_iter().nth(self.choice)?;
        self.pairing = true;
        Some(pick)
    }

    /// A pasted channel link or id is picked as the list's entry is; any other paste is refused whole.
    pub(super) fn paste(&mut self, text: &str) -> Result<Pick, &'static str> {
        let channel = link_channel(text).ok_or("not pasted: that is not a Slack channel link")?;
        self.pairing = true;
        Ok(Pick::Channel(channel))
    }

    /// Whether this picker sent the pair that was answered.
    pub(super) fn sent(&self) -> bool {
        self.pairing
    }

    /// The pair this picker sent was answered.
    pub(super) fn paired(&mut self) {
        self.pairing = false;
    }

    /// Whether the listed channel `id` is private, for the words of a refusal.
    pub(super) fn is_private(&self, id: &ChatChannel) -> bool {
        matches!(&self.list, List::Listed { channels, .. } if channels.iter().any(|c| &c.id == id && c.private))
    }

    pub(super) fn title(&self) -> String {
        let filter = if self.filter.is_empty() { String::new() } else { format!("  filter: {}", cell_row(&self.filter)) };
        format!(" chat · choose this project's channel{filter} ")
    }

    pub(super) fn body(&self) -> Body {
        let entries = self.entries();
        let (head, guide) = match &self.list {
            List::Reading => ("reading Slack's channels…".to_string(), String::new()),
            List::Failed(words) => (words.clone(), "Esc to close.".to_string()),
            List::Listed { more, .. } => {
                let head = if entries.is_empty() { "no channel matches" } else { "" };
                let more = if *more { " More channels exist than are listed: paste a channel's link to pick one." } else { "" };
                (head.to_string(), format!("{GUIDE}{}.{more}", invite(&self.app)))
            }
        };
        Body { head, entries: entries.into_iter().map(|(_, label, mark)| (label, mark)).collect(), chosen: self.choice, guide, ..Body::default() }
    }
}

/// The channel id a pasted link names: its last path segment, when it has Slack's id shape (`C` or `G`, then capitals and
/// digits, eight characters at least); an id alone is one. Any other paste (a word, a token) is no channel and is sent nowhere.
pub(super) fn link_channel(text: &str) -> Option<ChatChannel> {
    let path = text.trim().split(['?', '#']).next()?;
    let segment = path.trim_end_matches('/').rsplit('/').next()?;
    let shaped = segment.len() >= 8 && segment.starts_with(['C', 'G']) && segment.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    shaped.then(|| ChatChannel::try_from(segment.to_string()).ok()).flatten()
}

/// The words of a refused `chat.pair` (`private`: the channel is one the list marked private; `app`: the person's app).
pub(super) fn pair_words(reply: &ChatReply<sot_protocol::ops::ChatPairRes>, private: bool, app: &str) -> Option<String> {
    Some(match reply {
        ChatReply::Done(_) => return None,
        ChatReply::Unsupported => "not set: the hub's daemon must be updated to set a channel".into(),
        ChatReply::Refused(ChatCode::NotInChannel) if private => format!("not set: the app is not in that private channel; {}, then pick it again", invite(app)),
        ChatReply::Refused(ChatCode::NotInChannel) => format!("not set: the app could not join that channel; invite it: {}", invite(app)),
        ChatReply::Refused(code) => format!("not set: {}", code.text()),
    })
}

/// What the drawer shows in its body instead of records: a head, the entries (label and mark) with one chosen, a guide, and a
/// line pinned at the foot (a screen's keys), which stays in view while the rest scrolls by `scroll` rows.
#[derive(Default)]
pub(in crate::ui) struct Body {
    pub(super) head: String,
    pub(super) entries: Vec<(String, &'static str)>,
    pub(super) chosen: usize,
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

    /// The rows for a body `width` cells wide and `height` tall: the head, the entries (a window that keeps the choice in view,
    /// at least three rows) and the guide, wrapped, from `scroll` rows down and cut at the height; the pinned line, wrapped,
    /// below a blank row at the foot. While rows remain below, `MORE` says so: in that blank row, or with no pinned line in
    /// the last row.
    pub(in crate::ui) fn lines(&self, width: usize, height: usize) -> Vec<String> {
        let (room, content, pin) = self.parts(width, height);
        let end = content.len().saturating_sub(room);
        let start = usize::from(self.scroll).min(end);
        let more = start < end;
        let mut out: Vec<String> = content.into_iter().skip(start).take(room).collect();
        if !pin.is_empty() {
            out.resize(room, String::new());
            out.push(if more { MORE.to_string() } else { String::new() });
            out.extend(pin);
        } else if more {
            if let Some(last) = out.last_mut() {
                MORE.clone_into(last);
            }
        }
        out.into_iter().take(height).collect()
    }

    /// How far the body scrolls at that size: the rows of it that do not fit above the pinned line.
    pub(in crate::ui) fn scroll_end(&self, width: usize, height: usize) -> u16 {
        let (room, content, _) = self.parts(width, height);
        u16::try_from(content.len().saturating_sub(room)).unwrap_or(u16::MAX)
    }

    /// The rows above the pinned line, what scrolls in them, and the pinned rows.
    fn parts(&self, width: usize, height: usize) -> (usize, Vec<String>, Vec<String>) {
        let pin = wrapped(&self.pin, width);
        let room = if pin.is_empty() { height } else { height.saturating_sub(pin.len() + 1) };
        let (head, guide) = (wrapped(&self.head, width), wrapped(&self.guide, width));
        let window = room.saturating_sub(head.len() + guide.len() + usize::from(!guide.is_empty())).max(3);
        let start = (self.chosen + 1).saturating_sub(window);
        let across = self.entries.iter().map(|(label, _)| label.chars().count()).max().unwrap_or(0);
        let rows = self.entries.iter().enumerate().skip(start).take(window).map(|(i, (label, mark))| {
            format!("{} {label:<across$}  {mark}", if i == self.chosen { '>' } else { ' ' }).trim_end().to_string()
        });
        let gap = (!guide.is_empty()).then(String::new);
        (room, head.into_iter().chain(rows).chain(gap).chain(guide).collect(), pin)
    }
}
