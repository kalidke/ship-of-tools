//! `AttachProto`'s checkpoint streaming and the shared bookkeeping helpers (outstanding sends, close with refusal).

use super::*;

impl AttachProto {
    // -- checkpoint streaming (finding 10) -------------------------------

    pub(super) fn advance_checkpoint_stream(&mut self, conn: ConnId, now: Instant) -> Vec<Action> {
        let next = match self.conns.get(&conn).and_then(watcher_checkpoint) {
            Some(CheckpointProgress::Sending { bytes, offset }) => Some((bytes, offset)),
            _ => None,
        };
        let Some((bytes, offset)) = next else { return vec![] };
        self.emit_next_checkpoint_chunk(conn, bytes, offset, now)
    }

    pub(super) fn emit_next_checkpoint_chunk(&mut self, conn: ConnId, bytes: Arc<Vec<u8>>, offset: usize, now: Instant) -> Vec<Action> {
        let max = wire::MAX_CHECKPOINT_CHUNK_PAYLOAD;
        let end = (offset + max).min(bytes.len());
        let is_last = end == bytes.len();
        let chunk = AttachServer::CheckpointChunk {
            last: is_last,
            bytes: bytes[offset..end].to_vec(),
        };
        let encoded = wire::encode_attach_server(&chunk)
            .expect("each chunk is capped at MAX_CHECKPOINT_CHUNK_PAYLOAD by construction");
        let attach_request_id = match self.conns.get(&conn).map(|c| &c.role) {
            Some(Role::Watcher(w)) => Some(w.attach_request_id),
            _ => None,
        };
        let clears_request = if offset == 0 { attach_request_id } else { None };
        if let Some(Role::Watcher(w)) = self.conns.get_mut(&conn).map(|c| &mut c.role) {
            w.checkpoint = CheckpointProgress::Sending { bytes: bytes.clone(), offset: end };
        }
        vec![self.make_send(
            conn,
            encoded,
            Some(SentMarker::CheckpointChunk { clears_request, is_last }),
            now,
        )]
    }

    // -- shared helpers ------------------------------------------------

    /// Allocates a fresh [`RequestId`], records it as `conn`'s outstanding
    /// request, and returns it so the caller can embed it in whatever
    /// eventual reply resolves this request (finding 4).
    pub(super) fn mark_outstanding(&mut self, conn: ConnId) -> RequestId {
        self.next_request_id += 1;
        let rid = self.next_request_id;
        if let Some(c) = self.conns.get_mut(&conn) {
            c.outstanding_request = Some(rid);
        }
        rid
    }

    /// Clears `conn`'s outstanding request ONLY if it is still `rid`
    /// (finding 4) — an unrelated or late marker must never clear a
    /// DIFFERENT, still-pending request. Returns whether it actually
    /// cleared (round-2 e2e review, finding 1: `clear_outstanding_and_replay`
    /// uses this to know whether replaying a held frame is even in play).
    pub(super) fn clear_outstanding_if_matches(&mut self, conn: ConnId, rid: RequestId) -> bool {
        if let Some(c) = self.conns.get_mut(&conn) {
            if c.outstanding_request == Some(rid) {
                c.outstanding_request = None;
                c.reply_queued = false;
                return true;
            }
        }
        false
    }

    /// Round-2 e2e review, finding 1: once the reply that actually clears
    /// `conn`'s outstanding request completes, replay whatever frame
    /// `frame` held for it (see that method's own doc) through `frame`
    /// itself — `outstanding_request` now `None`, it processes normally.
    /// A no-op (returns `vec![]`) if `rid` didn't match (nothing cleared)
    /// or nothing was held. Only sound for markers that leave the
    /// connection ALIVE afterward — `ReplyThenClose`/`ShutdownAck` close
    /// it instead, so they call `clear_outstanding_if_matches` directly
    /// and never route through here.
    pub(super) fn clear_outstanding_and_replay(&mut self, conn: ConnId, rid: RequestId, now: Instant) -> Vec<Action> {
        if !self.clear_outstanding_if_matches(conn, rid) {
            return vec![];
        }
        let Some(held) = self.conns.get_mut(&conn).and_then(|c| c.held_frame.take()) else {
            return vec![];
        };
        self.frame(conn, held, now)
    }

    /// Whether `marker` is the [`SentMarker`] variant that, once its
    /// completion fires, would clear `outstanding` via
    /// `clear_outstanding_if_matches` — used only to flip
    /// `Conn::reply_queued` at the moment the send is QUEUED (`make_send`),
    /// never to clear anything itself.
    fn reply_clears(marker: &Option<SentMarker>, outstanding: Option<RequestId>) -> bool {
        let Some(outstanding) = outstanding else {
            return false;
        };
        match marker {
            Some(SentMarker::Reply { request_id })
            | Some(SentMarker::ReplyThenClose { request_id })
            | Some(SentMarker::ShutdownAck { request_id, .. }) => *request_id == outstanding,
            Some(SentMarker::CheckpointChunk {
                clears_request: Some(rid),
                ..
            }) => *rid == outstanding,
            _ => false,
        }
    }

