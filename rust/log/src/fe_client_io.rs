//! L1-unix LU3b (ADR 0043 decision 20): the FE attach-only client's
//! RENDERING half. ADR 0046 decision 3 (lane B3a) extracted the transport
//! half — connect, hello, attach, checkpoint reassembly, the episode
//! reader, take/input transactions, reconnect, quit — into
//! [`crate::attach_worker::AttachWorker`], a worker with a caller-
//! supplied event sink. This is a PURE, behavior-preserving extraction:
//! [`crate::attach_worker::WorkerEvent`] is exactly this module's own
//! pre-extraction `ClientEvent`, renamed; [`FeAttachClient`] is now a
//! THIN WRAPPER over the worker, owning only the [`vt100_ctt::Parser`]
//! and `pump()`-facing UI bookkeeping (`status`/`notice`/`quit_message`/
//! `should_exit`/`checkpointed`/`restore_ok`/`pending_fe_down_markers`).
//! `recorded_bytes`/`last_input_outcome` are the SAME shared observables
//! the pre-extraction worker wrote directly — the worker still writes
//! them directly; they were never part of the event vocabulary and still
//! are not. Its own public surface (`attach`/`attach_headless`/`pump`/
//! `screen`/`send_input`/`resize`/`request_quit`/`quit_message`/
//! `should_exit`/`shutdown`/...) is BYTE-IDENTICAL to before this lane,
//! for its existing callers: the frontend's Terminal drawer and session
//! pane (`gpu.rs`) and the daemon's headless callers
//! (`capsule_workspace.rs`).
//!
//! [`Self::attach_inner`] builds its own `mpsc::channel::<WorkerEvent>()`
//! and passes a closure over its sending half — plus the caller's `wake`
//! — as [`crate::attach_worker::AttachWorker::spawn`]'s own `sink`
//! argument, exactly the shape the pre-extraction module's `run_worker`
//! was spawned with. [`Self::pump`] drains that channel non-blockingly,
//! unchanged from before this lane.
//!
//! [`InputOutcome`] is [`crate::attach_worker::InputOutcome`], re-
//! exported at this module's own path so existing callers
//! (`capsule_workspace.rs`, `tests/fe_client.rs`) are unaffected by the
//! move.

use crate::attach_worker::{AttachWorker, DEFAULT_INGRESS_BOUND_BYTES};
pub use crate::attach_worker::{InputOutcome, WorkerEvent};
use crate::client::Endpoint;
// `PlatformEndpoint` only EXISTS on Windows/Linux (`client.rs`'s own
// cfg) -- this module's one remaining platform tie, confined to
// `FeAttachClient`'s default type parameter and the unit tests below
// that construct it directly.
#[cfg(any(windows, target_os = "linux"))]
use crate::client::PlatformEndpoint;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Local, FE-side scrollback depth for the restored screen — a UI
/// parameter, not protocol-defined (the capsule itself keeps none; see
/// ADR 0041 "Terminal state"). Matches `term::LocalTerminal`'s own value
/// for parity between the two drawer backends.
const SCROLLBACK_ROWS: usize = 5000;
/// How many bytes of `Output` one `pump()` call drains before returning
/// — a continuous-output flood drains in bounded slices across several
/// frames instead of stalling one; the caller already requests another
/// redraw whenever `pump()` returns `true`, which is what brings it back
/// for the rest. Comfortably below the worker's own reader cap so a
/// single `pump()` call can never itself observe the reader having
/// stalled.
const PUMP_DRAIN_CAP_BYTES: usize = 1024 * 1024;
/// Prefix of the status `pump()` sets when `WorkerEvent::Checkpoint`'s
/// own `restore_screen` fails — shared with the `WorkerEvent::Status`
/// handler right below it, which must not let a stale, already-queued
/// "attached" silently overwrite this.
const CHECKPOINT_RESTORE_FAILED_PREFIX: &str = "checkpoint restore failed";

/// Constructor/attach-time failures — everything AFTER a successful
/// [`FeAttachClient::attach`] is reported through [`FeAttachClient::pump`]
/// (status text / terminal notice), matching the ADR's "an actionable
/// error offering retry and reset" rather than a plain `Result` deep
/// inside a long-running reconnect loop. Resolving `state_dir` itself is
/// the CALLER's job (see `attach`'s own doc), so there is no
/// "no state dir" variant here — that failure is the caller's to name.
#[derive(Debug)]
pub enum FeAttachError {
    SpawnWorkerThread(std::io::Error),
}

impl std::fmt::Display for FeAttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeAttachError::SpawnWorkerThread(e) => write!(f, "spawn fe-client worker thread: {e}"),
        }
    }
}

