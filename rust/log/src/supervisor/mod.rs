//! ADR 0041 step 6 U2: the authority. `sot-capsule supervise` is the
//! process the launcher starts (Lifecycle "ONE AUTHORITY... Every act
//! that starts, ends, adopts or resets a run is performed by the process
//! holding `<state-dir>\supervisor.lock`"); [`endrun`] and [`reset`] are
//! the no-supervisor path's own fence-acquiring in-process callers ("the
//! same TRANSITION, not the same CAPABILITIES").
//!
//! # `Lifecycle`
//!
//! One state machine, `Recovering -> InitialProbe -> {Ready, Spawning,
//! EndedNoRespawn}`, with `Ready <-> Spawning` (respawn), `Ready ->
//! Ending -> {EndedNoRespawn, Terminal}`, and `EndedNoRespawn -> Resetting
//! -> Spawning` — plus `Terminal` (STICKY: nothing ever transitions out
//! of it once entered; carries its own `entered_at`).
//! Every OS-facing wait (probe episode, spawn readiness, end_run's
//! mgmt-lane exchange + process wait + O(history) verify, reset's
//! rename+bootstrap+publish) runs on its own background thread; the main
//! loop only ever polls a `Receiver` non-blockingly and services the
//! lane, on EVERY iteration, regardless of phase — "one linearized state
//! machine, not one blocking thread." Each worker-bearing state keeps its
//! own `JoinHandle<()>` and a `started_at` an operation watchdog
//! measures against; a worker panic is `Disconnected` on its receiver,
//! mapped to `Terminal` from WHATEVER state observes it, and a watchdog
//! EXPIRY never blocks the main thread in
//! `.join()` — it abandons the worker thread instead (see
//! `abandon_worker`), because a stuck worker is exactly the case a
//! blocking join on it would defeat the whole point of having a
//! watchdog at all.
//!
//! # Stop no longer owns a Lifecycle state
//!
//! An earlier `Lifecycle::Stopping` variant TRANSITIONED into on `stop`,
//! discarding whatever worker/receiver was in flight (retaining only a
//! bare `JoinHandle`, unable to preserve its actual RESULT) — exactly
//! how a `Fatal` outcome from a Stop-preempted Reset/EndRun could be
//! silently dropped and the process still exit 0, and how a SECOND stop
//! could recompute (not accumulate) `was_terminal` from whatever the
//! Lifecycle had ALREADY been overwritten to, clearing a `true` a FIRST
//! stop had legitimately set. Now `stop`'s acceptance touches NOTHING
//! about the `Lifecycle` — see [`AuthorityState::stop_requested`], the
//! ONLY thing it records. The underlying `Lifecycle` keeps resolving
//! itself through its own ordinary transition arms, unchanged, for
//! however long that takes; the main loop's own exit condition (in
//! `supervise_inner`) reads `stop_requested` back to decide WHEN a
//! resting point (`Ready`, `EndedNoRespawn`, or `Terminal`) is worth
//! exiting from, and `terminal_severity` there is MONOTONIC (`prior ||
//! new`, never reassigned) across however many `stop` commands arrive.
//! The reply itself reuses the SAME per-connection `PendingClose` gate
//! every other reply already uses
//! — `handle_lane_bytes`'s own `CommandEffect::Stop` arm sends it
//! inline, delivery-gated exactly like a version-skew refusal is.
//!
//! # Recovery runs before pointer discovery (ADR 0041 "Recovery is part
//! of the transaction, and it runs FIRST") —
//! and a recovered Stop does NOT stop this authority
//!
//! `Recovering` reconciles every active journal entry — voyage-agnostic,
//! keyed off nothing but `<state_dir>` itself and each entry's OWN
//! recorded voyage — BEFORE the pointer is ever read to decide the
//! current voyage id. Reversing this order let a crash between a reset's journal admission and
//! its rename/publish leave the authority probing or reporting a voyage
//! identity recovery was about to change out from under it. A crashed
//! `Stop`'s own active journal entry is finished as terminal `Stopping`
//! here (loud on failure, via the bare `?` this function already
//! propagates with) and then reconciliation simply CONTINUES to the
//! next entry — this authority does NOT enter any special state and
//! does NOT exit before pointer discovery on account of it: a Stop's
//! effect is process exit, and a crash after admission means that
//! effect already happened, but a FRESH `supervise` invocation is a
//! FRESH operator intent, and honoring a stale stop against THIS run
//! would make the authority unstartable. The old operation id stays
//! answerable via `query` for whoever originally asked.
//! [`reset_inner`] (the no-supervisor CLI path) runs this SAME
//! reconciliation first too — not only
//! `supervise`'s own startup.
//!
//! # EndRun: the marker is never enough alone
//!
//! The capsule commits its run-end marker BEFORE teardown begins, and
//! the verifier tolerates an open chain tip — so a marker ALONE does not
//! prove the writer is gone. Every marker check is preceded by proving
//! the voyage pipe itself is unreachable (a LIVE process handle already
//! proves this by `wait()`; the recovery path, with no handle, probes
//! the pipe first). But pipe-absence ALONE is not writer-absence either: the capsule removes the pipe NAME before
//! its final writes, seal, and writer-fence release, so
//! [`probe_writer_liveness`] additionally proves `writer.lock` itself is
//! free (a bounded acquire-then-immediately-release) before ever
//! trusting pipe-silence. A writer proven `Alive`, or whose liveness is
//! `Ambiguous`, is neither `Ended` nor a pre-barrier failure — it is
//! `PendingWriter`, leaving the operation ACTIVE, completely untouched:
//! [`spawn_end_run`] retries it in a bounded loop on the SAME worker
//! thread (bounded from the OUTSIDE by `ENDING_WATCHDOG`, measured from
//! when `Ending` was FIRST entered, never reset by the retries), NEVER
//! respawning or releasing the hold over a writer that might still be
//! alive. Only a CONFIRMED-gone writer with no marker is
//! `PreBarrierFailed`: NOT ended — the hold releases and ordinary
//! respawn logic decides, live or recovered, identically. A post-barrier
//! VERIFICATION failure is STICKY `Terminal` regardless of voyage. The
//! lane's own reply for an accepted `end_run` is DEFERRED to the moment
//! `record_closed` is reached (ADR 0041:592) — held via
//! `(ConnId, operation_id)` correlation through `Ending`; a client
//! disconnecting meanwhile is fine, since the journal itself carries the
//! result for a later `query`. This deferred-reply signal is now passed
//! on EVERY live no-process reconciliation attempt (an
//! earlier version hardcoded `None` here, so a `pending_reply` could
//! wait forever once THIS path — not the with-process one — was what
//! actually closed the record). A generic mgmt-lane error (not merely
//! Foreign/Pending) still runs marker reconciliation rather than failing
//! outright. The proven process handle is KEPT
//! through `Ending`: an unresponsive mgmt lane gets a hard-stop
//! (terminate + wait) fallback rather than leaking a live, untracked
//! process.
//!
//! # Reset: one state, one worker, sticky failure
//!
//! `reset` is admissible ONLY from `EndedNoRespawn` — every other state
//! refuses it (busy, or stale from `Terminal`'s own stickiness). Its
//! execution — `reset_pointer`'s rename/bootstrap/publish — is a
//! background worker (`Resetting`), never inline inside the lane's own
//! command handling. A FAILED `reset_pointer` is `Terminal`: a
//! half-mutated pointer is exactly the "an operator must investigate"
//! condition this crate's own recovery refusal already names for a
//! third, unexplained identity — and this journal write's OWN failure is
//! never silently ignored either, logged loud even though
//! the severity is unchanged either way. The no-supervisor CLI path,
//! [`reset_inner`], is routed through this SAME journaled transaction
//! now too ("the same TRANSITION, not the same
//! CAPABILITIES", applied for real) — an earlier version called
//! `reset_pointer` directly with no journal entry at all, so a crash
//! mid rename left nothing for a later invocation to reconcile against;
//! it also refuses loud on a CORRUPT pointer unconditionally now, never
//! silently treating corruption as "no observed voyage" to re-mint past.
//! A resubmitted operation id/digest — for EVERY command family, Reset
//! included — resolves against the journal BEFORE voyage fencing: fencing FIRST meant a successful Reset's own id, replayed
//! after the voyage it changed FROM no longer matches the current one,
//! hit `stale_voyage` instead of reading back its own stored
//! `ResetDone`.
//!
//! # Stop is durable too
//!
//! `stop` begins and finishes through the SAME journal as
//! `end_run`/`reset` (`ActiveOp::Stop`, at-most-once, `id_conflict` on a
//! digest mismatch). `journal::finish` failures are never
//! ignored: a `stop` whose terminal write fails still honors the
//! operator's own intent (the process still stops) but reports the
//! failure and forces the exit code to `Terminal` severity rather than
//! silently claiming a clean shutdown that was never durably recorded.
//! The reply itself is delivery-gated (Sent, then a flush-grace window)
//! through the SAME two-stage close machinery a version-skew refusal
//! uses, before the process actually exits — see "Stop no longer owns a
//! Lifecycle state" above for how that gating actually works now.
//!
//! # The lane is never blocked by its OWN traffic either
//!
//! `service_lane` drains at most `LANE_EVENT_QUOTA` transport events per
//! tick — an earlier version drained the WHOLE channel unconditionally,
//! so sustained lane traffic could keep `service_lane` (and every
//! `handle_lane_bytes` call it triggers) running indefinitely, never
//! returning control to poll `Lifecycle` transitions, worker results,
//! watchdogs, or `Terminal`/stop exit conditions — verified NOT already
//! bounded by connection count alone: `lane/pipe_win/conn.rs`'s own `reader_loop`
//! is a tight, unpaced `ReadFile`-then-deliver-then-loop with nothing
//! gating how much a single SUSTAINED connection can push over an
//! extended span of wall-clock time, so `MAX_LANE_INSTANCES` (a
//! connection-COUNT cap) does not bound per-tick THROUGHPUT. Leftover
//! transport events simply stay queued in the transport's own channel
//! for a LATER tick — no extra bookkeeping needed, since each event is
//! already bounded to `transport::READ_BUF_LEN` (64 KiB) by the
//! transport, which means this ONE cap already transitively bounds
//! per-tick FRAME-processing work too.

