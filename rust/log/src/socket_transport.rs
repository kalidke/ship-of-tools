//! The L1-unix LU1b bridge between [`crate::socket_unix`]'s real
//! Unix-domain-socket transport and [`crate::transport`]'s `Transport`
//! trait — a line-for-line twin of [`crate::pipe_transport`], adapted only
//! in the nouns it names. Neither side knows about the other:
//! `socket_unix` is a byte transport with no opinion on what rides over
//! it; `capsule.rs`'s run loop drives `AttachProto` against an abstract
//! `Transport`. This module is the thin adapter that makes the real
//! socket satisfy that trait — nothing here decides protocol behavior, it
//! only moves bytes and translates event/id shapes.
//!
//! # Conn-id spaces are the SAME space, not reconciled by a map
//!
//! `socket_unix::ConnId` and `attach_proto::ConnId` are both bare `u64`
//! aliases, each independently allocated by its own module. Since
//! `SocketServer` already guarantees its own ids are globally unique and
//! stable for the connection's whole life, this bridge reuses THAT id
//! verbatim as the capsule's `ConnId` — no translation table, exactly like
//! `pipe_transport`'s own reasoning.
//!
//! # This bridge OWNS the `SocketServer` directly — no actor thread
//!
//! Same shape as `pipe_transport::PipeTransport`: `SocketServer::send`/
//! `close`/`events()` are already `&self`, synchronous, non-blocking
//! methods (their own async work happens on `SocketServer`'s OWN internal
//! accept/reader/writer/reaper threads, invisible from out here), so this
//! bridge just OWNS a `SocketServer` (`Option`, empty until
//! [`Transport::bind`]) and calls straight through.
//! [`Transport::try_recv_event`] polls `SocketServer::events()` with a
//! single non-blocking `try_recv()` — `socket_unix`'s own bounded events
//! channel is the ONLY bound, exactly as `pipe_transport` establishes for
//! `pipe_win`.
//!
//! # `AcceptError` maps to `TransportEvent::TransportFatal`
//!
//! `socket_unix::TransportEvent::AcceptError` means no future connection
//! can ever be accepted while this capsule holds the socket's name — this
//! bridge translates it to [`transport::TransportEvent::TransportFatal`],
//! which the capsule's run loop maps to an orderly self-end. See
//! `pipe_transport`'s own doc for the full policy this shares.
//!
//! # A queue-full or otherwise-failed send LATCHES the connection
//!
//! Identical to `pipe_transport::PipeTransport`'s own `closing` latch: a
//! `SocketServer::send` failure (the connection's own outbound BYTE budget
//! exhausted) closes the connection right there AND remembers it in
//! `closing`, so a LATER, smaller send for that same connection is
//! dropped rather than reaching the peer out of order after a gap — the
//! per-connection stream-prefix property `pipe_transport`'s own module
//! doc explains in full.

#![cfg(unix)]

use crate::socket_unix::{ConnId, SocketServer, TransportEvent as SocketEvent};
use crate::transport::{Transport, TransportEvent as CapsuleEvent};
use crate::Result;
use std::collections::HashSet;
use std::time::Instant;

/// A `Transport` over a real `SocketServer`. Constructed UNBOUND (see
/// [`SocketTransport::new`]); `Transport::bind` is what the capsule's run
/// loop calls, at the exact point ADR 0041's endpoint-lifetime invariant
/// requires, to actually create the socket.
pub struct SocketTransport {
    max_connections: u32,
    server: Option<SocketServer>,
    /// See the module doc's "A queue-full or otherwise-failed send
    /// LATCHES the connection" section.
    closing: HashSet<ConnId>,
    next_send_id: u64,
}

impl SocketTransport {
    /// An unbound transport ready to hand to the capsule's run loop —
    /// `max_connections` is the RAW total simultaneous connection ceiling
    /// [`SocketServer::bind`]'s own doc requires (subscribers plus
    /// separately bounded pre-hello/mgmt connections; computing that
    /// combination is the CALLER's job, not this bridge's).
    pub fn new(max_connections: u32) -> Self {
        Self {
            max_connections,
            server: None,
            closing: HashSet::new(),
            next_send_id: 0,
        }
    }
}

impl Transport for SocketTransport {
    fn bind(&mut self, voyage_id: &str) -> Result<()> {
        self.server = Some(SocketServer::bind(voyage_id, self.max_connections)?);
        // A fresh server restarts conn ids; no latch entry may outlive
        // the server whose connections it described (see shutdown_all).
        self.closing.clear();
        Ok(())
    }

    fn try_recv_event(&mut self) -> Option<CapsuleEvent> {
        let evt = self.server.as_ref()?.events().try_recv().ok()?;
        if let SocketEvent::Closed(conn, _reason) = &evt {
            self.closing.remove(conn);
        }
        Some(translate(evt))
    }

    fn send(&mut self, conn: ConnId, bytes: Vec<u8>) -> u64 {
        self.next_send_id += 1;
        let id = self.next_send_id;
        if self.closing.contains(&conn) {
            // Latched: this connection's stream is already broken (a
            // prior close or send failure), so forwarding this send
            // could let it overtake the gap and reach the peer out of
            // order. Dropped, never queued.
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
        // Explicit cancellation-first teardown against the SHARED
        // `deadline` -- `disconnect_listener` makes the socket name (and
        // every live connection's fd) gone synchronously, THEN
        // `join_workers` waits out every thread this transport owns
        // against `deadline`. Dropping `SocketServer` afterward (its own
        // `Drop`) is then a documented no-op: both methods are
        // idempotent, and everything is already joined/cleared. The
        // closing latch is cleared with it: shutdown emits no Closed
        // events to clear entries, and a later bind's fresh
        // SocketServer restarts conn ids at zero.
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

/// Direct field-for-field translation — total (every `socket_unix` event
/// has a home on the capsule side now; see the module doc's
/// `AcceptError` section for the one variant that maps to something
/// other than a per-connection event). Identical five-arm shape to
/// `pipe_transport::translate`.
fn translate(evt: SocketEvent) -> CapsuleEvent {
    match evt {
        SocketEvent::Accepted(conn) => CapsuleEvent::ConnectionOpened(conn),
        SocketEvent::Bytes(conn, bytes) => CapsuleEvent::Bytes(conn, bytes),
        SocketEvent::Sent(conn, marker) => CapsuleEvent::Sent(conn, marker),
        SocketEvent::Closed(conn, _reason) => CapsuleEvent::ConnectionClosed(conn),
        SocketEvent::AcceptError(message) => CapsuleEvent::TransportFatal(message),
    }
}