/// The attach-only drawer backend — the same consumer-facing shape as
/// `term::LocalTerminal` (`pump`/`screen`/`send_input`/`resize`/
/// `is_dead`), plus the quit-dispatcher and fe_down surfaces
/// `LocalTerminal` has no analog for.
///
/// L1-unix LU3b: generic over `E: Endpoint`, defaulted to
/// [`PlatformEndpoint`] — every real caller (the frontend) names this
/// type unparameterised (`FeAttachClient`) and gets whatever `Endpoint`
/// this build's own platform speaks; a test naming a different `E`
/// still compiles the same struct. No separate `PhantomData<E>` marker
/// is needed: `worker: AttachWorker<E>` already names `E` in a field.
pub struct FeAttachClient<
    #[cfg(any(windows, target_os = "linux"))] E: Endpoint = PlatformEndpoint,
    #[cfg(not(any(windows, target_os = "linux")))] E: Endpoint,
> {
    parser: vt100_ctt::Parser,
    /// The pane's current `(rows, cols)` — the CALLER's rect, tracked
    /// independently of whatever size a just-restored checkpoint carries.
    /// A capsule's checkpoint reflects the capsule's own PTY dimensions
    /// (its `build_run_command` default of 80x24 until the capsule is
    /// actually resized), which is not necessarily the pane's current
    /// rect: a WATCHER cannot correct the capsule's geometry until it
    /// holds the pen (`resize`'s own doc), so the first checkpoint after
    /// attach — and any checkpoint from a reattach whose pane rect moved
    /// while disconnected — can arrive at a size that disagrees with the
    /// rect the renderer actually paints into. `pump`'s `Checkpoint` arm
    /// reflows the restored screen to this size right after every
    /// restore, so the render never depends on the wire-level resize
    /// (gated on holding the pen) completing first — see that arm's own
    /// comment for why `Screen::set_size`'s deterministic pad/clip is
    /// enough on its own, no protocol change needed.
    pane_size: (u16, u16),
    worker: AttachWorker<E>,
    events_rx: Receiver<WorkerEvent>,
    /// Codex review round, deletion candidate: the join handle used to be
    /// held for no reason a `JoinHandle`'s own `Drop` does not already
    /// give for free (dropping it neither joins nor detaches — Rust
    /// threads run detached from their handle either way). Not stored.
    /// Ruling (d)'s reader/worker teardown is unaffected: the WORKER
    /// thread's own loop exits on `Shutdown` or `Disconnected`
    /// regardless of whether anything outlives it holding the handle.
    status: String,
    notice: Option<String>,
    quit_message: Option<String>,
    should_exit: bool,
    dead: bool,
    /// LU6a: `true` once the `Checkpoint` arm of `pump` has applied a
    /// checkpoint to `parser` (set right after `restore_screen`,
    /// regardless of whether that restore itself succeeded — the
    /// checkpoint EVENT still landed either way, and there is only ever
    /// one per attach episode). The caller (`gpu.rs`'s session pane) uses
    /// this to know when it may stop holding the pane's previous content
    /// and paint this client's own screen instead — see
    /// `pane_screen_choice` there.
    checkpointed: bool,
    /// switch-latency Phase 1: `true` only when the MOST RECENT
    /// `Checkpoint` event's own `restore_screen` call actually succeeded
    /// — unlike `checkpointed` above (which stays `true` after a failed
    /// restore, by design), this reflects the latest attempt honestly so
    /// a caller's instrumentation can tell "attached, and rendering what
    /// it received" from "attached, but the restore itself failed" (see
    /// [`Self::restore_ok`]).
    restore_ok: bool,
    /// Codex review round, finding 10: markers `pump` receives land here
    /// (never re-drained from the same channel a second time, which
    /// silently ate whatever non-marker event happened to be next in
    /// line). `drain_fe_down_markers` drains ONLY this queue.
    pending_fe_down_markers: VecDeque<serde_json::Value>,
    /// ADR 0042 amendment (2026-09-07): `true` for [`Self::attach_headless`]
    /// — a client with no viewport. Read only by [`Self::pump`]'s
    /// `Checkpoint` arm: a headless client adopts the checkpoint's own
    /// geometry as [`Self::pane_size`] instead of reflowing the restored
    /// screen TO `pane_size` (there is no real viewport size to reflow to).
    /// The take transaction's own headless behavior (never sending
    /// `Resize`) is a separate, independent flag on [`fe_client::
    /// TakeTransaction`] itself, chosen by `attach_worker::run_worker` at
    /// construction (private there, so not a linkable path from here) —
    /// this field never reaches that decision directly.
    headless: bool,
    /// Sum of the byte lengths of every `input` this client has seen
    /// `InputRecorded` for (ADR 0042 amendment: "success requires
    /// `InputRecorded` covering the WHOLE payload, not a bare ack
    /// counter" — a caller compares this against the length it sent,
    /// rather than trusting a single increment-only tick). Shared with the
    /// worker thread, which is the only writer.
    recorded_bytes: Arc<AtomicU64>,
    /// Most recent `take_epoch` granted (`take_ok`), including one from
    /// the worker's own transparent reconnect. `0` before the first
    /// grant; a change means the pen moved to a different attach.
    /// Shared with the worker thread, the only writer.
    take_epoch: Arc<AtomicU64>,
    /// The wire's terminal answer to the MOST RECENT `input` this client
    /// sent (`Recorded` / `RefusedStale` / `DeliveryUnknown` — ADR 0041's
    /// own "three terminal answers," restated as [`InputOutcome`]). `None`
    /// until the first outcome arrives. Shared with the worker thread
    /// (the only writer); a `Mutex` rather than an atomic encoding because
    /// this is written and read at most a few times per client lifetime
    /// (never a hot path) and a 3-variant enum has no natural atomic
    /// representation worth inventing one for.
    last_input_outcome: Arc<Mutex<Option<InputOutcome>>>,
}


