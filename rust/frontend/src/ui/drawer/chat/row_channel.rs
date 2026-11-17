//! Which row the chat follows: the wanted row (host and canonical id) is resolved once it has stayed the same for
//! `NAV_FIRE_DEBOUNCE`, on a connected host. It sends nothing itself and keeps only the last answer, as a `Stand`, until
//! the row or a link changes: a resolve makes no Slack call.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::net::dial::HostKey;
use crate::net::transport::{ChatReply, OutgoingReq};
use sot_protocol::ops::{ChatCode, ChatResolveRes, ChatUnconfigured};
use crate::ui::app::NAV_FIRE_DEBOUNCE;
use crate::ui::chrome::cell_row;
use crate::ui::ResultRowIdentity;

use super::owner::{Out, GAP_PACE};

/// How long after a `mapped` answer with no name the row is resolved again: the daemon looks the names up in the background
/// after its start, so a first answer may come before it ends. Four tries at most, each counted from the last answer; after
/// the last the title keeps the id until the row or a link changes.
const NAME_RETRIES: [Duration; 4] = [Duration::from_secs(1), Duration::from_secs(3), Duration::from_secs(10), Duration::from_secs(30)];

struct Wanted {
    id: ResultRowIdentity,
    since: Instant,
    asked: bool,
    /// What the last answer came to, and when; `None` while a resolve is owed or in flight.
    answer: Option<(Stand, Instant)>,
    /// The last mapped answer's channel id and the name it carried, if any: the title says `#name` for that id.
    name: Option<(String, String)>,
    /// Mapped answers with no name so far, and when the next one is asked for (`None` once the asking ended).
    nameless: usize,
    retry_at: Option<Instant>,
}

/// Where the wanted row stands, for the title (`ChatDrawer::title`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stand {
    Unlisted,
    Offline,
    Resolving,
    Mapped,
    Unmapped,
    /// The hub's chat is off for a reason a connect fixes: the drawer shows the Connect step.
    Unconfigured(ChatUnconfigured),
    NoConfig,
    NotHub,
    RowGone,
    Failed(ChatCode),
    NoChat,
}

/// The computer a sentence names: this one for the window's own, the host's name for another, and for a line with no host the
/// words a person knows it by.
pub(super) fn computer(host: &str) -> String {
    match host {
        "" => "the computer that runs your agents".into(),
        "local" => "this computer".into(),
        name => name.to_string(),
    }
}

/// Where a person finds why the chat is not running: the daemon's log, named by its file and computer.
fn log_words(c: &str) -> String {
    format!("the log of Ship of Tools' service on {c}, sotd.log, says why")
}

impl Stand {
    pub(super) fn words(self, host: &str) -> String {
        let c = computer(host);
        match self {
            Stand::Unlisted => format!("waiting for {c} to report this project"),
            Stand::Offline => format!("no connection to {c}"),
            Stand::Resolving => "reading…".into(),
            Stand::Mapped => String::new(),
            Stand::Unmapped => "no channel".into(),
            Stand::Unconfigured(_) => "Slack is not connected".into(),
            Stand::NoConfig => format!("the chat on {c} is not set up correctly: {}", log_words(&c)),
            Stand::NotHub => format!("the chat does not run on {c}; it runs only on the computer named hub in hosts.toml, and projects there show it"),
            Stand::RowGone => ChatCode::StaleRow.text().into(),
            Stand::Failed(code) => format!("the chat on {c} could not answer: {}", code.text()),
            Stand::NoChat => format!("Ship of Tools on {c} has no chat: update it"),
        }
    }
    /// The words for a chat that is not running on `host`, as the title shows them.
    pub(super) fn stopped_words(host: &str) -> String {
        let c = computer(host);
        format!("the chat is not running on {c}: {}", log_words(&c))
    }
}

/// The row is resolved again on the next frame: its answer, the names asked for and the retries are forgotten.
fn ask_again(w: &mut Wanted) {
    (w.asked, w.answer, w.nameless, w.retry_at) = (false, None, 0, None);
}

/// What a frame's wanted row came to.
#[derive(Default)]
pub(super) struct Tick {
    /// The wanted row differs from the last frame's (it went, or became another): the old watch ends.
    pub changed: bool,
    pub resolve: Option<Out>,
    /// The row is still inside the debounce: the frame pacer must come back.
    pub settling: bool,
}

#[derive(Default)]
pub(super) struct RowChannel {
    connected: HashSet<HostKey>,
    wanted: Option<Wanted>,
}

