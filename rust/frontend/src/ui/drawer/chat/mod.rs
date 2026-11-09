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

    /// The row the chat follows: the selected row's host and canonical id (never the slug), only while the drawer shows
    /// Chat, or Help opened from Chat.
    fn wanted_chat_row(&self) -> Option<(HostKey, String)> {
        let shown = match self.layout_help.drawer() {
            DrawerContent::Chat => true,
            DrawerContent::Help => self.layout_help.help_opened_from_chat(),
            _ => false,
        };
        let row = self.active_result_workspace().filter(|_| shown)?;
        Some((self.selected.host().clone(), row.identity().workspace_id.clone()))
    }

    pub(in crate::ui) fn tick_chat(&mut self) {
        let wanted = self.wanted_chat_row();
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
