//! `AttachProto`'s construction, teardown and the lifecycle events the leg feeds back (`frame`, `sent`, `tick`, ...).

use super::*;

impl AttachProto {
    #[must_use]
    pub fn new(mgmt_status: MgmtStatus) -> Self {
        Self {
            conns: HashMap::new(),
            mgmt_status,
            non_watcher_count: 0,
            watcher_count: 0,
            checkpoint_slot: None,
            checkpoint_queue: VecDeque::new(),
            driver: None,
            nonce_counter: 0,
            next_request_id: 0,
            teardown: false,
            broadcasting: false,
            deferred_driver_eviction: None,
        }
    }

    /// Producer-bound admission (`take`/`input`/`resize`) is revoked from
    /// this point on (finding 7) — mgmt and the attach lane's `hello`/
    /// `attach` are unaffected. Idempotent.
    pub fn begin_teardown(&mut self) {
        self.teardown = true;
    }

    // -- lifecycle events ------------------------------------------------

    /// A new connection accepted at the transport level. Refused outright
    /// (closed, no frame) if the combined mgmt/pre-hello cap is already at
    /// [`NON_WATCHER_CAP`] — every connection starts non-watcher, so this is
    /// the only place that cap can ever be exceeded.
    pub fn connection_opened(&mut self, conn: ConnId, now: Instant) -> Vec<Action> {
        if self.non_watcher_count >= NON_WATCHER_CAP {
            return vec![
                Action::RecordRefusal {
                    conn: Some(conn),
                    reason: RefusalReason::NonWatcherCapExceeded,
                },
                Action::Close(conn),
            ];
        }
        self.conns.insert(
            conn,
            Conn {
                role: Role::Unclassified {
                    deadline: now + PRE_ADMISSION_TIMEOUT,
                },
                outstanding_request: None,
                reply_queued: false,
                held_frame: None,
                last_activity: now,
                outstanding_sends: 0,
                last_send_progress: now,
                last_keepalive_nonce: None,
                attach_proto_version: wire::ATTACH_PROTO_V1,
            },
        );
        self.non_watcher_count += 1;
        vec![]
    }

    /// The transport reports `conn` is gone (ordered EOF or an error). Frees
    /// every reservation it held; if it held the ephemeral driver
    /// capability, that capability simply vanishes — no durable transition
    /// (ADR 0041's spec-gate deletion of the local-grant/EOF-clears-holder
    /// behavior).
    pub fn connection_closed(&mut self, conn: ConnId, now: Instant) -> Vec<Action> {
        self.remove_connection(conn, now)
    }

