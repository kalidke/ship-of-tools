//! Per-connection threads: the reaper, lifecycle events, and each connection's reader and writer.

use super::*;

/// Switch-latency Phase 1 (c): ping this server's own wake callback, if
/// [`PipeServer::set_wake`] ever registered one — called ONLY after a
/// push to `events_tx` already succeeded, so a caller woken by it always
/// finds the real event already queued for `events()`'s own `try_recv`.
/// A no-op for the (common, unaffected) case nothing ever registered
/// one, e.g. the supervisor lane.
pub(super) fn notify_wake(shared: &Arc<ServerShared>) {
    if let Some(wake) = shared.activity_wake.get() {
        wake();
    }
}

/// Deliver one lifecycle event (`Accepted`/`Sent`/`Closed`/`AcceptError`)
/// RELIABLY — see the module doc's "Reliable lifecycle delivery" section
/// for the full contract this implements.
pub(super) fn send_lifecycle_event(shared: &Arc<ServerShared>, evt: LaneEvent) {
    let checkpoint = enqueue_checkpoint(&evt);
    let mut item = evt;
    let mut last = "";
    loop {
        if let Some((id, step, detail)) = &checkpoint {
            if last.is_empty() {
                shared.progress.note(*id, step, format_args!("begin{detail}"));
            }
        }
        let sent = shared.events_tx.try_send(item);
        let result = match &sent {
            Ok(()) => "ok",
            Err(TrySendError::Full(_)) => "full",
            Err(TrySendError::Disconnected(_)) => "disconnected",
        };
        if let Some((id, step, detail)) = &checkpoint {
            if result != last {
                shared.progress.note(*id, step, format_args!("{result}{detail}"));
            }
        }
        last = result;
        match sent {
            Ok(()) => {
                notify_wake(shared);
                return;
            }
            Err(TrySendError::Disconnected(_)) => return,
            Err(TrySendError::Full(v)) => {
                item = v;
                if shared.dropping.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(EVENTS_RETRY_INTERVAL);
            }
        }
    }
}

/// The checkpoint of a lifecycle event's enqueue: its connection, step and the marker identity of a `Sent`.
fn enqueue_checkpoint(evt: &LaneEvent) -> Option<(Option<ConnId>, &'static str, String)> {
    match evt {
        LaneEvent::Accepted(id) => Some((Some(*id), "accepted.enqueue", String::new())),
        LaneEvent::Closed(id, _) => Some((Some(*id), "closed.enqueue", String::new())),
        LaneEvent::Sent(id, marker) => Some((Some(*id), "sent.enqueue", format!(" marker={marker}"))),
        LaneEvent::AcceptError(_) => Some((None, "accept_error.enqueue", String::new())),
        LaneEvent::Bytes(..) => None,
    }
}

/// Request teardown for `conn_id`, at most once: every caller (an
/// explicit `close`, the reader's own EOF signal, a writer's own
/// `WriteFile`-error signal) races the SAME connection's `flag` via
/// `compare_exchange`; only the winner enqueues a [`ReaperMsg`], so
/// repeated or concurrent requests can never grow the reaper's bounded
/// inbox past one entry per connection.
pub(super) fn request_teardown(
    shared: &Arc<ServerShared>,
    conn_id: ConnId,
    flag: &AtomicBool,
    reason: ClosedReason,
) {
    if flag
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        shared.progress.note(Some(conn_id), "teardown.enqueue", "begin");
        let sent = shared.reaper_tx.send(ReaperMsg::Torn(conn_id, reason));
        shared.progress.note(
            Some(conn_id),
            "teardown.enqueue",
            if sent.is_ok() { "ok" } else { "disconnected" },
        );
    }
}

/// Notify the consumer that a just-connected instance could not be fully
/// registered (write-slot creation, or a worker's `thread::Builder::spawn`,
/// failed) — `Accepted` then an immediate `Closed(Error(..))`, both via
/// the reliable path, so the consumer's own per-connection bookkeeping is
/// created and discarded cleanly rather than never learning this
/// connection existed. Not terminal to the accept loop — the next
/// connection attempt is unaffected.
pub(super) fn report_registration_failure(shared: &Arc<ServerShared>, what: &str, e: impl std::fmt::Display) {
    let conn_id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    send_lifecycle_event(shared, LaneEvent::Accepted(conn_id));
    send_lifecycle_event(
        shared,
        LaneEvent::Closed(conn_id, ClosedReason::Error(format!("{what}: {e}"))),
    );
}

