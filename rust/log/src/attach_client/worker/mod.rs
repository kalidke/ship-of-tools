//! ADR 0046 decision 3 (lane B3a): the attach lane's transport half,
//! extracted out of `fe_client_io.rs` into a reusable worker,
//! [`AttachWorker`], with a caller-supplied event sink. This lane is a
//! PURE, behavior-preserving extraction — connect, hello, attach,
//! checkpoint reassembly (still whole, as before — chunked delivery is
//! B3b2's own change), the episode reader, the 2s liveness poll,
//! take/input transactions, [`OutstandingSlot`], [`ReconnectState`], and
//! quit (`request_quit`, the ONE dispatcher, unchanged) move here out of
//! `fe_client_io.rs` verbatim in spirit; [`WorkerEvent`] is exactly the
//! pre-extraction module's own `ClientEvent`, renamed, plus nothing else
//! — every other candidate addition (per-request input outcomes,
//! chunked checkpoint delivery, `Reattach`, `Take`, pen/geometry events,
//! `SupervisorStatus` observation) belongs to the lane that first
//! consumes it (B1, B3b1, B3b2, B3b3) and is deliberately NOT here.
//!
//! The one genuine addition is bounded ingress:
//! [`AttachWorker::send_input`] reserves `bytes.len()` against a fixed
//! byte budget before the command channel is ever touched, refusing
//! with [`IngressRefused`] rather than queuing unbounded — but the bound
//! limits ACCUMULATION, never a single send: an input is admitted
//! unconditionally whenever nothing else is currently reserved, even one
//! bigger than the bound itself, and refused only when something is
//! already queued and admitting this one too would push the total past
//! the bound (see that method's own doc — a large paste or a long
//! `sot-fe type --stdin` must never silently vanish for being bigger
//! than a bound sized for steady-state typing). The reservation is
//! OWNERSHIP-based ([`IngressReservation`]): it rides inside the queued
//! command itself and releases in `Drop`, so it is freed correctly
//! however that command is disposed of — consumed normally, discarded
//! mid-drain, or still sitting in the channel when the whole worker (and
//! its `Receiver`) exits — without any call site needing to remember to
//! release it by hand.
//!
//! `fe_client_io::FeAttachClient` is the thin wrapper: it owns the
//! `vt100_ctt::Parser` and `pump()`'s UI bookkeeping, subscribing an
//! `AttachWorker` via a sink closure built from its own mpsc channel +
//! wake, exactly as the pre-extraction module's `run_worker` was spawned
//! from its `attach_inner`. Its public surface (`attach`/
//! `attach_headless`/`pump`/`screen`/`send_input`/`resize`/
//! `request_quit`/`quit_message`/`should_exit`/`shutdown`/...) is
//! unchanged for its existing callers: the frontend's Terminal drawer
//! and session pane (`gpu.rs`) and the daemon's headless callers
//! (`capsule_workspace.rs`).

use crate::client::Endpoint;
use crate::fe_client::FeDownBaseline;
use crate::wire::{self, DecodedFrame};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// `hello`'s own reply budget (ADR 0041 Lifecycle "Every op has one
/// budget: connect 2 s, request write 2 s..." — hello is a fixed,
/// single-round-trip request, so it shares the 2 s figure rather than
/// the slower `status` budget below).
const HELLO_BUDGET: Duration = Duration::from_secs(2);
/// "Every client's first act, after the identity check above, is a
/// `status` with a 5 s budget; a lane that accepts but does not answer
/// within it is treated exactly as an absent lane."
const STATUS_BUDGET: Duration = Duration::from_secs(5);
/// Absolute deadline for an ENTIRE checkpoint transfer, not merely each
/// frame within it (Codex round on #194, finding 3): `STATUS_BUDGET`
/// alone re-arms every loop iteration in
/// [`attach_and_collect_checkpoint`], and the wire format allows any
/// number of `checkpoint_chunk` frames -- including empty non-final ones
/// (see `wire::CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD`'s own doc) -- so a
/// faulty or hostile capsule dripping one technically-legal frame every
/// ~5 s could hold this worker open forever. Sized at the greedy
/// encoder's own worst-case chunk count times the per-frame budget:
/// generous for the ordinary case (a real checkpoint that large is
/// itself the ADR 0041 worst case), and still a REAL bound on the
/// adversarial one. See [`checkpoint_frame_deadline`] for how it clamps
/// each per-frame deadline.
const CHECKPOINT_TRANSFER_BUDGET: Duration =
    Duration::from_secs(wire::CHECKPOINT_CHUNKS_AT_MAX_PAYLOAD as u64 * STATUS_BUDGET.as_secs());