    /// One decoded frame arrived on `conn`, in order. May return zero, one,
    /// or several actions.
    pub fn frame(&mut self, conn: ConnId, decoded: DecodedFrame, now: Instant) -> Vec<Action> {
        if let DecodedFrame::Keepalive { nonce } = decoded {
            return self.handle_keepalive_reply(conn, nonce, now);
        }
        let Some(c) = self.conns.get_mut(&conn) else {
            return vec![];
        };
        if c.outstanding_request.is_some() {
            // Round-2 e2e review, finding 1: a real transport's reader and
            // writer threads race independently of each other -- a
            // compliant, fast client can read a reply and send its next
            // lockstep request before THIS module ever observes that
            // reply's own completion (`sent`). That is indistinguishable,
            // right here, from a genuinely early client -- so hold the
            // ONE frame that arrives while this connection's reply is
            // already queued (`reply_queued`) rather than refusing it;
            // `clear_outstanding_and_replay` replays it the instant the
            // matching completion clears `outstanding_request`. A second
            // frame arriving while one is ALREADY held is a real
            // violation (a compliant client never sends two requests
            // without waiting for a reply to the first), and "no reply
            // queued at all yet" (e.g. `take`, mid-`CommitTake`, waiting
            // on the loop's own fsync round trip) is also a real
            // violation -- neither of those is a transport-timing
            // artifact.
            if c.held_frame.is_some() {
                return self.close_with_refusal(conn, RefusalReason::LockstepViolation, now);
            }
            if c.reply_queued {
                // Codex round-2b: a HELD frame is still a genuinely valid,
                // arrived frame -- exactly the race the comment above
                // describes, not a stalled connection -- so it must count
                // as activity. Before this fix, holding never touched
                // `last_activity`, so a compliant client whose valid
                // request arrived a moment before the mgmt idle deadline
                // was evicted anyway (`tick`'s own scan is the only
                // consumer of this clock the held-frame path could ever
                // have satisfied). `outstanding_sends` stays whatever it
                // already was -- this frame does not itself queue a send,
                // its eventual replay does, once `clear_outstanding_and_
                // replay` runs.
                c.held_frame = Some(decoded);
                c.last_activity = now;
                return vec![];
            }
            return self.close_with_refusal(conn, RefusalReason::LockstepViolation, now);
        }
        c.last_activity = now;
        match decoded {
            DecodedFrame::MgmtRequest(req) => self.handle_mgmt(conn, req, now),
            DecodedFrame::MgmtReply(_)
            | DecodedFrame::AttachServer(_)
            | DecodedFrame::SupervisorRequest(_)
            | DecodedFrame::SupervisorReply(_) => {
                // A client sending server-shaped bytes, or a frame from the
                // ADR 0041 step 6 U2 supervisor lane's own third magic — the
                // voyage pipe's `FrameSplitter` decodes by tag/magic alone
                // with no notion of which pipe it is bound to, so both are
                // reachable from adversarial input on THIS pipe, not merely
                // "impossible" — reject them here, not with a panic. A
                // supervisor-lane frame can never legitimately arrive on
                // the voyage pipe: that lane lives on its own, separate
                // named pipe with its own `FrameSplitter` instance.
                self.close_with_refusal(conn, RefusalReason::LaneSequenceViolation, now)
            }
            DecodedFrame::AttachClient(ac) => self.handle_attach_client(conn, ac, now),
            DecodedFrame::Keepalive { .. } => unreachable!("handled above"),
        }
    }

