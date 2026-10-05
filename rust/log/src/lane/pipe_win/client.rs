//! The pipe client: `PipeClient`, the voyage and supervisor connects, and `PipeEndpoint`.

use super::*;
use crate::lane::client::map_peer_auth_outcome;

/// The client side of one voyage's pipe: `read`/`write_all` are blocking
/// from the calling thread's own perspective, but `PipeClient` is `Sync`
/// (via the same [`IoSlot`] the server uses, rejecting a concurrent
/// same-direction submission rather than corrupting one) — a second
/// thread may call [`PipeClient::cancel`] at any time to unblock whichever
/// of the two is currently in flight.
pub struct PipeClient {
    #[allow(dead_code)] // held for its Drop (closes the pipe handle)
    handle: OwnedHandle,
    raw: SendableHandle,
    read_slot: IoSlot,
    write_slot: IoSlot,
}

impl std::fmt::Debug for PipeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeClient").finish_non_exhaustive()
    }
}

/// Connect to `\\.\pipe\sot-voyage-<voyage_id>` AND authenticate the
/// server behind it (ADR 0041 Lifecycle "The challenge", steps 1-3 —
/// U1a) before handing the connection back — the shared,
/// step-5-client-facing constructor every ordinary caller (tests, the
/// e2e harness, and any future mgmt/attach client) uses. A pipe's DACL is
/// directional (governs who may CONNECT, not who MADE the object), so a
/// raw successful `CreateFileW` here proves nothing about who is on the
/// other end; this function runs
/// [`crate::identity::challenge_win::authenticate_server`] (identify the peer process,
/// compare its token-user SID to this account's — NOT the full five-step
/// `challenge()`, which additionally binds a reply's own pid/creation to
/// this connection and needs a lane-specific request to get one) before
/// returning `Ok(_)` — the MINIMAL SAFE CALL for a connection whose lane
/// is not yet known here (the caller's own first frame —
/// `status`/`probe`/`shutdown` for mgmt, `hello` for attach — decides
/// that, and this function must not consume either by sending a
/// lane-specific request of its own; see `authenticate_server`'s own doc
/// for why the full proof does not apply at this layer). A failed
/// authentication is a loud, typed [`TransportError::Foreign`] or
/// [`TransportError::Undetermined`] — never a silent retry. A caller that
/// needs the FULL proof (mgmt lane; the probe classifier) runs
/// `challenge_win::challenge` itself on top of this — see
/// `supervisor::probe::win::RealProbeOps` for exactly that composition.
///
/// Retries `CreateFileW` (bounded, 2s total) on `ERROR_PIPE_BUSY` ONLY
/// (all instances currently connected — waits on `WaitNamedPipeW`
/// between attempts): an ordinary race in a healthy multi-client server,
/// not a failure. `ERROR_FILE_NOT_FOUND` (no instance exists yet — the
/// server has not called `bind` yet) returns on the FIRST attempt (ADR
/// 0043 decision 27) — the caller's own readiness wait owns that race
/// now, not this bounded retry.
pub fn connect_voyage_pipe(voyage_id: &str) -> Result<PipeClient, TransportError> {
    let client = connect_voyage_pipe_unchallenged(voyage_id)?;
    map_peer_auth_outcome(crate::identity::challenge_win::authenticate_server(&client))?;
    Ok(client)
}

/// The raw connect, with NO authentication — every step-5 client must go
/// through [`connect_voyage_pipe`] instead. `pub(crate)`, and MUST STAY
/// `pub(crate)` (U1a Codex round-1, Blocker 2): the only in-crate consumer
/// is `supervisor::probe::win::RealProbeOps::connect`, which is itself `pub(crate)` for
/// exactly this reason — an unchallenged `PipeClient` reachable through a
/// PUBLIC type would be a public path to raw pipe I/O on an unauthenticated
/// connection, defeating this whole module's own enforcement. See
/// `supervisor/probe/win.rs`'s module doc for why making `RealProbeOps` crate-private
/// costs nothing today (no production code instantiates it yet) and stays
/// architecturally sound once U2's classifier lands (a public function
/// in THIS crate, reachable from `sot-capsule`'s separate bin target,
/// wrapping this crate-private plumbing).
///
/// This exists ONLY for the probe classifier's own `ProbeOps::connect` (a
/// later unit, ADR 0041 "The probe"), which deliberately keeps "connect"
/// and "challenge" as two separately-observed steps — Stage B's own
/// transition table (B1-B6) is defined in terms of a raw connect outcome
/// followed by a SEPARATELY timed challenge (a bespoke deadline clamped to
/// the probe episode's remaining wall time), so folding authentication
/// into the connect itself here would collapse rows the classifier needs
/// to tell apart.
pub(crate) fn connect_voyage_pipe_unchallenged(voyage_id: &str) -> Result<PipeClient, TransportError> {
    validate_voyage_id(voyage_id)?;
    connect_named_pipe_unchallenged(pipe_name_wide(voyage_id), &AtomicBool::new(false))
}

