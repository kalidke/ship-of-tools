//! `AttachProto`'s pen/geometry broadcast and the per-frame dispatch of mgmt and attach-client requests.

use super::*;

impl AttachProto {
    // -- ADR 0046 decision 3 (lane B3b1): owner-emitted pen/geometry -----

    /// Sends `event` to `conn` right now if its checkpoint transfer is
    /// already `Done`, or queues it on `pending_post_watermark`
    /// (drained, in order, right after that watcher's own final chunk —
    /// see `sent`'s `CheckpointChunk` arm) otherwise. A no-op for
    /// anything but a v3 `Watcher`: v1/v2 have no wire shape for either
    /// event, so their `pending_post_watermark` only ever holds `Output`
    /// (the pre-existing, unchanged behavior for them).
    /// Codex review round, should-fix 5 (capsule half): a queued
    /// `PenChanged`/`Geometry` is charged against the SAME per-watcher
    /// `queued_live_bytes` budget as ordinary output, via the SAME
    /// `bytes_queued` call `output_committed` already makes for every
    /// live byte — "rare and small" is not itself a bound, and
    /// exhaustion takes the identical visible-termination path
    /// (`bytes_queued`'s own `QueueOverflow`/`DriverQueueOverflow`
    /// close), never a second, unbounded list. Mirrors
    /// `output_committed`'s own shape exactly: charge first, THEN
    /// decide send-now vs. queue against whatever role/checkpoint state
    /// remains — `None` covers both "closed by that charge's own
    /// overflow" and "not a watcher," with nothing further to do either
    /// way.
    pub(super) fn send_or_queue_pen_geometry(&mut self, conn: ConnId, event: QueuedPostWatermark, now: Instant) -> Vec<Action> {
        let Some(c) = self.conns.get(&conn) else { return vec![] };
        if c.attach_proto_version != wire::ATTACH_PROTO_V3 {
            return vec![];
        }
        let encoded = wire::encode_attach_server(&event.clone().into_attach_server())
            .expect("pen/geometry events are fixed-shape bodies, always within MAX_BODY_LEN");
        let n = encoded.len() as u64;
        let mut actions = self.bytes_queued(conn, n, now);
        match self.conns.get(&conn).and_then(watcher_checkpoint) {
            Some(CheckpointProgress::Done) => {
                actions.push(self.make_send(conn, encoded, Some(SentMarker::OutputBytes { n }), now));
            }
            Some(_) => {
                if let Some(Role::Watcher(w)) = self.conns.get_mut(&conn).map(|c| &mut c.role) {
                    w.pending_post_watermark.push_back(event);
                }
            }
            None => {} // closed by bytes_queued's own overflow handling, or not a watcher
        }
        actions
    }

