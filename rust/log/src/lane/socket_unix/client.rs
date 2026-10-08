//! The client: `SocketClient`, its `Client`/`Endpoint` impls, and the unchallenged and challenged connects.

use super::*;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::lane::client::map_peer_auth_outcome;
use super::connect::{capture_connect_anchor_boot_ticks, one_connect_attempt, ConnectAttempt};

// ---------------------------------------------------------------------
// L1-unix LU1c: the client side of one voyage's (or the supervisor
// lane's) socket. Mirrors `pipe_win::PipeClient` in SHAPE — the exact
// three-method surface (property 26) — with `shutdown(2)` standing in
// for `CancelIoEx` (ADR 0043 decision 5) and no completion-proof
// apparatus at all: POSIX `read`/`write` never borrow the caller's
// buffer past the call, so there is nothing here to leak or abort over
// (`pipe_win`'s own `CompletionUnproven` has no Unix analogue).
// ---------------------------------------------------------------------

/// The client side of one voyage's socket: `read`/`write_all` are
/// blocking from the calling thread's own perspective, but `SocketClient`
/// is `Sync` — a second thread may call [`SocketClient::cancel`] at any
/// time to unblock whichever of the two is currently in flight, via
/// `shutdown(2)` on the shared fd (ADR 0043 decision 5). `read_slot`/
/// `write_slot` are plain `Mutex<()>`s used only via `try_lock` — the
/// direct analogue of `pipe_win::IoSlot`'s own same-direction rejection,
/// simplified because POSIX has no per-op cancel target to protect a
/// shared `OVERLAPPED`-like structure from: the fd itself is the only
/// shared state, and `shutdown(2)` is safe to call while a read/write is
/// concurrently in flight on it.
pub struct SocketClient {
    stream: UnixStream,
    /// Set by [`cancel`](Self::cancel) BEFORE the `shutdown(2)` that
    /// unblocks a stalled read/write — checked on entry, on every
    /// partial-progress iteration, and after any error, so a call that
    /// races a cancel is classified `Cancelled` rather than an ordinary
    /// I/O failure (ADR 0043 decision 5), and a call made AFTER cancel
    /// completed is rejected before ever touching the OS again (property
    /// 34).
    cancelled: AtomicBool,
    read_slot: Mutex<()>,
    write_slot: Mutex<()>,
    /// TEST-SUPPORT ONLY (review round 2): set `true`, never cleared,
    /// the instant [`read`](Self::read) has genuinely ENTERED its
    /// critical section (immediately after `read_slot`'s lock succeeds)
    /// — a PASSIVE observation a test can wait on, unlike
    /// `read_slot.try_lock()` itself, which would momentarily contend
    /// for the SAME slot a real in-flight `read` holds and could make it
    /// see a spurious `ConcurrentSubmit`. Mirrors LU1b's own `Probes`:
    /// zero-cost outside a test build (the field and every store to it
    /// are `cfg`'d out).
    #[cfg(any(test, feature = "test-support"))]
    read_slot_entered: AtomicBool,
    /// The write twin of `read_slot_entered`, set inside
    /// [`write_all`](Self::write_all).
    #[cfg(any(test, feature = "test-support"))]
    write_slot_entered: AtomicBool,
    /// `CLOCK_BOOTTIME` sampled immediately BEFORE this connection's own
    /// `connect(2)` attempt began — NOT after it completed (review round
    /// fix: a post-connect sample left a pid-reuse window open between
    /// `connect` returning and the sample running), in the same clock
    /// ticks as `/proc/<pid>/stat` field 22 (`sysconf(_SC_CLK_TCK)`) —
    /// `challenge_unix::pin_peer`'s own race-free anchor: a peer that
    /// `connect()`s successfully was necessarily alive when THIS attempt
    /// began, so its start time must be strictly earlier, unless it is a
    /// replacement that started in the anchor-to-connect gap — which the
    /// pin's own strict `<` then correctly classifies `Undetermined`,
    /// never a false `Proven`. See
    /// [`connect_unix_socket_unchallenged`]'s own doc for where this is
    /// sampled. `0` on a non-Linux Unix target, where nothing ever reads
    /// it (the whole identity-pinning path is Linux-only; see
    /// `connect_voyage_socket`'s own two cfg'd bodies) — hence the
    /// `cfg_attr`: only the Linux `SocketChallengeable` impl below ever
    /// reads this field.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    connect_anchor_boot_ticks: u64,
}

