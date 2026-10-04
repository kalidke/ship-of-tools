//! Ruling (b): take-on-first-input is a transaction (`TakeTransaction`).

use crate::lane::wire::{self, ResizeRefusedReason};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------
// (b) Take-on-first-input is a transaction
// ---------------------------------------------------------------------

/// Bound on the transient hold queue a watcher fills while a `take` is in
/// flight, and REUSED (Codex review round, finding 5) as the one bounded
/// pending-byte queue for DRIVING mode too — an input already outstanding
/// holds the NEXT input here rather than dropping it. Pinned to
/// [`wire::MAX_INPUT_PAYLOAD_LEN`] — not an independently-chosen number —
/// because the queue becomes, verbatim, a single `input` frame the
/// instant it is safe to send (ADR 0041 "Step 6 as specified": "hold the
/// input in a bounded 8 KiB queue (encoded bytes, one wire input's cap; a
/// larger paste splits there and the remainder is discarded visibly,
/// never delivered minutes late into a context that no longer exists)").
/// Two callers needing two different numbers here would be the bug this
/// constant exists to prevent.
pub const TAKE_QUEUE_CAP: usize = wire::MAX_INPUT_PAYLOAD_LEN;

/// `take_refused{checkpoint_in_flight}` retry cadence (ADR 0041: "retries
/// every 250 ms for up to 30 s, matching the connection's own
/// write-progress allowance, since a legal 8.65 MiB checkpoint is
/// entitled to that window").
pub const CHECKPOINT_IN_FLIGHT_RETRY: Duration = Duration::from_millis(250);
/// The 30 s budget above which a `checkpoint_in_flight` retry loop gives
/// up and discards the queue.
pub const CHECKPOINT_IN_FLIGHT_BUDGET: Duration = Duration::from_secs(30);

/// What this client currently is, over the take-epoch lattice ADR 0037
/// defines: a fresh attach (or reconnect) always arrives a WATCHER; the
/// first keystroke starts a `take` transaction; `take_ok` moves to
/// RESIZING (the wire's own lockstep — attach_proto's `LockstepViolation`
/// — allows exactly one outstanding request per connection, so `resize`
/// must be sent ALONE and awaited before anything else goes out);
/// `resize_ok`/`resize_refused` promotes to DRIVING.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Watching,
    Taking,
    Resizing,
    Driving,
}

/// What [`TakeTransaction`] wants the caller (the Windows-only runtime)
/// to DO. Mirrors `attach_proto::Action`'s own shape: the transaction
/// itself never touches a socket — it only decides what the runtime
/// should send or show next. There is deliberately no `SendInput`
/// variant: flushing the queue is the runtime's own job, via
/// [`TakeTransaction::take_queued`], called only once it is safe to (see
/// that method's own doc) — folding it into an action returned eagerly
/// from `on_take_ok` is exactly the lockstep violation this redesign
/// fixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeAction {
    /// Send `take{controller_id}`.
    SendTake,
    /// On `take_ok`, resize FIRST and ALONE — "a watcher renders the
    /// driver's geometry and cannot correct it until it holds the pen,"
    /// and the wire's own lockstep rule permits exactly one outstanding
    /// request. The queue is released only after `resize_ok` (or its
    /// refusal) — see [`TakeTransaction::on_resize_ok`].
    SendResize { cols: u16, rows: u16 },
    /// The queue hit [`TAKE_QUEUE_CAP`] and further bytes are being
    /// dropped, or the `checkpoint_in_flight` retry window expired —
    /// either way, "discarded visibly, never delivered ... into a
    /// context that no longer exists." The caller surfaces this in the
    /// UI; it must never be a silent drop.
    QueueDiscarded,
    /// A non-empty input found the queue full and was dropped whole; the
    /// worker counts it in its one discard counter.
    InputDropped,
    /// `resize_refused{out_of_budget}`: "keeps the pen and reports the
    /// geometry unrepresentable." Promotes to [`Role::Driving`] anyway
    /// (the pen is still held; only the geometry failed) so the queue
    /// can still flush.
    GeometryUnrepresentable,
    /// `resize_refused{not_driver}`: "the pen is gone." Returns to
    /// [`Role::Watching`].
    PenLost,
    /// `take_refused{not_attached}`: "re-attaches first" — the caller
    /// ends the current episode and reconnects. The queue is discarded
    /// at the next attach (the count is [`TakeTransaction::
    /// reset_to_watching`]'s return), never delivered; only an input
    /// already in flight is resent, under its idempotency key.
    Reattach,
}

