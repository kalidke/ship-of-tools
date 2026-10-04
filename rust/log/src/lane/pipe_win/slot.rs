//! The overlapped I/O slot: one state machine for every direction and role, and the bounded completion wait.

use super::*;

/// One I/O slot's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SlotState {
    Idle,
    Pending,
    /// Terminal: latched by [`IoSlot::cancel`], never leaves this state.
    Closing,
}

/// A synchronized, reusable overlapped-I/O slot: one `Mutex<SlotState>`
/// plus one address-stable, manual-reset-event-backed `OVERLAPPED`. Used
/// for the accept loop's `ConnectNamedPipe`, every connection's read and
/// write directions (server and client alike). See the module doc's "I/O
/// slot" section for the full soundness argument.
pub(super) struct IoSlot {
    state: Mutex<SlotState>,
    ov: UnsafeCell<OVERLAPPED>,
    /// `true` iff the CURRENT (or most recently settled) submission on
    /// this slot genuinely went `ERROR_IO_PENDING` at the OS level —
    /// DISTINCT from `SlotState::Pending`, which is ALSO set for a
    /// synchronously-completed op still awaiting `GetOverlappedResult`
    /// collection (Codex round-5 finding: the two are not the same
    /// observable state — a test polling `SlotState::Pending` alone can
    /// pass during that synchronous-completion window without ever
    /// proving a genuine kernel-level pending op existed). Reset to
    /// `false` at the START of every submission (before its outcome is
    /// known) and set `true` only in the actual `ERROR_IO_PENDING`
    /// branch, so it always reflects the CURRENT attempt, never a stale
    /// one.
    genuinely_async: AtomicBool,
}
// SAFETY: `ov`'s contents are only ever touched (reset, issued, or read
// via `GetOverlappedResult`) by the ONE thread that got past
// `submit_and_wait`'s `Pending` check for this slot — a second thread
// racing that check is REJECTED before touching `ov` at all. A cancelling
// thread only ever reads the slot's STABLE ADDRESS to hand to
// `CancelIoEx`, an OS-level operation on that address, never a Rust-level
// read of the struct's bytes. `state`'s `Mutex` is what serializes
// submission against both cancellation and a second submission attempt.
unsafe impl Send for IoSlot {}
unsafe impl Sync for IoSlot {}

pub(super) fn aborted_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(ERROR_OPERATION_ABORTED as i32)
}

/// Marker error: `submit_and_wait` rejected a call because another
/// submission is already `Pending` on this exact direction. Server-
/// internal code never triggers this — each slot has exactly one driving
/// thread by construction — it exists so `PipeClient`'s genuinely `Sync`,
/// `&self`-based `read`/`write_all` reject concurrent same-direction
/// misuse with a distinct `Result` instead of corrupting one shared
/// `OVERLAPPED`.
#[derive(Debug)]
pub(super) struct ConcurrentSubmitMarker;
impl std::fmt::Display for ConcurrentSubmitMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another submission is already pending on this IoSlot direction")
    }
}
impl std::error::Error for ConcurrentSubmitMarker {}

pub(super) fn is_concurrent_submit(e: &std::io::Error) -> bool {
    e.get_ref()
        .is_some_and(|inner| inner.is::<ConcurrentSubmitMarker>())
}

/// Marker error (ADR 0041 step 6 U1b, Codex round-4 "pending-I/O
/// completion proof"): a GENUINELY PENDING overlapped op's completion
/// could not be affirmatively observed within
/// [`OVERLAPPED_COMPLETION_PROOF_TIMEOUT`]. `CancelIoEx` only REQUESTS
/// cancellation; Microsoft's synchronous/asynchronous I/O rules require
/// the `OVERLAPPED`, its event, and any I/O buffer to remain valid until
/// the kernel is DONE with them — an error return alone is not that
/// proof. This module cannot safely return normally in that case:
///
/// - Every SERVER-side caller ([`accept_loop`], [`reader_loop`],
///   [`writer_loop`]) owns the buffer/slot it handed to the OS outright
///   and MUST permanently leak it — `std::mem::forget` an extra
///   `Arc<IoSlot>` clone (so the underlying allocation, including the
///   `OVERLAPPED` and its event, is never freed) and `std::mem::forget`
///   the I/O buffer — then treat the connection (or the accept loop
///   itself) as unrecoverably gone, reported loudly (`eprintln!`), never
///   silently. This matches `join_within`'s own abandonment philosophy:
///   never silently unsafe, but also never a process-wide abort for a
///   condition scoped to one connection.
/// - [`PipeClient::write_all`]/[`PipeClient::read`] receive a BORROWED
///   buffer from their own caller — this module cannot leak memory it
///   does not own, and the caller may free or reuse that memory the
///   instant this call returns. The only safe response left is to abort
///   the whole process (`std::process::abort()`) rather than risk the
///   kernel writing into (or reading stale bytes from) memory that has
///   already been freed or reused.
#[derive(Debug)]
pub(super) struct CompletionUnproven;
impl std::fmt::Display for CompletionUnproven {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "a genuinely pending overlapped op's completion could not be affirmatively \
             observed; its storage must be leaked, never reused",
        )
    }
}
impl std::error::Error for CompletionUnproven {}

