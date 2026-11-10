//! The project chat's private owner: one watched channel's state, the one post in flight, and the reads that keep the
//! watched state whole. It consumes fe-net's chat events and replies and returns the requests to send; it sends nothing
//! itself and replays no mutation. Only `ChatDrawer` calls it; no drawer view reads it yet (the view is B5's).
#![cfg_attr(not(test), allow(dead_code, reason = "the drawer's view, which reads the records and the post outcome, is a later landing"))]

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::net::dial::HostKey;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use sot_protocol::ops::{
    ChatChangedEvt, ChatCode, ChatHistoryReq, ChatHistoryRes, ChatPostReq, ChatPostRes, ChatService, ChatStatusEvt, ChatTarget, ChatWatchRes,
    ChatWatchState, MAX_PAGE,
};
use sot_protocol::chat::{ChatRecord, Ts};

/// Between two watch attempts: the first waits `FIRST_PACE`, the rest `PACE`.
const FIRST_PACE: Duration = Duration::from_secs(1);
const PACE: Duration = Duration::from_secs(5);
/// What the daemon is given before a gap is shown: for its chat service to start (it refuses `unavailable` meanwhile), and,
/// from the first watch it admits, for its slowest snapshot (about 18.5 s at worst, so the last watch goes at 21 s).
const WINDOW: Duration = Duration::from_secs(20);
/// Between two watches in `Gap`, which has no cap: each also starts the daemon's own retry of its snapshot.
const GAP_PACE: Duration = Duration::from_secs(60);
/// The daemon's model keeps its newest 100 messages per channel (sot-chat's `watch::MAX_KEPT`).
const KEPT: usize = 100;

pub(super) type Out = (HostKey, OutgoingReq);

/// How whole the watched state is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fill {
    /// A recovery is owed: the watch to send again, or the snapshot read not yet sent.
    Needed,
    Reading,
    Complete,
    /// Recovery has not completed: shown, with the daemon's refusal when it gave one (`None`: the snapshot never completed).
    /// A watch goes out every `GAP_PACE`, and a `complete` reply to one reads as from `Needed`; a target refused for good
    /// (`dead`) is not asked again until a row or link change.
    Gap(Option<ChatCode>),
}

/// What the last post came to. `Unknown` means the post may exist: a post the owner never wrote is `Refused`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PostShown {
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
    /// When the recovery began, and the watch the daemon first admitted (anything but `unavailable`) in it; with none, the
    /// window runs from the start.
    began: Instant,
    admitted: Option<Instant>,
    /// Why the last snapshot read was refused: the reason of a gap that the daemon's watch did not give.
    read_refused: Option<ChatCode>,
    records: HashMap<Ts, ChatRecord>,
    /// The oldest timestamp the daemon still tracks: the snapshot's oldest when older history exists, raised by the trim.
    floor: Option<Ts>,
    /// The `chat.changed` events that came while the read was in flight: they are newer than its snapshot and win over it.
    overlay: Vec<ChatChangedEvt>,
}

/// Refusals that a later ask cannot clear: the mapping is read at daemon start, and a listing change follows a stale row.
fn dead(code: ChatCode) -> bool {
    matches!(code, ChatCode::StaleRow | ChatCode::StaleMapping | ChatCode::Unmapped)
}

/// A timestamp's order: seconds by length then digits, then the fraction.
fn ts_order(t: &Ts) -> (usize, &str, &str) {
    let (secs, frac) = t.as_str().split_once('.').unwrap_or((t.as_str(), ""));
    let secs = secs.trim_start_matches('0');
    (secs.len(), secs, frac)
}

impl Watch {
    /// A recovery begins: its start, its admission and the last read's refusal are forgotten.
    fn begin(&mut self, now: Instant) {
        (self.last_try, self.began, self.admitted, self.read_refused) = (None, now, None, None);
    }

    fn oldest(&self) -> Option<Ts> {
        self.records.keys().min_by(|a, b| ts_order(a).cmp(&ts_order(b))).cloned()
    }

