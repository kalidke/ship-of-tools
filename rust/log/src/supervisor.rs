//! ADR 0041 step 6 U2: the authority. `sot-capsule supervise` is the
//! `sot-capsule supervise` process the launcher starts (Lifecycle "ONE
//! AUTHORITY... Every act that starts, ends, adopts or resets a run is
//! performed by the process holding `<state-dir>\supervisor.lock`");
//! [`endrun`] and [`reset`] are the no-supervisor path's own
//! fence-acquiring in-process callers ("the same TRANSITION, not the
//! same CAPABILITIES").
//!
//! Ties together every U0-U2 library piece: [`crate::fence`] (the
//! election), [`crate::journal`] (durable operation records),
//! [`crate::pointer`] (drawer.voyage discovery), [`crate::classify`]
//! (the probe), [`crate::lease`] (the parent-death lease a spawned leg
//! checks), the supervisor lane's own pipe
//! ([`crate::pipe_win::PipeServer::bind_supervisor`]) and wire protocol
//! ([`crate::wire`]'s `Supervisor*` types), and
//! [`crate::recovery::latest_leg_state`] /
//! [`crate::verify::leg_carries_run_end_marker`] (the start-mode table).
//!
//! # Scope notes (resolved ambiguities / documented simplifications)
//!
//! - **Rollout evidence stays a U4 concern.** `rollout.rs`'s own doc
//!   pins the release-apply transaction that WRITES real
//!   `RolloutEvidence` as U4's work, not built here. This module is in
//!   the exact same "no real evidence" position `sot-capsule run`'s
//!   manual-testing harness already is, so it applies the SAME explicit,
//!   honestly-named override: `supervise` refuses to run without
//!   `--assume-no-rollback-target`, and passes the identical flag down
//!   to every leg it spawns — never a second, silently-invented file
//!   format standing in for U4's transaction.
//! - **The first-ever voyage.** `pointer::validate` returning `NotFound`
//!   is not, by that module's own doc, a licence to mint a fresh voyage
//!   — "the caller ... decides what 'no drawer yet' means." This module
//!   decides: `--start` with no pointer at all is a legitimate first-ever
//!   run (mint one); `--resume` with no pointer at all has nothing to
//!   resume (a loud, terminal refusal) — see [`discover_or_mint_voyage`].
//! - **`record_verified`'s O(retained history) walk is delegated to a
//!   background thread**, matching "never inside an interactive wait" —
//!   but the supervisor's own exit path joins any still-running verify
//!   thread before returning, matching "before reporting `record_verified`
//!   or **exiting 0**."
//! - **`reset` while a leg is live** has no dedicated wire refusal reason
//!   (`SupervisorRefusedReason` has none for it) — this module refuses it
//!   through the generic `Failed{detail}` shape rather than inventing a
//!   new wire variant for a single caller.
//! - **A single pending operation is assumed during journal recovery**
//!   for resolving which voyage an `end_run` targeted (`ActiveRecord::
//!   old_voyage`, read once per operation during the recovery sweep) —
//!   two DIFFERENT unresolved operations racing a crash in the same
//!   window is not specifically hardened beyond what the journal's own
//!   per-id atomicity already provides.

#![cfg(windows)]

use crate::challenge::{self, ChallengeOutcome, ChallengedProcess};
use crate::classify::{self, ProbeOutcome};
use crate::journal;
use crate::pipe_win::{self, ConnId, PipeServer, TransportEvent};
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
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------
// The numbers (ADR 0041 "The numbers, pinned here so no implementation
// invents them"). B is the ONE free number; every DERIVED row below is a
// formula over it, exactly as the ADR's own table states. B is
// PROVISIONAL until measured (60s today).
// ---------------------------------------------------------------------

/// B: the supported history bound.
const SUPPORTED_HISTORY_BOUND: Duration = Duration::from_secs(60);
/// Readiness cutoff and probe episode are both DERIVED `= B`.
const READINESS_CUTOFF: Duration = SUPPORTED_HISTORY_BOUND;
const PROBE_EPISODE: Duration = SUPPORTED_HISTORY_BOUND;
/// Anti-flap's stability interval is DERIVED `= readiness cutoff`.
const STABILITY_INTERVAL: Duration = READINESS_CUTOFF;
const KILL_WAIT_BOUND: Duration = Duration::from_secs(10);
const ATTEMPT_INTERVAL: Duration = Duration::from_millis(500);
const FLAP_THRESHOLD: u32 = 3;
/// Not ADR-pinned (the ADR's "mgmt idle | 5s" bound is the voyage mgmt
/// lane's own number) — applied here by extension, for the same "pool
/// squatting" reason, to the supervisor lane's own idle connections.
const LANE_IDLE_DEADLINE: Duration = Duration::from_secs(5);
/// The supervisor lane's own low-frequency control traffic needs nowhere
/// near the voyage pipe's subscriber-driven cap; a small, generous
/// constant, like the manual harness's own `MAX_PIPE_INSTANCES`.
const MAX_LANE_INSTANCES: u32 = 8;
/// The main loop's own poll granularity — comfortably inside every
/// deadline above, never a source of the numbers themselves.
const MAIN_LOOP_POLL: Duration = Duration::from_millis(100);

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
    /// What a spawned leg's producer should run (its own `argv`, after
    /// `--`) — this module has no opinion on it.
    pub producer_argv: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    /// The ONE explicit, honestly-named override this module accepts
    /// before U4's release-apply transaction exists — see the module's
    /// own doc. `false` refuses to run at all.
    pub assume_no_rollback_target: bool,
}