/// ADR 0041 step 6 U2: connect to the supervisor lane's own pipe with NO
/// authentication — every real caller must run the SAME five-step
/// [`crate::identity::challenge_win::challenge`] the mgmt lane's own client does (the
/// supervisor lane's security is "MUTUAL", not the weaker SID-only proof
/// [`connect_voyage_pipe`] settles for), so unlike that function this one
/// intentionally has no `_unchallenged`-free sibling here — the caller
/// composes the full challenge itself, exactly as `supervisor::probe::win::RealProbeOps`
/// does for the mgmt lane's own unchallenged connect.
pub(crate) fn connect_supervisor_pipe_unchallenged(h: &str) -> Result<PipeClient, TransportError> {
    connect_named_pipe_unchallenged(supervisor_pipe_name_wide(h), &AtomicBool::new(false))
}

/// Shared raw connect, given an already-resolved wide pipe name: retries
/// `CreateFileW` (bounded, [`CONNECT_BOUND`] total) on `ERROR_PIPE_BUSY`
/// ONLY — an instance is held continuously and recycled once bound (this
/// module's "Continuous name hold" doc above), so "unavailable" past that
/// point means busy, never absent. `ERROR_FILE_NOT_FOUND` — no instance
/// exists yet — returns [`TransportError::Io`] on the very first attempt:
/// an absent pipe is the caller's to poll (ADR 0043 decision 27), not a
/// transient this function retries. NO authentication of any kind —
/// every caller of either wrapper above is responsible for running the
/// OS-level identity check (and, where the lane needs it, the full
/// challenge) on top.
///
/// `cancel`: checked at the top of every loop iteration — i.e. between
/// every bounded (200 ms) `WaitNamedPipeW` wait, never mid-syscall. This
/// is the ONLY mid-dial cancellation Windows named pipes actually admit:
/// `CreateFileW`'s synchronous OPEN has no async/overlapped form to
/// cancel (unlike a read/write on an ALREADY-open handle, which
/// `PipeClient::cancel` can interrupt via `CancelIoEx`), and
/// `WaitNamedPipeW` has no cancellation handle either — there is no OS
/// object yet for another thread to act on. A bounded poll loop that
/// checks a flag between its own already-bounded waits is the only
/// mechanism that exists; existing callers (both wrappers above) pass a
/// fresh, never-set flag, so their own behavior is unchanged.
pub(super) fn connect_named_pipe_unchallenged(name: Vec<u16>, cancel: &AtomicBool) -> Result<PipeClient, TransportError> {
    let deadline = Instant::now() + CONNECT_BOUND;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(TransportError::Cancelled);
        }
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };
        if h != INVALID_HANDLE_VALUE {
            let handle = unsafe { OwnedHandle::from_raw_handle(h as RawHandle) };
            let raw = SendableHandle(h);
            let read_slot = IoSlot::new().map_err(|e| TransportError::Io {
                op: "CreateEventW(client read)",
                source: e,
            })?;
            let write_slot = IoSlot::new().map_err(|e| TransportError::Io {
                op: "CreateEventW(client write)",
                source: e,
            })?;
            return Ok(PipeClient {
                handle,
                raw,
                read_slot,
                write_slot,
            });
        }
        let err = std::io::Error::last_os_error();
        let code = err.raw_os_error();
        let retryable = code == Some(ERROR_PIPE_BUSY as i32);
        if !retryable || Instant::now() >= deadline {
            return Err(TransportError::Io {
                op: "CreateFileW",
                source: err,
            });
        }
        unsafe { WaitNamedPipeW(name.as_ptr(), 200) };
    }
}