/// ADR 0041 ruling (b): "Take-on-first-input is a transaction." Owns
/// exactly the role and the queue; the epoch itself lives in
/// [`OutstandingSlot`] (ruling (c)) since it survives past this
/// transaction's own lifetime (every later keystroke while DRIVING).
#[derive(Debug)]
pub struct TakeTransaction {
    role: Role,
    queue: Vec<u8>,
    /// How many non-empty inputs went into `queue` and have not been
    /// taken or cleared.
    queued_inputs: usize,
    checkpoint_retry_started_at: Option<Instant>,
    /// Gates [`Self::tick_checkpoint_retry`]'s own `SendTake` to the
    /// pinned 250 ms cadence (Codex review round, finding 4: the
    /// original `tick` fired on EVERY call, including immediately after
    /// each fast refusal).
    next_retry_at: Option<Instant>,
    /// ADR 0042 amendment (2026-09-07): a HEADLESS client (the daemon's own
    /// `pty.input` attach) has no viewport and must never resize the pane
    /// — "the take's resize is the identity" is not good enough, because
    /// even sending an identity resize is a wire round trip a watcher-with-
    /// no-screen has no business making. `true` turns [`Self::on_take_ok`]'s
    /// own `SendResize` step into a no-op: take_ok promotes straight to
    /// [`Role::Driving`] (skipping [`Role::Resizing`] entirely) and returns
    /// no actions, so the wire `take{controller_id}` frame — already the
    /// ONLY thing `SendTake` ever encodes — is genuinely the last thing a
    /// headless take ever sends.
    headless: bool,
}

impl Default for TakeTransaction {
    fn default() -> Self {
        Self::new()
    }
}

impl TakeTransaction {
    pub fn new() -> Self {
        Self {
            role: Role::Watching,
            queue: Vec::new(),
            queued_inputs: 0,
            checkpoint_retry_started_at: None,
            next_retry_at: None,
            headless: false,
        }
    }

