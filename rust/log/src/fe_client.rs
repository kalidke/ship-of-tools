//! ADR 0041 step 6 U3: the FE attach-only client's PURE state machines —
//! "the six FE rulings" from "Step 6 as specified", minus any OS call.
//! Portable (no `cfg(windows)`, no I/O): every rule here is a pure
//! function of an event, so it is genuinely exercised by `cargo test`
//! on every CI platform, not merely compile-checked on Windows — the
//! same split `attach_proto.rs` (the capsule's own Action-driven state
//! machine) and `classify.rs` (the probe classifier) already use. The
//! Windows-only runtime that wires these to a real `PipeClient` and a
//! real `vt100_ctt::Parser` lives in `fe_client_win.rs`; it is the ONLY
//! caller of anything here that also touches the OS.
//!
//! U3 SCOPE, pinned by "Step 6 units": "the six FE rulings in 'Step 6 as
//! specified', checkpoint restore into the drawer's parser, and deletion
//! of its DSR responder" — all "behind the same off-by-default flag" the
//! frontend calls `drawer.attach_only` (the ADR names none explicitly;
//! see `fe_client_win`'s own module doc for where it is read). Checkpoint
//! restore is `vt100_ctt::Parser::restore_screen` (already built, step 3)
//! — nothing to add here. "Deletion of its DSR responder": the attach-only
//! path never answers a DSR query at all — the capsule's own ConPTY DSR
//! responder (step 4) already resolves every query before a byte ever
//! reaches this client, so there is no responder here to delete FROM;
//! the deletion is that a naive port of `term::LocalTerminal`'s
//! `respond_to_queries` into this path was never written. `LocalTerminal`
//! itself is untouched — it remains the off-flag (default) code path,
//! and the ADR is explicit that "When off, NOTHING the FE does today
//! changes."
//!
//! Each ruling below is lettered to match the ADR's own list:
//! (a) [`QuitDispatcher`], (b) [`TakeTransaction`], (c) [`OutstandingSlot`],
//! (d) [`ReconnectState`], (e) [`legs_match`], (f) [`FeDownBaseline`] /
//! [`build_fe_down_marker`].

use crate::wire::{self, ResizeRefusedReason, SupervisorPhase};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------
// (b) Take-on-first-input is a transaction
// ---------------------------------------------------------------------

/// Bound on the transient hold queue a watcher fills while a `take` is in
/// flight. Pinned to [`wire::MAX_INPUT_PAYLOAD_LEN`] — not an
/// independently-chosen number — because the queue becomes, verbatim, the
/// single `input` frame sent the instant the pen is granted (ADR 0041
/// "Step 6 as specified": "hold the input in a bounded 8 KiB queue
/// (encoded bytes, one wire input's cap; a larger paste splits there and
/// the remainder is discarded visibly, never delivered minutes late into
/// a context that no longer exists)"). Two callers needing two different
/// numbers here would be the bug this constant exists to prevent.
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
/// first keystroke starts a `take` transaction; success promotes to
/// DRIVER.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Watching,
    Taking,
    Driving,
}

/// What [`TakeTransaction`] wants the caller (the Windows-only runtime)
/// to DO. Mirrors `attach_proto::Action`'s own shape: the transaction
/// itself never touches a socket — it only decides what the runtime
/// should send or show next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeAction {
    /// Send `take{controller_id}`.
    SendTake,
    /// On `take_ok`, resize FIRST — "a watcher renders the driver's
    /// geometry and cannot correct it until it holds the pen."
    SendResize { cols: u16, rows: u16 },
    /// The queue, drained as ONE `input` frame. The caller mints the
    /// idem key via [`OutstandingSlot::record`] and sends it.
    SendInput { bytes: Vec<u8> },
    /// The queue hit [`TAKE_QUEUE_CAP`] and further bytes are being
    /// dropped, or the `checkpoint_in_flight` retry window expired —
    /// either way, "discarded visibly, never delivered ... into a
    /// context that no longer exists." The caller surfaces this in the
    /// UI; it must never be a silent drop.
    QueueDiscarded,
    /// `resize_refused{out_of_budget}`: "keeps the pen and reports the
    /// geometry unrepresentable." Stays [`Role::Driving`].
    GeometryUnrepresentable,
    /// `resize_refused{not_driver}`: "the pen is gone." Returns to
    /// [`Role::Watching`].
    PenLost,
    /// `take_refused{not_attached}`: "re-attaches first" — the caller
    /// drives a fresh attach (through the reconnect machinery) and,
    /// once attached again, calls [`TakeTransaction::retry_take`] to
    /// re-issue `take` for the SAME still-queued bytes.
    Reattach,
}

/// ADR 0041 ruling (b): "Take-on-first-input is a transaction." Owns
/// exactly the queue and the role; the epoch itself lives in
/// [`OutstandingSlot`] (ruling (c)) since it survives past this
/// transaction's own lifetime (every later keystroke while DRIVING).
#[derive(Debug)]
pub struct TakeTransaction {
    role: Role,
    queue: Vec<u8>,
    checkpoint_retry_started_at: Option<Instant>,
}

impl Default for TakeTransaction {
    fn default() -> Self {
        Self::new()
    }
}