impl std::fmt::Debug for SocketClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocketClient").finish_non_exhaustive()
    }
}

/// The connected socket, for a check that reads it without I/O (`identity::connect_own`'s account check).
impl std::os::fd::AsFd for SocketClient {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.stream)
    }
}

impl SocketClient {
    /// Blocking write of the whole buffer, cancellable from another
    /// thread via [`cancel`](Self::cancel). `bytes` must be non-empty. A
    /// concurrent SECOND `write_all` call from another thread returns
    /// `Err(TransportError::ConcurrentSubmit)` rather than racing this one's
    /// own partial-progress loop (property 34's sibling — decided BEFORE
    /// touching the OS). ADR 0043 decisions 5/7: completes on success, an
    /// error may follow partial delivery (the byte-stream-prefix property
    /// holds either way — nothing here rewinds bytes already written) —
    /// AND (review round fix, decision 7's own "a failed send latches the
    /// connection", the same rule the server's `writer_loop` already
    /// follows) a TERMINAL failure discovered by THIS call latches the
    /// connection closed before returning: it calls [`cancel`](Self::cancel)
    /// itself (reusing the SAME `cancelled` flag/`shutdown(2)` — one
    /// latch, not two independent ones), so every later `read`/`write_all`
    /// call sees `Cancelled` (property 34), while THIS call still returns
    /// its own ORIGINAL error, never `Cancelled` — the caller that
    /// actually observed the failure gets to know what it was.
    pub fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError> {
        let _guard = self
            .write_slot
            .try_lock()
            .map_err(|_| TransportError::ConcurrentSubmit)?;
        // TEST-SUPPORT ONLY (review round 2): a PASSIVE record that this
        // call has genuinely entered its critical section -- set once
        // the slot lock has already succeeded, so observing it costs a
        // real in-flight call nothing and never contends for the slot
        // itself (unlike a test calling `try_lock` directly would).
        #[cfg(any(test, feature = "test-support"))]
        self.write_slot_entered.store(true, Ordering::SeqCst);
        if bytes.is_empty() {
            return Err(TransportError::EmptyPayload);
        }
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(TransportError::Cancelled);
        }
        let mut remaining = bytes;
        while !remaining.is_empty() {
            // Checked on EVERY iteration (ADR 0043 decision 5), before
            // ever issuing the next `write` — a cancel that lands between
            // two partial writes is observed here, not only via the
            // error branch below.
            if self.cancelled.load(Ordering::SeqCst) {
                return Err(TransportError::Cancelled);
            }
            match (&self.stream).write(remaining) {
                Ok(0) => {
                    // This write never legitimately returns 0 for a
                    // non-empty buffer -- a terminal failure, latching
                    // the connection exactly like the `Err` branch below.
                    self.cancel();
                    return Err(TransportError::Io {
                        op: "write",
                        source: io::Error::new(
                            io::ErrorKind::WriteZero,
                            "write returned 0 with bytes still to send",
                        ),
                    });
                }
                Ok(n) => remaining = &remaining[n..],
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    if self.cancelled.load(Ordering::SeqCst) {
                        // Already cancelled by a CONCURRENT `cancel()`
                        // call racing this write -- that external cancel
                        // wins the classification; this call's own
                        // failure is simply what a `shutdown(2)` under it
                        // looks like, never surfaced as an ordinary I/O
                        // error once cancellation is already in play.
                        return Err(TransportError::Cancelled);
                    }
                    // A genuinely terminal write failure THIS call
                    // discovered (not a racing external cancel): latch
                    // the connection closed (decision 7) so every LATER
                    // call sees it as spent, but return the ORIGINAL
                    // error here -- this caller earned the real
                    // diagnostic, not a generic `Cancelled`.
                    self.cancel();
                    return Err(TransportError::Io { op: "write", source: e });
                }
            }
        }
        Ok(())
    }

    /// Blocking read into `buf`, cancellable from another thread via
    /// [`cancel`](Self::cancel). `buf` must be non-empty. A concurrent
    /// SECOND `read` call from another thread returns
    /// `Err(TransportError::ConcurrentSubmit)`. `Ok(0)` is ordered EOF
    /// (property 13) — UNLESS the cancelled flag is set, in which case a
    /// zero-length or error result is `Cancelled` instead, checked both
    /// BEFORE and AFTER the call (ADR 0043 decision 5): a genuinely
    /// delivered nonzero read is returned as-is even after a cancel —
    /// "queued input may still be returned after a cancel" — only the
    /// EOF/error tail end of a cancelled connection is reclassified.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let _guard = self
            .read_slot
            .try_lock()
            .map_err(|_| TransportError::ConcurrentSubmit)?;
        // TEST-SUPPORT ONLY (review round 2): see `write_all`'s own
        // identical comment on its write twin.
        #[cfg(any(test, feature = "test-support"))]
        self.read_slot_entered.store(true, Ordering::SeqCst);
        if buf.is_empty() {
            return Err(TransportError::EmptyPayload);
        }
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
                return Err(TransportError::Cancelled);
            }
            match (&self.stream).read(buf) {
                Ok(0) => {
                    return if self.cancelled.load(Ordering::SeqCst) {
                        Err(TransportError::Cancelled)
                    } else {
                        Ok(0)
                    };
                }
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return if self.cancelled.load(Ordering::SeqCst) {
                        Err(TransportError::Cancelled)
                    } else {
                        Err(TransportError::Io { op: "read", source: e })
                    };
                }
            }
        }
    }

    /// Abort whatever is in flight, from any thread: latch `cancelled`
    /// FIRST (SeqCst), THEN `shutdown(SHUT_RDWR)` — the direct analogue
    /// of `pipe_win::IoSlot::cancel`'s own "latch, then request" order,
    /// simplified because POSIX's `shutdown(2)` unblocks a blocked read
    /// AND a blocked write in one call, from any thread, without needing
    /// to know which direction (if either) is currently mid-call.
    /// Idempotent. A cancelled client permanently rejects later
    /// submissions (property 34) — every later `read`/`write_all` call
    /// observes `cancelled` already set before it ever reaches the OS.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        unsafe {
            libc::shutdown(self.stream.as_raw_fd(), libc::SHUT_RDWR);
        }
    }

    /// ADR 0045 decision 2 (the daemon-side lane bridge): hand the raw
    /// socket off to a caller that will pipe raw bytes on its own
    /// runtime — for a caller that will pipe raw bytes on its own
    /// runtime; the client's cancel contract ends here. Moves `stream`
    /// out; the slots/flags (`cancelled`, `read_slot`, `write_slot`, the
    /// test-only entered flags, `connect_anchor_boot_ticks`) simply drop
    /// — nothing about them survives past this call, and nothing needs
    /// to.
    pub fn into_stream(self) -> UnixStream {
        self.stream
    }

    /// TEST-SUPPORT ONLY (review round 2, replacing an earlier
    /// `try_lock`-based version): `true` once a `read` has genuinely
    /// ENTERED its critical section at any point in this client's life —
    /// a PASSIVE read of `read_slot_entered`, never itself contending for
    /// `read_slot`. The earlier version called `read_slot.try_lock()`
    /// directly, which itself momentarily takes the slot and could make
    /// a real, concurrently in-flight `read` see a spurious
    /// `ConcurrentSubmit` it never actually raced. Lets a test WAIT on an
    /// OBSERVED precondition ("thread A's read has genuinely reached the
    /// blocking call") instead of a fixed sleep guessing at how long
    /// that takes.
    #[cfg(any(test, feature = "test-support"))]
    pub fn read_slot_entered_for_test(&self) -> bool {
        self.read_slot_entered.load(Ordering::SeqCst)
    }

    /// The write twin of
    /// [`read_slot_entered_for_test`](Self::read_slot_entered_for_test).
    #[cfg(any(test, feature = "test-support"))]
    pub fn write_slot_entered_for_test(&self) -> bool {
        self.write_slot_entered.load(Ordering::SeqCst)
    }

    /// TEST-SUPPORT ONLY: build a client around an already-connected
    /// stream (a raw `UnixStream::connect`, or one accepted server-side
    /// and handed to a paired test) with a controllable
    /// `connect_anchor_boot_ticks`, so the pin-validation tests can exercise
    /// both sides of its strict inequality without racing a real clock.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_stream_for_test(stream: UnixStream, connect_anchor_boot_ticks: u64) -> SocketClient {
        SocketClient {
            stream,
            cancelled: AtomicBool::new(false),
            read_slot: Mutex::new(()),
            write_slot: Mutex::new(()),
            #[cfg(any(test, feature = "test-support"))]
            read_slot_entered: AtomicBool::new(false),
            #[cfg(any(test, feature = "test-support"))]
            write_slot_entered: AtomicBool::new(false),
            connect_anchor_boot_ticks,
        }
    }
}