/// `sot-capsule supervise`'s own entry point — never panics by design;
/// every expected failure maps to [`EXIT_TERMINAL`], every success path
/// to [`EXIT_CLEAN`], and this function's own `main` caller passes
/// whatever it returns straight to `std::process::exit`.
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

/// `sot-capsule endrun`'s own entry point (the no-supervisor path): fails
/// closed on every ambiguous capsule state per the ADR's capability
/// matrix ("present but invalid: REFUSE LOUDLY ... it may never
/// terminate an unauthenticated same-user process").
pub fn endrun(state_dir: &Path, voyage: Option<String>, reason: String) -> i32 {
    match endrun_inner(state_dir, voyage, reason) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sot-capsule endrun: {e}");
            EXIT_TERMINAL
        }
    }
}

/// `sot-capsule reset`'s own entry point (the no-supervisor path): only
/// ever proceeds on a classifier ABSENT taken while holding the fence.
pub fn reset(state_dir: &Path, voyage: Option<String>) -> i32 {
    match reset_inner(state_dir, voyage) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sot-capsule reset: {e}");
            EXIT_TERMINAL
        }
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

/// A stable hash of the canonicalized state-dir path (ADR 0041 Lifecycle
/// "Name and identity") — scopes the supervisor lane's pipe name and the
/// parent-death lease's own name identically, "the same thing that
/// scopes the pointer and the fence."
fn state_dir_hash(state_dir: &Path) -> String {
    use sha2::{Digest as _, Sha256};
    let canonical = std::fs::canonicalize(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(canonical.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// This process's own pid and creation time, packed to the exact
/// FILETIME bits the wire's `status_ok`/`hello_ok` carry — the same
/// pattern `capsule_win.rs`'s own `self_status` uses, duplicated rather
/// than shared cross-module for the same reason `pipe_win.rs` duplicates
/// `wide_null`: a three-line leaf helper isn't worth a shared dependency.
fn self_pid_and_created() -> std::io::Result<(u32, u64)> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetProcessTimes};
    // SAFETY: `GetCurrentProcess` needs no close; the four FILETIME
    // out-params are stack-local, valid to write into regardless of the
    // call's outcome.
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

// ---------------------------------------------------------------------
// Pointer discovery / mint (supervisor startup only)
// ---------------------------------------------------------------------

/// `Some(voyage_id)` to proceed; `None` means the `--resume` "sealed,
/// carrying its own `run_end_requested`" row — exit 0, never spawn.
fn discover_or_mint_voyage(state_dir: &Path, mode: StartMode) -> crate::Result<Option<String>> {
    match pointer::validate(state_dir) {
        PointerState::Valid(id) => Ok(Some(id)),
        PointerState::NotFound => match mode {
            StartMode::Start => {
                std::fs::create_dir_all(voyages_dir(state_dir))?;
                let id = uuid::Uuid::now_v7().to_string();
                VoyageStore::bootstrap(&voyage_root_path(state_dir, &id), &id, RetentionClass::Archive)?;
                pointer::publish(state_dir, &id)?;
                Ok(Some(id))
            }
            StartMode::Resume => Err(err_state(
                "--resume with no drawer.voyage pointer at all: nothing to resume",
            )),
        },
        PointerState::Corrupt => Err(err_state("drawer.voyage is corrupt — run `sot-capsule reset`")),
        PointerState::OtherIo(e) => Err(e.into()),
    }
}

/// The start-mode table's OWN "what to do about the latest leg" half (ADR
/// 0041 Lifecycle "Startup authorization is a mode, not an identity"),
/// consulted ONLY when no live capsule was adopted. Returns `true` to
/// spawn a fresh leg, `false` for the one row that must not
/// (`--resume`, sealed, carrying its own marker).
fn should_spawn_after_absent(state_dir: &Path, voyage_id: &str, mode: StartMode) -> crate::Result<bool> {
    if mode == StartMode::Start {
        return Ok(true); // "anything -> adopt if live, else spawn"
    }
    let seg_dir = voyage_root_path(state_dir, voyage_id).join("seg");
    match recovery::latest_leg_state(&seg_dir).map_err(crate::Error::Io)? {
        LatestLegState::NoLeg | LatestLegState::Unsealed { .. } => Ok(true),
        LatestLegState::Sealed { epoch } => {
            let marked = verify::leg_carries_run_end_marker(&seg_dir, voyage_id, epoch)?;
            Ok(!marked)
        }
    }
}

// ---------------------------------------------------------------------
// The active leg
// ---------------------------------------------------------------------

enum ActiveLeg {
    None,
    Ready { process: ChallengedProcess, ready_at: Instant },
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
        // The supervisor is in the exact same "no real rollout evidence"
        // position the manual harness is until U4 — see the module doc.
        .arg("--assume-no-rollback-target")
        .arg("--")
        .args(producer_argv);
    command
}

// ---------------------------------------------------------------------
// EndRun over the voyage's own mgmt lane (the ADR's "healthy" row of the
// no-supervisor capability matrix, reused identically by the supervisor
// acting "on its own behalf")
// ---------------------------------------------------------------------

enum EndRunOutcome {
    /// No capsule to end — already gone.
    Absent,
    Foreign,
    Pending,
    Ended(ChallengedProcess),
}

/// Read exactly one wire frame from `conn`, bounded by `deadline` —
/// cancellable via [`crate::deadline::run_with_deadline`], the SAME
/// mechanism `challenge::challenge` itself uses, so a hung server cannot
/// block this past its budget.
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
            conn.write_all(&request)?;
            // The reply's own content carries no new information (the v0
            // mgmt lane has no refusal shape at all — every reply tag
            // means success); reading it is only proof the ack was
            // physically delivered before this connection's own EOF.
            let _ = read_one_frame(&conn, Instant::now() + Duration::from_secs(5))?;
            Ok(EndRunOutcome::Ended(process))
        }
    }
}