//! # Linux (L1-unix LU3c, ADR 0043 decision 21)
//!
//! On Linux the supervisor's legs are its OWN CHILDREN, so someone must
//! reap them — a role Windows' job-object/handle model has no analogue
//! for. Rule: **`SIGCHLD` is SET to `SIG_DFL` here, at the very start of
//! [`supervise_inner`] (never merely assumed — whatever launched this
//! process may have inherited `SIG_IGN` across `exec`, which auto-reaps
//! every child immediately and silently breaks the pid pin below; never
//! `SIG_IGN` ourselves either, for the identical reason: an ignored
//! `SIGCHLD` re-opens the pid-reuse window `pidfd_open` right after
//! `Command::spawn` depends on staying closed, and would be inherited by
//! the leg across `exec`), and a leg is reaped exactly once per handle
//! that observes its exit, after this process has read everything it
//! needs from the dead process — never eagerly, never via a global
//! `waitpid(-1, ..)` reaper.** `supervisor::probe::unix::SpawnedChild::wait` reaps the
//! moment it observes the exit (nothing further Stage A needs to read
//! off it); a leg identified by a [`challenge_unix::ChallengedProcess`]
//! (adopted, or promoted from a `SpawnedChild` once its own pipe
//! answers) instead reaps via the explicit, owner-called
//! `ChallengedProcess::reap()`, once this process's own main loop has
//! observed the exit (`wait` returned `true`) and read everything it
//! needs — never implicitly on drop (dropping only closes the pidfd, via
//! `OwnedFd`'s own `Drop`). `ECHILD` at every one of these is ignored: a
//! leg inherited from a DIFFERENT, earlier supervisor is not this
//! process's child at all — its own parent reaps it, not us.
//!
//! The parent-death lease (`LegLease`/[`SpawnLease`], replacing the
//! Windows-only `lease_win` module here) is a close-on-exec pipe
//! (pipe2(O_CLOEXEC) on Linux; pipe plus checked fcntl on macOS, whose creation-to-flagging window remains): this
//! process holds the WRITE end for its whole life and never writes to
//! it; the read end reaches the leg as `--parent-lease-fd 3`, installed
//! by [`build_run_command`]'s own `pre_exec` (a `dup2` onto the fixed fd,
//! which clears `CLOEXEC` on the COPY — the wanted effect; when the read
//! end already IS fd 3, `dup2(fd, fd)` would be a no-op that leaves the
//! flag SET, so that one case clears it with `fcntl` instead). The leg
//! itself is detached from this process's own kill domain entirely by
//! the producer's `setsid` (ADR 0043 decision 14) — no Unix analogue of
//! `DETACHED_PROCESS` is needed, or exists.
//!
//! The platform is chosen exactly ONCE, by three local type aliases
//! (`Client`/`Process`/`Lane`, just below the imports) over
//! [`crate::lane::client::PlatformEndpoint`] and
//! [`crate::lane::transport::PlatformLaneServer`] — never by threading a type
//! parameter through the state machine above. `Lane`/`Client`/`Process`
//! resolve to concrete platform types (`PipeServer`/`PipeClient`/
//! `challenge_win::ChallengedProcess` on Windows,
//! `SocketServer`/`SocketClient`/`challenge_unix::ChallengedProcess` on
//! Linux) whose own inherent methods already implement
//! [`crate::lane::transport::LaneServer`]/[`crate::lane::client::Client`]/
//! [`crate::lane::client::PeerProcess`] purely by delegation (see `client.rs`
//! and each concrete module's own `impl LaneServer`/`impl Client` block)
//! — this file calls those inherent methods directly, exactly as it did
//! against the concrete Windows types before this lane, so the seam
//! traits stay the CONTRACT the two concrete types are already proven
//! to satisfy identically, without this state machine itself needing to
//! be generic in the type-parameter sense. Every `pipe_win::connect_*`/
//! `challenge_win::challenge` call this file used to make instead goes
//! through `PlatformEndpoint::default().method(..)` (ADR 0045 decision 5:
//! an `Endpoint` is a value, so its four trait functions take `&self` —
//! `PlatformEndpoint` is a type alias, so its own unit value is reached
//! via `default()` rather than the alias name itself).
//! [`crate::lane::transport::TransportError::is_endpoint_absent`] is the
//! ONE absence predicate [`end_run_over_mgmt_lane`]/
//! [`probe_writer_liveness`] use on both platforms now, instead of a
//! Windows-shaped inline `NotFound` guard.