impl<E: Endpoint> FeAttachClient<E> {
    /// Connects through `endpoint` and starts the background worker; the
    /// worker itself performs the connect/hello/status/attach/checkpoint
    /// sequence and every reconnect thereafter — this constructor never
    /// blocks on the network, matching `LocalTerminal::spawn`'s own
    /// "returns once the reader thread is running" contract. `lane` is
    /// the supervisor lane's name in `endpoint`'s own namespace (ADR 0045
    /// decision 5 — the state-dir hash for the platform endpoints) and is
    /// the CALLER's resolved value (`state_dir::state_dir_hash` of
    /// `state_dir::sot_state_dir()` for the real frontend; an isolated
    /// tempdir's hash for `tests/fe_client.rs`) — this constructor takes
    /// it rather than resolving it itself, the same way `sot-capsule
    /// supervise <state_dir>` takes its state dir as an explicit argument
    /// rather than an internal env-var lookup, so a real client and a
    /// test can point at different trees in the same process without
    /// racing a shared env var. `fe_down_last_evidence` is likewise the
    /// CALLER's own read of `fe-inbox.jsonl`, taken at FE PROCESS START
    /// (Codex review round, finding 10) — this constructor never reads
    /// that file itself, so a drawer opened long after startup still
    /// reports the SAME baseline the process began with.
    pub fn attach(
        endpoint: E,
        lane: String,
        cols: u16,
        rows: u16,
        controller_id: String,
        fe_down_to_handle: String,
        fe_down_last_evidence: Option<String>,
        wake: Box<dyn Fn() + Send + 'static>,
    ) -> Result<Self, FeAttachError>
    where
        E: Send + 'static,
        E::Client: 'static,
    {
        Self::attach_inner(
            endpoint,
            lane,
            cols,
            rows,
            controller_id,
            fe_down_to_handle,
            fe_down_last_evidence,
            wake,
            false,
        )
    }

    /// ADR 0042 amendment (2026-09-07): a HEADLESS attach — the daemon's
    /// own `pty.input`/`pty.screen` client on a capsule row
    /// (`capsule_workspace::headless`), never the frontend. No viewport
    /// (`cols`/`rows` are a placeholder until the checkpoint lands — see
    /// [`Self::pump`]'s `Checkpoint` arm), no `fe_down` marker (this client
    /// makes exactly one attach in its short lifetime, and
    /// [`FeDownBaseline::marker_for_attach`]'s own "skipped on a first
    /// attach" rule means `fe_down_last_evidence: None` here is never
    /// observed to matter), no real wake (the caller pumps on its own
    /// clock, not an event loop). The take transaction this spawns is
    /// [`fe_client::TakeTransaction::new_headless`] — it sends no `Resize`,
    /// ever.
    pub fn attach_headless(
        endpoint: E,
        lane: String,
        controller_id: String,
    ) -> Result<Self, FeAttachError>
    where
        E: Send + 'static,
        E::Client: 'static,
    {
        // Placeholder viewport: wholesale-replaced by the first checkpoint
        // restore regardless (`pump`'s `Checkpoint` arm), and this client
        // adopts the checkpoint's OWN size into `pane_size` rather than
        // reflowing to this one (`headless: true` below) — the exact
        // number here is never rendered or reported.
        const PLACEHOLDER_SIZE: u16 = 24;
        Self::attach_inner(
            endpoint,
            lane,
            PLACEHOLDER_SIZE,
            PLACEHOLDER_SIZE,
            controller_id.clone(),
            controller_id,
            None,
            Box::new(|| {}),
            true,
        )
    }