// ---------------------------------------------------------------------
// Reset
// ---------------------------------------------------------------------

/// Rename the current pointer aside (evidence-preserving, unique,
/// no-replace) if one exists, then mint `new_voyage` fresh. Idempotent
/// enough for recovery's own retry: a `new_voyage` that already has a
/// bootstrapped store is left alone rather than re-bootstrapped.
fn reset_pointer(state_dir: &Path, new_voyage: &str) -> crate::Result<()> {
    let live = pointer::pointer_path(state_dir);
    if live.exists() {
        let mut nonce_bytes = [0u8; 8];
        getrandom::fill(&mut nonce_bytes).map_err(std::io::Error::from)?;
        let nonce = u64::from_le_bytes(nonce_bytes);
        let aside = state_dir.join(format!("drawer.voyage.reset-{nonce:016x}"));
        crate::fsutil::publish_noreplace(&live, &aside)?;
    }
    std::fs::create_dir_all(voyages_dir(state_dir))?;
    let root = voyage_root_path(state_dir, new_voyage);
    if !root.exists() {
        VoyageStore::bootstrap(&root, new_voyage, RetentionClass::Archive)?;
    }
    pointer::publish(state_dir, new_voyage)?;
    Ok(())
}

// ---------------------------------------------------------------------
// Journal recovery, run FIRST (ADR 0041 Lifecycle "Recovery is part of
// the transaction, and it runs FIRST")
// ---------------------------------------------------------------------

fn reconcile_journal_on_startup(state_dir: &Path) -> crate::Result<()> {
    for op_id in journal::active_operations(state_dir)? {
        let Some(active) = journal::read_active(state_dir, &op_id)? else { continue };
        if let Some(new_voyage) = &active.intended_new_voyage {
            reconcile_reset(state_dir, &op_id, new_voyage, active.old_voyage.as_deref())?;
        } else if let Some(epoch) = active.end_run_epoch {
            reconcile_end_run(state_dir, &op_id, active.old_voyage.as_deref(), epoch)?;
        } else {
            // `stop`: reaching this line at all means the process
            // restarted, so the operator already knows the supervisor
            // cycled — finishing it as stopping is harmless bookkeeping,
            // never a second destructive act.
            journal::finish(state_dir, &op_id, &journal::TerminalRecord::Stopping)?;
        }
    }
    Ok(())
}

