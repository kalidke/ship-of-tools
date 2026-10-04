//! The capsule: one process babysitting one producer, writing its voyage
//! (ADR 0041 step 4 — the capsule runtime; ADR 0039 is the format it
//! writes). Generic over [`crate::capsule::producer::Producer`] (ADR 0043
//! "Decisions for LU2"): the writer loop below — the frame factory, the
//! output budget, the input WAL, the run-end marker, the `AttachProto`
//! service path, `ShutdownGuard`, rotation and sealing — is
//! platform-neutral and drives whatever producer the caller names through
//! exactly nine trait calls: `capsule/producer/conpty/producer.rs`'s `ConptyProducer`
//! (Windows), wrapping `capsule/producer/conpty/`'s owned-ConPTY primitives, and (LU2b)
//! `capsule/producer/pty/`'s `PtyProducer` (Unix), a bare `openpty` fd plus a
//! process group.
//!
//! Three properties this module adds over the legacy Linux capsule, all
//! pinned by ADR 0041 "Step 4 as specified":
//! - a live `vt100_ctt` parser tracks the producer's screen for a later
//!   attach (step 5) to checkpoint from — this unit only keeps it current
//!   and resizes it, it never serializes it (no attach lane exists yet);
//! - the ConPTY host-facing DA1 handshake (`host_handshake.rs`) is
//!   answered THROUGH the teardown drain — the writer loop keeps servicing
//!   it while the pseudoconsole closes, never pausing to make the close
//!   call, so the pre-24H2 blocking close this sequence exists to survive
//!   can never deadlock on an unanswered query (see "Teardown" below);
//! - spawn failure and BOTH teardown entry points (an externally requested
//!   kill, or the producer exiting on its own) are handled by ONE
//!   compensation path / ONE orchestrator, so a segment is always sealed
//!   whenever the producer ever actually ran or a spawn was attempted. An
//!   unexpected PRE-close
//!   reader failure is the one case that still bails unsealed on purpose
//!   (see "Reader errors" below) — that is ADR 0039's crash shape, not a
//!   gap.
//!
//! - **Teardown is now a PHASE of the ordered loop, not a pause in it.**
//!   Now: the reap-poll
//!   keeps servicing the output channel (committing frames, answering the
//!   handshake) WHILE it polls; `close_pty()` runs on a dedicated CLOSER
//!   thread so the writer loop keeps draining concurrently with the call
//!   itself, never pausing for it; the drain timeout's clock starts the
//!   moment the closer thread is spawned, not after it returns.
//! - **The output budget is reserve-before-read and cancellable.** The
//!   reader now reserves a full `READ_CHUNK` BEFORE calling `read()` (and
//!   gives back the unused remainder), so `outstanding` can never exceed
//!   the budget even momentarily; a `BudgetCancelGuard` cancels it — waking
//!   every blocked/future `reserve` — on ANY exit from `run` (normal
//!   return, an early `?`, or a panic unwind), so a reader can never
//!   outlive the loop that is the only thing that ever releases it back.
//!   `release` is checked, not saturating: a mismatch is this module's own
//!   bookkeeping bug and must panic loudly, never absorb silently.
//! - **The handshake exchange is now request → response → outcome** (ADR
//!   0041's own phrase for a "query exchange"), and a write failure commits
//!   a FAILURE outcome instead of silently leaving the exchange looking
//!   like "never delivered, cause unknown". Per the ADR's own model — one
//!   host handshake, asked once, at startup — this module now answers and
//!   records only the FIRST match ever observed for a run; every later
//!   match (a hostile or broken producer's repeat queries) is counted, not
//!   re-answered and not re-recorded, closing the unbounded-frame-spam
//!   amplification.
//! - **Exit status is raw and unsigned end-to-end.** `capsule/producer/conpty/`'s
//!   `exit_code` had a real bug — even after a caller had *already*
//!   confirmed the process exited, it still mapped a genuine raw exit code
//!   of 259 to `None`, the exact `STILL_ACTIVE` value it was trying to
//!   avoid confusing with "still running". It is now
//!   `exit_code_after_confirmed_exit`, which trusts the caller's own prior
//!   observation and returns the DWORD unconditionally. This module carries
//!   that value as `u32` throughout (`ExitSummary`, `producer_dead`'s
//!   `detail.exit_code`) instead of casting to `i32`, which would turn a
//!   high-bit NTSTATUS-shaped code (an access violation, say) negative for
//!   no reason — reinterpretation to a process's own exit code happens only
//!   at the actual OS process-exit boundary, in the bin harness.
//! - **A read error is never silently folded into a sealed success.** The
//!   reader thread now sends one explicit terminal event carrying a real
//!   `Result` (not an undifferentiated "EOF" that swallowed both a clean
//!   close and a genuine I/O error). Whether that's expected depends
//!   entirely on WHEN it arrives: before this loop has ever called
//!   `close_pty()`, it is exactly the anomaly `capsule/producer/conpty/`'s own contract
//!   says shouldn't happen (ConPTY keeps `hOutput` open regardless of
//!   child lifetime until explicitly closed) — capsule-fatal, `run`
//!   returns an `Err` with nothing further written (ADR 0039's crash
//!   shape: recovery seals whatever valid prefix already committed). After
//!   `close_pty()` has been called, both a graceful EOF and a broken pipe
//!   are the ordinary, expected end of the drain.
//! - **`run` no longer owns stdin.** The fix
//!   that survives step 5: `run` takes exactly the caller-owned channel a
//!   caller feeds (today `commands: mpsc::Receiver<Command>` for `Kill`;
//!   the wire's own events are polled through `Transport::try_recv_event`
//!   instead of a second channel parameter — see that method's doc) — it owns none of the
//!   sources that feed them. Admission revocation is real: once the main
//!   loop is left
//!   for teardown, `commands` is never read from again — not
//!   received-then-discarded, simply never polled (the wire lane's own,
//!   NARROWER revocation — producer-bound ops only, mgmt/`Sent` still
//!   serviced — is `AttachProto::begin_teardown`, see "Step 5 (U2)").
//!
//! Judgment calls the review looked at and left standing (not litigated
//! further here): `FrameCtx::capsule_frame` (not `controller_frame`) as the
//! handshake exchange's source, and `to.kind = "producer"` on both the
//! handshake and resize requests, reusing that value to mean "concerns the
//! pty/producer channel" since no `ActorKind` names "the ConPTY host
//! itself"; `controller_frame` for resize (it IS the future driver-facing
//! command); resize's outcome `target` naming the resize request's own
//! `seq`; `ExitKind`'s remaining vocabulary (`ProducerExited`/`Requested`/
//! `SpawnFailed`) having no ADR-pinned name, existing purely for the Rust
//! caller and never reaching a frame.
//!
//! ## Step 5 (U2): the pipe protocol through this loop
//!
//! `run` gains a transport-event channel ([`TransportEvent`]/[`Transport`],
//! the U3 seam — a real named pipe on Windows, or a test transport here)
//! serviced every MAIN-LOOP iteration through
//! [`crate::lane::attach_proto::AttachProto`] — that module OWNS the
//! connection/role/lockstep/pen/keepalive state machine; this loop only
//! executes the [`crate::lane::attach_proto::Action`]s it returns
//! (`execute_actions`) and feeds events back
//! (`connection_opened`/`frame`/`sent`/`tick`/`ground_reached`/
//! `checkpoint_ready`/`take_committed`/`resize_outcome`/`input_outcome`).
//! `flush_output`'s watermark now ALSO publishes committed bytes to
//! existing subscribers and, on a ground boundary, promotes any pending
//! attach — the watermark barrier the ADR requires, one loop step.