/// Signal the reaper to shut down against `deadline`, once and without blocking: a repeat call never extends the first
/// deadline or queues a second message. The inbox keeps a slot for it beyond the live connections' `Torn` messages.
pub(super) fn signal_shutdown(shared: &ServerShared, deadline: Instant) {
    if !shared.shutdown_sent.swap(true, Ordering::AcqRel) {
        let _ = shared.reaper_tx.try_send(ReaperMsg::Shutdown(deadline));
        shared.progress.note(None, "reaper.shutdown", "signalled");
    }
}

/// The most messages one reaper pass takes before it polls the pending pairs, so continuous inbox traffic cannot
/// starve a pair that is already being joined.
const INTAKE_BATCH: usize = 32;

/// Where a claimed connection is: workers being joined, `Closed` waiting for channel room, instance awaiting recycle.
enum Stage {
    Joining,
    Closing,
    Recycling,
}

/// A claimed connection: its slots stay owned through completion; the registry stays the only closer of its handle.
struct Pending {
    id: ConnId,
    /// `None` is a shutdown claim: nothing is published for it.
    reason: Option<ClosedReason>,
    claimed_at: Instant,
    stage: Stage,
    joins: PendingJoins,
    panicked: bool,
    /// The `Closed` retained after both joins until the channel has room (or the consumer is gone).
    closed: Option<LaneEvent>,
    /// The last result noted for that `Closed`'s enqueue; empty before the first attempt.
    last_enqueue: &'static str,
    registry_id: u64,
    raw: SendableHandle,
    _slots: (Arc<IoSlot>, Arc<IoSlot>),
}

/// The at most one reaper-origin terminal `AcceptError` not yet published, with its last noted result.
#[derive(Default)]
struct Staged {
    error: Option<LaneEvent>,
    last: &'static str,
}

/// Claim `conn_id` if it is still registered, once: under the `conns` lock, cancel both slots (so the cancellation
/// precedes any `close_all`) and move the pair to a pending record. Only the reaper calls this. `reason: None` is a
/// shutdown claim; `shutdown` is the deadline every pair is held to once one was signalled.
fn claim(
    shared: &Arc<ServerShared>,
    conn_id: ConnId,
    reason: Option<ClosedReason>,
    shutdown: Option<Instant>,
) -> Option<Pending> {
    let conn = {
        let mut conns = shared.conns.lock().unwrap();
        let live = conns.get(&conn_id)?;
        let read_pending = live.read_slot.cancel_registered(&shared.instances, live.registry_id);
        let write_pending = live.write_slot.cancel_registered(&shared.instances, live.registry_id);
        shared.progress.note(
            Some(conn_id),
            "claim",
            match &reason {
                Some(reason) => format!("reaper reason={reason:?}"),
                None => "reaper reason=shutdown".to_string(),
            },
        );
        shared.progress.note(
            Some(conn_id),
            "cancel.registered",
            format_args!("read_genuinely_pending={read_pending} write_genuinely_pending={write_pending}"),
        );
        conns.remove(&conn_id)?
    };
    let ConnHandle {
        raw,
        registry_id,
        read_slot,
        write_slot,
        sender,
        reader_jh,
        writer_jh,
        ..
    } = conn;
    drop(sender); // unblocks a writer idle-waiting on `recv` with nothing queued
    shared.progress.note(Some(conn_id), "reader.join.begin", "begin");
    shared.progress.note(Some(conn_id), "writer.join.begin", "begin");
    let own = Instant::now() + shared.controls.teardown_deadline();
    let deadline = shutdown.map_or(own, |shutdown| own.min(shutdown));
    Some(Pending {
        id: conn_id,
        reason,
        claimed_at: Instant::now(),
        stage: Stage::Joining,
        joins: PendingJoins::new(reader_jh, writer_jh, deadline),
        panicked: false,
        closed: None,
        last_enqueue: "",
        registry_id,
        raw,
        _slots: (read_slot, write_slot),
    })
}

