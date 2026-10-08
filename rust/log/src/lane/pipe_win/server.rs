//! The pipe server: `PipeServer` and its `LaneServer` seam.

use super::*;
#[cfg(any(test, feature = "test-support"))]
use crate::lane::transport::JOIN_POLL_INTERVAL;

/// The server side of one voyage's pipe: `bind` creates the pipe (with the
/// squat-detecting first instance) and starts accepting; connections and
/// their bytes/completions/closes surface on [`PipeServer::events`].
///
/// # Lifetime rule (ADR 0041: "the pipe is never live while the writer
/// lock is free")
///
/// `bind` is an explicit constructor with no implicit background
/// construction — the CALLER (the capsule, in the follow-up unit) is
/// responsible for calling it only after `open_for_writing` holds the
/// voyage's writer lock, and for dropping the returned `PipeServer` before
/// releasing that lock. This module only guarantees: while a `PipeServer`
/// is alive, the pipe exists; the instant it is dropped, every instance is
/// closed.
///
/// `max_instances` is the RAW total simultaneous pipe-instance ceiling
/// this transport enforces (`CreateNamedPipeW`'s own `nMaxInstances`) —
/// ADR 0041 requires this to already be the CALLER's combined figure
/// (subscribers plus separately bounded pre-hello/mgmt connections); that
/// combination is the follow-up capsule unit's job, not this transport's.
pub struct PipeServer {
    shared: Arc<ServerShared>,
    events_rx: Receiver<LaneEvent>,
    accept_jh: Option<JoinHandle<()>>,
    reaper_jh: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for PipeServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeServer").finish_non_exhaustive()
    }
}

impl PipeServer {
    /// Create `\\.\pipe\sot-voyage-<voyage_id>` (squat-detected via
    /// `FILE_FLAG_FIRST_PIPE_INSTANCE` on this, its first instance,
    /// created AND REGISTERED synchronously — Codex round-4 finding 1 —
    /// so a squat is a loud, immediate `bind` failure and no unregistered
    /// handle can ever outlive this constructor) and start the reaper
    /// and accept threads. `max_instances` must be in Win32's own
    /// documented `1..=255` range.
    pub fn bind(voyage_id: &str, max_instances: u32) -> Result<Self, TransportError> {
        validate_voyage_id(voyage_id)?;
        Self::bind_named(pipe_name_wide(voyage_id), max_instances)
    }

    /// ADR 0041 step 6 U2: the supervisor lane's own pipe,
    /// `\\.\pipe\sot-supervisor-<h>` — otherwise identical to [`Self::bind`]
    /// (same security posture via [`create_pipe_instance`], same
    /// accept/reaper machinery, same squat detection). `h` is the caller's
    /// own stable hash of the canonicalized state-dir path (ADR 0041
    /// Lifecycle "Name and identity") — this constructor does not derive
    /// or validate it as a voyage id, unlike [`Self::bind`].
    pub fn bind_supervisor(h: &str, max_instances: u32) -> Result<Self, TransportError> {
        Self::bind_named(supervisor_pipe_name_wide(h), max_instances)
    }