impl RowChannel {
    pub(super) fn tick(&mut self, wanted: Option<ResultRowIdentity>, now: Instant) -> Tick {
        let same = self.wanted.as_ref().map(|w| &w.id) == wanted.as_ref();
        if !same {
            self.wanted = wanted.map(|id| Wanted { id, since: now, asked: false, answer: None, name: None, nameless: 0, retry_at: None });
        }
        // Only a refusal that is neither the hub's nor the row's is asked again, after the pace.
        if let Some(w) = self.wanted.as_mut().filter(|w| matches!(w.answer, Some((Stand::Failed(_), at)) if now.duration_since(at) >= GAP_PACE)) {
            (w.asked, w.answer) = (false, None);
        }
        // A mapped answer with no name is asked for again on its pace; its answer stays, so the title and the watch hold.
        if let Some(w) = self.wanted.as_mut().filter(|w| w.retry_at.is_some_and(|at| now >= at)) {
            (w.asked, w.retry_at) = (false, None);
        }
        let mut tick = Tick { changed: !same, ..Tick::default() };
        if let Some(w) = self.wanted.as_mut().filter(|w| !w.asked && self.connected.contains(&w.id.host)) {
            if now.duration_since(w.since) >= NAV_FIRE_DEBOUNCE {
                w.asked = true;
                tick.resolve = Some((w.id.host.clone(), OutgoingReq::ChatResolve { row: w.id.workspace_id.clone() }));
            } else {
                tick.settling = true;
            }
        }
        tick
    }

    /// A link coming up or going: the row is resolved again on the next frame (a replayed resolve may answer too; the
    /// owner takes the same target twice as nothing).
    pub(super) fn on_connection(&mut self, host: &HostKey, up: bool) {
        if up { self.connected.insert(host.clone()); } else { self.connected.remove(host); }
        self.ask_again(host);
    }

    /// The wanted row on `host` is resolved again on the next frame (a link changed, or a connect was answered).
    pub(super) fn ask_again(&mut self, host: &HostKey) {
        self.wanted.iter_mut().filter(|w| &w.id.host == host).for_each(ask_again);
    }

    /// The wanted row on `host` is resolved again on the next frame, its answer standing until the new one comes: the hub's
    /// service stopped, and may have ended for good, so the title and the watch hold while the hub is asked why.
    pub(super) fn check_again(&mut self, host: &HostKey) {
        self.wanted.iter_mut().filter(|w| &w.id.host == host).for_each(|w| w.asked = false);
    }

    /// A pairing was answered, whatever it said (the hub may have saved it): the row is resolved again on the next frame.
    pub(super) fn resolve_again(&mut self, host: &HostKey, row: &str) {
        self.wanted.iter_mut().filter(|w| &w.id.host == host && w.id.workspace_id == row).for_each(ask_again);
    }

    /// When the wanted row is next resolved for its name, so the window can draw a frame at that moment.
    pub(super) fn next_ask(&self) -> Option<Instant> {
        self.wanted.as_ref().and_then(|w| w.retry_at)
    }

    /// The wanted row's answer, kept for the title.
    pub(super) fn answered(&mut self, reply: &ChatReply<ChatResolveRes>, now: Instant) {
        let stand = match reply {
            ChatReply::Done(ChatResolveRes::Mapped { .. }) => Stand::Mapped,
            ChatReply::Done(ChatResolveRes::Unmapped) => Stand::Unmapped,
            ChatReply::Done(ChatResolveRes::Unconfigured { reason }) => Stand::Unconfigured(*reason),
            ChatReply::Done(_) => Stand::NoConfig,
            ChatReply::Unsupported => Stand::NoChat,
            ChatReply::Refused(ChatCode::Unavailable) => Stand::NotHub,
            ChatReply::Refused(ChatCode::StaleRow) => Stand::RowGone,
            ChatReply::Refused(code) => Stand::Failed(*code),
        };
        let name = match reply {
            ChatReply::Done(ChatResolveRes::Mapped { channel, name: Some(name), .. }) => Some((channel.as_str().to_string(), name.clone())),
            _ => None,
        };
        let nameless = stand == Stand::Mapped && name.is_none();
        for w in self.wanted.iter_mut() {
            (w.answer, w.name) = (Some((stand, now)), name.clone());
            w.nameless = if nameless { w.nameless + 1 } else { 0 };
            w.retry_at = nameless.then(|| NAME_RETRIES.get(w.nameless - 1).map(|d| now + *d)).flatten();
        }
    }

    /// A channel as the title and the foot say it: `#name` when the last answer named this id, else the id. The name is
    /// the daemon's text, so it reaches the cells through `cell_row`.
    pub(super) fn label(&self, channel: &str) -> String {
        match self.wanted.as_ref().and_then(|w| w.name.as_ref()) {
            Some((id, name)) if id == channel => format!("#{}", cell_row(name)),
            _ => channel.to_string(),
        }
    }

    /// The wanted row.
    pub(super) fn row(&self) -> Option<&ResultRowIdentity> {
        self.wanted.as_ref().map(|w| &w.id)
    }

    pub(super) fn host(&self) -> &str {
        self.wanted.as_ref().map_or("", |w| w.id.host.as_str())
    }

    pub(super) fn stand(&self) -> Stand {
        match &self.wanted {
            None => Stand::Unlisted,
            Some(w) if !self.connected.contains(&w.id.host) => Stand::Offline,
            Some(w) => w.answer.map_or(Stand::Resolving, |(s, _)| s),
        }
    }

    /// Whether an answer is for the row now wanted.
    pub(super) fn wants(&self, host: &HostKey, row: &str) -> bool {
        self.wanted.as_ref().is_some_and(|w| &w.id.host == host && w.id.workspace_id == row)
    }
}