/// Ordinary lane-operation write budget (connect/write halves of the
/// Lifecycle "one budget" triple; the read half is `STATUS_BUDGET` or,
/// for `command`/`query`, the same 5 s figure). `pub(crate)`: shared with
/// [`run_end_run_and_wait`]'s own callers (ADR 0042 L1a, Codex review
/// finding 4 — `supervisor_client::end_run` reuses this exact budget
/// rather than inventing its own).
pub(crate) const WRITE_BUDGET: Duration = Duration::from_secs(2);
/// How often the worker polls the supervisor lane for `status` during
/// steady state — a still-answering authority's phase transition
/// (ruling (d)) must become visible promptly, but polling faster than
/// this buys nothing beyond load.
const LIVENESS_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A re-dial after a missed probe is PACED. The dead socket must be
/// replaced, but a supervisor that is slow rather than gone would
/// otherwise be dialed every two seconds for as long as the stall lasts,
/// and each dial it cannot yet accept sits in its listener backlog
/// (bounded by `MAX_LANE_INSTANCES`) waiting to claim a slot the moment
/// it wakes. One dial, then a doubling wait, reset by the first answered
/// probe: recovery stays immediate and a minute-long stall costs a
/// handful of dials instead of thirty.
const SUPERVISOR_REDIAL_INITIAL: Duration = Duration::from_secs(2);
const SUPERVISOR_REDIAL_MAX: Duration = Duration::from_secs(30);

/// The one wording for a missed liveness probe, shared by the emit and by
/// the restore that undoes it: the restore compares against it, so the two
/// cannot drift apart into a header that never clears.
const PROBE_MISSED_STATUS: &str = "supervisor lane not answering \u{2014} the session is still live";
/// The worker's own message-loop tick — bounds how promptly a command
/// (input/resize/quit) is serviced and how often the pure tick-driven
/// timers (quit cutoff, checkpoint-in-flight retry, backoff) advance.
const WORKER_TICK: Duration = Duration::from_millis(100);
/// Ruling (d): "The reader's unbounded channel becomes BYTE-ACCOUNTED
/// and bounded at 4 MiB — bytes, not items... When it is full the FE
/// STOPS READING THE PIPE." (Codex review round, finding 7: the first
/// landing's counter was local to the reader thread and released
/// immediately, never actually shared with the consumer — see
/// [`crate::fe_client_io::FeAttachClient::pump`]'s own doc for the real,
/// shared half.)
const READER_QUEUE_CAP_BYTES: usize = 4 * 1024 * 1024;

/// The steady state's supervisor lane: the ONE connection the pre-attach
/// `Status` polling proved (no second connect+hello -- see
/// [`ReadyOutcome`]), its reader, and the paced re-dial state (see
/// [`SUPERVISOR_REDIAL_INITIAL`]). [`run_steady_state`] keeps it in a
/// `Mutex` so exactly one thread drives its request/reply lockstep at a
/// time: a liveness probe for its whole round trip, on its own thread,
/// or `run_quit` for the ending transaction.
pub(super) struct SupLane<C> {
    conn: C,
    reader: FrameReader,
    redial_at: Option<Instant>,
    redial_backoff: Duration,
}


