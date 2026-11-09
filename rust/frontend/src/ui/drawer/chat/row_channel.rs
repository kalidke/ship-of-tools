//! Which row the chat follows: the wanted row (host and canonical id) is resolved once it has stayed the same for
//! `NAV_FIRE_DEBOUNCE`, on a connected host. It sends nothing itself and caches no answer: a resolve makes no Slack call.

use std::collections::HashSet;
use std::time::Instant;

use crate::net::dial::HostKey;
use crate::net::transport::OutgoingReq;
use crate::ui::app::NAV_FIRE_DEBOUNCE;
use crate::ui::ResultRowIdentity;

use super::owner::Out;

struct Wanted {
    id: ResultRowIdentity,
    since: Instant,
    asked: bool,
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
            self.wanted = wanted.map(|id| Wanted { id, since: now, asked: false });
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
        self.wanted.iter_mut().filter(|w| &w.id.host == host).for_each(|w| w.asked = false);
    }

    /// Whether an answer is for the row now wanted.
    pub(super) fn wants(&self, host: &HostKey, row: &str) -> bool {
        self.wanted.as_ref().is_some_and(|w| &w.id.host == host && w.id.workspace_id == row)
    }
}
