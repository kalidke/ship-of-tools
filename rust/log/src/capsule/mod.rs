//! The capsule: one process babysitting one producer, the leg (`sot-capsule run`). The producer's output feeds a live
//! `vt100_ctt` parser that every attach checkpoints from; the leg keeps no record of the terminal, only the screen
//! file (`screen.rs`) and the small `ended` file (`ended.rs`). Generic over [`crate::capsule::producer::Producer`]:
//! the writer loop below, with the output budget, the `AttachProto` service path and `ShutdownGuard`, is
//! platform-neutral and drives whatever producer the caller names: `producer/conpty/producer.rs`'s `ConptyProducer`
//! (Windows) and `producer/pty/`'s `PtyProducer` (Unix).
//!
//! - **Teardown is a phase of the ordered loop, not a pause in it.** The reap poll keeps servicing the output channel
//!   (publishing output, answering the ConPTY host handshake) while it polls; `close_pty()` runs on a dedicated
//!   closer thread so the loop keeps draining during the call; the drain timeout's clock starts when the closer thread
//!   starts.
//! - **The output budget is reserve-before-read and cancellable.** The reader reserves a full `READ_CHUNK` before
//!   calling `read()`, so the outstanding bytes never exceed the budget; a `BudgetCancelGuard` cancels it on any exit
//!   from `run`, so a reader never outlives the loop that releases it.
//! - **The host handshake is answered once.** The first DA1 query of a run is answered; later matches are counted.
//! - **Exit status is raw and unsigned end to end** (`ExitSummary`, `ended`): a high-bit NTSTATUS code stays a `u32`.
//! - **A read error is never folded into a clean end.** The reader thread sends one terminal event carrying a real
//!   `Result`; before `close_pty()` it is an anomaly that only the producer's own exit explains (`READER_END_EXIT_GRACE`).
//! - **`run` owns no stdin.** It takes the caller's `commands` channel and the wire's events from the transport; once
//!   the main loop is left for teardown, `commands` is never read again.
//!
//! The pipe protocol runs through this loop: transport events go through [`crate::lane::attach_proto::AttachProto`],
//! which owns the connection, role, lockstep, pen and keepalive state; the loop executes the actions it returns and
//! feeds events back. Output is published to subscribers as it arrives, and a ground boundary promotes a pending attach.

use crate::lane::attach_proto::{
    Action as AttachAction, AttachProto, ConnId, InputOutcome, MgmtStatus, SentMarker,
};
use crate::capsule::producer::{ExitStatus, Producer};
use crate::capsule::producer::host_handshake::{self, HostHandshake};
// The shared-deadline poll-join primitive and the pinned aggregate bound `lane/pipe_win/` uses for its own worker
// joins: one mechanism, one constant.
use crate::lane::transport::{join_within, Transport, TransportEvent, TEARDOWN_AGGREGATE_DEADLINE};
use crate::lane::wire::{self, Survival};
use crate::{Error, Result};
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod ended;
mod fence;
mod output;
pub mod producer;
mod screen;
mod writer_loop;
use output::*;
pub use ended::{read as read_ended, Ended};
pub use fence::{bootstrap as bootstrap_voyage_folder, read_leg_epoch, WRITER_LOCK};
#[cfg(any(test, feature = "test-support"))]
pub use screen::set_screen_hook_for_test;
pub use writer_loop::run;

const READ_CHUNK: usize = 8192;

/// How long the leg's end waits for the screen writer: a stalled disk never holds the end of a leg longer.
const SCREEN_FINISH_BOUND: Duration = Duration::from_secs(2);

/// One diagnostic line, as one write that never panics: `eprintln!` panics when stderr cannot take the line, and a
/// full volume under the leg's log would then turn a diagnostic into a crash of the leg.
fn leg_note(args: std::fmt::Arguments<'_>) {
    let line = format!("{args}\n");
    let _ = std::io::Write::write_all(&mut std::io::stderr(), line.as_bytes());
}

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
    /// The leg's folder (`<state_dir>/voyages/<id>/`): the fence and the `ended` file.
    pub voyage_root: PathBuf,
    /// Names the leg's lane (`voyage-<id>`).
    pub voyage_id: String,
    /// Where the leg keeps its screen (`<state_dir>/screen`); `None` keeps none.
    pub screen_path: Option<PathBuf>,
    /// argv[0] is the program; must be non-empty (`Producer::spawn`'s own
    /// check is what actually enforces this).
    pub argv: Vec<String>,
    /// Initial terminal geometry, validated by the SAME 2x2..512x256 rule
    /// a later resize is.
    pub cols: u16,
    pub rows: u16,
    /// Supplied by the SPAWNER, never inferred (ADR 0041 decision 11: the
    /// breakaway attempt is the real source). Transported verbatim in mgmt
    /// `status`.
    pub survival: Survival,
    /// What the run tells its owner while it lives (`RunSignals`); `None` for a caller that asks nothing.
    pub signals: Option<Arc<RunSignals>>,
}

/// What a run tells its owner while it lives: when the agent was started and whether it has written yet. The owner
/// reads it to tell `starting` from `ready` (the supervisor's status).
#[derive(Debug, Default)]
pub struct RunSignals {
    spawned_at: std::sync::OnceLock<Instant>,
    output_seen: AtomicBool,
}

impl RunSignals {
    /// When the agent was started, `None` until it was.
    pub fn spawned_at(&self) -> Option<Instant> {
        self.spawned_at.get().copied()
    }

    /// The agent has written something since it started.
    pub fn output_seen(&self) -> bool {
        self.output_seen.load(Ordering::Acquire)
    }

    fn spawned(&self, at: Instant) {
        let _ = self.spawned_at.set(at);
    }

    fn output(&self) {
        self.output_seen.store(true, Ordering::Release);
    }
}

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
    /// `Command::Kill` was received, or a mgmt `shutdown` committed
    /// the durable EndRun latch. The latch starts teardown without waiting
    /// for physical ack completion; the ack grace bounds the wait for a
    /// pending shutdown acknowledgement (ADR 0041 EndRun).
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
    let created = sot_platform::identity::challenge_unix::self_start_ticks()
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
    let created = sot_platform::identity::challenge_macos::self_pidversion()
        .map_err(|e| Error::State(format!("capsule: self_pidversion failed: {e}")))?;
    Ok(MgmtStatus { pid, created: u64::from(created), survival })
}
