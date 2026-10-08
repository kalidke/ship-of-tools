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
    let checkpoint = pending::enqueue_checkpoint(&evt);
    let mut item = evt;
    let mut last = "";
    loop {
        if let Some((id, step, detail)) = &checkpoint {
            if last.is_empty() {
                shared
                    .progress
                    .note(*id, step, format_args!("begin{detail}"));
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
                shared
                    .progress
                    .note(*id, step, format_args!("{result}{detail}"));
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
        shared
            .progress
            .note(Some(conn_id), "teardown.enqueue", "begin");
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
pub(super) fn report_registration_failure(
    shared: &Arc<ServerShared>,
    what: &str,
    e: impl std::fmt::Display,
) {
    let conn_id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    send_lifecycle_event(shared, LaneEvent::Accepted(conn_id));
    send_lifecycle_event(
        shared,
        LaneEvent::Closed(conn_id, ClosedReason::Error(format!("{what}: {e}"))),
    );
}

/// Signal the reaper to shut down against `deadline` (see [`pending::signal_shutdown`]).
pub(super) fn signal_shutdown(shared: &ServerShared, deadline: Instant) {
    pending::signal_shutdown(
        &shared.shutdown_sent,
        &shared.reaper_tx,
        &shared.progress,
        deadline,
    );
}

/// A claimed connection: its slots stay owned through completion; the registry stays the only closer of its handle.
struct Pending {
    id: ConnId,
    claimed: Claimed,
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

/// What a reaper pass reads of this server.
fn reaper_ctx(shared: &ServerShared) -> pending::Ctx<'_> {
    pending::Ctx {
        prefix: "sot-pipe",
        progress: &shared.progress,
        events_tx: &shared.events_tx,
        dropping: &shared.dropping,
        teardown_failed: &shared.teardown_failed,
        wake: shared.activity_wake.get().map(|wake| &**wake as _),
    }
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
        let read_pending = live
            .read_slot
            .cancel_registered(&shared.instances, live.registry_id);
        let write_pending = live
            .write_slot
            .cancel_registered(&shared.instances, live.registry_id);
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
            format_args!(
                "read_genuinely_pending={read_pending} write_genuinely_pending={write_pending}"
            ),
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
    shared
        .progress
        .note(Some(conn_id), "reader.join.begin", "begin");
    shared
        .progress
        .note(Some(conn_id), "writer.join.begin", "begin");
    let own = Instant::now() + shared.controls.close_budget();
    let deadline = shutdown.map_or(own, |shutdown| own.min(shutdown));
    Some(Pending {
        id: conn_id,
        claimed: Claimed::new(conn_id, reason, reader_jh, writer_jh, deadline),
        registry_id,
        raw,
        _slots: (read_slot, write_slot),
    })
}

/// One pass over a pending connection. True when it is retired and its instance is no longer charged.
fn poll_pending(
    shared: &Arc<ServerShared>,
    pending: &mut Pending,
    staged: &mut Staged,
    now: Instant,
) -> bool {
    if !pending.claimed.poll(&reaper_ctx(shared), now) {
        return false;
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
        pending::try_publish(&reaper_ctx(shared), event, &mut slot, &mut last);
        staged.error = slot;
        staged.last = last;
    }
}

/// The reaper claims registered connections once, cancels both directions and polls every pending pair, joining only
/// finished workers. Expiry reports unfinished workers still owned; panic reports a completed panicked join, and only a
/// panic latches failed teardown. Phase-one registered pairs use this same owner; only never-registered gated workers
/// may be joined locally. Closed follows both joins.
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
        let open = pending::intake(&rx, idle, |message| match message {
            ReaperMsg::Torn(id, reason) => {
                shared.progress.note(Some(id), "teardown.dequeue", "ok");
                pending.extend(claim(&shared, id, Some(reason), shutdown));
            }
            ReaperMsg::Sweep => {}
            ReaperMsg::Shutdown(deadline) => {
                shutdown.get_or_insert(deadline);
            }
        });
        if !open {
            return;
        }
        shared
            .controls
            .barrier_point(&shared.progress, None, "reaper.pass");
        if let Some(deadline) = shutdown {
            for record in &mut pending {
                record.claimed.tighten(deadline);
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
                ReadFile(
                    h,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    std::ptr::null_mut(),
                    ov,
                )
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
    shared
        .controls
        .exit_point(&shared.progress, conn_id, Role::Reader);
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
                WriteFile(
                    h,
                    cmd.bytes.as_ptr(),
                    cmd.bytes.len() as u32,
                    std::ptr::null_mut(),
                    ov,
                )
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
    shared
        .controls
        .exit_point(&shared.progress, conn_id, Role::Writer);
}
