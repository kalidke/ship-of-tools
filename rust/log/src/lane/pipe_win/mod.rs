//! The ADR 0041 step-5 Windows named-pipe transport: a server and client
//! for `\\.\pipe\sot-voyage-<id>`. It moves bytes and reports completions;
//! it does not know about mgmt/attach lanes, `hello`, opcodes, or
//! checkpoints — `lane/wire/` owns every frame shape, and
//! [`wire::FrameSplitter`] is what a consumer of this module's `Bytes`
//! events feeds. Transport only: no dependency on the capsule or
//! `sot-capsule` bin, and none may be added here.
//!
//! # The I/O slot: one state machine, every direction, every role
//!
//! A `Mutex<SlotState>` (`Idle` / `Pending` / `Closing`) guards an
//! address-stable, MANUAL-RESET-event-backed `OVERLAPPED` — manual, not
//! auto, because Microsoft documents overlapped pipe I/O against
//! manual-reset events and warns that an auto-reset event can hang
//! `GetOverlappedResult(..., TRUE)` when a completion races the wait
//! call. [`IoSlot::submit_and_wait`] resets and issues the OS call and
//! flips the state to `Pending` ALL UNDER ONE LOCK ACQUISITION, then
//! releases the lock before the (possibly long) wait — so
//! [`IoSlot::cancel`], which also takes that lock, can only ever observe
//! `Idle` (latch `Closing`; the next submission refuses before touching
//! the OS) or `Pending` (call `CancelIoEx`, then latch `Closing`). A
//! cancel can never miss a submission that hasn't happened yet, nor land
//! after one has already started reusing the structure. The same check
//! also rejects a SECOND submission while one is already `Pending`,
//! distinctly from `Closing` — reachable only through `PipeClient`, whose
//! `read`/`write_all` take `&self`: without this, two concurrent
//! same-direction callers could both reset and reissue the one shared
//! `OVERLAPPED`, corrupting whichever completed second. Rejecting it
//! (`TransportError::ConcurrentSubmit` at the `PipeClient` boundary, never
//! touching the OS) is what makes `unsafe impl Sync for IoSlot` sound:
//! completion is always consumed by exactly the one thread that got past
//! this check.
//!
//! [`IoSlot::submit_and_wait`] is the CLIENT-facing primitive: it takes a
//! plain `HANDLE` the caller already owns outright. Every SERVER-side
//! instance handle instead goes through
//! [`IoSlot::submit_and_wait_registered`]/[`IoSlot::cancel_registered`]
//! (ADR 0041 step 6 U1b, Codex round-4), which additionally proves the
//! handle is still registered — via [`InstanceRegistry::live`] — for
//! exactly the moment it is handed to the OS, never merely inferring
//! liveness from some other, staler signal. See [`LiveHandle`]'s own doc
//! for the invariant this establishes and the "One registry, one closer"
//! section below for why it is necessary.
//!
//! # Reaping: one thread owns every join
//!
//! A single dedicated REAPER thread (started in [`PipeServer::bind`],
//! alongside the accept thread) is the only code in this module that ever
//! removes an entry from `conns` or joins a REGISTERED connection's
//! reader/writer — for any reason. [`PipeServer::close`], a reader's own
//! natural-EOF signal, and a writer's own `WriteFile`-error signal all
//! route through [`request_teardown`], which enqueues at most once per
//! connection (see "Bounded reaper inbox" below); [`teardown_if_present`]
//! is the one function that does the real work, called exclusively from
//! [`reaper_loop`], processing messages strictly one at a time. The one
//! correct exception: `handle_new_connection`'s own partial-spawn-failure
//! unwind directly `join`s an aborted reader that was NEVER registered
//! into `conns` — the reaper has nothing to reap because that connection
//! never had a lifecycle for it to know about.
//!
//! Registration is ordered so that a client which connects and
//! disconnects instantly can never let a reader reach the reaper before
//! the entry exists to be found: a connection's reader/writer threads
//! spawn already blocked on a [`StartGate`] and do not touch the pipe
//! until AFTER the `ConnHandle` is in the map AND `Accepted` has been
//! RELIABLY queued (see below).
//!
//! # Reliable lifecycle delivery
//!
//! `Accepted`, `Sent`, `Closed`, and `AcceptError` must be delivered, not
//! merely attempted — a dropped `Accepted` lets `Bytes` arrive for a
//! connection the consumer was never told exists; a dropped `Sent`
//! violates the ADR's physical-write barrier; a dropped `Closed` leaves a
//! stream gap the consumer's `FrameSplitter` can never detect.
//! [`send_lifecycle_event`] retries against a full `events()` channel
//! indefinitely, with exactly ONE escape: [`ServerShared::dropping`], set
//! only by [`PipeServer::drop`] — once true, nothing could ever call
//! `events()` again (its `Receiver` lives inside the `PipeServer` being
//! dropped), so continuing would be pure busywork. `dropping` is set at
//! the very START of `drop`, before anything else, INCLUDING before the
//! accept-thread join: any thread (the accept thread's own `AcceptError`/
//! `Accepted` publishes included) can be inside this retry loop when
//! `Drop` runs, and joining it first would deadlock `Drop` behind the
//! very escape hatch meant to unblock it. Memory-bounded by construction
//! (exactly one event is ever "in hand" being retried per caller, never
//! accumulated); time-bounded otherwise ONLY by the CONSUMER's own
//! contract — it must keep draining `events()`, which the future
//! capsule's one ordered loop does by construction. That contract is this
//! mechanism's other half; this module cannot enforce it, only document
//! it.
//!
//! ONLY `Bytes` may still be abandoned — the transport is the read-ahead
//! producer and must not let a stalled consumer grow memory without
//! bound. [`deliver_bytes`] retries for up to [`BYTES_ABANDON_AFTER`]
//! against a full channel (or until its own slot is independently
//! cancelled); abandoning always forces this ONE connection closed with a
//! GUARANTEED `Closed` (through the same reliable path), never a silent
//! stream gap with nothing to mark it.
//!
//! # Bounded reaper inbox
//!
//! `reaper_tx` is a bounded channel — `max_instances +`
//! [`REAPER_INBOX_SLACK`] — rather than unbounded. What makes that bound
//! actually hold: every connection carries its own
//! `torn_down_requested: Arc<AtomicBool>`, and [`request_teardown`]
//! enqueues a `ReaperMsg` only on the `compare_exchange` that WINS
//! flipping it — an explicit `close`, the reader's own EOF signal, and
//! the writer's own error signal can all race for the same connection,
//! but at most one of them ever reaches the channel. The inbox can
//! therefore never hold more than one live `Torn` message per
//! currently-open connection (≤ `max_instances`) plus `Drop`'s own single
//! `Shutdown`.
//!
//! # Continuous name hold
//!
//! An instance is never actually closed while the server lives AND
//! intends to keep accepting. Once `DisconnectNamedPipe`'d, a torn-down
//! instance is RECYCLED — pushed onto `AcceptState::recycled` — rather
//! than dropped and later re-created. If `DisconnectNamedPipe` itself
//! fails, the instance is in an unknown state and unsafe to hand back
//! for a future `ConnectNamedPipe` — but it is deliberately RETAINED
//! anyway (`AcceptState::retained_dead`), never closed, for the rest of
//! the server's life. This looks wasteful — that instance's capacity is
//! gone for good — but the alternative is worse: creating a replacement
//! here would need to exceed `max_instances` while the failed instance
//! is still open (at `max_instances == 1` this is not merely awkward, it
//! is impossible — `CreateNamedPipeW` fails with `ERROR_PIPE_BUSY` every
//! time, since the OS still counts the open, merely-broken handle
//! against the cap), and closing the failed instance to make room is
//! exactly the name-hold lapse this design exists to prevent. The
//! invariant this module promises is that the NAME stays held, not that
//! every instance stays usable — a held name and a dead handle both
//! satisfy it; a closed handle does not. `recycle_instance` remains the
//! ONLY way an instance is ever set aside short of teardown, and a
//! `DisconnectNamedPipe` failure there also terminalizes the accept loop
//! via [`terminalize_accept_loop`] — see that function's doc for why
//! stopping (rather than merely losing one slot's worth of capacity and
//! continuing) is the safer default.
//!
//! # One registry, one closer, one live-use guard (ADR 0041 step 6 U1b,
//! Codex rounds 3-4)
//!
//! `recycle_instance` never itself calls `CloseHandle` — not on
//! recycle, not on retain-dead, not once teardown is under way. EVERY
//! instance handle this module ever creates is created AND registered
//! ATOMICALLY, in [`InstanceRegistry::create_and_register`] (round-4:
//! creation and registration share ONE lock section with
//! [`InstanceRegistry::close_all`], so a handle can never come into
//! existence — or be recreated — in a window `close_all` has already
//! passed), and stays registered — through any number of recycle/reuse
//! cycles, through becoming a live connection, through sitting in
//! `retained_dead` — until `close_all` (called exactly once, from
//! [`PipeServer::disconnect_listener`]) finds and closes it. Because no
//! OTHER code path ever individually removes an id from the registry,
//! there is no "removed here, but the remover assumed someone else would
//! close it" gap: whichever of this module's several buckets (the accept
//! loop's own pending instance, `recycled`, `retained_dead`, or a live
//! `ConnHandle` in `conns`) an id's instance currently sits in, at the
//! instant `close_all` runs it is found and closed — independent of
//! `conns`' own, unrelated Rust-level bookkeeping timing.
//!
//! That closes the NAME-leak races (round 3). It does NOT, by itself,
//! make USING a handle safe: `close_all` can run at any instant, and
//! Windows can and does reuse a closed handle's numeric value for an
//! unrelated object soon after — so a stale raw `HANDLE` passed to
//! `CancelIoEx`/`DisconnectNamedPipe`/a fresh `ConnectNamedPipe`/
//! `ReadFile`/`WriteFile` SUBMISSION is a genuine use-after-close, not
//! merely a harmless failed call (round 4). The fix: [`InstanceRegistry`]
//! is a `RwLock`, and [`InstanceRegistry::live`] hands back a
//! [`LiveHandle`] that holds the READ side for exactly the span of ONE
//! such call — `close_all` needs the WRITE side, which cannot be granted
//! while any `LiveHandle` (for any id) is outstanding, so a handle is
//! NEVER closed while it is mid-use, and never used once closed. This is
//! deliberately narrow: a `LiveHandle` is NEVER held across
//! [`wait_overlapped`]'s own blocking wait (only reads/writes that are
//! themselves fast, non-blocking Win32 calls run under it), so
//! `disconnect_listener`'s "never blocks" contract survives — see
//! `LiveHandle`'s own doc.
//!
//! Every remaining `ServerShared::dropping` check in
//! `recycle_instance`/`accept_loop` is now a pure CLEANLINESS
//! optimization (skip a pointless `DisconnectNamedPipe`/`ConnectNamedPipe`
//! follow-up and a possible spurious `AcceptError` once teardown is under
//! way) — never a safety decision; a stale read there can at worst waste
//! one OS call, never leak, double-close, or use-after-close a handle.
//!
//! This is also what makes the pipe NAME actually disappear promptly on
//! teardown (ADR 0041 Lifecycle: "the pipe NAME disappears before any
//! blocking join") instead of only when the whole `ServerShared` finally
//! drops — `close_all` never blocks on anything but its own lock (every
//! entry is one `CloseHandle`, not a join), so `disconnect_listener` can
//! call it synchronously and return with every instance already gone.
//!
//! # Pending-I/O completion proof
//!
//! `CancelIoEx` only REQUESTS cancellation, and an external `CloseHandle`
//! only forces a pending op to complete or error — neither WAITS for
//! that to actually happen. Microsoft's own overlapped-I/O rules require
//! the `OVERLAPPED` structure, its event, and any I/O buffer to remain
//! valid until the kernel is DONE with a GENUINELY submitted (i.e.
//! `ERROR_IO_PENDING`) op — an error return from `GetOverlappedResult`
//! alone is not that proof, since an external close can race the call
//! itself. [`wait_overlapped`] additionally waits on the OVERLAPPED's own
//! event in that case (bounded, never Win32 `INFINITE`); a
//! SYNCHRONOUSLY-completed op (the OS call itself returned success) needs
//! no such wait — there is nothing left pending for the kernel to still
//! be doing, so a later `GetOverlappedResult` failure there just means
//! the handle is no longer valid for querying the byte count, not that
//! memory safety is at risk. If a GENUINELY pending op's completion is
//! still not observed within the bound, this module can never safely
//! return normally — [`CompletionUnproven`] is the marker every caller
//! must react to by leaking (never freeing) whatever storage it handed
//! to the OS, or, where that storage is caller-owned and cannot be
//! leaked on the caller's behalf ([`PipeClient::write_all`]/
//! [`PipeClient::read`]), aborting the process. See that marker's own
//! doc.
//!
//! # Byte-bounded both directions
//!
//! Outbound: [`OutboundBudget`] reserves BYTES (not items) per connection,
//! including the in-flight item, released only once the write physically
//! completes. Inbound: `events_tx` is a bounded channel and `Bytes`
//! delivery is bounded as described above.
//!
//! # Visibility
//!
//! Every type below is `pub`, not `pub(crate)` — `tests/pipe_win/` is a
//! separate integration-test crate, and an integration test can only ever
//! reach a library's `pub` items.