    /// Shared construction (round-4 finding 1's squat-detection ordering
    /// applies identically to both pipe families): given an
    /// already-resolved wide pipe name, create AND REGISTER the
    /// squat-detecting first instance synchronously, then start the
    /// reaper and accept threads. `max_instances` must be in Win32's own
    /// documented `1..=255` range.
    fn bind_named(name: Vec<u16>, max_instances: u32) -> Result<Self, TransportError> {
        if !(1..=255).contains(&max_instances) {
            return Err(TransportError::InvalidMaxConnections);
        }

        let (events_tx, events_rx) = mpsc::sync_channel(EVENTS_CHANNEL_CAP);
        let (reaper_tx, reaper_rx) =
            mpsc::sync_channel(max_instances as usize + REAPER_INBOX_SLACK);
        let shared = Arc::new(ServerShared {
            conns: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            accept: Mutex::new(AcceptState {
                accept_stopping: false,
                created: 0,
                recycled: VecDeque::new(),
                retained_dead: Vec::new(),
                current: None,
            }),
            accept_cv: Condvar::new(),
            reaper_tx,
            events_tx,
            activity_wake: OnceLock::new(),
            max_instances,
            name,
            dropping: AtomicBool::new(false),
            instances: InstanceRegistry::new(),
            accept_cancel_observed_genuine_pending: AtomicBool::new(false),
            write_cancel_observed_genuine_pending: Mutex::new(HashMap::new()),
            teardown_failed: AtomicBool::new(false),
            sweep_nudged: AtomicBool::new(false),
            shutdown: OnceLock::new(),
            progress: Progress::new("pipe"),
            controls: Controls::default(),
        });

        // Create AND register the squat-detecting first instance
        // synchronously, before this constructor ever returns (Codex
        // round-4 finding 1): `shared` is a brand-new `Arc` no other
        // code has a reference to yet, so `disconnect_listener` cannot
        // possibly have run against it -- `ShuttingDown` here would mean
        // this module's own invariant is broken, not a real runtime
        // condition.
        let (first_id, first_raw) = match shared
            .instances
            .create_and_register(|| create_pipe_instance(&shared.name, true, max_instances))
        {
            CreateOutcome::Created(id, raw) => {
                shared.accept.lock().unwrap().created = 1;
                (id, raw)
            }
            CreateOutcome::CreateFailed(e) => {
                return Err(TransportError::Io {
                    op: "CreateNamedPipeW(first instance)",
                    source: e,
                })
            }
            CreateOutcome::ShuttingDown => {
                unreachable!("a brand-new PipeServer's registry cannot already be torn down")
            }
        };

        // Spawn the reaper FIRST. If the accept thread then fails to
        // spawn, unwind the reaper (it has nothing queued yet, so its
        // own `Shutdown` drain is instant) rather than leave it running
        // forever with no accept thread able to feed it. No `JoinHandle`
        // is ever dropped while its thread could still run.
        let reaper_jh = thread::Builder::new()
            .name("sot-pipe-reaper".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || reaper_loop(shared, reaper_rx)
            });
        let reaper_jh = match reaper_jh {
            Ok(jh) => jh,
            Err(e) => {
                // The first instance was already created AND registered
                // above (Codex round-4: `mem::forget`'d into
                // `shared.instances`, no longer under Rust's own Drop) --
                // with no `PipeServer` ever coming into existence to call
                // `disconnect_listener`, nothing else will ever close it.
                // `close_all` here is this failure path's ONLY chance.
                shared.instances.close_all();
                return Err(TransportError::Io {
                    op: "spawn reaper thread",
                    source: e,
                });
            }
        };

        let accept_jh = thread::Builder::new()
            .name("sot-pipe-accept".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || accept_loop(shared, first_id, first_raw)
            });
        let accept_jh = match accept_jh {
            Ok(jh) => jh,
            Err(e) => {
                // Same reasoning as the reaper-spawn-failure arm above.
                shared.instances.close_all();
                signal_shutdown(&shared, Instant::now() + TEARDOWN_AGGREGATE_DEADLINE);
                reaper_jh.join().ok();
                return Err(TransportError::Io {
                    op: "spawn accept thread",
                    source: e,
                });
            }
        };

        Ok(Self {
            shared,
            events_rx,
            accept_jh: Some(accept_jh),
            reaper_jh: Some(reaper_jh),
        })
    }

    /// The event stream: `Accepted`/`Bytes`/`Sent`/`Closed`/`AcceptError`,
    /// in the order this transport observed them. Single-consumer by
    /// convention (a `Receiver` is not `Sync`). The CONSUMER's half of the
    /// reliable-lifecycle-delivery contract (see the module doc) is to
    /// keep draining this — a stalled consumer backs everything up but
    /// never silently loses a lifecycle event.
    pub fn events(&self) -> &Receiver<LaneEvent> {
        &self.events_rx
    }

    /// Switch-latency Phase 1 (c): register `wake` to be pinged (see
    /// [`notify_wake`]) after every event this server successfully queues
    /// from here on — `platform_transport::PlatformTransport::bind` is the one
    /// real caller, immediately after this server itself is bound, so
    /// `capsule::run`'s own `output_rx.recv_timeout` wakes on real voyage-
    /// pipe activity. `pub(crate)`: an implementation detail of the
    /// bridge, not part of this server's own public transport contract
    /// (`events`/`send`/`close`/...). Idempotent-once: a second call is a
    /// silent no-op (`OnceLock::set`'s own contract) — this server has
    /// exactly one bridge owner, which calls it at most once.
    pub(crate) fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        let _ = self.shared.activity_wake.set(wake);
    }

    /// Queue `bytes` for `conn_id`, tagged with `marker` if the caller
    /// wants a [`LaneEvent::Sent`] once the OS write physically
    /// completes. `bytes` must be non-empty and no larger than a single
    /// Win32 write can represent. Non-blocking: a full outbound budget or
    /// an unknown/already-closed connection both return `Err` immediately
    /// — backpressure POLICY belongs to whoever calls this.
    pub fn send(
        &self,
        conn_id: ConnId,
        bytes: Vec<u8>,
        marker: Option<SendMarker>,
    ) -> Result<(), TransportError> {
        if bytes.is_empty() {
            return Err(TransportError::EmptyPayload);
        }
        if bytes.len() > u32::MAX as usize {
            return Err(TransportError::PayloadTooLarge(bytes.len()));
        }
        let len = bytes.len();
        let map = self.shared.conns.lock().unwrap();
        let conn = map
            .get(&conn_id)
            .ok_or(TransportError::UnknownConnection(conn_id))?;
        if !conn.outbound.try_reserve(len) {
            return Err(TransportError::QueueFull(conn_id));
        }
        if conn.sender.send(WriteCmd { bytes, marker }).is_err() {
            conn.outbound.release(len);
            return Err(TransportError::UnknownConnection(conn_id));
        }
        Ok(())
    }

    /// Request cancellation and reaper-owned joins at most once. Closed is queued after both joins; instance recycling
    /// follows close-event retirement. An already-claimed connection is a no-op.
    pub fn close(&self, conn_id: ConnId) {
        let map = self.shared.conns.lock().unwrap();
        if let Some(conn) = map.get(&conn_id) {
            request_teardown(
                &self.shared,
                conn_id,
                &conn.torn_down_requested,
                ClosedReason::Closed,
            );
        }
    }
}