    /// One `Action::Send`'s bytes were reported PHYSICALLY WRITTEN. `marker`
    /// is whatever the originating `Send` carried, or `None` for a send
    /// with no bookkeeping consequence. Always resets `last_activity`/
    /// `last_send_progress` and decrements `outstanding_sends` for `conn`
    /// (finding 5) — a checked decrement: more completions than sends ever
    /// issued is this module's own bookkeeping bug.
    pub fn sent(&mut self, conn: ConnId, marker: Option<SentMarker>, now: Instant) -> Vec<Action> {
        match self.conns.get_mut(&conn) {
            Some(c) => {
                c.last_activity = now;
                c.outstanding_sends = c
                    .outstanding_sends
                    .checked_sub(1)
                    .expect("sent(): more completions than sends were ever issued for this connection");
                c.last_send_progress = now;
            }
            None => return vec![], // already closed; nothing to do (finding 11)
        }
        let Some(marker) = marker else { return vec![] };
        match marker {
            SentMarker::Reply { request_id } => self.clear_outstanding_and_replay(conn, request_id, now),
            SentMarker::ReplyThenClose { request_id } => {
                self.clear_outstanding_if_matches(conn, request_id);
                let mut actions = self.remove_connection(conn, now);
                actions.push(Action::Close(conn));
                actions
            }
            SentMarker::ShutdownAck { request_id, reason } => {
                self.clear_outstanding_if_matches(conn, request_id);
                let mut actions = self.remove_connection(conn, now);
                actions.push(Action::Shutdown { reason });
                actions.push(Action::Close(conn));
                actions
            }
            SentMarker::CheckpointChunk { clears_request, is_last } => {
                // Only `Reply`/`CheckpointChunk`'s first-chunk completion
                // can ever have a frame held behind it (see `frame`'s own
                // doc) -- `ReplyThenClose`/`ShutdownAck` close the
                // connection instead, so they never go through
                // `clear_outstanding_and_replay`.
                if !is_last {
                    let mut actions = match clears_request {
                        Some(rid) => self.clear_outstanding_and_replay(conn, rid, now),
                        None => Vec::new(),
                    };
                    actions.extend(self.advance_checkpoint_stream(conn, now));
                    return actions;
                }
                // Final chunk: the transfer's terminal state must be
                // committed BEFORE any held frame replays -- a one-chunk
                // transfer's first chunk IS its last, and a compliant
                // client that read it can have a `take` already held; a
                // replay against still-in-flight state falsely refuses it
                // with CheckpointInFlight (review-reproduced race).
                let proto_version = self.conns.get(&conn).map(|c| c.attach_proto_version);
                let pending = match self.conns.get_mut(&conn).map(|c| &mut c.role) {
                    Some(Role::Watcher(w)) => {
                        w.checkpoint = CheckpointProgress::Done;
                        std::mem::take(&mut w.pending_post_watermark)
                    }
                    _ => VecDeque::new(),
                };
                if self.checkpoint_slot == Some(conn) {
                    self.checkpoint_slot = None;
                    self.advance_checkpoint_queue(now);
                }
                // Codex review round, blocker 1: this watcher's OWN
                // completion -- PenSnapshot, then everything queued
                // behind the transfer -- must be FULLY built before any
                // REPLAYED held request runs. A replayed `CommitTake`/
                // `ApplyResize`'s own loop-side handling (`capsule/writer_loop/output_path.rs`'s
                // `flush_output`) publishes MORE output to this SAME,
                // now-`Done` watcher inline, the moment the caller's
                // action loop reaches it -- which, positioned first as
                // before this fix, could physically reach the wire ahead
                // of a `PenSnapshot`/queued item still sitting LATER in
                // the very same returned action list (reproduced: "later
                // Output -> PenSnapshot -> Geometry -> older Output").
                // Building this watcher's own completion FIRST and
                // replaying LAST means nothing a replay can trigger is
                // ever queued for the transport ahead of this watcher's
                // already-committed history.
                let mut actions = Vec::new();
                // ADR 0046 decision 3 (lane B3b1): a v3 watcher's OWN
                // FIRST v3 event, always, before any queued `PenChanged`
                // — the CURRENT driving connection's controller (this
                // module's own ephemeral `self.driver`, never the
                // durable historical holder; see `DriverState`'s own
                // doc), synthesized fresh right here rather than queued,
                // since it must reflect the pen as of THIS moment, not as
                // of whenever the checkpoint itself was captured.
                if proto_version == Some(wire::ATTACH_PROTO_V3) {
                    let (holder, take_epoch) = match &self.driver {
                        Some(d) => (Some(d.controller_id.clone()), d.take_epoch),
                        None => (None, 0),
                    };
                    let snapshot = wire::encode_attach_server(&AttachServer::PenSnapshot { holder, take_epoch })
                        .expect("PenSnapshot is a bounded-shape body, always within MAX_BODY_LEN");
                    actions.push(self.make_send(conn, snapshot, None, now));
                }
                // v1/v2 watchers only ever queued `Output` entries (lane
                // B3b1's `send_or_queue_pen_geometry` refuses anything
                // else for them) -- draining generically here is exactly
                // the pre-existing Output-only flush for them, unchanged.
                // `n` matches whatever `bytes_queued` was charged at
                // enqueue time for this SAME entry (`output_committed`'s
                // raw payload length for `Output`; `send_or_queue_pen_
                // geometry`'s own encoded length otherwise) so the
                // eventual `Sent(OutputBytes)` completion decrements
                // `queued_live_bytes` by exactly what was charged.
                for item in pending {
                    let output_len = match &item {
                        QueuedPostWatermark::Output(bytes) => Some(bytes.len() as u64),
                        QueuedPostWatermark::PenChanged { .. } | QueuedPostWatermark::Geometry { .. } => None,
                    };
                    let encoded = wire::encode_attach_server(&item.into_attach_server())
                        .expect("queued post-watermark frame within the outer 1 MiB cap is the loop's own responsibility");
                    let n = output_len.unwrap_or(encoded.len() as u64);
                    actions.push(self.make_send(conn, encoded, Some(SentMarker::OutputBytes { n }), now));
                }
                // Replay LAST -- see this block's own note above.
                actions.extend(match clears_request {
                    Some(rid) => self.clear_outstanding_and_replay(conn, rid, now),
                    None => Vec::new(),
                });
                actions
            }
            SentMarker::Keepalive { nonce } => {
                if let Some(d) = &mut self.driver {
                    if d.conn == conn && d.keepalive_outstanding == Some(nonce) {
                        d.keepalive_deadline = Some(now + KEEPALIVE_REPLY_DEADLINE);
                    }
                }
                vec![]
            }
            SentMarker::OutputBytes { n } => {
                if let Some(Role::Watcher(w)) = self.conns.get_mut(&conn).map(|c| &mut c.role) {
                    w.queued_live_bytes = w
                        .queued_live_bytes
                        .checked_sub(n)
                        .expect("sent(OutputBytes): more bytes reported sent than were ever queued");
                }
                vec![]
            }
        }
    }