impl TakeTransaction {
    pub fn new() -> Self {
        Self { role: Role::Watching, queue: Vec::new(), checkpoint_retry_started_at: None }
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// Appends `bytes` to the hold queue, capped at [`TAKE_QUEUE_CAP`].
    /// Returns `true` iff this call caused bytes to be discarded (the
    /// caller surfaces [`TakeAction::QueueDiscarded`] exactly once per
    /// discarding call, never once per dropped byte).
    fn push_queue(&mut self, bytes: &[u8]) -> bool {
        let room = TAKE_QUEUE_CAP.saturating_sub(self.queue.len());
        let take = room.min(bytes.len());
        self.queue.extend_from_slice(&bytes[..take]);
        take < bytes.len()
    }

    /// The first input while WATCHING: enters TAKING, holds `bytes`, and
    /// asks the caller to send `take`.
    pub fn on_input_while_watching(&mut self, bytes: &[u8]) -> Vec<TakeAction> {
        debug_assert_eq!(self.role, Role::Watching);
        self.role = Role::Taking;
        self.checkpoint_retry_started_at = None;
        let mut actions = vec![TakeAction::SendTake];
        if self.push_queue(bytes) {
            actions.push(TakeAction::QueueDiscarded);
        }
        actions
    }

    /// Further keystrokes arriving while still TAKING — appended to the
    /// same queue, discard reported the same way.
    pub fn on_input_while_taking(&mut self, bytes: &[u8]) -> Vec<TakeAction> {
        debug_assert_eq!(self.role, Role::Taking);
        if self.push_queue(bytes) {
            vec![TakeAction::QueueDiscarded]
        } else {
            vec![]
        }
    }

    /// `take_ok{take_epoch}`: resize the current viewport FIRST, then
    /// flush the queue as one `input` (if nonempty), then DRIVING.
    pub fn on_take_ok(&mut self, cols: u16, rows: u16) -> Vec<TakeAction> {
        self.role = Role::Driving;
        self.checkpoint_retry_started_at = None;
        let mut actions = vec![TakeAction::SendResize { cols, rows }];
        if !self.queue.is_empty() {
            actions.push(TakeAction::SendInput { bytes: std::mem::take(&mut self.queue) });
        }
        actions
    }

    /// `take_refused{not_attached}`.
    pub fn on_take_refused_not_attached(&mut self) -> Vec<TakeAction> {
        vec![TakeAction::Reattach]
    }

    /// Re-issue `take` for the still-queued bytes after a
    /// `not_attached`-triggered reattach completed.
    pub fn retry_take(&mut self) -> Vec<TakeAction> {
        debug_assert_eq!(self.role, Role::Taking);
        vec![TakeAction::SendTake]
    }

    /// `take_refused{checkpoint_in_flight}`: starts (or continues) the
    /// 250ms-until-30s retry window. `now` is the time the refusal was
    /// observed.
    pub fn on_take_refused_checkpoint_in_flight(&mut self, now: Instant) -> Vec<TakeAction> {
        let started = *self.checkpoint_retry_started_at.get_or_insert(now);
        if now.duration_since(started) >= CHECKPOINT_IN_FLIGHT_BUDGET {
            self.queue.clear();
            self.checkpoint_retry_started_at = None;
            self.role = Role::Watching;
            vec![TakeAction::QueueDiscarded]
        } else {
            vec![]
        }
    }

    /// Drives the 250ms retry cadence — the caller schedules this on a
    /// timer while [`Self::on_take_refused_checkpoint_in_flight`]'s
    /// window is open (`checkpoint_retry_started_at.is_some()`).
    pub fn tick_checkpoint_retry(&mut self, now: Instant) -> Vec<TakeAction> {
        let Some(started) = self.checkpoint_retry_started_at else {
            return vec![];
        };
        if now.duration_since(started) >= CHECKPOINT_IN_FLIGHT_BUDGET {
            self.queue.clear();
            self.checkpoint_retry_started_at = None;
            self.role = Role::Watching;
            return vec![TakeAction::QueueDiscarded];
        }
        vec![TakeAction::SendTake]
    }

    pub fn checkpoint_retry_pending(&self) -> bool {
        self.checkpoint_retry_started_at.is_some()
    }

    /// `resize_refused{..}` while DRIVING.
    pub fn on_resize_refused(&mut self, reason: ResizeRefusedReason) -> Vec<TakeAction> {
        match reason {
            ResizeRefusedReason::OutOfBudget => vec![TakeAction::GeometryUnrepresentable],
            ResizeRefusedReason::NotDriver => {
                self.role = Role::Watching;
                self.queue.clear();
                vec![TakeAction::PenLost]
            }
        }
    }

    /// A fresh attach (or reconnect) always arrives WATCHING — ADR 0037's
    /// who-may-type, restated by ruling (d).
    pub fn reset_to_watching(&mut self) {
        self.role = Role::Watching;
        self.queue.clear();
        self.checkpoint_retry_started_at = None;
    }
}

// ---------------------------------------------------------------------
// (c) Outstanding input survives reconnect, exactly once, within one voyage
// ---------------------------------------------------------------------

/// The exact tuple the ADR pins: `(voyage_uuid, idem_key, take_epoch,
/// bytes)` — at most one outstanding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutstandingInput {
    pub voyage_uuid: String,
    pub idem_key: [u8; 16],
    pub take_epoch: u64,
    pub bytes: Vec<u8>,
}

/// The wire's three terminal answers to an `input` frame (ADR 0041:
/// "the wire ... defines three terminal answers").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputWireOutcome {
    Recorded,
    DeliveryUnknown,
    RefusedStale,
}

