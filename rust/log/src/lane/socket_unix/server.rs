//! The server: `SocketServer`, its bind constructors, send/close and two-phase teardown.

use super::*;
use super::accept::accept_loop;
use super::conn::{reaper_loop, request_teardown};
use super::listener::{create_and_bind_listener, ensure_private_runtime_dir, open_verified_dir_fd, set_cloexec, set_nonblocking};

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
    /// Reader/writer `JoinHandle`s for every connection
    /// [`SocketServer::disconnect_listener`] closed directly — their
    /// `ConnHandle` never reaches the reaper (it drained `shared.conns`
    /// itself), so nothing else would ever join them.
    /// [`SocketServer::join_workers`] joins every entry here under the
    /// SAME shared deadline as the acceptor and reaper.
    detached_workers: Vec<JoinHandle<()>>,
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
            source: io::Error::new(io::ErrorKind::InvalidInput, "socket file name contains a NUL byte"),
        })?;

        let listener = create_and_bind_listener(dir_fd.as_raw_fd(), &file_name, &path, max_connections)?;

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
                let _ = shared.reaper_tx.send(ReaperMsg::Shutdown);
                reaper_jh.join().ok();
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
            detached_workers: Vec::new(),
        })
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

    /// Request that `conn_id` be torn down: cancelled (`shutdown(2)`),
    /// both threads joined. Fire-and-forget — this enqueues the request
    /// at most once for the reaper thread; completion is observed as
    /// [`LaneEvent::Closed`]. A no-op if `conn_id` is already gone or
    /// already has a teardown in flight.
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
    /// `dropping` transition — every other step below stays unconditional
    /// and idempotent, matching before: wake the accept loop out of
    /// `poll(2)`, then `shutdown(SHUT_RDWR)` every currently-live
    /// connection's stream (property 12) and move its reader/writer
    /// threads into `detached_workers` for [`Self::join_workers`] to join
    /// later.
    pub fn disconnect_listener(&mut self) {
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
                libc::unlinkat(self.shared.dir_fd.as_raw_fd(), self.shared.file_name.as_ptr(), 0);
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
        let drained: Vec<ConnHandle> = {
            let mut map = self.shared.conns.lock().unwrap();
            map.drain().map(|(_, conn)| conn).collect()
        };
        for conn in drained {
            // Fast-exits a reader stuck retrying `deliver_bytes` against
            // a saturated events channel -- the same role `pipe_win`'s
            // own `IoSlot::is_closing` plays there once its own
            // `cancel_registered` latches `Closing`.
            conn.torn_down_requested.store(true, Ordering::Release);
            unsafe { libc::shutdown(conn.stream.as_raw_fd(), libc::SHUT_RDWR) };
            drop(conn.sender); // unblocks a writer idle-waiting on `recv`
            self.detached_workers.push(conn.reader_jh);
            self.detached_workers.push(conn.writer_jh);
        }
    }

    /// Phase two: tell the reaper to drain (a no-op for any connection
    /// `disconnect_listener` already claimed), then wait for the accept
    /// thread, the reaper thread, AND every detached connection worker
    /// `disconnect_listener` stashed — ALL sharing ONE absolute
    /// `deadline`. `true` iff every one finished within budget; `false`
    /// (LOUD — the caller MUST treat this as terminal) on expiry. Call
    /// [`disconnect_listener`](Self::disconnect_listener) first — this
    /// method does not call it, so the two phases stay independently
    /// observable (and independently testable).
    pub fn join_workers(&mut self, deadline: Instant) -> bool {
        let mut ok = true;
        if let Some(jh) = self.accept_jh.take() {
            ok = join_within(jh, deadline) && ok;
        }
        let _ = self.shared.reaper_tx.send(ReaperMsg::Shutdown);
        if let Some(jh) = self.reaper_jh.take() {
            ok = join_within(jh, deadline) && ok;
        }
        for jh in self.detached_workers.drain(..) {
            ok = join_within(jh, deadline) && ok;
        }
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

    fn send(&self, conn: ConnId, bytes: Vec<u8>, marker: Option<u64>) -> Result<(), TransportError> {
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
    /// The two teardown phases in order, with a FRESH pinned 20 s
    /// budget computed here — mirrors `pipe_win::PipeServer`'s own
    /// `Drop`. This is the SAFETY-NET path; the designed path computes
    /// ONE deadline in the capsule's own run loop and calls both methods
    /// explicitly with it.
    fn drop(&mut self) {
        self.disconnect_listener();
        let deadline = Instant::now() + TEARDOWN_AGGREGATE_DEADLINE;
        if !self.join_workers(deadline) {
            eprintln!(
                "sot-sock: teardown did not complete within its {TEARDOWN_AGGREGATE_DEADLINE:?} \
                 aggregate deadline; a worker thread may still be running"
            );
        }
    }
}