// -----------------------------------------------------------------------
// Worker <-> foreground messages
// -----------------------------------------------------------------------


/// An ingress reservation, released automatically wherever it is
/// dropped — consumed normally by the worker's own loop, discarded
/// mid-drain (the several places a queued `WorkerMsg` is read and
/// thrown away without further action), or still sitting unconsumed in
/// the channel when the whole worker thread exits and its `Receiver`
/// drops (which drops every value still queued behind it). This is what
/// makes the ingress bound honest under every one of [`AttachWorker::
/// send_input`]'s own exit paths — including a send that fails because
/// the worker has already shut down, where the returned, undelivered
/// message (reservation included) drops right there — without any call
/// site needing to remember to release it by hand.
struct IngressReservation {
    ingress_bytes: Arc<AtomicUsize>,
    n: usize,
}

impl Drop for IngressReservation {
    fn drop(&mut self) {
        self.ingress_bytes.fetch_sub(self.n, Ordering::AcqRel);
    }
}

enum WorkerMsg {
    /// The bytes, their reservation, and the attach generation read when
    /// the caller sent them (see `attach_gen` in [`AttachWorker`]).
    Input(Vec<u8>, IngressReservation, u64),
    Resize(u16, u16),
    Quit(String),
    Shutdown,
    Frame(DecodedFrame),
    ReaderDone,
}

/// What the worker reports to the foreground — unchanged from the
/// pre-extraction module's own `ClientEvent`, renamed only. `pump()`
/// applies each of these to the parser/UI state.
pub enum WorkerEvent {
    Checkpoint(Vec<u8>),
    /// The count behind `inputs_discarded` changed; payload-free.
    InputsDiscarded,
    Output(Vec<u8>),
    Notice(String),
    Status(String),
    Terminal(String),
    QuitMessage(Option<String>),
    ShouldExit,
    FeDownMarker(serde_json::Value),
}

/// The wire's terminal answer to ONE `input` frame this client sent (ADR
/// 0041), exposed as an observable a caller (the headless daemon client)
/// can poll — unchanged from the pre-extraction module's own type,
/// moved here since it names a worker-level (not rendering-level)
/// observable; `fe_client_io` re-exports it at its own path so existing
/// callers (`capsule_workspace.rs`, `tests/fe_client.rs`) are unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOutcome {
    /// `input_recorded`: the record has it.
    Recorded,
    /// `input_refused_stale`: the epoch changed; this client's own worker
    /// re-takes automatically, but a headless caller treats the ORIGINAL
    /// send as failed and does not wait for that retry.
    RefusedStale,
    /// `input_delivery_unknown`: the record's own verdict is unknowable
    /// from here.
    DeliveryUnknown,
}

/// [`AttachWorker::send_input`]'s own refusal — the ingress reservation
/// would have exceeded the worker's own bound. Carries nothing: the
/// caller already has `bytes` (it just tried to send them) and the
/// bound itself is a construction-time constant the caller chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngressRefused;

impl std::fmt::Display for IngressRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ingress bound exceeded; refused rather than queued unbounded")
    }
}

impl std::error::Error for IngressRefused {}

/// Default bound for [`AttachWorker::send_input`]'s own ingress
/// reservation — a memory bound on "commands enqueued, not yet popped
/// by the worker's own loop," generous relative to [`fe_client::
/// TAKE_QUEUE_CAP`] (8 KiB: several pastes' worth of headroom) without
/// being unbounded. `fe_client_io::FeAttachClient` uses this constant;
/// a caller free to choose a tighter or looser bound may construct an
/// `AttachWorker` with its own.
pub const DEFAULT_INGRESS_BOUND_BYTES: usize = 64 * 1024;

// -----------------------------------------------------------------------
// Public surface: the worker handle
// -----------------------------------------------------------------------

