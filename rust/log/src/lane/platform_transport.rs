//! The capsule's `Transport` over the platform's real lane server, `PlatformLaneServer`.
//!
//! The ADR 0041 step-5 bridge between the platform's lane server
//! ([`crate::lane::pipe_win`]'s named-pipe server on Windows,
//! [`crate::lane::socket_unix`]'s domain-socket server on Linux and macOS) and
//! [`crate::lane::transport`]'s `Transport` trait. Neither side knows about
//! the other: the lane server is a byte transport with no opinion on what
//! rides over it; `capsule::run` drives `AttachProto` against an abstract
//! `Transport`. This module is the thin adapter that makes the real lane
//! satisfy that trait — nothing here decides protocol behavior, it only
//! moves bytes and translates event/id shapes.
//!
//! # Conn-id spaces are the SAME space, not reconciled by a map
//!
//! The lane server and `attach_proto` share one `ConnId`, a bare `u64`
//! alias defined in `attach_proto`, allocated only by the server (its
//! accept loop hands out its own sequence; `attach_proto` never allocates
//! one at all — it only ever learns of a `ConnId` through a
//! `transport::TransportEvent::ConnectionOpened` this module produces). Since
//! the server already guarantees its own ids are globally unique and
//! stable for the connection's whole life, this bridge reuses THAT id
//! verbatim as the capsule's `ConnId` — no translation table, because
//! none is needed: "stable and unique," the only property `attach_proto`
//! ever relies on, already holds for the id as the server minted it.
//!
//! # This bridge OWNS the lane server directly — no actor thread
//!
//! Nothing here ever needs to SHARE the server: `run`'s thread is the
//! ONLY thread that ever touches a `PlatformTransport`, and the server's
//! `send`/`close`/`events()` are already `&self`, synchronous, non-blocking
//! methods (their own async work happens on the server's OWN internal
//! accept/reader/writer/reaper threads, invisible from out here; the server
//! is `Send` but not `Sync`, as it holds an `mpsc::Receiver` internally). So
//! `PlatformTransport` just OWNS a `PlatformLaneServer` (`Option`, empty
//! until [`Transport::bind`]) and calls straight through: `send`/`close`
//! forward directly, and [`Transport::try_recv_event`] polls the server's
//! `events()` with a single non-blocking `try_recv()`. There is no actor
//! thread, command channel or forwarding channel, and `shutdown_all` is
//! the server's teardown against the shared deadline.
//!
//! The server's `events()` channel is deliberately BOUNDED (it force-closes
//! a connection whose `Bytes` cannot be delivered within its own timeout,
//! see the server module's doc). Polling it directly, with nothing standing
//! between it and `run`'s own loop, means that bound is the ONLY bound; a
//! second, UNBOUNDED forwarding channel would let a slow consumer
//! accumulate unlimited buffered bytes regardless of what the server itself
//! was willing to hold.
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
//! `crate::lane::transport::LaneEvent::AcceptError` means no future connection can
//! ever be accepted while this capsule holds the lane's endpoint name — an
//! unreachable-forever session if `run` just kept going regardless. This
//! bridge translates it to [`transport::TransportEvent::TransportFatal`],
//! which `run` maps to an orderly self-end on the SAME path as an
//! externally requested `EndRun` — see that variant's own doc for the full
//! policy.
//!
//! # A queue-full or otherwise-failed send LATCHES the connection
//!
//! The server's `send` can refuse a send outright (`Err`) if that
//! connection's own outbound BYTE budget is exhausted — a case
//! `attach_proto`'s own admission control is tuned to make vanishingly
//! rare, but not impossible under a genuine mismatch or a misbehaving
//! peer. `Transport::send` has no `Result` to report that through, and
//! silently swallowing it would leave `attach_proto`'s own
//! `outstanding_sends` bookkeeping for that connection permanently
//! non-zero. Closing the connection right there guarantees the loop
//! eventually sees a `ConnectionClosed` for it, but that alone is not
//! enough: nothing would stop a LATER, smaller `send` for that same
//! connection from reaching the server (and the peer) successfully,
//! landing AFTER a gap left by the failed one — a broken per-connection
//! stream-prefix property. `closing` (a `HashSet<ConnId>`) latches the
//! instant a connection is closed OR a send to it fails: every later
//! `send` for a latched connection is dropped without ever reaching the
//! server, never just delayed or reordered. Entries are removed once that
//! connection's `Closed` event is actually observed — pure memory
//! tidiness, not a correctness requirement, since the server's `ConnId`s
//! are never reused.
//!
//! One more asymmetry worth naming: the loop's `attach_proto` budgets LIVE
//! output in raw, undecoded bytes (`WATCHER_LIVE_QUEUE_BUDGET_BYTES`), while
//! the server budgets outbound bytes AFTER wire framing (magic + length
//! prefix + the encoded body, per connection). The two are close but not
//! identical — framing overhead means the server's own budget can
//! theoretically bind first on a connection with many small frames. That is
//! fine: the server's own budget is the transport's ENFORCEMENT of last
//! resort (it never silently drops what it accepted), and the latch above
//! is what keeps delivery FIFO-honest whichever budget actually trips.

