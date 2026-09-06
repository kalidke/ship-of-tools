//! ADR 0041 step 6 U2: the authority. `sot-capsule supervise` is the
//! process the launcher starts (Lifecycle "ONE AUTHORITY... Every act
//! that starts, ends, adopts or resets a run is performed by the process
//! holding `<state-dir>\supervisor.lock`"); [`endrun`] and [`reset`] are
//! the no-supervisor path's own fence-acquiring in-process callers ("the
//! same TRANSITION, not the same CAPABILITIES").
//!
//! # `Lifecycle` (Codex review round 2 rewrite; round 3 fixes below)
//!
//! One state machine, `Recovering -> InitialProbe -> {Ready, Spawning,
//! EndedNoRespawn}`, with `Ready <-> Spawning` (respawn), `Ready ->
//! Ending -> {EndedNoRespawn, Terminal}`, and `EndedNoRespawn -> Resetting
//! -> Spawning` — plus `Terminal` (STICKY: nothing ever transitions out
//! of it once entered; carries its own `entered_at`, replacing an
//! earlier separate `terminal_since` local the main loop tracked
//! alongside it — Codex review round 3 deletion candidate, applied).
//! Every OS-facing wait (probe episode, spawn readiness, end_run's
//! mgmt-lane exchange + process wait + O(history) verify, reset's
//! rename+bootstrap+publish) runs on its own background thread; the main
//! loop only ever polls a `Receiver` non-blockingly and services the
//! lane, on EVERY iteration, regardless of phase — "one linearized state
//! machine, not one blocking thread." Each worker-bearing state keeps its
//! own `JoinHandle<()>` and a `started_at` an operation watchdog
//! measures against; a worker panic is `Disconnected` on its receiver,
//! mapped to `Terminal` from WHATEVER state observes it, and a watchdog
//! EXPIRY (Codex review round 3, N7) never blocks the main thread in
//! `.join()` — it abandons the worker thread instead (see
//! `abandon_worker`), because a stuck worker is exactly the case a
//! blocking join on it would defeat the whole point of having a
//! watchdog at all.
//!
//! # Stop no longer owns a Lifecycle state (Codex review round 3, N4)
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
//! every other reply already uses (Codex review round 3 deletion
//! candidate: the former separate `StopReplyState` machine merged away)
//! — `handle_lane_bytes`'s own `CommandEffect::Stop` arm sends it
//! inline, delivery-gated exactly like a version-skew refusal is.
//!
//! # Recovery runs before pointer discovery (ADR 0041 "Recovery is part
//! of the transaction, and it runs FIRST"; Codex review round 2, B1) —
//! and a recovered Stop does NOT stop this authority (round 3, N5)
//!
//! `Recovering` reconciles every active journal entry — voyage-agnostic,
//! keyed off nothing but `<state_dir>` itself and each entry's OWN
//! recorded voyage — BEFORE the pointer is ever read to decide the
//! current voyage id. Reversing this order (an earlier version read the
//! pointer first) let a crash between a reset's journal admission and
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
//! reconciliation first too (round 3, N6, below) — not only
//! `supervise`'s own startup.
//!
//! # EndRun: the marker is never enough alone (Codex review round 2,
//! B3/B4; round 3 fixes below)
//!
//! The capsule commits its run-end marker BEFORE teardown begins, and
//! the verifier tolerates an open chain tip — so a marker ALONE does not
//! prove the writer is gone. Every marker check is preceded by proving
//! the voyage pipe itself is unreachable (a LIVE process handle already
//! proves this by `wait()`; the recovery path, with no handle, probes
//! the pipe first). But pipe-absence ALONE is not writer-absence either
//! (Codex review round 3, N2): the capsule removes the pipe NAME before
//! its final writes, seal, and writer-fence release, so
//! [`probe_writer_liveness`] additionally proves `writer.lock` itself is
//! free (a bounded acquire-then-immediately-release) before ever
//! trusting pipe-silence. A writer proven `Alive`, or whose liveness is
//! `Ambiguous`, is neither `Ended` nor a pre-barrier failure — it is
//! `PendingWriter` (round 3, N3: a former conflated `NotEnded` outcome
//! split in two), leaving the operation ACTIVE, completely untouched:
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
//! on EVERY live no-process reconciliation attempt (round 3, N8: an
//! earlier version hardcoded `None` here, so a `pending_reply` could
//! wait forever once THIS path — not the with-process one — was what
//! actually closed the record). A generic mgmt-lane error (not merely
//! Foreign/Pending) still runs marker reconciliation rather than failing
//! outright (B4's own bypass, closed). The proven process handle is KEPT
//! through `Ending`: an unresponsive mgmt lane gets a hard-stop
//! (terminate + wait) fallback rather than leaking a live, untracked
//! process.
//!
//! # Reset: one state, one worker, sticky failure (Codex review round 2,
//! B2; round 3 fixes below)
//!
//! `reset` is admissible ONLY from `EndedNoRespawn` — every other state
//! refuses it (busy, or stale from `Terminal`'s own stickiness). Its
//! execution — `reset_pointer`'s rename/bootstrap/publish — is a
//! background worker (`Resetting`), never inline inside the lane's own
//! command handling. A FAILED `reset_pointer` is `Terminal`: a
//! half-mutated pointer is exactly the "an operator must investigate"
//! condition this crate's own recovery refusal already names for a
//! third, unexplained identity — and this journal write's OWN failure is
//! never silently ignored either (round 3, B2), logged loud even though
//! the severity is unchanged either way. The no-supervisor CLI path,
//! [`reset_inner`], is routed through this SAME journaled transaction
//! now too (round 3, N6: "the same TRANSITION, not the same
//! CAPABILITIES", applied for real) — an earlier version called
//! `reset_pointer` directly with no journal entry at all, so a crash
//! mid rename left nothing for a later invocation to reconcile against;
//! it also refuses loud on a CORRUPT pointer unconditionally now, never
//! silently treating corruption as "no observed voyage" to re-mint past.
//! A resubmitted operation id/digest — for EVERY command family, Reset
//! included — resolves against the journal BEFORE voyage fencing (round
//! 3, N9): fencing FIRST meant a successful Reset's own id, replayed
//! after the voyage it changed FROM no longer matches the current one,
//! hit `stale_voyage` instead of reading back its own stored
//! `ResetDone`.
//!
//! # Stop is durable too (Codex review round 2, B5)
//!
//! `stop` begins and finishes through the SAME journal as
//! `end_run`/`reset` (`ActiveOp::Stop`, at-most-once, `id_conflict` on a
//! digest mismatch) — an earlier version let `stop` bypass the journal
//! entirely, so a REUSED operation id could later admit a conflicting
//! `reset` after a restart. `journal::finish` failures are never
//! ignored: a `stop` whose terminal write fails still honors the
//! operator's own intent (the process still stops) but reports the
//! failure and forces the exit code to `Terminal` severity rather than
//! silently claiming a clean shutdown that was never durably recorded.
//! The reply itself is delivery-gated (Sent, then a flush-grace window)
//! through the SAME two-stage close machinery a version-skew refusal
//! uses, before the process actually exits — see "Stop no longer owns a
//! Lifecycle state" above for how that gating actually works now.
//!
//! # The lane is never blocked by its OWN traffic either (Codex review
//! round 3, N10; owner-tightened to the one bound actually needed)
//!
//! `service_lane` drains at most `LANE_EVENT_QUOTA` transport events per
//! tick — an earlier version drained the WHOLE channel unconditionally,
//! so sustained lane traffic could keep `service_lane` (and every
//! `handle_lane_bytes` call it triggers) running indefinitely, never
//! returning control to poll `Lifecycle` transitions, worker results,
//! watchdogs, or `Terminal`/stop exit conditions — verified NOT already
//! bounded by connection count alone: `pipe_win.rs`'s own `reader_loop`
//! is a tight, unpaced `ReadFile`-then-deliver-then-loop with nothing
//! gating how much a single SUSTAINED connection can push over an
//! extended span of wall-clock time, so `MAX_LANE_INSTANCES` (a
//! connection-COUNT cap) does not bound per-tick THROUGHPUT. Leftover
//! transport events simply stay queued in the transport's own channel
//! for a LATER tick — no extra bookkeeping needed, since each event is
//! already bounded to `pipe_win::READ_BUF_LEN` (64 KiB) by the
//! transport, which means this ONE cap already transitively bounds
//! per-tick FRAME-processing work too. An earlier version of this fix
//! also queued decoded frames per-connection with a second, separate
//! quota and a sweep to drain leftovers — deleted once it became clear
//! that bought nothing this one cap did not already cover.

#![cfg(windows)]

use crate::challenge::{self, ChallengeOutcome, ChallengedProcess};
use crate::classify::{self, ProbeOutcome};
use crate::fsutil;
use crate::journal;
use crate::pipe_win::{self, ConnId, PipeServer, TransportEvent, PIPE_CONNECT_BOUND};
use crate::pointer::{self, PointerState};
use crate::probe::RealProbeOps;
use crate::recovery::{self, LatestLegState};
use crate::segment::RetentionClass;
use crate::verify;
use crate::voyage::VoyageStore;
use crate::wire::{
    self, DecodedFrame, Survival, SupervisorOp, SupervisorOperationState, SupervisorPhase, SupervisorReply,
    SupervisorRequest,
};
use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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
const ATTEMPT_INTERVAL: Duration = Duration::from_millis(500);
const FLAP_THRESHOLD: u32 = 3;
const LANE_IDLE_DEADLINE: Duration = Duration::from_secs(5);
const MAX_LANE_INSTANCES: u32 = 8;
const MAIN_LOOP_POLL: Duration = Duration::from_millis(100);
/// N10 (Codex review round 3, owner-tightened to this ONE cap): bounds
/// how long `service_lane`'s own event-drain loop runs per tick — an
/// earlier version drained the channel unconditionally, so sustained
/// lane traffic could starve `Lifecycle` polling, worker results,
/// watchdogs, and `Terminal` grace of their own turn indefinitely.
/// Leftover events stay queued in the transport's own channel, drained
/// on a LATER tick — never dropped, no extra bookkeeping needed: each
/// event is itself already bounded to `pipe_win::READ_BUF_LEN` by the
/// transport, so this ONE cap already transitively bounds per-tick
/// frame-processing work too (see `handle_lane_bytes`'s own doc — a
/// separate frame-level quota and queue were tried and then deleted,
/// buying nothing this one didn't already cover). Not an ADR-pinned
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
/// named (Codex review, PR #171) so [`RECOVERY_WATCHDOG`]'s formula can
/// cite the real constants a delivery attempt is bound by instead of
/// re-deriving the same numbers as independent, driftable literals. The
/// challenge and write bounds match every other "2s" per-op budget in
/// this module (`LIVENESS_PROBE_BUDGET`, `PIPE_CONNECT_BOUND`); the ack
/// read gets its own longer allowance because it waits on the CAPSULE's
/// own reply, not a bare OS call.
const END_RUN_CHALLENGE_BOUND: Duration = Duration::from_secs(2);
const END_RUN_WRITE_BOUND: Duration = Duration::from_secs(2);
const END_RUN_ACK_READ_BOUND: Duration = Duration::from_secs(5);
/// Margin added to each worker state's own known worst-case bound before
/// its operation watchdog fires (Codex review round 2, M2) — belt and
/// braces against a hang inside a call that SHOULD already be bounded by
/// its own internal deadline; not itself an ADR number.
const WATCHDOG_BUFFER: Duration = Duration::from_secs(10);
/// Bounds the WHOLE recovery worker (`reconcile_journal_on_startup`'s
/// `EndRun` arm, via [`reissue_and_reconcile_end_run`]), not merely the
/// wait-only reconcile — so its formula is the FULL sequential
/// worst-case path a legitimate, no-retry-needed recovery can legally
/// take (Codex review, PR #171: the previous formula omitted the
/// delivery attempt entirely, so a genuinely slow-but-legal recovery
/// could hit this watchdog before its own terminal journal record was
/// durable), plus [`WATCHDOG_BUFFER`]:
/// [`PIPE_CONNECT_BOUND`] (connect) + [`END_RUN_CHALLENGE_BOUND`]
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
    PIPE_CONNECT_BOUND.as_secs()
        + END_RUN_CHALLENGE_BOUND.as_secs()
        + END_RUN_WRITE_BOUND.as_secs()
        + END_RUN_ACK_READ_BOUND.as_secs()
        + SUPPORTED_HISTORY_BOUND.as_secs()
        + KILL_WAIT_BOUND.as_secs()
        + KILL_WAIT_BOUND.as_secs()
        + WATCHDOG_BUFFER.as_secs(),
);
const INITIAL_PROBE_WATCHDOG: Duration = Duration::from_secs(PROBE_EPISODE.as_secs() + WATCHDOG_BUFFER.as_secs());
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
/// Fence contention, distinct from [`EXIT_TERMINAL`] (round-2 Codex
/// finding, daemon-boot-adopts-supervisor fix): `supervise_inner` reaches
/// this ONLY when `crate::fence::lock_supervisor` fails with
/// `Error::State` -- the one error that specific call can produce, and
/// only when a bounded retry against an ALREADY-HELD lock finally times
/// out (`fsutil::lock_writer`'s own "lock held by another process").
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
    /// ADR 0042 slice L1a (Codex review finding 7): the spawner's own
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
}

/// `sot-capsule supervise`'s own entry point — never panics by design;
/// every expected failure maps to [`EXIT_TERMINAL`], every success path
/// to [`EXIT_CLEAN`].
pub fn supervise(config: SuperviseConfig) -> i32 {
    if !config.assume_no_rollback_target {
        eprintln!(
            "sot-capsule supervise: no rollout evidence available — this build cannot open a \
             feature-bearing segment until U4's release-apply transaction supplies real evidence \
             (ADR 0041 \"Upgrade and version skew\"). Pass --assume-no-rollback-target to \
             override for pre-U4 operation."
        );
        return EXIT_TERMINAL;
    }
    match supervise_inner(config) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sot-capsule supervise: {e}");
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
) -> crate::Result<(pipe_win::PipeClient, ChallengeOutcome<ChallengedProcess>)> {
    let conn = pipe_win::connect_supervisor_pipe_unchallenged(h)?;
    let mut exchange = crate::exchange::SupervisorLaneExchange::new(build.to_string());
    let outcome = challenge::challenge(&conn, &mut exchange, Instant::now() + Duration::from_secs(2));
    Ok((conn, outcome))
}

