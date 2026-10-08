//! The server: `SocketServer`, its bind constructors, send/close and two-phase teardown.

use super::accept::accept_loop;
use super::conn::{reaper_loop, request_teardown, signal_shutdown};
use super::listener::{
    create_and_bind_listener, ensure_private_runtime_dir, open_verified_dir_fd, set_cloexec,
    set_nonblocking,
};
use super::*;

/// The server side of one voyage's (or the supervisor lane's) socket:
/// [`SocketServer::bind`] creates the listener and starts accepting;
/// connections and their bytes/completions/closes surface on
/// [`SocketServer::events`]. Mirrors `pipe_win::PipeServer`'s own
/// lifetime rule verbatim: `bind` is an explicit constructor the CALLER
/// is responsible for invoking only after the endpoint's lifetime lock is
/// held (ADR 0043 decision 2 — the voyage writer lock for
/// `voyage-<id>.sock`, the supervisor fence for `supervisor-<h>.sock`),
/// and for dropping the returned `SocketServer` before releasing that
/// lock.
pub struct SocketServer {
    shared: Arc<ServerShared>,
    events_rx: Receiver<LaneEvent>,
    accept_jh: Option<JoinHandle<()>>,
    reaper_jh: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for SocketServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocketServer").finish_non_exhaustive()
    }
}

impl SocketServer {
    /// Bind `<runtime_dir>/voyage-<voyage_id>.sock` and start accepting.
    /// `max_connections` must be in `1..=255`.
    pub fn bind(voyage_id: &str, max_connections: u32) -> Result<Self, TransportError> {
        let path = voyage_socket_path(voyage_id)?;
        Self::bind_named(path, max_connections)
    }

    /// The supervisor lane's own socket,
    /// `<runtime_dir>/supervisor-<h>.sock` — otherwise identical to
    /// [`Self::bind`].
    pub fn bind_supervisor(h: &str, max_connections: u32) -> Result<Self, TransportError> {
        let path = supervisor_socket_path(h)?;
        Self::bind_named(path, max_connections)
    }

    fn bind_named(path: PathBuf, max_connections: u32) -> Result<Self, TransportError> {
        if !(1..=255).contains(&max_connections) {
            return Err(TransportError::InvalidMaxConnections);
        }
        let dir = path
            .parent()
            .expect("a socket path built by socket_path() always has a parent (the runtime dir)");
        ensure_private_runtime_dir(dir)?;
        // Module doc "Security": the by-path pre-check above is not the
        // authoritative one. Open the directory itself `O_NOFOLLOW` and
        // `fstat` the resulting fd -- keeping it open for this server's
        // whole life so every later `*at()` call is anchored to THIS
        // verified inode, never a fresh path lookup that could re-walk a
        // since-swapped ancestor.
        let dir_fd = open_verified_dir_fd(dir)?;
        let file_name = path
            .file_name()
            .expect("a socket path built by socket_path() always has a file name");
        let file_name = CString::new(file_name.as_bytes()).map_err(|_| TransportError::Io {
            op: "CString::new(socket file name)",
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "socket file name contains a NUL byte",
            ),
        })?;

        let listener =
            create_and_bind_listener(dir_fd.as_raw_fd(), &file_name, &path, max_connections)?;

        // The wake self-pipe (module doc: "the accept loop wakes via
        // poll(2) over a self-pipe"). O_NONBLOCK on both ends: the
        // accept loop's own drain read must never block, and a write
        // from `disconnect_listener` must never block either (its own
        // "never blocks" contract) -- one byte always fits in a fresh
        // pipe's buffer, but non-blocking costs nothing and removes any
        // doubt.
        //
        // `pipe(2)` + `fcntl` (not Linux's own combined-flag `pipe2(2)`):
        // macOS/BSD has no `pipe2` at all, so this crate sets CLOEXEC and
        // NONBLOCK as two ordinary, portable `fcntl` calls per fd instead
        // -- identical end state, one extra syscall pair, and it now
        // compiles (and behaves identically) on every Unix target this
        // workspace's CI checks, not only Linux.
        let mut fds: [RawFd; 2] = [-1, -1];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        if rc != 0 {
            let err = TransportError::Io {
                op: "pipe(wake)",
                source: io::Error::last_os_error(),
            };
            unsafe { libc::unlinkat(dir_fd.as_raw_fd(), file_name.as_ptr(), 0) };
            return Err(err);
        }
        // SAFETY: `pipe` just returned these two fds; each is valid,
        // open, and not owned by anything else yet.
        let wake_read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let wake_write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        crate::lane::test_progress::birth("wake.read", wake_read.as_raw_fd());
        crate::lane::test_progress::birth("wake.write", wake_write.as_raw_fd());
        for fd in [wake_read.as_raw_fd(), wake_write.as_raw_fd()] {
            if let Err(e) = set_cloexec(fd).and_then(|()| set_nonblocking(fd)) {
                let err = TransportError::Io {
                    op: "fcntl(wake pipe)",
                    source: e,
                };
                unsafe { libc::unlinkat(dir_fd.as_raw_fd(), file_name.as_ptr(), 0) };
                return Err(err);
            }
        }