#![cfg(windows)]

use crate::host::wide_null;
use crate::lane::client::{Client, Endpoint};
use crate::lane::transport::{
    join_within, ClosedReason, LaneEvent, LaneServer, OutboundBudget, StartGate, TransportError,
    BYTES_ABANDON_AFTER, CONNECT_BOUND, EVENTS_CHANNEL_CAP, EVENTS_RETRY_INTERVAL,
    READ_BUF_LEN, TEARDOWN_AGGREGATE_DEADLINE,
};
#[cfg(any(test, feature = "test-support"))]
use crate::lane::transport::JOIN_POLL_INTERVAL;
use std::cell::UnsafeCell;
use std::collections::{HashMap, VecDeque};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock, RwLockReadGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA,
    ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED,
    GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

/// Extra capacity on the bounded reaper inbox beyond `max_instances` — a
/// connection's own at-most-once teardown flag already caps live `Torn`
/// messages at one per open connection, so the only other traffic this
/// inbox ever carries is `Drop`'s own single `Shutdown` message.
const REAPER_INBOX_SLACK: usize = 1;

/// `\\.\pipe\sot-voyage-<id>`, UTF-16, NUL-terminated. Panics never: `id`
/// is validated by [`validate_voyage_id`] at every call site before this
/// runs.
fn pipe_name_wide(voyage_id: &str) -> Vec<u16> {
    wide_null(&format!(r"\\.\pipe\sot-voyage-{voyage_id}"))
}