/// What applying an outcome resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutstandingResolution {
    /// `input_recorded`: completes.
    Completed,
    /// `input_delivery_unknown`: "never auto-retried ... dropped and
    /// marked visibly unknown."
    Unknown,
    /// `input_refused_stale`: "re-sent under the new epoch with a NEW
    /// key — the old chain is closed by the refusal."
    RetryNewEpoch { idem_key: [u8; 16] },
    /// Nothing was outstanding — a caller applying a stray reply (should
    /// not happen in a correct driver; kept explicit rather than
    /// panicking so a protocol surprise degrades to a no-op instead of a
    /// crash).
    NothingOutstanding,
}

/// What a reconnect's own re-take should do with whatever was
/// outstanding when the connection dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconnectResendDecision {
    /// "Resends THE SAME KEY" under the freshly re-taken epoch.
    Resend { idem_key: [u8; 16] },
    /// The voyage UUID changed: "CANCELED and marked unknown, never
    /// replayed and never re-keyed."
    Cancel,
    /// Nothing was outstanding.
    None,
}

/// At most one outstanding `input`, ADR-tracked across reconnects within
/// one voyage.
#[derive(Debug, Default)]
pub struct OutstandingSlot(Option<OutstandingInput>);

impl OutstandingSlot {
    pub fn new() -> Self {
        Self(None)
    }

    pub fn outstanding(&self) -> Option<&OutstandingInput> {
        self.0.as_ref()
    }

    /// Records a fresh outstanding input (the take-transaction's flush,
    /// or ordinary steady-state typing while DRIVING). `mint_key` is
    /// injected so tests can pin the key; production callers pass a
    /// `getrandom`-backed closure.
    pub fn record(
        &mut self,
        voyage_uuid: String,
        take_epoch: u64,
        bytes: Vec<u8>,
        mint_key: impl FnOnce() -> [u8; 16],
    ) -> [u8; 16] {
        let idem_key = mint_key();
        self.0 = Some(OutstandingInput { voyage_uuid, idem_key, take_epoch, bytes });
        idem_key
    }

    /// Applies one of the wire's three terminal answers to whatever is
    /// currently outstanding.
    pub fn apply_outcome(
        &mut self,
        outcome: InputWireOutcome,
        new_epoch: u64,
        mint_key: impl FnOnce() -> [u8; 16],
    ) -> OutstandingResolution {
        let Some(o) = self.0.as_mut() else {
            return OutstandingResolution::NothingOutstanding;
        };
        match outcome {
            InputWireOutcome::Recorded => {
                self.0 = None;
                OutstandingResolution::Completed
            }
            InputWireOutcome::DeliveryUnknown => {
                self.0 = None;
                OutstandingResolution::Unknown
            }
            InputWireOutcome::RefusedStale => {
                let idem_key = mint_key();
                o.idem_key = idem_key;
                o.take_epoch = new_epoch;
                OutstandingResolution::RetryNewEpoch { idem_key }
            }
        }
    }

    /// After a reconnect re-attaches and re-takes: resend under the SAME
    /// key within the same voyage; cancel across a voyage change.
    pub fn resend_after_reconnect(
        &mut self,
        new_voyage_uuid: &str,
        new_take_epoch: u64,
    ) -> ReconnectResendDecision {
        let Some(o) = self.0.as_mut() else {
            return ReconnectResendDecision::None;
        };
        if o.voyage_uuid != new_voyage_uuid {
            self.0 = None;
            return ReconnectResendDecision::Cancel;
        }
        o.take_epoch = new_take_epoch;
        ReconnectResendDecision::Resend { idem_key: o.idem_key }
    }

    /// A quit or cancel with a key outstanding "reports it rather than
    /// dropping it silently" — the caller surfaces the returned value,
    /// never discards it unseen.
    pub fn cancel_for_quit(&mut self) -> Option<OutstandingInput> {
        self.0.take()
    }
}

// ---------------------------------------------------------------------
// (d) Reconnect is bounded, classified, and re-reads the pointer
// ---------------------------------------------------------------------

/// `disconnected → connecting → hello → attaching → watching/driving`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Episode {
    Disconnected,
    Connecting,
    Hello,
    Attaching,
    Watching,
    Driving,
}

/// Why a reconnect episode is TERMINAL — an actionable error offering
/// retry and reset, never silently retried again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalReason {
    HelloRefusedVersionSkew,
    ForeignPipe,
    AccessDenied,
    PointerAbsentOrCorrupt,
    OperatorCancel,
    /// A still-answering authority's own terminal phase.
    SupervisorPhase(SupervisorPhase),
    /// The voyage pipe is absent while the supervisor lane is absent OR
    /// unresponsive, sustained for the whole [`HEALTH_WINDOW`].
    HealthWindowExpired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconnectDecision {
    Retry,
    Terminal(TerminalReason),
}

pub const RECONNECT_BACKOFF_INITIAL: Duration = Duration::from_millis(250);
pub const RECONNECT_BACKOFF_CAP: Duration = Duration::from_secs(4);

/// The bound above which "the voyage pipe absent while [the supervisor]
/// lane is absent OR UNRESPONSIVE" gives up (ADR 0041 Lifecycle
/// "Reconnect is bounded, classified..."). Pinned to the same 120 s
/// figure the ADR's own "Upgrade and version skew" section names as THE
/// health window ("readiness + stability, 120 s at today's provisional
/// values") — a distinct instance of the same named concept, not
/// `supervisor.rs`'s private `READINESS_CUTOFF + STABILITY_INTERVAL`
/// (that pair governs the LAUNCHER's rollback decision, a different
/// authority, and is not `pub`). Re-derive both together if the
/// provisional value ever changes.
pub const HEALTH_WINDOW: Duration = Duration::from_secs(120);

