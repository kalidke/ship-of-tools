//! Steady state: the byte-accounted reader queue, the attach reader and the frame and input handlers.

use crate::lane::client::{Client, Endpoint};
use crate::attach_client::rules::{
    self, InputWireOutcome, OutstandingSlot, QuitDispatcher,
    ReconnectDecision, ReconnectState, Role, TakeAction, TakeTransaction,
};
use crate::lane::wire::{
    self, AttachClient, AttachServer, DecodedFrame, ResizeRefusedReason,
    SupervisorPhase, TakeRefusedReason,
};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::*;


// -----------------------------------------------------------------------
// Backpressure accounting shared between the episode reader and `pump`
// -----------------------------------------------------------------------

/// The byte-account behind Codex review round finding 7 ("the FE STOPS
/// READING THE PIPE"): the episode reader (`run_attach_reader`)
/// increments it on every `Output` byte it reads (`CheckpointChunk`
/// bytes are never counted — those are consumed earlier, by
/// `attach_and_collect_checkpoint` on the same connection, before this
/// reader exists) and blocks its own next `read()` while it is at or above
/// [`READER_QUEUE_CAP_BYTES`]; [`crate::attach_client::client::FeAttachClient::
/// pump`] decrements it, via [`AttachWorker::ack_output_consumed`], as it
/// actually consumes `Output` bytes — the ONLY place it is ever
/// decremented, which is what makes the accounting real (see
/// [`AttachWorker::ack_output_consumed`]'s own doc).
///
/// switch-latency Phase 1: the reader used to enforce the block with a
/// `thread::sleep(20ms)` poll loop. The count itself stays a plain
/// `AtomicUsize` — `add`/`load` need no lock, and stay on the hot path
/// `pump` and the reader already run on every `Output` byte — but
/// [`Self::wait_below_cap`] parks on a `Condvar` instead of polling,
/// woken by [`Self::sub`] the instant draining brings the count back
/// under the cap, or by [`Self::notify_stop`] at episode teardown. The
/// `Mutex`/`Condvar` pair here is a pure doorbell, the same shape
/// `deadline::run_with_deadline` uses for its own watchdog: the waiter
/// re-checks the real condition (the atomic count, or `stop`) every time
/// it wakes, under the SAME lock a notifier must hold to signal, so a
/// notify that lands between the waiter's check and its park can never
/// be lost — see [`Self::wait_below_cap`] for the full argument.
pub(super) struct QueuedBytes {
    count: AtomicUsize,
    gate: Mutex<()>,
    room: Condvar,
}

impl QueuedBytes {
    pub(super) fn new() -> Self {
        Self { count: AtomicUsize::new(0), gate: Mutex::new(()), room: Condvar::new() }
    }

    /// The reader's own increment — paired with [`Self::sub`].
    pub(super) fn add(&self, n: usize) {
        self.count.fetch_add(n, Ordering::AcqRel);
    }

