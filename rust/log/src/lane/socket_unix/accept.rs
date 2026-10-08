//! The accept loop thread and admission of one new connection.

use super::conn::{reader_loop, report_registration_failure, send_lifecycle_event, writer_loop};
use super::*;

/// The accept loop, one dedicated thread for the server's whole life:
/// blocks in `libc::poll` over `{listener, wake_read}` (module doc: "the
/// accept loop wakes via poll(2) over a self-pipe"); at capacity (ADR
/// 0043 decision 4) the newly accepted stream is closed immediately
/// rather than refused at the kernel level (Unix cannot refuse at
/// connect time — the kernel completes the handshake from the listen
/// backlog).
pub(super) fn accept_loop(shared: Arc<ServerShared>, listener: UnixListener, wake_read: OwnedFd) {
    let listener_fd = listener.as_raw_fd();
    let wake_fd = wake_read.as_raw_fd();
    loop {
        if shared.accept_stopping.load(Ordering::Acquire) {
            return;
        }
        let mut fds = [
            libc::pollfd {
                fd: listener_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        shared.progress.note(None, "poll.enter", "begin");
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        let poll_error = (rc < 0).then(io::Error::last_os_error);
        shared.progress.note(
            None,
            "poll.result",
            format_args!("rc={rc} error={poll_error:?}"),
        );
        if rc < 0 {
            let err = poll_error.expect("failed poll has an error");
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            terminalize_accept_loop(&shared, format!("poll: {err}"));
            return;
        }
        if fds[1].revents & libc::POLLIN != 0 {
            // Woken -- drain whatever is queued (defensive: only ever
            // one byte is written per `disconnect_listener` call, but a
            // repeat call is tolerated) and loop back to the top's
            // `accept_stopping` check.
            let mut discard = [0u8; 64];
            loop {
                let n = unsafe { libc::read(wake_fd, discard.as_mut_ptr().cast(), discard.len()) };
                if n <= 0 {
                    break;
                }
            }
            continue;
        }
        if fds[0].revents & (libc::POLLERR | libc::POLLNVAL | libc::POLLHUP) != 0 {
            // The listener itself is bad. `poll` would report this again
            // immediately, so `continue` here would spin the acceptor hot
            // forever: treat it as the permanent accept failure it is
            // (property 32) — one `AcceptError`, then stop accepting.
            terminalize_accept_loop(
                &shared,
                format!("poll: listener reported revents {:#x}", fds[0].revents),
            );
            return;
        }
        if fds[0].revents & libc::POLLIN == 0 {
            continue; // nothing to accept yet
        }
        shared.progress.note(None, "accept.enter", "begin");
        #[allow(
            clippy::disallowed_methods,
            reason = "listener: capsule lane socket: a private runtime folder, then the identity challenge"
        )]
        let accepted = listener.accept();
        shared.progress.note(
            None,
            "accept.result",
            format_args!(
                "{:?}",
                accepted.as_ref().map(|_| ()).map_err(|e| e.to_string())
            ),
        );
        match accepted {
            Ok((stream, _addr)) => {
                // Live plus pending: a claimed connection stays charged until its joins and close are done.
                let charged =
                    shared.conns.lock().unwrap().len() + shared.pending.load(Ordering::Acquire);
                if charged >= shared.max_connections as usize {
                    // ADR 0043 decision 4: accept-then-close at capacity.
                    drop(stream);
                    continue;
                }
                handle_new_connection(&shared, stream);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::WouldBlock
                ) =>
            {
                continue; // transient: ADR 0043 decision 4's retried family
            }
            Err(e) => {
                terminalize_accept_loop(&shared, format!("accept: {e}"));
                return;
            }
        }
    }
}

/// Stop the accept loop for good and report why — the ONE place every
/// persistent-resource-failure path routes through. Sets ONLY
/// `accept_stopping` (never `dropping` — see the module doc's "Two
/// distinct 'stop' signals" section for why conflating them would risk
/// silently losing the very `AcceptError` this function emits).
fn terminalize_accept_loop(shared: &Arc<ServerShared>, message: String) {
    shared.accept_stopping.store(true, Ordering::Release);
    send_lifecycle_event(shared, LaneEvent::AcceptError(message));
}