    /// A put of a record not held and older than the floor is not added: it is an edit below what the daemon tracks (its
    /// model refuses it the same way), and adding it would leave a hole. Past `KEPT` records the oldest go, as the
    /// daemon's do, and the floor rises.
    fn apply(&mut self, c: &ChatChangedEvt) {
        for r in &c.put {
            let below = !self.records.contains_key(&r.ts) && self.floor.as_ref().is_some_and(|f| ts_order(&r.ts) < ts_order(f));
            if !below {
                self.records.insert(r.ts.clone(), r.clone());
            }
        }
        c.deleted.iter().for_each(|ts| drop(self.records.remove(ts)));
        while self.records.len() > KEPT {
            self.oldest().iter().for_each(|k| drop(self.records.remove(k)));
            self.floor = self.oldest();
        }
    }
}

#[derive(Default)]
pub(super) struct ChatOwner {
    connected: HashSet<HostKey>,
    /// Hosts whose daemon answered a chat op as unknown; cleared per host when its connection comes back.
    unsupported: HashSet<HostKey>,
    watch: Option<Watch>,
    posting: Option<(HostKey, ChatPostReq)>,
    shown: Option<PostShown>,
}

impl ChatOwner {
    /// Watch `target` on `host`, replacing any earlier watch; the same host and target again is nothing.
    pub(super) fn watch(&mut self, host: HostKey, target: ChatTarget, now: Instant) -> Vec<Out> {
        if self.watch.as_ref().is_some_and(|w| w.host == host && w.target == target) {
            return Vec::new();
        }
        // The daemon's watch of a channel replaces the connection's earlier one, so the same channel needs no unwatch.
        let same = |w: &Watch| w.host == host && w.target.workspace == target.workspace && w.target.channel == target.channel;
        let mut out: Vec<Out> = self.watch.take().filter(|w| !same(w)).map(|w| (w.host, OutgoingReq::ChatUnwatch { target: w.target })).into_iter().collect();
        self.watch = Some(Watch { host, target, registered: false, fill: Fill::Needed, reading: None, stale: false, last_try: None, began: now, admitted: None, read_refused: None, records: HashMap::new(), floor: None, overlay: Vec::new() });
        out.extend(self.drive(now));
        out
    }

    /// End the watch at once, with one unwatch; its read in flight is given up (the drawer reads `reading` first).
    pub(super) fn stop(&mut self) -> Vec<Out> {
        self.watch.take().map(|w| (w.host, OutgoingReq::ChatUnwatch { target: w.target })).into_iter().collect()
    }

    /// The one snapshot read in flight, with its host.
    pub(super) fn reading(&self) -> Option<(HostKey, ChatHistoryReq)> {
        self.watch.as_ref().and_then(|w| w.reading.clone().map(|r| (w.host.clone(), r)))
    }

    pub(super) fn fill(&self) -> Option<Fill> {
        self.watch.as_ref().map(|w| w.fill)
    }