#![cfg(any(windows, target_os = "linux", target_os = "macos"))]

use crate::capsule::producer::ExitStatus;
use crate::host;
use crate::host::storage_exhaustion;
use crate::identity::challenge::ChallengeOutcome;
use crate::lane::attach_proto::ConnId;
use crate::lane::client::{Endpoint, PlatformEndpoint};
use crate::store::recovery::{self, LatestLegState};
use crate::store::segment::RetentionClass;
use crate::supervisor::journal::pointer::{self, PointerState};
use crate::supervisor::probe::classify::{self, ProbeOutcome};
use crate::supervisor::probe::leg_process::LegProcess;
#[cfg(target_os = "macos")]
use crate::supervisor::probe::macos::RealProbeOps;
#[cfg(target_os = "linux")]
use crate::supervisor::probe::unix::RealProbeOps;
#[cfg(windows)]
use crate::supervisor::probe::win::RealProbeOps;
// L1-unix LU3b: the client-side supervisor-lane helpers (and the shared
// error constructor) now live in `supervisor_client` -- the dependency
// points server -> client-helpers, the right way round (ADR 0043
// decision 20). This module's own production code keeps calling
// `err_state(..)` bare, unchanged at every call site.
use crate::attach_client::supervisor_client::err_state;
use crate::lane::transport::{LaneEvent, PlatformLaneServer, CONNECT_BOUND};
use crate::lane::wire::{
    self, DecodedFrame, SupervisorOp, SupervisorOperationState, SupervisorPhase, SupervisorReply,
    SupervisorRequest, Survival,
};
use crate::store::verify;
use crate::store::voyage::VoyageStore;
use std::collections::HashMap;
use std::fmt;
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