/// The production analog of [`connect_and_challenge_with_build_for_test`]:
/// connect the supervisor lane by state-dir hash and run the full
/// same-connection challenge with THIS build's own identity, folding
/// `Foreign`/`Undetermined` straight into `Err` — a production caller
/// (today: [`crate::supervisor_client`], ADR 0042 L1a) has no use for
/// telling those two apart any further than "not a proven connection to
/// my own supervisor". `pub(crate)` so that sibling module can reach it
/// without the `test-support` feature a normal, non-test consumer must
/// never need to enable (see this crate's own `Cargo.toml`).
pub(crate) fn connect_and_challenge(
    h: &str,
    build: &str,
    deadline: Instant,
) -> crate::Result<(pipe_win::PipeClient, ChallengedProcess)> {
    let conn = pipe_win::connect_supervisor_pipe_unchallenged(h)?;
    let mut exchange = crate::exchange::SupervisorLaneExchange::new(build.to_string());
    match challenge::challenge(&conn, &mut exchange, deadline) {
        ChallengeOutcome::Proven(process) => Ok((conn, process)),
        ChallengeOutcome::Foreign => Err(err_state("supervisor lane challenge: foreign")),
        ChallengeOutcome::Undetermined => Err(err_state("supervisor lane challenge: undetermined")),
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn connect_and_challenge_for_test(h: &str) -> crate::Result<(pipe_win::PipeClient, ChallengedProcess)> {
    connect_and_challenge(
        h,
        crate::exchange::SUPERVISOR_LANE_BUILD_ID,
        Instant::now() + Duration::from_secs(2),
    )
}

/// Encode `request`, write it, and read back exactly one reply — the one
/// request/reply round trip every supervisor-lane caller needs after its
/// own connect+challenge, factored out so [`request_for_test`] (test-only)
/// and [`crate::supervisor_client`] (the production, non-test-gated
/// caller) share one implementation rather than two that could drift.
pub(crate) fn send_and_read(
    conn: &pipe_win::PipeClient,
    request: &SupervisorRequest,
    deadline: Instant,
) -> crate::Result<SupervisorReply> {
    let bytes = wire::encode_supervisor_request(request).map_err(|e| err_state(format!("{e}")))?;
    conn.write_all(&bytes)?;
    match read_one_frame(conn, deadline)? {
        DecodedFrame::SupervisorReply(reply) => Ok(reply),
        other => Err(err_state(format!("expected a SupervisorReply, got {other:?}"))),
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn request_for_test(
    conn: &pipe_win::PipeClient,
    request: &SupervisorRequest,
    deadline: Instant,
) -> crate::Result<SupervisorReply> {
    send_and_read(conn, request, deadline)
}

// ---------------------------------------------------------------------
// Small shared helpers
// ---------------------------------------------------------------------

fn voyages_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("voyages")
}

fn voyage_root_path(state_dir: &Path, voyage_id: &str) -> PathBuf {
    voyages_dir(state_dir).join(voyage_id)
}

pub fn state_dir_hash(state_dir: &Path) -> String {
    use sha2::{Digest as _, Sha256};
    let canonical = std::fs::canonicalize(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn self_pid_and_created() -> std::io::Result<(u32, u64)> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetProcessTimes};
    unsafe {
        let pid = GetCurrentProcessId();
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        if GetProcessTimes(GetCurrentProcess(), &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let created = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        Ok((pid, created))
    }
}

/// `pub(crate)`: [`crate::supervisor_client`] (ADR 0042 L1a) reuses this
/// same "malformed/unexpected protocol shape" error shape rather than
/// minting a second one.
pub(crate) fn err_state(msg: impl Into<String>) -> crate::Error {
    crate::Error::State(msg.into())
}

/// Truncate `detail` to fit within [`wire::MAX_SUPERVISOR_STRING_LEN`]
/// bytes, on a UTF-8 boundary. Finds the boundary BEFORE truncating
/// (Codex review round 2, finding M4): `String::truncate` itself panics
/// if the cut point splits a codepoint — an earlier version truncated
/// first and "fixed" the boundary after, which never ran when the panic
/// already fired.
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
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn mint_aside_name() -> crate::Result<String> {
    let mut nonce_bytes = [0u8; 8];
    getrandom::fill(&mut nonce_bytes).map_err(std::io::Error::from)?;
    let nonce = u64::from_le_bytes(nonce_bytes);
    Ok(format!("drawer.voyage.reset-{nonce:016x}"))
}

fn join_and_warn(handle: JoinHandle<()>, what: &str) {
    if let Err(panic) = handle.join() {
        eprintln!("sot-capsule supervise: the {what} worker thread panicked: {panic:?}");
    }
}

/// N7 (Codex review round 3): "watchdogs are not deadlines" — at
/// WATCHDOG EXPIRY specifically (never at ordinary happy-path
/// completion, where the worker has already sent its result and
/// `join_and_warn` is a near-instant unwind, not a real block), the
/// main thread must NEVER block waiting for a worker that timed out
/// precisely because it might still be running arbitrarily long. This
/// drops the handle WITHOUT joining it — the thread keeps running
/// detached until it finishes on its own or the whole process exits
/// (harmless either way once `Terminal` is reached: see
/// `force_terminal`'s own comment on the SAME tradeoff) — which is what
/// makes the watchdog an actual DEADLINE rather than a number that gets
/// silently defeated by the very `.join()` meant to enforce it.
fn abandon_worker(handle: JoinHandle<()>, what: &str) {
    eprintln!(
        "sot-capsule supervise: the {what} worker's own watchdog expired; abandoning its thread \
         WITHOUT waiting for it (N7) — it will exit on its own or be torn down with the process"
    );
    drop(handle);
}

fn watchdog_expired(started_at: Instant, bound: Duration, now: Instant) -> bool {
    now.saturating_duration_since(started_at) >= bound
}

// ---------------------------------------------------------------------
// Pointer discovery / mint (supervisor startup only)
// ---------------------------------------------------------------------

fn discover_or_mint_voyage(state_dir: &Path, mode: StartMode) -> crate::Result<String> {
    match pointer::validate(state_dir) {
        PointerState::Valid(id) => Ok(id),
        PointerState::NotFound => match mode {
            StartMode::Start => {
                std::fs::create_dir_all(voyages_dir(state_dir))?;
                let id = uuid::Uuid::now_v7().to_string();
                VoyageStore::bootstrap(&voyage_root_path(state_dir, &id), &id, RetentionClass::Archive)?;
                pointer::publish(state_dir, &id)?;
                Ok(id)
            }
            StartMode::Resume => Err(err_state(
                "--resume with no drawer.voyage pointer at all: nothing to resume",
            )),
        },
        PointerState::Corrupt => Err(err_state("drawer.voyage is corrupt — run `sot-capsule reset`")),
        PointerState::OtherIo(e) => Err(e.into()),
    }
}

/// The start-mode table's OWN "what to do about the latest leg" half,
/// consulted ONLY when no live capsule was adopted. Checks the marker on
/// an UNSEALED leg too, not only a sealed one: the marker is written
/// mid graceful-teardown, before sealing completes.
fn should_spawn_after_absent(state_dir: &Path, voyage_id: &str, mode: StartMode) -> crate::Result<bool> {
    if mode == StartMode::Start {
        return Ok(true);
    }
    let seg_dir = voyage_root_path(state_dir, voyage_id).join("seg");
    match recovery::latest_leg_state(&seg_dir).map_err(crate::Error::Io)? {
        LatestLegState::NoLeg => Ok(true),
        LatestLegState::Sealed { epoch } | LatestLegState::Unsealed { epoch } => {
            let marked = verify::leg_carries_run_end_marker(&seg_dir, voyage_id, epoch)?;
            Ok(!marked)
        }
    }
}

fn leg_epoch_of(state_dir: &Path, voyage_id: &str) -> Option<u64> {
    let seg_dir = voyage_root_path(state_dir, voyage_id).join("seg");
    match recovery::latest_leg_state(&seg_dir) {
        Ok(LatestLegState::Unsealed { epoch }) => Some(epoch),
        _ => None,
    }
}

/// N1 (Codex review round 3): whether the CURRENT leg's own recorded
/// `producer_uptime_ms` (`verify::leg_producer_uptime_ms`) proves it
/// survived at least `STABILITY_INTERVAL` — the ONLY question the
/// anti-flap counter's reset now depends on. Fail-safe direction is
/// UNSTABLE (`false`) for every case that cannot prove otherwise:
/// no leg found, no `producer_dead` frame yet on it (unsealed, or a
/// spawn failure that never reached a real producer), the field absent,
/// or the segment itself unreadable (logged, but this is an anti-flap
/// HEURISTIC, not a safety-critical decision — never escalated past a
/// warning).
fn leg_was_stable(state_dir: &Path, voyage_id: &str) -> bool {
    let seg_dir = voyage_root_path(state_dir, voyage_id).join("seg");
    let epoch = match recovery::latest_leg_state(&seg_dir) {
        Ok(LatestLegState::Sealed { epoch } | LatestLegState::Unsealed { epoch }) => epoch,
        Ok(LatestLegState::NoLeg) => return false,
        Err(e) => {
            eprintln!(
                "sot-capsule supervise: could not read this leg's own state to judge stability \
                 ({e}); counting it unstable (N1 fail-safe)"
            );
            return false;
        }
    };
    match verify::leg_producer_uptime_ms(&seg_dir, voyage_id, epoch) {
        Ok(Some(ms)) => Duration::from_millis(ms) >= STABILITY_INTERVAL,
        Ok(None) => false,
        Err(e) => {
            eprintln!(
                "sot-capsule supervise: could not read this leg's own producer_uptime_ms ({e}); \
                 counting it unstable (N1 fail-safe)"
            );
            false
        }
    }
}

// `DETACHED_PROCESS`: `run` gets no console of its own. `supervise` has none
// either (the daemon spawns it detached — see `capsule_workspace.rs`), and a
// child of a console-less parent otherwise gets a brand-new console, which
// the user's default terminal adopts as a stray window. `run`'s ConPTY is a
// separate OS object for the agent child; its own stdio stays inherited.
const DETACHED_PROCESS: u32 = 0x0000_0008;

// ADR 0042 slice L1a added `survival` as an 8th parameter (Codex review
// finding 7) — matching this file's own existing precedent
// (`run_quit`-equivalent lane loops) for a constructor whose every
// parameter is load-bearing and independently documented at its call
// sites, rather than a struct that would only exist to satisfy this lint.
#[allow(clippy::too_many_arguments)]
fn build_run_command(
    capsule_exe: &Path,
    voyage_root: &Path,
    voyage_id: &str,
    cols: u16,
    rows: u16,
    lease_name: &str,
    survival: Survival,
    producer_argv: &[String],
) -> std::process::Command {
    let survival_flag = match survival {
        Survival::Normal => "normal",
        Survival::Degraded => "degraded",
    };
    let mut command = std::process::Command::new(capsule_exe);
    command
        .arg("run")
        .arg(voyage_root)
        .arg(voyage_id)
        .arg("--cols")
        .arg(cols.to_string())
        .arg("--rows")
        .arg(rows.to_string())
        .arg("--parent-lease-name")
        .arg(lease_name)
        .arg("--survival")
        .arg(survival_flag)
        .arg("--assume-no-rollback-target")
        .arg("--")
        .args(producer_argv)
        .creation_flags(DETACHED_PROCESS);
    command
}

// ---------------------------------------------------------------------
// EndRun over the voyage's own mgmt lane, and its reconciliation via the
// leg's own durable marker (ADR 0041; Codex review round 2, B3/B4)
// ---------------------------------------------------------------------

enum EndRunOutcome {
    Absent,
    Foreign,
    Pending,
    /// The challenge succeeded and the shutdown request reached the
    /// process. `Ended` covers BOTH a read-back ack AND an ack whose
    /// own write or read timed out/failed (Codex review round 3
    /// deletion candidate, applied — a former separate `AckUnknown`
    /// variant): every downstream caller already treated the two
    /// identically (`finish_end_run_with_process`'s own wait+marker
    /// sequence doesn't care whether the shutdown was CONFIRMED
    /// acknowledged or merely PROBABLY delivered — it proves the
    /// leg's actual outcome independently either way), so a distinct
    /// variant carried no decision either site ever made differently.
    Ended(ChallengedProcess),
}

fn read_one_frame(conn: &pipe_win::PipeClient, deadline: Instant) -> crate::Result<DecodedFrame> {
    let result = crate::deadline::run_with_deadline(
        deadline,
        || conn.cancel(),
        move || -> crate::Result<DecodedFrame> {
            let mut splitter = wire::FrameSplitter::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = conn.read(&mut buf)?;
                if n == 0 {
                    return Err(err_state("connection closed before a reply arrived"));
                }
                let (frames, err) = splitter.feed(&buf[..n]);
                if let Some(e) = err {
                    return Err(err_state(format!("wire error waiting for a reply: {e}")));
                }
                if let Some(frame) = frames.into_iter().next() {
                    return Ok(frame);
                }
            }
        },
    );
    result.unwrap_or_else(|| Err(err_state("timed out waiting for a reply")))
}

/// ADR 0041 capability matrix's "healthy" row, and EndRun "invoked by the
/// authority on its own behalf": challenge afresh, retain the handle,
/// send `shutdown{reason}` on the SAME connection, and wait its ack.
fn end_run_over_mgmt_lane(voyage_id: &str, reason: &str) -> crate::Result<EndRunOutcome> {
    let conn = match pipe_win::connect_voyage_pipe_unchallenged(voyage_id) {
        Ok(c) => c,
        Err(pipe_win::PipeError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(EndRunOutcome::Absent);
        }
        Err(e) => return Err(e.into()),
    };
    let mut exchange = crate::exchange::VoyageMgmtExchange::default();
    match challenge::challenge(&conn, &mut exchange, Instant::now() + END_RUN_CHALLENGE_BOUND) {
        ChallengeOutcome::Foreign => Ok(EndRunOutcome::Foreign),
        ChallengeOutcome::Undetermined => Ok(EndRunOutcome::Pending),
        ChallengeOutcome::Proven(process) => {
            let request = wire::encode_mgmt_request(&wire::MgmtRequest::Shutdown { reason: reason.to_string() })
                .map_err(|e| err_state(format!("encoding shutdown request: {e}")))?;
            // N7 (Codex review round 3): this write was UNBOUNDED --
            // the exchange machinery already has a cancellable deadline
            // primitive (`read_one_frame`, just below, already uses it
            // for its own read); reused here rather than a second one,
            // on the SAME "request write 2s" per-op budget every other
            // op already uses.
            let write_deadline = Instant::now() + END_RUN_WRITE_BOUND;
            let write_ok = crate::deadline::run_with_deadline(write_deadline, || conn.cancel(), || conn.write_all(&request))
                .is_some_and(|r| r.is_ok());
            if !write_ok {
                return Ok(EndRunOutcome::Ended(process));
            }
            // The ack itself is read for wire-protocol hygiene (drain
            // what the peer sends), but its outcome no longer branches
            // anything — see `Ended`'s own doc above.
            let _ = read_one_frame(&conn, Instant::now() + END_RUN_ACK_READ_BOUND);
            Ok(EndRunOutcome::Ended(process))
        }
    }
}

/// Whether the voyage's own mgmt-lane writer is still alive — the
/// question B3 requires answering BEFORE a marker check ever counts:
/// "recovery must first prove the writer is gone... only pipe-absent +
/// marker-present closes." A ONE-SHOT check (not a retry episode): this
/// asks "is anyone there RIGHT NOW", the same "connect 2s" per-op budget
/// every other op uses.
enum WriterLiveness {
    Alive,
    Absent,
    /// A wrong answer, or an OS-call failure — could not be determined
    /// either way. Treated the SAME as `Alive` by every caller: fail
    /// closed, never treat an ambiguous result as proof of absence.
    Ambiguous,
}

/// N2 (Codex review round 3): pipe absence is NOT writer absence. The
/// capsule removes the pipe NAME before its final writes, seal, and
/// writer-fence release (`capsule_win.rs`'s own teardown order) — so a
/// restarted supervisor that trusted pipe-silence alone could
/// `mark_closed`/`verify_voyage` an open chain tip while the original
/// writer still owns the fence and can append MORE history underneath
/// it. Once the pipe is proven absent, this additionally proves
/// `writer.lock` itself is free — the SAME bounded acquire-then-
/// immediately-release primitive `open_for_writing` uses
/// (`fsutil::lock_writer`, its own ~250ms bounded retry) — before ever
/// trusting the silence. A held fence still means a live writer
/// (`Ambiguous`, fail-closed, exactly like every other undetermined
/// case here); only a genuinely free fence reaches `Absent`.
fn probe_writer_liveness(state_dir: &Path, voyage_id: &str) -> WriterLiveness {
    let conn = match pipe_win::connect_voyage_pipe_unchallenged(voyage_id) {
        Ok(c) => c,
        Err(pipe_win::PipeError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            let root = voyage_root_path(state_dir, voyage_id);
            return match fsutil::lock_writer(&root.join("writer.lock")) {
                Ok(lock) => {
                    drop(lock); // released immediately, per the module's own convention
                    WriterLiveness::Absent
                }
                Err(_) => WriterLiveness::Ambiguous,
            };
        }
        Err(_) => return WriterLiveness::Ambiguous,
    };
    let mut exchange = crate::exchange::VoyageMgmtExchange::default();
    match challenge::challenge(&conn, &mut exchange, Instant::now() + LIVENESS_PROBE_BUDGET) {
        ChallengeOutcome::Proven(_) => WriterLiveness::Alive,
        ChallengeOutcome::Foreign | ChallengeOutcome::Undetermined => WriterLiveness::Ambiguous,
    }
}

/// What reconciling one `end_run` against the world concluded.
#[derive(Debug)]
enum EndRunReconciliation {
    /// Post-barrier (marker present), writer confirmed gone, verified.
    Ended,
    /// Pre-barrier: the writer is CONFIRMED gone (a wait()-confirmed
    /// exit, or [`probe_writer_liveness`] finding it `Absent`) but the
    /// marker never appeared — NOT ended: the hold releases, ordinary
    /// respawn/adopt logic decides, live or recovered, identically.
    /// `journal::finish` has ALREADY been called (`Failed{record_append}`).
    PreBarrierFailed,
    /// N3 (Codex review round 3): the writer is still `Alive`, or its
    /// liveness is `Ambiguous` — NEITHER `Ended` NOR `PreBarrierFailed`.
    /// The operation stays ACTIVE, untouched (no journal mutation at
    /// all) — never released, never respawned over. A live caller
    /// retries this same check in a bounded loop
    /// ([`spawn_end_run`]); a recovering caller simply leaves the entry
    /// `.active` for a LATER pass (this restart's own main loop already
    /// isn't reachable for an OLD entry — the NEXT restart, or a fresh
    /// `end_run`/`query` against this same id).
    PendingWriter,
}

/// The wait+marker+verify sequence for a LIVE caller holding a proven
/// process handle (an `Ended` outcome from [`end_run_over_mgmt_lane`]).
/// The wait result GATES `mark_closed`
/// (B4): only a CONFIRMED exit reaches the marker check. An unconfirmed
/// exit gets ONE hard-stop fallback (terminate + wait) before giving up
/// — B4's "an unresponsive mgmt lane has the hard-stop fallback instead
/// of leaking a live process" — never leaving a proven-but-unresponsive
/// process untracked. Never returns `PendingWriter`: a proven process
/// handle always resolves to a CONFIRMED exit or hard-stop before this
/// ever reaches [`reconcile_via_marker`], so writer-liveness ambiguity
/// (only possible with NO handle at all) cannot arise here.
/// `op_id` is `None` for the no-supervisor CLI path (`endrun_inner`),
/// which journals nothing at all — there is no `query{operation_id}`
/// caller for a durable record to ever serve, and a fixed placeholder id
/// would risk colliding with a REAL operation id a later supervised
/// session might actually use against the same state directory.
/// `on_closed` is `Option`, like [`finish_end_run_without_process`]'s own
/// parameter of the same name: `Some` for the live worker
/// ([`spawn_end_run`], via a connection it may owe a deferred reply to),
/// `None` for RECOVERY re-issuing this same call with no live connection
/// to signal through (`reconcile_journal_on_startup`) — the journal
/// itself carries the result for a later `query`.
fn finish_end_run_with_process(
    state_dir: &Path,
    op_id: Option<&str>,
    voyage_id: &str,
    epoch: Option<u64>,
    process: ChallengedProcess,
    on_closed: Option<&mpsc::Sender<EndingProgress>>,
) -> crate::Result<EndRunReconciliation> {
    let confirmed_exit = match process.wait(SUPPORTED_HISTORY_BOUND + KILL_WAIT_BOUND) {
        Ok(true) => true,
        Ok(false) | Err(_) => {
            let _ = process.terminate();
            matches!(process.wait(KILL_WAIT_BOUND), Ok(true))
        }
    };
    if !confirmed_exit {
        let detail = bounded_detail("the leg's process did not exit even after a hard stop");
        if let Some(op_id) = op_id {
            journal::finish(state_dir, op_id, &journal::TerminalRecord::Failed { detail: detail.clone() })?;
        }
        return Err(err_state(detail));
    }
    reconcile_via_marker(state_dir, op_id, voyage_id, epoch, on_closed)
}

/// As [`finish_end_run_with_process`], but for a caller with NO proven
/// handle at all (recovery, or the live path's Absent/Foreign/Pending/
/// error outcomes — B4: "Foreign/Pending/mgmt errors during EndRun still
/// run marker reconciliation"). Proves the writer is gone FIRST (B3),
/// and — N3/N8 — can return `PendingWriter` (never releasing the hold)
/// or, when the writer IS proven gone and `on_closed` is given, still
/// signal `RecordClosed` through it exactly as the WITH-process path
/// does (N8: an earlier version hardcoded `None` here for every call
/// site, so a client's deferred EndRun reply could wait forever once
/// this — not [`finish_end_run_with_process`] — was the path that
/// actually closed the record).
fn finish_end_run_without_process(
    state_dir: &Path,
    op_id: Option<&str>,
    voyage_id: &str,
    epoch: Option<u64>,
    on_closed: Option<&mpsc::Sender<EndingProgress>>,
) -> crate::Result<EndRunReconciliation> {
    match probe_writer_liveness(state_dir, voyage_id) {
        WriterLiveness::Alive | WriterLiveness::Ambiguous => Ok(EndRunReconciliation::PendingWriter),
        WriterLiveness::Absent => reconcile_via_marker(state_dir, op_id, voyage_id, epoch, on_closed),
    }
}

/// The shared marker-check-then-verify tail, reached only once the
/// writer is KNOWN gone (by a confirmed wait, or by
/// [`probe_writer_liveness`] finding it absent). `on_closed`, if given,
/// is signalled the moment `mark_closed` succeeds — the deferred-reply
/// correlation B3 requires (`None` for the recovery path, which has no
/// live connection to reply to). Every `journal::finish`/`mark_closed`
/// call is skipped when `op_id` is `None` (the CLI path) — the
/// RECONCILIATION outcome is computed identically either way. Never
/// returns `PendingWriter` — that outcome belongs to the writer-liveness
/// check ABOVE this function, never to what happens once it is gone.
fn reconcile_via_marker(
    state_dir: &Path,
    op_id: Option<&str>,
    voyage_id: &str,
    epoch: Option<u64>,
    on_closed: Option<&mpsc::Sender<EndingProgress>>,
) -> crate::Result<EndRunReconciliation> {
    let seg_dir = voyage_root_path(state_dir, voyage_id).join("seg");
    let epoch = match epoch {
        Some(e) => e,
        None => match recovery::latest_leg_state(&seg_dir).map_err(crate::Error::Io)? {
            LatestLegState::Sealed { epoch } | LatestLegState::Unsealed { epoch } => epoch,
            LatestLegState::NoLeg => {
                if let Some(op_id) = op_id {
                    journal::finish(
                        state_dir,
                        op_id,
                        &journal::TerminalRecord::Failed { detail: bounded_detail("no leg exists for this voyage") },
                    )?;
                }
                return Ok(EndRunReconciliation::PreBarrierFailed);
            }
        },
    };
    let marked = verify::leg_carries_run_end_marker(&seg_dir, voyage_id, epoch)?;
    if !marked {
        if let Some(op_id) = op_id {
            journal::finish(state_dir, op_id, &journal::TerminalRecord::Failed { detail: bounded_detail("record_append") })?;
        }
        return Ok(EndRunReconciliation::PreBarrierFailed);
    }
    if let Some(op_id) = op_id {
        journal::mark_closed(state_dir, op_id)?;
    }
    if let Some(tx) = on_closed {
        let _ = tx.send(EndingProgress::RecordClosed);
    }
    let root = voyage_root_path(state_dir, voyage_id);
    match verify::verify_voyage(&root, voyage_id) {
        Ok(()) => {
            if let Some(op_id) = op_id {
                journal::finish(state_dir, op_id, &journal::TerminalRecord::RecordVerified)?;
            }
            Ok(EndRunReconciliation::Ended)
        }
        Err(e) => {
            let detail = bounded_detail(format!("verify_voyage: {e}"));
            if let Some(op_id) = op_id {
                journal::finish(state_dir, op_id, &journal::TerminalRecord::Failed { detail: detail.clone() })?;
            }
            Err(err_state(detail)) // sticky Terminal — B4
        }
    }
}

// ---------------------------------------------------------------------
// Reset
// ---------------------------------------------------------------------

/// Rename the current pointer aside (evidence-preserving, no-replace) if
/// one exists, then mint `new_voyage` fresh. `aside_name` is the exact
/// filename to rename it to: `Some(name)` for the live, journaled path
/// (chosen and recorded AT ADMISSION time); `None` for the no-supervisor
/// CLI path, which journals nothing and so mints its own name here.
fn reset_pointer(state_dir: &Path, new_voyage: &str, aside_name: Option<&str>) -> crate::Result<()> {
    let live = pointer::pointer_path(state_dir);
    if live.exists() {
        crate::fsutil::fsync_file(&live).map_err(|e| {
            err_state(format!("reset_pointer: flushing the live pointer {live:?} before renaming it aside: {e}"))
        })?;
        let owned;
        let name: &str = match aside_name {
            Some(n) => n,
            None => {
                owned = mint_aside_name()?;
                &owned
            }
        };
        let aside = state_dir.join(name);
        crate::fsutil::publish_noreplace(&live, &aside).map_err(|e| {
            err_state(format!("reset_pointer: renaming {live:?} aside to {aside:?}: {e}"))
        })?;
    }
    std::fs::create_dir_all(voyages_dir(state_dir))
        .map_err(|e| err_state(format!("reset_pointer: creating {:?}: {e}", voyages_dir(state_dir))))?;
    let root = voyage_root_path(state_dir, new_voyage);
    if !root.exists() {
        VoyageStore::bootstrap(&root, new_voyage, RetentionClass::Archive)
            .map_err(|e| err_state(format!("reset_pointer: bootstrapping {root:?}: {e}")))?;
    }
    pointer::publish(state_dir, new_voyage)
        .map_err(|e| err_state(format!("reset_pointer: publishing the new pointer for {new_voyage:?}: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------
// Journal recovery, run FIRST — before pointer discovery (B1)
// ---------------------------------------------------------------------

struct ReconciliationSummary {
    /// Voyages whose recovered `end_run` reached `RecordVerified` this
    /// pass — the caller checks membership AFTER discovering the
    /// (now-reconciled) current voyage, never before (B1).
    ended_voyages: std::collections::HashSet<String>,
}

/// Reconciles every ACTIVE journal entry against the world — voyage
/// agnostic, keyed off nothing but `state_dir` and each entry's own
/// recorded voyage. A post-barrier verification failure propagates as
/// `Err` (STICKY `Terminal` for the caller, B4), aborting the sweep
/// immediately: the whole authority is going Terminal regardless, so
/// there is no point reconciling anything else.
fn reconcile_journal_on_startup(state_dir: &Path) -> crate::Result<ReconciliationSummary> {
    let mut ended_voyages = std::collections::HashSet::new();
    for op_id in journal::active_operations(state_dir)? {
        let Some(active) = journal::read_active(state_dir, &op_id)? else { continue };
        match &active.op {
            journal::ActiveOp::EndRun { voyage, epoch } => {
                // Re-issue the `end_run` exactly as the live worker
                // would, via the SAME shared sequence
                // ([`reissue_and_reconcile_end_run`]) — a worker killed
                // after the journal record was accepted but BEFORE it
                // ever called `end_run_over_mgmt_lane` leaves the
                // capsule never asked to end; jumping straight to
                // `retry_until_writer_resolved` (as an earlier version
                // did) only waits for a writer that was never told to
                // go away and so never resolves. `on_closed: None` —
                // recovery has no live connection to signal through;
                // the journal itself carries the result for a later
                // `query`. N3 (Codex review round 4): retried right
                // here, in THIS worker (already bounded from OUTSIDE by
                // RECOVERY_WATCHDOG, exactly as `spawn_end_run`'s own
                // retry is bounded by ENDING_WATCHDOG) — an earlier
                // version discarded a recovered `PendingWriter` outright
                // and returned `ended=false`, so startup would go on to
                // adopt/respawn right over a writer whose own EndRun was
                // still genuinely outstanding, leaving the operation
                // `Accepted` forever with nothing left to ever retry it.
                match reissue_and_reconcile_end_run(
                    state_dir,
                    Some(&op_id),
                    voyage,
                    *epoch,
                    RECOVERY_END_RUN_REASON,
                    None,
                )? {
                    EndRunReconciliation::Ended => {
                        ended_voyages.insert(voyage.clone());
                    }
                    EndRunReconciliation::PreBarrierFailed => {}
                    EndRunReconciliation::PendingWriter => {
                        unreachable!("retry_until_writer_resolved only returns once no longer PendingWriter")
                    }
                }
            }
            journal::ActiveOp::Reset { old_voyage, new_voyage, aside } => {
                reconcile_reset(state_dir, &op_id, new_voyage, old_voyage.as_deref(), aside.as_deref())?;
            }
            journal::ActiveOp::Stop => {
                // N5 (Codex review round 3): a Stop's effect is process
                // exit; a crash after admission means the effect
                // already happened. Finish it as the terminal fact it
                // always was — loud (`?`) if that write itself fails —
                // and then fall through to ordinary startup exactly
                // like any other reconciled entry: this authority does
                // NOT enter any special state and does NOT exit before
                // pointer discovery on account of it. A fresh
                // `supervise` invocation is a FRESH operator intent; an
                // old Stop stays answerable via `query` for whoever
                // asked, but honoring it against THIS run would make
                // the authority unstartable — see the module doc's own
                // "Stop no longer owns a Lifecycle state" section.
                journal::finish(state_dir, &op_id, &journal::TerminalRecord::Stopping)?;
            }
        }
    }
    Ok(ReconciliationSummary { ended_voyages })
}

fn reconcile_reset(
    state_dir: &Path,
    op_id: &str,
    new_voyage: &str,
    old_voyage: Option<&str>,
    aside: Option<&str>,
) -> crate::Result<()> {
    match pointer::validate(state_dir) {
        PointerState::Valid(id) if id == new_voyage => {}
        PointerState::Valid(id) if Some(id.as_str()) == old_voyage => {
            reset_pointer(state_dir, new_voyage, aside)?;
        }
        PointerState::NotFound => match (old_voyage, aside) {
            (Some(_), Some(aside_name)) => {
                if !state_dir.join(aside_name).exists() {
                    return Err(err_state(format!(
                        "reset {op_id}: the pointer is absent but its recorded evidence rename \
                         {aside_name:?} does not exist — an operator must investigate before this \
                         can be resumed"
                    )));
                }
                reset_pointer(state_dir, new_voyage, aside)?;
            }
            (Some(_), None) => {
                return Err(err_state(format!(
                    "reset {op_id}: a pointer existed at admission but no aside filename was \
                     journaled for it — cannot verify the pointer's disappearance is this \
                     operation's own doing"
                )));
            }
            (None, _) => {
                reset_pointer(state_dir, new_voyage, None)?;
            }
        },
        PointerState::Valid(_) => {
            return Err(err_state(format!(
                "reset {op_id}: the pointer names a THIRD identity — an operator must investigate; \
                 minting yet another would be exactly the double-mint at-most-once forbids"
            )));
        }
        PointerState::Corrupt | PointerState::OtherIo(_) => {
            return Err(err_state(format!("reset {op_id}: the pointer is unreadable during recovery")));
        }
    }
    journal::finish(
        state_dir,
        op_id,
        &journal::TerminalRecord::ResetDone { new_voyage: new_voyage.to_string() },
    )
}

// ---------------------------------------------------------------------
// Lifecycle: the authority's own state machine (see module doc)
// ---------------------------------------------------------------------

enum Lifecycle {
    /// Journal recovery + pointer discovery, folded into ONE
    /// non-blocking startup step (B1: recovery runs BEFORE pointer
    /// discovery; M1: neither may block the lane).
    Recovering { rx: mpsc::Receiver<RecoveryOutcome>, handle: JoinHandle<()>, started_at: Instant },
    /// The ONE initial placement decision (adopt if live, else consult
    /// the start-mode table).
    InitialProbe { rx: mpsc::Receiver<ProbeOutcome<ChallengedProcess>>, handle: JoinHandle<()>, started_at: Instant },
    /// A fresh owned-spawn attempt in flight — every respawn reaches
    /// this, never `InitialProbe` again.
    Spawning { rx: mpsc::Receiver<ProbeOutcome<ChallengedProcess>>, handle: JoinHandle<()>, started_at: Instant },
    /// A live leg. Stability is judged by [`leg_was_stable`] reading the
    /// leg's OWN recorded `producer_uptime_ms` (N1), never by a
    /// wall-clock `ready_at` this variant no longer carries — an
    /// earlier version's `ready_at.elapsed()` measured THIS PROCESS's
    /// own observation window, which a slow capsule teardown could
    /// inflate past the stability interval with nothing to do with how
    /// long the producer itself actually ran.
    Ready { process: ChallengedProcess },
    /// An `end_run` is in flight. `pending_reply` is the connection
    /// awaiting the DEFERRED reply at `record_closed` (B3) — `None` once
    /// delivered, or if that connection disconnected first (fine: the
    /// journal carries the result for a later `query`).
    Ending {
        operation_id: String,
        rx: mpsc::Receiver<EndingProgress>,
        handle: JoinHandle<()>,
        started_at: Instant,
        pending_reply: Option<ConnId>,
    },
    /// A `reset` is in flight — admissible ONLY from `EndedNoRespawn`
    /// (B2).
    Resetting { operation_id: String, rx: mpsc::Receiver<ResetWorkerResult>, handle: JoinHandle<()>, started_at: Instant },
    EndedNoRespawn,
    /// A loud, non-restartable stop. STICKY: no transition out of this
    /// variant exists ANYWHERE in this module — `reset` is refused from
    /// it (busy/stale), and `stop` no longer transitions the Lifecycle
    /// AT ALL (see [`AuthorityState::stop_requested`] and the module
    /// doc's own "Stop no longer owns a Lifecycle state" section, N4).
    Terminal { detail: String, entered_at: Instant },
}

enum RecoveryOutcome {
    Done { voyage_id: String, ended: bool },
    Fatal { detail: String },
}

enum EndingProgress {
    RecordClosed,
    Final(EndRunWorkerResult),
}

enum EndRunWorkerResult {
    Ended,
    /// Marker-absent pre-barrier failure (B4): the caller applies the
    /// SAME anti-flap accounting a naturally-exited `Ready` leg gets,
    /// then decides respawn. Never `PendingWriter` — [`spawn_end_run`]
    /// absorbs every `PendingWriter` attempt into its OWN bounded retry
    /// loop and never surfaces it as a final result (N3).
    PreBarrierFailed,
    Fatal(String),
}

enum ResetWorkerResult {
    Done { new_voyage: String },
    Fatal(String),
}

impl Lifecycle {
    fn wire_phase(&self) -> SupervisorPhase {
        match self {
            Lifecycle::Recovering { .. } | Lifecycle::InitialProbe { .. } | Lifecycle::Spawning { .. } => {
                SupervisorPhase::Starting
            }
            Lifecycle::Ready { .. } => SupervisorPhase::Ready,
            Lifecycle::Ending { .. } => SupervisorPhase::Ending,
            // Reset produces a not-yet-started new voyage; no dedicated
            // wire phase exists for it (the ADR's own phase vocabulary is
            // fixed at five values) — `Starting` is the closest fit.
            Lifecycle::Resetting { .. } => SupervisorPhase::Starting,
            Lifecycle::EndedNoRespawn => SupervisorPhase::EndedNoRespawn,
            Lifecycle::Terminal { .. } => SupervisorPhase::Terminal,
        }
    }
}

/// Pulls the `JoinHandle` out of whatever `*lifecycle` CURRENTLY is, for
/// [`force_terminal`]'s own "abandon an in-flight worker while jumping
/// straight to `Terminal` from OUTSIDE that state's own transition arm"
/// (a dead accept loop, an unreadable journal). The caller immediately
/// overwrites `*lifecycle` with `Lifecycle::Terminal{..}` right after
/// calling this, so the placeholder this leaves behind never actually
/// persists. N4 (Codex review round 3): `stop` no longer transitions the
/// Lifecycle at all — see [`AuthorityState::stop_requested`] — so this
/// is no longer also `Stop`'s own "carry the worker forward" mechanism;
/// it exists for `force_terminal` alone now.
fn take_worker_handle(lifecycle: &mut Lifecycle) -> Option<JoinHandle<()>> {
    match std::mem::replace(lifecycle, Lifecycle::EndedNoRespawn) {
        Lifecycle::Recovering { handle, .. }
        | Lifecycle::InitialProbe { handle, .. }
        | Lifecycle::Spawning { handle, .. }
        | Lifecycle::Ending { handle, .. }
        | Lifecycle::Resetting { handle, .. } => Some(handle),
        Lifecycle::Ready { .. } | Lifecycle::EndedNoRespawn | Lifecycle::Terminal { .. } => None,
    }
}

// ---------------------------------------------------------------------
// Background workers — every OS-facing wait runs on one of these,
// signature `-> ()` uniformly (the real result travels over the
// channel) so EVERY worker's `JoinHandle<()>` is the SAME type
// regardless of which phase spawned it (M2).
// ---------------------------------------------------------------------

fn spawn_recovery(state_dir: PathBuf, mode: StartMode) -> (mpsc::Receiver<RecoveryOutcome>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let outcome = (|| -> crate::Result<RecoveryOutcome> {
            let summary = reconcile_journal_on_startup(&state_dir)?;
            let voyage_id = discover_or_mint_voyage(&state_dir, mode)?;
            let ended = summary.ended_voyages.contains(&voyage_id);
            Ok(RecoveryOutcome::Done { voyage_id, ended })
        })()
        .unwrap_or_else(|e| RecoveryOutcome::Fatal { detail: bounded_detail(format!("{e}")) });
        let _ = tx.send(outcome);
    });
    (rx, handle)
}

fn spawn_initial_probe(
    voyage_id: String,
    voyage_root: PathBuf,
) -> (mpsc::Receiver<ProbeOutcome<ChallengedProcess>>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let episode_deadline = Instant::now() + PROBE_EPISODE;
        let outcome =
            classify::probe_adopt_only(&RealProbeOps, &voyage_id, &voyage_root, episode_deadline, ATTEMPT_INTERVAL);
        let _ = tx.send(outcome);
    });
    (rx, handle)
}

// Same 8th `survival` parameter (ADR 0042 L1a, Codex review finding 7)
// and the same reasoning as `build_run_command`'s own attribute above.
#[allow(clippy::too_many_arguments)]
fn spawn_owned_spawn_attempt(
    capsule_exe: PathBuf,
    voyage_root: PathBuf,
    voyage_id: String,
    cols: u16,
    rows: u16,
    lease_name: String,
    survival: Survival,
    producer_argv: Vec<String>,
) -> (mpsc::Receiver<ProbeOutcome<ChallengedProcess>>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let readiness_cutoff = Instant::now() + READINESS_CUTOFF;
        let mut command =
            build_run_command(&capsule_exe, &voyage_root, &voyage_id, cols, rows, &lease_name, survival, &producer_argv);
        let outcome = classify::probe_owned_spawn(
            &RealProbeOps,
            &mut command,
            &voyage_id,
            readiness_cutoff,
            KILL_WAIT_BOUND,
            ATTEMPT_INTERVAL,
        );
        let _ = tx.send(outcome);
    });
    (rx, handle)
}

/// The WAIT-ONLY reconcile, reached from [`reissue_and_reconcile_end_run`]
/// ONLY once `end_run_over_mgmt_lane` has proven the capsule `Absent` —
/// nothing left to redeliver `Shutdown` to (delivery is that caller's
/// own retry loop; this one never re-sends anything). Sends
/// [`EndingProgress::RecordClosed`] the moment `mark_closed` succeeds
/// (B3's deferred-reply signal — N8: on EVERY path that can reach it,
/// an earlier version hardcoded `None` for the no-process outcomes,
/// silently starving a `pending_reply` that would then wait forever).
/// Retries [`finish_end_run_without_process`] while it keeps returning
/// `PendingWriter` (the writer's own fence is still held or ambiguous,
/// per B3's "pipe absence is NOT writer absence"), until it resolves to
/// `Ended`/`PreBarrierFailed` or errors — N3, Codex review round 4: an
/// earlier version had recovery discard a recovered `PendingWriter`
/// outright, so startup would adopt/respawn right over a writer whose
/// own EndRun was still genuinely outstanding, leaving the operation
/// `Accepted` forever. Bounded from OUTSIDE only — by whichever
/// caller's own watchdog measures the WHOLE worker (`ENDING_WATCHDOG`
/// for the live path, `RECOVERY_WATCHDOG` for recovery, both of which
/// this loop shares with [`reissue_and_reconcile_end_run`]'s own
/// delivery retries) — never internally, which would just be a second
/// bound to keep synchronized with that one.
fn retry_until_writer_resolved(
    state_dir: &Path,
    op_id: Option<&str>,
    voyage_id: &str,
    epoch: Option<u64>,
    on_closed: Option<&mpsc::Sender<EndingProgress>>,
) -> crate::Result<EndRunReconciliation> {
    let mut result = finish_end_run_without_process(state_dir, op_id, voyage_id, epoch, on_closed);
    while matches!(result, Ok(EndRunReconciliation::PendingWriter)) {
        std::thread::sleep(ATTEMPT_INTERVAL);
        result = finish_end_run_without_process(state_dir, op_id, voyage_id, epoch, on_closed);
    }
    result
}

/// The ONE "deliver, then resolve" sequence — call
/// [`end_run_over_mgmt_lane`] and dispatch on its outcome exactly the
/// same way regardless of who is asking. Shared by the LIVE worker
/// ([`spawn_end_run`], the FE's own `end_run` command) and RECOVERY's
/// reconciliation of an `EndRun` journal entry
/// (`reconcile_journal_on_startup`'s `EndRun` arm). Recovery previously
/// skipped straight to [`retry_until_writer_resolved`], which only
/// WAITS for the writer to go away — correct for a worker that already
/// told the capsule and crashed waiting on the reply, but silently
/// wrong for a worker killed BEFORE that call ever landed (accepted the
/// journal record, then died): the capsule was never asked to end, so
/// its writer stays alive and a wait-only recovery would wait on it
/// forever. Calling this here instead makes recovery perform the exact
/// same first act the live worker does, closing that gap.
///
/// Codex review, PR #171: a ONE-SHOT delivery attempt reopened the
/// identical bug for any TRANSIENT outcome — a `Foreign`/`Pending`
/// challenge (an `Undetermined` OS-call hiccup, not a real identity
/// mismatch) or a connect `Err` fell straight through to the wait-only
/// [`retry_until_writer_resolved`], which never re-sends `Shutdown`, so
/// a capsule that was never actually told to end (this attempt's own
/// write never reached it) would again be waited on forever. The loop
/// below RE-ATTEMPTS DELIVERY on every iteration while the capsule
/// might still be alive — acting (via [`finish_end_run_with_process`])
/// only once a challenge is actually `Proven` (`Ended`), and falling to
/// the wait-only reconcile ONLY once the capsule is provably `Absent`
/// (nothing left to redeliver to) — never on `Foreign`/`Pending`/`Err`,
/// which prove nothing about whether the capsule is still there. Same
/// cadence as the wait-only loop it hands off to
/// ([`ATTEMPT_INTERVAL`]), bounded from OUTSIDE only, exactly like
/// [`retry_until_writer_resolved`] already was (`ENDING_WATCHDOG` for
/// the live path, [`RECOVERY_WATCHDOG`] for recovery — both now sized
/// to cover at least one full delivery attempt, not just the wait).
///
/// Re-issuing is harmless: ADR 0041's own EndRun transition says a
/// second `shutdown` against an already-latched capsule "is acked
/// without a second marker" (concurrent-request rule 4) — the capsule's
/// latch is a one-shot, so a redelivered request either lands before
/// the first has latched (ordinary first-time delivery) or lands on a
/// capsule already torn down, where the pipe is simply
/// [`EndRunOutcome::Absent`] and reconciliation proceeds from the
/// durable marker exactly as it would without a reissue at all.
fn reissue_and_reconcile_end_run(
    state_dir: &Path,
    op_id: Option<&str>,
    voyage_id: &str,
    epoch: Option<u64>,
    reason: &str,
    on_closed: Option<&mpsc::Sender<EndingProgress>>,
) -> crate::Result<EndRunReconciliation> {
    loop {
        match end_run_over_mgmt_lane(voyage_id, reason) {
            Ok(EndRunOutcome::Ended(process)) => {
                return finish_end_run_with_process(state_dir, op_id, voyage_id, epoch, process, on_closed);
            }
            Ok(EndRunOutcome::Absent) => {
                // Provably nothing left to deliver to — hand off to the
                // wait-only reconcile, which proves the writer's own
                // fence is free before ever trusting the marker (B3).
                return retry_until_writer_resolved(state_dir, op_id, voyage_id, epoch, on_closed);
            }
            Ok(EndRunOutcome::Foreign | EndRunOutcome::Pending) => {
                // Neither proves the capsule is gone — retry delivery,
                // not just the wait.
            }
            Err(e) => {
                // B4 (Codex review round 3): a generic mgmt-lane error
                // (e.g. a connect failure other than NotFound) proves
                // nothing about the capsule's own liveness either —
                // retry delivery rather than falling back to a
                // wait-only reconcile that would never re-send the
                // request this attempt never actually delivered.
                eprintln!(
                    "sot-capsule supervise: end_run_over_mgmt_lane failed ({e}); retrying delivery \
                     rather than falling back to a wait-only reconcile (B4)"
                );
            }
        }
        std::thread::sleep(ATTEMPT_INTERVAL);
    }
}

fn spawn_end_run(
    state_dir: PathBuf,
    operation_id: String,
    voyage_id: String,
    epoch: Option<u64>,
    reason: String,
) -> (mpsc::Receiver<EndingProgress>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = reissue_and_reconcile_end_run(&state_dir, Some(&operation_id), &voyage_id, epoch, &reason, Some(&tx));
        let final_result = match result {
            Ok(EndRunReconciliation::Ended) => EndRunWorkerResult::Ended,
            Ok(EndRunReconciliation::PreBarrierFailed) => EndRunWorkerResult::PreBarrierFailed,
            Ok(EndRunReconciliation::PendingWriter) => {
                unreachable!("retry_until_writer_resolved only returns once result is no longer PendingWriter")
            }
            Err(e) => EndRunWorkerResult::Fatal(bounded_detail(format!("{e}"))),
        };
        let _ = tx.send(EndingProgress::Final(final_result));
    });
    (rx, handle)
}

