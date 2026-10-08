//! The steps of one attach episode, in the order run_worker calls them: reach the supervisor, attach, announce, resume input, start and stop the episode reader.

use super::*;
use crate::identity::challenge::PeerAuthOutcome;
use crate::lane::client::Client;
use crate::attach_client::rules::{self, FeDownBaseline, OutstandingSlot, QuitDispatcher, ReconnectDecision, ReconnectState, Role, TakeTransaction};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;

/// Why an episode step stopped short of its result. `Retry` starts the next
/// episode (it was `continue 'episodes`); `End` ends the worker (it was
/// `return` or `break 'episodes`, one exit because the episode loop is the
/// last statement of `run_worker`).
pub(super) enum EpisodeExit {
    Retry,
    End,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn reach_supervisor<E: Endpoint>(
    endpoint: &E,
    lane: &str,
    cmd_rx: &Receiver<WorkerMsg>,
    reconnect: &mut ReconnectState,
    held: &mut Held,
    quit: &mut QuitDispatcher,
    outstanding: &mut OutstandingSlot,
    first_attach_deadline: Option<Instant>,
    viewed: &AtomicBool,
    emit: &dyn Fn(WorkerEvent),
    voyage_uuid: &Option<String>,
) -> Result<(E::Client, FrameReader, String, E::Client), EpisodeExit> {
    let supervisor_ready = dial_and_converge::<E>(endpoint, lane, cmd_rx, reconnect, held, quit, outstanding, first_attach_deadline, viewed, emit)?;

    // Ruling (d), Codex review round finding 8: only when the
    // supervisor lane is ALSO absent/unresponsive this round does the
    // health window even get consulted -- a reachable voyage pipe
    // (the capsule surviving headless) clears it unconditionally.
    let (supervisor_conn, sup_reader, voyage, voyage_conn) = match supervisor_ready {
        Some(v) => v,
        None => {
            // Finding 2: resolved FRESH here, not cached from the top
            // of the episode -- `converge_on_ready` may have polled
            // for a long while before reporting `LaneDown`/failing to
            // connect at all. Decision 6: the probe uses `voyage_uuid`
            // -- the voyage id the supervisor last reported to THIS
            // client, carried across episodes -- never a pointer file.
            match on_supervisor_absent_or_unresponsive::<E>(&endpoint, reconnect, &lane, voyage_uuid.as_deref(), Instant::now()) {
                ReconnectDecision::Terminal(reason) => {
                    emit(WorkerEvent::Terminal(format!("supervisor lane unreachable: {reason:?}")));
                    return Err(EpisodeExit::End);
                }
                ReconnectDecision::Retry => {
                    emit(WorkerEvent::Status("supervisor lane not answering \u{2014} retrying\u{2026}".to_string()));
                    match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), held) {
                        WaitOutcome::Shutdown => return Err(EpisodeExit::End),
                        WaitOutcome::Continue => return Err(EpisodeExit::Retry),
                    }
                }
            }
        }
    };
    Ok((supervisor_conn, sup_reader, voyage, voyage_conn))
}

