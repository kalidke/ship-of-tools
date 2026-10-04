//! The accept loop: obtaining instances, accepting connections, recycling and stopping.

use super::*;

/// Mark the accept loop stopped and request cancellation on its
/// currently in-flight `ConnectNamedPipe`, if any. Cancellation-request
/// ONLY -- closing whatever handle that in-flight attempt is using is
/// [`InstanceRegistry::close_all`]'s job (called by
/// `disconnect_listener`), not this function's: the registry, not
/// `AcceptState::current`, is this module's one source of truth for
/// "has this instance been closed" (see that type's own doc), so this
/// function no longer needs to hand anything back to its caller. Shared
/// by `PipeServer::disconnect_listener` (planned shutdown) and
/// [`terminalize_accept_loop`] (a persistent resource failure) — both
/// need the SAME cross-thread-safe cancellation, since either can be
/// triggered by a thread other than the accept thread itself (`Drop`
/// runs on the caller's thread; a resource failure can be discovered on
/// the reaper thread while tearing down an unrelated connection's
/// instance). Without this, a failure discovered off the accept thread
/// could leave a pending accept to linger — or even admit one more
/// client — after the consumer was already told no more connections are
/// coming.
pub(super) fn stop_accept_loop(shared: &Arc<ServerShared>) {
    let mut st = shared.accept.lock().unwrap();
    st.accept_stopping = true;
    if let Some((id, _raw, slot)) = st.current.take() {
        // Codex round-5 fix 2b/2c: latch whether THIS cancellation
        // observed a genuinely async pending `ConnectNamedPipe`, decided
        // under the same lock acquisition `cancel_registered` uses to
        // perform the cancellation -- see
        // `ServerShared::accept_cancel_observed_genuine_pending`'s own
        // doc for why this is the TOCTOU-free proof a test needs.
        let was_pending = slot.cancel_registered(&shared.instances, id);
        shared
            .accept_cancel_observed_genuine_pending
            .store(was_pending, Ordering::Release);
    }
}

/// Stop the accept loop for good and report why — the ONE place every
/// persistent-resource-failure path routes through, regardless of which
/// thread discovers the failure. Cancels only -- never closes the
/// pending instance's handle, unlike a whole-server teardown: a resource
/// failure is not that, and the accept thread itself still owns and will
/// dispose of that handle normally (`recycle_instance`, which is a
/// cleanliness no-op once the registry is already torn down -- see its
/// own doc).
pub(super) fn terminalize_accept_loop(shared: &Arc<ServerShared>, message: String) {
    stop_accept_loop(shared);
    shared.accept_cv.notify_all();
    send_lifecycle_event(shared, LaneEvent::AcceptError(message));
}

/// Set `id`/`raw` aside for reuse rather than closing it.
/// `DisconnectNamedPipe` resets it to the listening state; on success
/// it's pushed onto `AcceptState::recycled` (SAME `id`, never
/// re-registered). On FAILURE, it's retained (still registered, never
/// closed by this function) and the accept loop is terminalized — see
/// the module doc's "Continuous name hold" section for why retaining
/// the dead handle, rather than replacing or closing it, is the correct
/// response.
///
/// This function itself NEVER calls `CloseHandle`, and NEVER touches the
/// raw handle except through a [`LiveHandle`] (Codex round-4 finding 3):
/// `id` stays registered either way, and [`InstanceRegistry::close_all`]
/// is the only thing that ever actually closes it. If `id` is already
/// gone (`InstanceRegistry::live` returns `None` — `close_all` already
/// closed it, or is closing it this instant), this returns immediately,
/// touching neither `DisconnectNamedPipe` nor either queue: not a
/// cleanliness nicety here but the load-bearing check that prevents a
/// stale-handle `DisconnectNamedPipe` call racing `close_all`.
pub(super) fn recycle_instance(shared: &Arc<ServerShared>, id: u64, raw: SendableHandle) {
    let disconnected = match shared.instances.live(id) {
        Some(live) => (unsafe { DisconnectNamedPipe(live.get()) }) != 0,
        None => return,
    };
    if disconnected {
        shared.accept.lock().unwrap().recycled.push_back((id, raw));
        shared.accept_cv.notify_all();
        return;
    }
    shared.accept.lock().unwrap().retained_dead.push((id, raw));
    terminalize_accept_loop(
        shared,
        "DisconnectNamedPipe failed on a torn-down instance; it is retained (never closed) to keep the pipe \
         name held, permanently costing one instance's worth of capacity, and no further connections will be \
         accepted"
            .to_string(),
    );
}