use crate::lane::attach_proto::{
    Action as AttachAction, AttachProto, ConnId, InputOutcome, MgmtStatus, SentMarker,
};
use crate::store::envelope::*;
use crate::capsule::producer::{ExitStatus, ParentLease, Producer};
use crate::capsule::producer::host_handshake::{self, HostHandshake};
// the SAME shared-deadline poll-join
// primitive and the SAME pinned aggregate bound `lane/pipe_win/` uses for its
// own worker joins -- one mechanism, one constant, reused here for this
// module's closer/reader thread joins rather than a second bespoke copy.
use crate::lane::transport::{join_within, Transport, TransportEvent, TEARDOWN_AGGREGATE_DEADLINE};
use crate::store::segment::{Commit, RetentionClass, SegmentWriter};
use crate::store::dedupe::{DedupeEntry, DedupeState};
use crate::store::voyage::VoyageStore;
use crate::lane::wire::{self, Survival};
use crate::{Error, Result};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod frame;
mod output;
pub mod producer;
mod writer_loop;
use frame::*;
use output::*;
pub use writer_loop::run;

const SEGMENT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const READ_CHUNK: usize = 8192;

/// ADR 0041 "Terminal state" resource budget — the geometry a resize (or
/// the initial spawn) may request. Independent from the vt100 fork's own
/// `grid::MIN_ROWS`/`MIN_COLS` and `checkpoint::MAX_ROWS`/`MAX_COLS`
/// (confirmed identical by reading the fork's source — all four are
/// `pub(crate)` there, unreachable from here) — pinned separately so this
/// module's enforcement doesn't silently drift if the fork's own private
/// constants ever do; a mismatch would surface loudly, as
/// `Screen::checkpoint`/`set_size` refusing a geometry this module thought
/// was in-budget, never silently.
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 2;
const MAX_COLS: u16 = 512;
const MAX_ROWS: u16 = 256;

