//! The Windows capsule: one process babysitting one producer under an
//! owned ConPTY, writing its voyage (ADR 0041 step 4 — the capsule
//! runtime; ADR 0039 is the format it writes; `conpty.rs` is the
//! primitives layer this module orchestrates).
//!
//! Mirrors `capsule.rs`'s shape and spirit deliberately — same
//! writer-loop-owns-everything design, same frame protocol, same group-
//! commit/echo watermark, same input WAL shape — but is NOT that module:
//! a completely separate, independently maintained copy for a completely
//! different OS primitive (owned ConPTY + job containment vs. a bare PTY
//! fd). Even the parts that read identically (the base64 encoder, the
//! frame-context helper) are duplicated here, not shared: `capsule.rs` is
//! `#![cfg(target_os = "linux")]`-gated, so there is no common home for a
//! shared module without inventing one, which is out of this unit's scope
//! (Codex review: not a blocker, don't refactor the proven Linux path to
//! chase a Windows hazard).
//!
//! Three properties this module adds over the Linux capsule, all pinned by
//! ADR 0041 "Step 4 as specified":
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
//!   whenever the producer ever actually ran or a spawn was attempted —
//!   the Linux capsule's own known gap (a bare `?` on `spawn_on_pty` that
//!   escapes unsealed) is deliberately NOT inherited. An unexpected PRE-close
//!   reader failure is the one case that still bails unsealed on purpose
//!   (see "Reader errors" below) — that is ADR 0039's crash shape, not a
//!   gap.
//!
//! ## Discharge round (Codex adversarial review, capsule_win.rs unit)
//!
//! The first version of this file shipped a real teardown deadlock and six
//! other findings; this is the corrected version. What changed, and why:
//!
//! - **Teardown is now a PHASE of the ordered loop, not a pause in it.**
//!   The first version polled `ActiveProcesses` and called `close_pty()`
//!   WITHOUT draining the output channel, so a reader already blocked in
//!   `OutputBudget::reserve` (or a DA1 only the writer loop could answer)
//!   could leave `hOutput` undrained right when `ClosePseudoConsole` needed
//!   it drained — Microsoft documents that pre-24H2 build's close as
//!   capable of waiting indefinitely under exactly that condition, and the
//!   old `TEARDOWN_DRAIN_TIMEOUT` couldn't detect it because its clock
//!   started AFTER the (blocking) close call returned. Now: the reap-poll
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
//!   amplification the first version had no defense against.
//! - **Exit status is raw and unsigned end-to-end.** `conpty.rs`'s
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
//!   `close_pty()`, it is exactly the anomaly `conpty.rs`'s own contract
//!   says shouldn't happen (ConPTY keeps `hOutput` open regardless of
//!   child lifetime until explicitly closed) — capsule-fatal, `run`
//!   returns an `Err` with nothing further written (ADR 0039's crash
//!   shape: recovery seals whatever valid prefix already committed). After
//!   `close_pty()` has been called, both a graceful EOF and a broken pipe
//!   are the ordinary, expected end of the drain. The `ReaderClosedUnexpectedly`
//!   `ExitKind` variant is gone — it named a failure as if it were a
//!   legitimate way for a run to end successfully, which it never was.
//! - **`run` no longer owns stdin.** (Historical, step 4: at the time this
//!   applied to a `Command` enum that still carried `Input`/`Resize`
//!   alongside `Kill`, and to a bin harness that forwarded raw stdin bytes
//!   as `Command::Input` — step 5 later DELETED both, see "Step 5 (U2)"
//!   below; only `Kill` remains on `Command` today, and the bin harness has
//!   no stdin thread at all any more.) The first version of THIS file read
//!   the real process stdin internally, which (a) is process-global state a
//!   reusable library function has no business owning, and (b) meant
//!   "teardown revokes admission" was really just "teardown discards what
//!   it already accepted" — the thread kept enqueueing regardless. The fix
//!   that survives step 5: `run` takes exactly the caller-owned channel a
//!   caller feeds (today `commands: mpsc::Receiver<Command>` for `Kill`;
//!   the wire's own events are polled through `Transport::try_recv_event`
//!   instead of a second channel parameter, round-2 e2e review's own
//!   deletion pressure — see that method's doc) — it owns none of the
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
//! [`crate::attach_proto::AttachProto`] — that module OWNS the
//! connection/role/lockstep/pen/keepalive state machine; this loop only
//! executes the [`crate::attach_proto::Action`]s it returns
//! (`execute_actions!`) and feeds events back
//! (`connection_opened`/`frame`/`sent`/`tick`/`ground_reached`/
//! `checkpoint_ready`/`take_committed`/`resize_outcome`/`input_outcome`).
//! `flush_output!`'s watermark now ALSO publishes committed bytes to
//! existing subscribers and, on a ground boundary, promotes any pending
//! attach — the watermark barrier the ADR requires, one loop step.
//!
//! Four things this unit DELETES, per the ADR 0041 step-5 spec gate: the
//! preamble's automatic `"local"` take grant (the null-holder revoke is now
//! the whole preamble — the first driver ever is a pipe `take`);
//! `Command::Input`/`Command::Resize` (the wire lane replaces both —
//! `Command::Kill` stays, driven by either a direct caller or the wire's
//! mgmt `shutdown`); the bin harness's Windows stdin-forwarding thread and
//! its stdout echo mirroring (pipe fan-out is the real subscriber path now;
//! a bare `--echo` mirror duplicates that for no one); and the capsule's own
//! `random_idem_key` generator (the CLIENT supplies `idem_key` on the wire —
//! `hex_idem_key` still exists, for encoding it, not generating one).

#![cfg(windows)]

use crate::attach_proto::{
    Action as AttachAction, AttachProto, ConnId, InputOutcome, MgmtStatus, SentMarker,
};
use crate::conpty::{observe_spawning_process_jobbed, ConptySpawn};
use crate::envelope::*;
use crate::host_handshake::{self, HostHandshake};
// Codex round-1 Blocker 3 discharge: the SAME shared-deadline poll-join
// primitive and the SAME pinned aggregate bound `pipe_win.rs` uses for its
// own worker joins -- one mechanism, one constant, reused here for this
// module's closer/reader thread joins rather than a second bespoke copy.
use crate::transport::{join_within, Transport, TransportEvent, TEARDOWN_AGGREGATE_DEADLINE};
use crate::segment::{Commit, RetentionClass, SegmentWriter};
use crate::voyage::{DedupeEntry, DedupeState, VoyageStore};
use crate::wire::{self, Survival};
use crate::{Error, Result};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const GROUP_COMMIT_WINDOW: Duration = Duration::from_millis(50);
const GROUP_COMMIT_BYTES: usize = 256 * 1024;
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

/// The output channel's byte budget (ADR 0041 budget table: "producer
/// channel 8 MiB bounded"). Bounds OUTSTANDING bytes only — chunks the
/// reader thread has reserved (in `READ_CHUNK`-sized units, before it ever
/// calls `read()`) but the writer loop has not yet committed — never a
/// second buffer of its own; see [`OutputBudget`].
const OUTPUT_QUEUE_BUDGET_BYTES: u64 = 8 * 1024 * 1024;

/// Bounded wait for the containment job to reap every in-job process after
/// `TerminateJobObject` (teardown Phase A). Generous because it covers an
/// entire process TREE under load, not a single wait; a real failure to
/// reap is a genuine bug or an unkillable hang, and either way deserves a
/// loud, diagnosable error rather than an indefinite one.
const TEARDOWN_REAP_TIMEOUT: Duration = Duration::from_secs(10);
const TEARDOWN_REAP_POLL: Duration = Duration::from_millis(20);

/// Bounded wait, during teardown Phase B, for the reader thread's own
/// terminal event after the closer thread's `close_pty()` call is spawned.
/// Starts the moment that thread is spawned — CONCURRENTLY with the
/// (possibly blocking, pre-24H2) close, not after it returns, which is
/// exactly what the previous version got wrong (review finding, the
/// blocker) and why this bound can now actually do its job: a hang in
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

pub struct CapsuleWinConfig {
    pub voyage_root: PathBuf,
    pub voyage_id: String,
    pub retention: RetentionClass,
    pub producer_kind: String,
    /// argv[0] is the program; must be non-empty (`ConptySpawn::spawn`'s
    /// own check is what actually enforces this).
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
    /// version skew"; see `crate::rollout`) — TYPED, identity-bound
    /// evidence, never an `Option` a caller could pass `None` into as an
    /// implicit "no rollback target" (Codex round-1 Major 9): `run`
    /// refuses to open a segment declaring
    /// `sot.capsule.run-end-requested-v1` unless this evidence
    /// affirmatively clears it. The SPAWNER constructs this — a real
    /// supervisor (U2/U4) from its own release-apply transaction; this
    /// crate's manual testing harness (`sot-capsule.rs`) hardcodes
    /// `RolloutEvidence::NoRollbackTarget` directly, never reading a
    /// stopgap file that could quietly become load-bearing.
    pub rollout_evidence: crate::rollout::RolloutEvidence,
    /// ADR 0041 Lifecycle "Discovery, and the two windows a spawn passes
    /// through": `Some(name)` when a supervisor spawned this process and
    /// wants its parent-death lease checked as the writer fence's own
    /// first act (`crate::lease`) — a named, kernel-brokered mutex `run`
    /// opens and polls EXACTLY ONCE, immediately after the fence is
    /// acquired, via `VoyageStore::open_for_writing_with_lease`. `None`
    /// (this crate's own manual-testing harness, and every existing
    /// capsule_win test) is the U1a wrapper's own no-lease behavior,
    /// unchanged — every in-tree caller before U2.
    pub parent_lease_name: Option<String>,
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
/// through `AttachProto` and `execute_actions!`'s `ForwardInput`/
/// `ApplyResize` arms, never through this channel. `Kill` stays: it is the
/// step-4-visible primitive behind `EndRun` (ADR 0041 Lifecycle) that BOTH
/// the mgmt lane's `shutdown` (via `Action::Shutdown`) and a caller that
/// bypasses the pipe entirely (the bin harness, a supervisor) can drive.
/// `run` owns none of the sources that feed this channel — a caller is
/// responsible for keeping the `Sender` alive for as long as it wants
/// commands serviced (review finding: a previous version read stdin
/// itself, which made "teardown revokes admission" not really true).
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
    /// `ConptySpawn::spawn` failed, or the initial geometry was outside
    /// the budget — nothing ever ran. `producer_dead {spawn_failed:true}`
    /// was still committed and the segment still sealed.
    SpawnFailed,
}

#[derive(Debug)]
pub struct ExitSummary {
    pub exit_code: Option<u32>,
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
/// undifferentiated EOF (review finding: the previous version collapsed
/// every read error into the same signal a graceful close produces, so an
/// unexpected pre-close failure could silently become a normal, sealed
/// success). Kept SEPARATE from the caller's `Command` channel — see the
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
}