mod authority;
pub mod journal;
pub mod lease_win;
mod leg;
mod lifecycle;
mod main_loop;
mod oneshot;
pub mod probe;
mod storage;
mod transitions;
use authority::lane::*;
use authority::*;
use journal::end_run::*;
use journal::recover::*;
use journal::reset::*;
use leg::*;
use lifecycle::*;
use main_loop::*;
use oneshot::*;
use transitions::*;

// ---------------------------------------------------------------------
// L1-unix LU3c (ADR 0043 decisions 20/21): the platform, chosen ONCE.
// Every former `PipeClient`/`ChallengedProcess`/`PipeServer` name in this
// file now names one of these three aliases instead — "generic over
// LaneServer + Endpoint" without ever threading a type parameter through
// the state machine below. Named `Client`, not `Conn` (the brief's own
// suggestion): this file already has an unrelated `struct Conn` — the
// per-connection LANE bookkeeping `service_lane`/`handle_lane_bytes` key
// their `HashMap` by (splitter, hello_ok, pending_close) — predating this
// lane; reusing that name for the challenge/client type would collide
// with it, so this is named for what it actually is instead.
// ---------------------------------------------------------------------

// `Client` is named only by this module's own `#[cfg(any(test,
// feature = "test-support"))]` helpers below (every non-test caller
// gets a `Conn`/`Process` pair back from a function whose own return
// type already names it, never a bare local of this exact alias) — a
// plain `cargo build` therefore never mentions it by name, same
// "hoisted but not yet called on this cfg" shape `deadline.rs`/
// `host_handshake.rs` already use.
#[cfg_attr(not(any(test, feature = "test-support")), allow(dead_code))]
type Client = <PlatformEndpoint as Endpoint>::Client;
type Process = <PlatformEndpoint as Endpoint>::Process;
type Lane = PlatformLaneServer;

// ---------------------------------------------------------------------
// The numbers (ADR 0041 "The numbers, pinned here so no implementation
// invents them"). B is the ONE free number; every DERIVED row below is a
// formula over it, exactly as the ADR's own table states. B is
// PROVISIONAL until measured (60s today).
// ---------------------------------------------------------------------