fn note_poll(shared: &Arc<ServerShared>, pending: &mut Pending, poll: &JoinPoll) {
    let id = pending.id;
    for (worker, ok) in &poll.joined {
        let step = match worker {
            Worker::Reader => "reader.join.end",
            Worker::Writer => "writer.join.end",
        };
        shared.progress.note(Some(id), step, if *ok { "ok" } else { "panic" });
        if !ok {
            pending.panicked = true;
            shared
                .progress
                .note(Some(id), "pending.panicked", format_args!("worker={}", worker.name()));
        }
    }
    if let Some(unfinished) = &poll.expired {
        shared.progress.note(
            Some(id),
            "pending.expired",
            format_args!("worker={}", worker_label(unfinished)),
        );
    }
    if poll.failed() {
        shared.teardown_failed.store(true, Ordering::Release);
        poll.report("sot-pipe", id, pending.claimed_at.elapsed());
    }
}

/// One nonblocking attempt to publish `event`: true when it is retired -- sent, or nothing could ever read it (the
/// consumer gone, or the server dropping) -- and false with `event` put back in `slot` when the channel is full.
fn try_publish(
    shared: &Arc<ServerShared>,
    event: LaneEvent,
    slot: &mut Option<LaneEvent>,
    last: &mut &'static str,
) -> bool {
    let checkpoint = enqueue_checkpoint(&event);
    if let Some((id, step, _)) = &checkpoint {
        if last.is_empty() {
            shared.progress.note(*id, step, "begin");
        }
    }
    let sent = shared.events_tx.try_send(event);
    let result = match &sent {
        Ok(()) => "ok",
        Err(TrySendError::Full(_)) => "full",
        Err(TrySendError::Disconnected(_)) => "disconnected",
    };
    if let Some((id, step, _)) = &checkpoint {
        if result != *last {
            shared.progress.note(*id, step, result);
        }
    }
    *last = result;
    match sent {
        Ok(()) => {
            notify_wake(shared);
            true
        }
        Err(TrySendError::Disconnected(_)) => true,
        Err(TrySendError::Full(event)) => {
            if shared.dropping.load(Ordering::Acquire) {
                return true;
            }
            *slot = Some(event);
            false
        }
    }
}

/// One pass over a pending connection. True when it is retired and its instance is no longer charged.
fn poll_pending(shared: &Arc<ServerShared>, pending: &mut Pending, staged: &mut Staged, now: Instant) -> bool {
    if let Stage::Joining = pending.stage {
        let poll = pending.joins.poll(now);
        note_poll(shared, pending, &poll);
        if !poll.done {
            return false;
        }
        shared.progress.note(Some(pending.id), "pending.done", "joined");
        pending.stage = Stage::Closing;
        if let Some(reason) = pending.reason.take() {
            let reason = if pending.panicked {
                ClosedReason::Error("connection worker panicked".into())
            } else {
                reason
            };
            pending.closed = Some(LaneEvent::Closed(pending.id, reason));
        }
    }
    if let Stage::Closing = pending.stage {
        if let Some(event) = pending.closed.take() {
            let mut slot = None;
            let mut last = pending.last_enqueue;
            let retired = try_publish(shared, event, &mut slot, &mut last);
            pending.last_enqueue = last;
            if !retired {
                pending.closed = slot;
                return false;
            }
        }
        pending.stage = Stage::Recycling;
    }
    // The instance remains registered through pending teardown. Close-event retirement precedes recycling; deferred
    // recycling retains its capacity charge. Recycle failure retains the dead instance and stages AcceptError without
    // blocking other worker polls. Every handle operation still checks registry liveness after close_all.
    if staged.error.is_some() {
        return false;
    }
    shared
        .controls
        .barrier_point(&shared.progress, Some(pending.id), "recycle.barrier");
    if let Err(message) = recycle_checked(shared, pending.registry_id, pending.raw) {
        stop_accept_loop(shared);
        shared.accept_cv.notify_all();
        staged.error = Some(LaneEvent::AcceptError(message));
        staged.last = "";
    }
    true
}

/// Publish the staged terminal `AcceptError` without blocking; it stays staged while the channel is full.
fn publish_staged(shared: &Arc<ServerShared>, staged: &mut Staged) {
    if let Some(event) = staged.error.take() {
        let mut slot = None;
        let mut last = staged.last;
        try_publish(shared, event, &mut slot, &mut last);
        staged.error = slot;
        staged.last = last;
    }
}

