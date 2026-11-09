//! The chat drawer's model: the owner, the row it follows and the reads in flight. State sends what its methods return.

use std::collections::HashMap;
use std::time::Instant;

use sot_protocol::ops::{ChatHistoryReq, ChatResolveRes, ChatTarget};

use crate::net::dial::HostKey;
use crate::net::transport::{ChatEvt, ChatReply, OutgoingReq};
use crate::ui::ResultRowIdentity;

#[cfg(test)]
use super::owner::Fill;
use super::owner::{ChatOwner, Out};
use super::row_channel::RowChannel;

/// A `chat.history` request in flight. `orphan`: its reader gave it up, so its reply is dropped.
struct Read {
    req: ChatHistoryReq,
    orphan: bool,
}

/// Every read in flight, per host, so two equal reads are never in flight together (replies are matched by value and
/// may come back in either order). A read equal to one in flight waits in `held`.
#[derive(Default)]
struct Flight {
    reads: HashMap<HostKey, Vec<Read>>,
    held: Vec<Out>,
}

impl Flight {
    /// What may go out now; a read equal to one in flight is held, and a read that goes is in flight.
    fn send(&mut self, outs: Vec<Out>) -> Vec<Out> {
        let mut go = Vec::new();
        for out in outs {
            let (host, OutgoingReq::ChatHistory { req }) = &out else { go.push(out); continue };
            let reads = self.reads.entry(host.clone()).or_default();
            if reads.iter().any(|r| &r.req == req) {
                self.held.push(out);
            } else {
                reads.push(Read { req: req.clone(), orphan: false });
                go.push(out);
            }
        }
        go
    }

    /// A reply came: its one equal entry leaves; whether that entry was an orphan, and so the reply is dropped.
    fn answered(&mut self, host: &HostKey, req: &ChatHistoryReq) -> bool {
        let Some(reads) = self.reads.get_mut(host) else { return false };
        reads.iter().position(|r| &r.req == req).is_some_and(|i| reads.remove(i).orphan)
    }

    /// A reader gave the read up: a read still held is cancelled, one in flight stays as an orphan.
    fn abandon(&mut self, host: &HostKey, req: &ChatHistoryReq) {
        let held = |o: &Out| matches!(o, (h, OutgoingReq::ChatHistory { req: r }) if h == host && r == req);
        if let Some(i) = self.held.iter().position(held) {
            self.held.remove(i);
        } else if let Some(r) = self.reads.get_mut(host).and_then(|v| v.iter_mut().find(|r| &r.req == req)) {
            r.orphan = true;
        }
    }

    /// A link changed: nothing of it is in flight, and no reply will come.
    fn clear(&mut self, host: &HostKey) {
        self.reads.remove(host);
        self.held.retain(|(h, _)| h != host);
    }
}

#[derive(Default)]
pub(in crate::ui) struct ChatDrawer {
    owner: ChatOwner,
    rows: RowChannel,
    flight: Flight,
}

impl ChatDrawer {
    /// The frame: follow the wanted row (see `RowChannel`) and pace the owner. The flag asks for another frame.
    pub(super) fn tick(&mut self, wanted: Option<ResultRowIdentity>, now: Instant) -> (Vec<Out>, bool) {
        let step = self.rows.tick(wanted, now);
        let mut outs = if step.changed { self.owner_outs(ChatOwner::stop) } else { Vec::new() };
        outs.extend(step.resolve);
        outs.extend(self.owner_outs(|o| o.tick(now)));
        (outs, step.settling)
    }

    pub(super) fn on_connection(&mut self, host: &HostKey, up: bool, now: Instant) -> Vec<Out> {
        self.rows.on_connection(host, up);
        self.flight.clear(host);
        self.owner_outs(|o| o.on_connection(host, up, now))
    }

    pub(super) fn on_bus_lost(&mut self, host: &HostKey, bus: &str, now: Instant) -> Vec<Out> {
        self.owner_outs(|o| o.on_bus_lost(host, bus, now))
    }

    pub(super) fn on_evt(&mut self, host: &HostKey, evt: ChatEvt, now: Instant) -> Vec<Out> {
        match evt {
            // The wanted row's answer decides its watch: `mapped` watches, anything else (unmapped, invalid, refused, an
            // old daemon) ends it. An answer for a row no longer wanted is nothing.
            ChatEvt::Resolved { row, reply } if self.rows.wants(host, &row) => match reply {
                ChatReply::Done(ChatResolveRes::Mapped { workspace, channel, .. }) => {
                    let target = ChatTarget { row, workspace, channel };
                    self.owner_outs(|o| o.watch(host.clone(), target, now))
                }
                _ => self.owner_outs(ChatOwner::stop),
            },
            ChatEvt::Resolved { .. } => Vec::new(),
            ChatEvt::History { req, reply } => {
                let orphan = self.flight.answered(host, &req);
                let mut outs = if orphan { Vec::new() } else { self.owner_outs(|o| o.on_evt(host, ChatEvt::History { req, reply }, now)) };
                let held = std::mem::take(&mut self.flight.held);
                outs.extend(self.flight.send(held));
                outs
            }
            evt => self.owner_outs(|o| o.on_evt(host, evt, now)),
        }
    }

    #[cfg(test)]
    pub(super) fn fill(&self) -> Option<Fill> {
        self.owner.fill()
    }

    /// Runs `f` on the owner. A read the owner no longer holds and that was not answered (its watch stopped or was
    /// replaced) is given up before the outs are sent, so a read `f` itself asks for is never cancelled by the same call.
    fn owner_outs(&mut self, f: impl FnOnce(&mut ChatOwner) -> Vec<Out>) -> Vec<Out> {
        let before = self.owner.reading();
        let outs = f(&mut self.owner);
        if let Some((host, req)) = before.filter(|b| Some(b) != self.owner.reading().as_ref()) {
            self.flight.abandon(&host, &req);
        }
        self.flight.send(outs)
    }
}