/// Obtain the next instance to listen on: a recycled one (SAME
/// registered id it was given at its own original creation — see
/// [`InstanceRegistry`]'s own doc for why recycling never re-registers)
/// in preference to creating a fresh one (via
/// [`InstanceRegistry::create_and_register`], Codex round-4 finding 1),
/// blocking at the instance cap with nothing recycled yet. Waits on the
/// plain condvar — every state change that could satisfy this predicate
/// (`Drop`, a recycle) already `notify_all`s, so a polling wait would
/// buy nothing. `None` means: stop accepting — shutdown, a persistent
/// creation failure already reported via `LaneEvent::AcceptError`,
/// or (rarely) a creation attempt that found the registry already torn
/// down (`create_and_register` never even called `CreateNamedPipeW` in
/// that case).
pub(super) fn obtain_instance(shared: &Arc<ServerShared>) -> Option<(u64, SendableHandle)> {
    loop {
        let mut st = shared.accept.lock().unwrap();
        if st.accept_stopping {
            return None;
        }
        if let Some(entry) = st.recycled.pop_front() {
            return Some(entry);
        }
        if st.created < shared.max_instances {
            st.created += 1;
            drop(st);
            return match shared
                .instances
                .create_and_register(|| create_pipe_instance(&shared.name, false, shared.max_instances))
            {
                CreateOutcome::Created(id, raw) => Some((id, raw)),
                CreateOutcome::CreateFailed(e) => {
                    let mut st = shared.accept.lock().unwrap();
                    st.created -= 1;
                    drop(st);
                    terminalize_accept_loop(shared, e.to_string());
                    None
                }
                CreateOutcome::ShuttingDown => None,
            };
        }
        st = shared.accept_cv.wait(st).unwrap();
        drop(st);
    }
}

/// The accept loop, one dedicated thread for the server's whole life.
/// `(first_id, first_raw)` is the already-created-and-registered (with
/// `FILE_FLAG_FIRST_PIPE_INSTANCE`) instance from `bind`; every later
/// instance comes from [`obtain_instance`] (recycled or freshly created,
/// never carrying that flag).
pub(super) fn accept_loop(shared: Arc<ServerShared>, first_id: u64, first_raw: SendableHandle) {
    let mut pending_first = Some((first_id, first_raw));
    loop {
        let (id, raw) = match pending_first.take() {
            Some(entry) => entry,
            None => match obtain_instance(&shared) {
                Some(entry) => entry,
                None => return,
            },
        };

        let slot = match IoSlot::new() {
            Ok(s) => Arc::new(s),
            Err(e) => {
                // A slot-creation failure here is a resource failure
                // exactly like `create_pipe_instance`'s own.
                recycle_instance(&shared, id, raw);
                terminalize_accept_loop(&shared, format!("IoSlot::new (accept): {e}"));
                return;
            }
        };

        {
            let mut st = shared.accept.lock().unwrap();
            if st.accept_stopping {
                drop(st);
                recycle_instance(&shared, id, raw);
                return;
            }
            st.current = Some((id, raw, Arc::clone(&slot)));
        }
        let connect_result = slot.submit_and_wait_registered(
            &shared.instances,
            id,
            |h, ov| unsafe { ConnectNamedPipe(h, ov) },
            |code| code == ERROR_PIPE_CONNECTED as i32,
        );
        shared.accept.lock().unwrap().current = None;

        if let Err(e) = &connect_result {
            if is_completion_unproven(e) {
                // Codex round-4 finding 2: never reuse or drop this
                // slot's OVERLAPPED/event again -- leak an extra
                // reference to it forever (see `CompletionUnproven`'s
                // own doc) and stop accepting entirely. The instance's
                // OWN handle stays registered and will be closed
                // normally, safely, whenever the server actually tears
                // down (`CloseHandle` on a handle with genuinely pending
                // I/O is well documented as safe; it is only THIS
                // module's own OVERLAPPED/event/buffer memory that must
                // never be freed early).
                std::mem::forget(Arc::clone(&slot));
                terminalize_accept_loop(
                    &shared,
                    "a pending ConnectNamedPipe's completion could not be affirmatively observed; \
                     the accept loop stopped rather than risk a use-after-free"
                        .to_string(),
                );
                return;
            }
        }

        // Codex round-3/4: `id`/`raw` stay correctly owned by
        // `InstanceRegistry` regardless of this check's own timing (see
        // that type's doc, and `recycle_instance`'s) -- this is a pure
        // CLEANLINESS optimization, not a safety decision. Once
        // `disconnect_listener` has started, avoid a pointless
        // recycle/connection attempt and a possible spurious
        // `AcceptError` on what may already be a closed handle.
        if shared.dropping.load(Ordering::Acquire) {
            return;
        }

        match connect_result {
            Ok(_) => handle_new_connection(&shared, id, raw, slot),
            Err(e) if e.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) => {
                // This module's own cancellation (a `Drop`-triggered
                // stop) — never a real client.
                recycle_instance(&shared, id, raw);
                if shared.accept.lock().unwrap().accept_stopping {
                    return;
                }
                // Not actually stopping -- nothing else ever cancels this
                // slot, but stay defensive and just try again.
            }
            Err(e) if e.raw_os_error().is_some_and(is_disconnect_family) => {
                // A client connected and vanished before/while the
                // completion was processed -- register it anyway so the
                // reader's own first `ReadFile` discovers and classifies
                // whatever actually happened, giving it a real
                // `Accepted` -> `Closed` lifecycle instead of discarding
                // a real client attempt.
                handle_new_connection(&shared, id, raw, slot);
            }
            Err(e) => {
                // Any other connect failure is a genuine anomaly, not a
                // disconnect race -- recycle the instance and terminalize
                // rather than misreport it as a client that connected.
                recycle_instance(&shared, id, raw);
                terminalize_accept_loop(&shared, format!("ConnectNamedPipe: {e}"));
                return;
            }
        }
    }
}