    /// `pump`'s own decrement, the ONLY place this ever goes down. Wakes
    /// any reader parked in [`Self::wait_below_cap`] — a no-op lock/
    /// notify when nobody is waiting (the common case: the queue rarely
    /// reaches [`READER_QUEUE_CAP_BYTES`] at all).
    pub(super) fn sub(&self, n: usize) {
        self.count.fetch_sub(n, Ordering::AcqRel);
        drop(self.gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        self.room.notify_all();
    }

    fn load(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    /// Blocks the calling (reader) thread until either the count drops
    /// below `cap` or `stop` is set — woken by [`Self::sub`]'s notify or
    /// by [`Self::notify_stop`], never a periodic poll. Every wake
    /// re-checks both real conditions before deciding to park again, so
    /// a notify racing the initial check is never lost: it can only land
    /// while this thread holds `gate` (already past the point where it
    /// would go on to wait) or while it is genuinely parked inside
    /// `Condvar::wait` (where it is, by definition, listening).
    fn wait_below_cap(&self, cap: usize, stop: &AtomicBool) {
        self.wait_below_cap_traced(cap, stop, || {})
    }

    /// Same as [`Self::wait_below_cap`], plus `on_wake` — called once
    /// per loop iteration (initial entry, and again each time
    /// `Condvar::wait` returns). Lets tests count how many times this
    /// waits wakes instead of trusting wall-clock latency (which
    /// scheduler noise on a loaded CI runner makes an unreliable witness
    /// either way) to tell a genuinely notified wait apart from a poll.
    pub(super) fn wait_below_cap_traced(&self, cap: usize, stop: &AtomicBool, on_wake: impl Fn()) {
        let mut guard = self.gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            on_wake();
            if self.load() < cap || stop.load(Ordering::Acquire) {
                return;
            }
            guard = self.room.wait(guard).unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Wakes a reader parked in [`Self::wait_below_cap`] because `stop`
    /// was just set on the same `AtomicBool` it polls — called once, at
    /// episode teardown, immediately after that store (an `AtomicBool`
    /// write alone has nothing to make a parked `Condvar::wait` notice
    /// it).
    pub(super) fn notify_stop(&self) {
        drop(self.gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        self.room.notify_all();
    }
}

// -----------------------------------------------------------------------
// The episode-scoped attach-connection reader
// -----------------------------------------------------------------------

/// Blocking read, decode via its own `FrameSplitter`, forward each frame
/// to the worker. Gates its OWN next `read()` call on the SHARED
/// [`QueuedBytes`] counter (Codex review round, finding 7: "the FE STOPS
/// READING THE PIPE" is now real — this is the SAME `Arc` clone
/// [`AttachWorker::ack_output_consumed`] decrements, not a private,
/// immediately-released one). `stop` breaks the backpressure wait itself
/// (a notified wait `cancel()` cannot reach); a normal teardown sets it and calls
/// [`QueuedBytes::notify_stop`] just before calling `cancel()`.
/// `Keepalive` is answered directly here (bounced back byte-identical),
/// never round-tripped through the worker.
pub(super) fn run_attach_reader<E: Endpoint>(
    conn: Arc<E::Client>,
    mut reader: FrameReader,
    tx: Sender<WorkerMsg>,
    queued_bytes: Arc<QueuedBytes>,
    stop: Arc<AtomicBool>,
) {
    loop {
        queued_bytes.wait_below_cap(READER_QUEUE_CAP_BYTES, &stop);
        if stop.load(Ordering::Acquire) {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(3600); // steady-state: no artificial read deadline; EOF/cancel end it
        let frame = match reader.next_frame::<E>(&conn, deadline) {
            Ok(f) => f,
            Err(_) => {
                let _ = tx.send(WorkerMsg::ReaderDone);
                return;
            }
        };
        if let DecodedFrame::Keepalive { nonce } = &frame {
            let _ = conn.write_all(&wire::encode_keepalive(*nonce));
            continue;
        }
        if let DecodedFrame::AttachServer(AttachServer::Output { bytes }) = &frame {
            // The ONLY increment of the shared byte-account — paired
            // with `AttachWorker::ack_output_consumed`'s own decrement.
            queued_bytes.add(bytes.len());
        }
        if tx.send(WorkerMsg::Frame(frame)).is_err() {
            return;
        }
    }
}

// -----------------------------------------------------------------------
// Steady state
// -----------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub(super) fn run_steady_state<E: Endpoint + Sync>(
    endpoint: &E,
    cmd_rx: &Receiver<WorkerMsg>,
    emit: &dyn Fn(WorkerEvent),
    h: &str,
    attach_conn: &Arc<E::Client>,
    supervisor_conn: E::Client,
    sup_reader: FrameReader,
    take: &mut TakeTransaction,
    take_intent: &mut TakeIntent,
    outstanding: &mut OutstandingSlot,
    quit: &mut QuitDispatcher,
    reconnect: &mut ReconnectState,
    cols: &mut u16,
    rows: &mut u16,
    take_epoch: &mut u64,
    controller_id: &str,
    voyage: &str,
    last_liveness_poll: &mut Instant,
    recorded_bytes: &Arc<AtomicU64>,
    last_input_outcome: &Arc<Mutex<Option<InputOutcome>>>,
    take_epoch_pub: &Arc<AtomicU64>,
    attached_gen: u64,
    discarded: &Arc<AtomicUsize>,
) -> SteadyOutcome {
    // Whether the pane header currently shows the missed-probe line below:
    // one missed `Status` (a stalled link, not a dead supervisor) must not
    // retitle the pane until the next reattach -- the next answered probe
    // restores "attached" so the header tells the truth again.
    let mut probe_missed = false;
    // Codex review (PR 254): `probe_missed` alone records that a probe was
    // missed, NOT that the blink is still what the pane shows. Anything
    // else this loop reaches (a quit canceling outstanding input, a take
    // transaction) emits its own status in between, and restoring
    // "attached" over THAT is the same clobber this change exists to
    // remove. So the loop's every emit goes through here first and the
    // restore is conditional on the blink still being the last word.
    let last_status = std::cell::RefCell::new(String::new());
    let emit = |e: WorkerEvent| {
        if let WorkerEvent::Status(s) = &e {
            *last_status.borrow_mut() = s.clone();
        }
        emit(e)
    };
    // The lane lives here, not in the loop's own hands: a probe thread holds
    // the lock for its round trip, so the input path never waits on it.
    let sup_lane = Mutex::new(SupLane {
        conn: supervisor_conn,
        reader: sup_reader,
        redial_at: None,
        redial_backoff: SUPERVISOR_REDIAL_INITIAL,
    });
    // Scoped: a probe still in flight when this returns is joined, within
    // its own budgets, before the lane drops.
    thread::scope(|s| {
        let mut probe: Option<thread::ScopedJoinHandle<'_, Result<SupervisorPhase, LaneError>>> = None;
        loop {
            let msg = cmd_rx.recv_timeout(WORKER_TICK);
            // Harvest a finished probe. A Quit also waits out an unfinished
            // one, because `run_quit` needs the lane it holds -- bounded by
            // the probe's own budgets, never longer than the inline probe
            // used to hold this whole loop.
            let quitting = matches!(msg, Ok(WorkerMsg::Quit(_)));
            if let Some(job) = probe.take_if(|j| quitting || j.is_finished()) {
                match job.join().unwrap_or_else(|p| std::panic::resume_unwind(p)) {
                    Ok(phase) => {
                    // The supervisor answered -- unambiguously NOT
                    // absent/unresponsive; the voyage pipe question
                    // never even arises (ruling (d), finding 8).
                    reconnect.clear_unresponsive();
                    if let ReconnectDecision::Terminal(reason) = reconnect.classify_supervisor_phase(phase) {
                        return SteadyOutcome::Terminal(format!("supervisor: {reason:?}"));
                    }
                    if probe_missed {
                        probe_missed = false;
                        if *last_status.borrow() == PROBE_MISSED_STATUS {
                            emit(WorkerEvent::Status("attached".to_string()));
                        }
                    }
                    }
                    Err(_) => {
                    // Codex review round, finding 8: the attach
                    // connection is DEMONSTRABLY alive right now (we are
                    // actively reading it in this very loop) — the
                    // voyage-absent half of the AND condition is false
                    // by construction here, so the health window must
                    // NEVER be consulted from this branch. "The capsule
                    // survives headless": keep going.
                    reconnect.clear_unresponsive();
                    if !probe_missed {
                        probe_missed = true;
                        emit(WorkerEvent::Status(PROBE_MISSED_STATUS.to_string()));
                    }
                    }
                }
            }
            match msg {
                Ok(WorkerMsg::Shutdown) => return SteadyOutcome::Shutdown,
                Ok(WorkerMsg::Input(_, _reservation, gen)) if gen < attached_gen => {
                    // Sent before this attach: never delivered.
                    discarded.fetch_add(1, Ordering::AcqRel);
                    emit(WorkerEvent::InputsDiscarded);
                }
                Ok(WorkerMsg::Input(bytes, _reservation, _)) => {
                    // A current input that is queued or sent ends the count
                    // of discarded ones; one dropped whole adds to it.
                    let mut dropped = false;
                    match take.role() {
                    Role::Watching => {
                        for action in take.on_input_while_watching(&bytes) {
                            dropped |= apply_input_action::<E>(action, attach_conn, controller_id, &emit, discarded);
                        }
                    }
                    Role::Taking | Role::Resizing => {
                        for action in take.on_input_while_pending(&bytes) {
                            dropped |= apply_input_action::<E>(action, attach_conn, controller_id, &emit, discarded);
                        }
                    }
                    Role::Driving => {
                        if outstanding.outstanding().is_some() {
                            // Ruling (b), Codex review round finding 5: an
                            // input already outstanding queues the next one
                            // rather than dropping it.
                            for action in take.queue_while_driving(&bytes) {
                                dropped |= apply_input_action::<E>(action, attach_conn, controller_id, &emit, discarded);
                            }
                        } else {
                            send_new_input::<E>(attach_conn, outstanding, *take_epoch, controller_id, voyage, bytes);
                        }
                    }
                    }
                    if !dropped && discarded.swap(0, Ordering::AcqRel) > 0 {
                        emit(WorkerEvent::InputsDiscarded);
                    }
                }
                Ok(WorkerMsg::Resize(c, r)) => {
                    *cols = c;
                    *rows = r;
                    // An ad hoc resize while already DRIVING is sent
                    // immediately (unrelated to the take transaction's own
                    // resize, which is awaited alone before ANYTHING else
                    // goes out — see ruling (b)'s lockstep fix). Not sent
                    // while WATCHING/TAKING/RESIZING: a watcher cannot
                    // correct the geometry until it holds the pen, and while
                    // RESIZING a second resize would itself violate lockstep.
                    if take.role() == Role::Driving {
                        let frame = AttachClient::Resize { cols: c, rows: r };
                        if let Ok(enc) = wire::encode_attach_client(&frame) {
                            let _ = write_bounded::<E>(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
                        }
                    }
                }
                Ok(WorkerMsg::Quit(reason)) => {
                    // `run_quit` now loops internally (entirely within this
                    // call) until the dispatcher reaches a terminal state --
                    // see its own doc for why verification cannot depend on
                    // this steady-state loop running again (the attach
                    // connection dies with the capsule end_run tears down).
                    // The harvest above joined any in-flight probe, so the lock is free.
                    let mut guard = sup_lane.lock().unwrap();
                    let l = &mut *guard;
                    run_quit::<E>(endpoint, &mut l.conn, &mut l.reader, h, voyage, reason, quit, outstanding, &emit);
                }
                Ok(WorkerMsg::Frame(frame)) => {
                    match handle_attach_frame::<E>(
                        frame, attach_conn, take, take_intent, outstanding, take_epoch, controller_id, voyage, *cols,
                        *rows, &emit, recorded_bytes, last_input_outcome, take_epoch_pub,
                    ) {
                        FrameOutcome::ReattachRequested => return SteadyOutcome::Reconnect,
                        FrameOutcome::Handled | FrameOutcome::Ignored => {}
                    }
                }
                Ok(WorkerMsg::ReaderDone) => {
                    return SteadyOutcome::Reconnect;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return SteadyOutcome::Shutdown,
            }

            let now = Instant::now();
            quit.tick(now);
            if quit.should_exit() {
                return SteadyOutcome::QuitEnded;
            }
            for action in take.tick_checkpoint_retry(now) {
                apply_single_take_action::<E>(action, attach_conn, controller_id, &emit);
            }

            if probe.is_none() && now.duration_since(*last_liveness_poll) >= LIVENESS_POLL_INTERVAL {
                *last_liveness_poll = now;
                match thread::Builder::new()
                    .name("sot-fe-sup-probe".to_string())
                    .spawn_scoped(s, || probe_supervisor_lane::<E>(endpoint, h, &mut *sup_lane.lock().unwrap()))
                {
                    Ok(job) => probe = Some(job),
                    // No thread, no probe: a missed-probe status would claim
                    // what nobody observed, so this round reports nothing and
                    // the next one tries again.
                    Err(e) => eprintln!("attach_worker: liveness probe thread failed to start ({e}); skipping this round"),
                }
            }
        }
    })
}

pub(super) enum FrameOutcome {
    Handled,
    Ignored,
    /// `take_refused{not_attached}` — the caller ends this episode; the
    /// take transaction's queue is discarded and counted at the reset.
    ReattachRequested,
}

pub(super) fn mint_idem_key() -> [u8; 16] {
    let mut buf = [0u8; 16];
    let _ = getrandom::fill(&mut buf);
    buf
}

pub(super) fn send_wire_input<E: Endpoint>(attach_conn: &E::Client, controller_id: &str, take_epoch: u64, idem_key: [u8; 16], payload: Vec<u8>) {
    let frame = AttachClient::Input { controller_id: controller_id.to_string(), take_epoch, idem_key, payload };
    if let Ok(enc) = wire::encode_attach_client(&frame) {
        let _ = write_bounded::<E>(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
    }
}

/// Records a FRESH outstanding input (a new idem key) and sends it —
/// the ordinary path for both a first Driving-idle keystroke and a
/// flushed queue entry.
pub(super) fn send_new_input<E: Endpoint>(
    attach_conn: &E::Client,
    outstanding: &mut OutstandingSlot,
    take_epoch: u64,
    controller_id: &str,
    voyage: &str,
    bytes: Vec<u8>,
) {
    let idem_key = outstanding.record(voyage.to_string(), take_epoch, bytes.clone(), mint_idem_key);
    send_wire_input::<E>(attach_conn, controller_id, take_epoch, idem_key, bytes);
}

/// [`apply_single_take_action`] for the actions a typed input produced:
/// an input dropped whole is counted in the worker's one discard counter.
/// Returns whether this action was that whole drop.
pub(super) fn apply_input_action<E: Endpoint>(
    action: TakeAction,
    attach_conn: &E::Client,
    controller_id: &str,
    emit: &dyn Fn(WorkerEvent),
    discarded: &AtomicUsize,
) -> bool {
    if action == TakeAction::InputDropped {
        discarded.fetch_add(1, Ordering::AcqRel);
        emit(WorkerEvent::InputsDiscarded);
        true
    } else {
        apply_single_take_action::<E>(action, attach_conn, controller_id, emit);
        false
    }
}

/// Dispatches one `TakeAction`. `SendInput` no longer exists as a
/// variant (Codex review round, finding 3: flushing the queue is never
/// bundled with `take_ok`'s own actions) — every input send in this
/// module goes through [`send_new_input`]/[`send_wire_input`] instead,
/// called from the specific points ruling (b)/(c) pin (after
/// `resize_ok`, after an outstanding reply resolves while DRIVING).
pub(super) fn apply_single_take_action<E: Endpoint>(action: TakeAction, attach_conn: &E::Client, controller_id: &str, emit: &dyn Fn(WorkerEvent)) {
    match action {
        TakeAction::SendTake => {
            let frame = AttachClient::Take { controller_id: controller_id.to_string() };
            if let Ok(enc) = wire::encode_attach_client(&frame) {
                let _ = write_bounded::<E>(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
            }
        }
        TakeAction::SendResize { cols, rows } => {
            let frame = AttachClient::Resize { cols, rows };
            if let Ok(enc) = wire::encode_attach_client(&frame) {
                let _ = write_bounded::<E>(attach_conn, &enc, Instant::now() + WRITE_BUDGET);
            }
        }
        TakeAction::QueueDiscarded => {
            emit(WorkerEvent::Status("input discarded \u{2014} the pen never arrived in time".to_string()));
        }
        // Counted by `apply_input_action`, which has the counter.
        TakeAction::InputDropped => {}
        TakeAction::GeometryUnrepresentable => {
            emit(WorkerEvent::Status("window size not representable by this session".to_string()));
        }
        TakeAction::PenLost => {
            emit(WorkerEvent::Status("lost the pen".to_string()));
        }
        TakeAction::Reattach => {
            // Handled by the caller propagating `FrameOutcome::
            // ReattachRequested` up to `SteadyOutcome::
            // Reconnect` -- nothing to send here (the
            // server already does not recognize this connection as
            // attached).
        }
    }
}

/// After the pen is fully secured (`resize_ok`, or `resize_refused{out_
/// of_budget}` which still keeps it): send whatever is owed next, per
/// `take_intent` — the reconnect resend (SAME key), the stale retry
/// (NEW key under the now-current epoch), or the ordinary queued flush
/// (fresh key). Ruling (c), Codex review round finding 6.
pub(super) fn flush_after_pen_secured<E: Endpoint>(
    attach_conn: &E::Client,
    take: &mut TakeTransaction,
    take_intent: &mut TakeIntent,
    outstanding: &mut OutstandingSlot,
    take_epoch: u64,
    controller_id: &str,
    voyage: &str,
) {
    match std::mem::replace(take_intent, TakeIntent::Ordinary) {
        TakeIntent::ReconnectResend => {
            if let Some(o) = outstanding.outstanding() {
                send_wire_input::<E>(attach_conn, controller_id, o.take_epoch, o.idem_key, o.bytes.clone());
            }
        }
        TakeIntent::StaleRetry => {
            let resolution = outstanding.apply_outcome(InputWireOutcome::RefusedStale, take_epoch, mint_idem_key);
            if let rules::OutstandingResolution::RetryNewEpoch { idem_key } = resolution {
                if let Some(o) = outstanding.outstanding() {
                    send_wire_input::<E>(attach_conn, controller_id, take_epoch, idem_key, o.bytes.clone());
                }
            }
        }
        TakeIntent::Ordinary => {
            if let Some(bytes) = take.take_queued() {
                send_new_input::<E>(attach_conn, outstanding, take_epoch, controller_id, voyage, bytes);
            }
        }
    }
}

/// After an outstanding input's reply resolves (`InputRecorded`/
/// `InputDeliveryUnknown`) while still DRIVING: flush whatever the take
/// transaction queued behind it (ruling (b), Codex review round finding
/// 5's own "dispatch queued bytes after the outstanding reply").
pub(super) fn flush_next_driving_input<E: Endpoint>(
    attach_conn: &E::Client,
    take: &mut TakeTransaction,
    outstanding: &mut OutstandingSlot,
    take_epoch: u64,
    controller_id: &str,
    voyage: &str,
) {
    if take.role() != Role::Driving {
        return;
    }
    if let Some(bytes) = take.take_queued() {
        send_new_input::<E>(attach_conn, outstanding, take_epoch, controller_id, voyage, bytes);
    }
}

/// Dispatches one incoming attach-lane frame (unsolicited `Output` or a
/// reply to whatever the worker most recently sent).
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_attach_frame<E: Endpoint>(
    frame: DecodedFrame,
    attach_conn: &E::Client,
    take: &mut TakeTransaction,
    take_intent: &mut TakeIntent,
    outstanding: &mut OutstandingSlot,
    take_epoch: &mut u64,
    controller_id: &str,
    voyage: &str,
    cols: u16,
    rows: u16,
    emit: &dyn Fn(WorkerEvent),
    recorded_bytes: &Arc<AtomicU64>,
    last_input_outcome: &Arc<Mutex<Option<InputOutcome>>>,
    take_epoch_pub: &Arc<AtomicU64>,
) -> FrameOutcome {
    match frame {
        DecodedFrame::AttachServer(AttachServer::Output { bytes }) => {
            emit(WorkerEvent::Output(bytes));
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::TakeOk { take_epoch: epoch }) => {
            *take_epoch = epoch;
            // Published even for a re-take this worker granted itself
            // (a reconnect) — a headless caller pins this across a write.
            take_epoch_pub.store(epoch, Ordering::Release);
            if *take_intent == TakeIntent::ReconnectResend {
                if let rules::ReconnectResendDecision::Cancel { canceled } =
                    outstanding.resend_after_reconnect(voyage, epoch)
                {
                    emit(WorkerEvent::Status(format!(
                        "input canceled \u{2014} the voyage changed ({} byte(s) lost)",
                        canceled.bytes.len()
                    )));
                    *take_intent = TakeIntent::Ordinary;
                }
            }
            for action in take.on_take_ok(cols, rows) {
                apply_single_take_action::<E>(action, attach_conn, controller_id, emit);
            }
            // A HEADLESS take's own `on_take_ok` (above) skips RESIZING
            // entirely and promotes straight to DRIVING, returning no
            // actions — the ordinary `ResizeOk` frame that would normally
            // trigger the queue flush never arrives for it, so flush right
            // here whenever `on_take_ok` already landed in DRIVING. A
            // NON-headless transaction never reaches DRIVING from this
            // call (it always lands in RESIZING), so this is a no-op for
            // the frontend's own client.
            if take.role() == Role::Driving {
                flush_after_pen_secured::<E>(attach_conn, take, take_intent, outstanding, *take_epoch, controller_id, voyage);
            }
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::TakeRefused { reason }) => {
            match reason {
                TakeRefusedReason::NotAttached => {
                    let actions = take.on_take_refused_not_attached();
                    let reattach = actions.contains(&TakeAction::Reattach);
                    for action in actions {
                        apply_single_take_action::<E>(action, attach_conn, controller_id, emit);
                    }
                    if reattach {
                        return FrameOutcome::ReattachRequested;
                    }
                }
                TakeRefusedReason::CheckpointInFlight => {
                    for action in take.on_take_refused_checkpoint_in_flight(Instant::now()) {
                        apply_single_take_action::<E>(action, attach_conn, controller_id, emit);
                    }
                }
            }
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::ResizeOk) => {
            take.on_resize_ok();
            flush_after_pen_secured::<E>(attach_conn, take, take_intent, outstanding, *take_epoch, controller_id, voyage);
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::ResizeRefused { reason }) => {
            let was_resizing = take.role() == Role::Resizing;
            for action in take.on_resize_refused(reason) {
                apply_single_take_action::<E>(action, attach_conn, controller_id, emit);
            }
            if was_resizing && reason == ResizeRefusedReason::OutOfBudget {
                // `on_resize_refused` already promoted RESIZING ->
                // DRIVING for this refusal -- the pen is still held, so
                // whatever was queued behind the take-ok flushes exactly
                // as it would after a real `resize_ok`.
                flush_after_pen_secured::<E>(attach_conn, take, take_intent, outstanding, *take_epoch, controller_id, voyage);
            }
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::InputRecorded) => {
            // Capture the length BEFORE `apply_outcome` clears it — ADR
            // 0042 amendment's own observable: "success requires
            // `InputRecorded` covering the WHOLE payload," which a caller
            // verifies by summing recorded lengths, not by counting acks.
            let recorded_len = outstanding.outstanding().map(|o| o.bytes.len());
            let _ = outstanding.apply_outcome(InputWireOutcome::Recorded, *take_epoch, mint_idem_key);
            if let Some(len) = recorded_len {
                recorded_bytes.fetch_add(len as u64, Ordering::AcqRel);
            }
            if let Ok(mut g) = last_input_outcome.lock() {
                *g = Some(InputOutcome::Recorded);
            }
            flush_next_driving_input::<E>(attach_conn, take, outstanding, *take_epoch, controller_id, voyage);
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::InputRefusedStale) => {
            // Ruling (c), Codex review round finding 6: re-take FIRST;
            // the new key is minted once the fresh `take_ok` arrives
            // (see the `TakeOk` arm above and `flush_after_pen_secured`'s
            // own `StaleRetry` handling). ADR 0042 amendment: a headless
            // caller treats THIS send as failed and does not wait for that
            // retry (the daemon never retries an input on its own) — the
            // retry below is this client's own ordinary wire-protocol
            // behavior, unrelated to whatever the headless caller does
            // once it observes the outcome recorded here.
            if let Ok(mut g) = last_input_outcome.lock() {
                *g = Some(InputOutcome::RefusedStale);
            }
            *take_intent = TakeIntent::StaleRetry;
            for action in take.retake_while_driving() {
                apply_single_take_action::<E>(action, attach_conn, controller_id, emit);
            }
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::InputDeliveryUnknown) => {
            let res = outstanding.apply_outcome(InputWireOutcome::DeliveryUnknown, *take_epoch, mint_idem_key);
            if matches!(res, rules::OutstandingResolution::Unknown) {
                emit(WorkerEvent::Status("input delivery unknown".to_string()));
                if let Ok(mut g) = last_input_outcome.lock() {
                    *g = Some(InputOutcome::DeliveryUnknown);
                }
            }
            flush_next_driving_input::<E>(attach_conn, take, outstanding, *take_epoch, controller_id, voyage);
            FrameOutcome::Handled
        }
        DecodedFrame::AttachServer(AttachServer::AttachRefused { .. })
        | DecodedFrame::AttachServer(AttachServer::HelloOk { .. })
        | DecodedFrame::AttachServer(AttachServer::HelloRefused { .. })
        | DecodedFrame::AttachServer(AttachServer::CheckpointChunk { .. })
        // ADR 0046 decision 3: the capsule (lane/attach_proto/) speaks these
        // to any v3 client, but this worker still asks for v2 (unchanged
        // by this lane -- see lane/wire/'s own history) and so never
        // receives them for real; listed here only so this match stays
        // exhaustive over `AttachServer`. Consuming them belongs to
        // B3b2/B3b3, not this arm.
        | DecodedFrame::AttachServer(AttachServer::PenSnapshot { .. })
        | DecodedFrame::AttachServer(AttachServer::PenChanged { .. })
        | DecodedFrame::AttachServer(AttachServer::Geometry { .. }) => FrameOutcome::Ignored,
        DecodedFrame::Keepalive { .. } => FrameOutcome::Ignored, // answered by the reader thread directly
        DecodedFrame::MgmtRequest(_)
        | DecodedFrame::MgmtReply(_)
        | DecodedFrame::AttachClient(_)
        | DecodedFrame::SupervisorRequest(_)
        | DecodedFrame::SupervisorReply(_) => FrameOutcome::Ignored,
    }
}
