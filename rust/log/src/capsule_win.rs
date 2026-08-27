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
//! - **`run` no longer owns stdin.** The first version read the real
//!   process stdin internally, which (a) is process-global state a
//!   reusable library function has no business owning, and (b) meant
//!   "teardown revokes admission" was really just "teardown discards what
//!   it already accepted" — the thread kept enqueueing regardless. `run`
//!   now takes exactly ONE caller-owned command channel
//!   (`mpsc::Receiver<Command>`, `Command` now including `Input` alongside
//!   `Resize`/`Kill`); the bin harness owns its own stdin-reading thread
//!   and forwards `Command::Input` into it. Admission revocation is now
//!   real: once the main loop is left for teardown, this channel is never
//!   read from again — not received-then-discarded, simply never polled.
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

#![cfg(windows)]

use crate::conpty::{observe_spawning_process_jobbed, ConptySpawn};
use crate::envelope::*;
use crate::host_handshake::{self, HostHandshake};
use crate::segment::{Commit, RetentionClass};
use crate::voyage::VoyageStore;
use crate::{Error, Result};
use serde_json::json;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const GROUP_COMMIT_WINDOW: Duration = Duration::from_millis(50);
const GROUP_COMMIT_BYTES: usize = 256 * 1024;
const SEGMENT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const READ_CHUNK: usize = 8192;
const LOCAL_CONTROLLER: &str = "local";

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

pub struct CapsuleWinConfig {
    pub voyage_root: PathBuf,
    pub voyage_id: String,
    pub retention: RetentionClass,
    pub producer_kind: String,
    /// argv[0] is the program; must be non-empty (`ConptySpawn::spawn`'s
    /// own check is what actually enforces this).
    pub argv: Vec<String>,
    /// Echo producer output to the capsule's stdout after commit — same
    /// visibility watermark as the Linux capsule: after the fsync that
    /// covers it, never before.
    pub echo: bool,
    /// Initial terminal geometry, validated by the SAME 2x2..512x256 rule
    /// a later resize is (ADR 0041: "Initial geometry is validated by the
    /// same rule").
    pub cols: u16,
    pub rows: u16,
}

/// The caller-owned command surface `run` services. ADR 0041's attach
/// lane's `resize`/`input` ops and the mgmt lane's `shutdown` (which drives
/// `EndRun`) are all step 5's job to wire onto a real named pipe — this is
/// the internal Rust channel step 5 forwards into, not the wire protocol
/// itself, and this unit does not implement admission (who may call it).
/// `run` owns none of the sources that feed this channel — not stdin, not
/// a pipe — a caller (the bin harness, later a real attach lane) does, and
/// is responsible for keeping the `Sender` alive for as long as it wants
/// commands serviced (review finding: a previous version read stdin
/// itself, which made "teardown revokes admission" not really true).
#[derive(Debug, Clone)]
pub enum Command {
    /// Raw producer input bytes to forward, redacted by default per the
    /// raw-terminal profile (ADR 0037/0039).
    Input(Vec<u8>),
    /// "resize {cols, rows} — driver-only" (ADR 0041 attach protocol);
    /// this unit only implements the ordered exchange once a command
    /// arrives.
    Resize { cols: u16, rows: u16 },
    /// The step-4-visible primitive behind `EndRun` (ADR 0041 Lifecycle):
    /// an EXTERNALLY REQUESTED end. Never inferred from an exit code, a
    /// channel disconnect, or anything else — only an explicit `Kill`.
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
    /// `Command::Kill` was received.
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

/// 16 random bytes from the OS, as lowercase hex32 (the ADR idem_key
/// shape) — duplicated from `capsule.rs` rather than shared; see the
/// module doc.
fn random_idem_key() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(std::io::Error::from)?;
    Ok(b.iter().map(|x| format!("{:02x}", x)).collect())
}

fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
    take_epoch: u64,
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
    fn controller_frame(&mut self, class: Class, payload: serde_json::Value) -> Envelope {
        let mut e = self.capsule_frame(class, payload);
        e.source.actor = Actor {
            kind: ActorKind::Controller,
            controller_id: Some(LOCAL_CONTROLLER.into()),
            take_epoch: Some(self.take_epoch),
        };
        e
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

/// Run one producer under a Windows capsule. Blocks until the run ends —
/// either the producer exits on its own, or `commands` delivers
/// [`Command::Kill`] (ADR 0041 Lifecycle: an externally requested end).
/// `commands` mirrors `claude.rs`'s `operator: mpsc::Receiver<OperatorCmd>`
/// parameter — the raw command surface a future attach lane (step 5)
/// forwards into; the caller owns its `Sender` and everything that feeds
/// it (stdin included — see the module doc).
// unused_assignments: `flush_output!`'s state reset is dead only at its
// FINAL expansion (after the loop) — load-bearing at every other site,
// same allow capsule.rs carries for the identical reason.
#[allow(unused_assignments)]
pub fn run(config: CapsuleWinConfig, commands: mpsc::Receiver<Command>) -> Result<ExitSummary> {
    // Resolve ONCE — see capsule.rs's identical comment on the same call.
    let voyage_root = crate::fsutil::ensure_container(&config.voyage_root)?;
    if !voyage_root.exists() {
        VoyageStore::bootstrap(&voyage_root, &config.voyage_id, config.retention)?;
    }
    let mut store = VoyageStore::open_for_writing(&voyage_root, &config.voyage_id)?;
    store.seal_survivor()?;

    let mut ctx = FrameCtx {
        epoch: store.epoch,
        next_n: 1,
        t0: Instant::now(),
        take_epoch: 0,
        attached: None,
    };
    let mut w = store.open_segment(wall_ms())?;
    let mut seg_bytes: u64 = 0;
    let mut frames_written: u64 = 0;
    let mut segments_sealed: u64 = 0;

    // Control preamble — every frame here commits immediately. Identical
    // shape to capsule.rs: revoke-first (null holder, bumped epoch), then
    // grant local.
    let prior_take = store.last_take_epoch;
    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "take_state", "take": {"take_epoch": prior_take + 1, "holder": null}}),
    );
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;
    ctx.take_epoch = prior_take + 2;
    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "take_state", "take": {"take_epoch": ctx.take_epoch, "holder": LOCAL_CONTROLLER}}),
    );
    w.append(&f, Commit::Immediate)?;
    frames_written += 1;

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
    // it. NO scrollback (the capsule keeps none by design).
    let mut parser = vt100_ctt::Parser::new(config.rows, config.cols, 0);
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

    let mut stdout = std::io::stdout();
    let mut pending_echo: Vec<u8> = Vec::new();
    let mut pending_bytes: usize = 0;
    let mut last_commit = Instant::now();

    macro_rules! flush_output {
        ($w:expr) => {
            if pending_bytes > 0 {
                $w.commit()?; // the watermark: fsync BEFORE anything is echoed
                if config.echo {
                    let _ = stdout.write_all(&pending_echo);
                    let _ = stdout.flush();
                }
                pending_echo.clear();
                pending_bytes = 0;
            }
            last_commit = Instant::now();
        };
    }

    macro_rules! maybe_rotate {
        ($w:ident) => {
            if seg_bytes >= SEGMENT_MAX_BYTES {
                flush_output!($w);
                let digest = $w.seal(None)?;
                store.advance_chain(digest);
                segments_sealed += 1;
                $w = store.open_segment(wall_ms())?;
                seg_bytes = 0;
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
            pending_echo.extend_from_slice(&bytes);
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
    // again, which is what makes admission revocation real).
    let exit_kind = 'main: loop {
        if process.wait(Duration::ZERO)? {
            break 'main ExitKind::ProducerExited;
        }
        match commands.try_recv() {
            Ok(Command::Kill) => break 'main ExitKind::Requested,
            Ok(Command::Input(bytes)) => {
                // WAL order identical to capsule.rs: input -> intent ->
                // syscall -> forwarded.
                flush_output!(w);
                let input = ctx.controller_frame(
                    Class::Input,
                    json!({"idem_key": random_idem_key()?, "content": "redacted", "length": bytes.len()}),
                );
                let input_seq = input.seq;
                w.append(&input, Commit::Immediate)?;
                let mut intent = ctx.controller_frame(
                    Class::Lifecycle,
                    json!({"kind": "input_fact",
                           "fact": {"input": {"epoch": input_seq.epoch, "n": input_seq.n},
                                     "fact": "forward_intent"}}),
                );
                intent.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: input_seq }];
                let intent_seq = intent.seq;
                w.append(&intent, Commit::Immediate)?;
                writer.write_all(&bytes)?; // the forward syscall
                let mut fwd = ctx.controller_frame(
                    Class::Lifecycle,
                    json!({"kind": "input_fact",
                           "fact": {"input": {"epoch": input_seq.epoch, "n": input_seq.n},
                                     "fact": "forwarded",
                                     "intent": {"epoch": intent_seq.epoch, "n": intent_seq.n}}}),
                );
                fwd.refs = vec![FrameRef { kind: RefKind::CausedBy, frame: input_seq }];
                w.append(&fwd, Commit::Immediate)?;
                frames_written += 3;
                maybe_rotate!(w);
            }
            Ok(Command::Resize { cols, rows }) => {
                // ADR 0041: "Resize rejects, never clamps" — an
                // ordered-writer-loop command: request committed -> one
                // ResizePseudoConsole call (skipped entirely if
                // out-of-budget) -> parser + geometry updated only on
                // success -> outcome committed. request+outcome only (no
                // response phase): the exchange resolves entirely within
                // this loop, nothing external to await.
                flush_output!(w);
                let req = ctx.controller_frame(
                    Class::ControlExchange,
                    json!({"phase": "request", "kind_ns": "conpty/resize",
                           "to": {"kind": "producer"}, "body": {"cols": cols, "rows": rows}}),
                );
                let req_seq = req.seq;
                w.append(&req, Commit::Immediate)?;
                frames_written += 1;

                let in_budget =
                    (MIN_COLS..=MAX_COLS).contains(&cols) && (MIN_ROWS..=MAX_ROWS).contains(&rows);
                let outcome_body = if !in_budget {
                    json!({"disposition": "failed", "cols": cols, "rows": rows,
                           "reason": "outside the 2x2..512x256 budget"})
                } else {
                    resize_os_calls += 1;
                    match pty.resize(cols, rows) {
                        Ok(()) => {
                            // vt100_ctt's own argument order is (rows,
                            // cols) — the opposite of ConPTY's (cols,
                            // rows); both call sites are spelled out
                            // explicitly rather than sharing a tuple, so a
                            // future reorder of one can't silently swap
                            // the other.
                            parser.screen_mut().set_size(rows, cols);
                            json!({"disposition": "ok", "cols": cols, "rows": rows})
                        }
                        Err(e) => json!({"disposition": "failed", "cols": cols, "rows": rows,
                                          "reason": e.to_string()}),
                    }
                };
                let out = ctx.controller_frame(
                    Class::ControlExchange,
                    json!({"phase": "outcome", "kind_ns": "conpty/resize", "scope": "pty",
                           "target": format!("{}:{}", req_seq.epoch, req_seq.n), "body": outcome_body}),
                );
                w.append(&out, Commit::Immediate)?;
                frames_written += 1;
                maybe_rotate!(w);
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
                handle_output!(bytes);
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
    flush_output!(w);

    // ONE teardown orchestrator (ADR 0041: "Teardown has ONE orchestrator")
    // for both exit_kind::ProducerExited and exit_kind::Requested — every
    // step below is unconditional: terminating an already-empty job is a
    // harmless no-op. `commands` is never read again from this point on —
    // real admission revocation (module doc), not receive-then-discard.
    //
    // Phase A: terminate the job, then REAP-POLL `ActiveProcesses` WHILE
    // STILL SERVICING `output_rx` (committing frames, answering the
    // handshake) — review finding, the blocker: the previous version
    // polled the job with nobody draining the channel, so a reader already
    // blocked in `OutputBudget::reserve` (or a DA1 only this loop could
    // answer) could leave `hOutput` undrained right when
    // `ClosePseudoConsole` needed it drained, and Microsoft's own docs say
    // a pre-24H2 build's close can wait indefinitely under exactly that
    // condition.
    job.terminate()?;
    let reap_deadline = Instant::now() + TEARDOWN_REAP_TIMEOUT;
    loop {
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
                handle_output!(bytes);
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
        match output_rx.recv_timeout(TEARDOWN_DRAIN_POLL) {
            Ok(ReaderEvent::Output(bytes)) => {
                handle_output!(bytes);
                maybe_rotate!(w);
            }
            Ok(ReaderEvent::Done(_)) => break,
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
    let _ = closer_handle.join();
    let _ = reader_handle.join();

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

    let f = ctx.capsule_frame(
        Class::Lifecycle,
        json!({"kind": "producer_dead", "detail": {"exit_code": exit_code}}),
    );
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
}