pub(super) fn is_completion_unproven(e: &std::io::Error) -> bool {
    e.get_ref()
        .is_some_and(|inner| inner.is::<CompletionUnproven>())
}

/// The Win32 error codes that mean "the pipe is disconnected, broken, or
/// being closed" — the family both a live connection's read/write AND an
/// in-flight `ConnectNamedPipe` can report when a peer vanishes.
/// [`classify_terminal_error`] treats these (plus this module's own
/// cancellation code) as an ordinary, expected `Eof` for a LIVE
/// connection's reader/writer; the accept loop's own connect-result match
/// treats them as "a client connected and vanished before the completion
/// was fully processed" and registers that connection anyway rather than
/// silently discarding it — the SAME family, one call earlier.
/// `ERROR_BROKEN_PIPE` and `ERROR_PIPE_NOT_CONNECTED` are Microsoft's
/// documented disconnect-family codes for named-pipe I/O; `ERROR_NO_DATA`
/// ("The pipe is being closed") is the code Windows documents pipe I/O
/// (including a `ConnectNamedPipe` racing a local close) returning when
/// the local end is torn down mid-operation — exactly the instant-close
/// race this module must not misclassify as a fatal accept failure. Any
/// OTHER connect error is a genuine anomaly and is NOT in this list —
/// see the accept loop's own match for why that distinction matters.
pub(super) fn is_disconnect_family(code: i32) -> bool {
    code == ERROR_NO_DATA as i32
        || code == ERROR_BROKEN_PIPE as i32
        || code == ERROR_PIPE_NOT_CONNECTED as i32
}

impl IoSlot {
    pub(super) fn new() -> std::io::Result<Self> {
        // Manual-reset (bManualReset = TRUE): auto-reset events can hang
        // `GetOverlappedResult(..., TRUE)` per Microsoft's overlapped-I/O
        // documentation; every reuse below explicitly `ResetEvent`s.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = event;
        Ok(Self {
            state: Mutex::new(SlotState::Idle),
            ov: UnsafeCell::new(ov),
            genuinely_async: AtomicBool::new(false),
        })
    }

    fn ptr(&self) -> *mut OVERLAPPED {
        self.ov.get()
    }

    /// Zero every field except `hEvent`, then explicitly `ResetEvent` it —
    /// zeroing our copy of the struct does not touch the EVENT OBJECT's
    /// own kernel-side signaled state.
    fn reset(&self) {
        unsafe {
            let event = (*self.ov.get()).hEvent;
            ResetEvent(event);
            *self.ov.get() = std::mem::zeroed();
            (*self.ov.get()).hEvent = event;
        }
    }