/// B: the supported history bound.
const SUPPORTED_HISTORY_BOUND: Duration = Duration::from_secs(60);
const READINESS_CUTOFF: Duration = SUPPORTED_HISTORY_BOUND;
const PROBE_EPISODE: Duration = SUPPORTED_HISTORY_BOUND;
const STABILITY_INTERVAL: Duration = READINESS_CUTOFF;
const KILL_WAIT_BOUND: Duration = Duration::from_secs(10);
/// The interval between readiness/adopt-only probe rounds. ADR 0043
/// decision 27: now that an absent endpoint fails the connect
/// immediately instead of charging the full [`CONNECT_BOUND`], each
/// round is cheap again — 500 ms was sized back when a missed round
/// meant paying a 2 s dead connect on top of the interval; 250 ms keeps
/// the same shape (poll, sleep, poll) without that dead weight baked in.
const ATTEMPT_INTERVAL: Duration = Duration::from_millis(250);
const FLAP_THRESHOLD: u32 = 3;
const LANE_IDLE_DEADLINE: Duration = Duration::from_secs(5);
const MAX_LANE_INSTANCES: u32 = 8;
const MAIN_LOOP_POLL: Duration = Duration::from_millis(100);
/// Bounds
/// how long `service_lane`'s own event-drain loop runs per tick — an
/// earlier version drained the channel unconditionally, so sustained
/// lane traffic could starve `Lifecycle` polling, worker results,
/// watchdogs, and `Terminal` grace of their own turn indefinitely.
/// Leftover events stay queued in the transport's own channel, drained
/// on a LATER tick — never dropped, no extra bookkeeping needed: each
/// event is itself already bounded to `transport::READ_BUF_LEN` by the
/// transport, so this ONE cap already transitively bounds per-tick
/// frame-processing work too. Not an ADR-pinned
/// number, same "reasoned, not pinned" status as `LANE_IDLE_DEADLINE`.
const LANE_EVENT_QUOTA: usize = 64;
const REFUSAL_SENT_DEADLINE: Duration = Duration::from_secs(2);
const REFUSAL_FLUSH_GRACE: Duration = Duration::from_millis(250);
/// `Terminal` may be reached with no client watching at all (a
/// flap-threshold breach has no operation anyone submitted) — this
/// bounds how long the loop lingers there serving whatever lane traffic
/// happens to arrive before exiting on its own, independent of any
/// further traffic. An explicit `stop` still ends it sooner.
const TERMINAL_EXIT_GRACE: Duration = Duration::from_secs(2);
/// A single, one-shot check of the voyage pipe (recovery's own "prove
/// the writer is gone" step, and the live path's Foreign/Pending
/// fallback) — the ADR's own "connect 2s" per-op budget, not a
/// multi-attempt episode: this is asking "is anyone there RIGHT NOW",
/// never "wait for it to come up".
const LIVENESS_PROBE_BUDGET: Duration = Duration::from_secs(2);
/// [`end_run_over_mgmt_lane`]'s own three per-attempt sub-bounds —
/// named so [`RECOVERY_WATCHDOG`]'s formula can
/// cite the real constants a delivery attempt is bound by instead of
/// re-deriving the same numbers as independent, driftable literals. The
/// challenge and write bounds match every other "2s" per-op budget in
/// this module (`LIVENESS_PROBE_BUDGET`, `CONNECT_BOUND`); the ack
/// read gets its own longer allowance because it waits on the CAPSULE's
/// own reply, not a bare OS call.
const END_RUN_CHALLENGE_BOUND: Duration = Duration::from_secs(2);
const END_RUN_WRITE_BOUND: Duration = Duration::from_secs(2);
const END_RUN_ACK_READ_BOUND: Duration = Duration::from_secs(5);
/// Margin added to each worker state's own known worst-case bound before
/// its operation watchdog fires — belt and
/// braces against a hang inside a call that SHOULD already be bounded by
/// its own internal deadline; not itself an ADR number.
const WATCHDOG_BUFFER: Duration = Duration::from_secs(10);
/// Bounds the WHOLE recovery worker (`reconcile_journal_on_startup`'s
/// `EndRun` arm, via [`reissue_and_reconcile_end_run`]), not merely the
/// wait-only reconcile — so its formula is the FULL sequential
/// worst-case path a legitimate, no-retry-needed recovery can legally
/// take, plus [`WATCHDOG_BUFFER`]:
/// [`CONNECT_BOUND`] (connect) + [`END_RUN_CHALLENGE_BOUND`]
/// (challenge) + [`END_RUN_WRITE_BOUND`] (shutdown write) +
/// [`END_RUN_ACK_READ_BOUND`] (ack read) — one
/// [`end_run_over_mgmt_lane`] attempt — + [`SUPPORTED_HISTORY_BOUND`] +
/// [`KILL_WAIT_BOUND`] (the confirmed-exit wait) + [`KILL_WAIT_BOUND`]
/// again (the hard-stop fallback's own wait) — both inside
/// [`finish_end_run_with_process`] — + [`WATCHDOG_BUFFER`] (also
/// covers the marker check and `verify_voyage`'s own O(retained-history)
/// walk, neither separately bounded). Does NOT multiply the delivery
/// bound by a retry count: [`reissue_and_reconcile_end_run`]'s own
/// retry loop is intentionally bounded from OUTSIDE, by this watchdog,
/// exactly like the pre-existing `PendingWriter` retry it now shares a
/// loop with — an operation still genuinely undetermined after this
/// budget stays `.active` for a LATER pass, never silently abandoned.
const RECOVERY_WATCHDOG: Duration = Duration::from_secs(
    CONNECT_BOUND.as_secs()
        + END_RUN_CHALLENGE_BOUND.as_secs()
        + END_RUN_WRITE_BOUND.as_secs()
        + END_RUN_ACK_READ_BOUND.as_secs()
        + SUPPORTED_HISTORY_BOUND.as_secs()
        + KILL_WAIT_BOUND.as_secs()
        + KILL_WAIT_BOUND.as_secs()
        + WATCHDOG_BUFFER.as_secs(),
);
const INITIAL_PROBE_WATCHDOG: Duration =
    Duration::from_secs(PROBE_EPISODE.as_secs() + WATCHDOG_BUFFER.as_secs());
