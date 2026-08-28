//! The ADR 0041 step-5 bridge between [`crate::pipe_win`]'s real named-pipe
//! transport and [`crate::capsule_win`]'s `Transport` trait — the deferred
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
//! # One actor thread owns the `PipeServer` — not `Arc`-shared
//!
//! `PipeServer` is `Send` but NOT `Sync` (it holds an `mpsc::Receiver`
//! internally, which is never `Sync`), so it cannot be wrapped in `Arc`
//! and handed to a second thread while this one keeps calling `&self`
//! methods on it — `Arc<T>: Send` itself requires `T: Sync`. So instead of
//! sharing it, exactly ONE thread (spawned by
//! [`Transport::bind`](crate::capsule_win::Transport::bind), as
//! implemented here) OWNS the `PipeServer` for its entire life: created at
//! the top of [`actor_loop`], used there, and dropped there too, at the
//! bottom, when told to stop. `capsule_win::run`'s OWN thread never
//! touches the `PipeServer` value directly — [`PipeTransport::send`]/
//! [`PipeTransport::close`] instead post a [`PipeCmd`] over an (unbounded,
//! so posting never blocks — satisfying `Transport::send`'s own contract)
//! channel the actor thread drains every iteration.
//!
//! `capsule_win::run`'s own `transport_events: mpsc::Receiver<capsule_win::
//! TransportEvent>` parameter is a DIFFERENT channel of a DIFFERENT type
//! from `PipeServer::events()` — `run`'s signature is fixed (a concrete
//! `Receiver<T>`, not something a custom adapter can stand in for), so
//! direct channel reuse is not on the table; [`PipeTransport::new`] hands
//! back the `Receiver` half of a channel it owns the `Sender` half of, and
//! the actor thread is what pushes translated events into it (see
//! [`actor_loop`] for the direct field-for-field event mapping).
//!
//! The actor loop polls `PipeServer::events()` with `recv_timeout` rather
//! than blocking on `recv()`, specifically so it can also drain `PipeCmd`s
//! promptly and notice its own shutdown signal — a blocking `recv()` would
//! leave `send`/`close` commands (and shutdown) waiting arbitrarily long
//! behind pipe traffic that may never arrive.
//!
//! # `AcceptError` has no home here yet
//!
//! `pipe_win::TransportEvent::AcceptError` (the accept loop's own
//! persistent-failure signal) has no corresponding
//! `capsule_win::TransportEvent` variant — adding one would be a change to
//! `capsule_win`'s or `attach_proto`'s own protocol surface, out of this
//! bridge's scope. [`actor_loop`] logs it (`eprintln!`) and continues:
//! existing connections are unaffected either way, and the capsule's own
//! `run` loop has no mechanism today to observe "no more connections will
//! ever be accepted." A future round that wants this visible in the
//! voyage record needs to extend `capsule_win::TransportEvent`, not this
//! module.
//!
//! # A queue-full send force-closes its connection
//!
//! `PipeServer::send` can refuse a send outright (`Err`) if that
//! connection's own outbound BYTE budget is exhausted — a case
//! `attach_proto`'s own admission control is tuned to make vanishingly
//! rare, but not impossible to hit under a genuine mismatch or a
//! misbehaving peer. `Transport::send` has no `Result` to report that
//! through (it always returns a bare id, and by the time the actor thread
//! actually attempts it the caller is long gone), and silently swallowing
//! it would leave `attach_proto`'s own `outstanding_sends` bookkeeping for
//! that connection permanently non-zero — nothing would ever clear it.
//! Closing the connection right there instead guarantees the loop
//! eventually sees a `ConnectionClosed` for it, which `attach_proto`
//! already knows how to clean up.

#![cfg(windows)]

use crate::capsule_win::{Transport, TransportEvent as CapsuleEvent};
use crate::pipe_win::{ConnId, PipeServer, TransportEvent as PipeEvent};
use crate::{Error, Result};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// How often the actor thread's `PipeServer::events()` poll wakes to check
/// for a queued [`PipeCmd`] (including its own shutdown signal) — short
/// enough that `shutdown_all`'s join, and `send`/`close`'s own effective
/// latency, stay small, long enough not to spin.
const ACTOR_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A command posted to the actor thread — see the module doc's "One actor
/// thread owns the `PipeServer`" section for why these exist instead of
/// `PipeTransport::send`/`close` calling `PipeServer` directly.
enum PipeCmd {
    Send(ConnId, Vec<u8>, u64),
    Close(ConnId),
    /// Stop, drop the `PipeServer` (closing every connection and the
    /// listener), and return.
    Shutdown,
}

/// A `Transport` over a real `PipeServer`. Constructed UNBOUND (see
/// [`PipeTransport::new`]); `Transport::bind` is what `capsule_win::run`
/// calls, at the exact point ADR 0041's pipe-lifetime invariant requires,
/// to actually create the pipe and start the actor thread.
pub struct PipeTransport {
    max_instances: u32,
    forward_tx: Sender<CapsuleEvent>,
    cmd_tx: Option<Sender<PipeCmd>>,
    actor_jh: Option<JoinHandle<()>>,
    next_send_id: u64,
}