    /// Attempt to submit one overlapped op (`issue`, returning the raw
    /// `BOOL` of `ReadFile`/`WriteFile`) against a handle THIS CALLER
    /// already owns outright, and block for its definitive result.
    /// CLIENT-facing only — see the module doc's "I/O slot" section for
    /// why server-side instances go through
    /// [`submit_and_wait_registered`](Self::submit_and_wait_registered)
    /// instead. Refuses WITHOUT ever calling `issue` if this slot is
    /// `Closing` (`Err(aborted)`) or already `Pending`
    /// (`Err(ConcurrentSubmitMarker)`) — both checked under the same lock
    /// `cancel` uses, so neither race is possible. `synchronous_ok` names
    /// a synchronous-failure `GetLastError` code that actually means
    /// success; pass `|_| false` for plain reads/writes.
    pub(super) fn submit_and_wait(
        &self,
        handle: HANDLE,
        issue: impl FnOnce(*mut OVERLAPPED) -> i32,
        synchronous_ok: impl Fn(i32) -> bool,
    ) -> std::io::Result<u32> {
        let mut genuinely_pending = false;
        {
            let mut st = self.state.lock().unwrap();
            if *st == SlotState::Closing {
                return Err(aborted_error());
            }
            if *st == SlotState::Pending {
                return Err(std::io::Error::other(ConcurrentSubmitMarker));
            }
            self.genuinely_async.store(false, Ordering::Release);
            // Reset AND issue while holding the lock: a concurrent
            // `cancel` cannot observe a half-reset `OVERLAPPED`, and
            // cannot call `CancelIoEx` in the gap between this reset and
            // the `issue` call below.
            self.reset();
            let ok = issue(self.ptr());
            let sync_err = (ok == 0).then(std::io::Error::last_os_error);
            if ok == 0 {
                let err = sync_err.unwrap();
                let code = err.raw_os_error().unwrap_or(0);
                if code == ERROR_IO_PENDING as i32 {
                    *st = SlotState::Pending;
                    genuinely_pending = true;
                    self.genuinely_async.store(true, Ordering::Release);
                } else if synchronous_ok(code) {
                    return Ok(0);
                } else {
                    return Err(err);
                }
            } else {
                *st = SlotState::Pending;
            }
        } // lock released BEFORE the (possibly long) wait below.
        let result = wait_overlapped(handle, self.ptr(), genuinely_pending);
        if let Err(e) = &result {
            if is_completion_unproven(e) {
                // Never touch this slot's state again -- see
                // `CompletionUnproven`'s own doc. The caller MUST leak
                // whatever storage it owns.
                return result;
            }
        }
        self.genuinely_async.store(false, Ordering::Release);
        let mut st = self.state.lock().unwrap();
        if *st != SlotState::Closing {
            *st = SlotState::Idle;
        }
        result
    }

    /// Same contract as [`submit_and_wait`](Self::submit_and_wait), for a
    /// REGISTERED server-side instance (ADR 0041 step 6 U1b, Codex
    /// round-4): `issue` receives the raw handle only once a
    /// [`LiveHandle`] proves `id` is still registered in `registry` —
    /// held for exactly the duration of the `issue` call itself, never
    /// across the subsequent blocking wait (see [`LiveHandle`]'s own
    /// doc). If `id` is already gone (closed by
    /// [`InstanceRegistry::close_all`]), this behaves exactly like this
    /// module's own cancellation: `Err(aborted_error())`, without ever
    /// calling `issue` — a torn-down instance is, from every caller's
    /// perspective, indistinguishable from one THIS module cancelled.
    pub(super) fn submit_and_wait_registered(
        &self,
        registry: &InstanceRegistry,
        id: u64,
        issue: impl FnOnce(HANDLE, *mut OVERLAPPED) -> i32,
        synchronous_ok: impl Fn(i32) -> bool,
    ) -> std::io::Result<u32> {
        let mut genuinely_pending = false;
        let handle = {
            let mut st = self.state.lock().unwrap();
            if *st == SlotState::Closing {
                return Err(aborted_error());
            }
            if *st == SlotState::Pending {
                return Err(std::io::Error::other(ConcurrentSubmitMarker));
            }
            let Some(live) = registry.live(id) else {
                return Err(aborted_error());
            };
            let handle = live.get();
            self.genuinely_async.store(false, Ordering::Release);
            self.reset();
            let ok = issue(handle, self.ptr());
            // Codex round-5 finding 1: capture `GetLastError` IMMEDIATELY
            // after `issue` returns, BEFORE `drop(live)` -- dropping the
            // `LiveHandle` releases the registry's `RwLock` read side,
            // and this crate never assumes an intervening call (however
            // unlikely to actually touch it) leaves the thread's last-
            // error value alone. A real `ERROR_IO_PENDING` misread as an
            // ordinary failure here would free an `OVERLAPPED`/buffer
            // the kernel still owns.
            let sync_err = (ok == 0).then(std::io::Error::last_os_error);
            drop(live);
            if ok == 0 {
                let err = sync_err.unwrap();
                let code = err.raw_os_error().unwrap_or(0);
                if code == ERROR_IO_PENDING as i32 {
                    *st = SlotState::Pending;
                    genuinely_pending = true;
                    self.genuinely_async.store(true, Ordering::Release);
                } else if synchronous_ok(code) {
                    return Ok(0);
                } else {
                    return Err(err);
                }
            } else {
                *st = SlotState::Pending;
            }
            handle
        };
        let result = wait_overlapped(handle, self.ptr(), genuinely_pending);
        if let Err(e) = &result {
            if is_completion_unproven(e) {
                // Never touch this slot's state again -- see
                // `CompletionUnproven`'s own doc. The caller MUST leak
                // whatever storage it owns.
                return result;
            }
        }
        self.genuinely_async.store(false, Ordering::Release);
        let mut st = self.state.lock().unwrap();
        if *st != SlotState::Closing {
            *st = SlotState::Idle;
        }
        result
    }