    fn attach_inner(
        endpoint: E,
        lane: String,
        cols: u16,
        rows: u16,
        controller_id: String,
        fe_down_to_handle: String,
        fe_down_last_evidence: Option<String>,
        wake: Box<dyn Fn() + Send + 'static>,
        headless: bool,
    ) -> Result<Self, FeAttachError>
    where
        E: Send + 'static,
        E::Client: 'static,
    {
        let rows = rows.max(2);
        let cols = cols.max(2);
        let parser = vt100_ctt::Parser::new(rows, cols, SCROLLBACK_ROWS);

        let (events_tx, events_rx) = mpsc::channel::<WorkerEvent>();
        let sink = move |e: WorkerEvent| {
            let _ = events_tx.send(e);
            wake();
        };
        let recorded_bytes = Arc::new(AtomicU64::new(0));
        let last_input_outcome = Arc::new(Mutex::new(None));
        let take_epoch = Arc::new(AtomicU64::new(0));

        let worker = AttachWorker::spawn(
            endpoint,
            lane,
            cols,
            rows,
            controller_id,
            fe_down_to_handle,
            fe_down_last_evidence,
            headless,
            DEFAULT_INGRESS_BOUND_BYTES,
            Arc::clone(&recorded_bytes),
            Arc::clone(&last_input_outcome),
            Arc::clone(&take_epoch),
            sink,
        )
        .map_err(FeAttachError::SpawnWorkerThread)?;

        Ok(Self {
            parser,
            pane_size: (rows, cols),
            worker,
            events_rx,
            status: "connecting\u{2026}".to_string(),
            notice: None,
            quit_message: None,
            should_exit: false,
            dead: false,
            checkpointed: false,
            restore_ok: false,
            pending_fe_down_markers: VecDeque::new(),
            headless,
            recorded_bytes,
            take_epoch,
            last_input_outcome,
        })
    }