/// `\\.\pipe\sot-supervisor-<h>`, UTF-16, NUL-terminated (ADR 0041
/// Lifecycle "Name and identity") — the supervisor lane's OWN pipe, a
/// second, independently-named instance of this same server/client
/// machinery, never a voyage pipe under another name. `h` is the caller's
/// own stable hash of the canonicalized state-dir path; unlike
/// [`pipe_name_wide`]'s `voyage_id`, this function neither derives nor
/// validates it — the supervisor lane has no UUID-shape requirement to
/// enforce.
fn supervisor_pipe_name_wide(h: &str) -> Vec<u16> {
    wide_null(&format!(r"\\.\pipe\sot-supervisor-{h}"))
}

/// The voyage id is validated as a canonical RFC 4122 UUID — lowercase,
/// hyphenated, the exact form [`uuid::Uuid`]'s own `Display` produces —
/// before it is ever interpolated into a pipe name. Delegates to
/// `pointer::canonical_voyage_id` (ADR 0041 U0 round-1 minor finding 9):
/// one canonical-UUID check for this crate, not two that can drift, as
/// this one already had from `drawer.voyage`'s own (stricter) validation.
/// Anything that fails to parse at all (path-traversal shapes, wrong
/// length, non-hex bytes) is rejected the same way.
fn validate_voyage_id(voyage_id: &str) -> Result<(), TransportError> {
    if crate::supervisor::journal::pointer::canonical_voyage_id(voyage_id).is_some() {
        Ok(())
    } else {
        Err(TransportError::InvalidVoyageId(voyage_id.to_string()))
    }
}