/// The ONE journaled-reset transaction body — `reset_pointer` then
/// `journal::finish` — shared by [`spawn_reset`] (the live lane's own
/// background worker) and [`reset_inner`] (the no-supervisor CLI path,
/// which calls this directly and synchronously: N6, Codex review round
/// 3, owner-tightened — "reuse the one journaled reset function...
/// net fewer lines, not a second implementation").
fn do_reset(state_dir: &Path, operation_id: &str, new_voyage: &str, aside: Option<&str>) -> ResetWorkerResult {
    match reset_pointer(state_dir, new_voyage, aside) {
        Ok(()) => {
            let t = journal::TerminalRecord::ResetDone { new_voyage: new_voyage.to_string() };
            match journal::finish(state_dir, operation_id, &t) {
                Ok(()) => ResetWorkerResult::Done { new_voyage: new_voyage.to_string() },
                Err(e) => ResetWorkerResult::Fatal(bounded_detail(format!("journal finish failed: {e}"))),
            }
        }
        Err(e) => {
            // B2: a FAILED reset_pointer is Terminal -- a half-mutated
            // pointer is the same "operator must investigate" condition
            // this module's own recovery refusal already names for a
            // third, unexplained identity. This journal::finish's OWN
            // failure (Codex review round 3, B2) is never silently
            // ignored either -- logged loud, even though the overall
            // SEVERITY is unchanged either way (Fatal -> Terminal
            // regardless): an operator investigating this failure
            // deserves to know the journal record itself may be missing
            // too.
            let detail = bounded_detail(format!("{e}"));
            let t = journal::TerminalRecord::Failed { detail: detail.clone() };
            if let Err(finish_err) = journal::finish(state_dir, operation_id, &t) {
                eprintln!(
                    "sot-capsule supervise: reset {operation_id} failed ({detail}), and recording that \
                     failure in the journal ALSO failed ({finish_err})"
                );
            }
            ResetWorkerResult::Fatal(detail)
        }
    }
}

