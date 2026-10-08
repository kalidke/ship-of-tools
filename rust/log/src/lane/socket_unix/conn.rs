//! Per-connection threads and teardown: the reaper, reader and writer loops, and lifecycle events.

use super::*;

/// The role label of the reaper's own `shutdown(2)`.
const REAPER_CALLER: &str = "rust/log/src/lane/socket_unix/conn.rs::reaper_loop";

/// Marker error: a partial-progress write loop observed
/// [`ConnHandle::torn_down_requested`] flip mid-write and stopped rather
/// than continue submitting more of the payload — the write(s) that DID
/// land are real (never rolled back), but nothing further is attempted.
/// [`classify_terminal_error`] maps this to [`ClosedReason::Closed`].
#[derive(Debug)]
struct TeardownRequestedMarker;
impl std::fmt::Display for TeardownRequestedMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("teardown requested for this connection")
    }
}
impl std::error::Error for TeardownRequestedMarker {}

fn is_teardown_requested(e: &io::Error) -> bool {
    e.get_ref()
        .is_some_and(|inner| inner.is::<TeardownRequestedMarker>())
}

/// The POSIX disconnect family for a stream socket's read/write errors —
/// the direct analogue of `pipe_win::is_disconnect_family`: an ordinary,
/// expected `Eof` for a live connection whose peer vanished or whose own
/// end was `shutdown(2)`'d, never treated as an anomaly.
fn is_disconnect_family(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
    )
}

/// Classify a terminal read/write error. A SUCCESSFUL zero-byte read is
/// handled separately, in [`reader_loop`] itself — it never reaches this
/// function (mirrors `pipe_win::classify_terminal_error`).
fn classify_terminal_error(e: io::Error) -> ClosedReason {
    if is_teardown_requested(&e) {
        return ClosedReason::Closed;
    }
    if is_disconnect_family(e.kind()) {
        return ClosedReason::Eof;
    }
    ClosedReason::Error(e.to_string())
}

/// Switch-latency Phase 1 (c): ping this server's own wake callback, if
/// [`SocketServer::set_wake`] ever registered one — called ONLY after a
/// push to `events_tx` already succeeded, so a caller woken by it always
/// finds the real event already queued for `events()`'s own `try_recv`.
/// A no-op for the (common, unaffected) case nothing ever registered
/// one, e.g. the supervisor lane. Identical contract to
/// `pipe_win::notify_wake`.
fn notify_wake(shared: &Arc<ServerShared>) {
    if let Some(wake) = shared.activity_wake.get() {
        wake();
    }
}