/// A reusable attach transport, generic over `E: Endpoint` exactly like
/// the pre-extraction `FeAttachClient` was (see that type's own doc,
/// `fe_client_io.rs`, for why). Owns the background worker thread.
pub struct AttachWorker<E: Endpoint> {
    msg_tx: Sender<WorkerMsg>,
    ingress_bytes: Arc<AtomicUsize>,
    ingress_bound: usize,
    /// Bumped by the worker immediately before each attach's checkpoint
    /// is emitted; an input stamped with an older value was sent before
    /// that attach and is discarded and counted, never delivered.
    attach_gen: Arc<AtomicU64>,
    /// Inputs that were discarded and not yet followed by a delivered
    /// one: read while not attached, stamped before the current attach,
    /// or refused at the ingress bound.
    discarded: Arc<AtomicUsize>,
    /// The episode reader's own byte-accounted backpressure — see
    /// [`QueuedBytes`]'s own doc. Released by [`Self::ack_output_consumed`].
    queued_bytes: Arc<QueuedBytes>,
    /// Whether the client is the one the user is looking at; a worker
    /// paused for a down link resumes only while this is set.
    viewed: Arc<AtomicBool>,
    worker_handle: Option<thread::JoinHandle<()>>,
    _endpoint: PhantomData<E>,
}

