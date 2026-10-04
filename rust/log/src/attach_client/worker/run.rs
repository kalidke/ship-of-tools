//! The worker thread: `run_worker`, its held-input bookkeeping and the retry and link-pause waits.

use crate::lane::client::Endpoint;
use crate::attach_client::rules::{
    FeDownBaseline, OutstandingSlot, QuitDispatcher,
    ReconnectState, TakeTransaction,
};
use crate::lane::wire::{self};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;


// -----------------------------------------------------------------------
// The worker thread
// -----------------------------------------------------------------------

/// What triggered the take-on-first-input `take` currently in flight (or
/// about to be), so the post-`take_ok`/`resize_ok` flush knows what to
/// send once it is safe to. `Ordinary` covers the common case (a real
/// keystroke); the other two exist ONLY to correctly discharge ruling
/// (c)'s exactly-once contract (Codex review round, finding 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TakeIntent {
    /// Flush whatever `TakeTransaction::take_queued` returns, minting a
    /// FRESH idem key.
    Ordinary,
    /// Resend the retained `OutstandingInput` under the SAME key, once
    /// the fresh epoch is known (ruling (c): "resends THE SAME KEY").
    ReconnectResend,
    /// `input_refused_stale`'s own retry: mint a NEW key under the fresh
    /// epoch now that it is known (ruling (c): "re-sent under the new
    /// epoch with a NEW key").
    StaleRetry,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_worker<E: Endpoint>(
    endpoint: E,
    lane: String,
    controller_id: String,
    fe_down_to_handle: String,
    mut fe_down: FeDownBaseline,
    initial_cols: u16,
    initial_rows: u16,
    cmd_rx: Receiver<WorkerMsg>,
    msg_tx: Sender<WorkerMsg>,
    sink: impl Fn(WorkerEvent) + Send + 'static,
    queued_bytes: Arc<QueuedBytes>,
    recorded_bytes: Arc<AtomicU64>,
    last_input_outcome: Arc<Mutex<Option<InputOutcome>>>,
    take_epoch_pub: Arc<AtomicU64>,
    discarded: Arc<AtomicUsize>,
    attach_gen: Arc<AtomicU64>,
    viewed: Arc<AtomicBool>,
    headless: bool,
) where
    E: Send + Sync + 'static,
    E::Client: 'static,
{
    let mut reconnect = ReconnectState::new();
    let mut take = if headless { TakeTransaction::new_headless() } else { TakeTransaction::new() };
    let mut outstanding = OutstandingSlot::new();
    let mut quit = QuitDispatcher::new();
    let mut take_intent = TakeIntent::Ordinary;
    let mut cols = initial_cols;
    let mut rows = initial_rows;
    let mut voyage_uuid: Option<String> = None;
    // Owned by the worker, not re-derived per call: a rendering client
    // gets a deadline once; headless never tolerates EndedNoRespawn.
    let mut first_attach_deadline =
        (!headless).then(|| Instant::now() + FIRST_ATTACH_ENDED_NO_RESPAWN_BOUND);
    let mut take_epoch: u64 = 0;
    let mut shutdown = false;
    // ADR 0041 "attach proto v2 bound to checkpoint v2" (Codex round on
    // #194): the version THIS episode's `hello` asks for. Starts at the
    // newest this build speaks; a `hello_refused` naming a version this
    // client ALSO speaks (only ever `ATTACH_PROTO_V1`, an older capsule)
    // downgrades this and retries the whole episode immediately, rather
    // than failing outright -- survives across `continue 'episodes` on
    // purpose, unlike the per-episode locals above it.
    let mut preferred_attach_proto = wire::ATTACH_PROTO_V2;
    // Ruling (a), Codex review round finding 2: a `Quit` requested
    // while no supervisor connection is currently open (mid-backoff, or
    // before the first one ever connects) is LATCHED here rather than
    // dropped — applied the instant a fresh supervisor connection
    // exists, since `end_run` needs only that lane, never the attach
    // lane.
    let mut held = Held { quit: None, resize: None, discarded: Arc::clone(&discarded) };

    let emit = |e: WorkerEvent| sink(e);

    'episodes: while !shutdown {
        emit(WorkerEvent::Status("connecting\u{2026}".to_string()));

        let (supervisor_conn, sup_reader, voyage, voyage_conn) = match reach_supervisor::<E>(&endpoint, &lane, &cmd_rx, &mut reconnect, &mut held, &mut quit, &mut outstanding, first_attach_deadline, &viewed, &emit, &voyage_uuid) {
            Ok(v) => v,
            Err(EpisodeExit::Retry) => continue 'episodes,
            Err(EpisodeExit::End) => return,
        };

        // A resize read while not attached is applied before this attach's
        // hello, so the capsule sees the size the pane has now.
        if let Some((c, r)) = held.resize.take() {
            cols = c;
            rows = r;
        }

        if voyage_uuid.as_deref() != Some(voyage.as_str()) {
            cancel_input_for_new_voyage(&voyage, take_epoch, &mut outstanding, &mut take, &discarded, &emit);
            take_intent = TakeIntent::Ordinary;
            voyage_uuid = Some(voyage.clone());
        }

        let (attach_identity, attach_reader, checkpoint) = match attach_voyage::<E>(&endpoint, &voyage_conn, &cmd_rx, &mut reconnect, &mut held, &mut preferred_attach_proto, &controller_id, &emit) {
            Ok(v) => v,
            Err(EpisodeExit::Retry) => continue 'episodes,
            Err(EpisodeExit::End) => return,
        };

        // switch-latency Phase 1: the checkpoint (and the "attached"
        // status right behind it) now land BEFORE the mgmt-identity
        // round trip below, not after. That round trip is a THROWAWAY
        // connection + full challenge purely to word the attach notice —
        // best-effort commentary on an already-attached client, never a
        // precondition for showing one — and it used to sit ahead of the
        // checkpoint emit, delaying first paint by a whole extra
        // connection + challenge for no reason the client's own state
        // needed.
        // First completed attach: never tolerate EndedNoRespawn again.
        first_attach_deadline = None;
        // Stamped BEFORE the checkpoint is emitted: a headless caller may
        // send as soon as it applies the checkpoint, and that input must
        // read as current. An input sent earlier read the older value.
        let attached_gen = attach_gen.fetch_add(1, Ordering::AcqRel) + 1;
        announce_attach(checkpoint, attach_identity, &mut reconnect, &mut take, &discarded, &mut fe_down, &fe_down_to_handle, &emit);

        resume_outstanding_input::<E>(&voyage, take_epoch, &mut outstanding, &mut take, &mut take_intent, &voyage_conn, &controller_id, &emit);

        let Some((shared_conn, episode_stop, reader_thread)) = spawn_episode_reader::<E>(voyage_conn, attach_reader, &msg_tx, &queued_bytes, &emit) else {
            return;
        };
        let mut last_liveness_poll = Instant::now();

        // --- steady state ------------------------------------------
        let episode_result = run_steady_state::<E>(
            &endpoint,
            &cmd_rx,
            &emit,
            &lane,
            &shared_conn,
            supervisor_conn,
            sup_reader,
            &mut take,
            &mut take_intent,
            &mut outstanding,
            &mut quit,
            &mut reconnect,
            &mut cols,
            &mut rows,
            &mut take_epoch,
            &controller_id,
            &voyage,
            &mut last_liveness_poll,
            &recorded_bytes,
            &last_input_outcome,
            &take_epoch_pub,
            attached_gen,
            &discarded,
        );

        stop_episode_reader::<E>(&episode_stop, &queued_bytes, shared_conn, reader_thread);

        match episode_result {
            SteadyOutcome::Shutdown => {
                shutdown = true;
            }
            SteadyOutcome::QuitEnded => {
                emit(WorkerEvent::ShouldExit);
                shutdown = true;
            }
            SteadyOutcome::Terminal(reason) => {
                emit(WorkerEvent::Terminal(reason));
                return;
            }
            SteadyOutcome::Reconnect => {
                match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), &mut held) {
                    WaitOutcome::Shutdown => shutdown = true,
                    WaitOutcome::Continue => {}
                }
            }
        }
    }
}

