//! Which row the chat follows: the wanted row (host and canonical id) is resolved once it has stayed the same for
//! `NAV_FIRE_DEBOUNCE`, on a connected host. It sends nothing itself and caches no answer: a resolve makes no Slack call.

use std::collections::HashSet;
use std::time::Instant;

use crate::net::dial::HostKey;
use crate::net::transport::{ChatReply, OutgoingReq};
use sot_protocol::ops::{ChatCode, ChatResolveRes};
use crate::ui::app::NAV_FIRE_DEBOUNCE;
use crate::ui::ResultRowIdentity;

use super::owner::{Out, GAP_PACE};

struct Wanted {
    id: ResultRowIdentity,
    since: Instant,
    asked: bool,
    /// What the last answer came to, and when; `None` while a resolve is owed or in flight.
    answer: Option<(Stand, Instant)>,
}

/// Where the wanted row stands, for the title (`ChatDrawer::title`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stand {
    Unlisted,
    Offline,
    Resolving,
    Mapped,
    Unmapped,
    NoConfig,
    NotHub,
    RowGone,
    Failed(ChatCode),
    NoChat,
}

impl Stand {
    pub(super) fn words(self, host: &str) -> String {
        match self {
            Stand::Unlisted => "waiting for this row's listing".into(),
            Stand::Offline => "not connected".into(),
            Stand::Resolving => "reading…".into(),
            Stand::Mapped => String::new(),
            Stand::Unmapped => "the row's folder maps to no channel".into(),
            Stand::NoConfig => "the hub has no usable chat configuration: its daemon log says why".into(),
            Stand::NotHub => format!("the chat runs on the hub, not on {host}"),
            Stand::RowGone => "this row is gone or was replaced".into(),
            Stand::Failed(code) => format!("the hub could not answer: {code:?}"),
            Stand::NoChat => "this daemon has no chat: update it".into(),
        }
    }
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
            self.wanted = wanted.map(|id| Wanted { id, since: now, asked: false, answer: None });
        }
        // Only a refusal that is neither the hub's nor the row's is asked again, after the pace.
        if let Some(w) = self.wanted.as_mut().filter(|w| matches!(w.answer, Some((Stand::Failed(_), at)) if now.duration_since(at) >= GAP_PACE)) {
            (w.asked, w.answer) = (false, None);
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
        self.wanted.iter_mut().filter(|w| &w.id.host == host).for_each(|w| (w.asked, w.answer) = (false, None));
    }

    /// The wanted row's answer, kept for the title; the stand it came to.
    pub(super) fn answered(&mut self, reply: &ChatReply<ChatResolveRes>, now: Instant) -> Stand {
        let stand = match reply {
            ChatReply::Done(ChatResolveRes::Mapped { .. }) => Stand::Mapped,
            ChatReply::Done(ChatResolveRes::Unmapped) => Stand::Unmapped,
            ChatReply::Done(_) => Stand::NoConfig,
            ChatReply::Unsupported => Stand::NoChat,
            ChatReply::Refused(ChatCode::Unavailable) => Stand::NotHub,
            ChatReply::Refused(ChatCode::StaleRow) => Stand::RowGone,
            ChatReply::Refused(code) => Stand::Failed(*code),
        };
        self.wanted.iter_mut().for_each(|w| w.answer = Some((stand, now)));
        stand
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