#![cfg(any(windows, target_os = "linux", target_os = "macos"))]

use crate::lane::attach_proto::ConnId;
use crate::lane::transport::{
    LaneEvent, PlatformLaneServer, Transport, TransportEvent as CapsuleEvent,
};
use crate::Result;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

/// A `Transport` over a real `PlatformLaneServer`. Constructed UNBOUND (see
/// [`PlatformTransport::new`]); `Transport::bind` is what `capsule::run`
/// calls, at the exact point ADR 0041's endpoint-lifetime invariant
/// requires, to actually create the lane's endpoint.
pub struct PlatformTransport {
    max_connections: u32,
    server: Option<PlatformLaneServer>,
    /// See the module doc's "A queue-full or otherwise-failed send
    /// LATCHES the connection" section.
    closing: HashSet<ConnId>,
    next_send_id: u64,
    /// Switch-latency Phase 1 (c): `Transport::set_wake`'s callback,
    /// stashed here (rather than forgotten) because `bind` — not
    /// `PlatformTransport::new` — is what actually creates the server
    /// this needs to be forwarded to. `None` for a transport `run` never
    /// registered one on (degrades to the trait's own documented default:
    /// no early wake, `try_recv_event`'s regular polling still finds
    /// everything eventually).
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl PlatformTransport {
    /// An unbound transport ready to hand to `capsule::run` —
    /// `max_connections` is the RAW total simultaneous connection ceiling
    /// the lane server's `bind` requires (subscribers plus
    /// separately bounded pre-hello/mgmt connections; computing that
    /// combination is the CALLER's job, not this bridge's).
    pub fn new(max_connections: u32) -> Self {
        Self {
            max_connections,
            server: None,
            closing: HashSet::new(),
            next_send_id: 0,
            wake: None,
        }
    }
}

impl Transport for PlatformTransport {
    fn set_wake(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.wake = Some(wake);
    }

    fn bind(&mut self, voyage_id: &str) -> Result<()> {
        let server = PlatformLaneServer::bind(voyage_id, self.max_connections)?;
        // Switch-latency Phase 1 (c): forward a wake registered before
        // `bind` (the trait's own contract) to the FRESH server that
        // exists only from here on — every event it queues from this
        // point pings it, per the server's `set_wake` doc.
        if let Some(wake) = &self.wake {
            server.set_wake(Arc::clone(wake));
        }
        self.server = Some(server);
        // A fresh server restarts conn ids; no latch entry may outlive the
        // server whose connections it described (see shutdown_all).
        self.closing.clear();
        Ok(())
    }

    fn try_recv_event(&mut self) -> Option<CapsuleEvent> {
        let evt = self.server.as_ref()?.events().try_recv().ok()?;
        if let LaneEvent::Closed(conn, _reason) = &evt {
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
        // `deadline` -- `disconnect_listener` makes the endpoint name (and
        // every live connection's handle) gone synchronously, THEN
        // `join_workers` waits out every thread this transport owns
        // against `deadline`, never a budget it invents itself. Dropping
        // the server afterward (its own `Drop`) is then a documented
        // no-op: both methods are idempotent, and everything is already
        // joined/cleared. The closing latch is cleared with it: shutdown
        // emits no Closed events to clear entries, and a later bind's
        // fresh server restarts conn ids at zero -- a stale latch would
        // silently drop the new server's sends.
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

/// Direct field-for-field translation — total (every lane-server event has
/// a home on the capsule side now; see the module doc's `AcceptError`
/// section for the one variant that maps to something other than a
/// per-connection event).
fn translate(evt: LaneEvent) -> CapsuleEvent {
    match evt {
        LaneEvent::Accepted(conn) => CapsuleEvent::ConnectionOpened(conn),
        LaneEvent::Bytes(conn, bytes) => CapsuleEvent::Bytes(conn, bytes),
        LaneEvent::Sent(conn, marker) => CapsuleEvent::Sent(conn, marker),
        LaneEvent::Closed(conn, _reason) => CapsuleEvent::ConnectionClosed(conn),
        LaneEvent::AcceptError(message) => CapsuleEvent::TransportFatal(message),
    }
}