/// The reaper claims registered connections once, cancels both directions and polls every pending pair, joining only
/// finished workers. Expiry reports unfinished workers still owned; panic reports a completed panicked join. Both latch
/// failed teardown. Phase-one registered pairs use this same owner; only never-registered gated workers may be joined
/// locally. Closed follows both joins.
///
/// Each pass takes a bounded batch of messages, claims every live connection once phase one or a shutdown was
/// signalled, then polls every pending pair -- so one stuck pair, or a channel too full for one `Closed`, never stalls
/// another connection's teardown.
pub(super) fn reaper_loop(shared: Arc<ServerShared>, rx: Receiver<ReaperMsg>) {
    let mut pending: Vec<Pending> = Vec::new();
    let mut staged = Staged::default();
    let mut shutdown: Option<Instant> = None;
    loop {
        let idle = pending.is_empty() && shutdown.is_none() && staged.error.is_none();
        let mut message = if idle {
            match rx.recv() {
                Ok(message) => Some(message),
                Err(_) => return,
            }
        } else {
            rx.recv_timeout(JOIN_POLL_INTERVAL).ok()
        };
        for taken in 1..=INTAKE_BATCH {
            let Some(next) = message.take() else { break };
            match next {
                ReaperMsg::Torn(id, reason) => {
                    shared.progress.note(Some(id), "teardown.dequeue", "ok");
                    pending.extend(claim(&shared, id, Some(reason), shutdown));
                }
                ReaperMsg::Sweep => {}
                ReaperMsg::Shutdown(deadline) => {
                    shutdown.get_or_insert(deadline);
                }
            }
            if taken < INTAKE_BATCH {
                message = rx.try_recv().ok();
            }
        }
        if let Some(deadline) = shutdown {
            for record in &mut pending {
                record.joins.tighten(deadline);
            }
        }
        if shutdown.is_some() || shared.dropping.load(Ordering::Acquire) {
            let ids: Vec<ConnId> = shared.conns.lock().unwrap().keys().copied().collect();
            for id in ids {
                pending.extend(claim(&shared, id, None, shutdown));
            }
        }
        publish_staged(&shared, &mut staged);
        let now = Instant::now();
        pending.retain_mut(|record| !poll_pending(&shared, record, &mut staged, now));
        publish_staged(&shared, &mut staged);
        if shutdown.is_some()
            && pending.is_empty()
            && staged.error.is_none()
            && shared.conns.lock().unwrap().is_empty()
        {
            return;
        }
    }
}

/// Attempt to deliver one `Bytes` event, retrying against a full `events`
/// channel for up to [`BYTES_ABANDON_AFTER`] — abandoning delivery
/// (returning `false`) once that bound elapses OR the moment `slot` has
/// been independently cancelled (`Bytes` is the one event kind allowed to
/// be abandoned, but abandoning it always forces this connection closed
/// with a guaranteed `Closed` — see [`reader_loop`]). Returns `false`
/// also if the consumer is gone entirely (channel disconnected).
pub(super) fn deliver_bytes(
    shared: &Arc<ServerShared>,
    conn_id: ConnId,
    bytes: Vec<u8>,
    slot: &IoSlot,
) -> bool {
    let mut item = LaneEvent::Bytes(conn_id, bytes);
    let deadline = Instant::now() + BYTES_ABANDON_AFTER;
    loop {
        let sent = shared.events_tx.try_send(item);
        shared.progress.note(
            Some(conn_id),
            "bytes.enqueue",
            match &sent {
                Ok(()) => "ok",
                Err(TrySendError::Full(_)) => "full",
                Err(TrySendError::Disconnected(_)) => "disconnected",
            },
        );
        match sent {
            Ok(()) => {
                notify_wake(shared);
                return true;
            }
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(v)) => {
                item = v;
                if slot.is_closing() || Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(EVENTS_RETRY_INTERVAL);
            }
        }
    }
}

/// Classify a terminal `ReadFile`/`WriteFile`/`GetOverlappedResult`
/// error: the disconnect family (see [`is_disconnect_family`]) plus this
/// side's own cancellation is `Eof`; anything else is `Error`. A
/// SUCCESSFUL zero-byte read is handled separately, in [`reader_loop`]
/// itself — it never reaches this function.
pub(super) fn classify_terminal_error(e: std::io::Error) -> ClosedReason {
    match e.raw_os_error() {
        Some(c) if c == ERROR_OPERATION_ABORTED as i32 || is_disconnect_family(c) => {
            ClosedReason::Eof
        }
        _ => ClosedReason::Error(e.to_string()),
    }
}