fn spawn_reset(
    state_dir: PathBuf,
    operation_id: String,
    new_voyage: String,
    aside: Option<String>,
) -> (mpsc::Receiver<ResetWorkerResult>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = do_reset(&state_dir, &operation_id, &new_voyage, aside.as_deref());
        let _ = tx.send(result);
    });
    (rx, handle)
}

// ---------------------------------------------------------------------
// The supervisor lane's own command/query/status handling
// ---------------------------------------------------------------------

/// Everything the lane's own command/query/status handling needs —
/// deliberately separate from the main loop's own `Lifecycle` so the
/// borrow-checker never has to reason about both at once inside one
/// giant function. `voyage_id` is `None` only during `Recovering` (B1:
/// pointer discovery has not happened yet) — every state that can admit
/// a voyage-fenced command is reached strictly AFTER it becomes `Some`
/// and never reverts to `None`.
/// N4 (Codex review round 3): `stop` no longer transitions the
/// `Lifecycle` at all — an earlier `Lifecycle::Stopping` variant
/// discarded whatever worker/receiver was in flight (retaining only a
/// bare `JoinHandle`, unable to preserve its actual RESULT), which is
/// exactly how a `Fatal` outcome from a Stop-preempted Reset/EndRun
/// could be silently dropped and the process still exit 0. Instead, the
/// underlying `Lifecycle` is left COMPLETELY ALONE to keep resolving
/// itself through its own normal, already-correct transition arms
/// (worker ownership, panic detection, watchdogs — all unchanged); this
/// struct is the ONLY thing Stop's acceptance actually records, and the
/// main loop's own exit condition (in `supervise_inner`) reads it back
/// to decide when the underlying Lifecycle has reached a resting point
/// worth exiting from.
struct StopRequested {
    /// The FIRST connection whose `stop` was accepted — the one the main
    /// loop's exit condition waits to see fully delivered-or-given-up
    /// (via the SAME per-connection `PendingClose` gate every other
    /// reply already uses) before it actually breaks the loop. A SECOND
    /// (or Nth) `stop` from a DIFFERENT connection still gets its own
    /// honest, independently delivery-gated reply — see
    /// `handle_lane_bytes`'s own `CommandEffect::Stop` arm — but never
    /// re-arms this: "a second Stop... is answered from the existing
    /// gate, idempotent, never re-arming it."
    primary_conn: ConnId,
    /// MONOTONIC once `true` (never reset to `false`): OR'd from
    /// "already `Terminal`, or THIS Stop's own `journal::finish` itself
    /// failed" at the moment of acceptance, together with every
    /// SUBSEQUENT admitted Stop's own outcome. The final exit code
    /// additionally checks whether the underlying `Lifecycle` reached
    /// `Terminal` on its own account by the time the loop actually
    /// exits — computed fresh, never stored here, because sticky
    /// `Terminal` is already the `Lifecycle`'s own invariant.
    terminal_severity: bool,
}

struct AuthorityState {
    state_dir: PathBuf,
    voyage_id: Option<String>,
    self_pid: u32,
    self_created: u64,
    stop_requested: Option<StopRequested>,
}

/// What `handle_command` decided to do — the CALLER (`handle_lane_bytes`)
/// applies the resulting effect INLINE, before processing any further
/// frame in the same read.
enum CommandEffect {
    /// Begin ending the current `Ready` leg. The wire reply is DEFERRED
    /// to `record_closed` (B3) — this variant carries no reply value at
    /// all; `Accepted` is never sent, only implied.
    EndRun { operation_id: String, epoch: Option<u64>, reason: String },
    /// Begin a reset (admissible only from `EndedNoRespawn` — checked by
    /// the caller before this effect is ever produced). No `reply` field
    /// (Codex review round 3 deletion candidate, applied): a freshly
    /// admitted reset is ALWAYS `Accepted` — every OTHER outcome for
    /// this operation id (a digest conflict, an already-known id) is
    /// already intercepted earlier in `handle_command`, before this
    /// variant is ever constructed — so a stored value here could only
    /// ever equal the one constant `handle_lane_bytes` can just write
    /// directly.
    Reset { operation_id: String, new_voyage: String, aside: Option<String> },
    /// `stop` was accepted and already durably journaled (B5). No
    /// separate `journal_ok` field (Codex review round 4 deletion
    /// candidate, applied): it duplicated exactly `reply`'s own shape
    /// within this variant — `journal::finish` succeeding is the ONLY
    /// way `reply` becomes `Stopping` here, and failing is the ONLY way
    /// it becomes `Failed` — so the caller reads `journal::finish`'s
    /// own outcome directly off `reply` (`matches!(reply,
    /// SupervisorOperationState::Failed { .. })`) instead of a second,
    /// redundant bool always in lockstep with it.
    Stop { reply: SupervisorOperationState },
}

fn reset_refusal_detail(lifecycle: &Lifecycle) -> &'static str {
    match lifecycle {
        Lifecycle::Recovering { .. } => "the authority is still recovering from a prior run",
        Lifecycle::InitialProbe { .. } => "the authority is still determining whether a leg is already live",
        Lifecycle::Spawning { .. } => "a leg is currently being spawned",
        Lifecycle::Ready { .. } => "a leg is currently live; end the run before resetting",
        Lifecycle::Ending { .. } => "an end_run is currently in progress",
        Lifecycle::Resetting { .. } => "a reset is already in progress",
        Lifecycle::EndedNoRespawn => "reset is admissible here — this refusal should be unreachable",
        Lifecycle::Terminal { .. } => "the authority is in a terminal state",
    }
}

