//! The project chat's private owner: one watched channel's state, the one post in flight, and the reads that keep the
//! watched state whole. It consumes fe-net's chat events and replies and returns the requests to send; it sends nothing
//! itself and replays no mutation. No drawer reads it yet (the drawer is B5's), so only its tests call most of it.
#![cfg_attr(not(test), allow(dead_code, reason = "the chat drawer, its first caller, is a later landing"))]

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::net::dial::HostKey;
use super::super::*;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use sot_protocol::ops::{
    ChatCode, ChatHistoryReq, ChatHistoryRes, ChatPostReq, ChatPostRes, ChatService, ChatStatusEvt, ChatTarget, ChatWatchRes,
    ChatWatchState,
};
use sot_protocol::chat::{ChatRecord, Ts};

/// Between two watch attempts, and the attempts without a completed snapshot before the gap is shown instead.
const PACE: Duration = Duration::from_secs(5);
const MAX_TRIES: u8 = 3;

type Out = (HostKey, OutgoingReq);

/// How whole the watched state is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum Fill {
    /// A recovery is owed: the watch to send again, or the snapshot read not yet sent.
    Needed,
    Reading,
    Complete,
    /// Recovery cannot complete (the daemon refused the watch, or the attempts ran out): shown, not hidden. It ends when the
    /// link returns; the drawer owes a way out before that.
    Gap,
}

/// What the last post came to. `Unknown` means the post may exist: a post the owner never wrote is `Refused`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum PostShown {
    Posted(Ts),
    Refused(ChatCode),
    Limited(u64),
    Unknown,
}

struct Watch {
    host: HostKey,
    target: ChatTarget,
    /// The daemon's watch reply said `complete`, so a snapshot read may follow.
    registered: bool,
    fill: Fill,
    /// The one snapshot read in flight. `stale`: a loss came while it was, so its reply is not the snapshot.
    reading: Option<ChatHistoryReq>,
    stale: bool,
    last_try: Option<Instant>,
    tries: u8,
    records: HashMap<Ts, ChatRecord>,
}

#[derive(Default)]
pub(in crate::ui) struct ChatOwner {
    connected: HashSet<HostKey>,
    /// Hosts whose daemon answered a chat op as unknown; cleared per host when its connection comes back.
    unsupported: HashSet<HostKey>,
    watch: Option<Watch>,
    posting: Option<(HostKey, ChatPostReq)>,
    shown: Option<PostShown>,
}

impl ChatOwner {
    /// Watch `target` on `host`, replacing any earlier watch.
    pub(in crate::ui) fn watch(&mut self, host: HostKey, target: ChatTarget, now: Instant) -> Vec<Out> {
        let mut out: Vec<Out> = self.watch.take().map(|w| (w.host, OutgoingReq::ChatUnwatch { target: w.target })).into_iter().collect();
        self.watch = Some(Watch { host, target, registered: false, fill: Fill::Needed, reading: None, stale: false, last_try: None, tries: 0, records: HashMap::new() });
        out.extend(self.drive(now));
        out
    }

    /// Send a post, only when none is waiting for its reply. One to a host that is not connected is never written, and
    /// reads as refused.
    pub(in crate::ui) fn post(&mut self, host: HostKey, req: ChatPostReq) -> Option<Out> {
        if self.posting.is_some() {
            return None;
        }
        if !self.connected.contains(&host) {
            self.shown = Some(PostShown::Refused(ChatCode::Unavailable));
            return None;
        }
        self.posting = Some((host.clone(), req.clone()));
        Some((host, OutgoingReq::ChatPost { req }))
    }

    /// A link came or went. Either way the watch starts over: a coming link clears the unsupported mark and the attempts,
    /// a going link leaves the post unknown.
    pub(in crate::ui) fn on_connection(&mut self, host: &HostKey, up: bool, now: Instant) -> Vec<Out> {
        if up {
            self.connected.insert(host.clone());
            self.unsupported.remove(host);
        } else {
            self.connected.remove(host);
            if self.posting.take_if(|(h, _)| h == host).is_some() {
                self.shown = Some(PostShown::Unknown);
            }
        }
        if let Some(w) = self.watch.as_mut().filter(|w| &w.host == host) {
            (w.registered, w.fill, w.reading, w.stale, w.last_try, w.tries) = (false, Fill::Needed, None, false, None, 0);
        }
        self.drive(now)
    }