/// Identifies one accepted connection for the lifetime of a [`PipeServer`].
/// Assigned sequentially; never reused.
pub type ConnId = u64;

/// An opaque, caller-assigned correlation tag for one [`PipeServer::send`]
/// call, echoed back on [`LaneEvent::Sent`] when the OS reports that
/// send's `WriteFile` has PHYSICALLY completed.
pub type SendMarker = u64;

// `ClosedReason`, `LaneEvent`, and `TransportError` used to be defined
// here (`PipeError`/this module's own event enums) — L1-unix LU3a (ADR
// 0043 decisions 17/19) hoisted all three into `crate::lane::transport`, since
// `lane/socket_unix/`'s own copies were byte-for-byte identical in shape and
// both platforms' servers now produce the SAME event type. Imported
// above; nothing in this module defines them anymore.

/// A raw Windows `HANDLE`, asserted `Send` AND `Sync`. `Send`: exactly one
/// owner ever calls `CloseHandle` on it, only after every thread using a
/// copy has stopped. `Sync`: the wrapped value is never dereferenced as a
/// pointer — it is an opaque OS handle, passed only to `windows-sys`
/// calls — so reading a `&SendableHandle` from multiple threads at once
/// is just reading a plain integer.
#[derive(Clone, Copy)]
struct SendableHandle(HANDLE);
unsafe impl Send for SendableHandle {}
unsafe impl Sync for SendableHandle {}