        let (events_tx, events_rx) = mpsc::sync_channel(EVENTS_CHANNEL_CAP);
        let (reaper_tx, reaper_rx) =
            mpsc::sync_channel(max_connections as usize + REAPER_INBOX_SLACK);

        let shared = Arc::new(ServerShared {
            conns: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            reaper_tx,
            events_tx,
            activity_wake: OnceLock::new(),
            max_connections,
            dropping: AtomicBool::new(false),
            accept_stopping: AtomicBool::new(false),
            dir_fd,
            file_name,
            wake_write,
            probes: Probes::default(),
            progress: crate::lane::test_progress::Progress::default(),
            pending: AtomicUsize::new(0),
            teardown_failed: AtomicBool::new(false),
            sweep_nudged: AtomicBool::new(false),
            shutdown_sent: AtomicBool::new(false),
            controls: crate::lane::test_progress::Controls::default(),
        });

        // Spawn the reaper FIRST -- if the accept thread then fails to
        // spawn, unwind the reaper (nothing queued yet, so its own
        // `Shutdown` drain is instant) rather than leave it running
        // forever with no accept thread able to feed it. Mirrors
        // `pipe_win::PipeServer::bind_named`'s own ordering.
        let reaper_jh = match thread::Builder::new()
            .name("sot-sock-reaper".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || reaper_loop(shared, reaper_rx)
            }) {
            Ok(jh) => jh,
            Err(e) => {
                unsafe { libc::unlinkat(shared.dir_fd.as_raw_fd(), shared.file_name.as_ptr(), 0) };
                return Err(TransportError::Io {
                    op: "spawn reaper thread",
                    source: e,
                });
            }
        };

        let accept_jh = thread::Builder::new()
            .name("sot-sock-accept".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || accept_loop(shared, listener, wake_read)
            });
        let accept_jh = match accept_jh {
            Ok(jh) => jh,
            Err(e) => {
                signal_shutdown(&shared, Instant::now() + TEARDOWN_AGGREGATE_DEADLINE);
                Self::observe_bind_join(&shared, reaper_jh);
                unsafe { libc::unlinkat(shared.dir_fd.as_raw_fd(), shared.file_name.as_ptr(), 0) };
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

    fn observe_bind_join(shared: &ServerShared, reaper: JoinHandle<()>) {
        shared.progress.note(None, "server.join.begin", "begin");
        let joined = reaper.join();
        shared.progress.note(
            None,
            "server.join.end",
            if joined.is_ok() { "ok" } else { "panic" },
        );
    }

    /// The event stream: `Accepted`/`Bytes`/`Sent`/`Closed`/`AcceptError`,
    /// in the order this transport observed them. Single-consumer by
    /// convention (a `Receiver` is not `Sync`).
    pub fn events(&self) -> &Receiver<LaneEvent> {
        &self.events_rx
    }

    /// Switch-latency Phase 1 (c): register `wake` to be pinged (see
    /// [`notify_wake`]) after every event this server successfully queues
    /// from here on — `platform_transport::PlatformTransport::bind` is the
    /// one real caller, immediately after this server itself is bound, so
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
    /// completes. `bytes` must be non-empty. Non-blocking: a full
    /// outbound budget or an unknown/already-closed connection both
    /// return `Err` immediately — backpressure POLICY belongs to
    /// whoever calls this.
    pub fn send(
        &self,
        conn_id: ConnId,
        bytes: Vec<u8>,
        marker: Option<SendMarker>,
    ) -> Result<(), TransportError> {
        if bytes.is_empty() {
            return Err(TransportError::EmptyPayload);
        }
        // Parity with `pipe_win::PipeServer::send`'s own near-unreachable
        // representable-size check -- POSIX `write(2)` has no fixed
        // per-call ceiling this transport itself needs to enforce (the
        // writer loop below loops over partial writes), but a single
        // absurdly large payload is still rejected loudly rather than
        // silently accepted only to blow the (far smaller)
        // `OUTBOUND_BUDGET_BYTES` check that follows.
        if bytes.len() > isize::MAX as usize {
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

    /// Request cancellation and reaper-owned joins at most once. Closed is queued after both joins. An
    /// already-claimed connection is a no-op.
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

    /// Phase one of teardown: make the socket NAME disappear AND issue
    /// cancellation to every worker — synchronous, no blocking join.
    /// [`ServerShared::dropping`]'s `compare_exchange` is the unlink's
    /// EXACTLY-ONCE latch (Codex review finding 1, property 5): a second
    /// call to this method — or `Drop`'s own call, always made after an
    /// explicit one — must NOT unlink again, because by then a caller
    /// could legitimately have bound a REPLACEMENT `SocketServer` at the
    /// identical path (the same voyage's next leg, say), and a second
    /// unconditional unlink would delete THAT server's endpoint instead
    /// of this (already torn-down) one's. Unlinking is therefore anchored
    /// via `unlinkat` to `dir_fd`/`file_name` (module doc "Security"),
    /// never a fresh by-path lookup, and gated on actually WINNING the
    /// `dropping` transition.
    ///
    /// Wake the acceptor and cancel every registered connection. Its worker pair remains available for the reaper to
    /// claim; phase one never creates a caller-owned join list.
    pub fn disconnect_listener(&mut self) {
        self.shared
            .progress
            .note(None, "listener.disconnect", "begin");
        let unlink_once = self
            .shared
            .dropping
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        self.shared.accept_stopping.store(true, Ordering::Release);
        if unlink_once {
            // SAFETY: `dir_fd` was opened, `O_NOFOLLOW`+`fstat`-verified,
            // and kept open for this server's whole life; `unlinkat`
            // removes only the entry named `file_name` inside THAT
            // directory, never re-resolving any path.
            unsafe {
                libc::unlinkat(
                    self.shared.dir_fd.as_raw_fd(),
                    self.shared.file_name.as_ptr(),
                    0,
                );
            }
        }
        let wake_byte = [0u8; 1];
        // A failed/short write here just means the acceptor will notice
        // `accept_stopping` on its NEXT ordinary wakeup instead of this
        // one -- never a correctness issue, only latency, and one this
        // transport does not otherwise promise a bound on beyond
        // `TEARDOWN_AGGREGATE_DEADLINE`'s own join.
        let _ = unsafe {
            libc::write(
                self.shared.wake_write.as_raw_fd(),
                wake_byte.as_ptr().cast(),
                1,
            )
        };
        {
            let conns = self.shared.conns.lock().unwrap();
            for (&id, conn) in conns.iter() {
                // Fast-exits a reader stuck retrying `deliver_bytes`
                // against a saturated events channel -- the same role
                // `pipe_win`'s own `IoSlot::is_closing` plays there once its
                // own `cancel_registered` latches `Closing`.
                conn.torn_down_requested.store(true, Ordering::Release);
                super::conn::observe_shutdown(
                    &self.shared.progress,
                    Some(id),
                    &conn.stream,
                    "rust/log/src/lane/socket_unix/server.rs::disconnect_listener",
                );
                self.shared.progress.note(Some(id), "phase_one.route", "reaper");
            }
        }
        if !self.shared.sweep_nudged.swap(true, Ordering::AcqRel) {
            // Nonblocking: a full inbox already has the reaper awake, and the slack keeps the one `Shutdown` a slot.
            let _ = self.shared.reaper_tx.try_send(ReaperMsg::Sweep);
        }
        self.shared.progress.note(None, "listener.disconnect", "ok");
    }

    /// Phase two signals the reaper with the caller's absolute deadline and waits for aggregate completion. Every
    /// registered pair is reaper-owned, including phase-one shutdown. False means expiry or latched teardown failure.
    /// Call disconnect_listener first; the phases remain separately observable.
    pub fn join_workers(&mut self, deadline: Instant) -> bool {
        self.shared
            .progress
            .note(None, "server.join.begin", "begin");
        signal_shutdown(&self.shared, deadline);
        let mut expired = false;
        if let Some(jh) = self.accept_jh.take() {
            expired |= !self.join_observed(jh, deadline);
        }
        if let Some(jh) = self.reaper_jh.take() {
            expired |= !self.join_observed(jh, deadline);
        }
        if expired {
            // An aggregate worker unfinished at the deadline fails this teardown for good.
            self.shared.teardown_failed.store(true, Ordering::Release);
        }
        let failed = self.shared.teardown_failed.load(Ordering::Acquire);
        self.shared.progress.note(
            None,
            "server.join.end",
            if expired {
                "deadline-expired"
            } else if failed {
                "teardown-failed"
            } else {
                "ok"
            },
        );
        !failed
    }

    fn join_observed(&self, jh: JoinHandle<()>, deadline: Instant) -> bool {
        Self::join_with_progress(&self.shared, jh, deadline)
    }

    fn join_with_progress(shared: &ServerShared, jh: JoinHandle<()>, deadline: Instant) -> bool {
        #[cfg(any(test, feature = "test-support"))]
        let (id, role) = {
            let name = jh.thread().name().unwrap_or("pending");
            let id = name.rsplit('-').next().and_then(|n| n.parse().ok());
            let role = if name.starts_with("sot-sock-r-") {
                "reader"
            } else if name.starts_with("sot-sock-w-") {
                "writer"
            } else {
                "server"
            };
            shared
                .progress
                .note(id, "worker.join.begin", format_args!("role={role}"));
            (id, role)
        };
        let ok = join_within(jh, deadline);
        #[cfg(any(test, feature = "test-support"))]
        shared
            .progress
            .note(id, "worker.join.end", format_args!("role={role} ok={ok}"));
        #[cfg(not(any(test, feature = "test-support")))]
        let _ = shared;
        ok
    }
}

/// L1-unix LU3a (ADR 0043 decision 19): `SocketServer`'s own `LaneServer`
/// seam — pure delegation to the inherent methods above, which already
/// have these exact signatures (modulo the error type, unified by
/// decision 17). Every existing consumer keeps calling the concrete
/// `SocketServer` methods directly; nothing is generic yet (LU3c).
impl LaneServer for SocketServer {
    fn bind_supervisor(h: &str, max_connections: u32) -> Result<Self, TransportError> {
        SocketServer::bind_supervisor(h, max_connections)
    }

    fn events(&self) -> &std::sync::mpsc::Receiver<LaneEvent> {
        SocketServer::events(self)
    }

    fn send(
        &self,
        conn: ConnId,
        bytes: Vec<u8>,
        marker: Option<u64>,
    ) -> Result<(), TransportError> {
        SocketServer::send(self, conn, bytes, marker)
    }

    fn close(&self, conn: ConnId) {
        SocketServer::close(self, conn)
    }

    fn disconnect_listener(&mut self) {
        SocketServer::disconnect_listener(self)
    }

    fn join_workers(&mut self, deadline: Instant) -> bool {
        SocketServer::join_workers(self, deadline)
    }
}

/// TEST-SUPPORT ONLY (`#[cfg(any(test, feature = "test-support"))]`,
/// matching [`Probes`]'s own gate and `lane/pipe_win/`'s identical
/// convention for its own test-only methods): a way for a test to WAIT on
/// an OBSERVED precondition (the events channel genuinely full; a
/// `Bytes` delivery genuinely abandoned) instead of assuming either from
/// a fixed sleep, a client-side stall heuristic, or a fixed connection
/// count — Codex review round 2's own critique of the first fix pass.
#[cfg(any(test, feature = "test-support"))]
impl SocketServer {
    /// Opaque fixture hold of this recorder, with no connection-state lock.
    pub fn hold_progress_for_test(&self) -> ProgressHold {
        let shared = Arc::clone(&self.shared);
        let (ready, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let holder = thread::spawn(move || {
            let _held = shared.progress.hold();
            ready.send(()).expect("observe the recorder holder");
            let _ = released.recv();
        });
        observed
            .recv_timeout(Duration::from_secs(5))
            .expect("recorder holder did not become ready");
        ProgressHold {
            release,
            holder: Some(holder),
        }
    }

    /// Stop `conn`'s `role` worker at its exit point (after its last I/O and teardown request) until released.
    pub fn hold_worker_exit_for_test(&self, conn: ConnId, role: crate::lane::test_progress::Role) -> crate::lane::test_progress::Pause {
        crate::lane::test_progress::Pause::new(self.shared.controls.arm_exit_hold(conn, role))
    }

    /// Make `conn`'s `role` worker panic at its exit point.
    pub fn inject_worker_panic_for_test(&self, conn: ConnId, role: crate::lane::test_progress::Role) {
        self.shared.controls.arm_exit_panic(conn, role);
    }

    /// Stop the acceptor immediately before it registers its next connection, workers still gated, until released.
    pub fn pause_registration_for_test(&self) -> crate::lane::test_progress::Pause {
        crate::lane::test_progress::Pause::new(self.shared.controls.arm_barrier("registration.barrier"))
    }

    /// Shorten the per-connection teardown budget (and the one `Drop` uses) for this server.
    pub fn set_teardown_deadline_for_test(&self, deadline: Duration) {
        self.shared.controls.set_teardown_deadline(deadline);
    }

    /// Server-local checkpoints, retained after connection removal; never locks connection state.
    pub fn progress_for_test(&self) -> crate::lane::test_progress::Snapshot {
        self.shared.progress.snapshot()
    }

    /// How many times a `Bytes` delivery attempt has observed the events
    /// channel full (`TrySendError::Full`) since this server was bound.
    pub fn probe_events_full_bytes(&self) -> usize {
        self.shared.probes.events_full_bytes()
    }
    /// Same, for a lifecycle event (`Accepted`/`Sent`/`Closed`/
    /// `AcceptError`) delivery attempt.
    pub fn probe_events_full_lifecycle(&self) -> usize {
        self.shared.probes.events_full_lifecycle()
    }
    /// How many times [`deliver_bytes`] has genuinely given up (torn-down
    /// or [`BYTES_ABANDON_AFTER`] elapsed) — never merely found the
    /// consumer gone (channel disconnected), which is a different case.
    pub fn probe_bytes_abandoned(&self) -> usize {
        self.shared.probes.bytes_abandoned()
    }
}

impl Drop for SocketServer {
    /// Drop invokes both teardown phases with the existing aggregate budget without extending an earlier shutdown
    /// deadline. Failure is reported loudly; completed panic and unfinished expiry are distinct. The continuing reaper
    /// retains unfinished registered pairs until completion or process exit.
    fn drop(&mut self) {
        self.disconnect_listener();
        let deadline = Instant::now() + self.shared.controls.teardown_deadline();
        if !self.join_workers(deadline) {
            report_server_teardown_failed("sot-sock");
        }
    }
}

/// Opaque test fixture: owns the observed holder of only this server's recorder.
#[cfg(any(test, feature = "test-support"))]
pub struct ProgressHold {
    release: mpsc::Sender<()>,
    holder: Option<JoinHandle<()>>,
}
#[cfg(any(test, feature = "test-support"))]
impl Drop for ProgressHold {
    fn drop(&mut self) {
        let _ = self.release.send(());
        self.holder
            .take()
            .unwrap()
            .join()
            .expect("recorder holder panicked");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_progresses_while_recorder_is_busy() {
        let test = "lane::socket_unix::server::tests::transport_progresses_while_recorder_is_busy";
        if !crate::test_isolated::run_isolated(test) {
            return;
        }
        let root = tempfile::Builder::new()
            .prefix("sot-busy-")
            .tempdir_in("/tmp")
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::env::set_var("SOT_RUNTIME_DIR", root.path());
        let id = uuid::Uuid::now_v7().to_string();
        let server = SocketServer::bind(&id, 1).unwrap();
        let path = voyage_socket_path(&id).unwrap();
        let before = server.progress_for_test().skipped;
        let held = server.hold_progress_for_test();
        let (done, observed) = mpsc::channel();
        let producer = thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                use std::io::{Read, Write};
                let deadline = Instant::now() + Duration::from_secs(5);
                let event = || {
                    server
                        .events()
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .unwrap()
                };
                let mut client = UnixStream::connect(path).unwrap();
                let LaneEvent::Accepted(conn) = event() else {
                    panic!("expected Accepted");
                };
                client
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                client
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                client.write_all(b"a").unwrap();
                assert!(matches!(event(), LaneEvent::Bytes(_, ref bytes) if bytes == b"a"));
                server.send(conn, b"b".to_vec(), Some(7)).unwrap();
                let mut byte = [0];
                client.read_exact(&mut byte).unwrap();
                assert_eq!(byte, *b"b");
                assert!(matches!(event(), LaneEvent::Sent(_, 7)));
                server.close(conn);
                assert!(matches!(
                    event(),
                    LaneEvent::Closed(_, ClosedReason::Closed)
                ));
                assert!(server.progress_for_test().skipped > before);
            }));
            done.send(result.is_ok()).unwrap();
            (server, result)
        });
        let completed_while_held = observed.recv_timeout(Duration::from_secs(6));
        drop(held); // Always release before joining or ordinary server cleanup, including a red.
        let (server, result) = producer.join().unwrap();
        drop(server);
        assert!(
            completed_while_held == Ok(true),
            "transport stopped while recorder was busy"
        );
        result.unwrap();
        eprintln!("recorder-proof test={test} progress=while-held skipped=increased bodies=1");
    }
}
