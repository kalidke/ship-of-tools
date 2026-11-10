//! The one open thread: its own copy of the parent, the channel's view parked, and the replies read whole by the thread's
//! cursor, one read at a time. What it returns is sent through the drawer's reads-in-flight set.

use std::time::Instant;

use sot_protocol::chat::ChatRecord;
use sot_protocol::ops::{ChatCode, ChatHistoryReq, ChatHistoryRes, ChatTarget, MAX_PAGE};

use super::owner::{ts_order, Out, LIMITED_PACE, PACE};
use super::row_channel::Stand;
use super::view::ChatView;
use crate::net::dial::HostKey;
use crate::net::transport::{ChatReply, OutgoingReq};

/// The parent and replies a thread holds: the channel keeps the rest of the drawer's 256 records, its newest 100 at least.
const HELD: usize = 156;

pub(super) struct Thread {
    host: HostKey,
    target: ChatTarget,
    parent: ChatRecord,
    /// The parent was deleted when the thread opened.
    deleted: bool,
    /// The channel's view, with its position and mark, until the thread closes.
    pub parked: ChatView,
    replies: Vec<ChatRecord>,
    /// The one read in flight.
    pub reading: Option<ChatHistoryReq>,
    /// The read the hub refused, when and with what: asked again after a pace.
    refused: Option<(Instant, ChatCode, ChatHistoryReq)>,
    /// The hub's daemon answered the read as unknown.
    unsupported: bool,
    /// Replies past `HELD` were left out.
    cut: bool,
}

impl Thread {
    pub fn new(host: HostKey, target: ChatTarget, parent: ChatRecord, deleted: bool, parked: ChatView) -> Self {
        Thread { host, target, parent, deleted, parked, replies: Vec::new(), reading: None, refused: None, unsupported: false, cut: false }
    }

    pub fn host(&self) -> &HostKey {
        &self.host
    }

    /// The walk from its first page, with nothing held.
    pub fn start(&mut self) -> Vec<Out> {
        (self.replies, self.cut, self.refused, self.unsupported) = (Vec::new(), false, None, false);
        self.ask(None)
    }

    fn ask(&mut self, cursor: Option<String>) -> Vec<Out> {
        let thread = Some(self.parent.ts.clone());
        let req = ChatHistoryReq { target: self.target.clone(), thread, after: None, upper: None, limit: MAX_PAGE, cursor };
        self.reading = Some(req.clone());
        vec![(self.host.clone(), OutgoingReq::ChatHistory { req })]
    }

    /// The link ended: no read is in flight. Its return starts the walk again.
    pub fn on_connection(&mut self, host: &HostKey, up: bool) -> Vec<Out> {
        if *host != self.host {
            return Vec::new();
        }
        self.reading = None;
        if up { self.start() } else { Vec::new() }
    }

    /// A reply, taken only while it equals the read in flight (replies are matched by value). Only this parent's replies
    /// join, the first `HELD - 1` by time.
    pub fn on_reply(&mut self, host: &HostKey, req: &ChatHistoryReq, reply: ChatReply<ChatHistoryRes>, now: Instant) -> Vec<Out> {
        if *host != self.host || self.reading.take_if(|r| r == req).is_none() {
            return Vec::new();
        }
        let res = match reply {
            ChatReply::Done(res) => res,
            ChatReply::Refused(code) => return self.refuse(Some((now, code, req.clone()))),
            ChatReply::Unsupported => return self.refuse(None),
        };
        let (channel, parent) = (self.target.channel.as_str(), &self.parent.ts);
        self.replies.extend(res.records.iter().filter(|r| r.channel == channel && r.thread_ts == *parent && r.ts != *parent).cloned());
        self.replies.sort_by(|a, b| ts_order(&a.ts).cmp(&ts_order(&b.ts)));
        self.replies.dedup_by(|a, b| a.ts == b.ts);
        let over = self.replies.len() >= HELD;
        self.replies.truncate(HELD - 1);
        self.cut |= over || self.replies.len() == HELD - 1 && res.next.is_some();
        match res.next {
            Some(c) if !self.cut => self.ask(Some(c)),
            _ => Vec::new(),
        }
    }

    /// The hub refused the read (`Some`), or its daemon does not know it (`None`): nothing more is asked until the pace.
    fn refuse(&mut self, refused: Option<(Instant, ChatCode, ChatHistoryReq)>) -> Vec<Out> {
        (self.unsupported, self.refused) = (refused.is_none(), refused);
        Vec::new()
    }

    /// A refused read is asked again after the pace, or after the longer one for `limited`.
    pub fn tick(&mut self, now: Instant) -> Vec<Out> {
        let due = |(at, code, _): &(Instant, ChatCode, _)| now.duration_since(*at) >= if *code == ChatCode::Limited { LIMITED_PACE } else { PACE };
        let Some((_, _, req)) = self.refused.take_if(|r| due(r)) else { return Vec::new() };
        self.reading = Some(req.clone());
        vec![(self.host.clone(), OutgoingReq::ChatHistory { req })]
    }

    /// What the title says of the thread beyond its name.
    pub fn words(&self) -> String {
        match (&self.refused, self.unsupported, self.cut) {
            (Some((_, code, _)), ..) => format!("the thread is not read: {}", code.text()),
            (None, true, _) => Stand::NoChat.words(""),
            (None, false, true) => "the rest of this thread is not shown".into(),
            (None, false, false) => String::new(),
        }
    }

    /// The parent (deleted or not as it opened), then the replies by time.
    pub fn records(&self) -> Vec<(&ChatRecord, bool)> {
        std::iter::once((&self.parent, self.deleted)).chain(self.replies.iter().map(|r| (r, false))).collect()
    }

    pub fn len(&self) -> usize {
        1 + self.replies.len()
    }
}
