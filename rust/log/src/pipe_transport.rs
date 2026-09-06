//! The ADR 0041 step-5 bridge between [`crate::pipe_win`]'s real named-pipe
//! transport and [`crate::transport`]'s `Transport` trait — the deferred
//! half of unit U3 (round 2). Neither side knows about the other:
//! `pipe_win` is a byte transport with no opinion on what rides over it;
//! `capsule_win::run` drives `AttachProto` against an abstract `Transport`.
//! This module is the thin adapter that makes the real pipe satisfy that
//! trait — nothing here decides protocol behavior, it only moves bytes and
//! translates event/id shapes.
//!
//! # Conn-id spaces are the SAME space, not reconciled by a map
//!
//! `pipe_win::ConnId` and `attach_proto::ConnId` are both bare `u64`
//! aliases, each independently allocated by its own module (`pipe_win`'s
//! accept loop hands out its own sequence; `attach_proto` never allocates
//! one at all — it only ever learns of a `ConnId` through a
//! `TransportEvent::ConnectionOpened` this module produces). Since
//! `PipeServer` already guarantees its own ids are globally unique and
//! stable for the connection's whole life, this bridge reuses THAT id
//! verbatim as the capsule's `ConnId` — no translation table, because
//! none is needed: "stable and unique," the only property `attach_proto`
//! ever relies on, already holds for the id as pipe_win minted it.
//!
//! # This bridge OWNS the `PipeServer` directly — no actor thread
//!
//! Round-2 e2e review's own deletion pressure: an earlier version of this
//! module could not let `capsule_win::run`'s thread touch `PipeServer`
//! directly, because it wrongly assumed sharing was necessary and
//! `PipeServer` is `Send` but not `Sync` (it holds an `mpsc::Receiver`
//! internally). But nothing here ever needs to SHARE it — `run`'s thread
//! is the ONLY thread that ever touches a `PipeTransport`, and
//! `PipeServer::send`/`close`/`events()` are already `&self`, synchronous,
//! non-blocking methods (their own async work happens on `PipeServer`'s
//! OWN internal accept/reader/writer/reaper threads, invisible from out
//! here). So `PipeTransport` just OWNS a `PipeServer` (`Option`, empty
//! until [`Transport::bind`]) and calls straight through: `send`/`close`
//! forward directly, and [`Transport::try_recv_event`] polls
//! `PipeServer::events()` with a single non-blocking `try_recv()`. This
//! deletes the actor thread, its command channel, its unbounded forwarding
//! channel, and the join `shutdown_all` used to need — `shutdown_all` is
//! now just dropping the `PipeServer` (its own `Drop` already closes
//! every connection and the listener).
//!
//! Deleting the forwarding channel also discharges a real correctness gap
//! the old shape had: `PipeServer::events()` is deliberately BOUNDED (it
//! force-closes a connection whose `Bytes` cannot be delivered within its
//! own timeout, see that module's doc) — draining it into a second,
//! UNBOUNDED channel defeated that bound, letting a slow consumer
//! accumulate unlimited buffered bytes regardless of what `pipe_win`
//! itself was willing to hold. Polling the bounded channel directly, with
//! nothing standing between it and `run`'s own loop, means `pipe_win`'s
//! bound is the ONLY bound — exactly as intended.
//!
//! `Transport::try_recv_event` is on `run`'s own critical per-iteration
//! path (see that method's doc in `transport.rs`): it must never block
//! or add its own wait, since `run`'s ONE latency budget per iteration is
//! its `output_rx.recv_timeout(GROUP_COMMIT_WINDOW)`. A plain
//! `Receiver::try_recv()` (never `recv_timeout`) is what makes that true
//! here.
//!
//! # `AcceptError` maps to `TransportEvent::TransportFatal`
//!
//! `pipe_win::TransportEvent::AcceptError` means no future connection can
//! ever be accepted while this capsule holds the pipe's name — an
//! unreachable-forever session if `run` just kept going regardless
//! (round-2 e2e review, finding 4). This bridge translates it to
//! [`transport::TransportEvent::TransportFatal`], which `run` maps to an
//! orderly self-end on the SAME path as an externally requested `EndRun`
//! — see that variant's own doc for the full policy.
//!
//! # A queue-full or otherwise-failed send LATCHES the connection
//!
//! `PipeServer::send` can refuse a send outright (`Err`) if that
//! connection's own outbound BYTE budget is exhausted — a case
//! `attach_proto`'s own admission control is tuned to make vanishingly
//! rare, but not impossible under a genuine mismatch or a misbehaving
//! peer. `Transport::send` has no `Result` to report that through, and
//! silently swallowing it would leave `attach_proto`'s own
//! `outstanding_sends` bookkeeping for that connection permanently
//! non-zero. Closing the connection right there (as before) guarantees
//! the loop eventually sees a `ConnectionClosed` for it — but round-2 e2e
//! review, finding 3, caught that this alone is not enough: nothing
//! stopped a LATER, smaller `send` for that same connection from
//! reaching `PipeServer` (and the peer) successfully, landing AFTER a gap
//! left by the failed one — a broken per-connection stream-prefix
//! property. `closing` (a `HashSet<ConnId>`) latches the instant a
//! connection is closed OR a send to it fails: every later `send` for a
//! latched connection is dropped without ever reaching `PipeServer`,
//! never just delayed or reordered. Entries are removed once that
//! connection's `Closed` event is actually observed — pure memory
//! tidiness, not a correctness requirement, since `pipe_win::ConnId`s are
//! never reused.
//!
//! One more asymmetry worth naming (review's own observation): the loop's
//! `attach_proto` budgets LIVE output in raw, undecoded bytes
//! (`WATCHER_LIVE_QUEUE_BUDGET_BYTES`), while `PipeServer` budgets
//! outbound bytes AFTER wire framing (magic + length prefix + the encoded
//! body, per connection). The two are close but not identical — framing
//! overhead means `PipeServer`'s own budget can theoretically bind first
//! on a connection with many small frames. That is fine: `PipeServer`'s
//! own budget is the transport's ENFORCEMENT of last resort (it never
//! silently drops what it accepted), and the latch above is what keeps
//! delivery FIFO-honest whichever budget actually trips.