    /// The event bus dropped events on `host`'s link; `chat.changed` is the chat's bus.
    pub(in crate::ui) fn on_bus_lost(&mut self, host: &HostKey, bus: &str, now: Instant) -> Vec<Out> {
        if bus == sot_protocol::op::CHAT_CHANGED && self.watch.as_ref().is_some_and(|w| &w.host == host) { self.resync(now) } else { Vec::new() }
    }

    /// The paced retry of what a loss owes.
    pub(in crate::ui) fn tick(&mut self, now: Instant) -> Vec<Out> {
        self.drive(now)
    }

    pub(in crate::ui) fn on_evt(&mut self, host: &HostKey, evt: ChatEvt, now: Instant) -> Vec<Out> {
        let mine = self.watch.as_ref().is_some_and(|w| &w.host == host);
        match evt {
            // Rows are resolved by the drawer (B5), which also starts the watch.
            ChatEvt::Resolved { .. } => Vec::new(),
            ChatEvt::Watched { target, reply } => self.on_watched(host, &target, reply, now),
            ChatEvt::History { req, reply } => self.on_history(host, &req, reply, now),
            ChatEvt::Posted { req, reply } => {
                self.on_posted(host, &req, reply);
                Vec::new()
            }
            ChatEvt::Changed(c) => {
                if let Some(w) = self.watch.as_mut().filter(|w| mine && w.target.workspace == c.workspace && w.target.channel == c.channel) {
                    w.records.extend(c.put.into_iter().map(|r| (r.ts.clone(), r)));
                    c.deleted.iter().for_each(|ts| drop(w.records.remove(ts)));
                }
                Vec::new()
            }
            ChatEvt::Status(s) if mine && self.status_lost(&s) => self.resync(now),
            ChatEvt::Unreadable if mine => self.resync(now),
            ChatEvt::Status(_) | ChatEvt::Unreadable => Vec::new(),
        }
    }

    /// Whether a status tells the watched channel to resync: its entry, or a service that is not running. A status for
    /// another channel's entry says nothing.
    fn status_lost(&self, s: &ChatStatusEvt) -> bool {
        self.watch.as_ref().is_some_and(|w| s.service != ChatService::Running || s.resync.contains(&w.target.channel))
    }

    /// The one recovery for every loss: forget the registration (a read in flight no longer counts) and watch again.
    fn resync(&mut self, now: Instant) -> Vec<Out> {
        if let Some(w) = self.watch.as_mut() {
            w.registered = false;
            w.stale = w.reading.is_some();
            if w.fill != Fill::Gap {
                w.fill = Fill::Needed;
            }
        }
        self.drive(now)
    }

    /// Send what the watched state is owed: its snapshot read once the daemon's watch is complete, else the watch itself,
    /// which is paced and counted against the cap. Nothing while a read is in flight or the state is whole.
    fn drive(&mut self, now: Instant) -> Vec<Out> {
        let Some(w) = self.watch.as_mut() else { return Vec::new() };
        if w.fill != Fill::Needed || w.reading.is_some() || !self.connected.contains(&w.host) || self.unsupported.contains(&w.host) {
            return Vec::new();
        }
        if w.registered {
            let req = ChatHistoryReq { target: w.target.clone(), thread: None, after: None, upper: None, limit: 20, cursor: None };
            (w.fill, w.reading) = (Fill::Reading, Some(req.clone()));
            return vec![(w.host.clone(), OutgoingReq::ChatHistory { req })];
        }
        if w.tries >= MAX_TRIES {
            w.fill = Fill::Gap;
        } else if w.last_try.is_none_or(|t| now.duration_since(t) >= PACE) {
            (w.tries, w.last_try) = (w.tries + 1, Some(now));
            return vec![(w.host.clone(), OutgoingReq::ChatWatch { target: w.target.clone() })];
        }
        Vec::new()
    }