    /// Cancel this slot: if an operation is genuinely `Pending`, call
    /// `CancelIoEx`; either way, latch `Closing` so every FUTURE
    /// `submit_and_wait` call refuses before ever touching the OS again.
    /// Idempotent — safe to call more than once, from any thread.
    /// CLIENT-facing only — see
    /// [`cancel_registered`](Self::cancel_registered) for server-side
    /// instances.
    pub(super) fn cancel(&self, handle: HANDLE) {
        let mut st = self.state.lock().unwrap();
        if *st == SlotState::Pending {
            unsafe { CancelIoEx(handle, self.ptr()) };
        }
        *st = SlotState::Closing;
    }

    /// Same contract as [`cancel`](Self::cancel), for a REGISTERED
    /// server-side instance: `CancelIoEx` is issued only while a
    /// [`LiveHandle`] proves `id` is still registered. If `id` is
    /// already gone, there is nothing to cancel against — its
    /// `CloseHandle` already forced the pending op to complete/error —
    /// so this just latches `Closing`, exactly like the plain `cancel`
    /// always does.
    ///
    /// Returns whether the op THIS CALL cancelled (if any) was
    /// GENUINELY asynchronously pending, decided under the SAME lock
    /// acquisition that performs the cancellation (Codex round-5 fix
    /// 2b/2c) — this is the TOCTOU-free proof a caller needing to KNOW
    /// (not merely poll-and-hope) must use: a separate prior check of
    /// [`is_genuinely_pending`](Self::is_genuinely_pending) can always
    /// go stale between the check and this call; this return value
    /// cannot, because both the read and the cancellation happen inside
    /// one critical section.
    pub(super) fn cancel_registered(&self, registry: &InstanceRegistry, id: u64) -> bool {
        let mut st = self.state.lock().unwrap();
        let was_genuinely_pending =
            *st == SlotState::Pending && self.genuinely_async.load(Ordering::Acquire);
        if *st == SlotState::Pending {
            if let Some(live) = registry.live(id) {
                unsafe { CancelIoEx(live.get(), self.ptr()) };
            }
        }
        *st = SlotState::Closing;
        was_genuinely_pending
    }

    pub(super) fn is_closing(&self) -> bool {
        *self.state.lock().unwrap() == SlotState::Closing
    }

    /// `true` iff the CURRENT submission genuinely went `ERROR_IO_PENDING`
    /// at the OS level right now — see [`IoSlot::genuinely_async`]'s own
    /// doc for why this is NOT the same thing as `SlotState::Pending`.
    /// A caller that needs a race-free ANSWER (not merely a heuristic
    /// "is it probably time to act") must use
    /// [`cancel_registered`](Self::cancel_registered)'s own return value
    /// instead, which decides this under the same lock that performs
    /// the cancellation. This accessor is a best-effort PRE-check only
    /// — e.g. "has the writer plausibly reached a pending write yet, so
    /// it is worth proceeding to teardown" — never itself the proof.
    pub(super) fn is_genuinely_pending(&self) -> bool {
        self.genuinely_async.load(Ordering::Acquire)
    }
}

impl Drop for IoSlot {
    fn drop(&mut self) {
        unsafe { CloseHandle((*self.ov.get()).hEvent) };
    }
}