    /// Every currently-registered `Watcher` connection id — the shared
    /// target set for [`Self::broadcast_pen_changed`]/
    /// [`Self::broadcast_geometry`], collected up front (as
    /// `output_committed` already does) so the broadcast loop never
    /// borrows `self.conns` while also mutating it.
    pub(super) fn watcher_conns(&self) -> Vec<ConnId> {
        self.conns
            .iter()
            .filter_map(|(id, c)| match &c.role {
                Role::Watcher(_) => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// Codex round-2 review, blocker: marks a broadcast
    /// (`output_committed`/`broadcast_pen_changed`/`broadcast_geometry`)
    /// as in progress, returning whatever `broadcasting` already was.
    /// Pass the result back to [`Self::end_broadcast`] so a broadcast
    /// invoked from within another one still nests correctly — only the
    /// OUTERMOST pair actually toggles the flag and drains the deferred
    /// eviction, an inner one is a no-op past setting `broadcasting`
    /// (already `true`) and restoring it. This is the guard that makes
    /// [`Self::remove_connection`]'s driver-eviction branch defer its
    /// own `PenChanged` announcement rather than recursing into a
    /// broadcast that is still iterating its own connection snapshot —
    /// see [`Self::deferred_driver_eviction`]'s own doc for the bug this
    /// closes.
    pub(super) fn begin_broadcast(&mut self) -> bool {
        std::mem::replace(&mut self.broadcasting, true)
    }

    /// Restores `broadcasting` to `was_broadcasting` (the value
    /// [`Self::begin_broadcast`] returned) and, only for the OUTERMOST
    /// call (`was_broadcasting` was `false`), drains and publishes any
    /// driver eviction deferred during the broadcast that just finished
    /// — a fresh, top-level `broadcast_pen_changed(None, ..)` against
    /// the now fully-settled state, reaching every surviving watcher
    /// exactly once with the CURRENT truth. An inner call leaves the
    /// deferred eviction for its own outer caller to publish.
    pub(super) fn end_broadcast(&mut self, was_broadcasting: bool, now: Instant) -> Vec<Action> {
        self.broadcasting = was_broadcasting;
        if was_broadcasting {
            return vec![];
        }
        match self.deferred_driver_eviction.take() {
            Some(take_epoch) => self.broadcast_pen_changed(None, take_epoch, now),
            None => vec![],
        }
    }

    /// `PenChanged` to every v3 `Watcher`, `conn`'s own included where
    /// `conn` is itself a watcher (uniformly — see `take_committed`'s own
    /// doc for why the new driver is not special-cased out of its own
    /// broadcast).
    pub(super) fn broadcast_pen_changed(&mut self, holder: Option<String>, take_epoch: u64, now: Instant) -> Vec<Action> {
        let was_broadcasting = self.begin_broadcast();
        let mut actions = Vec::new();
        for conn in self.watcher_conns() {
            actions.extend(self.send_or_queue_pen_geometry(
                conn,
                QueuedPostWatermark::PenChanged { holder: holder.clone(), take_epoch },
                now,
            ));
        }
        actions.extend(self.end_broadcast(was_broadcasting, now));
        actions
    }

    /// `Geometry` to every v3 `Watcher` — see [`Self::broadcast_pen_changed`].
    pub(super) fn broadcast_geometry(&mut self, cols: u16, rows: u16, now: Instant) -> Vec<Action> {
        let was_broadcasting = self.begin_broadcast();
        let mut actions = Vec::new();
        for conn in self.watcher_conns() {
            actions.extend(self.send_or_queue_pen_geometry(conn, QueuedPostWatermark::Geometry { cols, rows }, now));
        }
        actions.extend(self.end_broadcast(was_broadcasting, now));
        actions
    }

    // -- internal dispatch -------------------------------------------------

    pub(super) fn handle_mgmt(&mut self, conn: ConnId, req: MgmtRequest, now: Instant) -> Vec<Action> {
        match self.conns.get(&conn).map(|c| &c.role) {
            Some(Role::Unclassified { .. }) => {
                if let Some(c) = self.conns.get_mut(&conn) {
                    c.role = Role::Mgmt;
                }
            }
            Some(Role::Mgmt) => {}
            // Structurally unreachable given wire.rs's own lane latching
            // (an mgmt-tagged body cannot arrive on a connection already
            // latched to the attach lane) — refuse rather than panic.
            _ => return self.close_with_refusal(conn, RefusalReason::LaneSequenceViolation, now),
        }
        let rid = self.mark_outstanding(conn);
        match req {
            MgmtRequest::Probe => {
                let bytes = wire::encode_mgmt_reply(&MgmtReply::ProbeOk).expect("fixed-shape body");
                vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id: rid }), now)]
            }
            MgmtRequest::Status => {
                let s = self.mgmt_status;
                let bytes = wire::encode_mgmt_reply(&MgmtReply::StatusOk {
                    pid: s.pid,
                    created: s.created,
                    survival: s.survival,
                })
                .expect("fixed-shape body");
                vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id: rid }), now)]
            }
            MgmtRequest::Shutdown { reason } => {
                let bytes = wire::encode_mgmt_reply(&MgmtReply::ShutdownOk).expect("fixed-shape body");
                // ADR 0041 EndRun steps 1-2: the durable marker is
                // appended and IRREVOCABLY LATCHED in the SAME
                // writer-loop step, BEFORE the ack is queued —
                // `RunEndRequested` sits first in this batch, and the
                // loop processes a batch in order (finding 7's own
                // established discipline), so the marker append always
                // runs before this `Send` even reaches the transport.
                vec![
                    Action::RunEndRequested { reason: reason.clone() },
                    self.make_send(conn, bytes, Some(SentMarker::ShutdownAck { request_id: rid, reason }), now),
                ]
            }
        }
    }

    pub(super) fn handle_attach_client(&mut self, conn: ConnId, frame: AttachClient, now: Instant) -> Vec<Action> {
        let role_is_unclassified = matches!(self.conns.get(&conn).map(|c| &c.role), Some(Role::Unclassified { .. }));
        let role_is_mgmt = matches!(self.conns.get(&conn).map(|c| &c.role), Some(Role::Mgmt));
        if role_is_mgmt {
            // Structurally unreachable given wire.rs's own lane latching.
            return self.close_with_refusal(conn, RefusalReason::LaneSequenceViolation, now);
        }
        if role_is_unclassified && !matches!(frame, AttachClient::Hello { .. }) {
            return self.close_with_refusal(conn, RefusalReason::LaneSequenceViolation, now);
        }
        if !role_is_unclassified && matches!(frame, AttachClient::Hello { .. }) {
            return self.close_with_refusal(conn, RefusalReason::LaneSequenceViolation, now);
        }
        let is_watcher = matches!(self.conns.get(&conn).map(|c| &c.role), Some(Role::Watcher(_)));
        if matches!(frame, AttachClient::Attach { .. }) && is_watcher {
            return self.close_with_refusal(conn, RefusalReason::LaneSequenceViolation, now);
        }

        let rid = self.mark_outstanding(conn);
        match frame {
            AttachClient::Hello { proto } => self.handle_hello(conn, proto, rid, now),
            AttachClient::Attach { controller_id: _ } => self.handle_attach(conn, rid, now),
            AttachClient::Take { controller_id } => self.handle_take(conn, controller_id, rid, now),
            AttachClient::Input {
                controller_id,
                take_epoch,
                idem_key,
                payload,
            } => self.handle_input(conn, controller_id, take_epoch, idem_key, payload, rid),
            AttachClient::Resize { cols, rows } => self.handle_resize(conn, cols, rows, rid, now),
        }
    }

    fn handle_hello(&mut self, conn: ConnId, proto: u32, request_id: RequestId, now: Instant) -> Vec<Action> {
        match wire::negotiate(proto) {
            wire::Negotiated::Accepted(v) => {
                if let Some(c) = self.conns.get_mut(&conn) {
                    if let Role::Unclassified { deadline } = c.role {
                        c.role = Role::PostHello { deadline };
                    }
                    c.attach_proto_version = v;
                }
                let bytes = wire::encode_attach_server(&AttachServer::HelloOk { proto: v }).expect("fixed-shape body");
                vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)]
            }
            wire::Negotiated::Refused { supported } => {
                let bytes =
                    wire::encode_attach_server(&AttachServer::HelloRefused { supported }).expect("fixed-shape body");
                vec![self.make_send(conn, bytes, Some(SentMarker::ReplyThenClose { request_id }), now)]
            }
        }
    }

    fn handle_attach(&mut self, conn: ConnId, request_id: RequestId, now: Instant) -> Vec<Action> {
        if self.watcher_count >= SUBSCRIBER_CAP {
            let bytes = wire::encode_attach_server(&AttachServer::AttachRefused {
                reason: AttachRefusedReason::SubscriberCap,
            })
            .expect("fixed-shape body");
            return vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)];
        }
        self.non_watcher_count = self.non_watcher_count.saturating_sub(1);
        self.watcher_count += 1;
        let checkpoint = if self.checkpoint_slot.is_none() {
            self.checkpoint_slot = Some(conn);
            CheckpointProgress::AwaitingGround {
                deadline: now + GROUND_TIMEOUT,
            }
        } else {
            self.checkpoint_queue.push_back(conn);
            CheckpointProgress::QueuedForSlot
        };
        if let Some(c) = self.conns.get_mut(&conn) {
            c.role = Role::Watcher(WatcherState {
                attach_request_id: request_id,
                checkpoint,
                queued_live_bytes: 0,
                pending_post_watermark: VecDeque::new(),
            });
        }
        vec![]
    }

    fn handle_take(&mut self, conn: ConnId, controller_id: String, request_id: RequestId, now: Instant) -> Vec<Action> {
        if self.teardown {
            self.clear_outstanding_if_matches(conn, request_id);
            return vec![];
        }
        let checkpoint = self.conns.get(&conn).and_then(watcher_checkpoint);
        let reason = match checkpoint {
            None => Some(TakeRefusedReason::NotAttached),
            Some(CheckpointProgress::Done) => None,
            Some(_) => Some(TakeRefusedReason::CheckpointInFlight),
        };
        if let Some(reason) = reason {
            let bytes = wire::encode_attach_server(&AttachServer::TakeRefused { reason }).expect("fixed-shape body");
            return vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)];
        }
        vec![Action::CommitTake { conn, controller_id, request_id }]
    }

    fn handle_input(
        &mut self,
        conn: ConnId,
        controller_id: String,
        take_epoch: u64,
        idem_key: [u8; 16],
        payload: Vec<u8>,
        request_id: RequestId,
    ) -> Vec<Action> {
        if self.teardown {
            self.clear_outstanding_if_matches(conn, request_id);
            return vec![];
        }
        let connection_authorized = self.driver.as_ref().map(|d| d.conn) == Some(conn);
        vec![Action::ForwardInput {
            conn,
            controller_id,
            take_epoch,
            idem_key,
            payload,
            connection_authorized,
            request_id,
        }]
    }

    fn handle_resize(&mut self, conn: ConnId, cols: u16, rows: u16, request_id: RequestId, now: Instant) -> Vec<Action> {
        if self.teardown {
            self.clear_outstanding_if_matches(conn, request_id);
            return vec![];
        }
        if self.driver.as_ref().map(|d| d.conn) != Some(conn) {
            let bytes = wire::encode_attach_server(&AttachServer::ResizeRefused {
                reason: ResizeRefusedReason::NotDriver,
            })
            .expect("fixed-shape body");
            return vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)];
        }
        vec![Action::ApplyResize { conn, cols, rows, request_id }]
    }

    /// Round-2 review, finding 3: nonces now retire by CONNECTION, not by
    /// "is this the current `DriverState`" — a same-connection retake
    /// (`take_committed`, same `conn`) preserves the prior nonce instead of
    /// discarding it, so this method no longer needs to special-case that
    /// path itself. Two questions, asked in this order:
    ///
    /// 1. Was `nonce` EVER issued to `conn`, at all? If not — a watcher
    ///    that was never driver, or any other fabricated value — this is a
    ///    genuine protocol violation, never silently accepted.
    /// 2. Is `conn` the driver RIGHT NOW, with THIS nonce still the one
    ///    outstanding? If so, the reply is answered for real (clear the
    ///    gate, refresh activity). Otherwise it is a recognized but no
    ///    longer actionable echo — a demoted former driver's late reply, or
    ///    a duplicate of one already cleared — ignorable, not fatal.
    pub(super) fn handle_keepalive_reply(&mut self, conn: ConnId, nonce: u64, now: Instant) -> Vec<Action> {
        let ever_issued = self.conns.get(&conn).and_then(|c| c.last_keepalive_nonce);
        if ever_issued != Some(nonce) {
            return self.close_with_refusal(conn, RefusalReason::UnexpectedKeepalive, now);
        }
        let is_current_and_outstanding = self
            .driver
            .as_ref()
            .is_some_and(|d| d.conn == conn && d.keepalive_outstanding == Some(nonce));
        if !is_current_and_outstanding {
            return vec![];
        }
        if let Some(d) = &mut self.driver {
            d.keepalive_outstanding = None;
            d.keepalive_deadline = None;
        }
        if let Some(c) = self.conns.get_mut(&conn) {
            c.last_activity = now;
        }
        vec![]
    }

    /// Round-2 review deletion residue: this used to also FREEZE the
    /// reply deadline while the driver's OWN checkpoint transfer was still
    /// in flight (finding 6, original discharge round) -- but `take`
    /// itself refuses admission (`CheckpointInFlight`) until a
    /// connection's own checkpoint is already `Done`, and nothing ever
    /// moves a watcher's checkpoint backward out of `Done` once reached.
    /// A connection can therefore never actually BECOME the driver while
    /// its own transfer is in flight, which made that whole branch (and
    /// `DriverState.last_tick`, which existed only to compute it) dead in
    /// every real path -- deleted rather than kept "in case", per this
    /// round's own deletion pressure. If a future design lets `take`
    /// admit a connection before its checkpoint finishes, this suspension
    /// needs reintroducing deliberately, not resurrecting from here.
    pub(super) fn tick_keepalive(&mut self, now: Instant) -> Vec<Action> {
        let Some(conn) = self.driver.as_ref().map(|d| d.conn) else {
            return vec![];
        };

        let (outstanding, deadline) = {
            let d = self.driver.as_ref().expect("checked above");
            (d.keepalive_outstanding, d.keepalive_deadline)
        };
        if let Some(deadline) = deadline {
            if now >= deadline {
                return self.close_with_refusal(conn, RefusalReason::KeepaliveDeath, now);
            }
            return vec![];
        }
        if outstanding.is_some() {
            // Sent, sent-completion not yet reported: deadline not armed.
            return vec![];
        }
        let idle = self
            .conns
            .get(&conn)
            .is_some_and(|c| now.saturating_duration_since(c.last_activity) >= KEEPALIVE_IDLE_TRIGGER);
        if !idle {
            return vec![];
        }
        self.nonce_counter += 1;
        let nonce = self.nonce_counter;
        if let Some(d) = &mut self.driver {
            d.keepalive_outstanding = Some(nonce);
        }
        if let Some(c) = self.conns.get_mut(&conn) {
            c.last_keepalive_nonce = Some(nonce);
        }
        let bytes = wire::encode_keepalive(nonce);
        vec![self.make_send(conn, bytes, Some(SentMarker::Keepalive { nonce }), now)]
    }
}