/// `backoff 250 ms doubling to a 4 s cap`.
pub fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(RECONNECT_BACKOFF_CAP)
}

/// The reconnect episode's own classifier state: which episode phase,
/// the current backoff, and how long the "pipe absent, lane
/// absent-or-unresponsive" condition has been continuously observed.
#[derive(Debug)]
pub struct ReconnectState {
    episode: Episode,
    backoff: Duration,
    unresponsive_since: Option<Instant>,
}

impl Default for ReconnectState {
    fn default() -> Self {
        Self::new()
    }
}

impl ReconnectState {
    pub fn new() -> Self {
        Self { episode: Episode::Disconnected, backoff: RECONNECT_BACKOFF_INITIAL, unresponsive_since: None }
    }

    pub fn episode(&self) -> Episode {
        self.episode
    }

    /// Every episode starts here — `drawer.voyage` is re-read and
    /// re-validated at this exact point, never against a cached UUID.
    pub fn begin_episode(&mut self) {
        self.episode = Episode::Connecting;
    }

    pub fn enter_hello(&mut self) {
        self.episode = Episode::Hello;
    }
    pub fn enter_attaching(&mut self) {
        self.episode = Episode::Attaching;
    }
    pub fn enter_watching(&mut self) {
        self.episode = Episode::Watching;
        self.unresponsive_since = None;
        self.reset_backoff();
    }
    pub fn enter_driving(&mut self) {
        self.episode = Episode::Driving;
    }
    pub fn drop_connection(&mut self) {
        self.episode = Episode::Disconnected;
    }

    pub fn classify_hello_refused_version_skew(&mut self) -> ReconnectDecision {
        ReconnectDecision::Terminal(TerminalReason::HelloRefusedVersionSkew)
    }
    pub fn classify_foreign(&mut self) -> ReconnectDecision {
        ReconnectDecision::Terminal(TerminalReason::ForeignPipe)
    }
    pub fn classify_access_denied(&mut self) -> ReconnectDecision {
        ReconnectDecision::Terminal(TerminalReason::AccessDenied)
    }
    pub fn classify_pointer_bad(&mut self) -> ReconnectDecision {
        ReconnectDecision::Terminal(TerminalReason::PointerAbsentOrCorrupt)
    }
    pub fn classify_operator_cancel(&mut self) -> ReconnectDecision {
        ReconnectDecision::Terminal(TerminalReason::OperatorCancel)
    }

    /// A STILL-ANSWERING supervisor lane reporting its own terminal
    /// phase — visible immediately, no timeout needed (the phase itself
    /// IS the proof).
    pub fn classify_supervisor_phase(&mut self, phase: SupervisorPhase) -> ReconnectDecision {
        match phase {
            SupervisorPhase::EndedNoRespawn | SupervisorPhase::Terminal => {
                ReconnectDecision::Terminal(TerminalReason::SupervisorPhase(phase))
            }
            _ => ReconnectDecision::Retry,
        }
    }

    /// The voyage pipe is absent AND the supervisor lane is absent or
    /// unresponsive — the ONE case that needs the timeout, since neither
    /// side can prove a terminal fact. `now` is checked against the
    /// FIRST time this condition was observed continuously.
    pub fn classify_unresponsive(&mut self, now: Instant) -> ReconnectDecision {
        let since = *self.unresponsive_since.get_or_insert(now);
        if now.duration_since(since) >= HEALTH_WINDOW {
            ReconnectDecision::Terminal(TerminalReason::HealthWindowExpired)
        } else {
            ReconnectDecision::Retry
        }
    }

    /// The unresponsive condition resolved (a `status` answered again,
    /// or the voyage pipe reappeared) — clears the clock so a LATER
    /// unresponsive spell starts its own fresh window rather than
    /// inheriting an old one.
    pub fn clear_unresponsive(&mut self) {
        self.unresponsive_since = None;
    }

    /// Everything else retries. Returns the backoff to wait before the
    /// next attempt and advances it (250ms doubling to 4s).
    pub fn retry_with_backoff(&mut self) -> Duration {
        self.drop_connection();
        let wait = self.backoff;
        self.backoff = next_backoff(self.backoff);
        wait
    }

    pub fn reset_backoff(&mut self) {
        self.backoff = RECONNECT_BACKOFF_INITIAL;
    }
}

// ---------------------------------------------------------------------
// (e) The attach notice is bound to the leg it describes
// ---------------------------------------------------------------------

/// `true` iff the mgmt-lane-observed leg identity and the attach-
/// connection-observed leg identity name the SAME process leg —
/// `(pid, creation-time bits)` compared on both. On a mismatch the
/// caller re-reads `status` rather than rendering the notice at all
/// ("a leg dying between them would let the FE render leg A's start
/// time over leg B's restored screen").
pub fn legs_match(mgmt: (u32, u64), attach: (u32, u64)) -> bool {
    mgmt.0 == attach.0 && mgmt.1 == attach.1
}

/// The one truthful attach-notice message, given the confirmed leg's own
/// creation time formatted by the caller (this module carries no clock
/// formatting opinion — the runtime supplies an already-rendered
/// timestamp string).
pub fn attach_notice_text(leg_started_at: &str) -> String {
    format!("attached to leg started {leg_started_at}")
}

// ---------------------------------------------------------------------
// (f) `fe_down` claims only what it can observe
// ---------------------------------------------------------------------