const SPAWNING_WATCHDOG: Duration = Duration::from_secs(
    READINESS_CUTOFF.as_secs() + KILL_WAIT_BOUND.as_secs() + WATCHDOG_BUFFER.as_secs(),
);
const ENDING_WATCHDOG: Duration = Duration::from_secs(
    SUPPORTED_HISTORY_BOUND.as_secs() + KILL_WAIT_BOUND.as_secs() + WATCHDOG_BUFFER.as_secs(),
);
/// Reset's own file work (rename, bootstrap, publish) is a handful of
/// fsyncs — not ADR-pinned, generous but bounded, matching
/// `LANE_IDLE_DEADLINE`'s own "reasoned, not pinned" status.
const RESETTING_WATCHDOG: Duration = Duration::from_secs(30);
/// The `reason` recovery re-issues an `EndRun` with when reconciling a
/// journal entry whose worker never reached
/// [`end_run_over_mgmt_lane`] before this authority died. The journal's
/// own `ActiveOp::EndRun` does not carry the original requester's
/// reason (only `voyage`/`epoch` — see its own doc), and
/// `producer_dead.detail.reason` stays a free-form diagnostic per ADR
/// 0041, never a discriminator, so a fixed, honest label is exactly as
/// informative as reconstructing one would be.
const RECOVERY_END_RUN_REASON: &str = "recovered";

/// Exit codes are the launcher's own contract (ADR 0041 Lifecycle
/// "Supervisor exit codes", amended for [`EXIT_CONTENDED`] -- see that
/// const's own doc): `0` = clean end, do not restart; `69` = terminal, do
/// not restart, surface it; `70` = the authority fence was already held
/// by a LIVE supervisor -- a launcher should re-probe for adoption, never
/// treat it as a failure or a crash; anything else (this module never
/// returns anything else on purpose) is read by the launcher as a crash
/// to restart with `--resume`.
pub const EXIT_CLEAN: i32 = 0;
pub const EXIT_TERMINAL: i32 = 69;
/// Fence contention, distinct from [`EXIT_TERMINAL`]: `supervise_inner` reaches
/// this ONLY when `crate::supervisor::journal::fence::lock_supervisor` fails with
/// `Error::State` -- the one error that specific call can produce, and
/// only when a bounded retry against an ALREADY-HELD lock finally times
/// out (`host::lock_writer`'s own "lock held by another process").
/// That means some OTHER process currently holds `supervisor.lock` --
/// almost always the previous authority for this SAME state dir, still
/// finishing its own teardown (it drops its lane BEFORE releasing the
/// fence, and that teardown is bounded by
/// `transport::TEARDOWN_AGGREGATE_DEADLINE`, up to 20s) -- never a
/// genuinely exhausted producer. A launcher that folded this into
/// [`EXIT_TERMINAL`] would mark a perfectly healthy workspace terminal
/// out from under a run the OTHER process is still actively serving. The
/// correct reaction is to re-probe the lane for a short bound and ADOPT
/// it once it answers, exactly as a daemon boot that finds the SAME lane
/// already alive would -- never spawn a THIRD contender, never give up
/// loudly.
pub const EXIT_CONTENDED: i32 = 70;

// ---------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    Start,
    Resume,
}

pub struct SuperviseConfig {
    pub state_dir: PathBuf,
    pub mode: StartMode,
    pub producer_argv: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    pub assume_no_rollback_target: bool,
    /// ADR 0042 slice L1a: the spawner's own
    /// breakaway outcome, supplied here rather than inferred (ADR 0041
    /// decision 11: "Survival is supplied, never inferred... Deriving it
    /// from `IsProcessInJob` observation would cross the ADR's
    /// observation-is-not-authority line"). Threaded into every leg this
    /// authority spawns (`build_run_command`'s own `--survival` flag) so
    /// `status_ok.survival` and the sealed voyage's own record are
    /// truthful for a supervisor itself spawned DEGRADED (still inside
    /// its own parent's job, because the parent's breakaway attempt was
    /// denied) — the marker must be RECORDED, not merely logged.
    /// Defaults to `Normal` for every existing manual invocation that
    /// predates this field.
    pub survival: Survival,
    /// Tokens (`sot-capsule supervise`'s repeatable `--first-leg-without
    /// <token>`) stripped from `producer_argv` for the very first leg THIS
    /// PROCESS spawns, and again for any leg that follows one
    /// [`leg_was_stable`] classified unstable — the self-heal that lets a
    /// producer flag failing fast on stale argv (e.g. an agent's
    /// `--continue` against a store with nothing to continue) get one
    /// fresh retry rather than flapping the row terminal. Never applied
    /// after a STABLE leg's respawn, a leg after a reset, or a later
    /// voyage: [`respawn_or_terminal`] and the spawn sites for those cases
    /// read this list too, but only under the exact conditions each names.
    /// This module stays agent-agnostic: it knows nothing about `claude`
    /// or `--continue`, only that the caller wants some tokens gone from
    /// an unstable leg's own argv.
    pub first_leg_without: Vec<String>,
}