fn reconcile_reset(
    state_dir: &Path,
    op_id: &str,
    new_voyage: &str,
    old_voyage: Option<&str>,
) -> crate::Result<()> {
    match pointer::validate(state_dir) {
        PointerState::Valid(id) if id == new_voyage => {}
        PointerState::Valid(id) if Some(id.as_str()) == old_voyage => {
            reset_pointer(state_dir, new_voyage)?;
        }
        PointerState::NotFound => {
            reset_pointer(state_dir, new_voyage)?;
        }
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

fn reconcile_end_run(
    state_dir: &Path,
    op_id: &str,
    voyage_id: Option<&str>,
    epoch: u64,
) -> crate::Result<()> {
    let Some(voyage_id) = voyage_id else {
        return journal::finish(
            state_dir,
            op_id,
            &journal::TerminalRecord::Failed { detail: "no voyage recorded for this end_run".into() },
        );
    };
    let seg_dir = voyage_root_path(state_dir, voyage_id).join("seg");
    let marked = verify::leg_carries_run_end_marker(&seg_dir, voyage_id, epoch)?;
    if !marked {
        return journal::finish(
            state_dir,
            op_id,
            &journal::TerminalRecord::Failed { detail: "record_append".into() },
        );
    }
    journal::mark_closed(state_dir, op_id)?;
    let root = voyage_root_path(state_dir, voyage_id);
    let terminal = match verify::verify_voyage(&root, voyage_id) {
        Ok(()) => journal::TerminalRecord::RecordVerified,
        Err(e) => journal::TerminalRecord::Failed { detail: format!("verify_voyage: {e}") },
    };
    journal::finish(state_dir, op_id, &terminal)
}

// ---------------------------------------------------------------------
// The supervisor lane's own connection state machine
// ---------------------------------------------------------------------

struct Conn {
    splitter: wire::FrameSplitter,
    hello_ok: bool,
    last_activity: Instant,
}

/// Everything the lane's own command/query/status handling needs —
/// deliberately separate from the main loop's own leg-supervision
/// variables so the borrow-checker never has to reason about both at
/// once inside one giant function.
struct AuthorityState {
    state_dir: PathBuf,
    voyage_id: Option<String>,
    leg_epoch: Option<u64>,
    phase: SupervisorPhase,
    no_respawn: bool,
    stop_requested: bool,
    self_pid: u32,
    self_created: u64,
    verify_handle: Option<std::thread::JoinHandle<()>>,
}

impl AuthorityState {
    fn handle_request(&mut self, active_leg_is_ready: bool, req: SupervisorRequest) -> SupervisorReply {
        match req {
            SupervisorRequest::Hello { .. } => unreachable!("handled by the caller before this is reached"),
            SupervisorRequest::Status => SupervisorReply::StatusOk {
                pid: self.self_pid,
                created: self.self_created,
                voyage: self.voyage_id.clone(),
                leg: self.leg_epoch,
                phase: self.phase,
            },
            SupervisorRequest::Command { operation_id, op } => {
                SupervisorReply::Operation(self.handle_command(active_leg_is_ready, operation_id, op))
            }
            SupervisorRequest::Query { operation_id } => {
                SupervisorReply::Operation(self.query_state(&operation_id))
            }
        }
    }

    fn query_state(&self, operation_id: &str) -> SupervisorOperationState {
        match journal::read_terminal(&self.state_dir, operation_id) {
            Ok(Some(t)) => return terminal_to_wire(t),
            Ok(None) => {}
            Err(e) => return SupervisorOperationState::Failed { detail: format!("journal read failed: {e}") },
        }
        match journal::is_closed(&self.state_dir, operation_id) {
            Ok(true) => return SupervisorOperationState::RecordClosed,
            Ok(false) => {}
            Err(e) => return SupervisorOperationState::Failed { detail: format!("journal read failed: {e}") },
        }
        match journal::read_active(&self.state_dir, operation_id) {
            Ok(Some(_)) => SupervisorOperationState::Accepted,
            Ok(None) => SupervisorOperationState::UnknownOperation,
            Err(e) => SupervisorOperationState::Failed { detail: format!("journal read failed: {e}") },
        }
    }

    fn handle_command(
        &mut self,
        active_leg_is_ready: bool,
        operation_id: String,
        op: SupervisorOp,
    ) -> SupervisorOperationState {
        // Lifecycle commands are VOYAGE-FENCED (ADR 0041): a mismatch is
        // `refused {stale_voyage}` with NO MUTATION — checked before the
        // journal is ever touched.
        let observed = match &op {
            SupervisorOp::EndRun { voyage, .. } => Some(voyage.clone()),
            SupervisorOp::Reset { voyage } => voyage.clone(),
            SupervisorOp::Stop => None,
        };
        if let Some(observed) = &observed {
            if Some(observed) != self.voyage_id.as_ref() {
                return SupervisorOperationState::Refused { reason: wire::SupervisorRefusedReason::StaleVoyage };
            }
        }

        let digest = format!("{op:?}");
        match journal::read_active(&self.state_dir, &operation_id) {
            Ok(Some(existing)) if existing.digest != digest => {
                return SupervisorOperationState::Refused { reason: wire::SupervisorRefusedReason::IdConflict };
            }
            Ok(Some(_)) => return self.query_state(&operation_id), // idempotent resubmit
            Ok(None) => {}
            Err(e) => return SupervisorOperationState::Failed { detail: format!("journal read failed: {e}") },
        }

        let intended_new_voyage = match &op {
            SupervisorOp::Reset { .. } => Some(uuid::Uuid::now_v7().to_string()),
            _ => None,
        };
        let record = journal::ActiveRecord {
            digest,
            intended_new_voyage: intended_new_voyage.clone(),
            old_voyage: self.voyage_id.clone(),
            end_run_epoch: matches!(&op, SupervisorOp::EndRun { .. }).then_some(self.leg_epoch).flatten(),
        };
        if let Err(e) = journal::begin(&self.state_dir, &operation_id, &record) {
            return SupervisorOperationState::Failed { detail: format!("journal begin failed: {e}") };
        }

        match op {
            SupervisorOp::EndRun { reason, .. } => self.perform_end_run(&operation_id, &reason),
            SupervisorOp::Reset { .. } => {
                if active_leg_is_ready {
                    let t = journal::TerminalRecord::Failed {
                        detail: "a leg is currently live; end the run before resetting".into(),
                    };
                    let _ = journal::finish(&self.state_dir, &operation_id, &t);
                    return terminal_to_wire(t);
                }
                self.perform_reset(&operation_id, intended_new_voyage.expect("set above for Reset"))
            }
            SupervisorOp::Stop => {
                self.stop_requested = true;
                let t = journal::TerminalRecord::Stopping;
                let _ = journal::finish(&self.state_dir, &operation_id, &t);
                terminal_to_wire(t)
            }
        }
    }

    fn perform_end_run(&mut self, operation_id: &str, reason: &str) -> SupervisorOperationState {
        let Some(voyage_id) = self.voyage_id.clone() else {
            let t = journal::TerminalRecord::Failed { detail: "no leg has ever started".into() };
            let _ = journal::finish(&self.state_dir, operation_id, &t);
            return terminal_to_wire(t);
        };
        let outcome = match end_run_over_mgmt_lane(&voyage_id, reason) {
            Ok(o) => o,
            Err(e) => {
                let t = journal::TerminalRecord::Failed { detail: format!("{e}") };
                let _ = journal::finish(&self.state_dir, operation_id, &t);
                return terminal_to_wire(t);
            }
        };
        match outcome {
            EndRunOutcome::Absent => {
                self.no_respawn = true;
                self.phase = SupervisorPhase::EndedNoRespawn;
                let t = journal::TerminalRecord::RecordClosed; // nothing left to verify
                let _ = journal::finish(&self.state_dir, operation_id, &t);
                terminal_to_wire(t)
            }
            EndRunOutcome::Foreign | EndRunOutcome::Pending => {
                let t = journal::TerminalRecord::Failed {
                    detail: "the capsule's mgmt lane is foreign or unresponsive".into(),
                };
                let _ = journal::finish(&self.state_dir, operation_id, &t);
                terminal_to_wire(t)
            }
            EndRunOutcome::Ended(process) => {
                self.no_respawn = true;
                self.phase = SupervisorPhase::Ending;
                let _ = process.wait(SUPPORTED_HISTORY_BOUND + KILL_WAIT_BOUND);
                if let Err(e) = journal::mark_closed(&self.state_dir, operation_id) {
                    let t = journal::TerminalRecord::Failed { detail: format!("mark_closed: {e}") };
                    let _ = journal::finish(&self.state_dir, operation_id, &t);
                    return terminal_to_wire(t);
                }
                self.phase = SupervisorPhase::EndedNoRespawn;
                // record_verified: O(retained history) -- delegated to a
                // background thread (ADR 0041: "never inside an
                // interactive wait"); `supervise_inner`'s own exit path
                // joins this handle before returning 0.
                let root = voyage_root_path(&self.state_dir, &voyage_id);
                let state_dir = self.state_dir.clone();
                let op_id = operation_id.to_string();
                let voyage_id_for_thread = voyage_id.clone();
                self.verify_handle = Some(std::thread::spawn(move || {
                    let terminal = match verify::verify_voyage(&root, &voyage_id_for_thread) {
                        Ok(()) => journal::TerminalRecord::RecordVerified,
                        Err(e) => journal::TerminalRecord::Failed { detail: format!("verify_voyage: {e}") },
                    };
                    let _ = journal::finish(&state_dir, &op_id, &terminal);
                }));
                SupervisorOperationState::RecordClosed
            }
        }
    }

    fn perform_reset(&mut self, operation_id: &str, new_voyage: String) -> SupervisorOperationState {
        match reset_pointer(&self.state_dir, &new_voyage) {
            Ok(()) => {
                self.voyage_id = Some(new_voyage.clone());
                self.leg_epoch = None;
                let t = journal::TerminalRecord::ResetDone { new_voyage };
                let _ = journal::finish(&self.state_dir, operation_id, &t);
                terminal_to_wire(t)
            }
            Err(e) => {
                let t = journal::TerminalRecord::Failed { detail: format!("{e}") };
                let _ = journal::finish(&self.state_dir, operation_id, &t);
                terminal_to_wire(t)
            }
        }
    }
}

fn terminal_to_wire(t: journal::TerminalRecord) -> SupervisorOperationState {
    match t {
        journal::TerminalRecord::RecordClosed => SupervisorOperationState::RecordClosed,
        journal::TerminalRecord::RecordVerified => SupervisorOperationState::RecordVerified,
        journal::TerminalRecord::ResetDone { new_voyage } => SupervisorOperationState::ResetDone { new_voyage },
        journal::TerminalRecord::Stopping => SupervisorOperationState::Stopping,
        journal::TerminalRecord::Failed { detail } => SupervisorOperationState::Failed { detail },
        journal::TerminalRecord::Refused { reason } => SupervisorOperationState::Failed {
            // The journal's own `Refused{reason: String}` is a free-form
            // diagnostic (it can record ANY refusal, including ones with
            // no corresponding `SupervisorRefusedReason` variant); wire
            // refusals are minted directly by `handle_command` before the
            // journal is ever touched (stale_voyage/id_conflict), so
            // nothing durable ever needs to round-trip back through this
            // arm today — kept total rather than `unreachable!()` so a
            // FUTURE journal-recorded refusal has somewhere honest to go.
            detail: format!("refused: {reason}"),
        },
    }
}

fn service_lane(
    lane: &PipeServer,
    conns: &mut HashMap<ConnId, Conn>,
    authority: &mut AuthorityState,
    active_leg_is_ready: bool,
    now: Instant,
) {
    while let Ok(event) = lane.events().try_recv() {
        match event {
            TransportEvent::Accepted(id) => {
                conns.insert(id, Conn { splitter: wire::FrameSplitter::new(), hello_ok: false, last_activity: now });
            }
            TransportEvent::Bytes(id, bytes) => {
                handle_lane_bytes(lane, conns, id, &bytes, authority, active_leg_is_ready, now);
            }
            TransportEvent::Closed(id, _reason) => {
                conns.remove(&id);
            }
            TransportEvent::Sent(_, _) => {}
            TransportEvent::AcceptError(e) => {
                eprintln!("sot-capsule supervise: supervisor lane accept error: {e}");
            }
        }
    }
    let idle: Vec<ConnId> = conns
        .iter()
        .filter(|(_, c)| now.saturating_duration_since(c.last_activity) >= LANE_IDLE_DEADLINE)
        .map(|(id, _)| *id)
        .collect();
    for id in idle {
        lane.close(id);
        conns.remove(&id);
    }
}

fn handle_lane_bytes(
    lane: &PipeServer,
    conns: &mut HashMap<ConnId, Conn>,
    id: ConnId,
    bytes: &[u8],
    authority: &mut AuthorityState,
    active_leg_is_ready: bool,
    now: Instant,
) {
    let mut close_after = false;
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
                        let _ = lane.send(id, reply, None);
                        close_after = true;
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
                DecodedFrame::SupervisorRequest(_) if !conn.hello_ok => {
                    // Hello must be the first frame of every connection.
                    close_after = true;
                    break;
                }
                DecodedFrame::SupervisorRequest(req) => {
                    let reply = authority.handle_request(active_leg_is_ready, req);
                    let bytes = wire::encode_supervisor_reply(&reply).expect("every reply field is pre-bounded");
                    let _ = lane.send(id, bytes, None);
                }
                _ => {
                    // A reply-shaped frame, or another lane's magic —
                    // never legitimate on this connection.
                    close_after = true;
                    break;
                }
            }
        }
        if err.is_some() {
            close_after = true;
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

    // The lane: bound AFTER the fence, BEFORE any adopt or spawn (ADR
    // 0041 Lifecycle "Lifetime, bracketed by the fence").
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

    // Recovery FIRST — before pointer discovery, before start-mode
    // authorization, before admitting any new command.
    reconcile_journal_on_startup(&config.state_dir)?;

    let voyage_id = match discover_or_mint_voyage(&config.state_dir, config.mode)? {
        Some(id) => id,
        None => return Ok(EXIT_CLEAN),
    };

    let self_ids = self_pid_and_created().unwrap_or((0, 0));
    let mut authority = AuthorityState {
        state_dir: config.state_dir.clone(),
        voyage_id: Some(voyage_id.clone()),
        leg_epoch: leg_epoch_of(&config.state_dir, &voyage_id),
        phase: SupervisorPhase::Starting,
        no_respawn: false,
        stop_requested: false,
        self_pid: self_ids.0,
        self_created: self_ids.1,
        verify_handle: None,
    };
    let mut conns: HashMap<ConnId, Conn> = HashMap::new();
    let capsule_exe = std::env::current_exe().map_err(crate::Error::Io)?;
    let voyage_root = voyage_root_path(&config.state_dir, &voyage_id);

    // The FIRST placement decision (adopt-if-live, else consult the
    // start-mode table) — happens exactly once, at supervisor startup.
    let episode_deadline = Instant::now() + PROBE_EPISODE;
    let first_probe = classify::probe_adopt_only(&RealProbeOps, &voyage_id, &voyage_root, episode_deadline, ATTEMPT_INTERVAL);
    let mut active_leg = match first_probe {
        ProbeOutcome::Adopted(process) => {
            authority.phase = SupervisorPhase::Ready;
            authority.leg_epoch = leg_epoch_of(&config.state_dir, &voyage_id);
            ActiveLeg::Ready { process, ready_at: Instant::now() }
        }
        ProbeOutcome::Absent => {
            if !should_spawn_after_absent(&config.state_dir, &voyage_id, config.mode)? {
                return Ok(EXIT_CLEAN);
            }
            ActiveLeg::None
        }
        ProbeOutcome::Foreign | ProbeOutcome::Wedged => {
            eprintln!("sot-capsule supervise: the voyage pipe is foreign or unreachable at startup");
            return Ok(EXIT_TERMINAL);
        }
        other => {
            return Err(err_state(format!("unexpected probe_adopt_only outcome at startup: {other:?}")));
        }
    };

    let mut consecutive_unstable_legs: u32 = 0;

    let exit_code = 'authority: loop {
        if authority.stop_requested {
            break 'authority EXIT_CLEAN;
        }

        match &active_leg {
            ActiveLeg::None => {
                if authority.no_respawn {
                    // ENDED-NO-RESPAWN: keep serving query/status/stop
                    // until an explicit stop or this process is killed.
                    service_lane(&lane, &mut conns, &mut authority, false, Instant::now());
                    std::thread::sleep(MAIN_LOOP_POLL);
                    continue 'authority;
                }
                let readiness_cutoff = Instant::now() + READINESS_CUTOFF;
                let mut command = build_run_command(
                    &capsule_exe,
                    &voyage_root,
                    &voyage_id,
                    config.cols,
                    config.rows,
                    &lease_name,
                    &config.producer_argv,
                );
                let outcome = classify::probe_owned_spawn(
                    &RealProbeOps,
                    &mut command,
                    &voyage_id,
                    readiness_cutoff,
                    KILL_WAIT_BOUND,
                    ATTEMPT_INTERVAL,
                );
                match outcome {
                    ProbeOutcome::Ready(process) => {
                        authority.phase = SupervisorPhase::Ready;
                        authority.leg_epoch = leg_epoch_of(&config.state_dir, &voyage_id);
                        active_leg = ActiveLeg::Ready { process, ready_at: Instant::now() };
                    }
                    ProbeOutcome::SpawnFailed(e) => {
                        eprintln!("sot-capsule supervise: spawn failed: {e}");
                        consecutive_unstable_legs += 1;
                        if consecutive_unstable_legs >= FLAP_THRESHOLD {
                            break 'authority EXIT_TERMINAL;
                        }
                    }
                    ProbeOutcome::KilledAfterTimeout | ProbeOutcome::LegEnded => {
                        consecutive_unstable_legs += 1;
                        if consecutive_unstable_legs >= FLAP_THRESHOLD {
                            break 'authority EXIT_TERMINAL;
                        }
                    }
                    ProbeOutcome::KillOrWaitFailed(e) => {
                        eprintln!("sot-capsule supervise: kill/wait failed: {e}");
                        break 'authority EXIT_TERMINAL;
                    }
                    other => {
                        return Err(err_state(format!("unexpected probe_owned_spawn outcome: {other:?}")));
                    }
                }
            }
            ActiveLeg::Ready { process, ready_at } => {
                let ready_at = *ready_at;
                service_lane(&lane, &mut conns, &mut authority, true, Instant::now());
                match process.wait(MAIN_LOOP_POLL) {
                    Ok(true) => {
                        // The leg ended. Stability is measured from when
                        // it became ready.
                        if ready_at.elapsed() < STABILITY_INTERVAL {
                            consecutive_unstable_legs += 1;
                        } else {
                            consecutive_unstable_legs = 0;
                        }
                        active_leg = ActiveLeg::None;
                        authority.leg_epoch = None;
                        if authority.no_respawn {
                            authority.phase = SupervisorPhase::EndedNoRespawn;
                        } else if consecutive_unstable_legs >= FLAP_THRESHOLD {
                            break 'authority EXIT_TERMINAL;
                        } else {
                            authority.phase = SupervisorPhase::Starting;
                        }
                    }
                    Ok(false) => {} // still running
                    Err(e) => {
                        eprintln!("sot-capsule supervise: wait on the leg's process handle failed: {e}");
                        break 'authority EXIT_TERMINAL;
                    }
                }
            }
        }
    };

    // Never report record_verified or exit 0 before a still-running
    // verify thread has actually finished.
    if let Some(handle) = authority.verify_handle.take() {
        let _ = handle.join();
    }
    // `lane`'s own `Drop` performs the teardown (`disconnect_listener`
    // then `join_workers` under `TEARDOWN_AGGREGATE_DEADLINE`) when it
    // goes out of scope below — no separate call needed.
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
    match end_run_over_mgmt_lane(&voyage_id, &reason)? {
        EndRunOutcome::Absent => {
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
        EndRunOutcome::Ended(process) => {
            let _ = process.wait(SUPPORTED_HISTORY_BOUND + KILL_WAIT_BOUND);
            let root = voyage_root_path(state_dir, &voyage_id);
            match verify::verify_voyage(&root, &voyage_id) {
                Ok(()) => {
                    eprintln!("sot-capsule endrun: record_closed, record_verified");
                    Ok(EXIT_CLEAN)
                }
                Err(e) => {
                    eprintln!("sot-capsule endrun: record_closed, but verify_voyage failed: {e}");
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
    let observed = voyage.or_else(|| match pointer::validate(state_dir) {
        PointerState::Valid(id) => Some(id),
        _ => None,
    });
    // Capability matrix: reset only ever proceeds on a classifier ABSENT
    // taken while holding the fence.
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
    reset_pointer(state_dir, &new_voyage)?;
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
        let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap().unwrap();
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
        let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap().unwrap();
        let again = discover_or_mint_voyage(dir.path(), StartMode::Resume).unwrap().unwrap();
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
        let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap().unwrap();
        assert!(should_spawn_after_absent(dir.path(), &id, StartMode::Start).unwrap());
    }

    #[test]
    fn should_spawn_after_absent_resume_with_no_leg_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let id = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap().unwrap();
        assert!(should_spawn_after_absent(dir.path(), &id, StartMode::Resume).unwrap());
    }

    #[test]
    fn reset_pointer_renames_the_old_one_aside_and_mints_the_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let old = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap().unwrap();
        let new_voyage = uuid::Uuid::now_v7().to_string();
        reset_pointer(dir.path(), &new_voyage).unwrap();
        assert!(matches!(pointer::validate(dir.path()), PointerState::Valid(v) if v == new_voyage));
        assert!(voyage_root_path(dir.path(), &new_voyage).exists());
        // The old voyage's own store is untouched -- only the POINTER
        // moved, never the data.
        assert!(voyage_root_path(dir.path(), &old).exists());
    }

    #[test]
    fn reconcile_reset_recovers_all_four_states() {
        let dir = tempfile::tempdir().unwrap();
        let old = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap().unwrap();
        let new_voyage = uuid::Uuid::now_v7().to_string();

        // Row 1: pointer still names the OLD voyage -- resume from the
        // beginning.
        reconcile_reset(dir.path(), "op-1", &new_voyage, Some(&old)).unwrap();
        assert!(matches!(pointer::validate(dir.path()), PointerState::Valid(v) if v == new_voyage));
        assert_eq!(
            journal_state(dir.path(), "op-1"),
            Some(journal::TerminalRecord::ResetDone { new_voyage: new_voyage.clone() })
        );

        // Row 3: pointer already names the INTENDED NEW voyage -- just
        // reconstruct the terminal fact.
        reconcile_reset(dir.path(), "op-2", &new_voyage, Some(&old)).unwrap();
        assert_eq!(
            journal_state(dir.path(), "op-2"),
            Some(journal::TerminalRecord::ResetDone { new_voyage: new_voyage.clone() })
        );

        // Row 4: pointer names something else entirely -- loud stop.
        let rogue = uuid::Uuid::now_v7().to_string();
        assert!(reconcile_reset(dir.path(), "op-3", &rogue, Some(&old)).is_err());

        // Row 2: pointer ABSENT with the evidence rename present --
        // resume from publication.
        std::fs::remove_file(pointer::pointer_path(dir.path())).unwrap();
        let third = uuid::Uuid::now_v7().to_string();
        reconcile_reset(dir.path(), "op-4", &third, Some(&new_voyage)).unwrap();
        assert!(matches!(pointer::validate(dir.path()), PointerState::Valid(v) if v == third));
    }

    fn journal_state(state_dir: &Path, op_id: &str) -> Option<journal::TerminalRecord> {
        journal::read_terminal(state_dir, op_id).unwrap()
    }
}