/// Reads the `ts` field of the LAST well-formed JSON line in `contents`
/// (an already-loaded `fe-inbox.jsonl`) — "last inbox evidence." `None`
/// when the file is empty or its last line is not a JSON object with a
/// string `ts` field: an unusable trailing line makes no evidence claim,
/// rather than reaching further back and claiming an OLDER line is the
/// most recent evidence (which would be false).
pub fn last_evidence_ts(contents: &str) -> Option<String> {
    let last = contents.lines().rev().find(|l| !l.trim().is_empty())?;
    let v: serde_json::Value = serde_json::from_str(last.trim()).ok()?;
    v.get("ts").and_then(|t| t.as_str()).map(|s| s.to_string())
}

/// The exact JSON shape ADR 0041 pins for the marker line:
/// `{"from":"sot-fe","to":"<handle>","text":"possible relay gap: last
/// inbox evidence <t0>, frontend reattached <t1>","ts":"<t1>",
/// "kind":"fe_down","window":{"last_evidence":"<t0>"}}`. `to` is the
/// durable inbox's own addressee handle; `last_evidence`/`reattach_ts`
/// are both ISO-8601 strings, carried verbatim (this function does no
/// time parsing or formatting of its own).
pub fn build_fe_down_marker(to: &str, last_evidence: &str, reattach_ts: &str) -> serde_json::Value {
    serde_json::json!({
        "from": "sot-fe",
        "to": to,
        "text": format!(
            "possible relay gap: last inbox evidence {last_evidence}, frontend reattached {reattach_ts}"
        ),
        "ts": reattach_ts,
        "kind": "fe_down",
        "window": { "last_evidence": last_evidence },
    })
}

/// `t0`, read at FE PROCESS START before this run appends anything —
/// "No baseline, no marker." Also tracks whether this process has
/// already made its first attach ("skipped on a first attach": the very
/// first attach of a process's life never emits a marker, since a
/// process that has not yet attached even once cannot itself have
/// missed traffic during ITS OWN prior downtime — the gap the marker
/// reports is always relative to a PRIOR attach).
#[derive(Debug, Clone)]
pub struct FeDownBaseline {
    last_evidence: Option<String>,
    first_attach_done: bool,
}

impl FeDownBaseline {
    /// `last_evidence`: the result of [`last_evidence_ts`] over
    /// `fe-inbox.jsonl`'s content as read at process start, before this
    /// run's own first append.
    pub fn capture(last_evidence: Option<String>) -> Self {
        Self { last_evidence, first_attach_done: false }
    }

    /// Called on every successful attach. Returns the marker to append,
    /// or `None` when no marker should be written this time (no
    /// baseline, or this is the process's first attach).
    pub fn marker_for_attach(&mut self, to: &str, reattach_ts: &str) -> Option<serde_json::Value> {
        let first = !self.first_attach_done;
        self.first_attach_done = true;
        if first {
            return None;
        }
        let t0 = self.last_evidence.as_deref()?;
        Some(build_fe_down_marker(to, t0, reattach_ts))
    }
}

// ---------------------------------------------------------------------
// (a) One quit dispatcher, waiting for `record_closed`
// ---------------------------------------------------------------------

/// Bounds how long the "ending session" window waits for `record_closed`
/// before switching to "outcome unknown" (ADR 0041: "on the FE QUIT
/// cutoff"). Pinned to the supervisor lane's own reply-read budget —
/// Lifecycle "Every op has one budget: connect 2 s, request write 2 s,
/// reply read 5 s" — since `end_run`'s own reply IS a lane reply and the
/// ADR names no separate cutoff value for this specific wait.
pub const QUIT_CUTOFF: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuitState {
    Idle,
    /// Holding the window open, waiting for `record_closed`.
    Ending { operation_id: String },
    /// `record_closed` arrived — the caller may now actually exit.
    Ended,
    /// The cutoff expired first: "the window STAYS UP and says 'ending
    /// the session did not complete — outcome unknown'."
    OutcomeUnknown,
}

/// ADR 0041 ruling (a): every user-requested exit routes through exactly
/// one of these, latched so a second quit press cannot fire a second
/// `end_run` while one is already outstanding.
#[derive(Debug)]
pub struct QuitDispatcher {
    state: QuitState,
    started_at: Option<Instant>,
}

impl Default for QuitDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl QuitDispatcher {
    pub fn new() -> Self {
        Self { state: QuitState::Idle, started_at: None }
    }

    pub fn state(&self) -> &QuitState {
        &self.state
    }

    /// Starts the ending transaction. Returns `true` iff THIS call is
    /// the one that should send `end_run` (idempotent against repeated
    /// quit presses while already ending/ended/unknown).
    pub fn request_quit(&mut self, operation_id: String, now: Instant) -> bool {
        if matches!(self.state, QuitState::Idle) {
            self.state = QuitState::Ending { operation_id };
            self.started_at = Some(now);
            true
        } else {
            false
        }
    }

    /// `record_closed` arrived on the supervisor lane for the operation
    /// this dispatcher is waiting on.
    pub fn on_record_closed(&mut self) {
        if matches!(self.state, QuitState::Ending { .. }) {
            self.state = QuitState::Ended;
        }
    }

    /// Advances the cutoff clock; call once per tick while `Ending`.
    pub fn tick(&mut self, now: Instant) {
        if let (QuitState::Ending { .. }, Some(started)) = (&self.state, self.started_at) {
            if now.duration_since(started) >= QUIT_CUTOFF {
                self.state = QuitState::OutcomeUnknown;
            }
        }
    }