impl<E: Endpoint> AttachWorker<E> {
    /// Connects through `endpoint` and starts the background worker;
    /// this constructor never blocks on the network — matches the
    /// pre-extraction `FeAttachClient::attach`'s own "returns once the
    /// worker thread is running" contract. `sink` is called
    /// synchronously, ON the worker thread, for every [`WorkerEvent`]
    /// this worker ever produces; a caller wanting cross-thread delivery
    /// (the rendering client's own `pump()`/mpsc shape) builds that INTO
    /// its own `sink` closure, exactly as `fe_client_io::FeAttachClient`
    /// does. `recorded_bytes`/`last_input_outcome` are the SAME shared
    /// observables the pre-extraction worker wrote directly — this
    /// worker still writes them directly; they are not part of
    /// [`WorkerEvent`] and never were.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        endpoint: E,
        lane: String,
        cols: u16,
        rows: u16,
        controller_id: String,
        fe_down_to_handle: String,
        fe_down_last_evidence: Option<String>,
        headless: bool,
        ingress_bound: usize,
        recorded_bytes: Arc<AtomicU64>,
        last_input_outcome: Arc<Mutex<Option<InputOutcome>>>,
        take_epoch_pub: Arc<AtomicU64>,
        sink: impl Fn(WorkerEvent) + Send + 'static,
    ) -> Result<Self, std::io::Error>
    where
        E: Send + Sync + 'static,
        E::Client: 'static,
    {
        let rows = rows.max(2);
        let cols = cols.max(2);
        let (msg_tx, msg_rx) = mpsc::channel::<WorkerMsg>();
        let worker_msg_tx = msg_tx.clone();
        let fe_down = FeDownBaseline::capture(fe_down_last_evidence);
        let queued_bytes = Arc::new(QueuedBytes::new());
        let worker_queued_bytes = Arc::clone(&queued_bytes);
        let ingress_bytes = Arc::new(AtomicUsize::new(0));
        let discarded = Arc::new(AtomicUsize::new(0));
        let attach_gen = Arc::new(AtomicU64::new(0));
        let viewed = Arc::new(AtomicBool::new(true));
        let worker_viewed = Arc::clone(&viewed);
        let worker_discarded = Arc::clone(&discarded);
        let worker_attach_gen = Arc::clone(&attach_gen);

        let worker_handle = thread::Builder::new()
            .name("sot-fe-attach-worker".to_string())
            .spawn(move || {
                run_worker::<E>(
                    endpoint,
                    lane,
                    controller_id,
                    fe_down_to_handle,
                    fe_down,
                    cols,
                    rows,
                    msg_rx,
                    worker_msg_tx,
                    sink,
                    worker_queued_bytes,
                    recorded_bytes,
                    last_input_outcome,
                    take_epoch_pub,
                    worker_discarded,
                    worker_attach_gen,
                    Arc::clone(&worker_viewed),
                    headless,
                );
            })?;

        Ok(Self { msg_tx, ingress_bytes, ingress_bound, attach_gen, discarded, queued_bytes, viewed, worker_handle: Some(worker_handle), _endpoint: PhantomData })
    }

    /// Whether this worker's pane is on screen. A worker pauses on a down link whether viewed or not, and resumes only when the link is up AND the client is viewed.
    pub fn set_viewed(&self, viewed: bool) {
        self.viewed.store(viewed, Ordering::Release);
    }

    /// A sink that consumes [`WorkerEvent::Output`] bytes calls this with
    /// however many it just consumed, releasing the episode reader's own
    /// backpressure by that much — the ONLY way [`QueuedBytes`] ever goes
    /// down (see that type's own doc). `fe_client_io::FeAttachClient::
    /// pump` calls this exactly where the pre-extraction module's own
    /// `queued_bytes.sub` call was.
    pub fn ack_output_consumed(&self, n: usize) {
        self.queued_bytes.sub(n);
    }

    /// Reserves `bytes.len()` against this worker's own ingress bound
    /// BEFORE the command channel is ever touched — but the bound limits
    /// ACCUMULATION, never a single send: an input is admitted
    /// unconditionally whenever the queue is otherwise idle, even one
    /// bigger than the bound itself (a large paste, or a long `sot-fe
    /// type --stdin`, must never silently vanish just for being bigger
    /// than a bound sized for steady-state typing); it is refused only
    /// when something is ALREADY reserved and admitting this one too
    /// would push the total past the bound (see this module's own top
    /// doc). Empty input is a silent no-op: it types nothing and would
    /// otherwise let a caller enqueue an unlimited number of zero-charge
    /// commands despite the advertised memory bound. A send to an
    /// already-exited worker is not itself a refusal — the reservation
    /// is released the moment the undelivered message (returned by the
    /// channel) drops, same as any other disposal.
    pub fn send_input(&self, bytes: Vec<u8>) -> Result<(), IngressRefused> {
        if bytes.is_empty() {
            return Ok(());
        }
        let n = bytes.len();
        let mut cur = self.ingress_bytes.load(Ordering::Acquire);
        loop {
            // The bound limits ACCUMULATION, not a single send: admit
            // unconditionally whenever nothing is currently reserved (an
            // idle queue), even if this one input alone exceeds the
            // bound — a large paste, or a long `sot-fe type --stdin`,
            // must never silently vanish just because it is bigger than
            // the bound a caller chose for steady-state typing. Refuse
            // only when something is ALREADY reserved and admitting this
            // one too would push the total past the bound.
            if cur > 0 && cur.saturating_add(n) > self.ingress_bound {
                self.discarded.fetch_add(1, Ordering::AcqRel);
                return Err(IngressRefused);
            }
            match self.ingress_bytes.compare_exchange_weak(cur, cur.saturating_add(n), Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
        let reservation = IngressReservation { ingress_bytes: Arc::clone(&self.ingress_bytes), n };
        // Whether this send succeeds or fails, the reservation is now
        // owned by the message: on success the worker's own loop drops
        // it once consumed (or discards it mid-drain, releasing it just
        // the same); on failure the returned `SendError` carries the
        // message right back here, and letting the `Result` drop
        // unused drops it immediately.
        let gen = self.attach_gen.load(Ordering::Acquire);
        if self.msg_tx.send(WorkerMsg::Input(bytes, reservation, gen)).is_err() {
            // The worker has exited: the input is never delivered.
            self.discarded.fetch_add(1, Ordering::AcqRel);
        }
        Ok(())
    }

    /// How many inputs have been discarded since the last one that was
    /// delivered (see the `discarded` field).
    pub fn inputs_discarded(&self) -> usize {
        self.discarded.load(Ordering::Acquire)
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.msg_tx.send(WorkerMsg::Resize(cols, rows));
    }

    /// Ruling (a): the ONE quit dispatcher — idempotent, latched across a
    /// reconnect in flight, unchanged from the pre-extraction module.
    pub fn request_quit(&self, reason: &str) {
        let _ = self.msg_tx.send(WorkerMsg::Quit(reason.to_string()));
    }

    /// Sends `Shutdown` (same as [`Drop`]) and waits up to `wait` for the
    /// worker thread to exit — unchanged bounded-join shape from the
    /// pre-extraction `FeAttachClient::shutdown`.
    pub fn shutdown(&mut self, wait: Duration) -> bool {
        let _ = self.msg_tx.send(WorkerMsg::Shutdown);
        let Some(handle) = self.worker_handle.take() else {
            return true;
        };
        let deadline = Instant::now() + wait;
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                eprintln!("attach_worker: worker thread still alive {wait:?} after Shutdown was sent");
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = handle.join();
        true
    }

    /// A worker handle with no worker thread behind it — `fe_client_io`'s
    /// own unit tests build an [`fe_client_io::FeAttachClient`] by struct
    /// literal (this crate's child-module-privacy pattern) and need SOME
    /// value for this field; nothing here is ever sent to. `pub(crate)`:
    /// test-only, never a real caller's construction path.
    #[cfg(test)]
    pub(crate) fn stub_for_test() -> Self {
        let (msg_tx, _msg_rx) = mpsc::channel::<WorkerMsg>();
        Self {
            msg_tx,
            ingress_bytes: Arc::new(AtomicUsize::new(0)),
            ingress_bound: usize::MAX,
            attach_gen: Arc::new(AtomicU64::new(0)),
            discarded: Arc::new(AtomicUsize::new(0)),
            queued_bytes: Arc::new(QueuedBytes::new()),
            viewed: Arc::new(AtomicBool::new(true)),
            worker_handle: None,
            _endpoint: PhantomData,
        }
    }
}

impl<E: Endpoint> Drop for AttachWorker<E> {
    fn drop(&mut self) {
        let _ = self.msg_tx.send(WorkerMsg::Shutdown);
    }
}

/// What the worker keeps of the commands it reads while it is NOT attached
/// (backing off, polling the supervisor, or in the handshake): one latched
/// `Quit`, the newest `Resize`, and a count of the keystrokes it had to
/// drop because there was no live attach connection to send them on.
pub(super) struct Held {
    quit: Option<String>,
    resize: Option<(u16, u16)>,
    discarded: Arc<AtomicUsize>,
}

// -----------------------------------------------------------------------
// Pure-logic unit tests. Most of this module's behavior needs a real
// supervisor + capsule process to attach to (`tests/fe_client.rs`'s own
// real-process harness -- L1-unix LU3c ungated it to run on Linux too,
// against a real socket lane, exactly like Windows); these pieces are pure
// enough to test directly on every platform this module now compiles on.
// LU6a's `checkpointed` test is the one exception: it builds an
// `FeAttachClient` by hand (a private-field struct literal, legal from
// this child module) rather than a real worker thread, so it can drive
// `pump` -- the SAME path a real worker's events go through -- with a
// synthetic `WorkerEvent::Checkpoint` and no process/network dependency.
// -----------------------------------------------------------------------


mod converge;
mod episode;
mod lane_io;
mod quit;
mod run;
mod steady;

pub(crate) use lane_io::FrameReader;
pub(crate) use quit::run_end_run_and_wait;
use converge::*;
use episode::*;
use lane_io::*;
use quit::*;
use run::*;
use steady::*;

#[cfg(test)]
mod support_tests;
#[cfg(test)]
mod ingress_tests;
#[cfg(test)]
mod converge_tests;
#[cfg(test)]
mod steady_tests;
