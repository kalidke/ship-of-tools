//! ADR 0041 step 6 U2: the authority. `sot-capsule supervise` is the
//! process the launcher starts (Lifecycle "ONE AUTHORITY... Every act
//! that starts, ends, adopts or resets a run is performed by the process
//! holding `<state-dir>\supervisor.lock`"); [`endrun`] and [`reset`] are
//! the no-supervisor path's own fence-acquiring in-process callers ("the
//! same TRANSITION, not the same CAPABILITIES").
//!
//! # `Lifecycle` (Codex review round 2 rewrite)
//!
//! One state machine, `Recovering -> InitialProbe -> {Ready, Spawning,
//! EndedNoRespawn}`, with `Ready <-> Spawning` (respawn), `Ready ->
//! Ending -> {EndedNoRespawn, Terminal}`, and `EndedNoRespawn -> Resetting
//! -> Spawning` — plus `Stopping` (reachable from ANY state) and
//! `Terminal` (STICKY: nothing ever transitions out of it once entered).
//! Every OS-facing wait (probe episode, spawn readiness, end_run's
//! mgmt-lane exchange + process wait + O(history) verify, reset's
//! rename+bootstrap+publish) runs on its own background thread; the main
//! loop only ever polls a `Receiver` non-blockingly and services the
//! lane, on EVERY iteration, regardless of phase — "one linearized state
//! machine, not one blocking thread." Each worker-bearing state keeps its
//! own `JoinHandle<()>` (joined on every exit from that state, including
//! into `Stopping`) and a `started_at` an operation watchdog measures
//! against; a worker panic is `Disconnected` on its receiver, mapped to
//! `Terminal` from WHATEVER state observes it.
//!
//! # Recovery runs before pointer discovery (ADR 0041 "Recovery is part
//! of the transaction, and it runs FIRST"; Codex review round 2, B1)
//!
//! `Recovering` reconciles every active journal entry — voyage-agnostic,
//! keyed off nothing but `<state_dir>` itself and each entry's OWN
//! recorded voyage — BEFORE the pointer is ever read to decide the
//! current voyage id. Reversing this order (an earlier version read the
//! pointer first) let a crash between a reset's journal admission and
//! its rename/publish leave the authority probing or reporting a voyage
//! identity recovery was about to change out from under it.
//!
//! # EndRun: the marker is never enough alone (Codex review round 2, B3/B4)
//!
//! The capsule commits its run-end marker BEFORE teardown begins, and
//! the verifier tolerates an open chain tip — so a marker ALONE does not
//! prove the writer is gone. Every marker check is preceded by proving
//! the voyage pipe itself is unreachable (a LIVE process handle already
//! proves this by `wait()`; the recovery path, with no handle, probes
//! the pipe first). A still-live writer leaves the operation ACTIVE
//! (untouched) rather than closing it — a later pass (this restart's own
//! main loop, or the NEXT restart) re-evaluates once the writer is
//! actually gone. Only pipe-absent + marker-present reaches
//! `record_closed`. A marker-ABSENT pre-barrier failure is NOT ended —
//! the hold releases and ordinary respawn logic decides, live or
//! recovered, identically. A post-barrier VERIFICATION failure is
//! STICKY `Terminal` regardless of voyage. The lane's own reply for an
//! accepted `end_run` is DEFERRED to the moment `record_closed` is
//! reached (ADR 0041:592) — held via `(ConnId, operation_id)`
//! correlation through `Ending`; a client disconnecting meanwhile is
//! fine, since the journal itself carries the result for a later
//! `query`. The proven process handle is KEPT through `Ending`: an
//! unresponsive mgmt lane gets a hard-stop (terminate + wait) fallback
//! rather than leaking a live, untracked process.
//!
//! # Reset: one state, one worker, sticky failure (Codex review round 2, B2)
//!
//! `reset` is admissible ONLY from `EndedNoRespawn` — every other state
//! refuses it (busy, or stale from `Terminal`'s own stickiness). Its
//! execution — `reset_pointer`'s rename/bootstrap/publish — is a
//! background worker (`Resetting`), never inline inside the lane's own
//! command handling. A FAILED `reset_pointer` is `Terminal`: a
//! half-mutated pointer is exactly the "an operator must investigate"
//! condition this crate's own recovery refusal already names for a
//! third, unexplained identity.
//!
//! # Stop is durable too (Codex review round 2, B5)
//!
//! `stop` now begins and finishes through the SAME journal as
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
//! uses, before the process actually exits.

#![cfg(windows)]