/// Hand off a just-connected instance: spawn its reader/writer threads
/// (gated — see [`StartGate`]), register it, THEN reliably publish
/// `Accepted` and open the gate. `thread::Builder::spawn` makes a spawn
/// failure recoverable: if the writer fails to spawn, the already-spawned
/// reader (still gated, having never touched the pipe) is `abort`ed and
/// joined directly here — bounded, since an aborted gate wait returns
/// immediately — before the instance is recycled, so a handle is never
/// closed (or recycled) while any thread might still be using it. Any
/// registration failure is reported via [`report_registration_failure`]
/// rather than silently dropping the client.
pub(super) fn handle_new_connection(
    shared: &Arc<ServerShared>,
    id: u64,
    raw: SendableHandle,
    read_slot: Arc<IoSlot>,
) {
    let write_slot = match IoSlot::new() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            recycle_instance(shared, id, raw);
            report_registration_failure(shared, "write-slot setup failed", e);
            return;
        }
    };
    let conn_id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<WriteCmd>();
    let outbound = Arc::new(OutboundBudget::new());
    let gate = StartGate::new();
    let torn_down_requested = Arc::new(AtomicBool::new(false));

    let reader_jh = {
        let shared2 = Arc::clone(shared);
        let read_slot = Arc::clone(&read_slot);
        let gate = Arc::clone(&gate);
        let torn = Arc::clone(&torn_down_requested);
        thread::Builder::new()
            .name(format!("sot-pipe-r-{conn_id}"))
            .spawn(move || {
                if !gate.wait_for_start() {
                    return;
                }
                reader_loop(read_slot, conn_id, shared2, id, torn)
            })
    };
    let reader_jh = match reader_jh {
        Ok(jh) => jh,
        Err(e) => {
            recycle_instance(shared, id, raw);
            report_registration_failure(shared, "reader thread spawn failed", e);
            return;
        }
    };

    let writer_jh = {
        let shared2 = Arc::clone(shared);
        let write_slot = Arc::clone(&write_slot);
        let outbound = Arc::clone(&outbound);
        let gate = Arc::clone(&gate);
        let torn = Arc::clone(&torn_down_requested);
        thread::Builder::new()
            .name(format!("sot-pipe-w-{conn_id}"))
            .spawn(move || {
                if !gate.wait_for_start() {
                    return;
                }
                writer_loop(write_slot, conn_id, rx, shared2, id, outbound, torn)
            })
    };
    let writer_jh = match writer_jh {
        Ok(jh) => jh,
        Err(e) => {
            // The reader is spawned but still gated -- abort makes its
            // `wait_for_start` return `false` immediately, so joining it
            // here (NOT through the reaper: it was never registered) is
            // bounded and it never touches `raw`.
            gate.abort();
            reader_jh.join().ok();
            recycle_instance(shared, id, raw);
            report_registration_failure(shared, "writer thread spawn failed", e);
            return;
        }
    };

    let conn = ConnHandle {
        raw,
        registry_id: id,
        read_slot,
        write_slot,
        outbound,
        sender: tx,
        reader_jh,
        writer_jh,
        torn_down_requested,
    };
    shared.conns.lock().unwrap().insert(conn_id, conn);
    // RELIABLE, not best-effort: retries until the consumer actually has
    // room, so the gate below can never open onto a connection the
    // consumer was never told exists.
    send_lifecycle_event(shared, LaneEvent::Accepted(conn_id));
    gate.open(); // ONLY now may the reader/writer threads touch the pipe.
}
