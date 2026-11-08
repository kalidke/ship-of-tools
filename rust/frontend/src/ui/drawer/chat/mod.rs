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
    ChatCode, ChatHistoryReq, ChatPostReq, ChatPostRes, ChatService, ChatStatusEvt, ChatTarget, ChatWatchRes,
};
use sot_protocol::chat::{ChatRecord, Ts};

/// Between two snapshot reads, and the reads tried without one completing before the gap is shown instead.
const READ_PACE: Duration = Duration::from_secs(5);
const MAX_READS: u8 = 3;

type Out = (HostKey, OutgoingReq);

/// How whole the watched state is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum Fill {
    /// A snapshot is owed and none is in flight.
    Needed,
    Reading,
    Complete,
    /// Recovery cannot complete (reads kept failing, or the snapshot has more pages than the owner reads): shown, not hidden.
    Gap,
}

/// What the last post came to. A post the owner cannot prove was not made is `Unknown`, never sent or failed.
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
    /// The daemon holds this connection's watch (a reply to `chat.watch` came, and the link has not ended since).
    registered: bool,
    fill: Fill,
    /// The one snapshot read whose reply is applied.
    reading: Option<ChatHistoryReq>,
    last_read: Option<Instant>,
    reads: u8,
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
    pub(in crate::ui) fn watch(&mut self, host: HostKey, target: ChatTarget) -> Vec<Out> {
        let mut out: Vec<Out> = self.watch.take().map(|w| (w.host, OutgoingReq::ChatUnwatch { target: w.target })).into_iter().collect();
        let up = self.connected.contains(&host) && !self.unsupported.contains(&host);
        out.extend(up.then(|| (host.clone(), OutgoingReq::ChatWatch { target: target.clone() })));
        self.watch = Some(Watch { host, target, registered: false, fill: Fill::Needed, reading: None, last_read: None, reads: 0, records: HashMap::new() });
        out
    }

    /// Send a post, only when none is waiting for its reply; a post to a host that is not connected reads as unknown.
    pub(in crate::ui) fn post(&mut self, host: HostKey, req: ChatPostReq) -> Option<Out> {
        if self.posting.is_some() {
            return None;
        }
        if !self.connected.contains(&host) {
            self.shown = Some(PostShown::Unknown);
            return None;
        }
        self.posting = Some((host.clone(), req.clone()));
        Some((host, OutgoingReq::ChatPost { req }))
    }

    /// A link came or went. A coming link clears the unsupported mark and watches again; a going link leaves the post
    /// unknown and the watched state owed.
    pub(in crate::ui) fn on_connection(&mut self, host: &HostKey, up: bool) -> Vec<Out> {
        if up {
            self.connected.insert(host.clone());
            self.unsupported.remove(host);
        } else {
            self.connected.remove(host);
            if self.posting.take_if(|(h, _)| h == host).is_some() {
                self.shown = Some(PostShown::Unknown);
            }
        }
        match self.watch.as_mut().filter(|w| &w.host == host) {
            Some(w) => {
                (w.registered, w.fill, w.reading, w.reads) = (false, Fill::Needed, None, 0);
                let target = w.target.clone();
                up.then(|| (host.clone(), OutgoingReq::ChatWatch { target })).into_iter().collect()
            }
            None => Vec::new(),
        }
    }

    /// The event bus dropped events on `host`'s link.
    pub(in crate::ui) fn on_bus_lost(&mut self, host: &HostKey, bus: &str, now: Instant) -> Vec<Out> {
        let chat = bus == sot_protocol::op::CHAT_CHANGED || bus == sot_protocol::op::CHAT_STATUS;
        if chat && self.watch.as_ref().is_some_and(|w| &w.host == host) { self.resync(now) } else { Vec::new() }
    }

    /// The paced retry of a read a loss owes.
    pub(in crate::ui) fn tick(&mut self, now: Instant) -> Vec<Out> {
        if self.watch.as_ref().is_some_and(|w| w.fill == Fill::Needed) { self.read(now) } else { Vec::new() }
    }

    pub(in crate::ui) fn on_evt(&mut self, host: &HostKey, evt: ChatEvt, now: Instant) -> Vec<Out> {
        let mine = self.watch.as_ref().is_some_and(|w| &w.host == host);
        match evt {
            // Rows are resolved by the drawer (B5), which also starts the watch.
            ChatEvt::Resolved { .. } => Vec::new(),
            ChatEvt::Watched { target, reply } => self.on_watched(host, &target, reply, now),
            ChatEvt::History { req, reply } => self.on_history(host, &req, reply),
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

    /// Whether a status says the watched channel lost events; an entry for any other channel says nothing.
    fn status_lost(&self, s: &ChatStatusEvt) -> bool {
        let Some(w) = &self.watch else { return false };
        s.gap || s.service != ChatService::Running || s.resync.contains(&w.target.channel)
    }

    /// The watched state is incomplete: any read in flight no longer counts, and one snapshot read is owed.
    fn resync(&mut self, now: Instant) -> Vec<Out> {
        if let Some(w) = self.watch.as_mut() {
            (w.fill, w.reading) = (Fill::Needed, None);
        }
        self.read(now)
    }

    /// Start the owed snapshot read: not while one is in flight, not within the pace of the last, and not past the cap.
    fn read(&mut self, now: Instant) -> Vec<Out> {
        let Some(w) = self.watch.as_mut() else { return Vec::new() };
        if !w.registered || w.reading.is_some() || w.fill == Fill::Gap || self.unsupported.contains(&w.host) {
            return Vec::new();
        }
        if w.reads >= MAX_READS {
            w.fill = Fill::Gap;
            return Vec::new();
        }
        if w.last_read.is_some_and(|t| now.duration_since(t) < READ_PACE) {
            return Vec::new();
        }
        let req = ChatHistoryReq { target: w.target.clone(), thread: None, after: None, upper: None, limit: 20, cursor: None };
        (w.reads, w.last_read, w.fill, w.reading) = (w.reads + 1, Some(now), Fill::Reading, Some(req.clone()));
        vec![(w.host.clone(), OutgoingReq::ChatHistory { req })]
    }

    fn on_watched(&mut self, host: &HostKey, target: &ChatTarget, reply: ChatReply<ChatWatchRes>, now: Instant) -> Vec<Out> {
        match reply {
            ChatReply::Unsupported => {
                self.unsupported.insert(host.clone());
                Vec::new()
            }
            ChatReply::Done(_) => {
                match self.watch.as_mut().filter(|w| &w.host == host && &w.target == target) {
                    Some(w) => w.registered = true,
                    None => return Vec::new(),
                }
                self.read(now)
            }
            ChatReply::Refused(_) => Vec::new(),
        }
    }

    /// Applies the reply to the read in flight, and no other: a second reply to one read finds none.
    fn on_history(&mut self, host: &HostKey, req: &ChatHistoryReq, reply: ChatReply<sot_protocol::ops::ChatHistoryRes>) -> Vec<Out> {
        if matches!(reply, ChatReply::Unsupported) {
            self.unsupported.insert(host.clone());
        }
        let Some(w) = self.watch.as_mut().filter(|w| &w.host == host && w.reading.as_ref() == Some(req)) else { return Vec::new() };
        w.reading = None;
        w.fill = Fill::Needed;
        if let ChatReply::Done(res) = reply {
            w.records.extend(res.records.into_iter().map(|r| (r.ts.clone(), r)));
            // One page is read; a longer snapshot is the drawer's to page, and until then the gap is shown.
            (w.fill, w.reads) = if res.next.is_none() { (Fill::Complete, 0) } else { (Fill::Gap, 0) };
        }
        Vec::new()
    }

    fn on_posted(&mut self, host: &HostKey, req: &ChatPostReq, reply: ChatReply<ChatPostRes>) {
        if !self.posting.take_if(|(h, r)| h == host && r == req).is_some() {
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
    /// Sends what the owner asked for; a request that cannot be queued is dropped with a warning (the owner's pace and
    /// cap, or the next connection, ask again).
    fn send_chat(&self, out: Vec<Out>) {
        for (host, req) in out {
            if let Err(e) = self.send_to(&host, req) {
                tracing::warn!(error = %e, %host, "drop chat request — channel closed");
            }
        }
    }

    pub(in crate::ui) fn on_chat(&mut self, host: &HostKey, evt: ChatEvt) {
        let out = self.chat.on_evt(host, evt, Instant::now());
        self.send_chat(out);
    }

    pub(in crate::ui) fn on_chat_bus_lost(&mut self, host: &HostKey, bus: &str) {
        let out = self.chat.on_bus_lost(host, bus, Instant::now());
        self.send_chat(out);
    }

    pub(in crate::ui) fn on_chat_connection(&mut self, host: &HostKey, up: bool) {
        let out = self.chat.on_connection(host, up);
        self.send_chat(out);
    }

    pub(in crate::ui) fn tick_chat(&mut self) {
        let out = self.chat.tick(Instant::now());
        self.send_chat(out);
    }
}

#[cfg(test)]
mod tests;