/// TEST-ONLY (ADR 0041 step 6 U1b, Codex round-3/4/5 test premise-gap
/// fixes). Not compiled into a production build — `feature =
/// "test-support"` only (see `Cargo.toml`'s own doc on that feature).
#[cfg(any(test, feature = "test-support"))]
impl PipeServer {
    /// Poll until the accept loop's CURRENT `ConnectNamedPipe` has
    /// GENUINELY gone `ERROR_IO_PENDING` at the OS level
    /// (`IoSlot::is_genuinely_pending`, set only once `issue` has
    /// actually returned that code — NOT `AcceptState::current.is_some()`,
    /// populated BEFORE `ConnectNamedPipe` is ever called, and NOT plain
    /// `SlotState::Pending`, which is ALSO set for a synchronously-
    /// completed op still awaiting result collection — Codex round-4
    /// finding 3 / round-5 finding 2) or `timeout` elapses. This poll is
    /// a best-effort PRE-check only, deciding WHEN it is worth calling
    /// `disconnect_listener` — the actual PROOF returned is the TOCTOU-
    /// free latch `stop_accept_loop`'s own synchronized cancellation
    /// records in `ServerShared::accept_cancel_observed_genuine_pending`
    /// (Codex round-5 fix 2b/2c: being in one function does not itself
    /// eliminate a TOCTOU between a pre-check and a later act — the
    /// PROOF must come from the SAME critical section that performs the
    /// cancellation, which this method's call to `disconnect_listener`
    /// triggers). Returns that latch's value; `false` on timeout
    /// (`disconnect_listener` NOT called at all).
    pub fn assert_accept_parked_then_disconnect_listener_for_test(
        &mut self,
        timeout: Duration,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let slot = self
                .shared
                .accept
                .lock()
                .unwrap()
                .current
                .as_ref()
                .map(|(_, _, slot)| Arc::clone(slot));
            if let Some(slot) = slot {
                if slot.is_genuinely_pending() {
                    self.disconnect_listener();
                    return self
                        .shared
                        .accept_cancel_observed_genuine_pending
                        .load(Ordering::Acquire);
                }
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(JOIN_POLL_INTERVAL);
        }
    }

    /// Server-local checkpoints, retained after connection removal; never locks connection state.
    pub fn progress_for_test(&self) -> crate::lane::test_progress::Snapshot {
        self.shared.progress.snapshot()
    }

    /// Stop `conn`'s `role` worker at its exit point (after its last I/O and teardown request) until released.
    pub fn hold_worker_exit_for_test(
        &self,
        conn: ConnId,
        role: Role,
    ) -> crate::lane::test_progress::Pause {
        crate::lane::test_progress::Pause::new(self.shared.controls.arm_exit_hold(conn, role))
    }

    /// Make `conn`'s `role` worker panic at its exit point.
    pub fn inject_worker_panic_for_test(&self, conn: ConnId, role: Role) {
        self.shared.controls.arm_exit_panic(conn, role);
    }