    /// A transaction for a headless client — see [`Self::headless`]'s own
    /// doc. Otherwise identical to [`Self::new`].
    pub fn new_headless() -> Self {
        Self { headless: true, ..Self::new() }
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// Appends `bytes` to the hold queue, capped at [`TAKE_QUEUE_CAP`].
    /// Returns the discard actions this call caused: none, or
    /// [`TakeAction::QueueDiscarded`] exactly once per discarding call
    /// (never once per dropped byte), plus [`TakeAction::InputDropped`]
    /// when a non-empty input found the queue full and was dropped whole.
    fn push_queue(&mut self, bytes: &[u8]) -> Vec<TakeAction> {
        let room = TAKE_QUEUE_CAP.saturating_sub(self.queue.len());
        let take = room.min(bytes.len());
        self.queue.extend_from_slice(&bytes[..take]);
        if take > 0 {
            self.queued_inputs += 1;
        }
        let mut actions = Vec::new();
        if take < bytes.len() {
            actions.push(TakeAction::QueueDiscarded);
            if take == 0 {
                actions.push(TakeAction::InputDropped);
            }
        }
        actions
    }

    /// The first input while WATCHING: enters TAKING, holds `bytes`, and
    /// asks the caller to send `take`.
    pub fn on_input_while_watching(&mut self, bytes: &[u8]) -> Vec<TakeAction> {
        debug_assert_eq!(self.role, Role::Watching);
        self.role = Role::Taking;
        self.checkpoint_retry_started_at = None;
        self.next_retry_at = None;
        let mut actions = vec![TakeAction::SendTake];
        actions.extend(self.push_queue(bytes));
        actions
    }

    /// Further keystrokes arriving before the pen is fully secured
    /// (TAKING or RESIZING) — appended to the same queue, discard
    /// reported the same way. No wire action: a `take`/`resize` is
    /// already outstanding.
    pub fn on_input_while_pending(&mut self, bytes: &[u8]) -> Vec<TakeAction> {
        debug_assert!(matches!(self.role, Role::Taking | Role::Resizing));
        self.push_queue(bytes)
    }

    /// Keystrokes arriving while DRIVING with an input ALREADY
    /// outstanding (ruling (c): one outstanding request at a time) —
    /// REUSES the same bounded queue (Codex review round, finding 5)
    /// rather than dropping them. No wire action; the caller flushes
    /// via [`Self::take_queued`] once the outstanding reply resolves.
    pub fn queue_while_driving(&mut self, bytes: &[u8]) -> Vec<TakeAction> {
        debug_assert_eq!(self.role, Role::Driving);
        self.push_queue(bytes)
    }

    /// `take_ok{take_epoch}`: RESIZING, and send `resize` ALONE — the
    /// queue is released only once [`Self::on_resize_ok`] runs. A
    /// [`Self::headless`] transaction skips RESIZING entirely: it promotes
    /// straight to DRIVING and sends no resize (`cols`/`rows` are simply
    /// unused in that case) — the caller must flush the queue itself once
    /// it sees [`Self::role`] already `Driving` after this call, since the
    /// ordinary `resize_ok` frame that would normally trigger that flush
    /// never arrives.
    pub fn on_take_ok(&mut self, cols: u16, rows: u16) -> Vec<TakeAction> {
        self.checkpoint_retry_started_at = None;
        self.next_retry_at = None;
        if self.headless {
            self.role = Role::Driving;
            return vec![];
        }
        self.role = Role::Resizing;
        vec![TakeAction::SendResize { cols, rows }]
    }

    /// `resize_ok`: DRIVING. The queue (if any) is released by a
    /// SEPARATE call to [`Self::take_queued`] — kept as two steps rather
    /// than one action-returning call so the runtime can record the
    /// idem key and mint the wire frame using ITS OWN clock/randomness,
    /// exactly the shape [`Self::take_queued`] already had for the
    /// steady-state DRIVING flush.
    pub fn on_resize_ok(&mut self) {
        debug_assert_eq!(self.role, Role::Resizing);
        self.role = Role::Driving;
    }

    /// Drains the queue for the caller to send as ONE input frame, once
    /// it is safe to: after `resize_ok` (or `resize_refused{out_of_
    /// budget}`, which still keeps the pen), or after a prior
    /// outstanding input resolved while still DRIVING. `None` if
    /// nothing is queued. Callable only while DRIVING — nothing may
    /// flush before the pen is fully secured.
    pub fn take_queued(&mut self) -> Option<Vec<u8>> {
        debug_assert_eq!(self.role, Role::Driving);
        if self.queue.is_empty() {
            None
        } else {
            self.queued_inputs = 0;
            Some(std::mem::take(&mut self.queue))
        }
    }

    /// `take_refused{not_attached}`: role and queue are left untouched
    /// here; the reattach that follows resets the transaction and counts
    /// the queue as discarded — see [`TakeAction::Reattach`]'s own doc.
    pub fn on_take_refused_not_attached(&mut self) -> Vec<TakeAction> {
        vec![TakeAction::Reattach]
    }

    /// `input_refused_stale` while DRIVING: "re-take first, then mint a
    /// new key under the new epoch" (ruling (c)) — the epoch is stale
    /// precisely because it is no longer current, so a fresh `take` is
    /// what learns the CURRENT one. TAKING, queue untouched (whatever
    /// was already queued behind the stale input stays queued).
    ///
    /// [`Self::headless`] SKIPS the re-take entirely ("each write happens
    /// at most once") — auto-recontesting here would move this
    /// transaction onto a new epoch the caller never asked for. Drops to
    /// WATCHING; the queue is discarded (a headless caller sends one
    /// input at a time and never leaves bytes queued across a refusal).
    pub fn retake_while_driving(&mut self) -> Vec<TakeAction> {
        debug_assert_eq!(self.role, Role::Driving);
        if self.headless {
            self.role = Role::Watching;
            self.clear_queue();
            return vec![];
        }
        self.role = Role::Taking;
        self.checkpoint_retry_started_at = None;
        self.next_retry_at = None;
        vec![TakeAction::SendTake]
    }

    /// `take_refused{checkpoint_in_flight}`: starts the 250ms-until-30s
    /// retry window. `now` is the time the refusal was observed; the
    /// first retry is scheduled 250 ms out, driven by
    /// [`Self::tick_checkpoint_retry`], never fired inline here (a
    /// refusal is not itself a retry).
    pub fn on_take_refused_checkpoint_in_flight(&mut self, now: Instant) -> Vec<TakeAction> {
        let started = *self.checkpoint_retry_started_at.get_or_insert(now);
        if now.duration_since(started) >= CHECKPOINT_IN_FLIGHT_BUDGET {
            self.clear_queue();
            self.checkpoint_retry_started_at = None;
            self.next_retry_at = None;
            self.role = Role::Watching;
            return vec![TakeAction::QueueDiscarded];
        }
        self.next_retry_at.get_or_insert(now + CHECKPOINT_IN_FLIGHT_RETRY);
        vec![]
    }

    /// Drives the 250ms retry cadence — call on every tick while
    /// [`Self::checkpoint_retry_pending`] is true. Gated by
    /// `next_retry_at`: a call before that instant is a no-op, so
    /// polling this every 100ms (the worker's own tick) does not resend
    /// `take` ten times a second.
    pub fn tick_checkpoint_retry(&mut self, now: Instant) -> Vec<TakeAction> {
        let Some(started) = self.checkpoint_retry_started_at else {
            return vec![];
        };
        if now.duration_since(started) >= CHECKPOINT_IN_FLIGHT_BUDGET {
            self.clear_queue();
            self.checkpoint_retry_started_at = None;
            self.next_retry_at = None;
            self.role = Role::Watching;
            return vec![TakeAction::QueueDiscarded];
        }
        let Some(next) = self.next_retry_at else {
            return vec![];
        };
        if now < next {
            return vec![];
        }
        self.next_retry_at = Some(now + CHECKPOINT_IN_FLIGHT_RETRY);
        vec![TakeAction::SendTake]
    }

    pub fn checkpoint_retry_pending(&self) -> bool {
        self.checkpoint_retry_started_at.is_some()
    }

    /// `resize_refused{..}` while RESIZING (the take-transaction's own
    /// resize) or DRIVING (an ad hoc later resize).
    pub fn on_resize_refused(&mut self, reason: ResizeRefusedReason) -> Vec<TakeAction> {
        match reason {
            ResizeRefusedReason::OutOfBudget => {
                // Keeps the pen: if this was the take-transaction's own
                // resize (RESIZING), the pen is still granted even
                // though the geometry failed, so promote to DRIVING —
                // the queue must still be able to flush.
                if self.role == Role::Resizing {
                    self.role = Role::Driving;
                }
                vec![TakeAction::GeometryUnrepresentable]
            }
            ResizeRefusedReason::NotDriver => {
                self.role = Role::Watching;
                self.clear_queue();
                vec![TakeAction::PenLost]
            }
        }
    }

    /// A fresh attach (or an ORDINARY reconnect) always arrives WATCHING —
    /// ADR 0037's who-may-type, restated by ruling (d). Returns how many
    /// queued inputs were dropped, for the caller to count.
    #[must_use]
    pub fn reset_to_watching(&mut self) -> usize {
        self.role = Role::Watching;
        let queued = self.queued_inputs;
        self.clear_queue();
        self.checkpoint_retry_started_at = None;
        self.next_retry_at = None;
        queued
    }

    fn clear_queue(&mut self) {
        self.queue.clear();
        self.queued_inputs = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- (b) TakeTransaction ------------------------------------------

    #[test]
    fn first_input_while_watching_sends_take_and_queues() {
        let mut t = TakeTransaction::new();
        let actions = t.on_input_while_watching(b"hello");
        assert_eq!(actions, vec![TakeAction::SendTake]);
        assert_eq!(t.role(), Role::Taking);
    }

    /// Codex review round, finding 3: `take_ok` must send ONLY resize —
    /// the queue is released ONLY after `resize_ok`, never bundled into
    /// the same action batch (the wire allows exactly one outstanding
    /// request; a bundled `SendInput` would violate lockstep the moment
    /// a real transport interleaves it before `resize`'s own reply).
    #[test]
    fn take_ok_sends_only_resize_never_input() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(b"ab");
        t.on_input_while_pending(b"cd");
        let actions = t.on_take_ok(80, 24);
        assert_eq!(actions, vec![TakeAction::SendResize { cols: 80, rows: 24 }]);
        assert_eq!(t.role(), Role::Resizing);
        // The queue is untouched -- take_queued is not callable yet
        // (still RESIZING), proving the bytes were not silently flushed.
    }

    /// ADR 0042 amendment (2026-09-07): a headless client's take sends NO
    /// resize, ever — `take_ok` promotes straight to DRIVING (skipping
    /// RESIZING) and returns no actions at all, so the wire `take` frame
    /// really is the last thing this transaction ever sends.
    #[test]
    fn headless_take_ok_sends_no_resize_and_promotes_straight_to_driving() {
        let mut t = TakeTransaction::new_headless();
        t.on_input_while_watching(b"ab");
        let actions = t.on_take_ok(80, 24);
        assert_eq!(actions, Vec::<TakeAction>::new());
        assert_eq!(t.role(), Role::Driving);
        // The queue is immediately drainable — no `resize_ok` needed.
        assert_eq!(t.take_queued(), Some(b"ab".to_vec()));
    }

    #[test]
    fn ordinary_take_ok_is_unaffected_by_the_headless_constructor_existing() {
        // Guards against the headless flag leaking into `new()`'s default.
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(b"ab");
        let actions = t.on_take_ok(80, 24);
        assert_eq!(actions, vec![TakeAction::SendResize { cols: 80, rows: 24 }]);
        assert_eq!(t.role(), Role::Resizing);
    }

    #[test]
    fn resize_ok_promotes_to_driving_and_releases_the_queue() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(b"ab");
        t.on_input_while_pending(b"cd");
        t.on_take_ok(80, 24);
        t.on_resize_ok();
        assert_eq!(t.role(), Role::Driving);
        assert_eq!(t.take_queued(), Some(b"abcd".to_vec()));
        // Draining twice returns None -- the queue is truly consumed.
        assert_eq!(t.take_queued(), None);
    }

