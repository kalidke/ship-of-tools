//! The chat drawer's model: the owner and the row it follows. State sends what its methods return.

use std::time::Instant;

use sot_protocol::ops::{ChatResolveRes, ChatTarget};

use crate::net::dial::HostKey;
use crate::net::transport::{ChatEvt, ChatReply};
use crate::ui::ResultRowIdentity;

#[cfg(test)]
use super::owner::Fill;
use super::owner::{ChatOwner, Out};
use super::row_channel::RowChannel;

#[derive(Default)]
pub(in crate::ui) struct ChatDrawer {
    owner: ChatOwner,
    rows: RowChannel,
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
            evt => self.owner_outs(|o| o.on_evt(host, evt, now)),
        }
    }

    #[cfg(test)]
    pub(super) fn fill(&self) -> Option<Fill> {
        self.owner.fill()
    }

    fn owner_outs(&mut self, f: impl FnOnce(&mut ChatOwner) -> Vec<Out>) -> Vec<Out> {
        f(&mut self.owner)
    }
}