impl AuthorityState {
    /// `status` and `query` only — never `command`, which
    /// `handle_lane_bytes` calls directly through [`Self::handle_command`]
    /// so it can apply the resulting [`CommandEffect`]'s `Lifecycle`
    /// transition INLINE, before processing any further frame in the
    /// same read (per-frame admissibility against the CURRENT lifecycle
    /// — Codex review round 2, B2's closing point).
    fn handle_status_or_query(&mut self, lifecycle: &Lifecycle, req: SupervisorRequest) -> SupervisorReply {
        match req {
            SupervisorRequest::Hello { .. } | SupervisorRequest::Command { .. } => {
                unreachable!("handled by the caller before this is reached")
            }
            SupervisorRequest::Status => SupervisorReply::StatusOk {
                pid: self.self_pid,
                created: self.self_created,
                voyage: self.voyage_id.clone(),
                leg: match lifecycle {
                    Lifecycle::Ready { .. } | Lifecycle::Ending { .. } => {
                        self.voyage_id.as_deref().and_then(|v| leg_epoch_of(&self.state_dir, v))
                    }
                    _ => None,
                },
                phase: lifecycle.wire_phase(),
            },
            SupervisorRequest::Query { operation_id } => SupervisorReply::Operation(self.query_state(&operation_id)),
        }
    }

    /// `Err` is a plain reply with no transition (a refusal, a
    /// query-style answer, or a query-time journal error — the latter a
    /// LOUD STOP, matched via [`is_journal_unreadable`]). `Ok` is a
    /// [`CommandEffect`] the caller applies immediately.
    fn handle_command(
        &mut self,
        lifecycle: &Lifecycle,
        operation_id: String,
        op: SupervisorOp,
    ) -> Result<CommandEffect, SupervisorOperationState> {
        let digest = match digest_of(&op) {
            Ok(d) => d,
            Err(e) => return Err(SupervisorOperationState::Failed { detail: bounded_detail(format!("{e}")) }),
        };

        // N9 (Codex review round 3): resolve an EXISTING operation id
        // BEFORE voyage fencing, for every command family — an earlier
        // version fenced FIRST, so replaying a SUCCESSFUL Reset's own
        // id/digest, still fenced to the voyage it changed FROM, would
        // hit `stale_voyage` (the CURRENT voyage has already moved on)
        // instead of reading back its own stored `ResetDone`. An ACTIVE
        // entry with a matching digest, or an id that has reached ANY
        // OTHER journal state at all, answers idempotently from that
        // state; only a genuinely UNKNOWN id ever reaches fencing.
        match journal::read_active(&self.state_dir, &operation_id) {
            Ok(Some(existing)) if existing.digest != digest => {
                return Err(SupervisorOperationState::Refused { reason: wire::SupervisorRefusedReason::IdConflict });
            }
            Ok(Some(_)) => return Err(self.query_state(&operation_id)), // idempotent resubmit, still active
            Ok(None) => {}
            Err(e) => {
                return Err(SupervisorOperationState::Failed { detail: bounded_detail(format!("journal unreadable: {e}")) })
            }
        }
        let existing = self.query_state(&operation_id);
        if !matches!(existing, SupervisorOperationState::UnknownOperation) {
            return Err(existing); // idempotent resubmit, already terminal/closed
        }

        // Voyage-fencing (ADR 0041): a mismatch is `refused
        // {stale_voyage}` with NO MUTATION — reached only once this id
        // is confirmed genuinely new. `Reset{voyage: None}` is legal
        // ONLY when there is truly no live voyage to fence against —
        // never true once `voyage_id` is `Some` (which every state able
        // to ADMIT a reset requires — B2).
        let fenced_ok = match &op {
            SupervisorOp::EndRun { voyage, .. } => self.voyage_id.as_deref() == Some(voyage.as_str()),
            SupervisorOp::Reset { voyage: Some(v) } => self.voyage_id.as_deref() == Some(v.as_str()),
            SupervisorOp::Reset { voyage: None } => self.voyage_id.is_none(),
            SupervisorOp::Stop => true,
        };
        if !fenced_ok {
            return Err(SupervisorOperationState::Refused { reason: wire::SupervisorRefusedReason::StaleVoyage });
        }

        match op {
            SupervisorOp::EndRun { reason, .. } => {
                if !matches!(lifecycle, Lifecycle::Ready { .. }) {
                    return Err(SupervisorOperationState::Failed { detail: bounded_detail("no leg is currently running") });
                }
                let voyage_id = self.voyage_id.clone().expect("fenced_ok already confirmed a voyage_id");
                let epoch = leg_epoch_of(&self.state_dir, &voyage_id);
                let record = journal::ActiveRecord {
                    operation_id: operation_id.clone(),
                    digest,
                    op: journal::ActiveOp::EndRun { voyage: voyage_id, epoch },
                };
                if let Err(e) = journal::begin(&self.state_dir, &operation_id, &record) {
                    return Err(SupervisorOperationState::Failed { detail: bounded_detail(format!("journal begin failed: {e}")) });
                }
                Ok(CommandEffect::EndRun { operation_id, epoch, reason })
            }
            SupervisorOp::Reset { .. } => {
                if !matches!(lifecycle, Lifecycle::EndedNoRespawn) {
                    return Err(SupervisorOperationState::Failed { detail: bounded_detail(reset_refusal_detail(lifecycle)) });
                }
                let new_voyage = uuid::Uuid::now_v7().to_string();
                let old_voyage = self.voyage_id.clone();
                let aside = Some(mint_aside_name().map_err(|e| SupervisorOperationState::Failed {
                    detail: bounded_detail(format!("{e}")),
                })?);
                let record = journal::ActiveRecord {
                    operation_id: operation_id.clone(),
                    digest,
                    op: journal::ActiveOp::Reset { old_voyage, new_voyage: new_voyage.clone(), aside: aside.clone() },
                };
                if let Err(e) = journal::begin(&self.state_dir, &operation_id, &record) {
                    return Err(SupervisorOperationState::Failed { detail: bounded_detail(format!("journal begin failed: {e}")) });
                }
                Ok(CommandEffect::Reset { operation_id, new_voyage, aside })
            }
            SupervisorOp::Stop => {
                let record = journal::ActiveRecord { operation_id: operation_id.clone(), digest, op: journal::ActiveOp::Stop };
                if let Err(e) = journal::begin(&self.state_dir, &operation_id, &record) {
                    return Err(SupervisorOperationState::Failed { detail: bounded_detail(format!("journal begin failed: {e}")) });
                }
                let t = journal::TerminalRecord::Stopping;
                match journal::finish(&self.state_dir, &operation_id, &t) {
                    Ok(()) => Ok(CommandEffect::Stop { reply: terminal_to_wire(t) }),
                    Err(e) => Ok(CommandEffect::Stop {
                        reply: SupervisorOperationState::Failed { detail: bounded_detail(format!("journal finish failed: {e}")) },
                    }),
                }
            }
        }
    }

    fn query_state(&self, operation_id: &str) -> SupervisorOperationState {
        match journal::read_terminal(&self.state_dir, operation_id) {
            Ok(Some(t)) => return terminal_to_wire(t),
            Ok(None) => {}
            Err(e) => return SupervisorOperationState::Failed { detail: bounded_detail(format!("journal unreadable: {e}")) },
        }
        match journal::is_closed(&self.state_dir, operation_id) {
            Ok(true) => return SupervisorOperationState::RecordClosed,
            Ok(false) => {}
            Err(e) => return SupervisorOperationState::Failed { detail: bounded_detail(format!("journal unreadable: {e}")) },
        }
        match journal::read_active(&self.state_dir, operation_id) {
            Ok(Some(_)) => SupervisorOperationState::Accepted,
            Ok(None) => SupervisorOperationState::UnknownOperation,
            Err(e) => SupervisorOperationState::Failed { detail: bounded_detail(format!("journal unreadable: {e}")) },
        }
    }
}

fn terminal_to_wire(t: journal::TerminalRecord) -> SupervisorOperationState {
    match t {
        journal::TerminalRecord::RecordVerified => SupervisorOperationState::RecordVerified,
        journal::TerminalRecord::ResetDone { new_voyage } => SupervisorOperationState::ResetDone { new_voyage },
        journal::TerminalRecord::Stopping => SupervisorOperationState::Stopping,
        journal::TerminalRecord::Failed { detail } => SupervisorOperationState::Failed { detail: bounded_detail(detail) },
    }
}

/// A journal-read failure surfacing all the way up to the main loop is a
/// LOUD STOP — `true` iff `reply`'s own detail text names one
/// (`query_state`/`handle_command`'s own "journal unreadable: " prefix,
/// minted nowhere else in this module).
fn is_journal_unreadable(reply: &SupervisorOperationState) -> bool {
    matches!(reply, SupervisorOperationState::Failed { detail } if detail.starts_with("journal unreadable: "))
}

fn encode_reply_or_fallback(reply: &SupervisorReply) -> Vec<u8> {
    wire::encode_supervisor_reply(reply).unwrap_or_else(|e| {
        eprintln!("sot-capsule supervise: a reply failed to encode ({e}); substituting a minimal failure reply");
        wire::encode_supervisor_reply(&SupervisorReply::Operation(SupervisorOperationState::Failed {
            detail: "internal error".into(),
        }))
        .expect("this minimal fallback reply is always encodable")
    })
}

/// Transitions `*lifecycle` to `Terminal`, first extracting (and
/// abandoning) any in-flight worker's `JoinHandle` rather than silently
/// dropping it as part of the same assignment that would otherwise
/// discard it unnoticed. Dropping a `JoinHandle` never kills its thread
/// — it keeps running detached until it finishes on its own or the
/// whole process exits — so this is a deliberate, LOGGED abandonment,
/// not a safety hazard; a worker mid-flight when something ELSE already
/// forces Terminal (a journal read failure, a dead accept loop) has
/// nothing further useful to report anyway.
/// M2 (Codex review round 3): abandoning the handle here — rather than
/// blocking to join it — is sound ONLY because `Terminal` is itself
/// unconditionally, boundedly exitable from this point on (the main
/// loop's own exit condition, below, reaches process exit within a
/// small fixed grace regardless of any further lane traffic): an
/// abandoned thread either finishes harmlessly on its own before that
/// happens, or is torn down WITH the process at exit, and nothing
/// downstream ever depends on its result once a FORCED Terminal has
/// already been decided by something else entirely (a dead accept loop,
/// an unreadable journal) — there is no result left to wait for.
fn force_terminal(lifecycle: &mut Lifecycle, detail: String) {
    if let Some(handle) = take_worker_handle(lifecycle) {
        eprintln!(
            "sot-capsule supervise: abandoning an in-flight worker thread while forcing a terminal \
             state ({detail}) — its thread will exit on its own or be torn down with the process"
        );
        drop(handle);
    }
    *lifecycle = Lifecycle::Terminal { detail, entered_at: Instant::now() };
}

// ---------------------------------------------------------------------
// The supervisor lane's own connection state machine
// ---------------------------------------------------------------------

/// A refusal (or `stop`) reply queued as a connection's LAST word waits
/// through TWO stages before actually closing: first for
/// `TransportEvent::Sent` (the write has physically completed) or a
/// bounded deadline if it never arrives, THEN an additional flush-grace
/// window so the client's own `read()` has a real chance to drain the
/// bytes before this end tears the connection down.
enum PendingClose {
    AwaitingSent { deadline: Instant },
    FlushGrace { close_at: Instant },
}

struct Conn {
    splitter: wire::FrameSplitter,
    hello_ok: bool,
    last_activity: Instant,
    pending_close: Option<PendingClose>,
}

/// Bundles everything `handle_lane_bytes`/`service_lane` need beyond the
/// transport and per-connection state itself — keeps both functions'
/// own argument counts small (clippy's own `too_many_arguments`). A
/// `reset`'s own admission only ever starts `spawn_reset` (the pointer
/// rename/bootstrap/publish, needing nothing about the NEXT leg to
/// spawn) — respawning after it completes happens later, in the main
/// loop's own `Resetting -> Spawning` transition, which already has
/// `capsule_exe`/`lease_name`/`config` in scope directly.
struct LaneCtx<'a> {
    authority: &'a mut AuthorityState,
    lifecycle: &'a mut Lifecycle,
}

/// Services the lane's event queue once. Returns `true` iff the accept
/// loop has died PERMANENTLY (`TransportEvent::AcceptError` — the
/// transport's own doc: "stopped accepting new connections FOR GOOD"),
/// which the caller treats as terminal.
fn service_lane(lane: &PipeServer, conns: &mut HashMap<ConnId, Conn>, ctx: &mut LaneCtx, now: Instant) -> bool {
    let mut accept_loop_dead = false;
    // N10 (Codex review round 3): bounded to LANE_EVENT_QUOTA per tick —
    // an earlier version drained the WHOLE channel unconditionally, so
    // sustained lane traffic (each `Bytes` event triggering its own
    // `handle_lane_bytes` call) could keep this loop running
    // indefinitely, starving `Lifecycle` polling, worker results,
    // watchdogs, and `Terminal` grace of their own turn. Leftover
    // events stay queued in the transport's own channel for the NEXT
    // tick — no extra bookkeeping needed here.
    for _ in 0..LANE_EVENT_QUOTA {
        let Ok(event) = lane.events().try_recv() else { break };
        match event {
            TransportEvent::Accepted(id) => {
                conns.insert(
                    id,
                    Conn { splitter: wire::FrameSplitter::new(), hello_ok: false, last_activity: now, pending_close: None },
                );
            }
            TransportEvent::Bytes(id, bytes) => {
                handle_lane_bytes(lane, conns, id, &bytes, ctx, now);
            }
            TransportEvent::Closed(id, _reason) => {
                conns.remove(&id);
            }
            TransportEvent::Sent(id, _marker) => {
                // N4/M3 (Codex review round 3): a `stop` reply is
                // tracked through this SAME per-connection mechanism,
                // not a second bespoke one — see `CommandEffect::Stop`'s
                // own handling in `handle_lane_bytes` and
                // `AuthorityState::stop_requested`'s own doc for how the
                // main loop's own exit condition reads it back out.
                if let Some(conn) = conns.get_mut(&id) {
                    if matches!(conn.pending_close, Some(PendingClose::AwaitingSent { .. })) {
                        conn.pending_close = Some(PendingClose::FlushGrace { close_at: now + REFUSAL_FLUSH_GRACE });
                    }
                }
            }
            TransportEvent::AcceptError(e) => {
                eprintln!("sot-capsule supervise: supervisor lane accept loop failed permanently: {e}");
                accept_loop_dead = true;
            }
        }
    }
    let mut to_close: Vec<ConnId> = Vec::new();
    for (id, conn) in conns.iter() {
        let idle = now.saturating_duration_since(conn.last_activity) >= LANE_IDLE_DEADLINE;
        let close_due = match &conn.pending_close {
            Some(PendingClose::AwaitingSent { deadline }) => {
                if now >= *deadline {
                    eprintln!("sot-capsule supervise: a refusal reply was never confirmed sent; closing anyway");
                }
                now >= *deadline
            }
            Some(PendingClose::FlushGrace { close_at }) => now >= *close_at,
            None => idle,
        };
        if close_due {
            to_close.push(*id);
        }
    }
    for id in to_close {
        lane.close(id);
        conns.remove(&id);
    }
    accept_loop_dead
}