/// L1-unix LU3a (ADR 0043 decision 19): the seam trait every concrete
/// client implements — `write_all`/`read`/`cancel` already have this
/// exact signature (modulo the error type, unified by decision 17), so
/// this is pure delegation. The blanket `impl<C: Client>
/// ChallengeableConnection for C` in `crate::lane::client` is what makes
/// `SocketClient` challengeable now — the hand-written façade this impl
/// used to be (`socket_error_to_io`, its own `TransportError -> io::Error`
/// mapping) is gone; `crate::lane::client`'s ONE mapping replaces it.
impl Client for SocketClient {
    fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError> {
        SocketClient::write_all(self, bytes)
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        SocketClient::read(self, buf)
    }

    fn cancel(&self) {
        SocketClient::cancel(self)
    }
}

/// L1-unix LU1c: the Linux-shaped extension every `SocketChallengeable`
/// this crate actually challenges must also supply — see
/// `challenge_unix.rs`'s own doc for why this is the Linux twin of
/// `challenge_win::PipeChallengeable`.
#[cfg(target_os = "linux")]
impl crate::identity::challenge_unix::SocketChallengeable for SocketClient {
    fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    fn connect_anchor_boot_ticks(&self) -> u64 {
        self.connect_anchor_boot_ticks
    }
}