/// Scrollback rows the capsule's own live parser keeps, so its checkpoint
/// carries history, not only the visible screen (the fix for "a local
/// capsule pane cannot be scrolled after attach" — the capsule kept none at
/// all, so every restore started the client's ring empty). Bounded well
/// under the vt100 fork's own `checkpoint::MAX_SCROLLBACK_ROWS` (confirmed
/// identical by reading the fork's source, same as `MIN_COLS`/`MAX_ROWS`
/// above — `pub(crate)` there, unreachable from here): the fork's own doc
/// works the arithmetic for why 1000 rows does not fit the ADR 0041 12 MiB
/// checkpoint bound and 200 does, so this is the SAME 200, duplicated the
/// same way.
pub const CAPSULE_SCROLLBACK_ROWS: usize = 200;

/// The oldest checkpoint format version this build still writes on
/// request (ADR 0041 "attach proto v2 bound to checkpoint v2") -- matches
/// the vt100 fork's own `checkpoint::MIN_READABLE_VERSION` (`pub(crate)`
/// there, unreachable from here, duplicated the same way `MIN_ROWS`/
/// `MAX_COLS` above already are). `BeginCheckpoint`'s handling below
/// requests this explicitly for a connection that negotiated
/// `wire::ATTACH_PROTO_V1` -- an old client's own vt100 fork build
/// refuses anything newer outright.
const LEGACY_CHECKPOINT_VERSION: u16 = 1;


/// Bounded wait for the containment job to reap every in-job process after
/// `TerminateJobObject` (teardown Phase A). Generous because it covers an
/// entire process TREE under load, not a single wait; a real failure to
/// reap is a genuine bug or an unkillable hang, and either way deserves a
/// loud, diagnosable error rather than an indefinite one.
const TEARDOWN_REAP_TIMEOUT: Duration = Duration::from_secs(10);
const TEARDOWN_REAP_POLL: Duration = Duration::from_millis(20);

/// How long a terminal reader event that arrived BEFORE `close_output_side`
/// gets to be explained by the producer's own exit (ADR 0043 decision 12, as
/// amended). On macOS a session leader's exit revokes every fd on its
/// controlling terminal -- the capsule's deliberately held slave included --
/// from INSIDE the exit path, so the master can report its terminal state a
/// moment before the leader is observable as exited. This bounds that
/// kernel-internal window and nothing else: it is not a knob, not
/// configurable, and expires only on a genuine anomaly, which is fatal. Four
/// orders of magnitude above the window it covers and an order of magnitude
/// below [`TEARDOWN_REAP_TIMEOUT`], so it can never mask a reap failure.
const READER_END_EXIT_GRACE: Duration = Duration::from_secs(2);

/// Bounded wait, during teardown Phase B, for the reader thread's own
/// terminal event after the closer thread's `close_pty()` call is spawned.
/// Starts the moment that thread is spawned — CONCURRENTLY with the
/// (possibly blocking, pre-24H2) close, not after it returns, which is
/// exactly what the previous version got wrong and why this bound can now actually do its job: a hang in
/// `ClosePseudoConsole` itself no longer prevents this deadline from firing.
const TEARDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const TEARDOWN_DRAIN_POLL: Duration = Duration::from_millis(200);