#[allow(clippy::too_many_arguments)]
fn dial_and_converge<E: Endpoint>(
    endpoint: &E,
    lane: &str,
    cmd_rx: &Receiver<WorkerMsg>,
    reconnect: &mut ReconnectState,
    held: &mut Held,
    quit: &mut QuitDispatcher,
    outstanding: &mut OutstandingSlot,
    first_attach_deadline: Option<Instant>,
    viewed: &AtomicBool,
    emit: &dyn Fn(WorkerEvent),
) -> Result<Option<(E::Client, FrameReader, String, E::Client)>, EpisodeExit> {
    // --- supervisor lane: hello (build identity) once, then converge
    // on Ready (ADR 0043 decision 28) -------------------------------
    let supervisor_ready = match connect_supervisor_lane::<E>(&endpoint, &lane) {
        Ok((conn, _proven)) => {
            match converge_on_ready::<E>(
                &endpoint,
                conn,
                FrameReader::new(),
                &lane,
                &cmd_rx,
                reconnect,
                held,
                quit,
                outstanding,
                first_attach_deadline,
                &viewed,
                &emit,
            ) {
                ReadyOutcome::Ready { conn, sup_reader, voyage_id, voyage_conn } => {
                    Some((conn, sup_reader, voyage_id, voyage_conn))
                }
                ReadyOutcome::Terminal(msg) => {
                    emit(WorkerEvent::Terminal(msg));
                    return Err(EpisodeExit::End);
                }
                ReadyOutcome::ShouldExit => {
                    emit(WorkerEvent::ShouldExit);
                    return Err(EpisodeExit::End);
                }
                ReadyOutcome::Shutdown => return Err(EpisodeExit::End),
                ReadyOutcome::LaneDown => None,
            }
        }
        Err(LaneError::Protocol(p)) if p.contains("version_skew") => {
            // `classify_hello_refused_version_skew` is always `Terminal`
            // (no `Retry` arm exists to discard it into) -- emit and
            // return directly rather than matching a foregone answer.
            reconnect.classify_hello_refused_version_skew();
            emit(WorkerEvent::Terminal(
                "the row's supervisor refused this client (another lane protocol, or a supervisor from before the protocol-only gate); end the row and recreate it".to_string(),
            ));
            return Err(EpisodeExit::End);
        }
        Err(LaneError::Protocol(p)) if p.contains("foreign") => {
            match reconnect.classify_foreign() {
                ReconnectDecision::Terminal(reason) => {
                    emit(WorkerEvent::Terminal(format!("supervisor lane: {reason:?}")));
                    return Err(EpisodeExit::End);
                }
                ReconnectDecision::Retry => unreachable!("classify_foreign is always terminal"),
            }
        }
        Err(LaneError::Io(e)) if is_access_denied(&e) => {
            emit(WorkerEvent::Terminal("supervisor lane: access denied".to_string()));
            return Err(EpisodeExit::End);
        }
        // ADR 0045 decision 4: the two bridge-only outcomes beyond
        // an ordinary `Io` — a refusal is terminal, its code named
        // in the pane line; the two uncertain arms retry, clearing
        // the health window's clock first so transport uncertainty
        // is never charged to it.
        Err(LaneError::Refused { code, detail }) => {
            let msg = if code == "no_bridge" {
                "this daemon has no bridge — it predates the lane bridge (ADR 0045)".to_string()
            } else {
                format!("daemon refused the lane ({code}): {detail}")
            };
            emit(WorkerEvent::Terminal(msg));
            return Err(EpisodeExit::End);
        }
        Err(LaneError::LinkDown) => {
            reconnect.clear_unresponsive();
            match pause_for_link(endpoint, &cmd_rx, held, &viewed, &emit) {
                WaitOutcome::Shutdown => return Err(EpisodeExit::End),
                WaitOutcome::Continue => return Err(EpisodeExit::Retry),
            }
        }
        Err(e @ (LaneError::Unreachable(_) | LaneError::Undetermined(_))) => {
            reconnect.clear_unresponsive();
            let msg = match &e {
                LaneError::Unreachable(d) => format!("daemon unreachable — retrying ({d})"),
                LaneError::Undetermined(_) => "daemon could not identify the lane — retrying".to_string(),
                _ => unreachable!("matched above"),
            };
            emit(WorkerEvent::Status(msg));
            match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), held) {
                WaitOutcome::Shutdown => return Err(EpisodeExit::End),
                WaitOutcome::Continue => return Err(EpisodeExit::Retry),
            }
        }
        Err(_) => None,
    };
    Ok(supervisor_ready)
}