    /// Constructs a `Send` action AND records the generic write-progress
    /// bookkeeping (finding 5): increments `outstanding_sends`, and resets
    /// `last_send_progress` on the empty→nonempty transition. Every `Send`
    /// this module ever emits goes through here — there is no other
    /// construction site.
    pub(super) fn make_send(&mut self, conn: ConnId, frame_bytes: Vec<u8>, marker: Option<SentMarker>, now: Instant) -> Action {
        if let Some(c) = self.conns.get_mut(&conn) {
            if c.outstanding_sends == 0 {
                c.last_send_progress = now;
            }
            c.outstanding_sends += 1;
            if Self::reply_clears(&marker, c.outstanding_request) {
                c.reply_queued = true;
            }
        }
        Action::Send { conn, frame_bytes, marker }
    }

    pub(super) fn ground_timeout(&mut self, conn: ConnId, request_id: RequestId, now: Instant) -> Vec<Action> {
        if self.checkpoint_slot == Some(conn) {
            self.checkpoint_slot = None;
            self.advance_checkpoint_queue(now);
        } else {
            self.checkpoint_queue.retain(|&id| id != conn);
        }

        let bytes = wire::encode_attach_server(&AttachServer::AttachRefused {
            reason: AttachRefusedReason::GroundTimeout,
        })
        .expect("fixed-shape body");

        if self.non_watcher_count >= NON_WATCHER_CAP {
            // Demoting would exceed the shared non-watcher cap (finding
            // 12) -- close instead. The role stays `Watcher` until this
            // reply's `Sent` fires `remove_connection`, which does the
            // `watcher_count` bookkeeping; touching it here would
            // double-count.
            return vec![self.make_send(conn, bytes, Some(SentMarker::ReplyThenClose { request_id }), now)];
        }

        // Demote: this connection stops being a Watcher right now, so its
        // OWN bookkeeping must happen here -- no later `remove_connection`
        // call will ever see it as a Watcher again.
        self.watcher_count = self.watcher_count.saturating_sub(1);
        self.non_watcher_count += 1;
        if let Some(c) = self.conns.get_mut(&conn) {
            c.role = Role::PostHello {
                deadline: now + PRE_ADMISSION_TIMEOUT,
            };
        }
        vec![self.make_send(conn, bytes, Some(SentMarker::Reply { request_id }), now)]
    }

    pub(super) fn advance_checkpoint_queue(&mut self, now: Instant) {
        if self.checkpoint_slot.is_some() {
            return;
        }
        let Some(next) = self.checkpoint_queue.pop_front() else {
            return;
        };
        self.checkpoint_slot = Some(next);
        if let Some(Role::Watcher(w)) = self.conns.get_mut(&next).map(|c| &mut c.role) {
            w.checkpoint = CheckpointProgress::AwaitingGround {
                deadline: now + GROUND_TIMEOUT,
            };
        }
    }

    /// Returns whatever this removal itself provokes — today, only ever
    /// the ADR 0046 decision 3 `PenChanged { holder: None }` broadcast
    /// when `conn` was the current driver (lane B3b1): every OTHER
    /// consequence of removal is pure bookkeeping with no wire action of
    /// its own.
    pub(super) fn remove_connection(&mut self, conn: ConnId, now: Instant) -> Vec<Action> {
        let Some(c) = self.conns.remove(&conn) else {
            return vec![];
        };
        match c.role {
            Role::Unclassified { .. } | Role::Mgmt | Role::PostHello { .. } => {
                self.non_watcher_count = self.non_watcher_count.saturating_sub(1);
            }
            Role::Watcher(_) => {
                self.watcher_count = self.watcher_count.saturating_sub(1);
            }
        }
        self.checkpoint_queue.retain(|&id| id != conn);
        if self.checkpoint_slot == Some(conn) {
            self.checkpoint_slot = None;
            self.advance_checkpoint_queue(now);
        }
        if self.driver.as_ref().map(|d| d.conn) == Some(conn) {
            // Capability-only EOF -- no durable transition (ADR 0041).
            // The durable epoch is unaffected by losing the connection,
            // so the broadcast echoes the LAST committed epoch, not a
            // reset one — lane B3b1: every remaining v3 watcher learns
            // the pen is gone from the voyage itself, never by inferring
            // it from a stalled stream.
            let take_epoch = self.driver.as_ref().map_or(0, |d| d.take_epoch);
            self.driver = None;
            // Codex round-2 review, blocker: if THIS removal was itself
            // discovered while charging a broadcast already in progress
            // (an overflow closing the connection a `PenChanged`/
            // `Geometry`/`Output` broadcast is still iterating over —
            // e.g. charging the new driver's own copy of its OWN
            // `PenChanged(Some(holder))`), publishing the loss HERE
            // would recurse into that still-running broadcast: the
            // recursive call reaches every watcher first, correctly,
            // but the OUTER broadcast then resumes and overwrites it
            // with its own now-stale `Some(holder)` for whichever
            // watchers it had not reached yet. Defer instead — `self.
            // driver` is already cleared above, so every OTHER decision
            // in this same pass already sees the truth; only the
            // ANNOUNCEMENT waits for `end_broadcast` to publish it once
            // the outer broadcast has fully settled.
            if self.broadcasting {
                self.deferred_driver_eviction = Some(take_epoch);
                return vec![];
            }
            return self.broadcast_pen_changed(None, take_epoch, now);
        }
        vec![]
    }

    pub(super) fn close_with_refusal(&mut self, conn: ConnId, reason: RefusalReason, now: Instant) -> Vec<Action> {
        let mut actions = self.remove_connection(conn, now);
        actions.push(Action::RecordRefusal {
            conn: Some(conn),
            reason,
        });
        actions.push(Action::Close(conn));
        actions
    }
}