/// M2: the macOS-shaped extension -- ONE method, because the audit
/// token `challenge_macos` reads carries the peer's pid and the
/// kernel's own reuse generation together and needs no pre-connect
/// anchor at all (see that module's own doc, "What macOS does not
/// need"). The `connect_anchor_boot_ticks` field this `SocketClient`
/// still carries is therefore never read on this target.
#[cfg(target_os = "macos")]
impl crate::identity::challenge_macos::SocketChallengeable for SocketClient {
    fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}

/// L1-unix LU3a (ADR 0043 decision 19): the Unix-socket `Endpoint` — a
/// unit struct (the concrete pipe/socket family is the type itself, not a
/// value any instance carries) delegating straight to the free functions
/// this module already exposes. ONE implementation for both targets that
/// have a trusted peer-identity mechanism: everything that differs
/// between Linux and macOS is already behind the `challenge_os` alias
/// above, so there is no second `Endpoint` to keep in step with this one.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Copy, Default)]
pub struct SocketEndpoint;

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Endpoint for SocketEndpoint {
    type Client = SocketClient;
    type Process = challenge_os::ChallengedProcess;

    fn connect_voyage_unchallenged(
        &self,
        _lane: &str,
        voyage_id: &str,
    ) -> Result<Self::Client, TransportError> {
        // The Linux voyage socket is named by voyage id alone; `_lane` is
        // the daemon-lane endpoint's namespace, unused here.
        connect_voyage_socket_unchallenged(voyage_id)
    }

    fn connect_supervisor_unchallenged(&self, lane: &str) -> Result<Self::Client, TransportError> {
        connect_supervisor_socket_unchallenged(lane)
    }

    fn challenge(
        &self,
        conn: &Self::Client,
        exchange: &mut dyn crate::identity::exchange::IdentityExchange,
        reply_deadline: Instant,
    ) -> crate::identity::challenge::ChallengeOutcome<Self::Process> {
        challenge_os::challenge(conn, exchange, reply_deadline)
    }

    fn authenticate_server(&self, conn: &Self::Client) -> crate::identity::challenge::PeerAuthOutcome {
        challenge_os::authenticate_server(conn)
    }
}