use crate::challenge::{self, ChallengeOutcome, ChallengedProcess};
use crate::classify::{self, ProbeOutcome};
use crate::journal;
use crate::pipe_win::{self, ConnId, PipeServer, SendMarker, TransportEvent};
use crate::pointer::{self, PointerState};
use crate::probe::RealProbeOps;
use crate::recovery::{self, LatestLegState};
use crate::segment::RetentionClass;
use crate::verify;
use crate::voyage::VoyageStore;
use crate::wire::{
    self, DecodedFrame, SupervisorOp, SupervisorOperationState, SupervisorPhase, SupervisorReply,
    SupervisorRequest,
};
use std::collections::HashMap;
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
/// Margin added to each worker state's own known worst-case bound before
/// its operation watchdog fires (Codex review round 2, M2) — belt and
/// braces against a hang inside a call that SHOULD already be bounded by
/// its own internal deadline; not itself an ADR number.
const WATCHDOG_BUFFER: Duration = Duration::from_secs(10);
const RECOVERY_WATCHDOG: Duration = Duration::from_secs(
    SUPPORTED_HISTORY_BOUND.as_secs() + KILL_WAIT_BOUND.as_secs() + WATCHDOG_BUFFER.as_secs(),
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

/// Exit codes are the launcher's own contract (ADR 0041 Lifecycle
/// "Supervisor exit codes"): `0` = clean end, do not restart; `69` =
/// terminal, do not restart, surface it; anything else (this module
/// never returns anything else on purpose) is read by the launcher as a
/// crash to restart with `--resume`.
pub const EXIT_CLEAN: i32 = 0;
pub const EXIT_TERMINAL: i32 = 69;

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

#[cfg(any(test, feature = "test-support"))]
pub fn connect_and_challenge_for_test(h: &str) -> crate::Result<(pipe_win::PipeClient, ChallengedProcess)> {
    let (conn, outcome) =
        connect_and_challenge_with_build_for_test(h, crate::exchange::SUPERVISOR_LANE_BUILD_ID)?;
    match outcome {
        ChallengeOutcome::Proven(process) => Ok((conn, process)),
        ChallengeOutcome::Foreign => Err(err_state("supervisor lane challenge: foreign")),
        ChallengeOutcome::Undetermined => Err(err_state("supervisor lane challenge: undetermined")),
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn request_for_test(
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

fn err_state(msg: impl Into<String>) -> crate::Error {
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

fn build_run_command(
    capsule_exe: &Path,
    voyage_root: &Path,
    voyage_id: &str,
    cols: u16,
    rows: u16,
    lease_name: &str,
    producer_argv: &[String],
) -> std::process::Command {
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
        .arg("--assume-no-rollback-target")
        .arg("--")
        .args(producer_argv);
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
    /// The challenge succeeded and the shutdown request reached
    /// `write_all` successfully, but its ack was never read back — the
    /// shutdown MAY have been delivered and acted on regardless.
    AckUnknown(ChallengedProcess),
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
    match challenge::challenge(&conn, &mut exchange, Instant::now() + Duration::from_secs(2)) {
        ChallengeOutcome::Foreign => Ok(EndRunOutcome::Foreign),
        ChallengeOutcome::Undetermined => Ok(EndRunOutcome::Pending),
        ChallengeOutcome::Proven(process) => {
            let request = wire::encode_mgmt_request(&wire::MgmtRequest::Shutdown { reason: reason.to_string() })
                .map_err(|e| err_state(format!("encoding shutdown request: {e}")))?;
            if conn.write_all(&request).is_err() {
                return Ok(EndRunOutcome::AckUnknown(process));
            }
            match read_one_frame(&conn, Instant::now() + Duration::from_secs(5)) {
                Ok(_) => Ok(EndRunOutcome::Ended(process)),
                Err(_) => Ok(EndRunOutcome::AckUnknown(process)),
            }
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

fn probe_writer_liveness(voyage_id: &str) -> WriterLiveness {
    let conn = match pipe_win::connect_voyage_pipe_unchallenged(voyage_id) {
        Ok(c) => c,
        Err(pipe_win::PipeError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            return WriterLiveness::Absent;
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
enum EndRunReconciliation {
    /// Post-barrier (marker present), writer confirmed gone, verified.
    Ended,
    /// Pre-barrier (marker never appeared, writer confirmed gone) OR the
    /// writer is still alive/its liveness is ambiguous: NOT ended either
    /// way — the hold releases, ordinary respawn/adopt logic decides.
    /// `journal::finish` has ALREADY been called for the pre-barrier
    /// case (`Failed{record_append}`); the still-alive/ambiguous case
    /// leaves the journal entry untouched (still `.active`) for a LATER
    /// pass to re-evaluate.
    NotEnded,
}

/// The wait+marker+verify sequence for a LIVE caller holding a proven
/// process handle (an `Ended`/`AckUnknown` outcome from
/// [`end_run_over_mgmt_lane`]). The wait result GATES `mark_closed`
/// (B4): only a CONFIRMED exit reaches the marker check. An unconfirmed
/// exit gets ONE hard-stop fallback (terminate + wait) before giving up
/// — B4's "an unresponsive mgmt lane has the hard-stop fallback instead
/// of leaking a live process" — never leaving a proven-but-unresponsive
/// process untracked.
/// `op_id` is `None` for the no-supervisor CLI path (`endrun_inner`),
/// which journals nothing at all — there is no `query{operation_id}`
/// caller for a durable record to ever serve, and a fixed placeholder id
/// would risk colliding with a REAL operation id a later supervised
/// session might actually use against the same state directory.
fn finish_end_run_with_process(
    state_dir: &Path,
    op_id: Option<&str>,
    voyage_id: &str,
    epoch: Option<u64>,
    process: ChallengedProcess,
    on_closed: &mpsc::Sender<EndingProgress>,
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
    reconcile_via_marker(state_dir, op_id, voyage_id, epoch, Some(on_closed))
}

/// As [`finish_end_run_with_process`], but for a caller with NO proven
/// handle at all (recovery, or the live path's Absent/Foreign/Pending
/// outcomes — B4: "Foreign/Pending/mgmt errors during EndRun still run
/// marker reconciliation"). Proves the writer is gone FIRST (B3).
fn finish_end_run_without_process(
    state_dir: &Path,
    op_id: Option<&str>,
    voyage_id: &str,
    epoch: Option<u64>,
) -> crate::Result<EndRunReconciliation> {
    match probe_writer_liveness(voyage_id) {
        WriterLiveness::Alive | WriterLiveness::Ambiguous => Ok(EndRunReconciliation::NotEnded),
        WriterLiveness::Absent => reconcile_via_marker(state_dir, op_id, voyage_id, epoch, None),
    }
}

/// The shared marker-check-then-verify tail, reached only once the
/// writer is KNOWN gone (by a confirmed wait, or by
/// [`probe_writer_liveness`] finding it absent). `on_closed`, if given,
/// is signalled the moment `mark_closed` succeeds — the deferred-reply
/// correlation B3 requires (`None` for the recovery path, which has no
/// live connection to reply to). Every `journal::finish`/`mark_closed`
/// call is skipped when `op_id` is `None` (the CLI path) — the
/// RECONCILIATION outcome is computed identically either way.
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
                return Ok(EndRunReconciliation::NotEnded);
            }
        },
    };
    let marked = verify::leg_carries_run_end_marker(&seg_dir, voyage_id, epoch)?;
    if !marked {
        if let Some(op_id) = op_id {
            journal::finish(state_dir, op_id, &journal::TerminalRecord::Failed { detail: bounded_detail("record_append") })?;
        }
        return Ok(EndRunReconciliation::NotEnded);
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
                if let EndRunReconciliation::Ended =
                    finish_end_run_without_process(state_dir, Some(&op_id), voyage, *epoch)?
                {
                    ended_voyages.insert(voyage.clone());
                }
            }
            journal::ActiveOp::Reset { old_voyage, new_voyage, aside } => {
                reconcile_reset(state_dir, &op_id, new_voyage, old_voyage.as_deref(), aside.as_deref())?;
            }
            journal::ActiveOp::Stop => {
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
    Ready { process: ChallengedProcess, ready_at: Instant },
    /// An `end_run` is in flight. `pending_reply` is the connection
    /// awaiting the DEFERRED reply at `record_closed` (B3) — `None` once
    /// delivered, or if that connection disconnected first (fine: the
    /// journal carries the result for a later `query`).
    Ending {
        operation_id: String,
        ready_at: Instant,
        rx: mpsc::Receiver<EndingProgress>,
        handle: JoinHandle<()>,
        started_at: Instant,
        pending_reply: Option<ConnId>,
    },
    /// A `reset` is in flight — admissible ONLY from `EndedNoRespawn`
    /// (B2).
    Resetting { operation_id: String, rx: mpsc::Receiver<ResetWorkerResult>, handle: JoinHandle<()>, started_at: Instant },
    EndedNoRespawn,
    /// A `stop` was accepted from ANY state — `worker`, if `Some`, is
    /// whatever background worker was in flight when it was accepted
    /// (carried forward to be JOINED rather than abandoned — M2);
    /// `reply` tracks delivery of the `stop` command's own wire reply
    /// (M3) before the loop actually exits. `was_terminal` decides the
    /// final exit code — STICKY: once true, always true.
    Stopping { was_terminal: bool, worker: Option<JoinHandle<()>>, reply: StopReplyState, since: Instant },
    /// A loud, non-restartable stop. STICKY: no transition out of this
    /// variant exists ANYWHERE in this module — `reset`/`stop` are the
    /// only commands admissible from it, and neither leaves it (`reset`
    /// is refused here; `stop` moves to `Stopping` with
    /// `was_terminal: true`, which still exits 69).
    Terminal { detail: String },
}

enum StopReplyState {
    AwaitingSent { conn_id: ConnId, marker: SendMarker, deadline: Instant },
    FlushGrace { close_at: Instant },
    Delivered,
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
    /// Marker-absent pre-barrier, or the writer turned out still alive —
    /// NOT ended either way (B4): the caller applies the SAME anti-flap
    /// accounting a naturally-exited `Ready` leg gets, then decides
    /// respawn.
    NotEnded,
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
            Lifecycle::Stopping { was_terminal: true, .. } => SupervisorPhase::Terminal,
            Lifecycle::Stopping { was_terminal: false, .. } => SupervisorPhase::EndedNoRespawn,
            Lifecycle::Terminal { .. } => SupervisorPhase::Terminal,
        }
    }
}

/// Pulls the `JoinHandle` out of whatever `*lifecycle` CURRENTLY is, for
/// `Stop`'s own "carry the in-flight worker forward into `Stopping`
/// rather than abandon it" (M2). The caller immediately overwrites
/// `*lifecycle` with `Lifecycle::Stopping{..}` right after calling this,
/// so the placeholder this leaves behind never actually persists.
fn take_worker_handle(lifecycle: &mut Lifecycle) -> Option<JoinHandle<()>> {
    match std::mem::replace(lifecycle, Lifecycle::EndedNoRespawn) {
        Lifecycle::Recovering { handle, .. }
        | Lifecycle::InitialProbe { handle, .. }
        | Lifecycle::Spawning { handle, .. }
        | Lifecycle::Ending { handle, .. }
        | Lifecycle::Resetting { handle, .. } => Some(handle),
        Lifecycle::Ready { .. } | Lifecycle::EndedNoRespawn | Lifecycle::Terminal { .. } => None,
        Lifecycle::Stopping { worker, .. } => worker,
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

fn spawn_owned_spawn_attempt(
    capsule_exe: PathBuf,
    voyage_root: PathBuf,
    voyage_id: String,
    cols: u16,
    rows: u16,
    lease_name: String,
    producer_argv: Vec<String>,
) -> (mpsc::Receiver<ProbeOutcome<ChallengedProcess>>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let readiness_cutoff = Instant::now() + READINESS_CUTOFF;
        let mut command =
            build_run_command(&capsule_exe, &voyage_root, &voyage_id, cols, rows, &lease_name, &producer_argv);
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

/// Carries the `end_run` to its terminal fact. Sends
/// [`EndingProgress::RecordClosed`] the moment `mark_closed` succeeds
/// (B3's deferred-reply signal), then [`EndingProgress::Final`] once the
/// operation concludes.
fn spawn_end_run(
    state_dir: PathBuf,
    operation_id: String,
    voyage_id: String,
    epoch: Option<u64>,
    reason: String,
) -> (mpsc::Receiver<EndingProgress>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = match end_run_over_mgmt_lane(&voyage_id, &reason) {
            Ok(EndRunOutcome::Absent | EndRunOutcome::Foreign | EndRunOutcome::Pending) => {
                finish_end_run_without_process(&state_dir, Some(&operation_id), &voyage_id, epoch)
            }
            Ok(EndRunOutcome::AckUnknown(process) | EndRunOutcome::Ended(process)) => {
                finish_end_run_with_process(&state_dir, Some(&operation_id), &voyage_id, epoch, process, &tx)
            }
            Err(e) => Err(e),
        };
        let final_result = match result {
            Ok(EndRunReconciliation::Ended) => EndRunWorkerResult::Ended,
            Ok(EndRunReconciliation::NotEnded) => EndRunWorkerResult::NotEnded,
            Err(e) => EndRunWorkerResult::Fatal(bounded_detail(format!("{e}"))),
        };
        let _ = tx.send(EndingProgress::Final(final_result));
    });
    (rx, handle)
}

fn spawn_reset(
    state_dir: PathBuf,
    operation_id: String,
    new_voyage: String,
    aside: Option<String>,
) -> (mpsc::Receiver<ResetWorkerResult>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = match reset_pointer(&state_dir, &new_voyage, aside.as_deref()) {
            Ok(()) => {
                let t = journal::TerminalRecord::ResetDone { new_voyage: new_voyage.clone() };
                match journal::finish(&state_dir, &operation_id, &t) {
                    Ok(()) => ResetWorkerResult::Done { new_voyage },
                    Err(e) => ResetWorkerResult::Fatal(bounded_detail(format!("journal finish failed: {e}"))),
                }
            }
            Err(e) => {
                // B2: a FAILED reset_pointer is Terminal -- a
                // half-mutated pointer is the same "operator must
                // investigate" condition this module's own recovery
                // refusal already names for a third, unexplained
                // identity.
                let detail = bounded_detail(format!("{e}"));
                let t = journal::TerminalRecord::Failed { detail: detail.clone() };
                let _ = journal::finish(&state_dir, &operation_id, &t);
                ResetWorkerResult::Fatal(detail)
            }
        };
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
struct AuthorityState {
    state_dir: PathBuf,
    voyage_id: Option<String>,
    self_pid: u32,
    self_created: u64,
}

/// What `handle_command` decided to do — the CALLER (`handle_lane_bytes`)
/// applies the resulting `Lifecycle` transition inline, before
/// processing any further frame in the same read.
enum CommandEffect {
    /// Begin ending the current `Ready` leg. The wire reply is DEFERRED
    /// to `record_closed` (B3) — this variant carries no reply value at
    /// all; `Accepted` is never sent, only implied.
    EndRun { operation_id: String, epoch: Option<u64>, reason: String, ready_at: Instant },
    /// Begin a reset (admissible only from `EndedNoRespawn` — checked by
    /// the caller before this effect is ever produced).
    Reset { operation_id: String, new_voyage: String, aside: Option<String>, reply: SupervisorOperationState },
    /// `stop` was accepted and already durably journaled (B5).
    /// `journal_ok` is `false` iff `journal::finish` itself failed —
    /// still honors the stop, but forces `Terminal` severity on exit
    /// (B2/B5: "journal::finish failures are never ignored — loud").
    Stop { reply: SupervisorOperationState, journal_ok: bool },
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
        Lifecycle::Stopping { .. } => "the authority is stopping",
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
        // Voyage-fencing (ADR 0041): a mismatch is `refused
        // {stale_voyage}` with NO MUTATION. `Reset{voyage: None}` is
        // legal ONLY when there is truly no live voyage to fence
        // against — never true once `voyage_id` is `Some` (which every
        // state able to ADMIT a reset requires — B2).
        let fenced_ok = match &op {
            SupervisorOp::EndRun { voyage, .. } => self.voyage_id.as_deref() == Some(voyage.as_str()),
            SupervisorOp::Reset { voyage: Some(v) } => self.voyage_id.as_deref() == Some(v.as_str()),
            SupervisorOp::Reset { voyage: None } => self.voyage_id.is_none(),
            SupervisorOp::Stop => true,
        };
        if !fenced_ok {
            return Err(SupervisorOperationState::Refused { reason: wire::SupervisorRefusedReason::StaleVoyage });
        }

        let digest = match digest_of(&op) {
            Ok(d) => d,
            Err(e) => return Err(SupervisorOperationState::Failed { detail: bounded_detail(format!("{e}")) }),
        };
        match journal::read_active(&self.state_dir, &operation_id) {
            Ok(Some(existing)) if existing.digest != digest => {
                return Err(SupervisorOperationState::Refused { reason: wire::SupervisorRefusedReason::IdConflict });
            }
            Ok(Some(_)) => return Err(self.query_state(&operation_id)), // idempotent resubmit
            Ok(None) => {}
            Err(e) => {
                return Err(SupervisorOperationState::Failed { detail: bounded_detail(format!("journal unreadable: {e}")) })
            }
        }

        match op {
            SupervisorOp::EndRun { reason, .. } => {
                let Lifecycle::Ready { ready_at, .. } = lifecycle else {
                    return Err(SupervisorOperationState::Failed { detail: bounded_detail("no leg is currently running") });
                };
                let ready_at = *ready_at;
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
                Ok(CommandEffect::EndRun { operation_id, epoch, reason, ready_at })
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
                Ok(CommandEffect::Reset { operation_id, new_voyage, aside, reply: SupervisorOperationState::Accepted })
            }
            SupervisorOp::Stop => {
                let record = journal::ActiveRecord { operation_id: operation_id.clone(), digest, op: journal::ActiveOp::Stop };
                if let Err(e) = journal::begin(&self.state_dir, &operation_id, &record) {
                    return Err(SupervisorOperationState::Failed { detail: bounded_detail(format!("journal begin failed: {e}")) });
                }
                let t = journal::TerminalRecord::Stopping;
                match journal::finish(&self.state_dir, &operation_id, &t) {
                    Ok(()) => Ok(CommandEffect::Stop { reply: terminal_to_wire(t), journal_ok: true }),
                    Err(e) => Ok(CommandEffect::Stop {
                        reply: SupervisorOperationState::Failed { detail: bounded_detail(format!("journal finish failed: {e}")) },
                        journal_ok: false,
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
fn force_terminal(lifecycle: &mut Lifecycle, detail: String) {
    if let Some(handle) = take_worker_handle(lifecycle) {
        eprintln!(
            "sot-capsule supervise: abandoning an in-flight worker thread while forcing a terminal \
             state ({detail}) — its thread will exit on its own or be torn down with the process"
        );
        drop(handle);
    }
    *lifecycle = Lifecycle::Terminal { detail };
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
    while let Ok(event) = lane.events().try_recv() {
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
            TransportEvent::Sent(id, marker) => {
                if let Some(conn) = conns.get_mut(&id) {
                    if matches!(conn.pending_close, Some(PendingClose::AwaitingSent { .. })) {
                        conn.pending_close = Some(PendingClose::FlushGrace { close_at: now + REFUSAL_FLUSH_GRACE });
                    }
                }
                // M3: the `stop` reply's own delivery is tracked the
                // SAME way, but at the `Lifecycle::Stopping` level (not
                // per-connection) since the loop's own exit, not this
                // one connection's close, is what's gated on it.
                if let Lifecycle::Stopping { reply, .. } = ctx.lifecycle {
                    if let StopReplyState::AwaitingSent { conn_id, marker: expected, .. } = reply {
                        if *conn_id == id && *expected == marker {
                            *reply = StopReplyState::FlushGrace { close_at: now + REFUSAL_FLUSH_GRACE };
                        }
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
                            Ok(CommandEffect::EndRun { operation_id, epoch, reason, ready_at }) => {
                                let voyage_id =
                                    ctx.authority.voyage_id.clone().expect("EndRun was admitted, so voyage_id is Some");
                                let (rx, handle) =
                                    spawn_end_run(ctx.authority.state_dir.clone(), operation_id.clone(), voyage_id, epoch, reason);
                                *ctx.lifecycle = Lifecycle::Ending {
                                    operation_id,
                                    ready_at,
                                    rx,
                                    handle,
                                    started_at: now,
                                    pending_reply: Some(id),
                                };
                                // B3: the reply is DEFERRED to record_closed — never sent here.
                                None
                            }
                            Ok(CommandEffect::Reset { operation_id, new_voyage, aside, reply }) => {
                                let (rx, handle) =
                                    spawn_reset(ctx.authority.state_dir.clone(), operation_id.clone(), new_voyage, aside);
                                *ctx.lifecycle = Lifecycle::Resetting { operation_id, rx, handle, started_at: now };
                                Some(reply)
                            }
                            Ok(CommandEffect::Stop { reply, journal_ok }) => {
                                let was_terminal = matches!(ctx.lifecycle, Lifecycle::Terminal { .. }) || !journal_ok;
                                let worker = take_worker_handle(ctx.lifecycle);
                                let wire_reply = SupervisorReply::Operation(reply);
                                let reply_bytes = encode_reply_or_fallback(&wire_reply);
                                let reply_state = match lane.send(id, reply_bytes, Some(id)) {
                                    Ok(()) => StopReplyState::AwaitingSent {
                                        conn_id: id,
                                        marker: id,
                                        deadline: now + REFUSAL_SENT_DEADLINE,
                                    },
                                    Err(_) => StopReplyState::Delivered,
                                };
                                *ctx.lifecycle = Lifecycle::Stopping { was_terminal, worker, reply: reply_state, since: now };
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
        return Lifecycle::Terminal { detail: "the anti-flap bound was reached".into() };
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
        config.producer_argv.clone(),
    );
    Lifecycle::Spawning { rx, handle, started_at: Instant::now() }
}

fn supervise_inner(config: SuperviseConfig) -> crate::Result<i32> {
    std::fs::create_dir_all(voyages_dir(&config.state_dir))?;

    // ONE AUTHORITY.
    let _fence = match crate::fence::lock_supervisor(&config.state_dir) {
        Ok(f) => f,
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
    let mut authority =
        AuthorityState { state_dir: config.state_dir.clone(), voyage_id: None, self_pid: self_ids.0, self_created: self_ids.1 };
    let mut conns: HashMap<ConnId, Conn> = HashMap::new();
    let capsule_exe = std::env::current_exe().map_err(crate::Error::Io)?;

    // B1: recovery + pointer discovery, folded into ONE non-blocking
    // background worker — the lane is already up and serviced from the
    // very first loop iteration below, well before either concludes.
    let (rx, handle) = spawn_recovery(config.state_dir.clone(), config.mode);
    let mut lifecycle = Lifecycle::Recovering { rx, handle, started_at: Instant::now() };

    let mut consecutive_unstable_legs: u32 = 0;
    let mut terminal_since: Option<Instant> = None;

    'authority: loop {
        let now = Instant::now();
        {
            let mut lane_ctx = LaneCtx { authority: &mut authority, lifecycle: &mut lifecycle };
            if service_lane(&lane, &mut conns, &mut lane_ctx, now) {
                force_terminal(&mut lifecycle, "supervisor lane accept loop failed permanently".into());
            }
        }

        let current = std::mem::replace(&mut lifecycle, Lifecycle::Terminal { detail: "transitioning".into() });
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
                    Lifecycle::Terminal { detail }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if watchdog_expired(started_at, RECOVERY_WATCHDOG, now) {
                        join_and_warn(handle, "recovery");
                        Lifecycle::Terminal { detail: "recovery operation watchdog expired".into() }
                    } else {
                        Lifecycle::Recovering { rx, handle, started_at }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    join_and_warn(handle, "recovery");
                    Lifecycle::Terminal { detail: "the recovery thread ended without a result (possible panic)".into() }
                }
            },
            Lifecycle::InitialProbe { rx, handle, started_at } => match rx.try_recv() {
                Ok(ProbeOutcome::Adopted(process)) => {
                    join_and_warn(handle, "initial probe");
                    Lifecycle::Ready { process, ready_at: Instant::now() }
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
                                config.producer_argv.clone(),
                            );
                            Lifecycle::Spawning { rx, handle, started_at: now }
                        }
                        Ok(false) => Lifecycle::EndedNoRespawn,
                        Err(e) => Lifecycle::Terminal { detail: bounded_detail(format!("should_spawn_after_absent: {e}")) },
                    }
                }
                Ok(ProbeOutcome::Foreign | ProbeOutcome::Wedged) => {
                    join_and_warn(handle, "initial probe");
                    Lifecycle::Terminal { detail: "the voyage pipe is foreign or unreachable at startup".into() }
                }
                Ok(other) => {
                    join_and_warn(handle, "initial probe");
                    Lifecycle::Terminal { detail: bounded_detail(format!("unexpected probe_adopt_only outcome at startup: {other:?}")) }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if watchdog_expired(started_at, INITIAL_PROBE_WATCHDOG, now) {
                        join_and_warn(handle, "initial probe");
                        Lifecycle::Terminal { detail: "initial probe operation watchdog expired".into() }
                    } else {
                        Lifecycle::InitialProbe { rx, handle, started_at }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    join_and_warn(handle, "initial probe");
                    Lifecycle::Terminal { detail: "the initial probe thread ended without a result (possible panic)".into() }
                }
            },
            Lifecycle::Spawning { rx, handle, started_at } => match rx.try_recv() {
                Ok(ProbeOutcome::Ready(process)) => {
                    join_and_warn(handle, "spawn");
                    consecutive_unstable_legs = 0;
                    Lifecycle::Ready { process, ready_at: Instant::now() }
                }
                Ok(ProbeOutcome::SpawnFailed(e)) => {
                    join_and_warn(handle, "spawn");
                    eprintln!("sot-capsule supervise: spawn failed: {e}");
                    consecutive_unstable_legs += 1;
                    respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &config, &lease_name, &authority)
                }
                Ok(ProbeOutcome::KilledAfterTimeout | ProbeOutcome::LegEnded) => {
                    join_and_warn(handle, "spawn");
                    consecutive_unstable_legs += 1;
                    respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &config, &lease_name, &authority)
                }
                Ok(ProbeOutcome::Foreign) => {
                    // Codex review round 2, finding M8: identity-
                    // mismatched interference is an OPERATOR concern,
                    // never counted as another unstable leg to respawn
                    // over.
                    join_and_warn(handle, "spawn");
                    Lifecycle::Terminal { detail: "a foreign process answered the freshly spawned leg's own pipe".into() }
                }
                Ok(ProbeOutcome::KillOrWaitFailed(e)) => {
                    join_and_warn(handle, "spawn");
                    Lifecycle::Terminal { detail: bounded_detail(format!("kill/wait failed: {e}")) }
                }
                Ok(other) => {
                    join_and_warn(handle, "spawn");
                    Lifecycle::Terminal { detail: bounded_detail(format!("unexpected probe_owned_spawn outcome: {other:?}")) }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if watchdog_expired(started_at, SPAWNING_WATCHDOG, now) {
                        join_and_warn(handle, "spawn");
                        Lifecycle::Terminal { detail: "spawn operation watchdog expired".into() }
                    } else {
                        Lifecycle::Spawning { rx, handle, started_at }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    join_and_warn(handle, "spawn");
                    Lifecycle::Terminal { detail: "the spawn thread ended without a result (possible panic)".into() }
                }
            },
            Lifecycle::Ready { process, ready_at } => match process.wait(Duration::ZERO) {
                Ok(true) => {
                    if ready_at.elapsed() < STABILITY_INTERVAL {
                        consecutive_unstable_legs += 1;
                    } else {
                        consecutive_unstable_legs = 0;
                    }
                    respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &config, &lease_name, &authority)
                }
                Ok(false) => Lifecycle::Ready { process, ready_at },
                Err(e) => Lifecycle::Terminal { detail: bounded_detail(format!("wait on the leg's process handle failed: {e}")) },
            },
            Lifecycle::Ending { operation_id, ready_at, rx, handle, started_at, mut pending_reply } => match rx.try_recv() {
                Ok(EndingProgress::RecordClosed) => {
                    if let Some(conn_id) = pending_reply.take() {
                        if conns.contains_key(&conn_id) {
                            let reply = SupervisorReply::Operation(SupervisorOperationState::RecordClosed);
                            let bytes = encode_reply_or_fallback(&reply);
                            let _ = lane.send(conn_id, bytes, None);
                        } // else: client disconnected meanwhile -- fine (B3).
                    }
                    Lifecycle::Ending { operation_id, ready_at, rx, handle, started_at, pending_reply }
                }
                Ok(EndingProgress::Final(EndRunWorkerResult::Ended)) => {
                    join_and_warn(handle, "end_run");
                    Lifecycle::EndedNoRespawn
                }
                Ok(EndingProgress::Final(EndRunWorkerResult::NotEnded)) => {
                    join_and_warn(handle, "end_run");
                    if ready_at.elapsed() < STABILITY_INTERVAL {
                        consecutive_unstable_legs += 1;
                    } else {
                        consecutive_unstable_legs = 0;
                    }
                    respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &config, &lease_name, &authority)
                }
                Ok(EndingProgress::Final(EndRunWorkerResult::Fatal(detail))) => {
                    join_and_warn(handle, "end_run");
                    Lifecycle::Terminal { detail: format!("end_run {operation_id}: {detail}") }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if watchdog_expired(started_at, ENDING_WATCHDOG, now) {
                        join_and_warn(handle, "end_run");
                        Lifecycle::Terminal { detail: format!("end_run {operation_id}: operation watchdog expired") }
                    } else {
                        Lifecycle::Ending { operation_id, ready_at, rx, handle, started_at, pending_reply }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    join_and_warn(handle, "end_run");
                    Lifecycle::Terminal {
                        detail: format!("the end_run thread for {operation_id} ended without a result (possible panic)"),
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
                        config.producer_argv.clone(),
                    );
                    Lifecycle::Spawning { rx, handle, started_at: now }
                }
                Ok(ResetWorkerResult::Fatal(detail)) => {
                    join_and_warn(handle, "reset");
                    Lifecycle::Terminal { detail } // B2
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if watchdog_expired(started_at, RESETTING_WATCHDOG, now) {
                        join_and_warn(handle, "reset");
                        Lifecycle::Terminal { detail: format!("reset {operation_id}: operation watchdog expired") }
                    } else {
                        Lifecycle::Resetting { operation_id, rx, handle, started_at }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    join_and_warn(handle, "reset");
                    Lifecycle::Terminal {
                        detail: format!("the reset thread for {operation_id} ended without a result (possible panic)"),
                    }
                }
            },
            other @ (Lifecycle::EndedNoRespawn | Lifecycle::Stopping { .. } | Lifecycle::Terminal { .. }) => other,
        };

        // `Stopping`'s own drain: join its carried-forward worker once
        // it finishes, or give up on it past its OWN watchdog (anchored
        // to `since`, set once when `Stopping` was entered — the loop
        // must still reach exit within a bound, never depend on a
        // worker that might itself be stuck).
        if let Lifecycle::Stopping { worker, since, .. } = &mut lifecycle {
            if let Some(h) = worker {
                if h.is_finished() {
                    if let Some(h) = worker.take() {
                        join_and_warn(h, "stopping-drain");
                    }
                } else if watchdog_expired(*since, ENDING_WATCHDOG, now) {
                    eprintln!(
                        "sot-capsule supervise: giving up waiting for an in-flight worker before stopping \
                         (watchdog expired); its thread will exit on its own or be torn down with the process"
                    );
                    *worker = None;
                }
            }
        }

        if matches!(lifecycle, Lifecycle::Terminal { .. }) {
            let since = *terminal_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= TERMINAL_EXIT_GRACE {
                break 'authority;
            }
        }

        if let Lifecycle::Stopping { worker, reply, .. } = &lifecycle {
            let worker_done = worker.is_none();
            let reply_done = matches!(reply, StopReplyState::Delivered)
                || matches!(reply, StopReplyState::AwaitingSent { deadline, .. } if now >= *deadline)
                || matches!(reply, StopReplyState::FlushGrace { close_at } if now >= *close_at);
            if worker_done && reply_done {
                break 'authority;
            }
        }

        std::thread::sleep(MAIN_LOOP_POLL);
    }

    let exit_code = match &lifecycle {
        Lifecycle::Terminal { detail } => {
            eprintln!("sot-capsule supervise: exiting terminal: {detail}");
            EXIT_TERMINAL
        }
        Lifecycle::Stopping { was_terminal: true, .. } => {
            eprintln!("sot-capsule supervise: exiting terminal (stop accepted while terminal, or its own journal write failed)");
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
            // No journaling on this path (nothing to reconcile via
            // query later) -- ABSENT here genuinely means nothing is
            // running at all, distinct from B3's "prove the writer is
            // gone before trusting a marker" concern, which is about
            // NOT fabricating success for an operation that WAS
            // admitted.
            eprintln!("sot-capsule endrun: no live capsule for this voyage — nothing to end");
            Ok(EXIT_CLEAN)
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
        EndRunOutcome::AckUnknown(process) | EndRunOutcome::Ended(process) => {
            let epoch = leg_epoch_of(state_dir, &voyage_id);
            // No lane reply to defer here; discarded. `None`: this
            // no-supervisor CLI path journals nothing at all.
            let (tx, _rx) = mpsc::channel();
            match finish_end_run_with_process(state_dir, None, &voyage_id, epoch, process, &tx) {
                Ok(EndRunReconciliation::Ended) => {
                    eprintln!("sot-capsule endrun: record_verified");
                    Ok(EXIT_CLEAN)
                }
                Ok(EndRunReconciliation::NotEnded) => {
                    eprintln!("sot-capsule endrun: the leg did not durably record an end (record_append) — nothing further to do here");
                    Ok(EXIT_TERMINAL)
                }
                Err(e) => {
                    eprintln!("sot-capsule endrun: {e}");
                    Ok(EXIT_TERMINAL)
                }
            }
        }
    }
}

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
    let current = pointer::validate(state_dir);
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
            PointerState::Corrupt | PointerState::OtherIo(_) => {
                eprintln!("sot-capsule reset: the current pointer is unreadable — refusing to compare --voyage against it");
                return Ok(EXIT_TERMINAL);
            }
        }
    }
    let observed = match current {
        PointerState::Valid(id) => Some(id),
        _ => None,
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
    reset_pointer(state_dir, &new_voyage, None)?;
    eprintln!("sot-capsule reset: reset_done {{new_voyage: {new_voyage}}}");
    Ok(EXIT_CLEAN)
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
    /// representative sample of the actual strings too.
    #[test]
    fn reset_refusal_detail_names_the_reason_for_every_busy_state() {
        let (_tx, rx) = mpsc::channel::<RecoveryOutcome>();
        let recovering = Lifecycle::Recovering { rx, handle: std::thread::spawn(|| {}), started_at: Instant::now() };
        assert!(reset_refusal_detail(&recovering).contains("recovering"));

        let terminal = Lifecycle::Terminal { detail: "x".into() };
        assert!(reset_refusal_detail(&terminal).contains("terminal"));

        let stopping = Lifecycle::Stopping { was_terminal: false, worker: None, reply: StopReplyState::Delivered, since: Instant::now() };
        assert!(reset_refusal_detail(&stopping).contains("stopping"));
    }

    #[test]
    fn wire_phase_maps_every_state_to_the_adr_s_five_values() {
        assert_eq!(Lifecycle::EndedNoRespawn.wire_phase(), SupervisorPhase::EndedNoRespawn);
        assert_eq!(Lifecycle::Terminal { detail: "x".into() }.wire_phase(), SupervisorPhase::Terminal);
        let stopping_terminal =
            Lifecycle::Stopping { was_terminal: true, worker: None, reply: StopReplyState::Delivered, since: Instant::now() };
        assert_eq!(stopping_terminal.wire_phase(), SupervisorPhase::Terminal, "sticky Terminal must survive into Stopping's own phase");
        let stopping_clean =
            Lifecycle::Stopping { was_terminal: false, worker: None, reply: StopReplyState::Delivered, since: Instant::now() };
        assert_eq!(stopping_clean.wire_phase(), SupervisorPhase::EndedNoRespawn);
    }

    /// `take_worker_handle` must actually extract (not merely drop) an
    /// in-flight worker's handle, so `Stop` can carry it forward to be
    /// joined rather than abandoned (M2). Constructing a real `Ready`
    /// variant needs a live, OS-proven `ChallengedProcess` this unit
    /// test has no safe way to fabricate (see `tests/supervisor_win.rs`
    /// for that half, exercised end-to-end against a real process); the
    /// worker-bearing states are what this function actually exists for
    /// and are fully exercisable here.
    #[test]
    fn take_worker_handle_extracts_the_handle_from_a_worker_bearing_state() {
        let (_tx, rx) = mpsc::channel::<RecoveryOutcome>();
        let mut recovering = Lifecycle::Recovering { rx, handle: std::thread::spawn(|| {}), started_at: Instant::now() };
        assert!(take_worker_handle(&mut recovering).is_some());

        let mut ended = Lifecycle::EndedNoRespawn;
        assert!(take_worker_handle(&mut ended).is_none());
    }
}