/// ADR 0045 decision 3: the lane bridge dials a pipe PATH handed to it on the wire (`LaneDial::Local`
/// carries whatever path the daemon's `lane.connect` reply implies),
/// never a name this crate derives from a voyage id or a state-dir hash
/// itself — so this is the raw connect by an arbitrary caller-supplied
/// path, wide-encoded and handed to the SAME bounded-retry connect every
/// other named-pipe client gets, never a parallel implementation. Crate-
/// private like every sibling raw connect above: other crates
/// (`sot-protocol`'s `DaemonLaneEndpoint`, `sotd stdio-bridge`) reach a pipe
/// by name only through `identity::connect_own::connect_own`, which checks
/// who serves it (ADR 0049, User isolation). NO authentication, exactly like
/// [`connect_named_pipe_unchallenged`] itself — the lane bridge's own
/// identity proof is decision 3's split (the daemon ran steps 1-3 on ITS
/// dial; this client runs steps 4-5 over the pipe this returns). `cancel`
/// is threaded straight through to [`connect_named_pipe_unchallenged`]'s
/// own bounded poll loop — see that function's own doc for why a checked
/// flag between its already-bounded waits is the only mid-dial
/// cancellation a synchronous `CreateFileW`/`WaitNamedPipeW` pair admits.
pub(crate) fn connect_pipe_path_unchallenged(path: &str, cancel: &AtomicBool) -> Result<PipeClient, TransportError> {
    connect_named_pipe_unchallenged(wide_null(path), cancel)
}

impl PipeClient {
    /// Blocking write of the whole buffer, cancellable from another
    /// thread via [`PipeClient::cancel`]. `bytes` must be non-empty and no
    /// larger than a single Win32 write can represent. A concurrent
    /// SECOND `write_all` call from another thread returns
    /// `Err(TransportError::ConcurrentSubmit)` rather than corrupting the
    /// shared `OVERLAPPED`. Named pipes complete a `WriteFile` as one
    /// atomic operation (byte-mode, no partial writes to retry-loop over).
    ///
    /// `bytes` is BORROWED from the caller — if a genuinely pending
    /// write's completion cannot be affirmatively observed
    /// ([`CompletionUnproven`]), this module cannot safely leak it on
    /// the caller's behalf (the caller may free/reuse it the instant
    /// this call returns), so it aborts the process instead — see
    /// `CompletionUnproven`'s own doc.
    pub fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError> {
        if bytes.is_empty() {
            return Err(TransportError::EmptyPayload);
        }
        if bytes.len() > u32::MAX as usize {
            return Err(TransportError::PayloadTooLarge(bytes.len()));
        }
        let result = self.write_slot.submit_and_wait(
            self.raw.0,
            |ov| unsafe {
                WriteFile(
                    self.raw.0,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    std::ptr::null_mut(),
                    ov,
                )
            },
            |_| false,
        );
        if let Err(e) = &result {
            if is_completion_unproven(e) {
                eprintln!(
                    "sot-pipe: a pending client WriteFile's completion could not be affirmatively \
                     observed and its buffer is caller-owned and cannot be safely leaked; \
                     aborting the process rather than risk a use-after-free"
                );
                std::process::abort();
            }
        }
        result.map(|_| ()).map_err(map_client_io_error("WriteFile"))
    }

    /// Blocking read into `buf`, cancellable from another thread via
    /// [`PipeClient::cancel`]. `buf` must be non-empty and no larger than
    /// a single Win32 read can represent — an empty buffer would loop
    /// forever re-issuing zero-byte reads, and a buffer whose length
    /// silently narrows to zero at the `u32` Win32 boundary (exactly
    /// 4 GiB) would have the identical failure. A concurrent SECOND
    /// `read` call from another thread returns
    /// `Err(TransportError::ConcurrentSubmit)`. `Ok(0)` means the server closed
    /// its end (ordered EOF) — NEVER a successful zero-byte completion,
    /// which this method silently retries past (this transport's own
    /// `send`/`write_all` never produce one).
    ///
    /// `buf` is BORROWED from the caller — see `write_all`'s own doc for
    /// why a [`CompletionUnproven`] result here aborts the process
    /// instead of returning.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        if buf.is_empty() {
            return Err(TransportError::EmptyPayload);
        }
        if buf.len() > u32::MAX as usize {
            return Err(TransportError::PayloadTooLarge(buf.len()));
        }
        loop {
            let result = self.read_slot.submit_and_wait(
                self.raw.0,
                |ov| unsafe {
                    ReadFile(
                        self.raw.0,
                        buf.as_mut_ptr(),
                        buf.len() as u32,
                        std::ptr::null_mut(),
                        ov,
                    )
                },
                |_| false,
            );
            match result {
                Ok(0) => continue,
                Ok(n) => return Ok(n as usize),
                Err(e) if is_completion_unproven(&e) => {
                    eprintln!(
                        "sot-pipe: a pending client ReadFile's completion could not be \
                         affirmatively observed and its buffer is caller-owned and cannot be \
                         safely leaked; aborting the process rather than risk a use-after-free"
                    );
                    std::process::abort();
                }
                Err(e) if matches!(e.raw_os_error(), Some(c) if is_disconnect_family(c)) => {
                    return Ok(0)
                }
                Err(e) => return Err(map_client_io_error("ReadFile")(e)),
            }
        }
    }

    /// Cancel whatever is currently in flight on EITHER direction, from
    /// any thread — safe to call concurrently with `read`/`write_all` on
    /// another thread. A cancelled call returns `Err(TransportError::Cancelled)`,
    /// distinct from an ordered EOF, an ordinary I/O error, or
    /// `ConcurrentSubmit`.
    pub fn cancel(&self) {
        self.read_slot.cancel(self.raw.0);
        self.write_slot.cancel(self.raw.0);
    }

    /// ADR 0045 decision 2 (the daemon-side lane bridge): hand the raw
    /// pipe handle off to a caller that will pipe raw bytes on its own
    /// runtime — the client's cancel contract ends here. Moves `handle`
    /// out (opened `FILE_FLAG_OVERLAPPED` above, so it is valid for a
    /// Tokio named-pipe client to adopt); `raw`/the two `IoSlot`s simply
    /// drop — there is no manual `Drop` on this type, so nothing else
    /// needs releasing.
    pub fn into_handle(self) -> OwnedHandle {
        self.handle
    }
}