pub(super) fn cancel_input_for_new_voyage(
    voyage: &str,
    take_epoch: u64,
    outstanding: &mut OutstandingSlot,
    take: &mut TakeTransaction,
    discarded: &AtomicUsize,
    emit: &dyn Fn(WorkerEvent),
) {
        // A reset landed underneath us: any outstanding input from
        // the OLD voyage is canceled, never replayed into the new
        // one -- and reported, never silently (finding 6). Keyed on
        // the id `converge_on_ready` confirmed against the
        // supervisor's own `Status` reply, not a locally cached
        // pointer read.
        let mut canceled_input = false;
        if let rules::ReconnectResendDecision::Cancel { canceled } =
            outstanding.resend_after_reconnect(&voyage, take_epoch)
        {
            canceled_input = true;
            emit(WorkerEvent::Status(format!(
                "input canceled \u{2014} the voyage changed ({} byte(s) lost)",
                canceled.bytes.len()
            )));
        }
        let mut lost = take.reset_to_watching();
        if canceled_input {
            lost += 1;
        }
        if lost > 0 {
            discarded.fetch_add(lost, Ordering::AcqRel);
            emit(WorkerEvent::InputsDiscarded);
        }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn attach_voyage<E: Endpoint>(
    endpoint: &E,
    voyage_conn: &E::Client,
    cmd_rx: &Receiver<WorkerMsg>,
    reconnect: &mut ReconnectState,
    held: &mut Held,
    preferred_attach_proto: &mut u32,
    controller_id: &str,
    emit: &dyn Fn(WorkerEvent),
) -> Result<((u32, u64), FrameReader, Vec<u8>), EpisodeExit> {
    // --- attach lane: the voyage pipe is already connected --
    // `converge_on_ready` made that ONE attempt itself (finding 1),
    // folded into its own Status-polling loop rather than an
    // independent retry here.
    let attach_identity = match endpoint.authenticate_server(&voyage_conn) {
        PeerAuthOutcome::Authenticated(a) => (a.pid, a.created),
        PeerAuthOutcome::Foreign => {
            emit(WorkerEvent::Terminal("voyage pipe: foreign".to_string()));
            return Err(EpisodeExit::End);
        }
        PeerAuthOutcome::Undetermined => {
            match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), held) {
                WaitOutcome::Shutdown => return Err(EpisodeExit::End),
                WaitOutcome::Continue => return Err(EpisodeExit::Retry),
            }
        }
    };

    let mut attach_reader = FrameReader::new();
    match attach_lane_hello::<E>(&voyage_conn, &mut attach_reader, *preferred_attach_proto) {
        Ok(HelloOutcome::Accepted) => {}
        Ok(HelloOutcome::RetryAt(fallback)) => {
            // The capsule does not speak `preferred_attach_proto` (an
            // older build, predating attach proto v2 / the
            // scrollback ring) but DOES speak a version this client
            // also understands. Retry the whole episode immediately
            // -- a fresh connection, since the refused one is already
            // closed server-side -- at that version rather than
            // failing outright: v1 still works, just without
            // history.
            *preferred_attach_proto = fallback;
            return Err(EpisodeExit::Retry);
        }
        Err(e) => match e {
            LaneError::Protocol(p) if p.contains("version_skew") => {
                emit(WorkerEvent::Terminal(
                    "attach hello: version_skew".to_string(),
                ));
                return Err(EpisodeExit::End);
            }
            _ => {
                match wait_for_retry_or_shutdown(
                    &cmd_rx,
                    reconnect.retry_with_backoff(),
                    held,
                ) {
                    WaitOutcome::Shutdown => return Err(EpisodeExit::End),
                    WaitOutcome::Continue => return Err(EpisodeExit::Retry),
                }
            }
        },
    }
    let checkpoint =
        match attach_and_collect_checkpoint::<E>(&voyage_conn, &mut attach_reader, &controller_id) {
            Ok(c) => c,
            Err(e) => {
                // A refusal the capsule NAMED is the one failure here
                // a user can act on, so it reaches the row's status
                // line (the same `WorkerEvent::Status` path every
                // other retrying lane failure uses). Every other
                // error keeps the silent retry: the pane's own
                // "connecting..." already says what is happening.
                if let LaneError::AttachRefused(reason) = e {
                    emit(WorkerEvent::Status(attach_refused_text(reason).to_string()));
                }
                match wait_for_retry_or_shutdown(&cmd_rx, reconnect.retry_with_backoff(), held) {
                    WaitOutcome::Shutdown => return Err(EpisodeExit::End),
                    WaitOutcome::Continue => return Err(EpisodeExit::Retry),
                }
            }
        };
    Ok((attach_identity, attach_reader, checkpoint))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn announce_attach(
    checkpoint: Vec<u8>,
    attach_identity: (u32, u64),
    take: &mut TakeTransaction,
    discarded: &AtomicUsize,
    fe_down: &mut FeDownBaseline,
    fe_down_to_handle: &str,
    emit: &dyn Fn(WorkerEvent),
) {
    emit(WorkerEvent::Checkpoint(checkpoint));
    emit(WorkerEvent::Status("attached".to_string()));

    // The attach notice names the leg from the attach connection's own
    // daemon-authenticated identity (ADR 0045: the daemon's OS-level
    // observation of the voyage process, bound to this connection).
    // A second, throwaway mgmt-lane dial used to re-prove the same
    // (pid, created) here and blocked input until it finished.
    emit(WorkerEvent::Notice(rules::attach_notice_text(&format!("{}", attach_identity.1))));

    // Anything still queued was typed before this attach: discarded
    // and counted, never delivered.
    let lost = take.reset_to_watching();
    if lost > 0 {
        discarded.fetch_add(lost, Ordering::AcqRel);
        emit(WorkerEvent::InputsDiscarded);
    }

    // Ruling (f): fe_down marker on every attach after the first.
    let now_iso = iso_now();
    if let Some(marker) = fe_down.marker_for_attach(&fe_down_to_handle, &now_iso) {
        emit(WorkerEvent::FeDownMarker(marker));
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resume_outstanding_input<E: Endpoint>(
    voyage: &str,
    take_epoch: u64,
    outstanding: &mut OutstandingSlot,
    take: &mut TakeTransaction,
    take_intent: &mut TakeIntent,
    voyage_conn: &E::Client,
    controller_id: &str,
    emit: &dyn Fn(WorkerEvent),
) {
    // Ruling (c), Codex review round finding 6: resume any input
    // left outstanding from a prior connection, within this same
    // voyage -- kick off the SAME take-on-first-input transaction
    // that a real keystroke would, so `resize` then the retained
    // frame flow through the identical lockstep-respecting path.
    match outstanding.resend_after_reconnect(&voyage, take_epoch) {
        rules::ReconnectResendDecision::Resend { .. } => {
            *take_intent = TakeIntent::ReconnectResend;
            if take.role() == Role::Watching {
                let actions = take.on_input_while_watching(&[]);
                for action in actions {
                    apply_single_take_action::<E>(action, &voyage_conn, &controller_id, &emit);
                }
            }
        }
        rules::ReconnectResendDecision::Cancel { canceled } => {
            emit(WorkerEvent::Status(format!(
                "input canceled \u{2014} the voyage changed ({} byte(s) lost)",
                canceled.bytes.len()
            )));
        }
        rules::ReconnectResendDecision::None => {}
    }
}

pub(super) fn spawn_episode_reader<E: Endpoint>(
    voyage_conn: E::Client,
    attach_reader: FrameReader,
    msg_tx: &Sender<WorkerMsg>,
    queued_bytes: &Arc<QueuedBytes>,
    emit: &dyn Fn(WorkerEvent),
) -> Option<(Arc<E::Client>, Arc<AtomicBool>, thread::JoinHandle<()>)>
where
    E: Send + Sync + 'static,
    E::Client: 'static,
{
    // Spawn the episode-scoped reader thread for the attach
    // connection's steady-state stream.
    let shared_conn = Arc::new(voyage_conn);
    let reader_tx = msg_tx.clone();
    let reader_conn = Arc::clone(&shared_conn);
    let episode_stop = Arc::new(AtomicBool::new(false));
    let reader_stop = Arc::clone(&episode_stop);
    let reader_queued_bytes = Arc::clone(&queued_bytes);
    let reader_thread = match thread::Builder::new()
        .name("sot-fe-attach-reader".to_string())
        .spawn(move || run_attach_reader::<E>(reader_conn, attach_reader, reader_tx, reader_queued_bytes, reader_stop))
    {
        Ok(jh) => jh,
        Err(e) => {
            // Codex review round, finding 13: a reader that could
            // not even be spawned must never look "attached" -- no
            // thread exists to ever deliver TakeOk, output, or input
            // acknowledgements.
            emit(WorkerEvent::Terminal(format!("failed to start the attach reader thread: {e}")));
            return None;
        }
    };
    Some((shared_conn, episode_stop, reader_thread))
}

pub(super) fn stop_episode_reader<E: Endpoint>(
    episode_stop: &AtomicBool,
    queued_bytes: &QueuedBytes,
    shared_conn: Arc<E::Client>,
    reader_thread: thread::JoinHandle<()>,
) {
    // Tear down this episode's connections before deciding what's
    // next. The stop flag interrupts the reader's OWN backpressure
    // wait (a `QueuedBytes` condvar wait, not a blocked read --
    // `cancel()` alone cannot reach it); `notify_stop` wakes it
    // promptly since the plain `store` above has nothing else to
    // make a parked `Condvar::wait` notice it. `cancel()` then
    // unblocks a blocked read so the thread observes an error, sends
    // the now-moot `ReaderDone` (harmlessly ignored; a fresh reader
    // is not spawned until the next successful attach), and exits;
    // only then do both `Arc` clones drop and the pipe handle
    // actually closes.
    episode_stop.store(true, Ordering::Release);
    queued_bytes.notify_stop();
    shared_conn.cancel();
    let _ = reader_thread.join();
    drop(shared_conn);
}