    /// Send a post, only when none is waiting for its reply. One to a host that is not connected is never written, and
    /// reads as refused.
    pub(super) fn post(&mut self, host: HostKey, req: ChatPostReq) -> Option<Out> {
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

    /// A link came or went. Either way the watch starts over: a coming link clears the unsupported mark and begins a recovery,
    /// a going link leaves the post unknown.
    pub(super) fn on_connection(&mut self, host: &HostKey, up: bool, now: Instant) -> Vec<Out> {
        if up {
            self.connected.insert(host.clone());
            self.unsupported.remove(host);
        } else {
            self.connected.remove(host);
            if self.posting.take_if(|(h, _)| h == host).is_some() {
                self.shown = Some(PostShown::Unknown);
            }
        }
        if let Some(w) = self.of(host) {
            (w.registered, w.fill, w.reading, w.stale) = (false, Fill::Needed, None, false);
            w.begin(now);
            w.overlay.clear();
        }
        self.drive(now)
    }

    /// The event bus dropped events on `host`'s link; `chat.changed` is the chat's bus.
    pub(super) fn on_bus_lost(&mut self, host: &HostKey, bus: &str, now: Instant) -> Vec<Out> {
        if bus == sot_protocol::op::CHAT_CHANGED && self.of(host).is_some() { self.resync(now) } else { Vec::new() }
    }

    /// The paced retry of what a loss owes.
    pub(super) fn tick(&mut self, now: Instant) -> Vec<Out> {
        self.drive(now)
    }

    pub(super) fn on_evt(&mut self, host: &HostKey, evt: ChatEvt, now: Instant) -> Vec<Out> {
        let mine = self.of(host).is_some();
        match evt {
            // The drawer (`ChatDrawer`) answers a resolve before the owner sees it.
            ChatEvt::Resolved { .. } => Vec::new(),
            ChatEvt::Watched { target, reply } => self.on_watched(host, &target, reply, now),
            ChatEvt::History { req, reply } => self.on_history(host, &req, reply, now),
            ChatEvt::Posted { req, reply } => {
                self.on_posted(host, &req, reply);
                Vec::new()
            }
            ChatEvt::Changed(c) => {
                if let Some(w) = self.watch.as_mut().filter(|w| mine && w.target.workspace == c.workspace && w.target.channel == c.channel) {
                    w.apply(&c);
                    if w.reading.is_some() {
                        w.overlay.push(c);
                    }
                }
                Vec::new()
            }
            ChatEvt::Status(s) if mine && self.status_lost(&s) => self.resync(now),
            ChatEvt::Unreadable if mine => self.resync(now),
            ChatEvt::Status(_) | ChatEvt::Unreadable => Vec::new(),
        }
    }

    /// The watch, if it is on `host`'s link.
    fn of(&mut self, host: &HostKey) -> Option<&mut Watch> {
        self.watch.as_mut().filter(|w| &w.host == host)
    }

    fn mark_unsupported<T>(&mut self, host: &HostKey, reply: &ChatReply<T>) {
        if matches!(reply, ChatReply::Unsupported) {
            self.unsupported.insert(host.clone());
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
            // A loss during a recovery keeps its start and pace; one from `Complete` begins it.
            if w.fill == Fill::Complete {
                w.begin(now);
            }
            if !matches!(w.fill, Fill::Gap(_)) {
                w.fill = Fill::Needed;
            }
        }
        self.drive(now)
    }

    /// Send what the watched state is owed: its snapshot read once the daemon's watch is complete, else the watch itself,
    /// which is paced and, past the daemon's slowest snapshot, a gap. Nothing while a read is in flight or the state is whole.
    fn drive(&mut self, now: Instant) -> Vec<Out> {
        let Some(w) = self.watch.as_mut() else { return Vec::new() };
        if w.reading.is_some() || !self.connected.contains(&w.host) || self.unsupported.contains(&w.host) {
            return Vec::new();
        }
        match w.fill {
            Fill::Reading | Fill::Complete => return Vec::new(),
            Fill::Gap(Some(code)) if dead(code) => return Vec::new(),
            Fill::Gap(_) => {
                if w.last_try.is_some_and(|t| now.duration_since(t) < GAP_PACE) {
                    return Vec::new();
                }
                w.last_try = Some(now);
                return vec![(w.host.clone(), OutgoingReq::ChatWatch { target: w.target.clone() })];
            }
            Fill::Needed => {}
        }
        if w.registered {
            let req = ChatHistoryReq { target: w.target.clone(), thread: None, after: None, upper: None, limit: MAX_PAGE, cursor: None };
            (w.fill, w.reading) = (Fill::Reading, Some(req.clone()));
            return vec![(w.host.clone(), OutgoingReq::ChatHistory { req })];
        }
        // A watch sent a window after the admitted one gets its full pace to be answered before the gap is shown.
        let pace = if w.last_try == Some(w.began) { FIRST_PACE } else { PACE };
        if w.last_try.is_none_or(|t| now.duration_since(t) >= pace) {
            if w.last_try.is_some_and(|t| t.duration_since(w.admitted.unwrap_or(w.began)) >= WINDOW) {
                w.fill = Fill::Gap(w.read_refused);
            } else {
                w.last_try = Some(now);
                return vec![(w.host.clone(), OutgoingReq::ChatWatch { target: w.target.clone() })];
            }
        }
        Vec::new()
    }

    /// Only a watch reply that says `complete` starts a read, and only while the state needs one or is a gap. A watch
    /// still pending is sent again by `tick`; a refused one is a gap.
    fn on_watched(&mut self, host: &HostKey, target: &ChatTarget, reply: ChatReply<ChatWatchRes>, now: Instant) -> Vec<Out> {
        self.mark_unsupported(host, &reply);
        // A reply that is not `complete`, arriving during a read, outdates it: the daemon began a snapshot the read may not hold.
        if !matches!(reply, ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete })) && self.of(host).is_some_and(|w| &w.target == target && w.fill == Fill::Reading) {
            return self.resync(now);
        }
        let Some(w) = self.of(host).filter(|w| &w.target == target && matches!(w.fill, Fill::Needed | Fill::Gap(_))) else { return Vec::new() };
        // Any answer but a starting service's means the daemon admitted the watch.
        if !matches!(reply, ChatReply::Refused(ChatCode::Unavailable)) {
            w.admitted = w.admitted.or(w.last_try);
        }
        match reply {
            ChatReply::Done(ChatWatchRes { state: ChatWatchState::Complete }) => (w.registered, w.fill) = (true, Fill::Needed),
            // A starting service refuses `unavailable`; it is asked on the pace until the window is spent.
            ChatReply::Refused(ChatCode::Unavailable) if now.duration_since(w.began) < WINDOW => {}
            ChatReply::Refused(code) => w.fill = Fill::Gap(Some(code)),
            // A pending snapshot is no longer the refusal the gap gave, nor the last read's.
            ChatReply::Done(_) => (w.fill, w.read_refused) = (if matches!(w.fill, Fill::Gap(_)) { Fill::Gap(None) } else { w.fill }, None),
            ChatReply::Unsupported => {}
        }
        self.drive(now)
    }

    /// Applies the reply to the read in flight, and no other: a second reply to one read finds none, and the reply of a
    /// read a loss outdated replaces nothing.
    fn on_history(&mut self, host: &HostKey, req: &ChatHistoryReq, reply: ChatReply<ChatHistoryRes>, now: Instant) -> Vec<Out> {
        self.mark_unsupported(host, &reply);
        let Some(w) = self.of(host).filter(|w| w.reading.as_ref() == Some(req)) else { return Vec::new() };
        w.reading = None;
        w.fill = Fill::Needed;
        match reply {
            ChatReply::Done(res) if !w.stale => {
                // The snapshot replaces what was held: a record it no longer holds was deleted during the loss. Its
                // `next` cursor is the older history, which the drawer pages.
                // Only the channel's top-level messages: the daemon tracks no thread reply, so it would never send its edit or delete.
                w.records = res.records.into_iter().filter(|r| r.channel == w.target.channel.as_str() && r.thread_ts == r.ts).map(|r| (r.ts.clone(), r)).collect();
                w.floor = if res.next.is_some() { w.oldest() } else { None };
                std::mem::take(&mut w.overlay).iter().for_each(|c| w.apply(c));
                w.fill = Fill::Complete;
            }
            ChatReply::Done(_) => {}
            // A refused or unsupported read goes back to the watch, on the pace.
            ChatReply::Refused(code) => (w.registered, w.read_refused) = (false, Some(code)),
            ChatReply::Unsupported => w.registered = false,
        }
        (w.stale, w.overlay) = (false, Vec::new());
        self.drive(now)
    }

    fn on_posted(&mut self, host: &HostKey, req: &ChatPostReq, reply: ChatReply<ChatPostRes>) {
        if self.posting.take_if(|(h, r)| h == host && r == req).is_none() {
            return;
        }
        self.mark_unsupported(host, &reply);
        self.shown = Some(match reply {
            ChatReply::Done(ChatPostRes::Posted { ts }) => PostShown::Posted(ts),
            ChatReply::Done(ChatPostRes::Refused { code }) => PostShown::Refused(code),
            ChatReply::Done(ChatPostRes::Limited { retry_after_secs, .. }) => PostShown::Limited(retry_after_secs),
            ChatReply::Unsupported => PostShown::Refused(ChatCode::Unavailable),
            ChatReply::Done(ChatPostRes::Unknown) | ChatReply::Refused(_) => PostShown::Unknown,
        });
    }
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod tests;