/// Encodes a wire `idem_key` (16 raw bytes) as the lowercase hex32 shape
/// ADR 0039's `input` frame requires. Step 5 deletes the capsule's own
/// `random_idem_key` generator from the production path — capsule-generated
/// idem keys are gone now that the CLIENT supplies one per wire input — but
/// the hex encoding itself is still needed, for the frame's JSON payload.
fn hex_idem_key(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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

struct BudgetState {
    outstanding: u64,
    closed: bool,
    /// Test-only waiter-entry witness: incremented under the mutex
    /// immediately before `Condvar::wait`, decremented on wake. Lets a
    /// unit test PROVE a reserve entered the wait before releasing or
    /// cancelling — without it, a test's release can win the race to the
    /// first bound check and pass without ever exercising the wake path,
    /// so a missing `notify_all` could escape (review finding).
    #[cfg(test)]
    waiters: u32,
}

/// The bounded output budget (ADR 0041: "producer channel 8 MiB bounded —
/// when full the writer loop stops POLLING output... control/liveness
/// always serviced"). Implemented as a shared OUTSTANDING-byte counter
/// rather than a second queue structure: `ReaderEvent::Output` chunks still
/// ride the same channel the reader thread always used, but that thread
/// blocks (via this condvar) BEFORE its next `read()` once the budget is
/// exhausted — so `Command`, read by the writer loop from a completely
/// separate channel, is never gated by it at all.
struct OutputBudget {
    state: Mutex<BudgetState>,
    space_available: Condvar,
}

impl OutputBudget {
    fn new() -> Self {
        Self {
            state: Mutex::new(BudgetState {
                outstanding: 0,
                closed: false,
                #[cfg(test)]
                waiters: 0,
            }),
            space_available: Condvar::new(),
        }
    }

    /// Reader-thread-only: reserves `n` bytes BEFORE the read that will
    /// produce them, blocking while `outstanding + n` would exceed the
    /// budget — never after the read, and never against `outstanding`
    /// alone (review finding: the previous version reserved AFTER
    /// reading and checked only the current total, so one over-budget
    /// chunk plus one already-in-flight chunk could both slip past the
    /// nominal bound). Returns `false` if the budget was (or became, while
    /// waiting) cancelled — the reader must stop immediately, WITHOUT
    /// reserving, and never call `read()` again.
    fn reserve(&self, n: u64) -> bool {
        let mut g = self.state.lock().unwrap();
        loop {
            if g.closed {
                return false;
            }
            if g.outstanding + n <= OUTPUT_QUEUE_BUDGET_BYTES {
                g.outstanding += n;
                return true;
            }
            #[cfg(test)]
            {
                g.waiters += 1;
            }
            g = self.space_available.wait(g).unwrap();
            #[cfg(test)]
            {
                g.waiters -= 1;
            }
        }
    }

    /// Writer-loop-only: releases exactly `n` bytes once they have been
    /// accounted for (a frame appended, or reservation given back unused).
    /// Checked, not saturating (review finding): a mismatch here is this
    /// module's own bookkeeping bug and must panic loudly, never silently
    /// absorb the discrepancy.
    fn release(&self, n: u64) {
        let mut g = self.state.lock().unwrap();
        g.outstanding =
            g.outstanding.checked_sub(n).expect("OutputBudget::release: released more than was reserved");
        self.space_available.notify_all();
    }

    /// Cancels the budget — every future and currently-blocked `reserve`
    /// call returns `false` immediately. Called from `BudgetCancelGuard`'s
    /// `Drop` on every exit from `run` (review finding: a reader blocked in
    /// `reserve` must never be able to outlive the writer loop that is the
    /// only thing that would otherwise ever release it).
    fn cancel(&self) {
        let mut g = self.state.lock().unwrap();
        g.closed = true;
        self.space_available.notify_all();
    }
}

/// RAII: cancels the shared output budget on ANY exit from `run` — a
/// normal return, an early `?`, or a panic unwind. See `OutputBudget::cancel`.
struct BudgetCancelGuard(Arc<OutputBudget>);
impl Drop for BudgetCancelGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// The writer loop's frame factory: sequential seq, capsule clocks, and the
/// per-run refs (attached_to / input WAL) threaded through one small state.
/// Identical in shape to `capsule.rs`'s `FrameCtx` — duplicated, not
/// shared (module doc).
struct FrameCtx {
    epoch: u64,
    next_n: u64,
    t0: Instant,
    /// The DURABLE take-epoch value — the capsule's own, fed in/out of
    /// `attach_proto` as a plain value (ADR 0041: "the durable holder/epoch
    /// is the CAPSULE's"). Starts at the null-holder revoke's value; the
    /// step-4 "local" grant is deleted (spec gate) — the first driver ever
    /// is a pipe `take`.
    take_epoch: u64,
    /// The DURABLE holder's controller_id, mirroring `take_epoch` — `None`
    /// until the first real `take` commits.
    holder: Option<String>,
    attached: Option<Seq>,
}

impl FrameCtx {
    fn seq(&mut self) -> Seq {
        let s = Seq {
            epoch: self.epoch,
            n: self.next_n,
        };
        self.next_n += 1;
        s
    }
    fn mono_us(&self) -> u64 {
        self.t0.elapsed().as_micros() as u64
    }
    fn capsule_frame(&mut self, class: Class, payload: serde_json::Value) -> Envelope {
        Envelope {
            seq: self.seq(),
            class,
            source: Source {
                emitter: Emitter::Capsule,
                actor: Actor {
                    kind: ActorKind::Unknown,
                    controller_id: None,
                    take_epoch: None,
                },
                derivation: Derivation::Synthetic,
            },
            t_wall_ms: wall_ms(),
            t_mono_us: self.mono_us(),
            stream: None,
            transformed: None,
            refs: vec![],
            payload: Some(payload),
            payload_ref: None,
        }
    }
    /// A controller-actor frame declaring EXACTLY `(controller_id,
    /// take_epoch)` — a thin constructor, not itself the source of which
    /// epoch is honest. Round-2 review deletion residue: this doc used to
    /// say a stale `input` is "honestly attributed to whatever it
    /// claimed", describing behavior finding 1 already replaced. Every
    /// real call site (`run_input_wal`) now passes the COMMITTED
    /// `ctx.take_epoch`, never the wire-claimed one — a stale input's
    /// CLAIMED epoch is recorded only inside the `refused_stale_epoch`
    /// fact's own diagnostic body, never on this envelope's actor
    /// identity (ADR 0039's take predicate judges staleness from the
    /// fact, not from a claimed-epoch envelope this module no longer
    /// writes).
    fn controller_frame(
        &mut self,
        class: Class,
        controller_id: String,
        take_epoch: u64,
        payload: serde_json::Value,
    ) -> Envelope {
        let mut e = self.capsule_frame(class, payload);
        e.source.actor = Actor {
            kind: ActorKind::Controller,
            controller_id: Some(controller_id),
            take_epoch: Some(take_epoch),
        };
        e
    }
    /// A controller-actor frame using THIS ctx's own current durable
    /// identity — correct only where the actor IS, by construction, the
    /// current holder (e.g. `resize`, driver-only and carrying no identity
    /// fields of its own on the wire).
    fn current_controller_frame(&mut self, class: Class, payload: serde_json::Value) -> Envelope {
        let controller_id = self.holder.clone().unwrap_or_default();
        let take_epoch = self.take_epoch;
        self.controller_frame(class, controller_id, take_epoch, payload)
    }
    fn producer_frame(&mut self, payload: serde_json::Value) -> Envelope {
        let mut e = self.capsule_frame(Class::Producer, payload);
        e.source.emitter = Emitter::Producer;
        e.source.actor.kind = ActorKind::Producer;
        e.source.derivation = Derivation::Native;
        e.refs = vec![FrameRef {
            kind: RefKind::AttachedTo,
            frame: self.attached.expect("attached before producer frames"),
        }];
        e
    }
}

/// ADR 0041 EndRun steps 1-2: append + fsync the ONE
/// `run_end_requested {reason}` lifecycle frame and IRREVOCABLY latch
/// EndRun — idempotent past the first (step 4: first commit wins, a
/// concurrent later request writes no second marker). Shared by BOTH of
/// `run`'s action executors (`execute_actions!` and
/// `execute_teardown_actions!`) since a `shutdown` arriving during the
/// final teardown poll must latch exactly the same way as one arriving
/// mid-run — see this crate's `verify::leg_carries_run_end_marker` for
/// the READ half a later unit's respawn decision uses.
///
/// A real function, not a macro, specifically so it is independently
/// testable against a plain `SegmentWriter`/`FrameCtx` pair (this
/// module's own `tests`), with no real ConPTY run needed to exercise the
/// one property that matters here: on an append failure, `?` propagates
/// with `run_end_latched` left false — no ack ever reached (it sits
/// AFTER this call in the same action batch, unreached once `?` returns)
/// and nothing is latched, exactly ADR 0039's crash shape ("no ack, no
/// marker, unsealed process exit"). Why the append could fail is not
/// this function's concern — a real storage fault and a plain
/// contiguity/schema violation reach the identical `Err` path, which is
/// the only property a capsule-side test can honestly claim (this
/// crate's fault harness is explicit that storage-level fault injection
/// itself is a separate, unclaimed follow-up — see `tests/fault_kill.rs`'s
/// own doc).
fn commit_run_end_marker(
    ctx: &mut FrameCtx,
    w: &mut SegmentWriter,
    frames_written: &mut u64,
    run_end_latched: &mut bool,
    reason: String,
) -> Result<()> {
    if !*run_end_latched {
        let f = ctx.capsule_frame(
            Class::Lifecycle,
            json!({"kind": "run_end_requested", "reason": reason}),
        );
        w.append(&f, Commit::Immediate)?;
        *frames_written += 1;
        *run_end_latched = true;
    }
    Ok(())
}

/// Executes the ADR 0039 input WAL for one wire `input` frame, using the
/// store's dedupe index (folded once at open, kept live here) to fold a
/// duplicate `idem_key` per the lattice exactly, and returns the outcome
/// for the caller to report back via [`AttachProto::input_outcome`]. See
/// [`AttachAction::ForwardInput`]'s doc for the full sequence this
/// implements; `connection_authorized` is `attach_proto`'s connection-scoped
/// half of the ADR's "the capability AND the durable holder/epoch" check —
/// the durable half (against `ctx`'s own state) is this function's job, and
/// both must pass for a fresh forward.
///
/// **Finding 1 (verifier-red frames):** every controller-actor envelope
/// this function writes — `input`, `refused_stale_epoch`, `forward_intent`,
/// `forwarded` alike — carries `ctx.take_epoch`, the CURRENTLY COMMITTED
/// epoch, in its `Actor.take_epoch` field, NEVER the wire-claimed
/// (possibly stale) `take_epoch` parameter. `verify.rs` requires every
/// controller frame's declared `take_epoch` to equal whatever is committed
/// AT THAT POINT in the frame stream (`controller take_epoch {te} !=
/// committed {committed_take_epoch}` is its exact check) — that field
/// records WHEN a frame was written, not what a client claimed. Staleness
/// lives ONLY in the `refused_stale_epoch` fact itself (the fact-kind IS
/// the record that this input was rejected) plus an informational
/// `claimed` object in its body (ignorable extra JSON — ADR 0039: "unknown
/// object members are ignorable") for operators who want to see what was
/// actually asserted. `controller_id` stays the WIRE-CLAIMED identity
/// (unlike `take_epoch`, `verify.rs` never checks it against anything, and
/// recording who actually attempted the write is more useful than
/// overwriting it with the current holder's name on a REFUSED attempt).
///
/// **Finding 2 (the last-moment recheck's position):** `is_fresh` is
/// computed ONCE, here, before `input` is even committed — and that single
/// computation is what "immediately before the PTY write" (ADR 0041) means
/// in a SINGLE-THREADED, ordered writer loop: this whole function executes
/// as one uninterrupted step of that loop (no other connection's action,
/// no tick, nothing else touches `ctx.holder`/`ctx.take_epoch` between here
/// and `writer.write_all` below), so re-evaluating the same durable state
/// again right before the syscall could only ever reproduce the SAME
/// answer — which the `debug_assert!` beside the syscall states as an
/// explicit invariant rather than leaving implicit. Checking any LATER
/// than here would also break the lattice: the legal refused chain is
/// `{input, refused}` (ADR 0039 lists no `{input, intent, refused}`
/// member), so staleness must be decided BEFORE `forward_intent` is ever
/// committed, not after.
#[allow(clippy::too_many_arguments)]
fn run_input_wal(
    ctx: &mut FrameCtx,
    w: &mut SegmentWriter,
    store: &mut VoyageStore,
    writer: &mut dyn Write,
    frames_written: &mut u64,
    controller_id: &str,
    take_epoch: u64,
    idem_key: [u8; 16],
    payload: &[u8],
    connection_authorized: bool,
) -> Result<InputOutcome> {
    let is_fresh =
        connection_authorized && ctx.holder.as_deref() == Some(controller_id) && take_epoch == ctx.take_epoch;

    // A brand-new idem_key commits `input` (fsync) first, always -- "input
    // is durably logged before the producer sees it" (ADR 0039) applies
    // regardless of whether the write will turn out to be stale.
    let existing = store.dedupe_index.get(&idem_key).copied();
    let input_seq = match existing {
        None => {
            let input = ctx.controller_frame(
                Class::Input,
                controller_id.to_string(),
                ctx.take_epoch,
                json!({"idem_key": hex_idem_key(&idem_key), "content": "redacted", "length": payload.len()}),
            );
            let input_seq = input.seq;
            w.append(&input, Commit::Immediate)?;
            *frames_written += 1;
            store.dedupe_index.insert(
                idem_key,
                DedupeEntry {
                    input: input_seq,
                    state: DedupeState::Input,
                    intent: None,
                },
            );
            input_seq
        }
        Some(DedupeEntry { state: DedupeState::Input, input, .. }) => {
            // Chain = {input}: a same-key retry MUST re-attempt (new
            // intent, SAME input identity) -- ADR 0039's deterministic
            // retry-fold, never a new `input` frame.
            input
        }
        Some(DedupeEntry { state: DedupeState::Intent, .. }) => return Ok(InputOutcome::DeliveryUnknown),
        Some(DedupeEntry { state: DedupeState::Forwarded, .. }) => return Ok(InputOutcome::Recorded),
        Some(DedupeEntry { state: DedupeState::Refused, .. }) => return Ok(InputOutcome::RefusedStale),
    };

    if !is_fresh {
        let mut refused = ctx.controller_frame(
            Class::Lifecycle,
            controller_id.to_string(),
            ctx.take_epoch,
            json!({"kind": "input_fact",
                   "fact": {"input": {"epoch": input_seq.epoch, "n": input_seq.n}, "fact": "refused_stale_epoch"},
                   "claimed": {"controller_id": controller_id, "take_epoch": take_epoch}}),
        );
        refused.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: input_seq }];
        w.append(&refused, Commit::Immediate)?;
        *frames_written += 1;
        if let Some(e) = store.dedupe_index.get_mut(&idem_key) {
            e.state = DedupeState::Refused;
        }
        return Ok(InputOutcome::RefusedStale);
    }

    let mut intent = ctx.controller_frame(
        Class::Lifecycle,
        controller_id.to_string(),
        ctx.take_epoch,
        json!({"kind": "input_fact",
               "fact": {"input": {"epoch": input_seq.epoch, "n": input_seq.n}, "fact": "forward_intent"}}),
    );
    intent.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: input_seq }];
    let intent_seq = intent.seq;
    w.append(&intent, Commit::Immediate)?;
    *frames_written += 1;
    if let Some(e) = store.dedupe_index.get_mut(&idem_key) {
        e.state = DedupeState::Intent;
        e.intent = Some(intent_seq);
    }

    // The "immediately before the PTY write" recheck (finding 2): stated as
    // an assertion, not a second decision branch -- see this function's own
    // doc for why a DIFFERENT answer here is impossible in this
    // single-threaded loop, and why the lattice forbids acting as if it
    // could be (there is no legal `{input, intent, refused}` chain to fall
    // back to).
    debug_assert!(
        connection_authorized && ctx.holder.as_deref() == Some(controller_id) && take_epoch == ctx.take_epoch,
        "durable state changed within one WAL step -- the single-threaded writer-loop invariant was violated"
    );
    writer.write_all(payload)?; // the forward syscall

    let mut fwd = ctx.controller_frame(
        Class::Lifecycle,
        controller_id.to_string(),
        ctx.take_epoch,
        json!({"kind": "input_fact",
               "fact": {"input": {"epoch": input_seq.epoch, "n": input_seq.n}, "fact": "forwarded",
                        "intent": {"epoch": intent_seq.epoch, "n": intent_seq.n}}}),
    );
    fwd.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: input_seq }];
    w.append(&fwd, Commit::Immediate)?;
    *frames_written += 1;
    if let Some(e) = store.dedupe_index.get_mut(&idem_key) {
        e.state = DedupeState::Forwarded;
    }

    Ok(InputOutcome::Recorded)
}

