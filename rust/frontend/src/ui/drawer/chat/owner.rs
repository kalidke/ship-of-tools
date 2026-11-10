//! The project chat's private owner: one watched channel's state, the one post in flight, and the reads that keep the
//! watched state whole. It consumes fe-net's chat events and replies and returns the requests to send; it sends nothing
//! itself and replays no mutation. Only `ChatDrawer` calls it; the view reads its records.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::net::dial::HostKey;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use sot_protocol::ops::{
    ChatChangedEvt, ChatCode, ChatHistoryReq, ChatHistoryRes, ChatPostReq, ChatPostRes, ChatService, ChatStatusEvt, ChatTarget, ChatWatchRes,
    ChatWatchState, ChatUpper, MAX_PAGE,
};
use sot_protocol::chat::{ChatRecord, Ts};

/// Between two watch attempts: the first waits `FIRST_PACE`, the second `SECOND_PACE` (so the third goes at one second from
/// the start), the rest `PACE`. A snapshot is usually held within a few hundred milliseconds, so a first show asks soon.
const FIRST_PACE: Duration = Duration::from_millis(250);
const SECOND_PACE: Duration = Duration::from_millis(750);
pub(super) const PACE: Duration = Duration::from_secs(5);
/// What the daemon is given before a gap is shown: for its chat service to start (it refuses `unavailable` meanwhile), and,
/// from the first watch it admits, for its slowest snapshot (about 18.5 s at worst, so the last watch goes at 21 s).
const WINDOW: Duration = Duration::from_secs(20);
/// Between two watches in `Gap`, which has no cap: each also starts the daemon's own retry of its snapshot. It also paces
/// the window's re-asking of a refused resolve (`RowChannel`).
pub(super) const GAP_PACE: Duration = Duration::from_secs(60);
/// The drawer's bound: the records the owner keeps above its floor. The daemon's model tracks only its newest 100
/// (sot-chat's `watch::MAX_KEPT`); an older record is as read, and no edit or delete of it comes.
const KEPT: usize = 256;
/// The most a parked reader holds the store to: `KEPT` and one page. Past it the oldest go whoever reads them.
const CEILING: usize = KEPT + MAX_PAGE as usize;
/// Between two asks of a page that was refused as rate limited.
pub(super) const LIMITED_PACE: Duration = Duration::from_secs(30);
/// The stopped watches whose records the owner keeps: two, so a person can pass between two channels and back.
const PARKED: usize = 2;

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

/// What the last post came to. `Unknown` means the post may exist: a post the owner never wrote is `Refused`,
/// and one fe-net never wrote (the connection's daemon does not post plain text) is `Outdated`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PostShown {
    Posted(Ts),
    Refused(ChatCode),
    /// The wait in seconds, and whether it is a guess.
    Limited(u64, bool),
    Unknown,
    /// Refused before it was written: the connection's daemon does not post plain text (T20).
    Outdated,
}

/// Why the older pages stopped.
#[derive(Clone, Copy, PartialEq)]
enum Stop {
    /// Slack's plan hides the history below the last page.
    Hides,
    /// A page or snapshot came back empty with older history said to remain: nothing to move the floor to, so paging stops
    /// until a replacing snapshot or a row change.
    Stuck,
    /// The last page read was refused, when and with what: asked again after a pace unless the refusal is for good.
    Refused(Instant, ChatCode),
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
    /// Each held record with whether the daemon told this window it was deleted: then only its identity, author, time and
    /// origin are kept. A put of the same time brings it back; a snapshot replaces the records from its oldest up, which hold only what exists, so
    /// only the deleted ones below an overlapping snapshot remain.
    records: HashMap<Ts, (ChatRecord, bool)>,
    /// The oldest timestamp the daemon still tracks: the snapshot's oldest when older history exists, raised by the trim.
    floor: Option<Ts>,
    /// The record the reader is anchored on (the view's, set each frame): the bound's trim never takes it.
    anchor: Option<Ts>,
    /// The newest record held when the state was last whole (`Complete`), kept while a snapshot is owed (a recovery that begins
    /// again, a link flap or a park and take-up, keeps it) and none if the state never was: a snapshot reaches back to what
    /// was held when it holds a record that old. Records held since, `chat.changed` puts of a recovery, are not history.
    reach: Option<Ts>,
    /// The oldest held record is in the view, as the view last placed it: older history is wanted.
    reaches: bool,
    /// What the open thread holds of the drawer's `KEPT` records: the channel keeps the rest.
    reserved: usize,
    /// The one page read in flight, apart from the snapshot's `reading`: it never delays a watch.
    paging: Option<ChatHistoryReq>,
    /// Why paging stopped, if it did.
    stop: Option<Stop>,
    /// The `chat.changed` events that came while the read was in flight: they are newer than its snapshot and win over it.
    overlay: Vec<ChatChangedEvt>,
}