/// N10 (Codex review round 3, owner-tightened): the ONE thing that must
/// be bounded per tick is HOW LONG `service_lane`'s own event-drain loop
/// runs before returning control — capped there, at `LANE_EVENT_QUOTA`
/// (`pipe_win.rs`'s own `reader_loop` is a tight, unpaced
/// `ReadFile`-then-`deliver_bytes`-then-loop with nothing gating a
/// sustained single connection's throughput, so `MAX_LANE_INSTANCES`
/// alone does not bound it — a genuine, not merely theoretical, per-tick
/// starvation risk). EACH `Bytes` event this function processes is
/// itself already bounded to `pipe_win::READ_BUF_LEN` (64 KiB) by the
/// transport, so bounding events-per-tick already transitively bounds
/// frames-per-tick too — an EARLIER version of this fix additionally
/// queued decoded frames per-connection with a SECOND quota and a sweep
/// to drain leftovers, which turned out to buy nothing: bytes beyond
/// the event quota already stay queued, for free, in the transport's
/// own bounded channel (backpressured, never dropped) — there was
/// nothing left for a second, hand-rolled queue to do.
fn handle_lane_bytes(lane: &PipeServer, conns: &mut HashMap<ConnId, Conn>, id: ConnId, bytes: &[u8], ctx: &mut LaneCtx, now: Instant) {
    let mut close_after = false;
    let mut pending: Option<PendingClose> = None;
    {
        let Some(conn) = conns.get_mut(&id) else { return };
        conn.last_activity = now;
        let (frames, err) = conn.splitter.feed(bytes);
        for frame in frames {
            match frame {
                DecodedFrame::SupervisorRequest(SupervisorRequest::Hello { proto, build }) if !conn.hello_ok => {
                    if proto != wire::SUPERVISOR_PROTO_V1 || build != crate::exchange::SUPERVISOR_LANE_BUILD_ID {
                        let reply = wire::encode_supervisor_reply(&SupervisorReply::Refused {
                            reason: wire::SupervisorRefusedReason::VersionSkew,
                        })
                        .expect("Refused encodes unconditionally");
                        match lane.send(id, reply, Some(id)) {
                            Ok(()) => pending = Some(PendingClose::AwaitingSent { deadline: now + REFUSAL_SENT_DEADLINE }),
                            Err(_) => close_after = true,
                        }
                        break;
                    }
                    conn.hello_ok = true;
                    let (pid, created) = self_pid_and_created().unwrap_or((0, 0));
                    let reply = wire::encode_supervisor_reply(&SupervisorReply::HelloOk {
                        proto: wire::SUPERVISOR_PROTO_V1,
                        build: crate::exchange::SUPERVISOR_LANE_BUILD_ID.to_string(),
                        pid,
                        created,
                    })
                    .expect("HelloOk's build is this crate's own bounded constant");
                    let _ = lane.send(id, reply, None);
                }
                DecodedFrame::SupervisorRequest(SupervisorRequest::Hello { .. }) => {
                    close_after = true;
                    break;
                }
                DecodedFrame::SupervisorRequest(_) if !conn.hello_ok => {
                    close_after = true;
                    break;
                }
                DecodedFrame::SupervisorRequest(req @ (SupervisorRequest::Status | SupervisorRequest::Query { .. })) => {
                    let reply = ctx.authority.handle_status_or_query(ctx.lifecycle, req);
                    if let SupervisorReply::Operation(state) = &reply {
                        if is_journal_unreadable(state) {
                            force_terminal(ctx.lifecycle, "the operation journal became unreadable".into());
                        }
                    }
                    let bytes = encode_reply_or_fallback(&reply);
                    let _ = lane.send(id, bytes, None);
                }
                DecodedFrame::SupervisorRequest(SupervisorRequest::Command { operation_id, op }) => {
                    let outcome: Option<SupervisorOperationState> =
                        match ctx.authority.handle_command(ctx.lifecycle, operation_id, op) {
                            Ok(CommandEffect::EndRun { operation_id, epoch, reason }) => {
                                let voyage_id =
                                    ctx.authority.voyage_id.clone().expect("EndRun was admitted, so voyage_id is Some");
                                let (rx, handle) =
                                    spawn_end_run(ctx.authority.state_dir.clone(), operation_id.clone(), voyage_id, epoch, reason);
                                *ctx.lifecycle = Lifecycle::Ending {
                                    operation_id,
                                    rx,
                                    handle,
                                    started_at: now,
                                    pending_reply: Some(id),
                                };
                                // B3: the reply is DEFERRED to record_closed — never sent here.
                                None
                            }
                            Ok(CommandEffect::Reset { operation_id, new_voyage, aside }) => {
                                let (rx, handle) =
                                    spawn_reset(ctx.authority.state_dir.clone(), operation_id.clone(), new_voyage, aside);
                                *ctx.lifecycle = Lifecycle::Resetting { operation_id, rx, handle, started_at: now };
                                Some(SupervisorOperationState::Accepted)
                            }
                            Ok(CommandEffect::Stop { reply }) => {
                                // N4 (Codex review round 3): `stop` no
                                // longer transitions the Lifecycle AT
                                // ALL — it stays exactly whatever it
                                // already was, resolving itself through
                                // its own normal transition arms in the
                                // main loop (worker ownership, panic
                                // detection, watchdogs, all UNCHANGED).
                                // Its reply is delivery-gated through the
                                // SAME per-connection `PendingClose` gate
                                // every other reply already uses — never
                                // a second bespoke mechanism — so it is
                                // sent HERE, inline, rather than falling
                                // through to the shared tail below (which
                                // never marker-tracks a reply). Whether
                                // the journal write failed is read
                                // straight off `reply`'s own shape
                                // (Codex review round 4 deletion
                                // candidate: a separate `journal_ok`
                                // bool only ever duplicated this).
                                let journal_failed = matches!(&reply, SupervisorOperationState::Failed { .. });
                                let wire_reply = SupervisorReply::Operation(reply);
                                let reply_bytes = encode_reply_or_fallback(&wire_reply);
                                match lane.send(id, reply_bytes, Some(id)) {
                                    Ok(()) => pending = Some(PendingClose::AwaitingSent { deadline: now + REFUSAL_SENT_DEADLINE }),
                                    Err(_) => close_after = true,
                                }
                                let terminal_now = matches!(ctx.lifecycle, Lifecycle::Terminal { .. }) || journal_failed;
                                match &mut ctx.authority.stop_requested {
                                    Some(existing) => existing.terminal_severity |= terminal_now,
                                    None => {
                                        ctx.authority.stop_requested =
                                            Some(StopRequested { primary_conn: id, terminal_severity: terminal_now });
                                    }
                                }
                                None // already sent (marker-tracked), above
                            }
                            Err(state) => Some(state),
                        };
                    if let Some(state) = outcome {
                        if is_journal_unreadable(&state) {
                            force_terminal(ctx.lifecycle, "the operation journal became unreadable".into());
                        }
                        let reply = SupervisorReply::Operation(state);
                        let bytes = encode_reply_or_fallback(&reply);
                        let _ = lane.send(id, bytes, None);
                    }
                }
                _ => {
                    close_after = true;
                    break;
                }
            }
            if close_after {
                break;
            }
        }
        if err.is_some() {
            close_after = true;
        }
        if let Some(p) = pending {
            conn.pending_close = Some(p);
        }
    }
    if close_after {
        lane.close(id);
        conns.remove(&id);
    }
}

// ---------------------------------------------------------------------
// The main authority loop
// ---------------------------------------------------------------------

/// Shared "a leg just ended with no `end_run` involved" tail: count
/// against the flap bound, then either go `Terminal` or start a fresh
/// `Spawning` attempt for the CURRENT voyage — read fresh off
/// `authority` every time, never a value captured before the loop began
/// (a live `reset` can change it; a stale local was a real bug this
/// crate already shipped once).
fn respawn_or_terminal(
    consecutive_unstable_legs: &mut u32,
    capsule_exe: &Path,
    config: &SuperviseConfig,
    lease_name: &str,
    authority: &AuthorityState,
) -> Lifecycle {
    if *consecutive_unstable_legs >= FLAP_THRESHOLD {
        eprintln!(
            "sot-capsule supervise: anti-flap bound reached (consecutive_unstable_legs={consecutive_unstable_legs} >= {FLAP_THRESHOLD}); entering Terminal"
        );
        return Lifecycle::Terminal { detail: "the anti-flap bound was reached".into(), entered_at: Instant::now() };
    }
    let voyage_id = authority.voyage_id.clone().expect("respawn is only reachable once voyage_id is Some");
    let voyage_root = voyage_root_path(&authority.state_dir, &voyage_id);
    let (rx, handle) = spawn_owned_spawn_attempt(
        capsule_exe.to_path_buf(),
        voyage_root,
        voyage_id,
        config.cols,
        config.rows,
        lease_name.to_string(),
        config.survival,
        config.producer_argv.clone(),
    );
    Lifecycle::Spawning { rx, handle, started_at: Instant::now() }
}