/// ADR 0041 bounds table ("ack grace", role "a final-poll request still
/// gets its ack") / EndRun state machine item 4: a mgmt `shutdown` accepted
/// in teardown's own FINAL service poll (the one Phase B runs the instant
/// EOF ends the drain — see that call site's own doc) has its `ShutdownAck`
/// queued but not yet reported physically written by the time the drain
/// loop itself is done. This capsule's transport must not disappear out
/// from under that unconfirmed ack — U1a defers the pipe's own teardown by
/// up to this long, polling for the completion, before proceeding.
const SHUTDOWN_ACK_GRACE: Duration = Duration::from_secs(2);
/// Poll interval while waiting out [`SHUTDOWN_ACK_GRACE`] — no output
/// channel to block on at this point (Phase B's own drain already reached
/// reader EOF), so this is a plain sleep between non-blocking
/// `Transport::try_recv_event` drains, the same granularity as
/// [`TEARDOWN_REAP_POLL`].
const SHUTDOWN_ACK_GRACE_POLL: Duration = Duration::from_millis(20);

pub struct CapsuleConfig {
    pub voyage_root: PathBuf,
    pub voyage_id: String,
    pub retention: RetentionClass,
    pub producer_kind: String,
    /// argv[0] is the program; must be non-empty (`Producer::spawn`'s own
    /// check is what actually enforces this).
    pub argv: Vec<String>,
    /// Initial terminal geometry, validated by the SAME 2x2..512x256 rule
    /// a later resize is (ADR 0041: "Initial geometry is validated by the
    /// same rule").
    pub cols: u16,
    pub rows: u16,
    /// Supplied by the SPAWNER, never inferred (ADR 0041 decision 11: step
    /// 6's breakaway attempt is the real source; `IsProcessInJob`
    /// observation stays diagnostics, never authority). Transported
    /// verbatim in mgmt `status`.
    pub survival: Survival,
    /// The reader-first rollout gate's input (ADR 0041 "Upgrade and
    /// version skew"; see `crate::store::rollout`) — TYPED, identity-bound
    /// evidence, never an `Option` a caller could pass `None` into as an
    /// implicit "no rollback target": `run`
    /// refuses to open a segment declaring
    /// `sot.capsule.run-end-requested-v1` unless this evidence
    /// affirmatively clears it. The SPAWNER constructs this — a real
    /// supervisor (U2/U4) from its own release-apply transaction; this
    /// crate's manual testing harness (`sot-capsule.rs`) hardcodes
    /// `RolloutEvidence::NoRollbackTarget` directly, never reading a
    /// stopgap file that could quietly become load-bearing.
    pub rollout_evidence: crate::store::rollout::RolloutEvidence,
    /// ADR 0041 Lifecycle "Discovery, and the two windows a spawn passes
    /// through": `Some(lease)` when a supervisor spawned this process and
    /// wants its parent-death lease checked as the writer fence's own
    /// first act — a per-platform [`ParentLease`] (ADR 0043 decision 15)
    /// `run` polls EXACTLY ONCE, immediately after the fence is acquired,
    /// via `VoyageStore::open_for_writing_with_lease`. `None` (this
    /// crate's own manual-testing harness, and every existing capsule
    /// test) is the U1a wrapper's own no-lease behavior, unchanged —
    /// every in-tree caller before U2.
    pub parent_lease: Option<ParentLease>,
}

/// The ADR 0039 registry entry a step-6 capsule's segments declare
/// unconditionally at creation (ADR 0041 Lifecycle: "the marker's timing
/// is not knowable in advance"). One name, one home — every
/// `open_segment_with_features` call site in this module names this
/// constant rather than the literal string.
const RUN_END_REQUESTED_FEATURE: &str = "sot.capsule.run-end-requested-v1";