    pub fn should_exit(&self) -> bool {
        matches!(self.state, QuitState::Ended)
    }

    /// The visible window message while ending or after a timed-out
    /// outcome; `None` once idle/ended (nothing to show).
    pub fn message(&self) -> Option<&'static str> {
        match self.state {
            QuitState::Ending { .. } => Some("ending session\u{2026}"),
            QuitState::OutcomeUnknown => {
                Some("ending the session did not complete \u{2014} outcome unknown")
            }
            QuitState::Idle | QuitState::Ended => None,
        }
    }
}

// =======================================================================
// Tests
// =======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> [u8; 16] {
        [b; 16]
    }

    // ---- (b) TakeTransaction ------------------------------------------

    #[test]
    fn first_input_while_watching_sends_take_and_queues() {
        let mut t = TakeTransaction::new();
        let actions = t.on_input_while_watching(b"hello");
        assert_eq!(actions, vec![TakeAction::SendTake]);
        assert_eq!(t.role(), Role::Taking);
    }

    #[test]
    fn take_ok_resizes_before_flushing_queued_input() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(b"ab");
        t.on_input_while_taking(b"cd");
        let actions = t.on_take_ok(80, 24);
        assert_eq!(
            actions,
            vec![
                TakeAction::SendResize { cols: 80, rows: 24 },
                TakeAction::SendInput { bytes: b"abcd".to_vec() },
            ]
        );
        assert_eq!(t.role(), Role::Driving);
    }

    #[test]
    fn take_ok_with_empty_queue_only_resizes() {
        // Reattaching a driver that never typed anything before take_ok.
        let mut t = TakeTransaction::new();
        t.role = Role::Taking; // synthesize: take sent with no queued bytes yet
        let actions = t.on_take_ok(10, 5);
        assert_eq!(actions, vec![TakeAction::SendResize { cols: 10, rows: 5 }]);
    }

    #[test]
    fn queue_caps_at_8kib_and_discards_visibly() {
        let mut t = TakeTransaction::new();
        let paste = vec![b'x'; TAKE_QUEUE_CAP + 100];
        let actions = t.on_input_while_watching(&paste);
        assert!(actions.contains(&TakeAction::QueueDiscarded));
        let take_ok = t.on_take_ok(80, 24);
        let TakeAction::SendInput { bytes } = &take_ok[1] else {
            panic!("expected SendInput as the second action");
        };
        assert_eq!(bytes.len(), TAKE_QUEUE_CAP);
    }

    #[test]
    fn queue_accumulates_across_multiple_calls_up_to_the_cap() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(&vec![b'a'; TAKE_QUEUE_CAP - 10]);
        let overflow_actions = t.on_input_while_taking(&[b'b'; 50]);
        assert!(overflow_actions.contains(&TakeAction::QueueDiscarded));
        let take_ok = t.on_take_ok(80, 24);
        let TakeAction::SendInput { bytes } = &take_ok[1] else {
            panic!("expected SendInput");
        };
        assert_eq!(bytes.len(), TAKE_QUEUE_CAP);
    }

    #[test]
    fn take_refused_not_attached_asks_to_reattach() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(b"x");
        let actions = t.on_take_refused_not_attached();
        assert_eq!(actions, vec![TakeAction::Reattach]);
        // Queue survives -- retry_take re-sends `take` for the same bytes.
        assert_eq!(t.retry_take(), vec![TakeAction::SendTake]);
        let take_ok = t.on_take_ok(1, 1);
        let TakeAction::SendInput { bytes } = &take_ok[1] else {
            panic!("expected SendInput");
        };
        assert_eq!(bytes, b"x");
    }

    #[test]
    fn checkpoint_in_flight_retries_then_expires_and_discards() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(b"typed-while-checkpoint-in-flight");
        let t0 = Instant::now();
        // First refusal opens the window; well within 30s -> keep retrying.
        assert_eq!(t.on_take_refused_checkpoint_in_flight(t0), vec![]);
        assert!(t.checkpoint_retry_pending());
        let mid = t0 + Duration::from_secs(10);
        assert_eq!(t.tick_checkpoint_retry(mid), vec![TakeAction::SendTake]);
        assert_eq!(t.role(), Role::Taking);
        // At/after 30s the queue is discarded and role returns to Watching.
        let expiry = t0 + CHECKPOINT_IN_FLIGHT_BUDGET;
        assert_eq!(t.tick_checkpoint_retry(expiry), vec![TakeAction::QueueDiscarded]);
        assert_eq!(t.role(), Role::Watching);
        assert!(!t.checkpoint_retry_pending());
    }

    #[test]
    fn resize_refused_out_of_budget_keeps_the_pen() {
        let mut t = TakeTransaction::new();
        t.on_input_while_watching(b"x");
        t.on_take_ok(80, 24);
        assert_eq!(t.role(), Role::Driving);
        let actions = t.on_resize_refused(ResizeRefusedReason::OutOfBudget);
        assert_eq!(actions, vec![TakeAction::GeometryUnrepresentable]);
        assert_eq!(t.role(), Role::Driving);
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

    // ---- (c) OutstandingSlot -------------------------------------------

    #[test]
    fn record_then_recorded_completes() {
        let mut slot = OutstandingSlot::new();
        let k = slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
        assert_eq!(k, key(1));
        assert!(slot.outstanding().is_some());
        let res = slot.apply_outcome(InputWireOutcome::Recorded, 1, || key(2));
        assert_eq!(res, OutstandingResolution::Completed);
        assert!(slot.outstanding().is_none());
    }

    #[test]
    fn delivery_unknown_is_never_auto_retried() {
        let mut slot = OutstandingSlot::new();
        slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
        let res = slot.apply_outcome(InputWireOutcome::DeliveryUnknown, 1, || key(2));
        assert_eq!(res, OutstandingResolution::Unknown);
        assert!(slot.outstanding().is_none());
    }

    #[test]
    fn refused_stale_mints_a_new_key_under_the_new_epoch() {
        let mut slot = OutstandingSlot::new();
        slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
        let res = slot.apply_outcome(InputWireOutcome::RefusedStale, 7, || key(2));
        assert_eq!(res, OutstandingResolution::RetryNewEpoch { idem_key: key(2) });
        let o = slot.outstanding().unwrap();
        assert_eq!(o.idem_key, key(2));
        assert_eq!(o.take_epoch, 7);
    }

    #[test]
    fn reconnect_within_the_same_voyage_resends_the_same_key() {
        let mut slot = OutstandingSlot::new();
        slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
        let decision = slot.resend_after_reconnect("v1", 9);
        assert_eq!(decision, ReconnectResendDecision::Resend { idem_key: key(1) });
        assert_eq!(slot.outstanding().unwrap().take_epoch, 9);
        assert_eq!(slot.outstanding().unwrap().idem_key, key(1));
    }

    #[test]
    fn reconnect_across_a_voyage_change_cancels_never_replays() {
        let mut slot = OutstandingSlot::new();
        slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
        let decision = slot.resend_after_reconnect("v2", 1);
        assert_eq!(decision, ReconnectResendDecision::Cancel);
        assert!(slot.outstanding().is_none());
    }

    #[test]
    fn quit_with_outstanding_reports_it_rather_than_dropping_silently() {
        let mut slot = OutstandingSlot::new();
        slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
        let reported = slot.cancel_for_quit();
        assert_eq!(reported.unwrap().bytes, b"hi");
        assert!(slot.outstanding().is_none());
    }

    #[test]
    fn apply_outcome_with_nothing_outstanding_is_a_harmless_no_op() {
        let mut slot = OutstandingSlot::new();
        let res = slot.apply_outcome(InputWireOutcome::Recorded, 1, || key(1));
        assert_eq!(res, OutstandingResolution::NothingOutstanding);
    }

    // ---- (d) ReconnectState ---------------------------------------------

    #[test]
    fn episode_moves_through_the_pinned_sequence() {
        let mut r = ReconnectState::new();
        assert_eq!(r.episode(), Episode::Disconnected);
        r.begin_episode();
        assert_eq!(r.episode(), Episode::Connecting);
        r.enter_hello();
        r.enter_attaching();
        r.enter_watching();
        assert_eq!(r.episode(), Episode::Watching);
        r.enter_driving();
        assert_eq!(r.episode(), Episode::Driving);
    }

    #[test]
    fn backoff_doubles_and_caps_at_4s() {
        let mut r = ReconnectState::new();
        let mut waits = vec![];
        for _ in 0..6 {
            waits.push(r.retry_with_backoff());
        }
        assert_eq!(
            waits,
            vec![
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(4),
            ]
        );
    }

    #[test]
    fn watching_resets_backoff() {
        let mut r = ReconnectState::new();
        r.retry_with_backoff();
        r.retry_with_backoff();
        r.enter_watching();
        assert_eq!(r.retry_with_backoff(), Duration::from_millis(250));
    }

    #[test]
    fn terminal_cases_are_immediately_terminal() {
        let mut r = ReconnectState::new();
        assert_eq!(
            r.classify_hello_refused_version_skew(),
            ReconnectDecision::Terminal(TerminalReason::HelloRefusedVersionSkew)
        );
        assert_eq!(r.classify_foreign(), ReconnectDecision::Terminal(TerminalReason::ForeignPipe));
        assert_eq!(
            r.classify_access_denied(),
            ReconnectDecision::Terminal(TerminalReason::AccessDenied)
        );
        assert_eq!(
            r.classify_pointer_bad(),
            ReconnectDecision::Terminal(TerminalReason::PointerAbsentOrCorrupt)
        );
        assert_eq!(
            r.classify_operator_cancel(),
            ReconnectDecision::Terminal(TerminalReason::OperatorCancel)
        );
    }

    #[test]
    fn supervisor_ended_no_respawn_and_terminal_are_terminal_ready_is_not() {
        let mut r = ReconnectState::new();
        assert_eq!(
            r.classify_supervisor_phase(SupervisorPhase::EndedNoRespawn),
            ReconnectDecision::Terminal(TerminalReason::SupervisorPhase(SupervisorPhase::EndedNoRespawn))
        );
        assert_eq!(
            r.classify_supervisor_phase(SupervisorPhase::Terminal),
            ReconnectDecision::Terminal(TerminalReason::SupervisorPhase(SupervisorPhase::Terminal))
        );
        assert_eq!(r.classify_supervisor_phase(SupervisorPhase::Ready), ReconnectDecision::Retry);
        assert_eq!(r.classify_supervisor_phase(SupervisorPhase::Starting), ReconnectDecision::Retry);
        assert_eq!(r.classify_supervisor_phase(SupervisorPhase::Ending), ReconnectDecision::Retry);
    }

    #[test]
    fn unresponsive_retries_until_the_health_window_then_goes_terminal() {
        let mut r = ReconnectState::new();
        let t0 = Instant::now();
        assert_eq!(r.classify_unresponsive(t0), ReconnectDecision::Retry);
        assert_eq!(r.classify_unresponsive(t0 + Duration::from_secs(60)), ReconnectDecision::Retry);
        assert_eq!(
            r.classify_unresponsive(t0 + HEALTH_WINDOW),
            ReconnectDecision::Terminal(TerminalReason::HealthWindowExpired)
        );
    }

    #[test]
    fn clearing_unresponsive_starts_a_fresh_window_next_time() {
        let mut r = ReconnectState::new();
        let t0 = Instant::now();
        r.classify_unresponsive(t0);
        r.clear_unresponsive();
        // A LATER unresponsive spell, well past the first window's own
        // deadline, must not inherit the old clock.
        let t1 = t0 + HEALTH_WINDOW + Duration::from_secs(1);
        assert_eq!(r.classify_unresponsive(t1), ReconnectDecision::Retry);
    }

    // ---- (e) legs_match ---------------------------------------------------

    #[test]
    fn legs_match_requires_both_pid_and_creation_time() {
        assert!(legs_match((10, 100), (10, 100)));
        assert!(!legs_match((10, 100), (10, 101)));
        assert!(!legs_match((10, 100), (11, 100)));
    }

    #[test]
    fn attach_notice_text_is_the_pinned_wording() {
        assert_eq!(attach_notice_text("2026-09-01T12:00:00Z"), "attached to leg started 2026-09-01T12:00:00Z");
    }

    // ---- (f) fe_down --------------------------------------------------

    #[test]
    fn last_evidence_ts_reads_the_last_lines_ts() {
        let contents = "{\"ts\":\"t-old\"}\n{\"ts\":\"t-new\"}\n";
        assert_eq!(last_evidence_ts(contents).as_deref(), Some("t-new"));
    }

    #[test]
    fn last_evidence_ts_none_on_empty_or_malformed_trailing_line() {
        assert_eq!(last_evidence_ts(""), None);
        assert_eq!(last_evidence_ts("not json\n"), None);
        assert_eq!(last_evidence_ts("{\"ts\":\"t-old\"}\nnot json\n"), None);
    }

    #[test]
    fn marker_shape_matches_the_pinned_json() {
        let v = build_fe_down_marker("kitt-dev", "t0", "t1");
        assert_eq!(v["from"], "sot-fe");
        assert_eq!(v["to"], "kitt-dev");
        assert_eq!(v["kind"], "fe_down");
        assert_eq!(v["ts"], "t1");
        assert_eq!(v["window"]["last_evidence"], "t0");
        assert_eq!(v["text"], "possible relay gap: last inbox evidence t0, frontend reattached t1");
    }

    #[test]
    fn no_baseline_no_marker() {
        let mut b = FeDownBaseline::capture(None);
        // Even on a SECOND attach (so "first attach" is not why), no
        // baseline still means no marker.
        b.marker_for_attach("h", "t1");
        assert!(b.marker_for_attach("h", "t2").is_none());
    }

    #[test]
    fn first_attach_is_skipped_even_with_a_baseline() {
        let mut b = FeDownBaseline::capture(Some("t0".to_string()));
        assert!(b.marker_for_attach("h", "t1").is_none());
        let second = b.marker_for_attach("h", "t2").expect("second attach should mark");
        assert_eq!(second["window"]["last_evidence"], "t0");
        assert_eq!(second["ts"], "t2");
    }

    // ---- (a) QuitDispatcher --------------------------------------------

    #[test]
    fn quit_then_record_closed_is_ended() {
        let mut q = QuitDispatcher::new();
        let t0 = Instant::now();
        assert!(q.request_quit("op-1".into(), t0));
        assert!(matches!(q.state(), QuitState::Ending { .. }));
        assert_eq!(q.message(), Some("ending session\u{2026}"));
        q.on_record_closed();
        assert!(q.should_exit());
    }

    #[test]
    fn a_second_quit_press_does_not_refire_end_run() {
        let mut q = QuitDispatcher::new();
        let t0 = Instant::now();
        assert!(q.request_quit("op-1".into(), t0));
        assert!(!q.request_quit("op-2".into(), t0));
        assert!(matches!(q.state(), QuitState::Ending { operation_id } if operation_id == "op-1"));
    }

    #[test]
    fn cutoff_expiry_shows_outcome_unknown_and_never_exits() {
        let mut q = QuitDispatcher::new();
        let t0 = Instant::now();
        q.request_quit("op-1".into(), t0);
        q.tick(t0 + QUIT_CUTOFF);
        assert!(matches!(q.state(), QuitState::OutcomeUnknown));
        assert!(!q.should_exit());
        assert_eq!(q.message(), Some("ending the session did not complete \u{2014} outcome unknown"));
        // A record_closed arriving late (after the window already gave
        // up) must not resurrect it into Ended -- the ADR's window
        // stays in "outcome unknown", it does not flip back.
        q.on_record_closed();
        assert!(!q.should_exit());
    }

    #[test]
    fn idle_and_ended_show_no_message() {
        let q = QuitDispatcher::new();
        assert_eq!(q.message(), None);
        let mut q2 = QuitDispatcher::new();
        q2.request_quit("op".into(), Instant::now());
        q2.on_record_closed();
        assert_eq!(q2.message(), None);
    }
}
