//! The window's requests waiting for a connection, and the rule that a request which must not run twice never
//! outlives the connection that was current when it was admitted.

use super::*;

use std::collections::VecDeque;

/// The receiving end of a host's request channel, which survives every reconnect, with the replayable requests a
/// retirement set aside. A replayable request (a read, or a write that is safe to repeat) waits here for whichever
/// connection comes next. A request that is not (`OutgoingReq::replayable`) is written on the connection that is
/// current when it is dequeued or on none: [`Outbox::retire`] ends every one still waiting when a connection ends and
/// when the next one is about to send its hello, so none is written on a later one.
pub(crate) struct Outbox {
    rx: UnboundedReceiver<OutgoingReq>,
    held: VecDeque<OutgoingReq>,
}

impl Outbox {
    pub(crate) fn new(rx: UnboundedReceiver<OutgoingReq>) -> Self {
        Self { rx, held: VecDeque::new() }
    }

    /// The next request, in the order it was sent; `None` once the senders are gone and nothing is held. Cancel-safe.
    pub(crate) async fn recv(&mut self) -> Option<OutgoingReq> {
        if let Some(req) = self.held.pop_front() {
            return Some(req);
        }
        self.rx.recv().await
    }

    /// End every waiting request that is not replayable, as a connection's end would end it had it been written: the
    /// same one outcome (`PendingGuard`'s), once, and nothing reaches a daemon. Replayable requests keep their order.
    pub(crate) async fn retire(&mut self, evt_tx: &StdSender<(HostKey, IncomingEvt)>, host: &HostKey) {
        while let Ok(req) = self.rx.try_recv() {
            self.held.push_back(req);
        }
        for req in std::mem::take(&mut self.held) {
            if req.replayable() {
                self.held.push_back(req);
                continue;
            }
            // Encoded into a buffer nobody reads: only the pending entry it records is wanted, and the guard's drop
            // reports it. Recorded even when encoding fails, as the steady loop does.
            let mut entries = HashMap::new();
            let _ = send_request(&mut Vec::new(), &mut entries, 0, req).await;
            drop(PendingGuard { map: entries, evt_tx, host: host.clone() });
        }
    }
}

#[cfg(test)]
#[path = "outbox_tests.rs"]
mod tests;