pub(super) enum SteadyOutcome {
    Shutdown,
    QuitEnded,
    Terminal(String),
    /// An episode end -- the NEXT episode resets the take transaction to
    /// Watching, counting any queued input as discarded.
    Reconnect,
}



/// The one reader of a command read while not attached. `Shutdown` is the
/// only message that ends the wait; a `Quit` is latched (applied the moment
/// a supervisor connection exists), a `Resize` is held for the next attach,
/// an `Input` is counted and dropped (its reservation drops with it), and
/// frames the worker's own reader sent are moot.
pub(super) fn hold(msg: WorkerMsg, held: &mut Held) -> Option<WaitOutcome> {
    match msg {
        WorkerMsg::Shutdown => return Some(WaitOutcome::Shutdown),
        WorkerMsg::Quit(reason) => {
            held.quit.get_or_insert(reason);
        }
        WorkerMsg::Resize(c, r) => held.resize = Some((c, r)),
        WorkerMsg::Input(..) => {
            held.discarded.fetch_add(1, Ordering::AcqRel);
        }
        _ => {}
    }
    None
}

pub(super) enum WaitOutcome {
    Continue,
    Shutdown,
}

/// Blocks up to `wait` for a `Shutdown` command, otherwise returns after
/// the backoff elapses so the next episode can start. Every other command
/// read during the wait goes through [`hold`]: a `Quit` is LATCHED rather
/// than dropped (Codex review round, finding 2) — the top of the next
/// episode applies it the moment a supervisor connection exists, since
/// `end_run` needs only that lane — and an `Input` has no live connection
/// to be sent on, so it is counted and dropped.
pub(super) fn wait_for_retry_or_shutdown(cmd_rx: &Receiver<WorkerMsg>, wait: Duration, held: &mut Held) -> WaitOutcome {
    let deadline = Instant::now() + wait;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return WaitOutcome::Continue;
        }
        match cmd_rx.recv_timeout(remaining.min(WORKER_TICK)) {
            Ok(msg) => {
                if let Some(outcome) = hold(msg, held) {
                    return outcome;
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return WaitOutcome::Shutdown,
        }
    }
}