fn supervise_inner(config: SuperviseConfig) -> crate::Result<i32> {
    std::fs::create_dir_all(voyages_dir(&config.state_dir))?;

    // ONE AUTHORITY.
    let _fence = match crate::fence::lock_supervisor(&config.state_dir) {
        Ok(f) => f,
        // `Error::State` is the ONE error `lock_supervisor` can return for
        // "already held" (see `EXIT_CONTENDED`'s own doc for why this is
        // the only path that produces it here) -- distinct from a genuine
        // bootstrap/IO failure (`Error::Io`), which stays EXIT_TERMINAL.
        Err(e @ crate::Error::State(_)) => {
            eprintln!(
                "sot-capsule supervise: authority fence already held by a live supervisor: {e}"
            );
            return Ok(EXIT_CONTENDED);
        }
        Err(e) => {
            eprintln!("sot-capsule supervise: could not become the authority: {e}");
            return Ok(EXIT_TERMINAL);
        }
    };

    let h = state_dir_hash(&config.state_dir);

    // The lane: bound AFTER the fence, BEFORE any adopt or spawn.
    let lane = match PipeServer::bind_supervisor(&h, MAX_LANE_INSTANCES) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("sot-capsule supervise: could not bind the supervisor lane: {e}");
            return Ok(EXIT_TERMINAL);
        }
    };

    // The parent-death lease: created ONCE, held for this process's
    // whole life.
    let lease_name = crate::lease::lease_name(&h, std::process::id());
    let _lease = match crate::lease::create(&lease_name) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("sot-capsule supervise: could not create the parent-death lease: {e}");
            return Ok(EXIT_TERMINAL);
        }
    };

    let self_ids = self_pid_and_created().unwrap_or((0, 0));
    let mut authority = AuthorityState {
        state_dir: config.state_dir.clone(),
        voyage_id: None,
        self_pid: self_ids.0,
        self_created: self_ids.1,
        stop_requested: None,
    };
    let mut conns: HashMap<ConnId, Conn> = HashMap::new();
    let capsule_exe = std::env::current_exe().map_err(crate::Error::Io)?;

    // B1: recovery + pointer discovery, folded into ONE non-blocking
    // background worker — the lane is already up and serviced from the
    // very first loop iteration below, well before either concludes.
    let (rx, handle) = spawn_recovery(config.state_dir.clone(), config.mode);
    let mut lifecycle = Lifecycle::Recovering { rx, handle, started_at: Instant::now() };

    let mut consecutive_unstable_legs: u32 = 0;

    'authority: loop {
        let now = Instant::now();
        {
            let mut lane_ctx = LaneCtx { authority: &mut authority, lifecycle: &mut lifecycle };
            if service_lane(&lane, &mut conns, &mut lane_ctx, now) {
                force_terminal(&mut lifecycle, "supervisor lane accept loop failed permanently".into());
            }
        }

        let current = std::mem::replace(
            &mut lifecycle,
            Lifecycle::Terminal { detail: "transitioning".into(), entered_at: now },
        );
        lifecycle = match current {
            Lifecycle::Recovering { rx, handle, started_at } => match rx.try_recv() {
                Ok(RecoveryOutcome::Done { voyage_id, ended }) => {
                    join_and_warn(handle, "recovery");
                    authority.voyage_id = Some(voyage_id.clone());
                    if ended {
                        Lifecycle::EndedNoRespawn
                    } else {
                        let voyage_root = voyage_root_path(&config.state_dir, &voyage_id);
                        let (rx, handle) = spawn_initial_probe(voyage_id, voyage_root);
                        Lifecycle::InitialProbe { rx, handle, started_at: now }
                    }
                }
                Ok(RecoveryOutcome::Fatal { detail }) => {
                    join_and_warn(handle, "recovery");
                    Lifecycle::Terminal { detail, entered_at: now }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if watchdog_expired(started_at, RECOVERY_WATCHDOG, now) {
                        abandon_worker(handle, "recovery");
                        Lifecycle::Terminal { detail: "recovery operation watchdog expired".into(), entered_at: now }
                    } else {
                        Lifecycle::Recovering { rx, handle, started_at }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    join_and_warn(handle, "recovery");
                    Lifecycle::Terminal {
                        detail: "the recovery thread ended without a result (possible panic)".into(),
                        entered_at: now,
                    }
                }
            },
            Lifecycle::InitialProbe { rx, handle, started_at } => match rx.try_recv() {
                Ok(ProbeOutcome::Adopted(process)) => {
                    join_and_warn(handle, "initial probe");
                    Lifecycle::Ready { process }
                }
                Ok(ProbeOutcome::Absent) => {
                    join_and_warn(handle, "initial probe");
                    let voyage_id = authority.voyage_id.clone().expect("set once Recovering completes");
                    match should_spawn_after_absent(&config.state_dir, &voyage_id, config.mode) {
                        Ok(true) => {
                            let voyage_root = voyage_root_path(&config.state_dir, &voyage_id);
                            let (rx, handle) = spawn_owned_spawn_attempt(
                                capsule_exe.clone(),
                                voyage_root,
                                voyage_id,
                                config.cols,
                                config.rows,
                                lease_name.clone(),
                                config.survival,
                                config.producer_argv.clone(),
                            );
                            Lifecycle::Spawning { rx, handle, started_at: now }
                        }
                        Ok(false) => Lifecycle::EndedNoRespawn,
                        Err(e) => Lifecycle::Terminal {
                            detail: bounded_detail(format!("should_spawn_after_absent: {e}")),
                            entered_at: now,
                        },
                    }
                }
                Ok(ProbeOutcome::Foreign | ProbeOutcome::Wedged) => {
                    join_and_warn(handle, "initial probe");
                    Lifecycle::Terminal { detail: "the voyage pipe is foreign or unreachable at startup".into(), entered_at: now }
                }
                Ok(other) => {
                    join_and_warn(handle, "initial probe");
                    Lifecycle::Terminal {
                        detail: bounded_detail(format!("unexpected probe_adopt_only outcome at startup: {other:?}")),
                        entered_at: now,
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if watchdog_expired(started_at, INITIAL_PROBE_WATCHDOG, now) {
                        abandon_worker(handle, "initial probe");
                        Lifecycle::Terminal { detail: "initial probe operation watchdog expired".into(), entered_at: now }
                    } else {
                        Lifecycle::InitialProbe { rx, handle, started_at }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    join_and_warn(handle, "initial probe");
                    Lifecycle::Terminal {
                        detail: "the initial probe thread ended without a result (possible panic)".into(),
                        entered_at: now,
                    }
                }
            },
            Lifecycle::Spawning { rx, handle, started_at } => match rx.try_recv() {
                Ok(ProbeOutcome::Ready(process)) => {
                    // No anti-flap accounting here at all — the counter
                    // resets or increments ONLY once this leg's own
                    // eventual death is observed and its recorded
                    // producer_uptime_ms is read (`leg_was_stable`, N1),
                    // never on merely reaching Ready. An earlier version
                    // zeroed the counter HERE, before any death could
                    // ever be counted against a prior unstable run: a
                    // leg that died moments after every respawn was
                    // "the first" unstable leg forever — 90 legs in
                    // ~120s of real Windows CI, the anti-flap bound
                    // never tripping.
                    join_and_warn(handle, "spawn");
                    Lifecycle::Ready { process }
                }
                Ok(ProbeOutcome::SpawnFailed(e)) => {
                    join_and_warn(handle, "spawn");
                    consecutive_unstable_legs += 1;
                    eprintln!(
                        "sot-capsule supervise: leg failed to spawn: {e} (unstable=true) consecutive_unstable_legs={consecutive_unstable_legs}"
                    );
                    respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &config, &lease_name, &authority)
                }
                Ok(ProbeOutcome::KilledAfterTimeout | ProbeOutcome::LegEnded) => {
                    join_and_warn(handle, "spawn");
                    consecutive_unstable_legs += 1;
                    eprintln!(
                        "sot-capsule supervise: leg ended before reaching Ready (unstable=true) consecutive_unstable_legs={consecutive_unstable_legs}"
                    );
                    respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &config, &lease_name, &authority)
                }
                Ok(ProbeOutcome::Foreign) => {
                    // Codex review round 2, finding M8: identity-
                    // mismatched interference is an OPERATOR concern,
                    // never counted as another unstable leg to respawn
                    // over.
                    join_and_warn(handle, "spawn");
                    Lifecycle::Terminal {
                        detail: "a foreign process answered the freshly spawned leg's own pipe".into(),
                        entered_at: now,
                    }
                }
                Ok(ProbeOutcome::KillOrWaitFailed(e)) => {
                    join_and_warn(handle, "spawn");
                    Lifecycle::Terminal { detail: bounded_detail(format!("kill/wait failed: {e}")), entered_at: now }
                }
                Ok(other) => {
                    join_and_warn(handle, "spawn");
                    Lifecycle::Terminal {
                        detail: bounded_detail(format!("unexpected probe_owned_spawn outcome: {other:?}")),
                        entered_at: now,
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if watchdog_expired(started_at, SPAWNING_WATCHDOG, now) {
                        abandon_worker(handle, "spawn");
                        Lifecycle::Terminal { detail: "spawn operation watchdog expired".into(), entered_at: now }
                    } else {
                        Lifecycle::Spawning { rx, handle, started_at }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    join_and_warn(handle, "spawn");
                    Lifecycle::Terminal { detail: "the spawn thread ended without a result (possible panic)".into(), entered_at: now }
                }
            },
            Lifecycle::Ready { process } => match process.wait(Duration::ZERO) {
                Ok(true) => {
                    // N1 (Codex review round 3): stability is judged on
                    // the PRODUCER's own recorded lifetime
                    // (`leg_was_stable`), never on a wall-clock interval
                    // measured from Ready to THIS observation — a slow
                    // capsule teardown (job reap, ConPTY drain, the
                    // aggregate deadline, a final wait) could alone
                    // exceed the stability interval with nothing to do
                    // with how long the producer itself actually ran.
                    let voyage_id = authority.voyage_id.clone().expect("Ready implies a voyage_id");
                    let unstable = !leg_was_stable(&config.state_dir, &voyage_id);
                    if unstable {
                        consecutive_unstable_legs += 1;
                    } else {
                        consecutive_unstable_legs = 0;
                    }
                    eprintln!(
                        "sot-capsule supervise: leg ended (unstable={unstable}) consecutive_unstable_legs={consecutive_unstable_legs}"
                    );
                    respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &config, &lease_name, &authority)
                }
                Ok(false) => Lifecycle::Ready { process },
                Err(e) => Lifecycle::Terminal {
                    detail: bounded_detail(format!("wait on the leg's process handle failed: {e}")),
                    entered_at: now,
                },
            },
            Lifecycle::Ending { operation_id, rx, handle, started_at, mut pending_reply } => match rx.try_recv() {
                Ok(EndingProgress::RecordClosed) => {
                    if let Some(conn_id) = pending_reply.take() {
                        if conns.contains_key(&conn_id) {
                            let reply = SupervisorReply::Operation(SupervisorOperationState::RecordClosed);
                            let bytes = encode_reply_or_fallback(&reply);
                            let _ = lane.send(conn_id, bytes, None);
                        } // else: client disconnected meanwhile -- fine (B3).
                    }
                    Lifecycle::Ending { operation_id, rx, handle, started_at, pending_reply }
                }
                Ok(EndingProgress::Final(EndRunWorkerResult::Ended)) => {
                    join_and_warn(handle, "end_run");
                    Lifecycle::EndedNoRespawn
                }
                Ok(EndingProgress::Final(EndRunWorkerResult::PreBarrierFailed)) => {
                    join_and_warn(handle, "end_run");
                    // N1 (Codex review round 3): the SAME producer-
                    // recorded stability check the natural-death Ready
                    // arm uses — a pre-barrier failure still means the
                    // writer is CONFIRMED gone (finish_end_run_with/
                    // without_process already proved that before ever
                    // returning PreBarrierFailed), so this leg's own
                    // producer_uptime_ms is equally readable and
                    // authoritative here.
                    let voyage_id = authority.voyage_id.clone().expect("Ending implies a voyage_id");
                    let unstable = !leg_was_stable(&config.state_dir, &voyage_id);
                    if unstable {
                        consecutive_unstable_legs += 1;
                    } else {
                        consecutive_unstable_legs = 0;
                    }
                    eprintln!(
                        "sot-capsule supervise: leg ended (end_run not durably accepted; unstable={unstable}) consecutive_unstable_legs={consecutive_unstable_legs}"
                    );
                    respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &config, &lease_name, &authority)
                }
                Ok(EndingProgress::Final(EndRunWorkerResult::Fatal(detail))) => {
                    join_and_warn(handle, "end_run");
                    Lifecycle::Terminal { detail: format!("end_run {operation_id}: {detail}"), entered_at: now }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if watchdog_expired(started_at, ENDING_WATCHDOG, now) {
                        abandon_worker(handle, "end_run");
                        Lifecycle::Terminal {
                            detail: format!("end_run {operation_id}: operation watchdog expired"),
                            entered_at: now,
                        }
                    } else {
                        Lifecycle::Ending { operation_id, rx, handle, started_at, pending_reply }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    join_and_warn(handle, "end_run");
                    Lifecycle::Terminal {
                        detail: format!("the end_run thread for {operation_id} ended without a result (possible panic)"),
                        entered_at: now,
                    }
                }
            },
            Lifecycle::Resetting { operation_id, rx, handle, started_at } => match rx.try_recv() {
                Ok(ResetWorkerResult::Done { new_voyage }) => {
                    join_and_warn(handle, "reset");
                    authority.voyage_id = Some(new_voyage.clone());
                    // Spawn IMMEDIATELY for the new voyage, never an
                    // adopt-only probe first: the freshly-minted voyage
                    // is definitely empty.
                    let voyage_root = voyage_root_path(&config.state_dir, &new_voyage);
                    let (rx, handle) = spawn_owned_spawn_attempt(
                        capsule_exe.clone(),
                        voyage_root,
                        new_voyage,
                        config.cols,
                        config.rows,
                        lease_name.clone(),
                        config.survival,
                        config.producer_argv.clone(),
                    );
                    Lifecycle::Spawning { rx, handle, started_at: now }
                }
                Ok(ResetWorkerResult::Fatal(detail)) => {
                    join_and_warn(handle, "reset");
                    Lifecycle::Terminal { detail, entered_at: now } // B2
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if watchdog_expired(started_at, RESETTING_WATCHDOG, now) {
                        abandon_worker(handle, "reset");
                        Lifecycle::Terminal {
                            detail: format!("reset {operation_id}: operation watchdog expired"),
                            entered_at: now,
                        }
                    } else {
                        Lifecycle::Resetting { operation_id, rx, handle, started_at }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    join_and_warn(handle, "reset");
                    Lifecycle::Terminal {
                        detail: format!("the reset thread for {operation_id} ended without a result (possible panic)"),
                        entered_at: now,
                    }
                }
            },
            other @ (Lifecycle::EndedNoRespawn | Lifecycle::Terminal { .. }) => other,
        };

        // N4 (Codex review round 3): `stop`'s own exit condition — the
        // underlying Lifecycle is NEVER touched by Stop's acceptance
        // (see `AuthorityState::stop_requested`'s own doc), so it keeps
        // resolving itself through EVERY ordinary transition arm above,
        // worker ownership/panic-detection/watchdogs all UNCHANGED and
        // fully shared with the non-stopping path. This only decides
        // WHEN to actually break the loop once accepted:
        //   - `Terminal` with NO stop pending: the pre-existing
        //     `TERMINAL_EXIT_GRACE` timer (now the `Lifecycle::Terminal`
        //     variant's own `entered_at` field — folded in, no separate
        //     `terminal_since` local needed).
        //   - `Terminal`, or a RESTING state (`Ready`/`EndedNoRespawn`),
        //     WITH a stop pending: "stop ends it sooner" — exit as soon
        //     as the primary connection's own reply has been delivered
        //     or given up on, via the SAME per-connection `PendingClose`
        //     gate every other reply already uses (bounded by
        //     `REFUSAL_SENT_DEADLINE` + `REFUSAL_FLUSH_GRACE`,
        //     ~2.25s — `service_lane` removes a connection from `conns`
        //     once that gate closes it, which is exactly the signal
        //     this reads).
        //   - Any OTHER state (a worker still genuinely in flight): never
        //     exits here regardless of a pending stop — "keep servicing
        //     its result until it lands or its own watchdog fires".
        let exit_now = match (&lifecycle, &authority.stop_requested) {
            (Lifecycle::Terminal { .. }, Some(stop)) => !conns.contains_key(&stop.primary_conn),
            (Lifecycle::Terminal { entered_at, .. }, None) => {
                now.saturating_duration_since(*entered_at) >= TERMINAL_EXIT_GRACE
            }
            (Lifecycle::Ready { .. } | Lifecycle::EndedNoRespawn, Some(stop)) => !conns.contains_key(&stop.primary_conn),
            _ => false,
        };
        if exit_now {
            break 'authority;
        }

        std::thread::sleep(MAIN_LOOP_POLL);
    }

    let exit_code = match (&lifecycle, &authority.stop_requested) {
        (Lifecycle::Terminal { detail, .. }, _) => {
            eprintln!("sot-capsule supervise: exiting terminal: {detail}");
            EXIT_TERMINAL
        }
        (_, Some(stop)) if stop.terminal_severity => {
            eprintln!(
                "sot-capsule supervise: exiting terminal (a stop was accepted while already terminal, \
                 or its own journal write failed)"
            );
            EXIT_TERMINAL
        }
        _ => EXIT_CLEAN,
    };

    // `lane`'s own `Drop` performs the teardown when it goes out of
    // scope below.
    Ok(exit_code)
}

// ---------------------------------------------------------------------
// endrun / reset: fence-acquiring in-process callers (no supervisor
// running) — "the same TRANSITION, not the same CAPABILITIES"
// ---------------------------------------------------------------------

fn endrun_inner(state_dir: &Path, voyage: Option<String>, reason: String) -> crate::Result<i32> {
    let _fence = match crate::fence::lock_supervisor(state_dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "sot-capsule endrun: could not acquire the authority fence ({e}) — a supervisor may \
                 already be running; send `end_run` over its lane instead of using this command"
            );
            return Ok(EXIT_TERMINAL);
        }
    };
    let voyage_id = match voyage.or_else(|| match pointer::validate(state_dir) {
        PointerState::Valid(id) => Some(id),
        _ => None,
    }) {
        Some(id) => id,
        None => {
            eprintln!("sot-capsule endrun: no voyage given and no valid drawer.voyage pointer to infer one from");
            return Ok(EXIT_TERMINAL);
        }
    };
    let outcome = end_run_over_mgmt_lane(&voyage_id, &reason)?;
    match outcome {
        EndRunOutcome::Absent => {
            // N2 (Codex review round 4): raw pipe-NotFound alone is NOT
            // proof the writer is gone -- the capsule removes the pipe
            // NAME before its final writes, seal, and writer-lock
            // release (capsule_win.rs's own teardown order), the SAME
            // race B3/N2 already guard against for every OTHER caller.
            // Trusting it directly here let a concurrent natural
            // teardown be misreported as EXIT_CLEAN with no
            // requested-end marker ever written, so a later `--resume`
            // would respawn. Reused, not reinvented:
            // finish_end_run_without_process (op_id: None -- this path
            // still journals nothing) already IS "probe_writer_liveness
            // then, if genuinely absent, reconcile via the marker" --
            // the capability matrix's own "proven ABSENT: reset only"
            // means even a CONFIRMED-gone writer with no marker is
            // refused here (this operator's own end was never actually
            // delivered), never silently reported as success; only a
            // marker a concurrent racer already committed makes this
            // legitimately `Ended`.
            let epoch = leg_epoch_of(state_dir, &voyage_id);
            match finish_end_run_without_process(state_dir, None, &voyage_id, epoch, None) {
                Ok(EndRunReconciliation::Ended) => {
                    eprintln!("sot-capsule endrun: record_verified");
                    Ok(EXIT_CLEAN)
                }
                Ok(EndRunReconciliation::PreBarrierFailed) => {
                    eprintln!(
                        "sot-capsule endrun: the voyage pipe is genuinely gone (writer.lock proven \
                         free), but no requested-end marker exists for its latest leg -- this end \
                         was never actually delivered; refusing to report success"
                    );
                    Ok(EXIT_TERMINAL)
                }
                Ok(EndRunReconciliation::PendingWriter) => {
                    eprintln!(
                        "sot-capsule endrun: could not prove the voyage pipe's own writer is gone \
                         (writer.lock still held or its liveness is ambiguous) — refusing"
                    );
                    Ok(EXIT_TERMINAL)
                }
                Err(e) => {
                    eprintln!("sot-capsule endrun: {e}");
                    Ok(EXIT_TERMINAL)
                }
            }
        }
        EndRunOutcome::Foreign => {
            eprintln!(
                "sot-capsule endrun: the voyage pipe is FOREIGN — refusing to act on an \
                 unauthenticated same-user process; start a supervisor or run explicit recovery"
            );
            Ok(EXIT_TERMINAL)
        }
        EndRunOutcome::Pending => {
            eprintln!("sot-capsule endrun: the voyage pipe did not answer within its budget");
            Ok(EXIT_TERMINAL)
        }
        EndRunOutcome::Ended(process) => {
            let epoch = leg_epoch_of(state_dir, &voyage_id);
            // No lane reply to defer here; discarded. `None`: this
            // no-supervisor CLI path journals nothing at all.
            let (tx, _rx) = mpsc::channel();
            match finish_end_run_with_process(state_dir, None, &voyage_id, epoch, process, Some(&tx)) {
                Ok(EndRunReconciliation::Ended) => {
                    eprintln!("sot-capsule endrun: record_verified");
                    Ok(EXIT_CLEAN)
                }
                Ok(EndRunReconciliation::PreBarrierFailed) => {
                    eprintln!("sot-capsule endrun: the leg did not durably record an end (record_append) — nothing further to do here");
                    Ok(EXIT_TERMINAL)
                }
                Ok(EndRunReconciliation::PendingWriter) => {
                    unreachable!(
                        "finish_end_run_with_process always resolves a CONFIRMED process exit before \
                         reconcile_via_marker; PendingWriter can only arise with no process handle at all"
                    )
                }
                Err(e) => {
                    eprintln!("sot-capsule endrun: {e}");
                    Ok(EXIT_TERMINAL)
                }
            }
        }
    }
}