/// Hand off a just-accepted stream: spawn its reader/writer threads
/// (gated — see [`StartGate`]), register it, THEN reliably publish
/// `Accepted` and open the gate. Mirrors
/// `pipe_win::handle_new_connection`'s own ordering and its recoverable-
/// spawn-failure handling — with no instance to recycle on failure (the
/// `UnixStream` simply drops, closing its fd, once this function
/// returns).
fn handle_new_connection(shared: &Arc<ServerShared>, stream: UnixStream) {
    let stream = Arc::new(stream);
    let conn_id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<WriteCmd>();
    let outbound = Arc::new(OutboundBudget::new());
    let gate = StartGate::new();
    let torn_down_requested = Arc::new(AtomicBool::new(false));

    let reader_jh = {
        let shared2 = Arc::clone(shared);
        let stream2 = Arc::clone(&stream);
        let gate2 = Arc::clone(&gate);
        let torn = Arc::clone(&torn_down_requested);
        thread::Builder::new()
            .name(format!("sot-sock-r-{conn_id}"))
            .spawn(move || {
                if !observe_gate(&shared2, conn_id, &gate2, "reader") {
                    return;
                }
                reader_loop(stream2, conn_id, shared2, torn)
            })
    };
    let reader_jh = match reader_jh {
        Ok(jh) => jh,
        Err(e) => {
            report_registration_failure(shared, "reader thread spawn failed", e);
            return;
        }
    };

    let writer_jh = {
        let shared2 = Arc::clone(shared);
        let stream2 = Arc::clone(&stream);
        let outbound2 = Arc::clone(&outbound);
        let gate2 = Arc::clone(&gate);
        let torn = Arc::clone(&torn_down_requested);
        thread::Builder::new()
            .name(format!("sot-sock-w-{conn_id}"))
            .spawn(move || {
                if !observe_gate(&shared2, conn_id, &gate2, "writer") {
                    return;
                }
                writer_loop(stream2, conn_id, rx, shared2, outbound2, torn)
            })
    };
    let writer_jh = match writer_jh {
        Ok(jh) => jh,
        Err(e) => {
            // The reader is spawned but still gated -- abort makes its
            // `wait_for_start` return `false` immediately, so joining it
            // here (NOT through the reaper: it was never registered) is
            // bounded.
            gate.abort();
            observe_join(shared, conn_id, reader_jh, "reader");
            report_registration_failure(shared, "writer thread spawn failed", e);
            return;
        }
    };

    // Registration and shutdown use the same connection-state lock. An earlier insert is cancelled by phase one and
    // owned by the reaper; an earlier shutdown refuses insertion. Only never-registered gated workers are aborted and
    // joined locally.
    shared
        .controls
        .barrier_point(&shared.progress, Some(conn_id), "registration.barrier");
    let mut conns = shared.conns.lock().unwrap();
    if shared.dropping.load(Ordering::Acquire) {
        drop(conns);
        shared
            .progress
            .note(Some(conn_id), "registration.cutoff", "rejected");
        // Never touched the stream (still gated) -- `abort` makes both
        // threads' own `wait_for_start` return `false` immediately, so
        // joining them here (NOT through the reaper: neither was ever
        // registered) is bounded and legal, the same as the writer-spawn-
        // failure path above. `shutdown` first anyway, defensively, in
        // case either thread is somehow already past the gate (it is
        // not, by construction) -- costs nothing, removes any doubt.
        super::conn::observe_shutdown(
            &shared.progress,
            Some(conn_id),
            &stream,
            "rust/log/src/lane/socket_unix/accept.rs::handle_new_connection",
        );
        gate.abort();
        observe_join(shared, conn_id, reader_jh, "reader");
        observe_join(shared, conn_id, writer_jh, "writer");
        return; // no event: this connection was never told to exist.
    }
    conns.insert(
        conn_id,
        ConnHandle {
            stream,
            outbound,
            sender: tx,
            reader_jh,
            writer_jh,
            torn_down_requested,
        },
    );
    drop(conns); // never hold this lock while sending on the events channel
    shared
        .progress
        .note(Some(conn_id), "registration.cutoff", "inserted");
    shared.progress.note(Some(conn_id), "registered", "ok");
    // RELIABLE, not best-effort: retries until the consumer actually has
    // room, so the gate below can never open onto a connection the
    // consumer was never told exists.
    send_lifecycle_event(shared, LaneEvent::Accepted(conn_id));
    shared.progress.note(Some(conn_id), "gate.open", "begin");
    gate.open(); // ONLY now may the reader/writer threads touch the stream.
    shared.progress.note(Some(conn_id), "gate.open", "ok");
}

// Passive wrappers keep every gate/join call and its original ordering.
fn observe_gate(shared: &ServerShared, id: ConnId, gate: &StartGate, role: &str) -> bool {
    let (wait, result, exit) = if role == "reader" {
        ("reader.gate.wait", "reader.gate.result", "reader.exit")
    } else {
        ("writer.gate.wait", "writer.gate.result", "writer.exit")
    };
    shared.progress.note(Some(id), wait, "begin");
    let started = gate.wait_for_start();
    shared.progress.note(Some(id), result, started);
    if !started {
        shared.progress.note(Some(id), exit, "aborted");
    }
    started
}

fn observe_join(shared: &ServerShared, id: ConnId, jh: JoinHandle<()>, role: &str) {
    let (begin, end) = if role == "reader" {
        ("reader.join.begin", "reader.join.end")
    } else {
        ("writer.join.begin", "writer.join.end")
    };
    shared.progress.note(Some(id), begin, "begin");
    let joined = jh.join();
    shared
        .progress
        .note(Some(id), end, if joined.is_ok() { "ok" } else { "panic" });
}