/// The caller-owned command surface `run` services, alongside the wire
/// protocol. Step 5 DELETES this channel's raw `Input`/`Resize` variants
/// (ADR 0041 spec gate): the wire lane replaces both — real input and
/// resize now arrive as `AttachClient::Input`/`Resize` frames, handled
/// through `AttachProto` and `execute_actions`'s `ForwardInput`/
/// `ApplyResize` arms, never through this channel. `Kill` stays: it is the
/// step-4-visible primitive behind `EndRun` (ADR 0041 Lifecycle) that BOTH
/// the mgmt lane's `shutdown` (via `Action::Shutdown`) and a caller that
/// bypasses the pipe entirely (the bin harness, a supervisor) can drive.
/// `run` owns none of the sources that feed this channel — a caller is
/// responsible for keeping the `Sender` alive for as long as it wants
/// commands serviced.
#[derive(Debug, Clone)]
pub enum Command {
    /// An EXTERNALLY REQUESTED end. Never inferred from an exit code, a
    /// channel disconnect, or anything else — only an explicit `Kill` (or
    /// the wire's `Action::Shutdown`, which drives the identical
    /// `ExitKind::Requested` path).
    Kill,
}

/// Why `run` returned. In-memory only — no frame field encodes this (ADR
/// 0041 Lifecycle: "Exit codes play no role in run lifetime", and the same
/// is true of this signal, which exists purely for the Rust caller). Naming
/// is a judgment call: no ADR-pinned vocabulary exists for it yet (EndRun's
/// own reason enum is step 5's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    /// The producer exited on its own; this run's own program ending is
    /// what closed it — never treated as a request.
    ProducerExited,
    /// `Command::Kill` was received, or the mgmt lane's `shutdown` drove
    /// `Action::Shutdown` (EndRun) after its ack was physically written.
    Requested,
    /// `P::spawn` failed, or the initial geometry was outside the
    /// budget — nothing ever ran. `producer_dead {spawn_failed:true}` was
    /// still committed and the segment still sealed.
    SpawnFailed,
}

#[derive(Debug)]
pub struct ExitSummary {
    pub exit_code: Option<ExitStatus>,
    pub exit_kind: ExitKind,
    pub frames_written: u64,
    pub segments_sealed: u64,
    /// Whether the host-facing DA1 handshake was answered and recorded
    /// (ADR 0041's model: conhost asks once, at startup) — `false` if it
    /// never arrived at all in this run.
    pub handshake_answered: bool,
    /// How many DA1 matches arrived AFTER the first was already answered
    /// (including extras within the very same chunk as the first) — a
    /// hostile or broken producer's repeat queries, counted but never
    /// re-answered and never re-recorded (the amplification fix).
    pub handshake_suppressed_matches: u64,
    /// How many times `ResizePseudoConsole` was actually invoked. An
    /// out-of-budget resize command never reaches this call at all — the
    /// seam a test needs to prove rejection is a real short-circuit, not
    /// just a recorded disposition string.
    pub resize_os_calls: u64,
}

/// The reader thread's own event stream: producer output, or its ONE
/// terminal event. `Done` carries a real `Result` rather than an
/// undifferentiated EOF. Kept SEPARATE from the caller's `Command` channel — see the
/// module doc's stdin-ownership point — so during teardown this loop can
/// keep servicing this channel while never touching that one again, which
/// is what makes "teardown revokes admission" literally true rather than
/// "teardown discards what it still received".
enum ReaderEvent {
    Output(Vec<u8>),
    /// `Ok(())` is a graceful `read() == Ok(0)`; `Err(e)` is a real I/O
    /// error. Sent EXACTLY once, always, as the last thing this thread
    /// ever sends.
    Done(std::result::Result<(), std::io::Error>),
    /// Switch-latency Phase 1 (c): a transport event (an accepted
    /// connection, a readable frame, a completed send) may be waiting —
    /// carries no data itself. Sent by `Transport::set_wake`'s own
    /// callback (`run`'s own `wake_pending`-gated closure), NEVER by the
    /// reader thread. Purely a wake: every `output_rx.recv_timeout` site
    /// that matches this just clears `wake_pending` and loops back to its
    /// own top, where `service_transport_events`/
    /// `service_transport_events_teardown` (already run there,
    /// unconditionally, every iteration) is what actually drains and
    /// processes whatever the transport queued.
    TransportActivity,
    /// the reader thread's `tx` is no longer the
    /// channel's only real sender — `Transport::set_wake`'s callback holds
    /// a clone too (above) — so a bare channel disconnect can no longer be
    /// trusted to mean "the reader thread dropped its sender". This event
    /// restores that guarantee explicitly: `ReaderGoneGuard`, constructed
    /// as the reader thread's closure's first local, sends exactly this on
    /// every exit from that closure — the two designed `Done`-then-return
    /// exits above AND an unwind (a panic partway through a `read()` or a
    /// `budget` call) — so a reader-thread death always reaches every
    /// `output_rx.recv_timeout` site as a real, matched event, never a
    /// silent disconnect the wake clone happens to paper over. Treated
    /// exactly where `RecvTimeoutError::Disconnected` is already handled
    /// (same arm, same error) — it is that same "the reader is gone and
    /// said nothing" condition, just reached through an explicit send
    /// instead of a channel with zero senders left.
    ReaderGone,
}