    /// `n` LIVE output bytes are about to be enqueued for `conn` (a
    /// `Watcher`; any other role is a no-op — checkpoint bytes never call
    /// this). Closes on overflow past [`WATCHER_LIVE_QUEUE_BUDGET_BYTES`],
    /// per decision 5's "the checkpoint work item rides OUTSIDE this
    /// budget" — this is the LIVE-only counter, tracked regardless of
    /// whether the watcher is `Done` yet (finding 3: pre-`Done` output
    /// still counts against the budget even though it is not sent yet).
    ///
    /// Round-2 review, finding 2 (an EARLIER exemption here was
    /// INCOMPLETE): the ADR's driver row is TWO clauses, not one —
    /// "driver queue 4 MiB; committed driver-visible bytes are never
    /// dropped while the connection is live, but transport liveness is
    /// bounded... a hung driver cannot wedge the writer loop". The 4 MiB
    /// BOUND STAYS for the driver exactly as for a watcher; what the ADR
    /// actually promises is HOW overflow resolves — by CLOSING the
    /// connection (never by silently dropping bytes while it stays live).
    /// The bytes themselves are never lost either way: they are already
    /// durable in the voyage before this call ever runs (the watermark
    /// barrier commits before it publishes), so a reconnect after this
    /// close replays them via a fresh `attach`'s checkpoint. This is the
    /// SAME mechanism as an ordinary watcher's eviction, just labeled
    /// `DriverQueueOverflow` instead of `QueueOverflow` — distinct enough
    /// for step 6's adoption UX to tell "a passive subscriber never
    /// drained" apart from "the driver itself could not keep up with its
    /// own producer" — because unlike a watcher, losing the driver ALSO
    /// clears the pen (`take`'s null-holder state), which a client is
    /// meant to notice and re-`attach`/`take` for.
    pub fn bytes_queued(&mut self, conn: ConnId, n: u64, now: Instant) -> Vec<Action> {
        let is_driver = self.driver.as_ref().is_some_and(|d| d.conn == conn);
        let overflowed = match self.conns.get_mut(&conn).map(|c| &mut c.role) {
            Some(Role::Watcher(w)) => {
                w.queued_live_bytes += n;
                w.queued_live_bytes > WATCHER_LIVE_QUEUE_BUDGET_BYTES
            }
            _ => false,
        };
        if overflowed {
            let reason = if is_driver { RefusalReason::DriverQueueOverflow } else { RefusalReason::QueueOverflow };
            self.close_with_refusal(conn, reason, now)
        } else {
            vec![]
        }
    }

    /// Time-driven checks with no inbound frame to trigger them: the two
    /// admission timeouts, the ground-wait deadline, the generic
    /// queue-progress stall (finding 5), and the driver keepalive state
    /// machine (finding 6).
    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();

        let mut pre_admission_timed_out = Vec::new();
        for (id, c) in &self.conns {
            let deadline = match &c.role {
                Role::Unclassified { deadline } | Role::PostHello { deadline } => Some(*deadline),
                _ => None,
            };
            if deadline.is_some_and(|d| now >= d) {
                pre_admission_timed_out.push(*id);
            }
        }
        for id in pre_admission_timed_out {
            actions.extend(self.close_with_refusal(id, RefusalReason::PreAdmissionTimeout, now));
        }