/// Refusals that a later ask cannot clear: the mapping is read at daemon start, and a listing change follows a stale row.
pub(super) fn dead(code: ChatCode) -> bool {
    matches!(code, ChatCode::StaleRow | ChatCode::StaleMapping | ChatCode::Unmapped)
}

/// A timestamp's order: seconds by length then digits, then the fraction.
pub(super) fn ts_order(t: &Ts) -> (usize, &str, &str) {
    let (secs, frac) = t.as_str().split_once('.').unwrap_or((t.as_str(), ""));
    let secs = secs.trim_start_matches('0');
    (secs.len(), secs, frac)
}

impl Watch {
    /// A recovery begins: its start, its admission and the last read's refusal are forgotten.
    fn begin(&mut self, now: Instant) {
        (self.last_try, self.began, self.admitted, self.read_refused) = (None, now, None, None);
    }

    /// A loss ends a whole state: what it held is the history a snapshot may reach back to.
    fn note_reach(&mut self) {
        if self.fill == Fill::Complete {
            self.reach = self.newest();
        }
    }

    fn newest(&self) -> Option<Ts> {
        self.records.keys().max_by(|a, b| ts_order(a).cmp(&ts_order(b))).cloned()
    }

    /// The wait after the last watch sent: `FIRST_PACE` after the one at the start, `SECOND_PACE` after one within the first
    /// second, else `PACE`.
    fn pace(&self) -> Duration {
        match self.last_try.map(|t| t.duration_since(self.began)) {
            Some(Duration::ZERO) => FIRST_PACE,
            Some(since) if since < FIRST_PACE + SECOND_PACE => SECOND_PACE,
            _ => PACE,
        }
    }

    fn oldest(&self) -> Option<Ts> {
        self.records.keys().min_by(|a, b| ts_order(a).cmp(&ts_order(b))).cloned()
    }

    /// The page below the floor, asked for while the reader is at the oldest held record, older history exists and a page
    /// fits. A refused page waits out its pace first, and one refused for good is not asked again until a row or link change.
    fn older(&mut self, now: Instant) -> Option<ChatHistoryReq> {
        let due = match self.stop {
            None => true,
            Some(Stop::Refused(at, code)) => !dead(code) && now.duration_since(at) >= if code == ChatCode::Limited { LIMITED_PACE } else { PACE },
            Some(Stop::Hides | Stop::Stuck) => false,
        };
        let floor = self.floor.clone().filter(|_| self.reaches && self.paging.is_none() && due && self.records.len() < self.cap())?;
        let req = ChatHistoryReq { target: self.target.clone(), thread: None, after: None, upper: Some(ChatUpper::Before(floor)), limit: MAX_PAGE, cursor: None };
        self.paging = Some(req.clone());
        Some(req)
    }