    #[test]
    fn take_ok_with_empty_queue_then_resize_ok_yields_no_queued_input() {
        // A driver that never typed anything before take_ok.
        let mut t = TakeTransaction::new();
        t.role = Role::Taking; // synthesize: take sent with no queued bytes yet
        let actions = t.on_take_ok(10, 5);
        assert_eq!(actions, vec![TakeAction::SendResize { cols: 10, rows: 5 }]);
        t.on_resize_ok();
        assert_eq!(t.take_queued(), None);
    }

    #[test]
    fn queue_caps_at_8kib_and_discards_visibly() {
        let mut t = TakeTransaction::new();
        let paste = vec![b'x'; TAKE_QUEUE_CAP + 100];
        let actions = t.on_input_while_watching(&paste);
        assert!(actions.contains(&TakeAction::QueueDiscarded));
        t.on_take_ok(80, 24);
        t.on_resize_ok();
        let bytes = t.take_queued().expect("queue had bytes");
        assert_eq!(bytes.len(), TAKE_QUEUE_CAP);
    }

    #[test]
    fn an_input_dropped_whole_on_a_full_queue_is_reported() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(&vec![b'a'; TAKE_QUEUE_CAP]);
        let actions = t.on_input_while_pending(b"z");
        assert!(actions.contains(&TakeAction::InputDropped));
        let partly = TakeTransaction::new().on_input_while_watching(&vec![b'x'; TAKE_QUEUE_CAP + 1]);
        assert!(!partly.contains(&TakeAction::InputDropped), "a partly queued input is not a whole drop");
    }

    #[test]
    fn queue_accumulates_across_multiple_calls_up_to_the_cap() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(&vec![b'a'; TAKE_QUEUE_CAP - 10]);
        let overflow_actions = t.on_input_while_pending(&[b'b'; 50]);
        assert!(overflow_actions.contains(&TakeAction::QueueDiscarded));
        t.on_take_ok(80, 24);
        t.on_resize_ok();
        let bytes = t.take_queued().expect("queue had bytes");
        assert_eq!(bytes.len(), TAKE_QUEUE_CAP);
    }

    /// Codex review round, finding 5: driving input while a request is
    /// already outstanding must be queued, not dropped.
    #[test]
    fn driving_input_while_outstanding_is_queued_not_dropped() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(b"x");
        t.on_take_ok(1, 1);
        t.on_resize_ok();
        t.take_queued(); // flush the first byte "x" -- now nothing queued
        let actions = t.queue_while_driving(b"second keystroke");
        assert_eq!(actions, vec![]);
        assert_eq!(t.take_queued(), Some(b"second keystroke".to_vec()));
    }

    #[test]
    fn take_refused_not_attached_leaves_the_queue_to_be_counted_at_the_reset() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(b"x");
        t.on_input_while_pending(b"y");
        let actions = t.on_take_refused_not_attached();
        assert_eq!(actions, vec![TakeAction::Reattach]);
        assert_eq!(t.role(), Role::Taking);
        // The reattach resets the transaction and reports both queued inputs.
        assert_eq!(t.reset_to_watching(), 2);
        assert_eq!(t.role(), Role::Watching);
        assert_eq!(t.reset_to_watching(), 0);
    }

    /// Codex review round, finding 6: a stale-epoch refusal while
    /// DRIVING must re-take BEFORE minting a new key (the epoch is only
    /// known once the fresh `take_ok` arrives).
    #[test]
    fn retake_while_driving_transitions_to_taking() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(b"x");
        t.on_take_ok(1, 1);
        t.on_resize_ok();
        assert_eq!(t.role(), Role::Driving);
        let actions = t.retake_while_driving();
        assert_eq!(actions, vec![TakeAction::SendTake]);
        assert_eq!(t.role(), Role::Taking);
    }

    #[test]
    fn headless_retake_while_driving_never_re_takes() {
        let mut t = TakeTransaction::new_headless();
        t.on_input_while_watching(b"x");
        t.on_take_ok(1, 1); // headless: promotes straight to Driving
        assert_eq!(t.role(), Role::Driving);
        let actions = t.retake_while_driving();
        assert_eq!(actions, vec![], "a headless transaction never sends a fresh take on its own");
        assert_eq!(t.role(), Role::Watching);
    }

    #[test]
    fn checkpoint_in_flight_gated_at_250ms_then_expires_and_discards() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(b"typed-while-checkpoint-in-flight");
        let t0 = Instant::now();
        // First refusal opens the window; no immediate retry action.
        assert_eq!(t.on_take_refused_checkpoint_in_flight(t0), vec![]);
        assert!(t.checkpoint_retry_pending());
        // A tick well before the 250ms gate is a no-op -- Codex review
        // finding 4: the original fired on every call.
        assert_eq!(t.tick_checkpoint_retry(t0 + Duration::from_millis(50)), vec![]);
        assert_eq!(t.tick_checkpoint_retry(t0 + Duration::from_millis(250)), vec![TakeAction::SendTake]);
        // Immediately after that retry, another tick before the NEXT
        // 250ms boundary is again a no-op.
        assert_eq!(t.tick_checkpoint_retry(t0 + Duration::from_millis(300)), vec![]);
        assert_eq!(t.tick_checkpoint_retry(t0 + Duration::from_millis(500)), vec![TakeAction::SendTake]);
        assert_eq!(t.role(), Role::Taking);
        // At/after 30s the queue is discarded and role returns to Watching.
        let expiry = t0 + CHECKPOINT_IN_FLIGHT_BUDGET;
        assert_eq!(t.tick_checkpoint_retry(expiry), vec![TakeAction::QueueDiscarded]);
        assert_eq!(t.role(), Role::Watching);
        assert!(!t.checkpoint_retry_pending());
    }

    #[test]
    fn resize_refused_out_of_budget_keeps_the_pen_and_releases_the_queue() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(b"x");
        t.on_take_ok(80, 24);
        assert_eq!(t.role(), Role::Resizing);
        let actions = t.on_resize_refused(ResizeRefusedReason::OutOfBudget);
        assert_eq!(actions, vec![TakeAction::GeometryUnrepresentable]);
        assert_eq!(t.role(), Role::Driving);
        assert_eq!(t.take_queued(), Some(b"x".to_vec()));
    }

    #[test]
    fn resize_refused_not_driver_loses_the_pen() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(b"x");
        t.on_take_ok(80, 24);
        let actions = t.on_resize_refused(ResizeRefusedReason::NotDriver);
        assert_eq!(actions, vec![TakeAction::PenLost]);
        assert_eq!(t.role(), Role::Watching);
    }
}