/// This process's mgmt `status` fields (ADR 0041 attach protocol): pid and
/// process CREATION TIME as the raw FILETIME bits — computed ONCE, here,
/// because `attach_proto` must never make an OS call itself. `survival` is
/// the spawner-supplied value (decision 11), never derived from
/// `IsProcessInJob`.
///
/// Finding 14: `GetProcessTimes`' return value is CHECKED, not ignored — a
/// failure becomes a loud `Err`, never a silent `created: 0`. A synthesized
/// zero would be indistinguishable, to step 6's adoption identity
/// challenge, from a process genuinely created at the Windows epoch; the
/// challenge compares this value EXACTLY against `GetProcessTimes` called a
/// second time (via `OpenProcess`) on a candidate handle, so a wrong-but
/// -plausible value here is worse than an explicit failure the caller can
/// act on.
///
/// Per-platform sibling (ADR 0043 decision 16: "per-platform siblings,
/// not knobs") — the Linux sibling just below is
/// `(getpid, challenge_unix::self_start_ticks())` and the macOS one
/// `(getpid, challenge_macos::self_pidversion())`.
#[cfg(windows)]
fn self_status(survival: Survival) -> Result<MgmtStatus> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetProcessTimes};
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle needing no close;
    // the four FILETIME out-params are plain, stack-local structs, valid to
    // write into regardless of the call's outcome.
    let (pid, created) = unsafe {
        let pid = GetCurrentProcessId();
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        if GetProcessTimes(GetCurrentProcess(), &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
            return Err(Error::State(format!(
                "capsule_win: GetProcessTimes on the current process failed: {:?}",
                std::io::Error::last_os_error()
            )));
        }
        let created = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        (pid, created)
    };
    Ok(MgmtStatus { pid, created, survival })
}

/// ADR 0043 decision 16: the Linux arm — `getpid()` plus
/// `challenge_unix::self_start_ticks()`, the SAME start-time identity the
/// Linux socket challenge (`challenge_unix.rs`) reports for a supervisor's
/// own adoption proof, so a capsule's `status` reply and its later
/// adoption challenge, if any, describe the identical process the
/// identical way.
#[cfg(target_os = "linux")]
fn self_status(survival: Survival) -> Result<MgmtStatus> {
    let pid = std::process::id();
    let created = crate::identity::challenge_unix::self_start_ticks()
        .map_err(|e| Error::State(format!("capsule: self_start_ticks failed: {e}")))?;
    Ok(MgmtStatus { pid, created, survival })
}

/// ADR 0043 decision 16, the macOS arm — `getpid()` plus
/// `challenge_macos::self_pidversion()`. The UNIT differs from Linux's
/// and that is the whole point: every platform's `created` is "whatever
/// this OS's own `status_ok.created` carries, compared for equality
/// only" (`client::PeerIdentity::created`), and on macOS that unit is
/// the `pidversion` the peer's audit token carries — NOT a start time.
/// `challenge_macos`'s step 5 compares a reply's `created` against the
/// pidversion it read out of the token, so a start time reported here
/// would make every macOS adoption challenge `Foreign`. `self_pidversion`
/// exists for exactly this call site: it is the self-facing twin of the
/// value a client reads off its own socket, the same way
/// `self_start_ticks` is on Linux.
#[cfg(target_os = "macos")]
fn self_status(survival: Survival) -> Result<MgmtStatus> {
    let pid = std::process::id();
    let created = crate::identity::challenge_macos::self_pidversion()
        .map_err(|e| Error::State(format!("capsule: self_pidversion failed: {e}")))?;
    Ok(MgmtStatus { pid, created: u64::from(created), survival })
}