    /// A put of a record not held and older than the floor is not added: it is an edit below what the daemon tracks (its
    /// model refuses it the same way), and adding it would leave a hole.
    fn apply(&mut self, c: &ChatChangedEvt) {
        for r in &c.put {
            let below = !self.records.contains_key(&r.ts) && self.floor.as_ref().is_some_and(|f| ts_order(&r.ts) < ts_order(f));
            if !below {
                self.records.insert(r.ts.clone(), (r.clone(), false));
            }
        }
        // A delete leaves the record's place, author and time with no text; it counts against `KEPT` like any record.
        for ts in &c.deleted {
            if let Some(e) = self.records.get_mut(ts) {
                let r = &e.0;
                let gone = ChatRecord { workspace: r.workspace.clone(), channel: r.channel.clone(), ts: r.ts.clone(), thread_ts: r.thread_ts.clone(), author: r.author.clone(), origin: r.origin, text: String::new(), truncated: false, reply_count: 0, files: Vec::new(), latest_reply: None, limited: false, omitted: false, files_omitted: 0 };
                *e = (gone, true);
            }
        }
        self.bound();
    }

    /// The channel's share of `KEPT`.
    fn cap(&self) -> usize {
        KEPT - self.reserved
    }

    /// The store's bound, after every change to the records: past `KEPT` the oldest go in one pass and the floor rises, but
    /// never the record the reader is anchored on nor a newer one, so a reader on the oldest does not move up a record per
    /// arrival. A reader parked there holds the store to `CEILING`; past that the oldest go anyway and the reader moves by
    /// the view's rule for an anchor no longer held.
    fn bound(&mut self) {
        let over = self.records.len().saturating_sub(self.cap());
        if over == 0 {
            return;
        }
        let mut held: Vec<Ts> = self.records.keys().cloned().collect();
        held.sort_by(|a, b| ts_order(a).cmp(&ts_order(b)));
        let take = match &self.anchor {
            Some(a) if self.records.len() <= CEILING => over.min(held.partition_point(|k| ts_order(k) < ts_order(a))),
            _ => over,
        };
        held[..take].iter().for_each(|k| drop(self.records.remove(k)));
        if take > 0 {
            self.floor = Some(held[take].clone());
        }
    }
}