/// Deliver one lifecycle event (`Accepted`/`Sent`/`Closed`/`AcceptError`)
/// RELIABLY: retries against a full `events` channel indefinitely, with
/// exactly one escape — [`ServerShared::dropping`] — once true, nothing
/// could ever call `events()` again. Identical contract to
/// `pipe_win::send_lifecycle_event`.
pub(super) fn send_lifecycle_event(shared: &Arc<ServerShared>, evt: LaneEvent) {
    let checkpoint = enqueue_checkpoint(&evt);
    let mut item = evt;
    loop {
        if let Some((id, step, detail)) = &checkpoint {
            shared
                .progress
                .note(*id, step, format_args!("begin{detail}"));
        }
        let sent = shared.events_tx.try_send(item);
        if let Some((id, step, detail)) = &checkpoint {
            let result = match &sent {
                Ok(()) => "ok",
                Err(TrySendError::Full(_)) => "full",
                Err(TrySendError::Disconnected(_)) => "disconnected",
            };
            shared
                .progress
                .note(*id, step, format_args!("{result}{detail}"));
        }
        match sent {
            Ok(()) => {
                notify_wake(shared);
                return;
            }
            Err(TrySendError::Disconnected(_)) => return,
            Err(TrySendError::Full(v)) => {
                shared.probes.note_events_full_lifecycle();
                item = v;
                if shared.dropping.load(Ordering::Acquire) {
                    return;
                }
                thread::sleep(EVENTS_RETRY_INTERVAL);
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

/// Attempt to deliver one `Bytes` event, retrying against a full `events`
/// channel for up to [`BYTES_ABANDON_AFTER`] — abandoning delivery
/// (returning `false`) once that bound elapses OR the moment
/// `torn_down_requested` is observed set (this connection is being torn
/// down by another path already; further retrying is pure busywork).
/// Identical contract to `pipe_win::deliver_bytes`.
fn deliver_bytes(
    shared: &Arc<ServerShared>,
    conn_id: ConnId,
    bytes: Vec<u8>,
    torn_down_requested: &AtomicBool,
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
                shared.probes.note_events_full_bytes();
                item = v;
                if torn_down_requested.load(Ordering::Acquire) || Instant::now() >= deadline {
                    shared.probes.note_bytes_abandoned();
                    return false;
                }
                thread::sleep(EVENTS_RETRY_INTERVAL);
            }
        }
    }
}

/// Request teardown for `conn_id`, at most once: every caller (an
/// explicit `close`, the reader's own EOF/error signal, the writer's own
/// error signal) races the SAME connection's `flag` via
/// `compare_exchange`; only the winner enqueues a [`ReaperMsg`].
/// Identical contract to `pipe_win::request_teardown`.
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

/// Notify the consumer that a just-accepted stream could not be fully
/// registered (a worker's `thread::Builder::spawn` failed) — `Accepted`
/// then an immediate `Closed(Error(..))`, both via the reliable path.
/// Identical contract to `pipe_win::report_registration_failure`.
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

/// Tear down `conn_id` if it is still present — called EXCLUSIVELY from
/// [`reaper_loop`], which processes messages strictly one at a time, so
/// the `conns.remove` below is the single, uncontested point of truth
/// for "who claims this connection". Issues ONE `shutdown(SHUT_RDWR)`
/// regardless of which of the three triggers requested teardown — see
/// the module doc's "Cancellation" section for why this is the direct
/// analogue of `pipe_win::teardown_if_present` cancelling both of its
/// `IoSlot`s unconditionally. `reason: None` is `Drop`'s shutdown pass —
/// no event is emitted (nothing could ever observe it).
fn teardown_if_present(shared: &Arc<ServerShared>, conn_id: ConnId, reason: Option<ClosedReason>) {
    let conn = shared.conns.lock().unwrap().remove(&conn_id);
    let Some(conn) = conn else { return };
    shared.progress.note(
        Some(conn_id),
        "claim",
        match &reason {
            Some(reason) => format!("reaper reason={reason:?}"),
            None => "reaper reason=shutdown".to_string(),
        },
    );
    observe_shutdown(&shared.progress, Some(conn_id), &conn.stream, REAPER_CALLER);
    drop(conn.sender); // unblocks a writer idle-waiting on `recv` with nothing queued
    shared
        .progress
        .note(Some(conn_id), "reader.join.begin", "begin");
    let joined = conn.reader_jh.join();
    shared.progress.note(
        Some(conn_id),
        "reader.join.end",
        if joined.is_ok() { "ok" } else { "panic" },
    );
    shared
        .progress
        .note(Some(conn_id), "writer.join.begin", "begin");
    let joined = conn.writer_jh.join();
    shared.progress.note(
        Some(conn_id),
        "writer.join.end",
        if joined.is_ok() { "ok" } else { "panic" },
    );
    if let Some(reason) = reason {
        send_lifecycle_event(shared, LaneEvent::Closed(conn_id, reason));
    }
}

/// The reaper: the only thread that ever removes a registered connection
/// from `conns` or joins its reader/writer. Processes [`ReaperMsg`]s
/// strictly one at a time. Identical contract to `pipe_win::reaper_loop`.
pub(super) fn reaper_loop(shared: Arc<ServerShared>, rx: Receiver<ReaperMsg>) {
    for msg in rx.iter() {
        match msg {
            ReaperMsg::Torn(id, reason) => {
                shared.progress.note(Some(id), "teardown.dequeue", "ok");
                teardown_if_present(&shared, id, Some(reason));
            }
            ReaperMsg::Shutdown => {
                let ids: Vec<ConnId> = shared.conns.lock().unwrap().keys().copied().collect();
                for id in ids {
                    teardown_if_present(&shared, id, None);
                }
                return;
            }
        }
    }
}

/// One connection's read side: at most one outstanding `read` at a time.
/// On any terminal condition this thread does NOT touch `conns` or join
/// anything itself — it only [`request_teardown`]s and returns. Mirrors
/// `pipe_win::reader_loop`.
pub(super) fn reader_loop(
    stream: Arc<UnixStream>,
    conn_id: ConnId,
    shared: Arc<ServerShared>,
    torn_down_requested: Arc<AtomicBool>,
) {
    shared.progress.note(Some(conn_id), "reader.enter", "ok");
    let mut buf = vec![0u8; READ_BUF_LEN];
    let reason = loop {
        shared
            .progress
            .note(Some(conn_id), "reader.io.enter", "begin");
        let read = (&*stream).read(&mut buf);
        shared
            .progress
            .note(Some(conn_id), "reader.io.result", format_args!("{read:?}"));
        match read {
            Ok(0) => break ClosedReason::Eof, // ordered EOF (property 13), never reaches classify_terminal_error
            Ok(n) => {
                if !deliver_bytes(&shared, conn_id, buf[..n].to_vec(), &torn_down_requested) {
                    break ClosedReason::Error(format!(
                        "events channel saturated for longer than {BYTES_ABANDON_AFTER:?}; \
                         Bytes delivery abandoned"
                    ));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => break classify_terminal_error(e),
        }
    };
    request_teardown(&shared, conn_id, &torn_down_requested, reason);
    shared.progress.note(Some(conn_id), "reader.exit", "ok");
    shared
        .controls
        .exit_point(&shared.progress, conn_id, Role::Reader);
}

/// `write(2)` in a partial-progress loop, checking `torn_down_requested`
/// on EVERY iteration (ADR 0043 decision 5, "checked on every iteration
/// of a partial-progress loop, not only at entry") — belt and braces
/// alongside `shutdown(2)`'s own prompt effect on the underlying fd.
fn write_all_checking_teardown(
    stream: &UnixStream,
    mut bytes: &[u8],
    torn_down_requested: &AtomicBool,
    shared: &ServerShared,
    conn_id: ConnId,
) -> io::Result<()> {
    while !bytes.is_empty() {
        if torn_down_requested.load(Ordering::Acquire) {
            return Err(io::Error::other(TeardownRequestedMarker));
        }
        shared
            .progress
            .note(Some(conn_id), "writer.io.enter", "begin");
        let written = (&*stream).write(bytes);
        shared.progress.note(
            Some(conn_id),
            "writer.io.result",
            format_args!("{written:?}"),
        );
        match written {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "write returned 0 with bytes still to send",
                ))
            }
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// One connection's write side: drains queued sends in order, one
/// outstanding `write` sequence at a time, reliably emitting `Sent` for
/// marker-tagged sends once the OS reports the write physically
/// complete, and releasing its outbound-budget reservation once that
/// write RETURNS either way (property 11: the in-flight item stays
/// counted the whole time). Exits when its channel disconnects (the
/// reaper dropped the sender) or its current write is cancelled/fails —
/// a write failure [`request_teardown`]s directly. Never touches
/// `shared.conns` — teardown is always the reaper's. Mirrors
/// `pipe_win::writer_loop`.
pub(super) fn writer_loop(
    stream: Arc<UnixStream>,
    conn_id: ConnId,
    rx: Receiver<WriteCmd>,
    shared: Arc<ServerShared>,
    outbound: Arc<OutboundBudget>,
    torn_down_requested: Arc<AtomicBool>,
) {
    shared.progress.note(Some(conn_id), "writer.enter", "ok");
    loop {
        shared
            .progress
            .note(Some(conn_id), "writer.queue.wait", "begin");
        let received = rx.recv();
        shared.progress.note(
            Some(conn_id),
            "writer.queue.result",
            if received.is_ok() {
                "ok"
            } else {
                "disconnected"
            },
        );
        let Ok(cmd) = received else { break };
        let len = cmd.bytes.len();
        let result = write_all_checking_teardown(
            &stream,
            &cmd.bytes,
            &torn_down_requested,
            &shared,
            conn_id,
        );
        outbound.release(len);
        match result {
            Ok(()) => {
                if let Some(marker) = cmd.marker {
                    send_lifecycle_event(&shared, LaneEvent::Sent(conn_id, marker));
                }
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

/// The one observation wrapper of every server and client `shutdown(2)`: it records `shutdown.enter`, performs the
/// actual call, captures `errno` straight after a failure (before any logging or locking), then records the result,
/// the numeric errno and `caller`. It changes neither the call nor what the caller does with the outcome.
pub(super) fn observe_shutdown(
    progress: &crate::lane::test_progress::Progress,
    conn: Option<ConnId>,
    stream: &UnixStream,
    caller: &'static str,
) {
    progress.note_with(conn, "shutdown.enter", "begin", caller, None);
    let rc = unsafe { libc::shutdown(stream.as_raw_fd(), libc::SHUT_RDWR) };
    #[cfg(any(test, feature = "test-support"))]
    {
        let error = (rc != 0).then(io::Error::last_os_error);
        let errno = error.as_ref().and_then(io::Error::raw_os_error);
        progress.note_with(
            conn,
            "shutdown.result",
            format_args!("rc={rc} error={error:?}"),
            caller,
            errno,
        );
    }
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = (rc, progress, conn, caller);
}