        let mut ground_timed_out = Vec::new();
        for (id, c) in &self.conns {
            if let Role::Watcher(w) = &c.role {
                if let CheckpointProgress::AwaitingGround { deadline } = w.checkpoint {
                    if now >= deadline {
                        ground_timed_out.push((*id, w.attach_request_id));
                    }
                }
            }
        }
        for (id, rid) in ground_timed_out {
            actions.extend(self.ground_timeout(id, rid, now));
        }

        // U1a Codex round-1, Major 4 discharge: the effective mgmt bound is
        // the EARLIER of `MGMT_IDLE_DEADLINE` (5s) and the generic
        // `PROGRESS_DEADLINE` (30s) below, not the later one. Round 1
        // shipped this gated on `outstanding_sends == 0`, which let a
        // client that stopped reading its OWN unconfirmed reply squat on
        // the non-watcher pool for the full 30s — exactly the "outstanding
        // server send expands the management bound" escape review found:
        // a tiny mgmt reply nobody drains for 5s is squatting, not
        // legitimate write progress.
        //
        // Codex round-2b: the clock this check reads is the MORE RECENT of
        // `last_activity` and `last_send_progress` — not a choice between
        // the two based on `outstanding_sends`, which round-1's own fix
        // shipped and which a live repro then broke: the transport can
        // report a peer's next lockstep request (a HELD frame, held
        // because THIS reply's own `Sent` completion has not arrived yet —
        // see `frame`'s own doc on the round-2 e2e review race) while
        // `outstanding_sends` is still nonzero, so reading ONLY
        // `last_send_progress` in that state ignored the held frame's own
        // freshly-refreshed `last_activity` entirely and evicted a
        // genuinely active, compliant client. Taking the max keeps BOTH
        // properties true at once: a client that stays silent (both clocks
        // stale) is still evicted at 5s exactly as round-1 intended, and a
        // client that keeps sending valid requests — even while our own
        // reply write is independently stuck — is not penalized for
        // activity `last_send_progress` alone cannot see. This scan runs
        // BEFORE the generic stalled scan below so a connection matching
        // BOTH gets the mgmt-specific label and eviction, never the
        // generic one — the two bounds would otherwise race for whichever
        // scan's `RefusalReason` a single large clock jump happens to see
        // first.
        let mut mgmt_idle = Vec::new();
        for (id, c) in &self.conns {
            if matches!(c.role, Role::Mgmt) {
                let last_progress = c.last_activity.max(c.last_send_progress);
                if now.saturating_duration_since(last_progress) >= MGMT_IDLE_DEADLINE {
                    mgmt_idle.push(*id);
                }
            }
        }
        for id in mgmt_idle {
            actions.extend(self.close_with_refusal(id, RefusalReason::MgmtIdleTimeout, now));
        }

        let mut stalled = Vec::new();
        for (id, c) in &self.conns {
            if c.outstanding_sends > 0 && now.saturating_duration_since(c.last_send_progress) >= PROGRESS_DEADLINE {
                stalled.push(*id);
            }
        }
        for id in stalled {
            actions.extend(self.close_with_refusal(id, RefusalReason::ProgressStall, now));
        }

