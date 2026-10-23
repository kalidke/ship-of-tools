//! The capsule: one process babysitting one producer, the leg (`sot-capsule run`). The producer's output feeds a live
//! `vt100_ctt` parser that the row socket's attaches checkpoint from; the leg keeps no record of the terminal, only the
//! screen file (`screen.rs`) and the small `ended` file (`ended.rs`). Generic over
//! [`crate::capsule::producer::Producer`]: the writer loop below, with the output budget and the row socket's
//! requests, is platform-neutral and drives whatever producer the caller names: `producer/conpty/producer.rs`'s
//! `ConptyProducer` (Windows) and `producer/pty/`'s `PtyProducer` (Unix).
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
//! - **`run` owns no stdin.** It takes the caller's `commands` channel; once the main loop is left for teardown,
//!   `commands` is never read again.
//!
//! The row socket runs through this loop (`crate::row`): the loop feeds the run's `RowRun` every committed chunk and
//! serves its requests (a waiting attach at a parser ground state, typed input, a resize) every pass.

use crate::capsule::producer::{ExitStatus, Producer};
use crate::capsule::producer::host_handshake::{self, HostHandshake};
// The shared-deadline poll-join primitive and the pinned aggregate bound the loop's own worker joins use.
use crate::lane::transport::{join_within, TEARDOWN_AGGREGATE_DEADLINE};
use crate::{Error, Result};
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
pub use ended::{mark_requested as mark_end_requested, read as read_ended, Ended};
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

pub struct CapsuleConfig {
    /// The leg's folder (`<state_dir>/voyages/<id>/`): the fence and the `ended` file.
    pub voyage_root: PathBuf,
    /// Where the leg keeps its screen (`<state_dir>/screen`); `None` keeps none.
    pub screen_path: Option<PathBuf>,
    /// argv[0] is the program; must be non-empty (`Producer::spawn`'s own
    /// check is what actually enforces this).
    pub argv: Vec<String>,
    /// Initial terminal geometry, validated by the SAME 2x2..512x256 rule
    /// a later resize is.
    pub cols: u16,
    pub rows: u16,
    /// What the run tells its owner while it lives (`RunSignals`); `None` for a caller that asks nothing.
    pub signals: Option<Arc<RunSignals>>,
}

/// What a run tells its owner while it lives: when the agent was started and whether it has written yet. The owner
/// reads it to tell `starting` from `ready` (the supervisor's status).
#[derive(Debug, Default)]
pub struct RunSignals {
    spawned_at: std::sync::OnceLock<Instant>,
    output_seen: AtomicBool,
    /// The row socket's fan-out for this run, when the owner serves one: the run feeds it and services its requests.
    row: std::sync::OnceLock<Arc<crate::row::RowRun>>,
}

impl RunSignals {
    /// Names the row socket's fan-out this run feeds. Set once, before the run starts.
    pub fn attach_row(&self, row: Arc<crate::row::RowRun>) {
        let _ = self.row.set(row);
    }

    pub(crate) fn row(&self) -> Option<Arc<crate::row::RowRun>> {
        self.row.get().cloned()
    }

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

/// The caller-owned command surface `run` services. `Kill` is the primitive behind `EndRun` (ADR 0041 Lifecycle): a
/// supervisor drives it, and it is the only way a run's end is requested. `run` owns none of the sources that feed this
/// channel: a caller keeps the `Sender` alive for as long as it wants commands serviced.
#[derive(Debug, Clone)]
pub enum Command {
    /// An EXTERNALLY REQUESTED end. Never inferred from an exit code, a
    /// channel disconnect, or anything else — only an explicit `Kill`.
    Kill,
    /// The run ends because its supervisor is stopping, not because anyone ended the run: `ended` records no request,
    /// so a `--resume` of the row starts the agent again.
    Stop,
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
    /// `Command::Kill` was received (the durable EndRun latch is set), or `Command::Stop`.
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
    /// A request on the row socket may be waiting: carries no data itself. Sent by the row's wake callback; every
    /// `output_rx.recv_timeout` site that matches this just loops back to its own top, where the row's requests are
    /// served.
    Wake,
    /// the reader thread's `tx` is not the
    /// channel's only real sender — the row's wake callback holds
    /// a clone too — so a bare channel disconnect cannot be
    /// trusted to mean "the reader thread dropped its sender". This event
    /// states it explicitly: `ReaderGoneGuard`, constructed
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
