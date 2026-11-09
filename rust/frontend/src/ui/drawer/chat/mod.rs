//! The chat drawer's State side: forwards chat events, `bus.lost` and each link's change to the private owner
//! (`drawer.rs`, over the private `owner.rs`), and sends the requests it returns.

use std::time::Instant;

use crate::net::dial::HostKey;
use crate::net::transport::ChatEvt;
use super::super::*;

mod drawer;
mod owner;
mod row_channel;
pub(in crate::ui) use drawer::ChatDrawer;
use owner::Out;
#[cfg(test)]
mod row_channel_tests;

impl State {
    /// Sends what the owner asked for; a request that cannot be queued is dropped with a warning (the owner's pace, or
    /// the next connection, ask again).
    fn with_chat(&mut self, f: impl FnOnce(&mut ChatDrawer, Instant) -> Vec<Out>) {
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
        let wanted = wanted_row(self.layout_help.chat_shown(), || self.active_result_workspace());
        let mut settling = false;
        self.with_chat(|c, now| {
            let (outs, wait) = c.tick(wanted, now);
            settling = wait;
            outs
        });
        if settling {
            self.pacer.mark_dirty();
        }
    }
}

/// The row the chat follows: the selected row's host and canonical id (never the slug, which the daemon refuses as
/// stale), and only while the chat is shown. The selected row is not even resolved otherwise.
pub(in crate::ui) fn wanted_row(shown: bool, selected: impl FnOnce() -> Option<ResolvedWorkspace>) -> Option<ResultRowIdentity> {
    shown.then(selected).flatten().map(|row| row.identity().clone())
}