    /// Only a watch reply that says `complete` starts a read, and only while the state needs one. A watch still pending
    /// is sent again by `tick`; a refused one is a gap.
    fn on_watched(&mut self, host: &HostKey, target: &ChatTarget, reply: ChatReply<ChatWatchRes>, now: Instant) -> Vec<Out> {
        if matches!(reply, ChatReply::Unsupported) {
            self.unsupported.insert(host.clone());
        }
        let Some(w) = self.watch.as_mut().filter(|w| &w.host == host && &w.target == target && w.fill == Fill::Needed) else { return Vec::new() };
        match reply {
            ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete }) => w.registered = true,
            ChatReply::Refused(_) => w.fill = Fill::Gap,
            ChatReply::Done(_) | ChatReply::Unsupported => {}
        }
        self.drive(now)
    }

    /// Applies the reply to the read in flight, and no other: a second reply to one read finds none, and the reply of a
    /// read a loss outdated replaces nothing.
    fn on_history(&mut self, host: &HostKey, req: &ChatHistoryReq, reply: ChatReply<ChatHistoryRes>, now: Instant) -> Vec<Out> {
        if matches!(reply, ChatReply::Unsupported) {
            self.unsupported.insert(host.clone());
        }
        let Some(w) = self.watch.as_mut().filter(|w| &w.host == host && w.reading.as_ref() == Some(req)) else { return Vec::new() };
        w.reading = None;
        w.fill = Fill::Needed;
        match reply {
            ChatReply::Done(res) if !w.stale => {
                // The snapshot replaces what was held: a record it no longer holds was deleted during the loss. Its
                // `next` cursor is the older history, which the drawer pages.
                w.records = res.records.into_iter().map(|r| (r.ts.clone(), r)).collect();
                (w.fill, w.tries) = (Fill::Complete, 0);
            }
            ChatReply::Done(_) => {}
            // A refused or unsupported read goes back to the watch, on the pace.
            _ => w.registered = false,
        }
        w.stale = false;
        self.drive(now)
    }

    fn on_posted(&mut self, host: &HostKey, req: &ChatPostReq, reply: ChatReply<ChatPostRes>) {
        if self.posting.take_if(|(h, r)| h == host && r == req).is_none() {
            return;
        }
        self.shown = Some(match reply {
            ChatReply::Done(ChatPostRes::Posted { ts }) => PostShown::Posted(ts),
            ChatReply::Done(ChatPostRes::Refused { code }) => PostShown::Refused(code),
            ChatReply::Done(ChatPostRes::Limited { retry_after_secs, .. }) => PostShown::Limited(retry_after_secs),
            ChatReply::Unsupported => {
                self.unsupported.insert(host.clone());
                PostShown::Refused(ChatCode::Unavailable)
            }
            ChatReply::Done(ChatPostRes::Unknown) | ChatReply::Refused(_) => PostShown::Unknown,
        });
    }
}

impl State {
    /// Sends what the owner asked for; a request that cannot be queued is dropped with a warning (the owner's pace, or
    /// the next connection, ask again).
    fn with_chat(&mut self, f: impl FnOnce(&mut ChatOwner, Instant) -> Vec<Out>) {
        for (host, req) in f(&mut self.chat, Instant::now()) {
            if let Err(e) = self.send_to(&host, req) {
                tracing::warn!(error = %e, %host, "drop chat request — channel closed");
            }
        }
    }

    pub(in crate::ui) fn on_chat(&mut self, host: &HostKey, evt: ChatEvt) {
        self.with_chat(|c, now| c.on_evt(host, evt, now));
    }

    pub(in crate::ui) fn on_chat_bus_lost(&mut self, host: &HostKey, bus: &str) {
        self.with_chat(|c, now| c.on_bus_lost(host, bus, now));
    }

    pub(in crate::ui) fn on_chat_connection(&mut self, host: &HostKey, up: bool) {
        self.with_chat(|c, now| c.on_connection(host, up, now));
    }

    pub(in crate::ui) fn tick_chat(&mut self) {
        self.with_chat(|c, now| c.tick(now));
    }
}

#[cfg(test)]
mod tests;
