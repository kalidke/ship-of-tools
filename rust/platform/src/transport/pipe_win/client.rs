//! The pipe client: `PipeClient` and the unchallenged connect `connect_own` builds on.

use super::*;

/// The client side of a row's pipe: `read`/`write_all` are blocking
/// from the calling thread's own perspective, but `PipeClient` is `Sync`
/// (via an [`IoSlot`] per direction, rejecting a concurrent
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

/// Shared raw connect, given an already-resolved wide pipe name: retries
/// `CreateFileW` (bounded, [`CONNECT_BOUND`] total) on `ERROR_PIPE_BUSY`
/// ONLY — a listener holds its instances continuously and recycles them once bound, so "unavailable" past that
/// point means busy, never absent. `ERROR_FILE_NOT_FOUND` — no instance
/// exists yet — returns [`TransportError::Io`] on the very first attempt:
/// an absent pipe is the caller's to poll (ADR 0043 decision 27), not a
/// transient this function retries. NO authentication of any kind —
/// every caller is responsible for running the
/// OS-level identity check (and, where the caller needs it, the full
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
/// mechanism that exists; a caller with nothing to cancel passes a
/// fresh, never-set flag.
pub(super) fn connect_named_pipe_unchallenged(name: Vec<u16>, cancel: &AtomicBool) -> Result<PipeClient, TransportError> {
    let deadline = Instant::now() + CONNECT_BOUND;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(TransportError::Cancelled);
        }
        #[allow(clippy::disallowed_methods, reason = "the one raw open of the unchallenged connector, at identification level")]
        let h = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                // Identification level (ADR 0049, User isolation): whatever serves the pipe can read who this is but
                // never act as this account, whichever process it turns out to be.
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
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

/// The raw connect by an arbitrary caller-supplied path, wide-encoded and handed to the same bounded-retry connect every
/// named-pipe client gets, never a parallel implementation. Crate-private like [`connect_named_pipe_unchallenged`]:
/// every other crate reaches a pipe by name only through [`connect_own`](super::connect_own::connect_own), which checks
/// who serves it (ADR 0049, User isolation). NO authentication, exactly like [`connect_named_pipe_unchallenged`]
/// itself. `cancel` is threaded straight through to its bounded poll loop, whose doc says why a checked flag between
/// its already-bounded waits is the only mid-dial cancellation a synchronous `CreateFileW`/`WaitNamedPipeW` pair
/// admits.
pub(crate) fn connect_pipe_path_unchallenged(path: &str, cancel: &AtomicBool) -> Result<PipeClient, TransportError> {
    #[allow(clippy::disallowed_methods, reason = "the connector connect_own wraps")]
    let client = connect_named_pipe_unchallenged(wide_null(path), cancel)?;
    Ok(client)
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

    /// Hands the raw pipe handle off to a caller that will pipe raw bytes on its own
    /// runtime (the daemon's lane bridge); the client's cancel contract ends here. Moves `handle`
    /// out (opened `FILE_FLAG_OVERLAPPED` above, so it is valid for a
    /// Tokio named-pipe client to adopt); `raw`/the two `IoSlot`s simply
    /// drop — there is no manual `Drop` on this type, so nothing else
    /// needs releasing.
    pub fn into_handle(self) -> OwnedHandle {
        self.handle
    }
}

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