/// Bound on the affirmative wait for a GENUINELY PENDING overlapped op's
/// OWN completion signal (Codex round-4, "pending-I/O completion
/// proof"): Microsoft documents `CancelIoEx` as REQUESTING cancellation,
/// never waiting for it, and `GetOverlappedResult`'s error return alone
/// is not itself proof the kernel is done with this `OVERLAPPED` — if
/// `handle` was closed by another thread (e.g. `InstanceRegistry::close_all`
/// racing this exact call) in the gap between `IoSlot::submit_and_wait`
/// releasing its lock and `wait_overlapped` ever calling
/// `GetOverlappedResult`, that call can fail IMMEDIATELY against the
/// now-invalid handle without ever having waited on anything. The
/// completion EVENT's own lifetime is independent of `handle` (this
/// `IoSlot` owns the event; see `IoSlot::new`/`Drop`), so on error
/// [`wait_overlapped`] additionally waits on the event directly — but
/// ONLY when the op was genuinely submitted asynchronously
/// (`ERROR_IO_PENDING`); a synchronously-completed op has nothing left
/// pending and may never signal the event at all (Microsoft's named-pipe
/// overlapped example notes exactly this), so waiting on it
/// unconditionally would manufacture a FALSE timeout. Bounded, never
/// Win32 `INFINITE` — this crate's own rule (see
/// `host::duration_to_wait_ms`'s doc); in the ordinary case (the error
/// came from a properly-waited cancellation) the event is ALREADY
/// signalled, so this returns effectively instantly.
pub(super) const OVERLAPPED_COMPLETION_PROOF_TIMEOUT: Duration = Duration::from_secs(5);

/// Block for the definitive result of an overlapped op already submitted
/// on `handle`/`ov`. `genuinely_pending` MUST be `true` iff `issue`
/// itself returned `ERROR_IO_PENDING` (an actual asynchronous
/// submission) rather than a synchronous result — see
/// [`OVERLAPPED_COMPLETION_PROOF_TIMEOUT`]'s own doc for why that
/// distinction is load-bearing. On a GENUINELY pending op whose
/// completion cannot be affirmatively observed within the bound, this
/// returns [`CompletionUnproven`] rather than an ordinary error — every
/// caller MUST react to that marker per its own doc, never treat it as a
/// normal I/O failure.
pub(super) fn wait_overlapped(
    handle: HANDLE,
    ov: *const OVERLAPPED,
    genuinely_pending: bool,
) -> std::io::Result<u32> {
    let mut transferred: u32 = 0;
    let ok = unsafe { GetOverlappedResult(handle, ov, &mut transferred, 1) };
    if ok == 0 {
        let err = std::io::Error::last_os_error();
        if !genuinely_pending {
            // This op completed SYNCHRONOUSLY (`issue` itself returned
            // success) -- there is nothing left for the kernel to still
            // be doing with this OVERLAPPED. A failure here means only
            // that `handle` is no longer valid for QUERYING the result
            // (e.g. an external close raced this exact call), never that
            // a pending kernel operation might still touch this memory.
            // Nothing to prove; return the plain error.
            return Err(err);
        }
        let event = unsafe { (*ov).hEvent };
        let ms = crate::host::duration_to_wait_ms(OVERLAPPED_COMPLETION_PROOF_TIMEOUT);
        return match unsafe { WaitForSingleObject(event, ms) } {
            WAIT_OBJECT_0 => Err(err),
            WAIT_TIMEOUT => {
                eprintln!(
                    "sot-pipe: a genuinely pending overlapped op's completion was not \
                     affirmatively observed within {OVERLAPPED_COMPLETION_PROOF_TIMEOUT:?}; its \
                     OVERLAPPED/event/buffer must be leaked, never reused (see \
                     CompletionUnproven's own doc)"
                );
                Err(std::io::Error::other(CompletionUnproven))
            }
            WAIT_FAILED => {
                eprintln!(
                    "sot-pipe: WaitForSingleObject on the overlapped completion event failed \
                     ({:?}) while establishing the completion proof; leaking rather than risking \
                     a use-after-free",
                    std::io::Error::last_os_error()
                );
                Err(std::io::Error::other(CompletionUnproven))
            }
            other => {
                eprintln!(
                    "sot-pipe: WaitForSingleObject on the overlapped completion event returned \
                     an unexpected result ({other:#x}) while establishing the completion proof; \
                     leaking rather than risking a use-after-free"
                );
                Err(std::io::Error::other(CompletionUnproven))
            }
        };
    }
    Ok(transferred)
}