/// One connection's read side: at most one outstanding `ReadFile` at a
/// time. On any terminal condition this thread does NOT touch `conns` or
/// join anything itself — it only [`request_teardown`]s and returns.
pub(super) fn reader_loop(
    slot: Arc<IoSlot>,
    conn_id: ConnId,
    shared: Arc<ServerShared>,
    registry_id: u64,
    torn_down_requested: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; READ_BUF_LEN];
    let reason = loop {
        let result = slot.submit_and_wait_registered(
            &shared.instances,
            registry_id,
            |h, ov| unsafe {
                ReadFile(h, buf.as_mut_ptr(), buf.len() as u32, std::ptr::null_mut(), ov)
            },
            |_| false,
        );
        match result {
            // A SUCCESSFUL zero-byte completion is not EOF — Microsoft
            // documents it as a legitimate outcome of the peer issuing
            // its own zero-byte write. Just read again.
            Ok(0) => continue,
            Ok(n) => {
                if !deliver_bytes(&shared, conn_id, buf[..n as usize].to_vec(), &slot) {
                    break ClosedReason::Error(format!(
                        "events channel saturated for longer than {BYTES_ABANDON_AFTER:?}; Bytes delivery abandoned"
                    ));
                }
            }
            Err(e) if is_completion_unproven(&e) => {
                // Codex round-4 finding 2: leak this slot and the
                // in-flight read buffer forever rather than let either
                // be freed/reused while the kernel might still write
                // into them -- see `CompletionUnproven`'s own doc.
                std::mem::forget(Arc::clone(&slot));
                std::mem::forget(buf);
                break ClosedReason::Error(
                    "a pending read's completion could not be affirmatively observed; its \
                     buffer was leaked rather than risk a use-after-free"
                        .to_string(),
                );
            }
            Err(e) => break classify_terminal_error(e),
        }
    };
    request_teardown(&shared, conn_id, &torn_down_requested, reason);
    shared.progress.note(Some(conn_id), "reader.exit", "ok");
    shared.controls.exit_point(&shared.progress, conn_id, Role::Reader);
}

/// One connection's write side: drains queued sends in order, one
/// outstanding `WriteFile` at a time, reliably emitting `Sent` for
/// marker-tagged sends once the OS reports the write physically complete,
/// and releasing its outbound-budget reservation once that write RETURNS
/// either way. Exits when its channel disconnects (the reaper dropped the
/// sender) or its current write is cancelled/fails — a write failure
/// [`request_teardown`]s directly rather than merely exiting, so a
/// write-side failure the reader never independently notices still gets
/// the connection torn down. Never touches `shared.conns` — teardown is
/// always the reaper's.
pub(super) fn writer_loop(
    slot: Arc<IoSlot>,
    conn_id: ConnId,
    rx: Receiver<WriteCmd>,
    shared: Arc<ServerShared>,
    registry_id: u64,
    outbound: Arc<OutboundBudget>,
    torn_down_requested: Arc<AtomicBool>,
) {
    while let Ok(cmd) = rx.recv() {
        let len = cmd.bytes.len();
        let result = slot.submit_and_wait_registered(
            &shared.instances,
            registry_id,
            |h, ov| unsafe {
                WriteFile(h, cmd.bytes.as_ptr(), cmd.bytes.len() as u32, std::ptr::null_mut(), ov)
            },
            |_| false,
        );
        outbound.release(len);
        match result {
            Ok(_) => {
                if let Some(marker) = cmd.marker {
                    send_lifecycle_event(&shared, LaneEvent::Sent(conn_id, marker));
                }
            }
            Err(e) if is_completion_unproven(&e) => {
                // Codex round-4 finding 2: leak this slot and the
                // in-flight write buffer forever -- see
                // `CompletionUnproven`'s own doc.
                std::mem::forget(Arc::clone(&slot));
                std::mem::forget(cmd.bytes);
                request_teardown(
                    &shared,
                    conn_id,
                    &torn_down_requested,
                    ClosedReason::Error(
                        "a pending write's completion could not be affirmatively observed; its \
                         buffer was leaked rather than risk a use-after-free"
                            .to_string(),
                    ),
                );
                break;
            }
            Err(e) => {
                request_teardown(
                    &shared,
                    conn_id,
                    &torn_down_requested,
                    classify_terminal_error(e),
                );
                break;
            }
        }
    }
    shared.progress.note(Some(conn_id), "writer.exit", "ok");
    shared.controls.exit_point(&shared.progress, conn_id, Role::Writer);
}