#![cfg(windows)]

use crate::pipe_win::{ConnId, PipeServer, TransportEvent as PipeEvent};
use crate::transport::{Transport, TransportEvent as CapsuleEvent};
use crate::Result;
use std::collections::HashSet;
use std::time::Instant;

/// A `Transport` over a real `PipeServer`. Constructed UNBOUND (see
/// [`PipeTransport::new`]); `Transport::bind` is what `capsule_win::run`
/// calls, at the exact point ADR 0041's pipe-lifetime invariant requires,
/// to actually create the pipe.
pub struct PipeTransport {
    max_instances: u32,
    server: Option<PipeServer>,
    /// See the module doc's "A queue-full or otherwise-failed send
    /// LATCHES the connection" section.
    closing: HashSet<ConnId>,
    next_send_id: u64,
}

impl PipeTransport {
    /// An unbound transport ready to hand to `capsule_win::run` —
    /// `max_instances` is the RAW total simultaneous pipe-instance ceiling
    /// `PipeServer::bind`'s own doc requires (subscribers plus separately
    /// bounded pre-hello/mgmt connections; computing that combination is
    /// the CALLER's job, not this bridge's).
    pub fn new(max_instances: u32) -> Self {
        Self {
            max_instances,
            server: None,
            closing: HashSet::new(),
            next_send_id: 0,
        }
    }
}

impl Transport for PipeTransport {
    fn bind(&mut self, voyage_id: &str) -> Result<()> {
        self.server = Some(PipeServer::bind(voyage_id, self.max_instances)?);
        // A fresh server restarts conn ids; no latch entry may outlive the
        // server whose connections it described (see shutdown_all).
        self.closing.clear();
        Ok(())
    }

    fn try_recv_event(&mut self) -> Option<CapsuleEvent> {
        let evt = self.server.as_ref()?.events().try_recv().ok()?;
        if let PipeEvent::Closed(conn, _reason) = &evt {
            self.closing.remove(conn);
        }
        Some(translate(evt))
    }

    fn send(&mut self, conn: ConnId, bytes: Vec<u8>) -> u64 {
        self.next_send_id += 1;
        let id = self.next_send_id;
        if self.closing.contains(&conn) {
            // Latched: this connection's stream is already broken (a
            // prior close or send failure), so forwarding this send could
            // let it overtake the gap and reach the peer out of order.
            // Dropped, never queued -- `ConnectionClosed` (already under
            // way, or already delivered) is what clears the loop's own
            // `pending_sends`/`outstanding_sends` bookkeeping for it, not
            // a completion for this id, which will never arrive.
            return id;
        }
        if let Some(server) = &self.server {
            if server.send(conn, bytes, Some(id)).is_err() {
                // See the module doc's "A queue-full or otherwise-failed
                // send LATCHES the connection" section.
                self.closing.insert(conn);
                server.close(conn);
            }
        }
        id
    }

    fn close(&mut self, conn: ConnId) {
        self.closing.insert(conn);
        if let Some(server) = &self.server {
            server.close(conn);
        }
    }

    fn shutdown_all(&mut self, deadline: Instant) -> bool {
        // Codex round-1 Blocker 2/3 discharge: explicit cancellation-first
        // teardown against the SHARED `deadline` -- `disconnect_listener`
        // makes the pipe name (and every live connection's handle) gone
        // synchronously, THEN `join_workers` waits out every thread this
        // transport owns against `deadline`, never a budget it invents
        // itself. Dropping `PipeServer` afterward (its own `Drop`) is then
        // a documented no-op: both methods are idempotent, and everything
        // is already joined/cleared. The closing latch is cleared with it:
        // shutdown emits no Closed events to clear entries, and a later
        // bind's fresh PipeServer restarts conn ids at zero -- a stale
        // latch would silently drop the new server's sends (review
        // finding).
        let ok = if let Some(server) = &mut self.server {
            server.disconnect_listener();
            server.join_workers(deadline)
        } else {
            true
        };
        self.server = None;
        self.closing.clear();
        ok
    }
}

/// Direct field-for-field translation — total (every `pipe_win` event has
/// a home on the capsule side now; see the module doc's `AcceptError`
/// section for the one variant that maps to something other than a
/// per-connection event).
fn translate(evt: PipeEvent) -> CapsuleEvent {
    match evt {
        PipeEvent::Accepted(conn) => CapsuleEvent::ConnectionOpened(conn),
        PipeEvent::Bytes(conn, bytes) => CapsuleEvent::Bytes(conn, bytes),
        PipeEvent::Sent(conn, marker) => CapsuleEvent::Sent(conn, marker),
        PipeEvent::Closed(conn, _reason) => CapsuleEvent::ConnectionClosed(conn),
        PipeEvent::AcceptError(message) => CapsuleEvent::TransportFatal(message),
    }
}