/// `sot-capsule supervise`'s own entry point — never panics by design;
/// every expected failure maps to [`EXIT_TERMINAL`], every success path
/// to [`EXIT_CLEAN`].
pub fn supervise(config: SuperviseConfig) -> i32 {
    // Defect fix (field-proven, see `winhandle`'s module doc): harden this
    // process's own inherited stdin/stdout before the first leg spawn, so
    // `build_run_command`'s default-inherit stdio never carries anything
    // past its own, intentionally-shared stderr (decision 25). Non-fatal.
    #[cfg(windows)]
    if let Err(e) = crate::host::winhandle::harden_own_stdio(false) {
        note(format_args!(
            "could not harden inherited stdin/stdout ({e}); continuing"
        ));
    }
    if !config.assume_no_rollback_target {
        note(format_args!(
            "no rollout evidence available — this build cannot open a feature-bearing segment \
             until U4's release-apply transaction supplies real evidence (ADR 0041 \"Upgrade and \
             version skew\"). Pass --assume-no-rollback-target to override for pre-U4 operation."
        ));
        return EXIT_TERMINAL;
    }
    match supervise_inner(config) {
        Ok(code) => code,
        Err(e) => {
            note(format_args!("{e}"));
            EXIT_TERMINAL
        }
    }
}

/// `sot-capsule endrun`'s own entry point (the no-supervisor path).
pub fn endrun(state_dir: &Path, voyage: Option<String>, reason: String) -> i32 {
    match endrun_inner(state_dir, voyage, reason) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sot-capsule endrun: {e}");
            EXIT_TERMINAL
        }
    }
}

/// `sot-capsule reset`'s own entry point (the no-supervisor path).
pub fn reset(state_dir: &Path, voyage: Option<String>) -> i32 {
    match reset_inner(state_dir, voyage) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sot-capsule reset: {e}");
            EXIT_TERMINAL
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn connect_and_challenge_with_build_for_test(
    h: &str,
    build: &str,
) -> crate::Result<(Client, ChallengeOutcome<Process>)> {
    let conn = PlatformEndpoint::default().connect_supervisor_unchallenged(h)?;
    let mut exchange = crate::identity::exchange::SupervisorLaneExchange::new(build.to_string());
    let outcome = PlatformEndpoint::default().challenge(
        &conn,
        &mut exchange,
        Instant::now() + Duration::from_secs(2),
    );
    Ok((conn, outcome))
}

/// ADR 0045 decision 7: the lane gate is the `proto` integer, not
/// `build` (see [`connect_and_challenge_with_build_for_test`] above, now
/// kept only to prove a different build id is fine) — this is the one
/// way to drive a genuine proto mismatch in a test.
#[cfg(any(test, feature = "test-support"))]
pub fn connect_and_challenge_with_proto_for_test(
    h: &str,
    proto: u32,
) -> crate::Result<(Client, ChallengeOutcome<Process>)> {
    let conn = PlatformEndpoint::default().connect_supervisor_unchallenged(h)?;
    let mut exchange = crate::identity::exchange::SupervisorLaneExchange::with_proto_for_test(
        crate::identity::exchange::SUPERVISOR_LANE_BUILD_ID,
        proto,
    );
    let outcome = PlatformEndpoint::default().challenge(
        &conn,
        &mut exchange,
        Instant::now() + Duration::from_secs(2),
    );
    Ok((conn, outcome))
}

/// L1-unix LU3b: [`crate::attach_client::supervisor_client::connect_and_challenge`] is
/// now the production analog of
/// [`connect_and_challenge_with_build_for_test`] — connect the supervisor
/// lane by state-dir hash and run the full same-connection challenge with
/// THIS build's own identity, generic over `client::Endpoint` since a
/// production caller off Windows now exists too
/// ([`crate::attach_client::supervisor_client`], ADR 0042 L1a / ADR 0043 decision 20).
/// L1-unix LU3c: this test helper instantiates it at [`PlatformEndpoint`]
/// now too — the SAME alias `supervisor/`'s own production code is
/// generic over, rather than hard-coding `pipe_win::PipeEndpoint` the way
/// it did while this module was still Windows-only.
#[cfg(any(test, feature = "test-support"))]
pub fn connect_and_challenge_for_test(h: &str) -> crate::Result<(Client, Process)> {
    crate::attach_client::supervisor_client::connect_and_challenge::<PlatformEndpoint>(
        &PlatformEndpoint::default(),
        h,
        crate::identity::exchange::SUPERVISOR_LANE_BUILD_ID,
        Instant::now() + Duration::from_secs(2),
    )
}

/// L1-unix LU3b: [`crate::attach_client::supervisor_client::send_and_read`] is now the
/// production analog this test-only wrapper delegates to — one
/// request/reply round trip every supervisor-lane caller needs after its
/// own connect+challenge, generic over `client::Client` so both
/// [`request_for_test`] and [`crate::attach_client::supervisor_client`]'s own production
/// callers share one implementation rather than two that could drift.
#[cfg(any(test, feature = "test-support"))]
pub fn request_for_test(
    conn: &Client,
    request: &SupervisorRequest,
    deadline: Instant,
) -> crate::Result<SupervisorReply> {
    crate::attach_client::supervisor_client::send_and_read(conn, request, deadline)
}

// ---------------------------------------------------------------------
// Small shared helpers
// ---------------------------------------------------------------------

fn voyages_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("voyages")
}