/// Shared raw connect, given an already-validated path: the bounded,
/// non-blocking retry loop (ADR 0043 decision 4, property 18; decision 27
/// for what is retried) — retries `EAGAIN` (a full listen backlog) and
/// `EINTR` (the attempt itself was interrupted) until
/// `Instant::now() + CONNECT_BOUND` (one attempt may overrun the bound by
/// a single 20 ms sleep, exactly like `pipe_win`'s own loop).
/// `ECONNREFUSED`/`ENOENT` — no listener at all, whether the socket path
/// does not exist yet or nothing is `accept`ing on it — return on the
/// FIRST attempt: an absent or refused endpoint is the caller's to poll at its
/// own interval, never this loop's to retry (an unavailable connect used
/// to cost the full `CONNECT_BOUND` even when no supervisor process
/// existed yet). Any other error is immediate and fatal. NO
/// authentication of any kind — every caller is responsible for running
/// the OS-level identity check on top, exactly like
/// `pipe_win::connect_named_pipe_unchallenged`.
///
/// The anchor `SocketClient::connect_anchor_boot_ticks` carries is sampled
/// HERE, immediately BEFORE each `one_connect_attempt` call — never after
/// one succeeds (review round fix, Codex finding: a post-connect sample
/// left a pid-reuse window open between `connect(2)` returning and the
/// sample actually running, during which a recycled pid could satisfy the
/// pin's strict `<` even though it raced this very attempt). Sampling
/// fresh on every loop iteration — not once before the loop — means a
/// RETRY re-anchors too: a legitimate peer that happens to start in the
/// narrow anchor-to-connect gap of one attempt is `Undetermined` for
/// that attempt, and proven by a later one (or by the caller's own
/// outer retry, once this whole call returns `Undetermined` up through
/// `authenticate_server`/`challenge`).
///
/// ADR 0045 decision 3: the lane bridge dials a socket PATH handed to it
/// on the wire (`LaneDial::Local`), never a name this crate derives from a
/// voyage id or a state-dir hash itself. Crate-private: other crates
/// (`sot-protocol`'s `DaemonLaneEndpoint`, `sotd stdio-bridge`, `sotd
/// topology`'s dial, the window) reach a socket by name only through
/// `identity::connect_own::connect_own`, which checks the listener's account
/// after this connect, before client I/O (ADR 0049, User isolation). The
/// retry budget is fixed at `CONNECT_BOUND`; an attempt or wait in progress
/// finishes first, including the 20 ms retry sleep, so it is not an exact
/// elapsed-time limit. Reused
/// rather than reimplemented: this is the SAME bounded,
/// non-blocking, pid-anchored connector every other Unix caller gets —
/// a caller that rolled its own blocking `UnixStream::connect` wrapped
/// in an external deadline would leak the blocked connect thread past
/// that deadline on a full listen backlog, which this loop's own
/// non-blocking `connect(2)` + bounded poll never does.
pub(crate) fn connect_unix_socket_unchallenged(path: &Path) -> Result<SocketClient, TransportError> {
    let addr_bytes = path.as_os_str().as_bytes();
    let deadline = Instant::now() + CONNECT_BOUND;
    loop {
        let connect_anchor_boot_ticks = capture_connect_anchor_boot_ticks();
        match one_connect_attempt(addr_bytes, deadline) {
            Ok(stream) => {
                return Ok(SocketClient {
                    stream,
                    cancelled: AtomicBool::new(false),
                    read_slot: Mutex::new(()),
                    write_slot: Mutex::new(()),
                    #[cfg(any(test, feature = "test-support"))]
                    read_slot_entered: AtomicBool::new(false),
                    #[cfg(any(test, feature = "test-support"))]
                    write_slot_entered: AtomicBool::new(false),
                    connect_anchor_boot_ticks,
                });
            }
            Err(ConnectAttempt::Fatal(e)) => {
                return Err(TransportError::Io {
                    op: "connect",
                    source: e,
                });
            }
            Err(ConnectAttempt::Retryable(e)) => {
                if Instant::now() >= deadline {
                    return Err(TransportError::Io {
                        op: "connect(bounded retry)",
                        source: e,
                    });
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// The raw connect to the voyage socket, with NO authentication — every
/// step-5 client must go through [`connect_voyage_socket`] instead.
/// `pub(crate)`, and MUST STAY `pub(crate)` — mirrors
/// `pipe_win::connect_voyage_pipe_unchallenged`'s own "never widen" doc:
/// an unchallenged `SocketClient` reachable through a PUBLIC path would
/// defeat this whole module's enforcement.
pub(crate) fn connect_voyage_socket_unchallenged(voyage_id: &str) -> Result<SocketClient, TransportError> {
    let path = voyage_socket_path(voyage_id)?;
    #[allow(clippy::disallowed_methods, reason = "a voyage lane connector: the caller runs the lane's identity challenge")]
    let client = connect_unix_socket_unchallenged(&path)?;
    Ok(client)
}

/// The supervisor lane's own raw connect, with NO authentication — see
/// `pipe_win::connect_supervisor_pipe_unchallenged`'s own doc for why
/// this intentionally has no `_unchallenged`-free sibling: the supervisor
/// lane needs the full five-step `challenge_os::challenge`,
/// which the caller composes itself on top of this. L1-unix LU3b: now
/// called on Linux AND macOS, via `Endpoint for SocketEndpoint`'s own
/// `connect_supervisor_unchallenged` (`attach_client/client.rs` and
/// `supervisor_client`, both generic over `Endpoint`, are its callers).
pub(crate) fn connect_supervisor_socket_unchallenged(h: &str) -> Result<SocketClient, TransportError> {
    let path = supervisor_socket_path(h)?;
    #[allow(clippy::disallowed_methods, reason = "a supervisor lane connector: the caller runs the lane's identity challenge")]
    let client = connect_unix_socket_unchallenged(&path)?;
    Ok(client)
}

/// Connect to `<runtime_dir>/voyage-<voyage_id>.sock` AND authenticate
/// the server behind it (ADR 0043 decision 8, steps 1-3) before handing
/// the connection back — the shared, step-5-client-facing constructor
/// every ordinary caller uses, mirroring `pipe_win::connect_voyage_pipe`'s
/// own doc almost verbatim: a raw successful `connect(2)` proves nothing
/// about who is listening, so this runs
/// the platform's own `authenticate_server` (the account the kernel
/// recorded for the connection only — NOT the full five-step `challenge()`,
/// which also matches an honest reply's own pid/creation and needs a
/// lane-specific request this layer must not consume) before returning
/// `Ok(_)`. A failed authentication is
/// a loud, typed [`TransportError::Foreign`] or
/// [`TransportError::Undetermined`] — never a silent retry.
///
/// ONE body for both targets: which module answers is the `challenge_os`
/// alias's business, not this function's. The two differ only in
/// mechanism — Linux reads `SO_PEERCRED` plus a pidfd pin, macOS `getpeereid`
/// for the account plus one `LOCAL_PEERTOKEN` audit token carrying pid
/// and reuse generation together (M2, MAC-ID) — and that difference is
/// already stated once, where the alias is declared.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn connect_voyage_socket(voyage_id: &str) -> Result<SocketClient, TransportError> {
    let client = connect_voyage_socket_unchallenged(voyage_id)?;
    map_peer_auth_outcome(challenge_os::authenticate_server(&client))?;
    Ok(client)
}