/// One queued outbound send: raw bytes, plus an optional marker to echo
/// back on physical write completion.
struct WriteCmd {
    bytes: Vec<u8>,
    marker: Option<SendMarker>,
}

/// One live connection's threads, handles, and slots — owned by the
/// `conns` map for the connection's whole life; removed and torn down
/// exclusively by [`teardown_if_present`], called exclusively from
/// [`reaper_loop`]. The underlying instance HANDLE's own closing is
/// entirely [`InstanceRegistry`]'s job (`registry_id` is this
/// connection's persistent id there, registered once at creation and
/// never released individually) — this struct's own `raw` is a
/// non-owning reference, usable only through a [`LiveHandle`].
struct ConnHandle {
    raw: SendableHandle,
    registry_id: u64,
    read_slot: Arc<IoSlot>,
    write_slot: Arc<IoSlot>,
    outbound: Arc<OutboundBudget>,
    sender: Sender<WriteCmd>,
    reader_jh: JoinHandle<()>,
    writer_jh: JoinHandle<()>,
    /// At-most-once teardown gate shared with the reader/writer threads —
    /// see [`request_teardown`].
    torn_down_requested: Arc<AtomicBool>,
}

/// Accept-loop state shared with [`PipeServer`]'s public methods and
/// `Drop`.
struct AcceptState {
    /// The accept loop should stop (and, once observed, HAS stopped)
    /// accepting new connections — set either by `PipeServer::drop` or by
    /// a persistent resource failure the accept loop reported via
    /// `AcceptError` (see `ServerShared::dropping` for the distinct
    /// "the whole server is being dropped" flag). EXISTING connections
    /// are unaffected either way.
    accept_stopping: bool,
    /// Instances successfully created and currently held (recycled or
    /// live) for this pipe name (<= `max_instances`): incremented when a
    /// creation attempt is about to run, decremented if that attempt
    /// fails, so it always reflects instances this server actually holds
    /// rather than a permanently-climbing attempt counter.
    created: u32,
    /// Disconnected, ready-to-relisten instances, each paired with its
    /// PERSISTENT `InstanceRegistry` id (registered once, at creation;
    /// recycling reuses the SAME id, never re-registers). Popped by the
    /// accept loop in preference to creating a fresh instance.
    recycled: VecDeque<(u64, SendableHandle)>,
    /// Instances a failed `DisconnectNamedPipe` left in an unknown state
    /// — retained (never closed here, never reused) for the rest of the
    /// server's life so the pipe name's continuous hold survives the
    /// failure; `InstanceRegistry::close_all` closes them like every
    /// other still-registered instance once the server actually tears
    /// down. See the module doc's "Continuous name hold" section.
    retained_dead: Vec<(u64, SendableHandle)>,
    /// The accept loop's currently in-flight `ConnectNamedPipe` attempt,
    /// if any — consulted so [`stop_accept_loop`] can
    /// [`IoSlot::cancel_registered`] exactly the operation that's
    /// actually pending, from whichever thread discovers a reason to
    /// stop (the caller dropping the server, or the reaper thread
    /// finding a `DisconnectNamedPipe` failure while tearing down an
    /// unrelated connection).
    current: Option<(u64, SendableHandle, Arc<IoSlot>)>,
}