/// `pub` (ADR 0043 decision 33): exported for the daemon's own leg-absence
/// probe (`sot-backend`'s `rows::run::end_run::leg_absent`) — the
/// destroy proof's LEG half needs the exact same voyage root this module
/// uses internally, never a second, possibly-drifting derivation.
pub fn voyage_root_path(state_dir: &Path, voyage_id: &str) -> PathBuf {
    voyages_dir(state_dir).join(voyage_id)
}

/// Truncate `detail` to fit within [`wire::MAX_SUPERVISOR_STRING_LEN`]
/// bytes, on a UTF-8 boundary. Finds the boundary BEFORE truncating:
/// `String::truncate` itself panics
/// if the cut point splits a codepoint.
fn bounded_detail(detail: impl Into<String>) -> String {
    let mut s = detail.into();
    if s.len() > wire::MAX_SUPERVISOR_STRING_LEN {
        let mut cut = wire::MAX_SUPERVISOR_STRING_LEN;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

fn digest_of(op: &SupervisorOp) -> crate::Result<String> {
    use sha2::{Digest as _, Sha256};
    let bytes = wire::canonical_supervisor_op_bytes(op).map_err(|e| err_state(format!("{e}")))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn mint_aside_name() -> crate::Result<String> {
    let mut nonce_bytes = [0u8; 8];
    getrandom::fill(&mut nonce_bytes).map_err(std::io::Error::from)?;
    let nonce = u64::from_le_bytes(nonce_bytes);
    Ok(format!("drawer.voyage.reset-{nonce:016x}"))
}

/// ADR 0043 decision 25: one process-global prefix, set ONCE (the first
/// statement of [`supervise_inner`], ahead of everything else including
/// the SIGCHLD reset) to `"sot-capsule supervise[<state dir basename>]"`
/// — the workspace id, so concurrent legs sharing the daemon's ONE log
/// file (many workspaces, one `sotd.log`) are distinguishable. Read by
/// [`note`] below; unset for a bare `sot-capsule endrun`/`reset`
/// invocation (no supervisor authority ever runs in that process), which
/// falls back to the bare `"sot-capsule supervise"` text — the literal,
/// pre-existing wording [`do_reset`]'s own diagnostic already used
/// regardless of caller, preserved verbatim rather than invented.
static NOTE_PREFIX: OnceLock<String> = OnceLock::new();

/// Format one complete diagnostic line and issue it as ONE `write_all` on
/// stderr — never `eprintln!`'s own piecewise writes. `O_APPEND` (the
/// daemon's `sotd.log`, opened fresh per spawn by
/// `rows::spawn::detach::spawn_detached_supervisor`) keeps a single
/// `write(2)` intact between processes appending to the SAME file;
/// nothing else does, so every line from the supervise path goes through
/// this instead of `eprintln!`. Best-effort (a write failure here has no
/// further fallback — the same posture `eprintln!` itself has).
fn note(args: fmt::Arguments<'_>) {
    let prefix = NOTE_PREFIX
        .get()
        .map(String::as_str)
        .unwrap_or("sot-capsule supervise");
    let line = format!("{prefix}: {args}\n");
    let _ = std::io::stderr().write_all(line.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voyage_root_path_is_scoped_under_a_voyages_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let root = voyage_root_path(dir.path(), "abc");
        assert_eq!(root, dir.path().join("voyages").join("abc"));
    }
    #[test]
    fn digest_of_is_stable_and_distinguishes_ops() {
        let a = SupervisorOp::Stop;
        let b = SupervisorOp::Stop;
        let c = SupervisorOp::EndRun {
            reason: "r".into(),
            voyage: "v".into(),
        };
        assert_eq!(digest_of(&a).unwrap(), digest_of(&b).unwrap());
        assert_ne!(digest_of(&a).unwrap(), digest_of(&c).unwrap());
    }

    #[test]
    fn bounded_detail_truncates_on_a_char_boundary() {
        let long: String = "é".repeat(wire::MAX_SUPERVISOR_STRING_LEN); // 2 bytes each
        let truncated = bounded_detail(long);
        assert!(truncated.len() <= wire::MAX_SUPERVISOR_STRING_LEN);
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    /// `String::truncate` panics if
    /// the cut point splits a codepoint. A 3-byte codepoint repeated
    /// enough times to exceed 128 bytes puts byte 128 strictly INSIDE a
    /// character, unlike the `é` (2 bytes) case above where byte 128
    /// happens to land on a boundary regardless.
    #[test]
    fn bounded_detail_does_not_panic_when_a_char_straddles_byte_128() {
        let long: String = "€".repeat(50); // 3 bytes each = 150 bytes
        assert!(
            !long.is_char_boundary(wire::MAX_SUPERVISOR_STRING_LEN),
            "test setup must actually straddle byte 128"
        );
        let truncated = bounded_detail(long);
        assert!(truncated.len() <= wire::MAX_SUPERVISOR_STRING_LEN);
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }
}