/// The pause for a down link: one status line, then a tick-by-tick
/// wait that reads the channel through [`hold`] (a `Resize` is kept for the
/// next attach, an `Input` is counted) until the endpoint's link is up AND
/// the client is viewed. A tick always passes first, so an endpoint that
/// reports `LinkDown` while `link_up()` is true cannot spin.
pub(super) fn pause_for_link<E: Endpoint>(
    endpoint: &E,
    cmd_rx: &Receiver<WorkerMsg>,
    held: &mut Held,
    viewed: &AtomicBool,
    emit: &dyn Fn(WorkerEvent),
) -> WaitOutcome {
    emit(WorkerEvent::Status("host offline, waiting for the link".to_string()));
    loop {
        match cmd_rx.recv_timeout(WORKER_TICK) {
            Ok(msg) => {
                if let Some(outcome) = hold(msg, held) {
                    return outcome;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return WaitOutcome::Shutdown,
        }
        if endpoint.link_up() && viewed.load(Ordering::Acquire) {
            return WaitOutcome::Continue;
        }
    }
}

pub(super) fn iso_now() -> String {
    // No chrono dependency in this crate; a plain RFC-3339-shaped UTC
    // stamp built from `SystemTime` is sufficient here since this string
    // is carried opaquely (fe_client::build_fe_down_marker never parses
    // it) and only ever compared/read by a human or a future durable
    // reader that already tolerates the daemon's own ISO-8601 strings.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant's algorithm) -- avoids a chrono
    // dependency for one timestamp string.
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m2 = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m2 <= 2 { y + 1 } else { y };
    format!("{y:04}-{m2:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}