/// N6 (Codex review round 3): routed through the SAME journaled Reset
/// transaction the live lane uses — "the same TRANSITION, not the same
/// CAPABILITIES" (ADR 0041's own words), applied for real. An earlier
/// version called `reset_pointer` directly with no journal entry at
/// all: a crash mid rename/bootstrap/publish left NOTHING for a LATER
/// `sot-capsule supervise`/`reset` invocation to reconcile against —
/// unlike `endrun_inner`, which stays deliberately journal-free because
/// the CAPSULE's own `run_end_requested` marker is ALREADY a complete,
/// independent crash-recovery mechanism for that operation; Reset has
/// no such secondary marker, so the journal is its ONLY recovery hook.
/// Recovery runs FIRST here too (B1, generalized to this path): a prior
/// crashed reset's own active journal entry is reconciled against the
/// world before this invocation reads the pointer or decides anything,
/// exactly like a fresh `supervise` startup — never minting a THIRD
/// identity over an unresolved one.
fn reset_inner(state_dir: &Path, voyage: Option<String>) -> crate::Result<i32> {
    let _fence = match crate::fence::lock_supervisor(state_dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "sot-capsule reset: could not acquire the authority fence ({e}) — a supervisor may \
                 already be running; send `reset` over its lane instead of using this command"
            );
            return Ok(EXIT_TERMINAL);
        }
    };
    reconcile_journal_on_startup(state_dir)?;
    let current = pointer::validate(state_dir);
    // N6: a corrupt pointer is a LOUD REFUSAL, never silently treated as
    // "no observed voyage" — regardless of whether `--voyage` was given.
    // An earlier version only checked this inside the `--voyage`
    // branch below, so an OMITTED `--voyage` against a corrupt pointer
    // fell through to `observed = None` and re-minted right past
    // evidence of corruption ADR 0039 pins as a loud stop everywhere
    // else in this crate.
    if matches!(current, PointerState::Corrupt | PointerState::OtherIo(_)) {
        eprintln!("sot-capsule reset: the current pointer is unreadable — refusing to reset past unexplained corruption");
        return Ok(EXIT_TERMINAL);
    }
    if let Some(claimed) = &voyage {
        match &current {
            PointerState::Valid(id) if id == claimed => {}
            PointerState::Valid(id) => {
                eprintln!(
                    "sot-capsule reset: --voyage {claimed:?} does not match the current pointer {id:?} — refusing"
                );
                return Ok(EXIT_TERMINAL);
            }
            PointerState::NotFound => {
                eprintln!("sot-capsule reset: --voyage {claimed:?} given, but there is no current pointer to match it against — refusing");
                return Ok(EXIT_TERMINAL);
            }
            PointerState::Corrupt | PointerState::OtherIo(_) => unreachable!("refused loud, above, before this branch"),
        }
    }
    let observed = match current {
        PointerState::Valid(id) => Some(id),
        PointerState::NotFound => None,
        PointerState::Corrupt | PointerState::OtherIo(_) => unreachable!("refused loud, above"),
    };
    if let Some(voyage_id) = &observed {
        let voyage_root = voyage_root_path(state_dir, voyage_id);
        let episode_deadline = Instant::now() + PROBE_EPISODE;
        match classify::probe_adopt_only(&RealProbeOps, voyage_id, &voyage_root, episode_deadline, ATTEMPT_INTERVAL) {
            ProbeOutcome::Absent => {}
            ProbeOutcome::Adopted(_) => {
                eprintln!("sot-capsule reset: a live capsule answered — refusing to reset a live voyage");
                return Ok(EXIT_TERMINAL);
            }
            ProbeOutcome::Foreign => {
                eprintln!(
                    "sot-capsule reset: the voyage pipe is FOREIGN — refusing to destroy the pointer \
                     while that server lives"
                );
                return Ok(EXIT_TERMINAL);
            }
            ProbeOutcome::Wedged => {
                eprintln!("sot-capsule reset: could not determine liveness within the probe episode");
                return Ok(EXIT_TERMINAL);
            }
            other => return Err(err_state(format!("unexpected probe_adopt_only outcome: {other:?}"))),
        }
    }
    let new_voyage = uuid::Uuid::now_v7().to_string();
    let aside = observed.is_some().then(mint_aside_name).transpose()?;
    // A freshly minted id, distinguishable in the journal as this CLI
    // path's own — no wire caller will ever `query` it, but the
    // journal's own crash-recovery reconciliation (`reconcile_reset`)
    // needs SOME id to key this transaction under, exactly as the live
    // lane's own `Reset` command does.
    let operation_id = format!("cli-reset-{}", uuid::Uuid::now_v7());
    let op = SupervisorOp::Reset { voyage: observed.clone() };
    let digest = digest_of(&op)?;
    let record = journal::ActiveRecord {
        operation_id: operation_id.clone(),
        digest,
        op: journal::ActiveOp::Reset { old_voyage: observed, new_voyage: new_voyage.clone(), aside: aside.clone() },
    };
    journal::begin(state_dir, &operation_id, &record)?;
    // N6: the SAME journaled-reset transaction body the live lane's own
    // spawn_reset uses — called synchronously (this CLI path is already
    // blocking by nature; no background thread is needed here at all).
    match do_reset(state_dir, &operation_id, &new_voyage, aside.as_deref()) {
        ResetWorkerResult::Done { new_voyage } => {
            eprintln!("sot-capsule reset: reset_done {{new_voyage: {new_voyage}}}");
            Ok(EXIT_CLEAN)
        }
        ResetWorkerResult::Fatal(detail) => {
            eprintln!("sot-capsule reset: {detail}");
            Ok(EXIT_TERMINAL)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_hash_is_stable_for_the_same_path() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(state_dir_hash(dir.path()), state_dir_hash(dir.path()));
    }

    #[test]
    fn state_dir_hash_differs_for_different_paths() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        assert_ne!(state_dir_hash(a.path()), state_dir_hash(b.path()));
    }

    #[test]
    fn voyage_root_path_is_scoped_under_a_voyages_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let root = voyage_root_path(dir.path(), "abc");
        assert_eq!(root, dir.path().join("voyages").join("abc"));
    }

    #[test]
    fn discover_or_mint_voyage_start_mints_a_fresh_voyage_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        assert!(matches!(pointer::validate(dir.path()), PointerState::Valid(v) if v == id));
        assert!(voyage_root_path(dir.path(), &id).exists());
    }

    #[test]
    fn discover_or_mint_voyage_resume_refuses_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover_or_mint_voyage(dir.path(), StartMode::Resume).is_err());
    }

    #[test]
    fn discover_or_mint_voyage_returns_the_existing_id_when_valid() {
        let dir = tempfile::tempdir().unwrap();
        let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        let again = discover_or_mint_voyage(dir.path(), StartMode::Resume).unwrap();
        assert_eq!(id, again);
    }

    #[test]
    fn discover_or_mint_voyage_refuses_a_corrupt_pointer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(pointer::pointer_path(dir.path()), b"not-a-uuid").unwrap();
        assert!(discover_or_mint_voyage(dir.path(), StartMode::Start).is_err());
        assert!(discover_or_mint_voyage(dir.path(), StartMode::Resume).is_err());
    }

    #[test]
    fn should_spawn_after_absent_start_mode_always_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        assert!(should_spawn_after_absent(dir.path(), &id, StartMode::Start).unwrap());
    }

    #[test]
    fn should_spawn_after_absent_resume_with_no_leg_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        assert!(should_spawn_after_absent(dir.path(), &id, StartMode::Resume).unwrap());
    }

    #[test]
    fn reset_pointer_renames_the_old_one_aside_and_mints_the_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let old = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        let new_voyage = uuid::Uuid::now_v7().to_string();
        reset_pointer(dir.path(), &new_voyage, None).unwrap();
        assert!(matches!(pointer::validate(dir.path()), PointerState::Valid(v) if v == new_voyage));
        assert!(voyage_root_path(dir.path(), &new_voyage).exists());
        // The old voyage's own store is untouched -- only the POINTER
        // moved, never the data.
        assert!(voyage_root_path(dir.path(), &old).exists());
    }

    #[test]
    fn reset_pointer_uses_the_exact_journaled_aside_name() {
        let dir = tempfile::tempdir().unwrap();
        discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        let new_voyage = uuid::Uuid::now_v7().to_string();
        let aside_name = "drawer.voyage.reset-deadbeefdeadbeef";
        reset_pointer(dir.path(), &new_voyage, Some(aside_name)).unwrap();
        assert!(dir.path().join(aside_name).exists(), "the pre-chosen aside name must be exactly what's used");
    }

    #[test]
    fn reconcile_reset_recovers_all_four_states() {
        let dir = tempfile::tempdir().unwrap();
        let old = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        let new_voyage = uuid::Uuid::now_v7().to_string();
        let aside = "drawer.voyage.reset-cafefacecafeface".to_string();

        // Row 1: pointer still names the OLD voyage -- resume from the
        // beginning.
        reconcile_reset(dir.path(), "op-1", &new_voyage, Some(&old), Some(&aside)).unwrap();
        assert!(matches!(pointer::validate(dir.path()), PointerState::Valid(v) if v == new_voyage));
        assert_eq!(
            journal_state(dir.path(), "op-1"),
            Some(journal::TerminalRecord::ResetDone { new_voyage: new_voyage.clone() })
        );
        assert!(dir.path().join(&aside).exists(), "the journaled aside name must be exactly what got used");

        // Row 3: pointer already names the INTENDED NEW voyage -- just
        // reconstruct the terminal fact.
        reconcile_reset(dir.path(), "op-2", &new_voyage, Some(&old), Some(&aside)).unwrap();
        assert_eq!(
            journal_state(dir.path(), "op-2"),
            Some(journal::TerminalRecord::ResetDone { new_voyage: new_voyage.clone() })
        );

        // Row 4: pointer names something else entirely -- loud stop.
        let rogue = uuid::Uuid::now_v7().to_string();
        assert!(reconcile_reset(dir.path(), "op-3", &rogue, Some(&old), Some(&aside)).is_err());

        // Row 2: pointer ABSENT with the evidence rename PRESENT --
        // resume from publication. Codex review round 2: the row's own
        // setup must actually MATERIALIZE the file it claims exists.
        std::fs::remove_file(pointer::pointer_path(dir.path())).unwrap();
        let third = uuid::Uuid::now_v7().to_string();
        let aside2 = "drawer.voyage.reset-0000000000000001".to_string();
        std::fs::write(dir.path().join(&aside2), b"drawer.voyage").unwrap();
        reconcile_reset(dir.path(), "op-4", &third, Some(&new_voyage), Some(&aside2)).unwrap();
        assert!(matches!(pointer::validate(dir.path()), PointerState::Valid(v) if v == third));
    }

    #[test]
    fn reconcile_reset_refuses_when_the_pointer_is_absent_but_no_evidence_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let old = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        std::fs::remove_file(pointer::pointer_path(dir.path())).unwrap();
        let new_voyage = uuid::Uuid::now_v7().to_string();
        let never_written = "drawer.voyage.reset-ffffffffffffffff";
        let err = reconcile_reset(dir.path(), "op-1", &new_voyage, Some(&old), Some(never_written)).unwrap_err();
        assert!(format!("{err}").contains("investigate"));
    }

    fn journal_state(state_dir: &Path, op_id: &str) -> Option<journal::TerminalRecord> {
        journal::read_terminal(state_dir, op_id).unwrap()
    }

    #[test]
    fn digest_of_is_stable_and_distinguishes_ops() {
        let a = SupervisorOp::Stop;
        let b = SupervisorOp::Stop;
        let c = SupervisorOp::EndRun { reason: "r".into(), voyage: "v".into() };
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

    /// Codex review round 2, finding M4: `String::truncate` panics if
    /// the cut point splits a codepoint. A 3-byte codepoint repeated
    /// enough times to exceed 128 bytes puts byte 128 strictly INSIDE a
    /// character, unlike the `é` (2 bytes) case above where byte 128
    /// happens to land on a boundary regardless.
    #[test]
    fn bounded_detail_does_not_panic_when_a_char_straddles_byte_128() {
        let long: String = "€".repeat(50); // 3 bytes each = 150 bytes
        assert!(!long.is_char_boundary(wire::MAX_SUPERVISOR_STRING_LEN), "test setup must actually straddle byte 128");
        let truncated = bounded_detail(long);
        assert!(truncated.len() <= wire::MAX_SUPERVISOR_STRING_LEN);
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    #[test]
    fn is_journal_unreadable_matches_only_that_shape() {
        assert!(is_journal_unreadable(&SupervisorOperationState::Failed { detail: "journal unreadable: boom".into() }));
        assert!(!is_journal_unreadable(&SupervisorOperationState::Failed { detail: "record_append".into() }));
        assert!(!is_journal_unreadable(&SupervisorOperationState::Accepted));
    }

    /// Every non-`EndedNoRespawn` state must have SOME refusal detail
    /// (B2) -- `reset_refusal_detail`'s own match is exhaustive over
    /// every OTHER variant at compile time; this exercises a
    /// representative sample of the actual strings too. No `Stopping`
    /// case any more (N4, Codex review round 3): `stop` no longer
    /// touches the `Lifecycle` at all, so there is no "busy because
    /// stopping" state for `reset_refusal_detail` to describe -- a
    /// reset attempted while a stop is pending is admitted and resolved
    /// exactly like any other command would be (N9, Codex review round
    /// 4: a former blanket "refuse everything once stopping" gate here
    /// was DELETED, since it ran before existing-id resolution and made
    /// a replayed EndRun/Reset id return a fresh refusal instead of its
    /// own stored terminal state).
    #[test]
    fn reset_refusal_detail_names_the_reason_for_every_busy_state() {
        let (_tx, rx) = mpsc::channel::<RecoveryOutcome>();
        let recovering = Lifecycle::Recovering { rx, handle: std::thread::spawn(|| {}), started_at: Instant::now() };
        assert!(reset_refusal_detail(&recovering).contains("recovering"));

        let terminal = Lifecycle::Terminal { detail: "x".into(), entered_at: Instant::now() };
        assert!(reset_refusal_detail(&terminal).contains("terminal"));
    }

    #[test]
    fn wire_phase_maps_every_state_to_the_adr_s_five_values() {
        assert_eq!(Lifecycle::EndedNoRespawn.wire_phase(), SupervisorPhase::EndedNoRespawn);
        assert_eq!(
            Lifecycle::Terminal { detail: "x".into(), entered_at: Instant::now() }.wire_phase(),
            SupervisorPhase::Terminal
        );
    }

    /// `take_worker_handle` must actually extract (not merely drop) an
    /// in-flight worker's handle, for `force_terminal`'s own "abandon
    /// the worker while jumping straight to Terminal" (N4, Codex review
    /// round 3: `stop` no longer uses this at all — only `force_terminal`
    /// does now). Constructing a real `Ready` variant needs a live,
    /// OS-proven `ChallengedProcess` this unit test has no safe way to
    /// fabricate (see `tests/supervisor_win.rs` for that half, exercised
    /// end-to-end against a real process); the worker-bearing states are
    /// what this function actually exists for and are fully exercisable
    /// here.
    #[test]
    fn take_worker_handle_extracts_the_handle_from_a_worker_bearing_state() {
        let (_tx, rx) = mpsc::channel::<RecoveryOutcome>();
        let mut recovering = Lifecycle::Recovering { rx, handle: std::thread::spawn(|| {}), started_at: Instant::now() };
        assert!(take_worker_handle(&mut recovering).is_some());

        let mut ended = Lifecycle::EndedNoRespawn;
        assert!(take_worker_handle(&mut ended).is_none());
    }

    fn test_authority(state_dir: &Path) -> AuthorityState {
        AuthorityState {
            state_dir: state_dir.to_path_buf(),
            voyage_id: None,
            self_pid: 0,
            self_created: 0,
            stop_requested: None,
        }
    }

    /// N9 (Codex review round 3): a SUCCESSFUL Reset's own operation id,
    /// resubmitted with the SAME digest AFTER the voyage it changed
    /// FROM no longer matches the current one, must answer with the
    /// stored `ResetDone` — not `refused{stale_voyage}`. Drives
    /// `handle_command` directly (pure logic + journal I/O, no OS
    /// process needed) rather than through a real lane connection.
    #[test]
    fn a_completed_reset_replayed_after_the_voyage_moved_on_is_idempotent_not_stale_voyage() {
        let dir = tempfile::tempdir().unwrap();
        let old_voyage = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        let mut authority = test_authority(dir.path());
        authority.voyage_id = Some(old_voyage.clone());

        let op = SupervisorOp::Reset { voyage: Some(old_voyage.clone()) };
        let effect = authority
            .handle_command(&Lifecycle::EndedNoRespawn, "reset-1".into(), op.clone())
            .expect("a fresh reset from EndedNoRespawn is admitted");
        let CommandEffect::Reset { operation_id, new_voyage, aside } = effect else {
            panic!("expected a Reset effect");
        };
        reset_pointer(dir.path(), &new_voyage, aside.as_deref()).unwrap();
        journal::finish(dir.path(), &operation_id, &journal::TerminalRecord::ResetDone { new_voyage: new_voyage.clone() })
            .unwrap();

        // The authority's own voyage_id has now moved on (as it would
        // in the main loop, once Resetting concludes) -- replaying the
        // SAME id+digest, still naming the OLD voyage, must NOT be
        // fenced against the NEW one.
        authority.voyage_id = Some(new_voyage.clone());
        let replay = authority.handle_command(&Lifecycle::EndedNoRespawn, "reset-1".into(), op);
        match replay {
            Err(SupervisorOperationState::ResetDone { new_voyage: replayed }) => assert_eq!(replayed, new_voyage),
            Ok(_) => panic!("expected an idempotent Err(ResetDone), got a fresh CommandEffect instead"),
            Err(other) => panic!("expected Err(ResetDone), got {other:?}"),
        }
    }

    /// N4 (Codex review round 3): `terminal_severity` is MONOTONIC —
    /// computed as `prior || new`, never reassigned from scratch. The
    /// regression this guards: the FIRST stop is accepted while the
    /// authority is genuinely `Terminal` (severity forced `true`); by
    /// the time a SECOND, entirely ordinary stop arrives, the
    /// underlying `Lifecycle` is untouched by Stop (N4's whole point) so
    /// it is STILL `Terminal` in reality — but this proves the
    /// bookkeeping itself never depends on that, by exercising the
    /// second stop against a DIFFERENT, non-Terminal lifecycle and a
    /// cleanly-succeeding journal write (`terminal_now` computes
    /// `false` on its own): the OR must still leave `terminal_severity`
    /// `true`, exactly the `|=`, never `=`, `handle_lane_bytes`'s own
    /// `CommandEffect::Stop` arm applies.
    #[test]
    fn stop_requested_terminal_severity_is_monotonic_across_repeated_stops() {
        let dir = tempfile::tempdir().unwrap();
        let mut authority = test_authority(dir.path());
        let terminal = Lifecycle::Terminal { detail: "x".into(), entered_at: Instant::now() };

        let CommandEffect::Stop { reply } =
            authority.handle_command(&terminal, "stop-1".into(), SupervisorOp::Stop).unwrap()
        else {
            panic!("expected a Stop effect");
        };
        let journal_failed = matches!(reply, SupervisorOperationState::Failed { .. });
        assert!(!journal_failed);
        // Mirror what handle_lane_bytes does with the effect: this is
        // the FIRST stop, accepted while already Terminal.
        authority.stop_requested = Some(StopRequested {
            primary_conn: 0, // ConnId is a bare u64; no real connection needed for this test
            terminal_severity: matches!(terminal, Lifecycle::Terminal { .. }) || journal_failed,
        });
        assert!(authority.stop_requested.as_ref().unwrap().terminal_severity);

        // A SECOND stop, a DIFFERENT id, against a NON-Terminal
        // lifecycle, whose own journal write succeeds cleanly -- this
        // stop's OWN severity computes `false` on its own; the stored
        // flag must still not clear.
        let not_terminal = Lifecycle::EndedNoRespawn;
        let CommandEffect::Stop { reply: second_reply } =
            authority.handle_command(&not_terminal, "stop-2".into(), SupervisorOp::Stop).unwrap()
        else {
            panic!("expected a Stop effect");
        };
        let second_journal_failed = matches!(second_reply, SupervisorOperationState::Failed { .. });
        assert!(!second_journal_failed);
        let terminal_now = matches!(not_terminal, Lifecycle::Terminal { .. }) || second_journal_failed;
        assert!(!terminal_now, "test setup: the second stop alone must look clean, or this proves nothing");
        authority.stop_requested.as_mut().unwrap().terminal_severity |= terminal_now;
        assert!(
            authority.stop_requested.as_ref().unwrap().terminal_severity,
            "a clean second stop against a non-terminal lifecycle must never clear the first stop's own terminal severity"
        );
    }

    /// N5 (Codex review round 3): a crashed authority's own
    /// admitted-but-unfinished `Stop` is FINISHED as terminal `Stopping`
    /// by the next restart's recovery pass — loud on failure, via the
    /// bare `?` `reconcile_journal_on_startup` already propagates.
    #[test]
    fn recovery_finishes_a_crashed_stop_as_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let record = journal::ActiveRecord {
            operation_id: "stop-1".into(),
            digest: digest_of(&SupervisorOp::Stop).unwrap(),
            op: journal::ActiveOp::Stop,
        };
        journal::begin(dir.path(), "stop-1", &record).unwrap();
        reconcile_journal_on_startup(dir.path()).unwrap();
        assert_eq!(journal::read_terminal(dir.path(), "stop-1").unwrap(), Some(journal::TerminalRecord::Stopping));
    }

    /// N3 (Codex review round 3): a writer whose liveness cannot be
    /// disproven (here: `writer.lock` genuinely HELD, with no pipe
    /// bound at all for this voyage — the same "pipe absent" state a
    /// real post-teardown window produces) must be `PendingWriter`,
    /// never `PreBarrierFailed` — the operation stays untouched, never
    /// released for a respawn to run over a writer that might still be
    /// alive.
    #[test]
    fn a_held_writer_lock_with_no_pipe_is_pending_writer_never_pre_barrier_failed() {
        let dir = tempfile::tempdir().unwrap();
        let voyage_id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        let root = voyage_root_path(dir.path(), &voyage_id);
        let _held = fsutil::lock_writer(&root.join("writer.lock")).unwrap();

        let (tx, _rx) = mpsc::channel();
        let result = finish_end_run_without_process(dir.path(), Some("op-1"), &voyage_id, None, Some(&tx));
        assert!(matches!(result, Ok(EndRunReconciliation::PendingWriter)), "expected PendingWriter, got {result:?}");
        // No journal entry was ever begun for "op-1" in this test, and
        // PendingWriter must not have created one either -- there is
        // nothing to release or respawn over.
        assert!(journal::read_active(dir.path(), "op-1").unwrap().is_none());
    }
}