/// L1-unix LU3a (ADR 0043 decision 19): the seam trait every concrete
/// client implements — `write_all`/`read`/`cancel` already have this
/// exact signature (modulo the error type, unified by decision 17), so
/// this is pure delegation. The blanket `impl<C: Client>
/// ChallengeableConnection for C` in `crate::lane::client` is what makes
/// `PipeClient` challengeable now — the hand-written façade this impl
/// used to be (`pipe_error_to_io`, its own `TransportError -> io::Error`
/// mapping) is gone; `crate::lane::client`'s ONE mapping replaces it.
impl Client for PipeClient {
    fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError> {
        PipeClient::write_all(self, bytes)
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        PipeClient::read(self, buf)
    }

    fn cancel(&self) {
        PipeClient::cancel(self)
    }
}

/// L1-unix LU1a: the Windows-shaped extension half of the same-connection
/// challenge — see `challenge_win.rs`'s own doc for why this is a separate
/// trait from [`crate::identity::challenge::ChallengeableConnection`] above.
impl crate::identity::challenge_win::PipeChallengeable for PipeClient {
    fn raw_handle(&self) -> HANDLE {
        self.raw.0
    }
}

/// L1-unix LU3a (ADR 0043 decision 19): the Windows `Endpoint` — a unit
/// struct (the concrete pipe/socket family is the type itself, not a
/// value any instance carries) delegating straight to the free functions
/// this module already exposes.
#[derive(Clone, Copy, Default)]
pub struct PipeEndpoint;

impl Endpoint for PipeEndpoint {
    type Client = PipeClient;
    type Process = crate::identity::challenge_win::ChallengedProcess;

    fn connect_voyage_unchallenged(
        &self,
        _lane: &str,
        voyage_id: &str,
    ) -> Result<Self::Client, TransportError> {
        // The Windows voyage pipe is named by voyage id alone; `_lane` is
        // the daemon-lane endpoint's namespace, unused here.
        connect_voyage_pipe_unchallenged(voyage_id)
    }

    fn connect_supervisor_unchallenged(&self, lane: &str) -> Result<Self::Client, TransportError> {
        connect_supervisor_pipe_unchallenged(lane)
    }

    fn challenge(
        &self,
        conn: &Self::Client,
        exchange: &mut dyn crate::identity::exchange::IdentityExchange,
        reply_deadline: Instant,
    ) -> crate::identity::challenge::ChallengeOutcome<Self::Process> {
        crate::identity::challenge_win::challenge(conn, exchange, reply_deadline)
    }

    fn authenticate_server(&self, conn: &Self::Client) -> crate::identity::challenge::PeerAuthOutcome {
        crate::identity::challenge_win::authenticate_server(conn)
    }
}

/// Shared client-side error mapping: `ERROR_OPERATION_ABORTED` becomes
/// [`TransportError::Cancelled`]; the [`ConcurrentSubmitMarker`] becomes
/// [`TransportError::ConcurrentSubmit`]; everything else is an ordinary I/O
/// failure.
pub(super) fn map_client_io_error(op: &'static str) -> impl Fn(std::io::Error) -> TransportError {
    move |e| {
        if is_concurrent_submit(&e) {
            TransportError::ConcurrentSubmit
        } else if e.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32) {
            TransportError::Cancelled
        } else {
            TransportError::Io { op, source: e }
        }
    }
}