    /// Make the reaper panic at its next pass that follows an intake, outside every transport lock.
    pub fn inject_reaper_panic_for_test(&self) {
        self.shared.controls.arm_panic("reaper.pass");
    }

    /// Make the acceptor panic immediately before it registers its next connection.
    pub fn inject_acceptor_panic_for_test(&self) {
        self.shared.controls.arm_panic("registration.barrier");
    }

    /// Stop the acceptor immediately before it registers its next connection, workers still gated, until released.
    pub fn pause_registration_for_test(&self) -> crate::lane::test_progress::Pause {
        crate::lane::test_progress::Pause::new(
            self.shared.controls.arm_barrier("registration.barrier"),
        )
    }

    /// Stop the reaper immediately before the next instance recycle, after both joins, until released.
    pub fn pause_recycle_for_test(&self) -> crate::lane::test_progress::Pause {
        crate::lane::test_progress::Pause::new(self.shared.controls.arm_barrier("recycle.barrier"))
    }

    /// Make the next recycle (`DisconnectNamedPipe`) fail, so the retained-dead path runs for real.
    pub fn fail_next_recycle_for_test(&self) {
        self.shared.controls.arm_failure("recycle");
    }

    /// Shorten the per-connection teardown budget (and the one `Drop` uses) for this server.
    pub fn set_teardown_deadline_for_test(&self, deadline: Duration) {
        self.shared.controls.set_teardown_deadline(deadline);
    }

    /// Poll until `conn_id`'s writer has genuinely gone `ERROR_IO_PENDING`
    /// at the OS level (`IoSlot::is_genuinely_pending`) or `timeout`
    /// elapses. `TransportError::QueueFull` alone only proves the outbound
    /// BYTE budget is reserved, and plain `SlotState::Pending` is ALSO
    /// set for a synchronously-completed write still awaiting result
    /// collection (Codex round-4 finding 3 / round-5 finding 2) — neither
    /// proves the writer thread has actually reached a GENUINE pending
    /// `WriteFile` against a peer that never drains. This is a
    /// best-effort PRE-check only, deciding WHEN it is worth proceeding
    /// to teardown — see
    /// `conn_write_was_genuinely_pending_at_teardown_for_test` for the
    /// actual, TOCTOU-free proof, which must be read AFTER
    /// `disconnect_listener` has run.
    pub fn conn_write_pending_for_test(&self, conn_id: ConnId, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let pending = self
                .shared
                .conns
                .lock()
                .unwrap()
                .get(&conn_id)
                .map(|c| c.write_slot.is_genuinely_pending())
                .unwrap_or(false);
            if pending {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(JOIN_POLL_INTERVAL);
        }
    }

    /// The TOCTOU-free proof (Codex round-5 fix 2b/2c) that `conn_id`'s
    /// WRITE slot was GENUINELY asynchronously pending at the exact
    /// synchronized instant `disconnect_listener`'s own cancellation
    /// pass touched it — decided under the SAME lock acquisition that
    /// performed the cancellation, so (unlike a separate pre-check) it
    /// cannot go stale between being observed and being acted on. Call
    /// AFTER `disconnect_listener`, never before. `None` if `conn_id`
    /// was never live at any `disconnect_listener` call.
    pub fn conn_write_was_genuinely_pending_at_teardown_for_test(
        &self,
        conn_id: ConnId,
    ) -> Option<bool> {
        self.shared
            .write_cancel_observed_genuine_pending
            .lock()
            .unwrap()
            .get(&conn_id)
            .copied()
    }
}