/// A message to [`reaper_loop`] — the only thread that ever removes a
/// registered connection from `conns` or joins its threads.
enum ReaperMsg {
    /// A connection ended (natural EOF/error, or a caller's `close`).
    Torn(ConnId, ClosedReason),
    /// The server is being dropped: drain and tear down every connection
    /// still in `conns` (no `Closed` event for these — nothing could ever
    /// observe it), then stop.
    Shutdown,
}

struct ServerShared {
    conns: Mutex<HashMap<ConnId, ConnHandle>>,
    next_id: AtomicU64,
    accept: Mutex<AcceptState>,
    accept_cv: Condvar,
    reaper_tx: SyncSender<ReaperMsg>,
    events_tx: SyncSender<LaneEvent>,
    /// Switch-latency Phase 1 (c): the bridging `Transport` impl's own
    /// wake callback (`PipeTransport::bind`, via [`PipeServer::set_wake`])
    /// — invoked, if set, every time [`send_lifecycle_event`]/
    /// [`deliver_bytes`] successfully push a fresh event, AFTER the push
    /// (so a caller woken by it is guaranteed the event is already
    /// sitting in `events_tx` for `events()`'s own `try_recv` to find).
    /// `OnceLock`, not a `Mutex`: set at most once, by the ONE caller
    /// that ever calls `set_wake` (immediately after `bind`/
    /// `bind_supervisor` returns) — every read after that is wait-free.
    /// Never set at all for the supervisor lane (`bind_supervisor`'s own
    /// caller, `supervisor/`, wakes its main loop by blocking on
    /// `events()` directly — see that module's own `MAIN_LOOP_POLL`
    /// comment — so it has no need of this).
    activity_wake: OnceLock<Arc<dyn Fn() + Send + Sync>>,
    max_instances: u32,
    name: Vec<u16>,
    /// Set exactly once, by `PipeServer::disconnect_listener` (which
    /// `Drop::drop` always calls first), at the very START of that
    /// call — before anything else, including the accept-thread join —
    /// because it is the one escape for [`send_lifecycle_event`]'s
    /// otherwise-indefinite retry loop, and that loop can be running on
    /// the very thread `drop` is about to join. See the module doc's
    /// "Reliable lifecycle delivery" section. `recycle_instance`/
    /// `accept_loop` also read it, purely as a CLEANLINESS optimization
    /// (skip a pointless OS call once teardown is under way) — never a
    /// safety decision; see [`InstanceRegistry`]'s own doc for why actual
    /// instance closing/use no longer depends on this flag at all.
    dropping: AtomicBool,
    /// Every pipe-instance handle this server has ever created, and the
    /// SOLE mechanism that ever closes one or proves one live — see
    /// [`InstanceRegistry`]'s own doc for the ownership invariant.
    instances: InstanceRegistry,
    /// TEST-OBSERVABLE (Codex round-5 fix 2b/2c), written unconditionally
    /// by production code: `stop_accept_loop` sets this to whatever
    /// [`IoSlot::cancel_registered`] returned for the accept loop's own
    /// pending `ConnectNamedPipe`, if any — the TOCTOU-free proof that a
    /// genuinely async op existed at the EXACT instant `disconnect_listener`
    /// cancelled it, as opposed to a separate pre-check that could go
    /// stale before teardown actually runs. Left `false` if there was
    /// nothing pending to cancel.
    accept_cancel_observed_genuine_pending: AtomicBool,
    /// TEST-OBSERVABLE (Codex round-5 fix 2b/2c), written unconditionally
    /// by `disconnect_listener`: for every connection still live at
    /// teardown, whether its WRITE slot's cancellation observed a
    /// genuinely async pending `WriteFile` — same TOCTOU-free reasoning
    /// as `accept_cancel_observed_genuine_pending`, scoped per
    /// connection since several can be torn down at once.
    write_cancel_observed_genuine_pending: Mutex<HashMap<ConnId, bool>>,
}