impl PipeTransport {
    /// An unbound transport ready to hand to `capsule_win::run` —
    /// `max_instances` is the RAW total simultaneous pipe-instance ceiling
    /// `PipeServer::bind`'s own doc requires (subscribers plus separately
    /// bounded pre-hello/mgmt connections; computing that combination is
    /// the CALLER's job, not this bridge's). Returns the `Receiver` half
    /// of the translated event stream — hand that straight to `run`'s
    /// `transport_events` parameter, and `&mut` this value as `run`'s
    /// `transport` parameter.
    pub fn new(max_instances: u32) -> (Self, Receiver<CapsuleEvent>) {
        let (forward_tx, rx) = mpsc::channel();
        (
            Self {
                max_instances,
                forward_tx,
                cmd_tx: None,
                actor_jh: None,
                next_send_id: 0,
            },
            rx,
        )
    }
}

impl Transport for PipeTransport {
    fn bind(&mut self, voyage_id: &str) -> Result<()> {
        let server = PipeServer::bind(voyage_id, self.max_instances).map_err(Error::from)?;
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let forward_tx = self.forward_tx.clone();
        let jh = thread::Builder::new()
            .name("sot-pipe-transport-actor".into())
            .spawn(move || actor_loop(server, cmd_rx, forward_tx))
            .map_err(|e| Error::State(format!("spawn pipe-transport actor thread: {e}")))?;
        self.cmd_tx = Some(cmd_tx);
        self.actor_jh = Some(jh);
        Ok(())
    }

    fn send(&mut self, conn: ConnId, bytes: Vec<u8>) -> u64 {
        self.next_send_id += 1;
        let id = self.next_send_id;
        if let Some(tx) = &self.cmd_tx {
            // An unbounded `mpsc` post never blocks — satisfies
            // `Transport::send`'s own "enqueues without blocking"
            // contract trivially.
            let _ = tx.send(PipeCmd::Send(conn, bytes, id));
        }
        id
    }

    fn close(&mut self, conn: ConnId) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(PipeCmd::Close(conn));
        }
    }

    fn shutdown_all(&mut self) {
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(PipeCmd::Shutdown);
        }
        if let Some(jh) = self.actor_jh.take() {
            jh.join().ok();
        }
    }
}

/// The actor thread's body: OWNS `server` for its whole life (see the
/// module doc). Each iteration drains every currently-queued `PipeCmd`
/// (so `send`/`close`/`shutdown` are serviced promptly rather than
/// waiting behind an idle events poll), then polls for one `pipe_win`
/// event and translates it. Returns (dropping `server`, closing
/// everything) on `PipeCmd::Shutdown`, or if `cmd_rx` disconnects (the
/// `PipeTransport` itself was dropped without an explicit `shutdown_all`
/// — treated the same as a shutdown request).
fn actor_loop(server: PipeServer, cmd_rx: Receiver<PipeCmd>, forward_tx: Sender<CapsuleEvent>) {
    'outer: loop {
        loop {
            match cmd_rx.try_recv() {
                Ok(PipeCmd::Send(conn, bytes, marker)) => {
                    if server.send(conn, bytes, Some(marker)).is_err() {
                        // See the module doc's "A queue-full send
                        // force-closes its connection" section.
                        server.close(conn);
                    }
                }
                Ok(PipeCmd::Close(conn)) => server.close(conn),
                Ok(PipeCmd::Shutdown) => break 'outer,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break 'outer,
            }
        }
        match server.events().recv_timeout(ACTOR_POLL_INTERVAL) {
            Ok(evt) => {
                if let Some(mapped) = translate(evt) {
                    // If `run` has already stopped reading (mid-teardown),
                    // there is nothing left to forward to — keep servicing
                    // commands/shutdown regardless rather than exiting
                    // early, since `shutdown_all` (below) is what this
                    // loop's OWN termination is gated on, not the
                    // consumer's.
                    let _ = forward_tx.send(mapped);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break 'outer,
        }
    }
    drop(server);
}

/// Direct field-for-field translation — see the module doc's "`AcceptError`
/// has no home here yet" section for the one event kind this returns
/// `None` for instead.
fn translate(evt: PipeEvent) -> Option<CapsuleEvent> {
    match evt {
        PipeEvent::Accepted(id) => Some(CapsuleEvent::ConnectionOpened(id)),
        PipeEvent::Bytes(id, bytes) => Some(CapsuleEvent::Bytes(id, bytes)),
        PipeEvent::Sent(id, marker) => Some(CapsuleEvent::Sent(id, marker)),
        PipeEvent::Closed(id, _reason) => Some(CapsuleEvent::ConnectionClosed(id)),
        PipeEvent::AcceptError(message) => {
            eprintln!("sot-capsule: pipe accept loop stopped accepting connections: {message}");
            None
        }
    }
}