impl PipeServer {
    /// Phase one of teardown: make the pipe NAME disappear AND issue cancellation to every worker -- synchronous, no
    /// blocking join. Latches [`ServerShared::dropping`] FIRST, cancels a pending accept ([`stop_accept_loop`]), THEN
    /// cancels every live connection's read AND write I/O, latching whether its WRITE slot was genuinely pending at
    /// that instant into [`ServerShared::write_cancel_observed_genuine_pending`]. The explicit `CancelIoEx` pass runs
    /// BEFORE `close_all`: real Windows showed `CloseHandle` alone does not promptly unstick a write stalled on
    /// full-buffer backpressure, and the reaper's claim cancels under the same `conns` lock, so no claimed pair's
    /// cancellation can fall after the handles are closed.
    ///
    /// After cancelling live and already-claimed I/O, close_all closes every registered instance. Registered worker
    /// pairs remain for the reaper to own and join; phase one performs no worker join and creates no detached-worker
    /// list. Registry liveness checks remain mandatory. Idempotent.
    pub fn disconnect_listener(&mut self) {
        self.shared.dropping.store(true, Ordering::Release);
        stop_accept_loop(&self.shared);
        self.shared.accept_cv.notify_all();
        {
            let map = self.shared.conns.lock().unwrap();
            let mut write_latches = self
                .shared
                .write_cancel_observed_genuine_pending
                .lock()
                .unwrap();
            for (&conn_id, conn) in map.iter() {
                let read_was_pending = conn
                    .read_slot
                    .cancel_registered(&self.shared.instances, conn.registry_id);
                let write_was_pending = conn
                    .write_slot
                    .cancel_registered(&self.shared.instances, conn.registry_id);
                self.shared.progress.note(
                    Some(conn_id),
                    "cancel.registered",
                    format_args!(
                        "read_genuinely_pending={read_was_pending} write_genuinely_pending={write_was_pending}"
                    ),
                );
                write_latches.insert(conn_id, write_was_pending);
            }
        }
        // THE atomic close -- see this method's own doc and `InstanceRegistry`'s.
        self.shared.instances.close_all();
        {
            let mut st = self.shared.accept.lock().unwrap();
            st.recycled.clear();
            st.retained_dead.clear();
        }
        if !self.shared.sweep_nudged.swap(true, Ordering::AcqRel) {
            // Nonblocking: a full inbox already has the reaper awake, and the slack keeps the one `Shutdown` a slot.
            let _ = self.shared.reaper_tx.try_send(ReaperMsg::Wake);
        }
    }

    /// Phase two signals the reaper with the caller's absolute deadline and waits for aggregate completion. Every
    /// registered pair is reaper-owned, including phase-one shutdown. False means expiry or latched teardown failure.
    /// Call disconnect_listener first; the phases remain separately observable.
    pub fn join_workers(&mut self, deadline: Instant) -> bool {
        self.shared
            .progress
            .note(None, "server.join.begin", "begin");
        signal_shutdown(&self.shared, deadline);
        let mut joins = ThreadJoins::default();
        if let Some(jh) = self.accept_jh.take() {
            joins.record("sot-pipe", "acceptor", join_checked(jh, deadline));
        }
        if let Some(jh) = self.reaper_jh.take() {
            joins.record("sot-pipe", "reaper", join_checked(jh, deadline));
        }
        if joins.failed() {
            // A server thread unfinished at the deadline, or one that panicked, fails this teardown for good.
            self.shared.teardown_failed.store(true, Ordering::Release);
        }
        let failed = self.shared.teardown_failed.load(Ordering::Acquire);
        self.shared
            .progress
            .note(None, "server.join.end", joins.result(failed));
        !failed
    }
}

impl Drop for PipeServer {
    /// Drop invokes both teardown phases with the existing aggregate budget without extending an earlier shutdown
    /// deadline. Failure is reported loudly; completed panic and unfinished expiry are distinct. The continuing reaper
    /// retains unfinished registered pairs until completion or process exit.
    fn drop(&mut self) {
        self.disconnect_listener();
        let deadline = Instant::now() + self.shared.controls.teardown_deadline();
        if !self.join_workers(deadline) {
            report_server_teardown_failed("sot-pipe");
        }
    }
}

/// L1-unix LU3a (ADR 0043 decision 19): `PipeServer`'s own `LaneServer`
/// seam — pure delegation to the inherent methods above, which already
/// have these exact signatures (modulo the error type, unified by
/// decision 17). Every existing consumer keeps calling the concrete
/// `PipeServer` methods directly; nothing is generic yet (LU3c).
impl LaneServer for PipeServer {
    fn bind_supervisor(h: &str, max_connections: u32) -> Result<Self, TransportError> {
        PipeServer::bind_supervisor(h, max_connections)
    }

    fn events(&self) -> &std::sync::mpsc::Receiver<LaneEvent> {
        PipeServer::events(self)
    }

    fn send(
        &self,
        conn: ConnId,
        bytes: Vec<u8>,
        marker: Option<u64>,
    ) -> Result<(), TransportError> {
        PipeServer::send(self, conn, bytes, marker)
    }

    fn close(&self, conn: ConnId) {
        PipeServer::close(self, conn)
    }

    fn disconnect_listener(&mut self) {
        PipeServer::disconnect_listener(self)
    }

    fn join_workers(&mut self, deadline: Instant) -> bool {
        PipeServer::join_workers(self, deadline)
    }
}