/// Run one producer under a Windows capsule. Blocks until the run ends —
/// either the producer exits on its own, `commands` delivers
/// [`Command::Kill`], or the mgmt lane's `shutdown` drives `Action::Shutdown`
/// once its ack is physically written (ADR 0041 Lifecycle: an externally
/// requested end — EndRun). `commands` mirrors `claude.rs`'s `operator:
/// mpsc::Receiver<OperatorCmd>` parameter; the caller owns its `Sender` (see
/// the module doc's stdin-ownership point). `transport` is the ADR 0041
/// step-5 pipe protocol's seam: a real named pipe on Windows (U3) or a
/// test transport (this unit) drives the SAME [`AttachProto`] state
/// machine either way, polled every iteration via
/// [`Transport::try_recv_event`] — see `attach_proto`'s module doc for the
/// protocol this loop executes.
// unused_assignments: `flush_output!`'s state reset is dead only at its
// FINAL expansion (after the loop) — load-bearing at every other site,
// same allow capsule.rs carries for the identical reason.
#[allow(unused_assignments)]
pub fn run(
    config: CapsuleWinConfig,
    commands: mpsc::Receiver<Command>,
    transport: &mut dyn Transport,
) -> Result<ExitSummary> {
    // Resolve ONCE — see capsule.rs's identical comment on the same call.
    let voyage_root = crate::fsutil::ensure_container(&config.voyage_root)?;
    if !voyage_root.exists() {
        VoyageStore::bootstrap(&voyage_root, &config.voyage_id, config.retention)?;
    }
    // The lease OPEN itself is deferred to inside this closure, so it
    // happens lazily at the exact point `open_prepared` calls it EXACTLY
    // ONCE -- immediately after the writer fence is acquired, before any
    // other pre-fence-adjacent I/O -- never before. A name that cannot
    // even be opened (the supervisor already exited before this child
    // got this far) is folded to `true` (broken) here, matching
    // `crate::lease::open`'s own documented contract: an unopenable
    // lease name is reported identically to an opened-but-broken one,
    // never treated as "no lease was ever passed" (that is `None` below).
    let lease_broken_fn = {
        let name = config.parent_lease_name.clone();
        move || match &name {
            Some(name) => crate::lease::open(name).map(|c| c.is_broken()).unwrap_or(true),
            None => false,
        }
    };
    let lease_broken: Option<&dyn Fn() -> bool> =
        config.parent_lease_name.is_some().then_some(&lease_broken_fn);
    let mut store =
        VoyageStore::open_for_writing_with_lease(&voyage_root, &config.voyage_id, lease_broken)?;

    // Finding 7 (round-1) / round-2 finding 4: `shutdown_all` must run
    // before the writer lock releases (`store`'s own drop) on EVERY exit
    // path from this point on, not only the success one -- an RAII guard
    // is the only way to guarantee that regardless of which `?` returns
    // early below. Declared AFTER `store`: Rust drops locals in REVERSE
    // declaration order, so this guard's `Drop` (closing the pipe) runs
    // BEFORE `store`'s (releasing the lock). Constructed HERE, immediately
    // after `store` itself and BEFORE `seal_survivor()?` -- round-2 review
    // caught the guard originally sitting AFTER that call: a failure
    // there would have returned early with the lock already held (via
    // `store`) but the guard never built, releasing the lock with the
    // pipe still live. Every fallible operation from this point on that
    // runs while the lock is held must stay AFTER the guard, not before
    // it.
    //
    // U1a: this guard is never explicitly DISARMED. The ack-grace call site
    // below calls `transport.0.shutdown_all()` directly once its window
    // resolves, so the pipe disappears promptly rather than staying live
    // through the remaining process-exit wait and seal; THIS Drop then
    // calls it again regardless, on every exit path, exactly as before.
    // That second call is safe only because `Transport::shutdown_all` is
    // now a documented idempotent contract (see that method's own doc) --
    // disarming this guard with a boolean flag would be the OTHER way to
    // make the double call safe, but it is strictly more machinery for the
    // same guarantee an idempotent method already gives for free.
    struct ShutdownGuard<'a>(&'a mut dyn Transport);
    impl Drop for ShutdownGuard<'_> {
        fn drop(&mut self) {
            // This Drop is the FALLBACK path only (an early `?` return
            // before the designed, explicit call at real teardown time
            // ever runs, or that call's own redundant second pass) -- it
            // has no outer aggregate deadline to share, so it computes a
            // fresh one. `run` has ALREADY returned by the time this
            // executes (it is dropping `run`'s own locals during
            // unwind/return), so a `false` here can only be reported
            // loudly (stderr), never turned into `run`'s result.
            if !self.0.shutdown_all(Instant::now() + TEARDOWN_AGGREGATE_DEADLINE) {
                eprintln!(
                    "sot-capsule: ShutdownGuard's fallback teardown did not complete within its \
                     aggregate deadline; a worker thread may still be running"
                );
            }
        }
    }
    let transport = ShutdownGuard(transport);

    // The pipe-lifetime invariant, enforced here by code order (see
    // `Transport::bind`'s own doc): the writer lock is already held
    // (`store`, above) and `ShutdownGuard` is already in place to close
    // whatever `bind` DID manage to set up on any later early return, so
    // `bind` runs before any OTHER fallible step gets a chance to leave
    // the lock held with the transport in a half-set-up state.
    transport.0.bind(&config.voyage_id)?;

    store.seal_survivor()?;

    let mut ctx = FrameCtx {
        epoch: store.epoch,
        next_n: 1,
        t0: Instant::now(),
        take_epoch: 0,
        holder: None,
        attached: None,
    };
    // ADR 0041 "Upgrade and version skew" reader-first rollout gate (see
    // `crate::rollout`): refuse to open ANY segment for this run if the
    // installed rollback target's reader cannot decode one declaring the
    // EndRun-marker feature. Checked once, before the first segment
    // (rotation reuses the SAME declared set — a run's declared features
    // are its own commitment for its whole life, not renegotiated
    // segment to segment).
    crate::rollout::gate(
        &config.rollout_evidence,
        RUN_END_REQUESTED_FEATURE,
    )?;
    // Every segment a step-6 capsule opens declares the EndRun-marker
    // feature UNCONDITIONALLY (ADR 0041 Lifecycle: "a feature cannot be
    // added to an immutable header later and the marker's timing is not
    // knowable in advance").
    let segment_features = vec![RUN_END_REQUESTED_FEATURE.to_string()];
    let mut w = store.open_segment_with_features(wall_ms(), segment_features.clone())?;
    let mut seg_bytes: u64 = 0;
    let mut frames_written: u64 = 0;
    let mut segments_sealed: u64 = 0;

    // Control preamble — every frame here commits immediately. The step-4
    // "local" take grant is DELETED (ADR 0041 step-5 spec gate): this stops
    // after the null-holder revoke. The first driver ever is a pipe `take`
    // (`Action::CommitTake`, below).
    let prior_take = store.last_take_epoch;
    ctx.take_epoch = prior_take + 1;
    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "take_state", "take": {"take_epoch": ctx.take_epoch, "holder": null}}),
    );
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;

    // The attach protocol: platform-neutral state machine (`attach_proto`);
    // `pid`/`created` are OS values it must never compute itself.
    let mut attach_proto = AttachProto::new(self_status(config.survival)?);
    let mut splitters: HashMap<ConnId, wire::FrameSplitter> = HashMap::new();
    // Finding 11: keyed by (conn, id), not id alone -- a transport's send
    // ids are only ever meaningful scoped to the connection that issued
    // them (a real transport may recycle ids across connections), and
    // every entry for a connection is purged the moment it closes (see
    // `execute_light_actions!`'s `Close` arm), so a canceled write can
    // never leak an entry, nor can a stale/mismatched completion apply a
    // marker meant for a connection that no longer exists.
    let mut pending_sends: HashMap<(ConnId, u64), Option<SentMarker>> = HashMap::new();
    let mut shutdown_requested = false;
    let mut shutdown_reason: Option<String> = None;
    // ADR 0041 EndRun step 2: IRREVOCABLE once true — never unset by
    // anything past this point (a stalled ack, a stopped-reading client,
    // a progress-deadline close, or a lost connection). Distinct from
    // `shutdown_requested`, which governs when TEARDOWN starts (only
    // once the ack ships); this one governs whether the durable marker
    // has already been committed, so a second concurrent `shutdown`
    // request writes no second frame (step 4).
    let mut run_end_latched = false;

    // producer_attached: the raw-terminal redaction profile, content-hashed
    // — identical to capsule.rs (this is a cross-platform semantic, not a
    // Linux one).
    let rules = json!({"input_content": "redacted", "turns": "none"});
    let rules_bytes = serde_json::to_vec(&rules)?;
    let rules_sha = {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(&rules_bytes);
        h.finalize().iter().map(|b| format!("{:02x}", b)).collect::<String>()
    };
    let attached_seq_holder = ctx.capsule_frame(
        Class::ProducerAttached,
        json!({
            "producer_kind": config.producer_kind,
            "version": "raw-pty-1",
            "profile_def": {"id": "raw-terminal-default", "sha256": rules_sha, "rules": rules},
        }),
    );
    let attached_seq = attached_seq_holder.seq;
    w.append(&attached_seq_holder, Commit::Immediate)?;
    frames_written += 1;
    ctx.attached = Some(attached_seq);

    // producer_spawn commits BEFORE spawn is even attempted — the
    // spawn-failure compensation path (below) depends on this already
    // being on the wire. `spawning_process_was_jobbed` is observed here,
    // independent of spawn's own outcome (see `observe_spawning_process_jobbed`'s
    // doc) — `SpawnDetail`, only available after a SUCCESSFUL spawn, would
    // be too late for a failure to fold in.
    let spawning_process_was_jobbed = observe_spawning_process_jobbed();
    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "producer_spawn",
               "detail": {"argv": config.argv, "spawning_process_was_jobbed": spawning_process_was_jobbed}}),
    );
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;

    // Initial geometry validated by the SAME rule a resize is (ADR 0041).
    // An out-of-budget request here is treated exactly like a spawn
    // failure: nothing was ever created, so the same compensation path
    // applies — no separate code path needed for "never even tried".
    let geometry_ok =
        (MIN_COLS..=MAX_COLS).contains(&config.cols) && (MIN_ROWS..=MAX_ROWS).contains(&config.rows);
    let spawn_result = if !geometry_ok {
        Err(format!(
            "initial geometry {}x{} outside the 2x2..512x256 budget",
            config.cols, config.rows
        ))
    } else {
        ConptySpawn::spawn(&config.argv, config.cols, config.rows).map_err(|e| e.to_string())
    };

    let spawn = match spawn_result {
        Ok(s) => s,
        Err(reason) => {
            // Compensation: the producer never ran, but a real spawn
            // attempt was already recorded above — close the run out
            // honestly instead of the Linux capsule's known bare-`?`
            // escape (which seals nothing). No pty/job exists to tear
            // down; this is the one path that reaches producer_dead
            // without ever having a ConptySpawn.
            let f = ctx.capsule_frame(
                Class::Lifecycle,
                json!({"kind": "producer_dead",
                       "detail": {"exit_code": null, "spawn_failed": true, "reason": reason}}),
            );
            w.append(&f, Commit::Immediate)?;
            frames_written += 1;
            let digest = w.seal(None)?;
            store.advance_chain(digest);
            segments_sealed += 1;
            return Ok(ExitSummary {
                exit_code: None,
                exit_kind: ExitKind::SpawnFailed,
                frames_written,
                segments_sealed,
                handshake_answered: false,
                handshake_suppressed_matches: 0,
                resize_os_calls: 0,
            });
        }
    };
    // N1 (Codex review round 3): the supervisor's own anti-flap counter
    // must judge stability on the PRODUCER's lifetime, never on how long
    // this capsule process's own teardown (job reap, ConPTY drain,
    // aggregate deadline, final wait) happens to take afterward -- those
    // are all supervisor-invisible-until-exit timers that can alone
    // exceed the stability interval regardless of how long the producer
    // itself actually ran. Captured HERE, the instant a real spawn
    // succeeds (not before the attempt, which would count spawn latency
    // itself as producer uptime) -- `Instant` is `Copy`, so this survives
    // unmoved all the way to the `producer_dead` frame far below.
    let spawned_at = Instant::now();

    // Destructure rather than keep `spawn` around: partial moves out of a
    // Drop-less struct (conpty.rs pins `ConptySpawn` as deliberately
    // Drop-less) are fine, and this avoids repeating `spawn.` everywhere.
    // `pid`/`detail` are dropped: `detail` duplicates the pre-spawn
    // jobbed-observation already committed above (see conpty.rs's own
    // doc — the value is identical either way), and `pid` has no use in
    // this unit (nothing here re-opens the process by PID).
    let ConptySpawn {
        job,
        process,
        pty,
        reader,
        mut writer,
        pid: _,
        detail: _,
    } = spawn;

    // Live vt100 parser (ADR 0041 "Terminal state") — kept current for a
    // later attach (step 5) to checkpoint from; this unit never serializes
    // it. Bounded scrollback (`CAPSULE_SCROLLBACK_ROWS`): a local capsule
    // pane could not be scrolled after attach, because every checkpoint
    // carried the visible screen only and the client's own ring started
    // empty on every attach. The ring rides in the checkpoint now (vt100
    // fork format version 2) and a restore REPLACES the client's ring
    // rather than appending, so re-attaching does not double it.
    let mut parser = vt100_ctt::Parser::new(config.rows, config.cols, CAPSULE_SCROLLBACK_ROWS);
    let mut handshake = HostHandshake::new();
    // ADR 0041's model is ONE host handshake, at startup — answer and
    // record only the first match ever observed; count the rest (the
    // amplification fix, review finding).
    let mut dsr_answered = false;
    let mut handshake_suppressed_matches: u64 = 0;
    let mut resize_os_calls: u64 = 0;

    // The output budget, and the guard that cancels it on ANY exit from
    // this function from this point on — declared as early as the budget
    // itself so an early `?` anywhere below unwinds through it.
    let output_budget = Arc::new(OutputBudget::new());
    let _budget_guard = BudgetCancelGuard(Arc::clone(&output_budget));

    // The reader thread is the ONLY sender on this channel — no bridging
    // threads for input/control any more (see the module doc): `commands`
    // is serviced directly, by this loop, from its own separate receiver.
    let (tx, output_rx) = mpsc::channel::<ReaderEvent>();
    let reader_handle = {
        let budget = Arc::clone(&output_budget);
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; READ_CHUNK];
            loop {
                if !budget.reserve(READ_CHUNK as u64) {
                    // Cancelled: `run` is already exiting some other way.
                    // Nothing left to report; just stop.
                    return;
                }
                match reader.read(&mut buf) {
                    Ok(0) => {
                        budget.release(READ_CHUNK as u64);
                        let _ = tx.send(ReaderEvent::Done(Ok(())));
                        return;
                    }
                    Err(e) => {
                        budget.release(READ_CHUNK as u64);
                        let _ = tx.send(ReaderEvent::Done(Err(e)));
                        return;
                    }
                    Ok(n) => {
                        let n = n as u64;
                        if n < READ_CHUNK as u64 {
                            budget.release(READ_CHUNK as u64 - n);
                        }
                        if tx.send(ReaderEvent::Output(buf[..n as usize].to_vec())).is_err() {
                            budget.release(n);
                            return;
                        }
                    }
                }
            }
        })
    };

    let mut pending_output: Vec<u8> = Vec::new();
    let mut pending_bytes: usize = 0;
    let mut last_commit = Instant::now();
    // Loop-fairness MITIGATION, not a guarantee (real CI failure, windows-
    // latest only: `attach_mid_stream_checkpoint_reproduces_reference_
    // screen` timed out waiting on a connection the capsule itself closed
    // with `PreAdmissionTimeout`, even though the test's `hello` had
    // already arrived on the wire). Root cause: `output_rx.recv_timeout`
    // below never actually blocks -- and so never yields this thread --
    // for as long as output keeps arriving faster than the timeout; on a
    // CPU-constrained runner with a fast enough producer (windows-latest's
    // conhost measured ~2x windows-2022's), that starves whatever OS
    // thread delivers transport bytes for this connection, long enough for
    // the unrelated `PRE_ADMISSION_TIMEOUT` deadline to fire on a `hello`
    // the loop never got a chance to even see.
    //
    // Round-2 review, finding 10 (the fairness claim below was overstated):
    // `pace_output!` sleeps 1 ms per `GROUP_COMMIT_BYTES` of output
    // processed. The first version used `yield_now` (`SwitchToThread`),
    // which offers a ready thread ON THE CURRENT PROCESSOR a chance and
    // may return without switching -- and the starvation it was meant to
    // cure RECURRED on the faster CI image: with two hot threads (this
    // loop + the ConPTY reader) on a two-core runner, the thread that
    // must run to DELIVER a transport event never got a core, and
    // `PreAdmissionTimeout` fired on a hello that had already been sent.
    // A timed sleep is a real scheduling point on every Windows build:
    // the OS will run other ready threads for the duration. The cost is
    // bounded and named: 1 ms per 256 KiB caps output throughput near
    // 250 MiB/s -- two orders of magnitude above anything conhost
    // delivers -- and at real delivery rates the pacer fires rarely.
    // Nothing about protocol ordering or durability depends on this:
    // `service_transport_events!`/`tick` already run once per iteration,
    // every write is fsynced before it's published (the watermark
    // barrier), and `attach_proto`'s replay tests prove the protocol
    // correct independent of timing.
    //
    // No deterministic unit test pins this one: `output_rx` (and the
    // reader thread feeding it) is constructed a few lines below, entirely
    // INSIDE this function, over a real spawned ConPTY -- unlike
    // `commands`/`transport`, it is not a parameter a test can substitute
    // a synthetic, saturated source for. The two real
    // Windows CI legs (windows-2022, windows-latest) are the pin for this
    // specific behavior until `run` grows an injectable output source
    // worth the refactor.
    let mut bytes_since_yield: usize = 0;

    // THIS module decides nothing; `attach_proto::AttachProto` does (see
    // its module doc). `execute_light_actions!` runs the action kinds that
    // `output_committed`/`ground_reached`/`checkpoint_ready` can ever
    // produce (proven by `attach_proto`'s own implementation: never
    // `CommitTake`/`ForwardInput`/`ApplyResize`/`Shutdown`) -- kept SEPARATE
    // from the full `execute_actions!` below rather than one macro calling
    // itself, because `flush_output!` needs to run actions too, and
    // `flush_output!` is itself called FROM `execute_actions!`'s
    // `CommitTake`/`ApplyResize` arms: a macro invoking itself through that
    // path is not runtime recursion (which would be fine) but INFINITE
    // COMPILE-TIME macro expansion (`recursion limit reached`, hit and
    // fixed while building this unit) -- every match arm is expanded
    // unconditionally at compile time, `flush_output!`'s body included,
    // regardless of which arm ever actually runs. Splitting the acyclic
    // subset out breaks the cycle: `execute_light_actions!` calls nothing
    // else here; `flush_output!` calls only `execute_light_actions!`;
    // `execute_actions!` calls `flush_output!`, `maybe_rotate!`, and (for
    // its own light-kind actions) `execute_light_actions!` -- all strictly
    // "downward", never back.
    macro_rules! execute_light_actions {
        ($seed:expr) => {{
            let mut queue: VecDeque<AttachAction> = VecDeque::from($seed);
            while let Some(action) = queue.pop_front() {
                match action {
                    AttachAction::Send { conn, frame_bytes, marker } => {
                        let id = transport.0.send(conn, frame_bytes);
                        // Round-2 review, finding 7: the Transport contract
                        // (this trait's own doc) requires every outstanding
                        // (conn, id) to be unique -- a reused id before its
                        // predecessor's completion is reported would
                        // silently resolve the WRONG marker for a later
                        // `Sent`. A transport that violates this has a bug
                        // in it, not something this loop can route around.
                        let prior = pending_sends.insert((conn, id), marker);
                        assert!(
                            prior.is_none(),
                            "Transport::send returned (conn={conn:?}, id={id}) while a previous send with the \
                             SAME id was still outstanding -- violates the unique-outstanding-id contract"
                        );
                    }
                    AttachAction::Close(conn) => {
                        transport.0.close(conn);
                        splitters.remove(&conn);
                        // Finding 11: purge every pending send this
                        // connection still had outstanding -- a canceled
                        // write's completion, if the transport ever
                        // reported one anyway, must find nothing to apply
                        // a marker to.
                        pending_sends.retain(|&(c, _), _| c != conn);
                        queue.extend(attach_proto.connection_closed(conn, Instant::now()));
                    }
                    AttachAction::RecordRefusal { conn, reason } => {
                        // Diagnostic only -- no wire frame exists for most
                        // of these refusals, by design (ADR 0041 decision
                        // 5: queue overflow has none at all).
                        eprintln!("sot-capsule: attach protocol refusal conn={conn:?} reason={reason:?}");
                    }
                    AttachAction::BeginCheckpoint { conn } => {
                        // A connection that negotiated attach proto v1
                        // gets a checkpoint format v1 payload -- no
                        // scrollback ring -- regardless of the capsule's
                        // own live ring capacity: an old client's own
                        // vt100 fork build refuses anything newer
                        // outright, and its own pinned
                        // `wire::MAX_CHECKPOINT_LEN` predates the ring
                        // too (Codex round on #194, finding 1). Never
                        // silently downgrade a v2 connection; only ever
                        // an explicitly negotiated v1 one gets the
                        // legacy shape.
                        let legacy = attach_proto.negotiated_proto(conn) == wire::ATTACH_PROTO_V1;
                        let bytes = if legacy {
                            parser.screen().checkpoint_at_version(LEGACY_CHECKPOINT_VERSION)
                        } else {
                            parser.screen().checkpoint()
                        }
                        .expect(
                            "geometry is bounded to 2x2..512x256, always representable at that range (ADR 0041)",
                        );
                        queue.extend(attach_proto.checkpoint_ready(conn, bytes, Instant::now()));
                    }
                    other => unreachable!(
                        "execute_light_actions!: {other:?} is not one output_committed/ground_reached/checkpoint_ready can produce"
                    ),
                }
            }
        }};
    }

    macro_rules! flush_output {
        ($w:expr) => {
            if pending_bytes > 0 {
                $w.commit()?; // the watermark: fsync BEFORE anything is published
                execute_light_actions!(attach_proto.output_committed(&pending_output, Instant::now()));
                pending_output.clear();
                pending_bytes = 0;
            }
            last_commit = Instant::now();
            // ADR 0041: attach is GROUND-GATED; the watermark barrier
            // (force pending commit -> publish to EXISTING subscribers ->
            // checkpoint -> subscribe) is exactly this ordering -- publish
            // above already ran, so a ground boundary found HERE is the
            // single loop step the barrier requires.
            if parser.is_ground() {
                execute_light_actions!(attach_proto.ground_reached(Instant::now()));
            }
        };
    }

    macro_rules! maybe_rotate {
        ($w:ident) => {
            if seg_bytes >= SEGMENT_MAX_BYTES {
                flush_output!($w);
                let digest = $w.seal(None)?;
                store.advance_chain(digest);
                segments_sealed += 1;
                $w = store.open_segment_with_features(wall_ms(), segment_features.clone())?;
                seg_bytes = 0;
            }
        };
    }

    /// Bounds consecutive output work to one `GROUP_COMMIT_BYTES` worth
    /// (the SAME threshold the writer already paces its own fsyncs by)
    /// before yielding this thread -- the loop-fairness fix above. Takes
    /// `bytes` itself (not a pre-computed length) so every call site reads
    /// as "handle this chunk, paced" in one line: `.len()` borrows before
    /// `handle_output!` moves it.
    /// Offers a scheduling window to another ready thread every
    /// `GROUP_COMMIT_BYTES` worth of output -- a timed sleep (see
    /// the doc above `bytes_since_yield`), not a bound: it may do nothing.
    macro_rules! pace_output {
        ($bytes:ident) => {
            bytes_since_yield += $bytes.len();
            handle_output!($bytes);
            if bytes_since_yield >= GROUP_COMMIT_BYTES {
                std::thread::sleep(Duration::from_millis(1));
                bytes_since_yield = 0;
            }
        };
    }

    /// Attaching to an idle session (real CI failure, windows-latest
    /// only): `ground_reached` was previously fed ONLY from
    /// `flush_output!`, itself reached only by fresh output crossing the
    /// group-commit threshold, or a periodic idle check gated behind the
    /// OUTPUT CHANNEL's own `recv_timeout` cadence — never directly by
    /// admission, and never by `tick`, the one hook this loop already
    /// calls unconditionally every iteration. An attach landing on an
    /// ALREADY-idle, already-at-ground session (a shell sitting at its
    /// prompt — the ordinary case, exercised once the fidelity test's
    /// producer goes silent after `--linger`) depended entirely on that
    /// separate cadence happening to notice, which is exactly the kind of
    /// dependency `pace_output!`'s own history above already proved
    /// fragile on a loaded windows-latest runner: the attach pended for
    /// the full 5 s `GroundTimeout` and was refused instead of completing
    /// on the very next iteration.
    ///
    /// Called every iteration, right after `tick`, so it runs in the SAME
    /// iteration an attach was just admitted in (a) and on every
    /// subsequent iteration while one still pends (b) — no separate
    /// cadence to depend on. Scoped behind `ground_gate_pending()` (a
    /// cheap check) so the vastly more common "nothing pending" iteration
    /// pays nothing beyond it. Watermark semantics stay exact: with no
    /// pending uncommitted bytes, NOW already is a valid commit boundary
    /// (`flush_output!` skips the commit but still evaluates ground); with
    /// some pending, `flush_output!` forces the SAME commit-then-check
    /// barrier it always runs, just immediately rather than waiting for
    /// the group-commit threshold or the idle timer to get to it.
    macro_rules! eager_ground_check {
        () => {
            if attach_proto.ground_gate_pending() {
                flush_output!(w);
            }
        };
    }

    // The full action set -- everything `execute_light_actions!` handles,
    // delegated one line at a time (never re-expanding `flush_output!`
    // itself), PLUS the five action kinds only an inbound CLIENT frame can
    // ever produce.
    macro_rules! execute_actions {
        ($seed:expr) => {{
            let mut queue: VecDeque<AttachAction> = VecDeque::from($seed);
            while let Some(action) = queue.pop_front() {
                match action {
                    light @ (AttachAction::Send { .. }
                    | AttachAction::Close(_)
                    | AttachAction::RecordRefusal { .. }
                    | AttachAction::BeginCheckpoint { .. }) => {
                        execute_light_actions!(vec![light]);
                    }
                    AttachAction::CommitTake { conn, controller_id, request_id } => {
                        flush_output!(w);
                        ctx.take_epoch += 1;
                        ctx.holder = Some(controller_id.clone());
                        let f = ctx.capsule_frame(
                            Class::Lifecycle,
                            json!({"kind": "take_state",
                                   "take": {"take_epoch": ctx.take_epoch, "holder": controller_id}}),
                        );
                        w.append(&f, Commit::Immediate)?;
                        frames_written += 1;
                        queue.extend(attach_proto.take_committed(conn, ctx.take_epoch, request_id, Instant::now()));
                    }
                    AttachAction::ForwardInput {
                        conn,
                        controller_id,
                        take_epoch,
                        idem_key,
                        payload,
                        connection_authorized,
                        request_id,
                    } => {
                        let outcome = run_input_wal(
                            &mut ctx,
                            &mut w,
                            &mut store,
                            &mut writer,
                            &mut frames_written,
                            &controller_id,
                            take_epoch,
                            idem_key,
                            &payload,
                            connection_authorized,
                        )?;
                        maybe_rotate!(w);
                        queue.extend(attach_proto.input_outcome(conn, outcome, request_id, Instant::now()));
                    }
                    AttachAction::ApplyResize { conn, cols, rows, request_id } => {
                        // ADR 0041: "resize (driver-only) routes into the
                        // step-4 exchange unchanged" -- same ordered
                        // request -> one ResizePseudoConsole call (skipped
                        // if out of budget) -> parser/geometry updated
                        // only on success -> outcome shape step 4 already
                        // built, now reachable from the wire too.
                        flush_output!(w);
                        let req = ctx.current_controller_frame(
                            Class::ControlExchange,
                            json!({"phase": "request", "kind_ns": "conpty/resize",
                                   "to": {"kind": "producer"}, "body": {"cols": cols, "rows": rows}}),
                        );
                        let req_seq = req.seq;
                        w.append(&req, Commit::Immediate)?;
                        frames_written += 1;
                        let in_budget =
                            (MIN_COLS..=MAX_COLS).contains(&cols) && (MIN_ROWS..=MAX_ROWS).contains(&rows);
                        let ok = if !in_budget {
                            false
                        } else {
                            resize_os_calls += 1;
                            match pty.resize(cols, rows) {
                                Ok(()) => {
                                    parser.screen_mut().set_size(rows, cols);
                                    true
                                }
                                Err(_) => false,
                            }
                        };
                        let outcome_body = if ok {
                            json!({"disposition": "ok", "cols": cols, "rows": rows})
                        } else {
                            json!({"disposition": "failed", "cols": cols, "rows": rows,
                                   "reason": "outside the 2x2..512x256 budget, or ResizePseudoConsole failed"})
                        };
                        let out = ctx.current_controller_frame(
                            Class::ControlExchange,
                            json!({"phase": "outcome", "kind_ns": "conpty/resize", "scope": "pty",
                                   "target": format!("{}:{}", req_seq.epoch, req_seq.n), "body": outcome_body}),
                        );
                        w.append(&out, Commit::Immediate)?;
                        frames_written += 1;
                        maybe_rotate!(w);
                        queue.extend(attach_proto.resize_outcome(conn, ok, request_id, Instant::now()));
                    }
                    AttachAction::RunEndRequested { reason } => {
                        // Codex round-1 Blocker 1 discharge: record the
                        // reason HERE, from the marker's own commit -- not
                        // only from `Action::Shutdown` (ack-completion-
                        // driven), which may never fire at all (a stalled
                        // ack, a lost connection). `get_or_insert_with`
                        // matches "first commit wins" (step 4): a
                        // concurrent second request's reason never
                        // overwrites the one that actually got latched.
                        shutdown_reason.get_or_insert_with(|| reason.clone());
                        commit_run_end_marker(&mut ctx, &mut w, &mut frames_written, &mut run_end_latched, reason)?;
                    }
                    AttachAction::Shutdown { reason } => {
                        shutdown_requested = true;
                        shutdown_reason.get_or_insert(reason);
                    }
                }
            }
        }};
    }

    // Drains every currently-available transport event (non-blocking, like
    // `commands.try_recv()` below) through `AttachProto`, executing
    // whatever it decides. Called every MAIN-LOOP iteration only: once this
    // loop is left for teardown, the wire lane's admission is revoked at
    // the SAME boundary `commands` already is (`pty` is also moved into the
    // Phase-B closer thread by then, so a wire-triggered resize could not
    // run even if admitted).
    // Per-pass event quota: a client flood can refill the bounded transport
    // channel as fast as this loop drains it, and an UNBOUNDED while-let
    // would then starve output commits, tick, and the exit checks
    // indefinitely (review finding). The quota bounds one pass; the next
    // loop iteration resumes immediately, so nothing is dropped -- only
    // interleaved.
    const TRANSPORT_EVENTS_PER_PASS: usize = 64;
    macro_rules! service_transport_events {
        () => {
            let mut quota = TRANSPORT_EVENTS_PER_PASS;
            while quota > 0 {
                quota -= 1;
                let Some(ev) = transport.0.try_recv_event() else { break };
                match ev {
                    TransportEvent::ConnectionOpened(conn) => {
                        splitters.insert(conn, wire::FrameSplitter::new());
                        execute_actions!(attach_proto.connection_opened(conn, Instant::now()));
                    }
                    TransportEvent::Bytes(conn, bytes) => {
                        let Some(splitter) = splitters.get_mut(&conn) else { continue };
                        let (frames, err) = splitter.feed(&bytes);
                        for f in frames {
                            execute_actions!(attach_proto.frame(conn, f, Instant::now()));
                        }
                        if err.is_some() {
                            transport.0.close(conn);
                            splitters.remove(&conn);
                            pending_sends.retain(|&(c, _), _| c != conn); // finding 11
                            execute_actions!(attach_proto.connection_closed(conn, Instant::now()));
                        }
                    }
                    TransportEvent::ConnectionClosed(conn) => {
                        splitters.remove(&conn);
                        pending_sends.retain(|&(c, _), _| c != conn); // finding 11
                        execute_actions!(attach_proto.connection_closed(conn, Instant::now()));
                    }
                    TransportEvent::Sent(conn, id) => {
                        match pending_sends.remove(&(conn, id)) {
                            Some(marker) => execute_actions!(attach_proto.sent(conn, marker, Instant::now())),
                            // Round-2 review, finding 7: legitimate ONLY
                            // for a connection this loop already forgot
                            // (closed, `pending_sends` purged by finding
                            // 11's own retain) -- a late completion racing
                            // the close. For a connection STILL active
                            // (still in `splitters`), an unmatched `Sent`
                            // is a transport contract violation: a
                            // duplicate completion, or one for an id never
                            // actually issued.
                            None => assert!(
                                !splitters.contains_key(&conn),
                                "Transport reported Sent({conn:?}, {id}) for an ACTIVE connection with no \
                                 matching outstanding send"
                            ),
                        }
                    }
                    // Round-2 e2e review, finding 4: a terminal transport
                    // failure gets the SAME orderly self-end as an
                    // externally requested EndRun -- no future connection
                    // can ever be admitted, so continuing to run would
                    // leave this capsule silently unreachable forever.
                    TransportEvent::TransportFatal(detail) => {
                        eprintln!(
                            "sot-capsule: transport reported a terminal failure, ending this run: {detail}"
                        );
                        shutdown_requested = true;
                        shutdown_reason = Some("transport-accept-failed".to_string());
                    }
                }
            }
        };
    }

    // Finding 7: producer-bound admission is revoked once EndRun begins
    // (`AttachProto::begin_teardown`), but mgmt (`probe`/`status`) and
    // `Sent` completions must keep being serviced through BOTH teardown
    // phases, until the pipe is explicitly closed -- step 6's adoption
    // status-challenge premise depends on it ("revoke admission" applies to
    // producer-bound input/resize/take, never to mgmt status/probe). This
    // is the teardown-safe action executor: every "light" action
    // (Send/Close/RecordRefusal/BeginCheckpoint -- none of which need
    // `pty`, already moved into the Phase-B closer thread by the time this
    // runs there) delegates to `execute_light_actions!`; `RunEndRequested`/
    // `Shutdown` (a second EndRun request racing the first) are harmless
    // (idempotent past the first marker); `CommitTake`/
    // `ForwardInput`/`ApplyResize` are asserted UNREACHABLE --
    // `begin_teardown` guarantees `AttachProto` never emits them again at
    // the SOURCE, so this is a documented invariant enforced loudly, not a
    // live code path (which could not exist here regardless: `pty` is not
    // even in scope during Phase B).
    macro_rules! execute_teardown_actions {
        ($seed:expr) => {{
            let mut queue: VecDeque<AttachAction> = VecDeque::from($seed);
            while let Some(action) = queue.pop_front() {
                match action {
                    light @ (AttachAction::Send { .. }
                    | AttachAction::Close(_)
                    | AttachAction::RecordRefusal { .. }
                    | AttachAction::BeginCheckpoint { .. }) => {
                        execute_light_actions!(vec![light]);
                    }
                    AttachAction::RunEndRequested { reason } => {
                        // A `shutdown` admitted during the final teardown
                        // poll (ADR 0041 EndRun step 4's "accepted in the
                        // final service poll" case) still latches the SAME
                        // way -- mgmt keeps being serviced through both
                        // teardown phases (finding 7), and this is the one
                        // place that knows whether the marker already
                        // committed. Same reason-recording discipline as
                        // the main loop's own arm (Codex round-1 Blocker 1).
                        shutdown_reason.get_or_insert_with(|| reason.clone());
                        commit_run_end_marker(&mut ctx, &mut w, &mut frames_written, &mut run_end_latched, reason)?;
                    }
                    AttachAction::Shutdown { reason } => {
                        // Round-2 review deletion residue: `shutdown_requested`
                        // is only ever READ inside the main `'main: loop`
                        // (the `if shutdown_requested { break 'main ... }`
                        // check) -- which has already exited by the time
                        // `execute_teardown_actions!` ever runs. Setting it
                        // here was dead. `shutdown_reason` still matters: a
                        // second, teardown-time `Shutdown` (a racing EndRun
                        // request) still gets its own reason string folded
                        // into `producer_dead`'s eventual detail -- UNLESS
                        // an earlier request's reason (via
                        // `RunEndRequested`, above) already won (first
                        // commit wins, ADR 0041 step 4): `get_or_insert`,
                        // not an unconditional overwrite.
                        shutdown_reason.get_or_insert(reason);
                    }
                    other @ (AttachAction::CommitTake { .. }
                    | AttachAction::ForwardInput { .. }
                    | AttachAction::ApplyResize { .. }) => {
                        unreachable!("AttachProto must never emit {other:?} once begin_teardown() has run");
                    }
                }
            }
        }};
    }

    /// As `service_transport_events!`, but dispatching through
    /// `execute_teardown_actions!` -- used by BOTH teardown phases so mgmt
    /// traffic and `Sent` completions keep flowing right up until the pipe
    /// is closed (finding 7).
    macro_rules! service_transport_events_teardown {
        () => {
            // Same per-pass quota as the main loop's macro, same reason --
            // teardown's own deadlines must not be defeatable by a client
            // flood refilling the channel mid-drain (review finding).
            let mut quota = TRANSPORT_EVENTS_PER_PASS;
            while quota > 0 {
                quota -= 1;
                let Some(ev) = transport.0.try_recv_event() else { break };
                match ev {
                    TransportEvent::ConnectionOpened(conn) => {
                        splitters.insert(conn, wire::FrameSplitter::new());
                        execute_teardown_actions!(attach_proto.connection_opened(conn, Instant::now()));
                    }
                    TransportEvent::Bytes(conn, bytes) => {
                        let Some(splitter) = splitters.get_mut(&conn) else { continue };
                        let (frames, err) = splitter.feed(&bytes);
                        for f in frames {
                            execute_teardown_actions!(attach_proto.frame(conn, f, Instant::now()));
                        }
                        if err.is_some() {
                            transport.0.close(conn);
                            splitters.remove(&conn);
                            pending_sends.retain(|&(c, _), _| c != conn);
                            execute_teardown_actions!(attach_proto.connection_closed(conn, Instant::now()));
                        }
                    }
                    TransportEvent::ConnectionClosed(conn) => {
                        splitters.remove(&conn);
                        pending_sends.retain(|&(c, _), _| c != conn);
                        execute_teardown_actions!(attach_proto.connection_closed(conn, Instant::now()));
                    }
                    TransportEvent::Sent(conn, id) => {
                        match pending_sends.remove(&(conn, id)) {
                            Some(marker) => execute_teardown_actions!(attach_proto.sent(conn, marker, Instant::now())),
                            // Finding 7, same reasoning as the main loop's
                            // identical arm: tolerated only for a
                            // connection already closed.
                            None => assert!(
                                !splitters.contains_key(&conn),
                                "Transport reported Sent({conn:?}, {id}) for an ACTIVE connection with no \
                                 matching outstanding send"
                            ),
                        }
                    }
                    TransportEvent::TransportFatal(detail) => {
                        // Round-2 e2e review, finding 4, teardown-phase
                        // analog of `AttachAction::Shutdown`'s own
                        // teardown-time arm just above: `shutdown_requested`
                        // is dead here (already left `'main`), but a fatal
                        // transport failure arriving DURING teardown still
                        // deserves its own reason folded into the eventual
                        // `producer_dead` detail -- unless a real reason is
                        // already recorded (the run is ending for some
                        // OTHER cause; don't overwrite it with a fatal
                        // event that is likely just this SAME pipe closing
                        // as a side effect of that other teardown).
                        eprintln!(
                            "sot-capsule: transport reported a terminal failure during teardown: {detail}"
                        );
                        shutdown_reason.get_or_insert_with(|| "transport-accept-failed".to_string());
                    }
                }
            }
        };
    }

    // U1a Codex round-1, Major 6 discharge: the ack-grace window's own
    // drain -- STOP ADMITTING new connections or new request bytes once
    // the final ordinary teardown poll is behind us, so a request that
    // slips in with, say, 50ms left in the grace can never be credited
    // with the full 2s the "final service poll" guarantee actually
    // promises. `Sent`/`ConnectionClosed` still drain normally (the whole
    // POINT of the grace is letting an ALREADY-QUEUED ack finish); a brand
    // new `ConnectionOpened` or a new `Bytes` payload on an existing
    // connection is closed outright, WITHOUT ever reaching `attach_proto`
    // -- no admission, so no new obligation this bounded window cannot
    // keep.
    macro_rules! drain_pending_sends_only {
        () => {
            let mut quota = TRANSPORT_EVENTS_PER_PASS;
            while quota > 0 {
                quota -= 1;
                let Some(ev) = transport.0.try_recv_event() else { break };
                match ev {
                    TransportEvent::ConnectionOpened(conn) => {
                        // Never admitted: no splitter, no `attach_proto`
                        // event, just closed.
                        transport.0.close(conn);
                    }
                    TransportEvent::Bytes(conn, _bytes) => {
                        // A connection admitted during ORDINARY teardown
                        // (before the grace began) sending more bytes now:
                        // still no new admission -- close it, purging
                        // whatever this loop already tracked for it.
                        transport.0.close(conn);
                        splitters.remove(&conn);
                        pending_sends.retain(|&(c, _), _| c != conn);
                        execute_teardown_actions!(attach_proto.connection_closed(conn, Instant::now()));
                    }
                    TransportEvent::ConnectionClosed(conn) => {
                        splitters.remove(&conn);
                        pending_sends.retain(|&(c, _), _| c != conn);
                        execute_teardown_actions!(attach_proto.connection_closed(conn, Instant::now()));
                    }
                    TransportEvent::Sent(conn, id) => {
                        match pending_sends.remove(&(conn, id)) {
                            Some(marker) => execute_teardown_actions!(attach_proto.sent(conn, marker, Instant::now())),
                            None => assert!(
                                !splitters.contains_key(&conn),
                                "Transport reported Sent({conn:?}, {id}) for an ACTIVE connection with no \
                                 matching outstanding send"
                            ),
                        }
                    }
                    TransportEvent::TransportFatal(detail) => {
                        eprintln!(
                            "sot-capsule: transport reported a terminal failure during the shutdown-ack grace: {detail}"
                        );
                        shutdown_reason.get_or_insert_with(|| "transport-accept-failed".to_string());
                    }
                }
            }
        };
    }

    // One producer-output handler, used identically pre-teardown AND
    // during BOTH teardown phases — so "the handshake keeps answering
    // through the drain" (ADR 0041) can't be missed by one call site and
    // not the other. Feeds the live parser, answers the FIRST DA1 query
    // ever seen (recording request -> response -> outcome), records the
    // raw producer frame, and tracks the group-commit/echo state.
    macro_rules! handle_output {
        ($bytes:expr) => {{
            let bytes = $bytes;
            parser.process(&bytes);

            use base64_engine::encode_b64;
            let f = ctx.producer_frame(json!({"bytes_b64": encode_b64(&bytes)}));
            w.append(&f, Commit::Buffered)?;
            frames_written += 1;
            seg_bytes += bytes.len() as u64 + 128;
            output_budget.release(bytes.len() as u64);
            pending_output.extend_from_slice(&bytes);
            pending_bytes += bytes.len();
            if pending_bytes >= GROUP_COMMIT_BYTES {
                flush_output!(w);
            }

            let matches = handshake.feed(&bytes);
            if matches > 0 {
                if !dsr_answered {
                    dsr_answered = true;
                    // Query exchange, ADR 0041's own phrase and shape:
                    // request -> response (only on a successful write) ->
                    // outcome (always, reflecting whether it was).
                    let req = ctx.capsule_frame(
                        Class::ControlExchange,
                        json!({"phase": "request", "kind_ns": "conpty/host-handshake",
                               "to": {"kind": "producer"}, "body": {"query": "da1"}}),
                    );
                    let req_seq = req.seq;
                    w.append(&req, Commit::Immediate)?;
                    frames_written += 1;

                    let write_result = writer.write_all(host_handshake::DA1_REPLY);
                    if write_result.is_ok() {
                        let mut resp = ctx.capsule_frame(
                            Class::ControlExchange,
                            json!({"phase": "response", "kind_ns": "conpty/host-handshake",
                                   "body": {"query": "da1"}}),
                        );
                        resp.refs = vec![FrameRef { kind: RefKind::RespondsTo, frame: req_seq }];
                        w.append(&resp, Commit::Immediate)?;
                        frames_written += 1;
                    }
                    let outcome_body = match &write_result {
                        Ok(()) => json!({"disposition": "ok"}),
                        Err(e) => json!({"disposition": "failed", "reason": e.to_string()}),
                    };
                    let out = ctx.capsule_frame(
                        Class::ControlExchange,
                        json!({"phase": "outcome", "kind_ns": "conpty/host-handshake", "scope": "pty",
                               "target": format!("{}:{}", req_seq.epoch, req_seq.n), "body": outcome_body}),
                    );
                    w.append(&out, Commit::Immediate)?;
                    frames_written += 1;

                    // Any FURTHER matches in this SAME chunk are already
                    // "later" than the one just answered.
                    handshake_suppressed_matches += (matches - 1) as u64;
                } else {
                    handshake_suppressed_matches += matches as u64;
                }
            }
        }};
    }

    // Main loop: natural-exit polled every iteration (bounded to one
    // GROUP_COMMIT_WINDOW of latency, regardless of event volume); the
    // caller's command channel is polled NON-BLOCKINGLY (rare traffic, and
    // this is the last point it is EVER polled — teardown never touches it
    // again, which is what makes admission revocation real). The wire
    // transport is serviced every iteration too (`service_transport_events!`
    // + `tick`) — see the module doc's "Step 5 (U2)" section.
    let exit_kind = 'main: loop {
        if process.wait(Duration::ZERO)? {
            break 'main ExitKind::ProducerExited;
        }
        service_transport_events!();
        execute_actions!(attach_proto.tick(Instant::now()));
        eager_ground_check!();
        // ADR 0041 EndRun step 2 / Codex round-1 Blocker 1 discharge: the
        // LATCH drives teardown, not the ack -- "ack completion only
        // ACCELERATES teardown". `shutdown_requested` alone (the OLD,
        // ack-completion-only trigger via `AttachAction::Shutdown`, and the
        // transport-fatal self-end path) is not enough: a stalled ack, a
        // client that stops reading, a progress-deadline close, or a lost
        // connection must still tear this run down once the marker is
        // durable, exactly the cases ADR 0041 lists as unable to unlatch
        // it. The ack remains a courtesy -- serviced normally through
        // teardown (still tracked via `pending_sends`/the ack-grace window)
        // but never a precondition for STARTING it.
        if shutdown_requested || run_end_latched {
            break 'main ExitKind::Requested;
        }
        match commands.try_recv() {
            // Major 6 discharge: `Command::Kill` is the direct-caller/
            // supervisor own-behalf EndRun primitive (this module's own
            // doc on `Command`) -- it must carry a reason and route
            // through the SAME commit/latch transition as a wire
            // `shutdown`, or a resume could respawn a run that a caller
            // deliberately ended. Idempotent like every other caller of
            // `commit_run_end_marker`: a concurrent wire shutdown racing
            // this Kill still writes only one marker.
            Ok(Command::Kill) => {
                shutdown_reason.get_or_insert_with(|| "operator_kill".to_string());
                commit_run_end_marker(
                    &mut ctx,
                    &mut w,
                    &mut frames_written,
                    &mut run_end_latched,
                    "operator_kill".to_string(),
                )?;
                break 'main ExitKind::Requested;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            // The caller dropped its `Sender` — NOT a kill (ADR: no
            // channel-disconnect-as-kill, "no exit code, no FE event, no
            // supervisor inference may request one"). Just means no
            // FUTURE commands will arrive; keep running on natural-exit
            // polling alone. `try_recv` on an already-disconnected channel
            // returns immediately, so there is no cost to leaving this
            // arm empty rather than tracking "stop trying".
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
        match output_rx.recv_timeout(GROUP_COMMIT_WINDOW) {
            Ok(ReaderEvent::Output(bytes)) => {
                pace_output!(bytes);
                maybe_rotate!(w);
            }
            Ok(ReaderEvent::Done(result)) => {
                // Reached only if the reader's read loop ended BEFORE this
                // loop ever called `close_pty()` — exactly the anomaly
                // `conpty.rs`'s own contract says shouldn't happen (ConPTY
                // keeps `hOutput` open regardless of child lifetime until
                // explicitly closed), whether that end was a graceful EOF
                // or a real error. Capsule-fatal either way (review
                // finding): bail unsealed, matching ADR 0039's crash shape
                // — recovery seals whatever valid prefix already committed.
                return Err(Error::State(format!(
                    "capsule_win: reader reached its terminal state before close_pty was ever called: {result:?}"
                )));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if last_commit.elapsed() >= GROUP_COMMIT_WINDOW {
                    flush_output!(w);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // The reader thread always sends exactly one `Done` before
                // its sender drops — reaching a bare disconnect without
                // one is an internal bug in this module, not a producer
                // condition. Loud, not a panic: nothing external caused
                // this.
                return Err(Error::State(
                    "capsule_win: reader thread's channel disconnected without a terminal Done event"
                        .into(),
                ));
            }
        }
    };
    // N1 (Codex review round 3, owner-corrected): captured HERE, the
    // instant the main loop concludes for EITHER exit kind -- NOT after
    // the teardown machinery below (job reap, ConPTY drain, the
    // aggregate deadline, a final wait), which alone can outlast the
    // producer's own life and would otherwise pollute this measurement
    // with exactly the capsule-side latency the supervisor's own
    // anti-flap counter must never see (an earlier version of this fix
    // measured it at the LATE producer_dead-detail-construction site
    // below, reproducing the identical bug it exists to close, just
    // moved inside this process instead of the supervisor's). For
    // ProducerExited the producer is already dead by definition; for
    // Requested it is about to be forcibly killed by `job.terminate()`
    // a few lines into teardown, with no intervening I/O between here
    // and there.
    let producer_uptime_ms = u64::try_from(spawned_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    flush_output!(w);

    // Producer-bound admission (take/input/resize) is revoked from here on
    // (finding 7) — but mgmt (probe/status/shutdown) and Sent completions
    // keep being serviced through BOTH phases below, via
    // `service_transport_events_teardown!`, until the pipe closes (see that
    // macro's own doc for why, and `execute_teardown_actions!` for the
    // reduced action set this implies).
    attach_proto.begin_teardown();

    // ONE teardown orchestrator (ADR 0041: "Teardown has ONE orchestrator")
    // for both exit_kind::ProducerExited and exit_kind::Requested — every
    // step below is unconditional: terminating an already-empty job is a
    // harmless no-op. `commands` is never read again from this point on —
    // real admission revocation (module doc), not receive-then-discard.
    //
    // Phase A: terminate the job, then REAP-POLL `ActiveProcesses` WHILE
    // STILL SERVICING `output_rx` (committing frames, answering the
    // handshake) AND the transport (mgmt/Sent, per finding 7) — review
    // finding, the blocker: the previous version polled the job with
    // nobody draining the channel, so a reader already blocked in
    // `OutputBudget::reserve` (or a DA1 only this loop could answer) could
    // leave `hOutput` undrained right when `ClosePseudoConsole` needed it
    // drained, and Microsoft's own docs say a pre-24H2 build's close can
    // wait indefinitely under exactly that condition.
    job.terminate()?;
    let reap_deadline = Instant::now() + TEARDOWN_REAP_TIMEOUT;
    loop {
        service_transport_events_teardown!();
        execute_teardown_actions!(attach_proto.tick(Instant::now()));
        eager_ground_check!();
        if job.active_processes()? == 0 {
            break;
        }
        if Instant::now() >= reap_deadline {
            return Err(Error::State(
                "capsule_win: job did not reap within the teardown timeout".into(),
            ));
        }
        match output_rx.recv_timeout(TEARDOWN_REAP_POLL) {
            Ok(ReaderEvent::Output(bytes)) => {
                pace_output!(bytes);
                maybe_rotate!(w);
            }
            Ok(ReaderEvent::Done(result)) => {
                // Same anomaly as the main loop's identical check: nothing
                // has called close_pty() yet, so this cannot be an
                // ordinary end of the drain.
                return Err(Error::State(format!(
                    "capsule_win: reader reached its terminal state before close_pty was ever called: {result:?}"
                )));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {} // just recheck active_processes
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::State(
                    "capsule_win: reader thread's channel disconnected without a terminal Done event during reap"
                        .into(),
                ));
            }
        }
    }
    flush_output!(w);

    // Phase B: close the pseudoconsole on a DEDICATED thread so THIS loop
    // can keep draining `output_rx` (feeding `handle_output!`, answering
    // the handshake) CONCURRENTLY with the close — the documented call
    // pattern ("reader already draining, THEN call this") applied
    // literally: draining must never itself pause to make the call. Both a
    // graceful EOF and a broken-pipe error are the ORDINARY, expected end
    // of this drain (the close is what produces them) — unlike Phase A's
    // identical-looking check, neither is an anomaly here.
    let closer_handle = std::thread::spawn(move || pty.close_pty());
    let drain_deadline = Instant::now() + TEARDOWN_DRAIN_TIMEOUT;
    loop {
        service_transport_events_teardown!();
        execute_teardown_actions!(attach_proto.tick(Instant::now()));
        eager_ground_check!();
        match output_rx.recv_timeout(TEARDOWN_DRAIN_POLL) {
            Ok(ReaderEvent::Output(bytes)) => {
                pace_output!(bytes);
                maybe_rotate!(w);
            }
            Ok(ReaderEvent::Done(_)) => {
                // Round-2 review, finding 5: service transport ONE more
                // time at the exact instant EOF ends this drain, so a
                // status/mgmt request that arrived just after the last
                // loop-top poll still gets answered while the pipe is
                // provably still live -- without this, everything from
                // here to `shutdown_all`'s eventual close (the flush and
                // joins below, the exit-status wait, writing lifecycle
                // state, sealing) is a live-but-unserviced pipe tail.
                service_transport_events_teardown!();
                execute_teardown_actions!(attach_proto.tick(Instant::now()));
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= drain_deadline {
                    return Err(Error::State(
                        "capsule_win: reader did not reach EOF within the teardown drain timeout".into(),
                    ));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::State(
                    "capsule_win: reader thread's channel disconnected without a terminal Done event during drain"
                        .into(),
                ));
            }
        }
    }
    flush_output!(w);

    // U1a, EndRun state machine item 4 / ack grace: the FINAL service poll
    // just above (the one at the exact EOF instant) can itself have
    // admitted a NEW mgmt `shutdown` and queued its `ShutdownAck` — give
    // that specific send up to `SHUTDOWN_ACK_GRACE` to be reported
    // physically written (removing its entry from `pending_sends`) before
    // this capsule's own transport goes away.
    //
    // U1a Codex round-1, Major 6 discharge: this window drains ONLY what
    // is ALREADY pending (`Sent`/`ConnectionClosed`, via
    // `drain_pending_sends_only!`) — it admits NOTHING new
    // (`ConnectionOpened`/fresh `Bytes` are closed outright, never reaching
    // `attach_proto`). A request newly accepted with, say, 50ms left in
    // this window would have almost no time to get its own ack physically
    // written, contradicting the "final service poll" guarantee this grace
    // exists to honor — so after the ordinary teardown drain ends, no
    // request is newly admitted at all; only what is already outstanding
    // (the ack this grace exists for, or any other send already queued
    // when the drain ended) gets to finish.
    let shutdown_ack_deadline = Instant::now() + SHUTDOWN_ACK_GRACE;
    while pending_sends
        .values()
        .any(|m| matches!(m, Some(SentMarker::ShutdownAck { .. })))
        && Instant::now() < shutdown_ack_deadline
    {
        drain_pending_sends_only!();
        execute_teardown_actions!(attach_proto.tick(Instant::now()));
        std::thread::sleep(SHUTDOWN_ACK_GRACE_POLL);
    }
    // The pipe's own disappearance: explicit HERE, rather than only
    // whenever `run` happens to return next (the exit-status wait and the
    // seal below need no pipe at all) — ADR 0041's grace is specifically
    // about DEFERRING that disappearance until it resolves, which requires
    // an actual close at THIS point, not a hope that returning soon is soon
    // enough. `shutdown_all` is idempotent (U1a): `ShutdownGuard`'s own
    // `Drop`, still ahead on every path, is a safe no-op the second time.
    //
    // Codex round-1 Blocker 3 discharge: ONE absolute aggregate deadline,
    // shared by the transport's OWN internal joins (accepted/reaper/every
    // connection worker, all cancellation-first per `Transport::
    // shutdown_all`'s own doc) AND this module's closer/reader threads —
    // "over an acceptor, a reaper, up to sixteen connection workers and
    // the capsule's own threads" (ADR 0041 bounds table). Cancellation for
    // THIS module's own threads already happened earlier in this same
    // function (Phase A's `job.terminate()`, Phase B's `pty.close_pty()`
    // on `closer_handle`) — by this point both threads are expected to be
    // at or near their own natural return, so `join_within` (never the
    // raw blocking `.join()`) is what actually bounds the residual gap
    // between "signalled EOF/exit" and "the thread function returned".
    // Expiry is TERMINAL: `run` must not seal-and-succeed, nor release the
    // writer fence (via `store`'s own drop), past a teardown that could
    // not prove every worker stopped — an `Err` here propagates before
    // `w.seal`/`store.advance_chain` are ever reached, and `store` (the
    // fence) still drops via its own destructor on this return path,
    // exactly as any other early `?` in this function already does.
    let teardown_deadline = Instant::now() + TEARDOWN_AGGREGATE_DEADLINE;
    let transport_ok = transport.0.shutdown_all(teardown_deadline);
    let closer_ok = join_within(closer_handle, teardown_deadline);
    let reader_ok = join_within(reader_handle, teardown_deadline);
    if !(transport_ok && closer_ok && reader_ok) {
        return Err(Error::State(format!(
            "capsule_win: aggregate teardown did not complete within its {TEARDOWN_AGGREGATE_DEADLINE:?} \
             deadline (transport ok={transport_ok}, closer ok={closer_ok}, reader ok={reader_ok}); \
             refusing to seal or report success past an unproven teardown"
        )));
    }

    // Step 5: the primary's own exit status, raw and unsigned end-to-end
    // (review finding: a Unix-style `i32` cast would turn a high-bit
    // NTSTATUS-shaped code negative for no reason). `wait()` first
    // establishes the honesty-bound precondition
    // `exit_code_after_confirmed_exit`'s own doc requires — ActiveProcesses
    // == 0 above already proved the process isn't running, but this
    // satisfies the bound by the letter of its doc, not just by inference.
    if !process.wait(Duration::from_secs(5))? {
        return Err(Error::State(
            "capsule_win: process handle did not signal after the job reaped to zero".into(),
        ));
    }
    let exit_code = process.exit_code_after_confirmed_exit()?;

    // The mgmt `shutdown` reason, if that is what drove this EndRun (ADR
    // 0041: "the reason string is recorded in producer_dead's detail").
    // `producer_uptime_ms` (N1, captured well above, at the exit_kind
    // boundary -- NOT recomputed here, past all the teardown machinery
    // this point sits after) is an ADDITIVE, free-form diagnostic field
    // -- like `reason` already is -- not a registered ADR 0039 feature:
    // it changes no authority, so no segment needs to declare anything
    // to carry it, and an older reader simply ignores an unknown plain
    // JSON field, exactly as `detail` has always allowed.
    let mut detail = json!({
        "exit_code": exit_code,
        "producer_uptime_ms": producer_uptime_ms,
    });
    if let Some(reason) = &shutdown_reason {
        detail["reason"] = json!(reason);
    }
    let f = ctx.capsule_frame(Class::Lifecycle, json!({"kind": "producer_dead", "detail": detail}));
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;

    let digest = w.seal(None)?;
    store.advance_chain(digest);
    segments_sealed += 1;

    Ok(ExitSummary {
        exit_code: Some(exit_code),
        exit_kind,
        frames_written,
        segments_sealed,
        handshake_answered: dsr_answered,
        handshake_suppressed_matches,
        resize_os_calls,
    })
}

/// Minimal base64 (standard alphabet, padded) — duplicated from
/// `capsule.rs`; see the module doc.
mod base64_engine {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    pub fn encode_b64(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            out.push(TABLE[(n >> 18) as usize & 63] as char);
            out.push(TABLE[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
            out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
        }
        out
    }
}

/// `OutputBudget`'s blocking is proven HERE, deterministically, because it
/// cannot be proven end-to-end: whether the e2e flood ever fills the budget
/// depends on conhost's burst pacing on the host machine, which no test
/// controls — a runner-image change turned exactly that e2e assertion red
/// on unchanged code. The flood test keeps the properties that are always
/// true (no deadlock, verify-green, bookkeeping live); the bound itself is
/// a plain condvar protocol, provable right at the primitive.
#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    /// Spins until exactly one reserve is parked in `Condvar::wait`,
    /// observed via the test-only `waiters` witness under the same mutex
    /// the wait releases atomically — the deterministic guarantee that the
    /// wake path (not a lucky early bound-check) is what the test then
    /// exercises.
    fn await_one_waiter(budget: &OutputBudget) {
        loop {
            if budget.state.lock().unwrap().waiters == 1 {
                return;
            }
            thread::yield_now();
        }
    }

    /// A reserve that finds the budget full parks in the condvar wait
    /// (proven by the waiter witness, not scheduling luck) and completes
    /// exactly when room is released — so a lost `notify_all` cannot
    /// escape this test on any interleaving.
    #[test]
    fn output_budget_blocks_at_the_bound_and_unblocks_on_release() {
        let budget = Arc::new(OutputBudget::new());
        assert!(budget.reserve(OUTPUT_QUEUE_BUDGET_BYTES));

        let (done_tx, done_rx) = mpsc::channel();
        let b = Arc::clone(&budget);
        let worker = thread::spawn(move || {
            done_tx.send(b.reserve(1)).unwrap();
        });

        await_one_waiter(&budget);
        budget.release(1);
        assert!(done_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("parked reserve never completed after release"));
        worker.join().unwrap();
        assert_eq!(budget.state.lock().unwrap().outstanding, OUTPUT_QUEUE_BUDGET_BYTES);
    }

    /// `cancel` wakes a PARKED reserve (same witness) and makes it return
    /// `false` — the reader-must-stop signal — without any release ever
    /// happening; and a cancelled budget refuses every future reserve.
    #[test]
    fn output_budget_cancel_unblocks_a_parked_reserve_with_false() {
        let budget = Arc::new(OutputBudget::new());
        assert!(budget.reserve(OUTPUT_QUEUE_BUDGET_BYTES));

        let (done_tx, done_rx) = mpsc::channel();
        let b = Arc::clone(&budget);
        let worker = thread::spawn(move || {
            done_tx.send(b.reserve(1)).unwrap();
        });

        await_one_waiter(&budget);
        budget.cancel();
        assert!(!done_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("parked reserve never returned after cancel"));
        worker.join().unwrap();
        assert!(!budget.reserve(1), "a cancelled budget must refuse every future reserve");
    }

    // -- commit_run_end_marker: ADR 0041 EndRun steps 1-2, proven directly
    // against a plain SegmentWriter/FrameCtx pair -- no real ConPTY run
    // needed for the one property this function owns (the lane operation
    // semantics -- `failed {record_append}`, the hold release, the leg
    // replacement -- are U2's; `tests/capsule_win.rs` proves the ack
    // ordering end to end against a real capsule run).

    fn run_end_marker_writer(dir: &std::path::Path, name: &str) -> (VoyageStore, SegmentWriter) {
        let root = dir.join(name);
        VoyageStore::bootstrap(&root, name, RetentionClass::Discard).unwrap();
        let mut store = VoyageStore::open_for_writing(&root, name).unwrap();
        let w = store
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        (store, w)
    }

    fn run_end_marker_ctx() -> FrameCtx {
        FrameCtx {
            epoch: 1,
            next_n: 1,
            t0: Instant::now(),
            take_epoch: 0,
            holder: None,
            attached: None,
        }
    }

    #[test]
    fn commit_run_end_marker_appends_and_latches_on_first_call() {
        let dir = tempfile::tempdir().unwrap();
        let (_store, mut w) = run_end_marker_writer(dir.path(), "rem1");
        let mut ctx = run_end_marker_ctx();
        let mut frames_written = 0u64;
        let mut latched = false;
        commit_run_end_marker(&mut ctx, &mut w, &mut frames_written, &mut latched, "quit".into())
            .unwrap();
        assert!(latched);
        assert_eq!(frames_written, 1);
    }

    /// Step 4: concurrent requests -- the first commit wins and writes the
    /// only marker; a later one is a no-op (its own ack still ships, at
    /// the call site, regardless of what this function does).
    #[test]
    fn commit_run_end_marker_second_call_writes_no_second_marker() {
        let dir = tempfile::tempdir().unwrap();
        let (_store, mut w) = run_end_marker_writer(dir.path(), "rem2");
        let mut ctx = run_end_marker_ctx();
        let mut frames_written = 0u64;
        let mut latched = false;
        commit_run_end_marker(&mut ctx, &mut w, &mut frames_written, &mut latched, "first".into())
            .unwrap();
        commit_run_end_marker(&mut ctx, &mut w, &mut frames_written, &mut latched, "second".into())
            .unwrap();
        assert_eq!(frames_written, 1, "a second concurrent request must write no second marker");
    }

    /// A failed append (forced here via a contiguity violation — the SAME
    /// `?`-propagation path a real storage fault reaches; see
    /// `commit_run_end_marker`'s own doc for why the CAUSE of the failure
    /// is not this function's concern) leaves the latch false and
    /// propagates the error, exactly ADR 0039's crash shape: no marker,
    /// no latch — and, at the real call site, the ack action sitting
    /// after this one in the same batch is never reached either, since
    /// `run` returns before continuing the action queue.
    #[test]
    fn commit_run_end_marker_failed_append_leaves_the_latch_false_and_propagates() {
        let dir = tempfile::tempdir().unwrap();
        let (_store, mut w) = run_end_marker_writer(dir.path(), "rem3");
        let mut ctx = run_end_marker_ctx();
        // Test-bug fix (CI: windows-latest caught this; the code path is
        // portable, not platform-divergent — see the commit message). A
        // FRESH `SegmentWriter`'s `last_seq` is `None`, and `append`'s own
        // contiguity check only fires once a PRIOR frame exists (see
        // `SegmentWriter::append`): `ctx.next_n` alone cannot violate
        // contiguity on the very first frame, so the original version of
        // this test never actually reached the error path it claimed to
        // test — it happened to type-check (this module only compiles on
        // Windows) but was never RUN until real Windows CI executed it.
        // Seed one real frame directly via `w.append` (NOT
        // `commit_run_end_marker`, which would latch and short-circuit the
        // second call this test actually exercises) so the writer has a
        // real last_seq (n=1) to violate.
        let seed = ctx.capsule_frame(Class::Lifecycle, json!({"kind": "producer_ready"}));
        w.append(&seed, Commit::Immediate).unwrap();
        ctx.next_n = 5; // breaks contiguity: the segment's last n is 1, expects 2 next
        let mut frames_written = 0u64;
        let mut latched = false;
        let err =
            commit_run_end_marker(&mut ctx, &mut w, &mut frames_written, &mut latched, "quit".into())
                .unwrap_err();
        assert!(format!("{err}").contains("non-contiguous"), "got: {err}");
        assert!(!latched, "a failed append must never latch");
        assert_eq!(frames_written, 0);
    }

    /// Codex round-1 Major 5 discharge: a post-write, fsync-reported
    /// failure (the write itself already durable — see
    /// `SegmentWriter::inject_fault_on_next_append_sync`'s own doc) must
    /// still propagate as a failure to THIS caller (no latch, ADR 0039's
    /// crash shape from `commit_run_end_marker`'s own perspective) —
    /// but, per ADR 0041's one-fact-one-barrier rule, the marker is
    /// ALREADY visible on disk regardless, and the typed accessor a
    /// later unit's respawn decision reads
    /// (`verify::leg_carries_run_end_marker`) must report it as present.
    /// A requester's pessimistic report can never make a real marker
    /// disappear; treating the visible byte as authoritative is what
    /// keeps "one fact, one barrier" true even when the ONE frame commit
    /// step itself is split into a write and a separate fsync outcome.
    #[test]
    fn fsync_failure_after_a_durable_write_still_leaves_the_marker_visible() {
        let dir = tempfile::tempdir().unwrap();
        let (_store, mut w) = run_end_marker_writer(dir.path(), "rem4");
        let mut ctx = run_end_marker_ctx();
        let mut frames_written = 0u64;
        let mut latched = false;
        w.inject_fault_on_next_append_sync();
        let err = commit_run_end_marker(
            &mut ctx,
            &mut w,
            &mut frames_written,
            &mut latched,
            "quit".into(),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("injected fsync failure"), "got: {err}");
        // The caller's own view: no latch, no bookkeeping credit -- ADR
        // 0039's crash shape from this function's own perspective.
        assert!(!latched);
        assert_eq!(frames_written, 0);

        // The world's view: the bytes are genuinely on disk. Drop the
        // writer (releases the file, no further writes) and read the
        // STILL-OPEN segment fresh, exactly as a crashed leg's
        // reconciliation/accessor would.
        drop(w);
        let seg_dir = dir.path().join("rem4").join("seg");
        assert!(
            crate::verify::leg_carries_run_end_marker(&seg_dir, "rem4", 1).unwrap(),
            "a marker whose write succeeded (only its fsync report lied) must still be visible \
             to the accessor -- a requester's pessimistic report can never erase a real byte"
        );
    }
}