        actions.extend(self.tick_keepalive(now));
        actions
    }

    /// Fed by the loop right after a group-commit where `parser.is_ground()`
    /// held. Requests a checkpoint for the current checkpoint-slot holder,
    /// if it is waiting — a no-op otherwise (ground recurs constantly; most
    /// calls have nothing to promote).
    pub fn ground_reached(&mut self, now: Instant) -> Vec<Action> {
        let _ = now;
        let Some(conn) = self.checkpoint_slot else {
            return vec![];
        };
        let awaiting = matches!(
            self.conns.get(&conn).and_then(watcher_checkpoint),
            Some(CheckpointProgress::AwaitingGround { .. })
        );
        if !awaiting {
            return vec![];
        }
        vec![Action::BeginCheckpoint { conn }]
    }

    /// Whether the loop has any reason to call [`AttachProto::ground_reached`]
    /// right now, beyond its own regular fresh-output cadence — i.e.
    /// whether the checkpoint-slot holder is currently `AwaitingGround`.
    ///
    /// Real CI failure (windows-latest, U2 round-3): `ground_reached` used
    /// to be fed ONLY from `flush_output`, itself reached only by FRESH
    /// output crossing the group-commit threshold, or a periodic idle
    /// timer tied to the output channel's own polling cadence — never
    /// directly by admission or by `tick`, the loop's one truly
    /// unconditional, every-iteration hook. Attaching to an ALREADY-idle,
    /// already-at-ground session (a shell sitting at its prompt — THE
    /// ordinary case) landed in that gap: nothing forced a fresh
    /// evaluation, so the attach pended for the full 5 s `GroundTimeout`
    /// before being refused, work that should have completed on the very
    /// next loop iteration. The loop now calls this cheap check every
    /// iteration and, when true, evaluates ground directly (see
    /// `capsule/writer_loop/output_path.rs`'s call site) instead of waiting on that separate
    /// cadence — `false` on the vastly more common "nothing pending"
    /// iteration costs one `Option` comparison.
    pub fn ground_gate_pending(&self) -> bool {
        self.checkpoint_slot
            .and_then(|conn| self.conns.get(&conn))
            .and_then(watcher_checkpoint)
            .is_some_and(|cp| matches!(cp, CheckpointProgress::AwaitingGround { .. }))
    }

    /// The attach-lane protocol version `conn` negotiated at `hello`
    /// (Codex round on #194, finding 1 — "attach proto v2 bound to
    /// checkpoint v2"). `capsule/writer_loop/lanes.rs`'s `BeginCheckpoint` handler
    /// reads this to decide which checkpoint format version to encode:
    /// [`wire::ATTACH_PROTO_V1`] gets checkpoint format v1 (no
    /// scrollback ring — the shape an old client's own vt100 fork build
    /// can actually read); anything newer gets the current format.
    /// [`wire::ATTACH_PROTO_V1`] for an unknown `conn` too — the same
    /// conservative default a connection that has not said hello yet
    /// carries, which `BeginCheckpoint` can structurally never fire for.
    pub fn negotiated_proto(&self, conn: ConnId) -> u32 {
        self.conns
            .get(&conn)
            .map_or(wire::ATTACH_PROTO_V1, |c| c.attach_proto_version)
    }

    /// The loop encoded `conn`'s checkpoint (only it can — see the module
    /// doc) and hands back the bytes. Wraps them in `Arc` ONCE and emits
    /// only the FIRST chunk (finding 10: streamed, not materialized all at
    /// once) — later chunks are requested by [`AttachProto::sent`]'s
    /// `CheckpointChunk` handling as each one completes. Ignored (defensive
    /// no-op) if `conn` is not this run's current slot holder still
    /// `AwaitingGround` — should not happen given the loop only ever calls
    /// this in response to `BeginCheckpoint`.
    ///
    /// Real CI failure (windows-2022, PR #139 discharge round): every
    /// `output_committed` call made while `conn` was `QueuedForSlot` or
    /// `AwaitingGround` — i.e. every group-commit round from `attach` until
    /// THIS one — has already queued its bytes into
    /// `WatcherState::pending_post_watermark` (`output_committed`'s `Some(_)
    /// => queue` arm treats every non-`Done` state alike). `bytes` (the live
    /// parser's checkpoint, taken via `capsule/writer_loop/output_path.rs`'s
    /// `flush_output`/`ground_reached` watermark barrier: fsync -> publish
    /// -> checkpoint, in that order, one loop step) reflects EXACTLY that
    /// same committed history — the barrier's own ordering is correct; the
    /// bug was never syncing the queue to it. Left alone, that backlog is a
    /// duplicate: the SAME bytes are already baked into the grid `bytes`
    /// encodes, and clearing it later at `Done` would deliver them a SECOND
    /// time on top of the checkpoint. Purging it HERE — the one moment this
    /// checkpoint's cut point and the queue's own contents are both in
    /// scope — is what makes the checkpoint and
    /// `WatcherState::pending_post_watermark` two genuinely
    /// non-overlapping halves of the same committed timeline, the
    /// invariant a fidelity check across the two can only hold if it's true
    /// (`tests/capsule/windows_only.rs`'s `attach_mid_stream_checkpoint_reproduces_
    /// reference_screen`).
    pub fn checkpoint_ready(&mut self, conn: ConnId, bytes: Vec<u8>, now: Instant) -> Vec<Action> {
        let awaiting = matches!(
            self.conns.get(&conn).and_then(watcher_checkpoint),
            Some(CheckpointProgress::AwaitingGround { .. })
        );
        if !awaiting || self.checkpoint_slot != Some(conn) {
            return vec![];
        }
        let shared: Arc<Vec<u8>> = Arc::new(bytes);
        if let Some(Role::Watcher(w)) = self.conns.get_mut(&conn).map(|c| &mut c.role) {
            w.checkpoint = CheckpointProgress::Sending { bytes: shared.clone(), offset: 0 };
            // Round-2 review, finding 1: clearing the backlog without also
            // releasing its OWN queued_live_bytes charge left that charge
            // permanently stuck (only an `OutputBytes` `Sent` ever
            // decrements it, and these cleared vectors will never produce
            // one) — a scratch probe reproduced a FALSE eviction one byte
            // after capture, from a charge belonging to bytes that no
            // longer exist anywhere but the checkpoint. Release it
            // atomically with the same clear that retires the bytes.
            //
            // Lane B3b1: this purge is `Output`-only. A `PenChanged`/
            // `Geometry` entry queued before this capture is NOT
            // redundant with `bytes` the way queued output is — the
            // checkpoint encodes screen content (and, incidentally, its
            // own capture-time geometry), never "who holds the pen," so
            // dropping a queued `PenChanged` here would be a real fact
            // this watcher would otherwise never learn. Both ride through
            // untouched, to be drained (still in order) once this watcher
            // reaches `Done` — see `sent`'s `CheckpointChunk` arm.
            let cleared_bytes: u64 = w
                .pending_post_watermark
                .iter()
                .filter_map(|e| match e {
                    QueuedPostWatermark::Output(bytes) => Some(bytes.len() as u64),
                    QueuedPostWatermark::PenChanged { .. } | QueuedPostWatermark::Geometry { .. } => None,
                })
                .sum();
            w.pending_post_watermark.retain(|e| !matches!(e, QueuedPostWatermark::Output(_)));
            w.queued_live_bytes = w.queued_live_bytes.checked_sub(cleared_bytes).expect(
                "pending_post_watermark's own Output contribution cannot exceed the connection's total queued_live_bytes",
            );
        }
        self.emit_next_checkpoint_chunk(conn, shared, 0, now)
    }

    /// The loop fsynced `take_state {holder: controller_id, epoch:
    /// new_take_epoch}`. Installs the ephemeral capability on `conn`,
    /// silently overwriting whoever held it before (the demotion — that
    /// connection's own `Watcher` entry is untouched; its keepalive nonce,
    /// if any, is simply discarded — see the module doc's keepalive
    /// section for why a late echo for it is then ignored rather than
    /// fatal). `controller_id` is the SAME string the loop's own
    /// `CommitTake` action carried (ADR 0046 decision 3, lane B3b1):
    /// stored on `DriverState` now that something reads it back — see
    /// that struct's own doc — and broadcast as `PenChanged` to every v3
    /// watcher, `conn`'s own included (uniformly with every other
    /// watcher: the voyage declares the fact to everyone who can hear
    /// it, not just the ones who didn't already know).
    pub fn take_committed(
        &mut self,
        conn: ConnId,
        controller_id: String,
        new_take_epoch: u64,
        request_id: RequestId,
        now: Instant,
    ) -> Vec<Action> {
        self.driver = Some(DriverState {
            conn,
            controller_id: controller_id.clone(),
            take_epoch: new_take_epoch,
            keepalive_outstanding: None,
            keepalive_deadline: None,
        });
        if let Some(c) = self.conns.get_mut(&conn) {
            c.last_activity = now;
        }
        let bytes = wire::encode_attach_server(&AttachServer::TakeOk { take_epoch: new_take_epoch })
            .expect("TakeOk is a fixed-shape body, always within MAX_BODY_LEN");
        let mut actions = vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)];
        actions.extend(self.broadcast_pen_changed(Some(controller_id), new_take_epoch, now));
        actions
    }

    /// The loop ran the input WAL for `conn`'s `input` frame and reports the
    /// outcome.
    pub fn input_outcome(&mut self, conn: ConnId, outcome: InputOutcome, request_id: RequestId, now: Instant) -> Vec<Action> {
        if let Some(c) = self.conns.get_mut(&conn) {
            c.last_activity = now;
        }
        let frame = match outcome {
            InputOutcome::Recorded => AttachServer::InputRecorded,
            InputOutcome::RefusedStale => AttachServer::InputRefusedStale,
            InputOutcome::DeliveryUnknown => AttachServer::InputDeliveryUnknown,
        };
        let bytes =
            wire::encode_attach_server(&frame).expect("input reply is a fixed-shape body, always within MAX_BODY_LEN");
        vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)]
    }

    /// The loop ran the ordered resize exchange for `conn` and reports
    /// whether the geometry was in budget. `cols`/`rows` are the SAME
    /// values the loop's own `Action::ApplyResize` carried — not stored
    /// anywhere on this module (geometry is externally OS-determined,
    /// unlike the pen; see the module doc's "Not owned" list), just
    /// round-tripped through, the same shape `new_take_epoch` already
    /// is for `take_committed`. On success (ADR 0046 decision 3, lane
    /// B3b1), broadcasts `Geometry` to every v3 watcher — never on
    /// refusal, which changed nothing about the actual terminal size.
    pub fn resize_outcome(&mut self, conn: ConnId, ok: bool, cols: u16, rows: u16, request_id: RequestId, now: Instant) -> Vec<Action> {
        if let Some(c) = self.conns.get_mut(&conn) {
            c.last_activity = now;
        }
        let frame = if ok {
            AttachServer::ResizeOk
        } else {
            AttachServer::ResizeRefused {
                reason: ResizeRefusedReason::OutOfBudget,
            }
        };
        let bytes =
            wire::encode_attach_server(&frame).expect("resize reply is a fixed-shape body, always within MAX_BODY_LEN");
        let mut actions = vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)];
        if ok {
            actions.extend(self.broadcast_geometry(cols, rows, now));
        }
        actions
    }

    /// Live producer output just committed (the watermark). For every
    /// `Watcher`: budget-check via `bytes_queued`; if `Done`, enqueue an
    /// `output` frame now; otherwise queue it behind the in-flight
    /// checkpoint transfer (finding 3) — flushed once that watcher reaches
    /// `Done`.
    pub fn output_committed(&mut self, bytes: &[u8], now: Instant) -> Vec<Action> {
        let was_broadcasting = self.begin_broadcast();
        let mut actions = Vec::new();
        for conn in self.watcher_conns() {
            actions.extend(self.bytes_queued(conn, bytes.len() as u64, now));
            match self.conns.get(&conn).and_then(watcher_checkpoint) {
                Some(CheckpointProgress::Done) => {
                    let encoded = wire::encode_attach_server(&AttachServer::Output { bytes: bytes.to_vec() })
                        .expect("output frame within the outer 1 MiB cap is the loop's own responsibility");
                    actions.push(self.make_send(
                        conn,
                        encoded,
                        Some(SentMarker::OutputBytes { n: bytes.len() as u64 }),
                        now,
                    ));
                }
                Some(_) => {
                    if let Some(Role::Watcher(w)) = self.conns.get_mut(&conn).map(|c| &mut c.role) {
                        w.pending_post_watermark.push_back(QueuedPostWatermark::Output(bytes.to_vec()));
                    }
                }
                None => {} // closed by bytes_queued's own overflow handling, or not a watcher
            }
        }
        actions.extend(self.end_broadcast(was_broadcasting, now));
        actions
    }
}