mod accept;
mod client;
mod conn;
mod registry;
mod server;
mod slot;

use accept::*;
pub use client::{connect_pipe_path_unchallenged, connect_voyage_pipe, PipeClient, PipeEndpoint};
pub(crate) use client::connect_voyage_pipe_unchallenged;
use conn::*;
pub use server::PipeServer;
use registry::*;
use slot::*;

#[cfg(test)]
mod tests {
    use super::*;

    // -- join_within: the ADR 0041 step 6 U1b teardown-deadline mechanism,
    // proven directly against plain `std::thread::spawn` closures this
    // module fully controls -- no real pipe needed for the shared-deadline
    // / loud-on-expiry properties themselves (`tests/pipe_win/` proves
    // the same mechanism composed with real workers).

    #[test]
    fn join_within_true_when_the_thread_already_finished() {
        let jh = thread::spawn(|| {});
        // Real time for the thread to actually finish before polling
        // starts -- this asserts the HAPPY path, not a race against it.
        thread::sleep(Duration::from_millis(50));
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(join_within(jh, deadline));
    }

    #[test]
    fn join_within_false_on_expiry_and_never_blocks_past_the_deadline() {
        let (tx, rx) = mpsc::channel::<()>();
        let jh = thread::spawn(move || {
            let _ = rx.recv(); // never sent: blocks until this process exits
        });
        let budget = Duration::from_millis(50);
        let deadline = Instant::now() + budget;
        let started = Instant::now();
        let ok = join_within(jh, deadline);
        let elapsed = started.elapsed();
        assert!(!ok);
        // Bounded, not merely eventually false -- the whole point of never
        // calling the blocking `JoinHandle::join`.
        assert!(
            elapsed < budget + Duration::from_secs(2),
            "took {elapsed:?} against a {budget:?} budget"
        );
        drop(tx);
    }

    #[test]
    fn one_shared_deadline_not_a_fresh_budget_per_join() {
        // Two joins against the SAME deadline: the first consumes most of
        // the budget by construction (a thread that finishes only after
        // most of it has elapsed), so the second must see a near-zero
        // REMAINING budget -- proving the deadline is shared, not reset
        // per call (ADR 0041: "each wait taking the remaining budget").
        let budget = Duration::from_millis(150);
        let deadline = Instant::now() + budget;

        let jh1 = thread::spawn(move || thread::sleep(Duration::from_millis(100)));
        assert!(join_within(jh1, deadline), "the first join should still fit its share");

        let (tx, rx) = mpsc::channel::<()>();
        let jh2 = thread::spawn(move || {
            let _ = rx.recv();
        });
        assert!(
            !join_within(jh2, deadline),
            "the second join must not get a fresh budget after the first consumed most of it"
        );
        drop(tx);
    }

    /// Codex round-2b, ruling on finding 2: the boundary test proving
    /// expiry with a GENUINELY UNFINISHED thread is terminal, even
    /// though that same thread finishes moments later -- "no acceptance
    /// after the decision". Deterministic, not a race: BOTH preconditions
    /// (the thread is still unfinished, AND the deadline has already
    /// passed) are independently confirmed BEFORE `join_within` is ever
    /// called, so this is not racing the exact expiry instant -- it
    /// proves the DECISION itself (`false`), captured once, is never
    /// revisited by the thread's later completion.
    #[test]
    fn expiry_with_a_genuinely_unfinished_thread_is_terminal_even_though_it_finishes_moments_later() {
        let (tx, rx) = mpsc::channel::<()>();
        let jh = thread::spawn(move || {
            let _ = rx.recv(); // blocks until released below, AFTER the decision is made
        });
        let budget = Duration::from_millis(30);
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !jh.is_finished(),
            "the thread must be genuinely unfinished at the deadline for this test to mean              anything -- confirmed BEFORE join_within is ever called"
        );
        let decision = join_within(jh, deadline);
        assert!(!decision, "an unfinished thread at expiry must be terminal (false)");
        // Release the thread now, strictly AFTER the decision was made --
        // it finishing here must not (and structurally cannot: `decision`
        // is a plain bool already captured) retroactively flip anything.
        drop(tx);
    }
}