#[derive(Default)]
pub(super) struct ChatOwner {
    connected: HashSet<HostKey>,
    /// Hosts whose daemon answered a chat op as unknown; cleared per host when its connection comes back.
    unsupported: HashSet<HostKey>,
    watch: Option<Watch>,
    /// The watches `stop` ended, newest first, each within `KEPT`: a watch of the same host and channel takes up its
    /// records and floor, shown while its snapshot is read again, as after a link's return.
    parked: Vec<Watch>,
    posting: Option<(HostKey, ChatPostReq)>,
    /// The outcome taken and not yet reported to the drawer (`take_report`).
    report: Option<PostShown>,
    /// When a `limited` post may be sent again, and whether that wait is a guess.
    wait_until: Option<(Instant, bool)>,
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
        // What the stopped watch knew of the channel stays true: its records, floor and reach, and that Slack's plan hides what is older.
        let (records, floor, reach, stop) = self.parked.iter().position(same).map(|i| self.parked.remove(i)).map_or_else(Default::default, |p| (p.records, p.floor, p.reach, p.stop.filter(|s| *s == Stop::Hides)));
        self.watch = Some(Watch { host, target, registered: false, fill: Fill::Needed, reading: None, stale: false, last_try: None, began: now, admitted: None, read_refused: None, records, floor, reach, anchor: None, reaches: false, reserved: 0, paging: None, stop, overlay: Vec::new() });
        out.extend(self.drive(now));
        out
    }

    /// End the watch at once, with one unwatch; its read in flight is given up (the drawer reads `reading` first), and it is
    /// parked, trimmed to `KEPT`, when it holds records.
    pub(super) fn stop(&mut self) -> Vec<Out> {
        let Some(mut w) = self.watch.take() else { return Vec::new() };
        let out = vec![(w.host.clone(), OutgoingReq::ChatUnwatch { target: w.target.clone() })];
        // A channel passed through before its read holds nothing, and would push out one that does.
        if !w.records.is_empty() {
            w.note_reach();
            w.anchor = None;
            w.bound();
            self.parked.insert(0, w);
            self.parked.truncate(PARKED);
        }
        out
    }

    /// The one snapshot read in flight, with its host.
    pub(super) fn reading(&self) -> Option<(HostKey, ChatHistoryReq)> {
        self.watch.as_ref().and_then(|w| w.reading.clone().map(|r| (w.host.clone(), r)))
    }

    /// The reader's anchor, as the view last placed it.
    pub(super) fn anchored(&mut self, anchor: Option<Ts>) {
        if let Some(w) = self.watch.as_mut() {
            w.anchor = anchor;
        }
    }

    #[cfg(test)]
    pub(super) fn anchor_held(&self) -> Option<Ts> {
        self.watch.as_ref().and_then(|w| w.anchor.clone())
    }

    /// Whether the oldest held record is in the view, as the view last placed it.
    pub(super) fn reaches(&mut self, yes: bool) {
        if let Some(w) = self.watch.as_mut() {
            w.reaches = yes;
        }
    }

    /// The open thread holds `held` of the `KEPT` records (none once closed): the channel's oldest give way to it.
    pub(super) fn reserve(&mut self, held: usize) {
        if let Some(w) = self.watch.as_mut() {
            w.reserved = held;
            w.bound();
        }
    }

    /// The page read in flight, with its host.
    pub(super) fn paging(&self) -> Option<(HostKey, ChatHistoryReq)> {
        self.watch.as_ref().and_then(|w| w.paging.clone().map(|r| (w.host.clone(), r)))
    }

    /// What the title says of the older history: Slack's plan hides it, a page was refused, or the store is full.
    pub(super) fn older_words(&self) -> Option<String> {
        let w = self.watch.as_ref()?;
        match w.stop {
            Some(Stop::Hides) => Some("Slack's plan hides older messages".into()),
            Some(Stop::Stuck) => Some("older messages could not be read".into()),
            Some(Stop::Refused(_, code)) if w.floor.is_some() => Some(format!("older messages are not read: {}", code.text())),
            _ if w.floor.is_some() && w.records.len() >= KEPT => Some("older messages past the drawer's bound are not shown".into()),
            _ => None,
        }
    }

    /// The held records with whether each was deleted, oldest first.
    pub(super) fn records(&self) -> Vec<(&ChatRecord, bool)> {
        let mut held: Vec<_> = self.watch.iter().flat_map(|w| w.records.values().map(|(r, d)| (r, *d))).collect();
        held.sort_by(|a, b| ts_order(&a.0.ts).cmp(&ts_order(&b.0.ts)));
        held
    }

    /// The channel is known to be empty: the snapshot was read whole and holds no record. Not while it is read, offline, a gap
    /// or unsupported.
    pub(super) fn no_messages(&self) -> bool {
        self.fill() == Some(Fill::Complete) && self.watch.as_ref().is_some_and(|w| w.records.is_empty() && w.stop != Some(Stop::Stuck))
    }

    /// The watched channel's id.
    pub(super) fn channel(&self) -> Option<&str> {
        self.target().map(|t| t.channel.as_str())
    }

    pub(super) fn target(&self) -> Option<&ChatTarget> {
        self.watch.as_ref().map(|w| &w.target)
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
            self.took(PostShown::Refused(ChatCode::Unavailable));
            return None;
        }
        self.posting = Some((host.clone(), req.clone()));
        Some((host, OutgoingReq::ChatPost { req }))
    }

    /// A post's outcome is taken: the drawer is owed its words, also when it equals the one before.
    fn took(&mut self, outcome: PostShown) {
        self.report = Some(outcome);
    }

    /// The outcome taken since the last ask, once.
    pub(super) fn take_report(&mut self) -> Option<PostShown> {
        self.report.take()
    }

    /// The post waiting for its reply: its host and target row.
    pub(super) fn posting_row(&self) -> Option<(&HostKey, &str)> {
        self.posting.as_ref().map(|(h, r)| (h, r.target.row.as_str()))
    }

    /// The seconds left of a `limited` wait (rounded up, never zero), and whether it is a guess.
    pub(super) fn waiting(&self, now: Instant) -> Option<(u64, bool)> {
        let (until, assumed) = self.wait_until?;
        let left = until.checked_duration_since(now).filter(|d| !d.is_zero())?;
        Some((left.as_secs() + u64::from(left.subsec_nanos() > 0), assumed))
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
                self.took(PostShown::Unknown);
            }
        }
        if let Some(w) = self.of(host) {
            w.note_reach();
            (w.registered, w.fill, w.reading, w.stale, w.paging) = (false, Fill::Needed, None, false, None);
            w.begin(now);
            // A link change does not change what Slack's plan hides.
            w.stop = w.stop.filter(|s| *s == Stop::Hides);
            w.overlay.clear();
        }
        self.drive(now)
    }

    /// The event bus dropped events on `host`'s link; `chat.changed` is the chat's bus.
    pub(super) fn on_bus_lost(&mut self, host: &HostKey, bus: &str, now: Instant) -> Vec<Out> {
        if bus == sot_protocol::op::CHAT_CHANGED && self.of(host).is_some() { self.resync(now) } else { Vec::new() }
    }

    /// When the watch waiting on one of its two short paces is next asked, so the window can draw a frame at that moment;
    /// `None` when none waits, or the ask would not go (the daemon is unsupported, a read is in flight).
    pub(super) fn next_ask(&self) -> Option<Instant> {
        let w = self.watch.as_ref().filter(|w| w.fill == Fill::Needed && w.reading.is_none() && !self.unsupported.contains(&w.host))?;
        w.last_try.filter(|_| w.pace() < PACE).map(|t| t + w.pace())
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
                self.on_posted(host, &req, reply, now);
                Vec::new()
            }
            ChatEvt::PostNotSent { req } => {
                if self.posting.take_if(|(h, r)| h == host && *r == req).is_some() {
                    self.took(PostShown::Outdated);
                }
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
                w.note_reach();
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
            Fill::Reading => return Vec::new(),
            Fill::Complete => return w.older(now).map(|req| (w.host.clone(), OutgoingReq::ChatHistory { req })).into_iter().collect(),
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
        if w.last_try.is_none_or(|t| now.duration_since(t) >= w.pace()) {
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
        // A refusal takes back what the daemon had registered.
        if matches!(reply, ChatReply::Refused(_)) {
            w.registered = false;
        }
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
        if self.of(host).is_some_and(|w| w.paging.as_ref() == Some(req)) {
            return self.on_page(host, req, reply, now);
        }
        let Some(w) = self.of(host).filter(|w| w.reading.as_ref() == Some(req)) else { return Vec::new() };
        w.reading = None;
        w.fill = Fill::Needed;
        match reply {
            ChatReply::Done(res) if !w.stale => {
                // The snapshot replaces what was held: a record it no longer holds was deleted during the loss. Its
                // `next` cursor is the older history, which the drawer pages.
                // Only the channel's top-level messages: the daemon tracks no thread reply, so it would never send its edit or delete.
                let raw = res.records.iter().filter(|r| r.channel == w.target.channel.as_str()).map(|r| r.ts.clone()).min_by(|a, b| ts_order(a).cmp(&ts_order(b)));
                let fresh: HashMap<_, _> = res.records.into_iter().filter(|r| r.channel == w.target.channel.as_str() && r.thread_ts == r.ts).map(|r| (r.ts.clone(), (r, false))).collect();
                // Without `next` it is the whole channel. With `next` that reaches back to what was held, the older held
                // records stay (tombstones too) and the floor with them; otherwise they go and its oldest is the floor.
                let empty_more = fresh.is_empty() && res.next.is_some();
                // A replacing snapshot ends a stuck page.
                if w.stop == Some(Stop::Stuck) {
                    w.stop = None;
                }
                let first = fresh.keys().min_by(|a, b| ts_order(a).cmp(&ts_order(b))).cloned();
                let newest = w.reach.take();
                let overlaps = first.as_ref().zip(newest.as_ref()).is_some_and(|(f, n)| ts_order(f) <= ts_order(n));
                if let Some(first) = first.filter(|_| res.next.is_some() && overlaps) {
                    w.records.retain(|t, _| ts_order(t) < ts_order(&first));
                    w.records.extend(fresh);
                    // A snapshot reaching below the floor lowers it.
                    w.floor = w.floor.take().map(|f| if ts_order(&first) < ts_order(&f) { first.clone() } else { f });
                } else {
                    w.records = fresh;
                    // A snapshot of nothing but thread replies has no top-level record to be the floor: the oldest reply is.
                    w.floor = if res.next.is_some() { w.oldest().or(raw) } else { None };
                    // The page in flight was for the older records: its reply is an orphan.
                    (w.paging, w.stop) = (None, empty_more.then_some(Stop::Stuck));
                }
                w.reaches = false;
                std::mem::take(&mut w.overlay).iter().for_each(|c| w.apply(c));
                w.bound();
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

    /// The page read's reply, taken only while its upper bound is still `before(floor)`: a floor that moved meanwhile (a trim)
    /// would leave a hole. As many of its newest top-level records as fit in `KEPT` join, and the floor falls to the oldest
    /// joined, or to the page's oldest while a `next` remains (Slack's plan hiding the rest ends the paging). An empty page
    /// with a `next` keeps the floor and stops the paging. A refusal is asked again by `older` after its pace.
    fn on_page(&mut self, host: &HostKey, req: &ChatHistoryReq, reply: ChatReply<ChatHistoryRes>, now: Instant) -> Vec<Out> {
        let Some(w) = self.of(host) else { return Vec::new() };
        w.paging = None;
        match reply {
            ChatReply::Done(res) if req.upper == w.floor.clone().map(ChatUpper::Before) => {
                let raw = res.records.iter().map(|r| r.ts.clone()).min_by(|a, b| ts_order(a).cmp(&ts_order(b)));
                let mut top: Vec<ChatRecord> = res.records.into_iter().filter(|r| r.channel == w.target.channel.as_str() && r.thread_ts == r.ts && w.floor.as_ref().is_none_or(|f| ts_order(&r.ts) < ts_order(f))).collect();
                top.sort_by(|a, b| ts_order(&b.ts).cmp(&ts_order(&a.ts)));
                let room = w.cap().saturating_sub(w.records.len());
                let cut = top.len() > room;
                top.truncate(room);
                let joined = top.last().map(|r| r.ts.clone());
                w.records.extend(top.into_iter().map(|r| (r.ts.clone(), (r, false))));
                w.stop = None;
                if cut {
                    w.floor = joined.or(w.floor.take());
                } else {
                    if raw.is_none() && res.next.is_some() {
                        w.stop = Some(Stop::Stuck);
                    }
                    w.floor = if w.stop.is_some() { w.floor.take() } else { raw.filter(|_| res.next.is_some() && !res.limited) };
                    w.stop = if res.limited { Some(Stop::Hides) } else { w.stop };
                }
                // The view's word is of the records before this page: the next frame says whether the reader is at the oldest now.
                w.reaches = false;
            }
            ChatReply::Done(_) => w.reaches = false,
            ChatReply::Refused(code) => w.stop = Some(Stop::Refused(now, code)),
            ChatReply::Unsupported => {}
        }
        self.drive(now)
    }

    /// Takes the outcome of the post waiting for this reply, and returns the row whose post the reply proves was made.
    pub(super) fn on_posted(&mut self, host: &HostKey, req: &ChatPostReq, reply: ChatReply<ChatPostRes>, now: Instant) -> Option<String> {
        self.posting.take_if(|(h, r)| h == host && r == req)?;
        self.mark_unsupported(host, &reply);
        let outcome = match reply {
            ChatReply::Done(ChatPostRes::Posted { ts }) => PostShown::Posted(ts),
            ChatReply::Done(ChatPostRes::Refused { code }) => PostShown::Refused(code),
            ChatReply::Done(ChatPostRes::Limited { retry_after_secs, assumed }) => {
                self.wait_until = Some((now + Duration::from_secs(retry_after_secs), assumed));
                PostShown::Limited(retry_after_secs, assumed)
            }
            ChatReply::Unsupported => PostShown::Refused(ChatCode::Unavailable),
            ChatReply::Done(ChatPostRes::Unknown) | ChatReply::Refused(_) => PostShown::Unknown,
        };
        let made = matches!(outcome, PostShown::Posted(_)).then(|| req.target.row.clone());
        self.took(outcome);
        made
    }
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod tests;