    /// Drains pending events into the parser/UI state (non-blocking).
    /// Returns `true` iff anything changed — the caller schedules a
    /// repaint, matching `LocalTerminal::pump`'s own contract. Caps
    /// `Output` bytes drained per call at [`PUMP_DRAIN_CAP_BYTES`] (Codex
    /// review round, finding 7): a continuous-output flood is drained in
    /// bounded slices across several frames rather than stalling
    /// rendering for one unbounded call — the caller already requests
    /// another redraw whenever this returns `true`, which is what brings
    /// `pump` back for the rest.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        let mut drained_output_bytes = 0usize;
        loop {
            if drained_output_bytes >= PUMP_DRAIN_CAP_BYTES {
                break;
            }
            match self.events_rx.try_recv() {
                Ok(WorkerEvent::Checkpoint(bytes)) => {
                    // switch-latency Phase 1: a fresh checkpoint starts a
                    // new attach episode against a (possibly different)
                    // leg, so any notice left over from the PREVIOUS one
                    // is retracted here rather than left standing until a
                    // new `Notice` event replaces it. The attach notice
                    // is emitted only after this checkpoint (the
                    // mgmt-lane identity lookup it depends on runs
                    // afterward — see `run_worker`'s own comment at that
                    // reorder), so without this clear a reconnect from
                    // leg A to leg B would render B's freshly restored
                    // screen under A's stale notice text for as long as
                    // that lookup takes.
                    self.notice = None;
                    let restore_result = self.parser.restore_screen(&bytes);
                    // switch-latency Phase 1: recorded separately from
                    // `checkpointed` below (which is set unconditionally
                    // either way) so a caller can tell "did apply a
                    // checkpoint" from "did apply it SUCCESSFULLY" — see
                    // `restore_ok`'s own doc.
                    self.restore_ok = restore_result.is_ok();
                    if let Err(e) = restore_result {
                        self.status = format!("{CHECKPOINT_RESTORE_FAILED_PREFIX}: {e:?}");
                    } else if self.headless {
                        // ADR 0042 amendment: a headless client has no
                        // viewport to reflow TO — adopt the checkpoint's
                        // OWN dimensions as `pane_size` instead (never
                        // `set_size`, which would pad/clip it to whatever
                        // placeholder `attach_headless` was constructed
                        // with). This is also what keeps the take
                        // transaction's own `Resize` a true no-op honest:
                        // there is no local geometry disagreement to
                        // correct in the first place.
                        self.pane_size = self.parser.screen().size();
                    } else {
                        // `restore_screen` REPLACES the parser's screen
                        // wholesale with one sized to the checkpoint's own
                        // encoded dimensions (`Screen::restore`, vt100 crate)
                        // — not this parser's construction size, and not
                        // `pane_size`. Reflow it to the pane's current rect
                        // immediately, rather than waiting on the wire-level
                        // resize: that only reaches the capsule once this
                        // client holds the pen (`resize`'s own doc), which a
                        // fresh WATCHER never does before its first paint.
                        // `Screen::set_size` pads/clips deterministically
                        // (`vt100::grid::Grid::set_size`), so this is a pure
                        // local reflow with no protocol involvement — the
                        // actual capsule-side resize still happens exactly
                        // as before, via the take-on-first-input handshake.
                        let (rows, cols) = self.pane_size;
                        self.parser.screen_mut().set_size(rows, cols);
                    }
                    // LU6a: right after `restore_screen`, success or not —
                    // see `checkpointed`'s own doc for why a failed restore
                    // still counts (the checkpoint EVENT landed either way,
                    // and there is only ever one per episode).
                    self.checkpointed = true;
                    changed = true;
                }
                Ok(WorkerEvent::Output(bytes)) => {
                    drained_output_bytes += bytes.len();
                    // The ONLY decrement of the shared byte-account — see
                    // this struct's own `queued_bytes` doc. Wakes the
                    // reader if it is parked in `wait_below_cap`.
                    self.worker.ack_output_consumed(bytes.len());
                    self.parser.process(&bytes);
                    changed = true;
                }
                Ok(WorkerEvent::Notice(text)) => {
                    self.notice = Some(text);
                    changed = true;
                }
                Ok(WorkerEvent::Status(text)) => {
                    // The worker queues `Status("attached")` unconditionally
                    // right behind the checkpoint bytes -- it does not
                    // itself know whether the LOCAL restore will succeed,
                    // that only happens above, foreground-side, in the
                    // SAME drain (Codex round on #194, finding 1). A
                    // client that could not render what it received is
                    // not honestly "attached"; do not let this stale
                    // success overwrite the failure that was just set.
                    let stale_attached_after_failed_restore = text == "attached"
                        && self.status.starts_with(CHECKPOINT_RESTORE_FAILED_PREFIX);
                    if !stale_attached_after_failed_restore {
                        self.status = text;
                    }
                    changed = true;
                }
                Ok(WorkerEvent::Terminal(text)) => {
                    // ADR 0030 §8 "Where it is shown": mirror the reason to
                    // `tracing` here too, not only `self.status` (which a
                    // caller must poll) -- this is the ONE place `self.dead`
                    // ever becomes true, so it fires exactly once per
                    // episode, same as every other one-shot log line in
                    // this file.
                    tracing::warn!(reason = %text, "fe attach client: reached a terminal state");
                    self.status = text;
                    self.dead = true;
                    changed = true;
                }
                Ok(WorkerEvent::QuitMessage(msg)) => {
                    self.quit_message = msg;
                    changed = true;
                }
                Ok(WorkerEvent::ShouldExit) => {
                    self.should_exit = true;
                    changed = true;
                }
                Ok(WorkerEvent::FeDownMarker(v)) => {
                    // Codex review round, finding 10: land it in the
                    // dedicated queue rather than discarding the payload
                    // — `drain_fe_down_markers` reads ONLY this queue,
                    // never the channel again, so no other event can be
                    // swallowed alongside it.
                    self.pending_fe_down_markers.push_back(v);
                    changed = true;
                }
                Err(_) => break,
            }
        }
        changed
    }

    /// Any `fe_down` markers `pump` received since the last drain — the
    /// caller (the frontend) appends each to `fe-inbox.jsonl` and must
    /// surface a VISIBLE failure if the append itself fails ("a marker
    /// that exists so a failure is not quiet cannot fail quietly
    /// itself"). Call AFTER `pump`, which is what actually populates the
    /// queue this drains.
    pub fn drain_fe_down_markers(&mut self) -> Vec<serde_json::Value> {
        self.pending_fe_down_markers.drain(..).collect()
    }

    pub fn screen(&self) -> &vt100_ctt::Screen {
        self.parser.screen()
    }

    pub fn screen_mut(&mut self) -> &mut vt100_ctt::Screen {
        self.parser.screen_mut()
    }

    pub fn mouse_tracking_on(&self) -> bool {
        !matches!(self.parser.screen().mouse_protocol_mode(), vt100_ctt::MouseProtocolMode::None)
    }

    /// Forwards keystroke bytes to the worker, which drives the
    /// take-on-first-input transaction (ruling (b)).
    pub fn send_input(&mut self, bytes: &[u8]) {
        if let Err(e) = self.worker.send_input(bytes.to_vec()) {
            tracing::warn!(error = %e, "fe attach client: dropped an input send");
        }
    }

    /// Records the desired viewport. A WATCHER cannot correct the
    /// geometry until it holds the pen (ruling (b)) — the worker applies
    /// this only once `take_ok` grants the pen (via its OWN `resize`,
    /// awaited alone — see `fe_client::TakeTransaction::on_take_ok`), or
    /// immediately (as an ordinary `resize` request) while already
    /// DRIVING.
    ///
    /// The LOCAL screen is reflowed right here, unconditionally — mirrors
    /// `term::LocalTerminal::resize`'s own "resize both the PTY and the
    /// vt100 parser so their grids stay in sync," except the capsule side
    /// of that pair is the wire resize above (protocol-gated on holding
    /// the pen, unlike a local PTY spawn). Updating `pane_size` here is
    /// also what makes a later reattach renegotiate: the next checkpoint
    /// this episode restores — whatever size the capsule's own PTY
    /// happens to carry — is reflowed to THIS size in `pump`'s
    /// `Checkpoint` arm, not to the size the pane happened to be at the
    /// original `attach` call.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let rows = rows.max(2);
        let cols = cols.max(2);
        self.pane_size = (rows, cols);
        self.parser.screen_mut().set_size(rows, cols);
        self.worker.resize(cols, rows);
    }

    /// `true` once the reconnect episode has reached a TERMINAL
    /// classification (ruling (d)) — the drawer shows the terminal
    /// notice rather than a blank pane.
    pub fn is_dead(&mut self) -> bool {
        self.dead
    }

    /// Ruling (a): the ONE quit dispatcher. Idempotent — a second call
    /// while already ending does nothing (the worker's own
    /// `QuitDispatcher` enforces this). Never lost across a reconnect in
    /// flight (Codex review round, finding 2) — the worker LATCHES this
    /// message rather than dropping it if a reconnect backoff is
    /// currently in progress.
    pub fn request_quit(&mut self, reason: &str) {
        self.worker.request_quit(reason);
    }

    /// `Some("ending session…")` / `Some("verifying…")` / `Some("...
    /// outcome unknown")` / `Some("...failed: ...")` /
    /// `Some("...refused: ...")` while a quit is in flight, verifying,
    /// or reached a terminal outcome; `None` otherwise.
    pub fn quit_message(&self) -> Option<&str> {
        self.quit_message.as_deref()
    }

    /// `true` once `record_verified` arrived (ADR Lifecycle: "the
    /// COMMAND reply arrives at record_closed, and record_verified
    /// follows through query") — the caller may now call
    /// `event_loop.exit()`.
    pub fn should_exit(&self) -> bool {
        self.should_exit
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn status_line(&self) -> &str {
        &self.status
    }

    /// `true` once the FIRST `WorkerEvent::Checkpoint` has been applied —
    /// restore ATTEMPTED, whether or not it succeeded (a failed restore
    /// still sets [`Self::status`] to the `CHECKPOINT_RESTORE_FAILED_PREFIX`
    /// text, which a caller polling this should also check). ADR 0042
    /// amendment: the headless client's own "pump until attached" loop
    /// condition — a watcher's checkpoint arrives exactly once per attach,
    /// so this never resets after the first `true`.
    pub fn is_checkpointed(&self) -> bool {
        self.checkpointed
    }

    /// `true` only when the MOST RECENT `Checkpoint` event's own
    /// `restore_screen` call actually succeeded — switch-latency Phase
    /// 1's own addition, distinguishing "attached and rendering it" from
    /// [`Self::is_checkpointed`]'s weaker "a checkpoint arrived, whether
    /// or not it could be applied" (that one stays `true` after a failed
    /// restore, by design — see its own doc). `false` before any
    /// `Checkpoint` event has landed at all.
    pub fn restore_ok(&self) -> bool {
        self.restore_ok
    }

    /// Sum of the byte lengths of every `input` `InputRecorded` for so
    /// far. ADR 0042 amendment's own observable: a caller compares this
    /// (before vs. after sending) against the length it sent, rather than
    /// trusting a bare increment-only ack counter.
    pub fn recorded_bytes(&self) -> u64 {
        self.recorded_bytes.load(Ordering::Acquire)
    }

    /// The most recent `take_epoch` this client's worker has been
    /// granted — see the field's own doc for what a change means.
    pub fn take_epoch(&self) -> u64 {
        self.take_epoch.load(Ordering::Acquire)
    }

    /// The wire's terminal answer to the most recent `input` this client
    /// sent — see [`InputOutcome`]'s own doc. A poisoned lock (a prior
    /// panic while holding it) reads as `None` rather than panicking here
    /// too: a caller polling this in a loop must never itself become the
    /// second panic.
    pub fn last_input_outcome(&self) -> Option<InputOutcome> {
        self.last_input_outcome.lock().ok().and_then(|g| *g)
    }

    /// Sends `Shutdown` (same as [`Drop`] does) and waits UP TO `wait` for
    /// the worker thread to actually exit, polling [`thread::JoinHandle::
    /// is_finished`] rather than an unbounded `join()` — ADR 0042
    /// amendment: "on EVERY exit path: drop the client, then observe the
    /// worker's closure." Returns `true` iff the worker exited within the
    /// bound; `false` logs a `warn` and leaves the handle for `Drop` to
    /// forget about (a `JoinHandle` that is never joined does not leak the
    /// thread — it simply runs to completion on its own, same as today).
    /// Callable at most meaningfully once — a second call after the first
    /// already took the handle returns `true` (nothing left to wait for).
    pub fn shutdown(&mut self, wait: Duration) -> bool {
        self.worker.shutdown(wait)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// LU6a: a fresh client reports `is_checkpointed() == false`, and
    /// `true` once a `Checkpoint` event has gone through `pump` -- the
    /// SAME arm a real worker's checkpoint event drains through. Built by
    /// hand rather than via `attach` (which spawns a real worker thread
    /// and needs a real state dir/lane): this module's own child-module
    /// privacy lets the struct literal reach every private field, and
    /// `pump` neither knows nor cares whether `events_tx` belongs to a
    /// worker thread or a test.
    // `PlatformEndpoint` only exists on Windows/Linux.
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn checkpoint_event_marks_the_client_checkpointed() {
        let (events_tx, events_rx) = mpsc::channel();
        let mut client = FeAttachClient::<PlatformEndpoint> {
            parser: vt100_ctt::Parser::new(24, 80, 100),
            pane_size: (24, 80),
            worker: AttachWorker::stub_for_test(),
            events_rx,
            status: "connecting\u{2026}".to_string(),
            notice: None,
            quit_message: None,
            should_exit: false,
            dead: false,
            checkpointed: false,
            restore_ok: false,
            pending_fe_down_markers: VecDeque::new(),
            headless: false,
            recorded_bytes: Arc::new(AtomicU64::new(0)),
            take_epoch: Arc::new(AtomicU64::new(0)),
            last_input_outcome: Arc::new(Mutex::new(None)),
        };
        assert!(!client.is_checkpointed(), "a fresh client must not report checkpointed");
        assert!(!client.restore_ok(), "a fresh client must not report a successful restore");

        let bytes = vt100_ctt::Parser::new(24, 80, 100)
            .screen()
            .checkpoint()
            .expect("encode a checkpoint of a fresh, in-range screen");
        events_tx.send(WorkerEvent::Checkpoint(bytes)).expect("send a synthetic checkpoint event");
        client.pump();
        assert!(
            client.is_checkpointed(),
            "pump()'s Checkpoint arm must mark the client checkpointed"
        );
        assert!(client.restore_ok(), "a well-formed checkpoint must restore successfully");
    }

    /// switch-latency Phase 1: `restore_ok` must go `false`, even though
    /// `is_checkpointed` still goes `true` (LU6a's own "the checkpoint
    /// EVENT landed either way") -- the exact gap `restore_ok` exists to
    /// close for a caller's instrumentation.
    // `PlatformEndpoint` only exists on Windows/Linux.
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn checkpoint_event_with_undecodable_bytes_marks_checkpointed_but_not_restore_ok() {
        let (events_tx, events_rx) = mpsc::channel();
        let mut client = FeAttachClient::<PlatformEndpoint> {
            parser: vt100_ctt::Parser::new(24, 80, 100),
            pane_size: (24, 80),
            worker: AttachWorker::stub_for_test(),
            events_rx,
            status: "connecting\u{2026}".to_string(),
            notice: None,
            quit_message: None,
            should_exit: false,
            dead: false,
            checkpointed: false,
            restore_ok: false,
            pending_fe_down_markers: VecDeque::new(),
            headless: false,
            recorded_bytes: Arc::new(AtomicU64::new(0)),
            take_epoch: Arc::new(AtomicU64::new(0)),
            last_input_outcome: Arc::new(Mutex::new(None)),
        };

        events_tx
            .send(WorkerEvent::Checkpoint(vec![0xff; 4]))
            .expect("send a synthetic, undecodable checkpoint event");
        client.pump();
        assert!(client.is_checkpointed(), "the checkpoint EVENT still landed");
        assert!(!client.restore_ok(), "a failed restore must not report restore_ok");
    }
    /// Codex review round finding 2: a fresh checkpoint (a new attach
    /// episode, possibly against a DIFFERENT leg after a reconnect) must
    /// retract whatever notice was showing for the PREVIOUS leg, rather
    /// than leaving it standing until a new `Notice` event replaces it.
    /// The attach notice is emitted only after the checkpoint (see
    /// `run_worker`'s own reorder comment), so without this clear, a
    /// caller reading `notice()` right after this `pump()` call -- before
    /// the worker's own mgmt-lane lookup for the NEW leg has produced its
    /// own `Notice` -- would see leg A's stale text rendered over leg B's
    /// freshly restored screen.
    // `PlatformEndpoint` only exists on Windows/Linux.
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn checkpoint_event_clears_a_notice_left_over_from_the_previous_leg() {
        let (events_tx, events_rx) = mpsc::channel();
        let mut client = FeAttachClient::<PlatformEndpoint> {
            parser: vt100_ctt::Parser::new(24, 80, 100),
            pane_size: (24, 80),
            worker: AttachWorker::stub_for_test(),
            events_rx,
            status: "connecting\u{2026}".to_string(),
            notice: None,
            quit_message: None,
            should_exit: false,
            dead: false,
            checkpointed: false,
            restore_ok: false,
            pending_fe_down_markers: VecDeque::new(),
            headless: false,
            recorded_bytes: Arc::new(AtomicU64::new(0)),
            take_epoch: Arc::new(AtomicU64::new(0)),
            last_input_outcome: Arc::new(Mutex::new(None)),
        };

        events_tx
            .send(WorkerEvent::Notice("leg A started at ...".to_string()))
            .expect("send a synthetic notice for leg A");
        client.pump();
        assert_eq!(client.notice(), Some("leg A started at ..."));

        let bytes = vt100_ctt::Parser::new(24, 80, 100)
            .screen()
            .checkpoint()
            .expect("encode a checkpoint of a fresh, in-range screen");
        events_tx
            .send(WorkerEvent::Checkpoint(bytes))
            .expect("send leg B's own checkpoint -- no Notice for leg B has arrived yet");
        client.pump();
        assert_eq!(
            client.notice(),
            None,
            "leg A's notice must be retracted the moment leg B's checkpoint lands, not left \
             standing until leg B's own Notice (if any) arrives"
        );
    }

    /// Successive checkpoints (each attach episode gets its own) must
    /// each report `restore_ok` for THEIR OWN restore, not a value stuck
    /// from an earlier one -- proven here across three in a row:
    /// success, failure, success again.
    // `PlatformEndpoint` only exists on Windows/Linux.
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn restore_ok_reflects_only_the_most_recent_checkpoint_across_several_in_a_row() {
        let (events_tx, events_rx) = mpsc::channel();
        let mut client = FeAttachClient::<PlatformEndpoint> {
            parser: vt100_ctt::Parser::new(24, 80, 100),
            pane_size: (24, 80),
            worker: AttachWorker::stub_for_test(),
            events_rx,
            status: "connecting\u{2026}".to_string(),
            notice: None,
            quit_message: None,
            should_exit: false,
            dead: false,
            checkpointed: false,
            restore_ok: false,
            pending_fe_down_markers: VecDeque::new(),
            headless: false,
            recorded_bytes: Arc::new(AtomicU64::new(0)),
            take_epoch: Arc::new(AtomicU64::new(0)),
            last_input_outcome: Arc::new(Mutex::new(None)),
        };
        let good_checkpoint = || {
            vt100_ctt::Parser::new(24, 80, 100)
                .screen()
                .checkpoint()
                .expect("encode a checkpoint of a fresh, in-range screen")
        };

        events_tx.send(WorkerEvent::Checkpoint(good_checkpoint())).expect("send checkpoint 1 (good)");
        client.pump();
        assert!(client.restore_ok(), "checkpoint 1 (good) must report restore_ok");

        events_tx.send(WorkerEvent::Checkpoint(vec![0xff; 4])).expect("send checkpoint 2 (bad)");
        client.pump();
        assert!(!client.restore_ok(), "checkpoint 2 (bad) must clear restore_ok, not inherit checkpoint 1's");

        events_tx.send(WorkerEvent::Checkpoint(good_checkpoint())).expect("send checkpoint 3 (good)");
        client.pump();
        assert!(client.restore_ok(), "checkpoint 3 (good) must report restore_ok again, not inherit checkpoint 2's");
    }
}
