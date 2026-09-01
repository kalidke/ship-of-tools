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
//! # `Lifecycle`: one state machine, not four independent fields
//!
//! An earlier version of this module tracked "what phase is the
//! authority in" across FOUR independently-mutated pieces — a
//! `SupervisorPhase`, a `no_respawn: bool`, a `stop_requested: bool`, and
//! a separate `ActiveLeg` enum — plus a SECOND, independently-tracked
//! voyage id in `supervise_inner` itself that `perform_reset` had to keep
//! in sync by hand. That combination admitted states this authority can
//! never actually be in (e.g. "no_respawn but still mid-spawn") and one
//! it silently forgot to represent (an `end_run` submitted while spawning
//! had nowhere honest to record "there is no leg yet"). [`Lifecycle`]
//! replaces all of it: one tag, one shape per phase (Codex review round
//! 1, simplicity audit).
//!
//! # The lane is serviced in EVERY phase (Codex review round 1, finding 1)
//!
//! An earlier version blocked the ENTIRE supervisor lane — no `status`,
//! no `query`, nothing — for as long as an initial adopt-only probe, an
//! owned spawn attempt, or an `end_run`'s mgmt-lane exchange+wait+verify
//! took (up to ~70s for the last one), because each ran as a synchronous
//! call directly inside the one function that also serviced the lane.
//! Every one of those OS-facing operations now runs on its own
//! background thread; the main loop polls each thread's result
//! non-blockingly and services the lane on EVERY iteration regardless of
//! which phase the authority is in — "one linearized STATE MACHINE, not
//! one blocking thread." Journal writes for a given operation id stay
//! confined to whichever thread owns that operation's lifecycle
//! end-to-end (the pattern this crate's own `record_verified` background
//! thread already established before this round); at most one mutating
//! operation is ever in flight at a time (voyage-fenced, single
//! authority), so no two threads ever contend for the same `.active`/
//! `.terminal` file pair.
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
//!   resume (a loud, terminal refusal) — see [`discover_or_mint_voyage`],
//!   which therefore never itself needs an `Option` return: every path
//!   through it either yields an id or refuses loudly (an earlier
//!   version's `Option<String>` return type had a `None` arm no branch
//!   ever actually produced — Codex review round 1, simplicity audit).
//! - **`record_verified`'s O(retained history) walk, and every other
//!   OS-facing wait, runs on a background thread**, matching "never
//!   inside an interactive wait" — but the supervisor's own exit path
//!   joins every still-running background thread before returning,
//!   matching "before reporting `record_verified` or **exiting 0**."
//! - **`reset` while a leg is live** has no dedicated wire refusal reason
//!   (`SupervisorRefusedReason` has none for it) — this module refuses it
//!   through the generic `Failed{detail}` shape rather than inventing a
//!   new wire variant for a single caller.
//! - **A single pending operation is assumed during journal recovery**
//!   for resolving which voyage an `end_run` targeted (`ActiveOp::EndRun`'s
//!   own `voyage` field, read once per operation during the recovery
//!   sweep) — two DIFFERENT unresolved operations racing a crash in the
//!   same window is not specifically hardened beyond what the journal's
//!   own per-id atomicity already provides.

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
use std::sync::mpsc;
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
/// A `hello`'s own `refused {version_skew}` reply is this connection's
/// last word — closing right after `PipeServer::send` returns would race
/// the client seeing EOF before ever reading it (a bare `WriteFile`
/// completing on the server side is not proof the client has DRAINED the
/// pipe's buffer yet). This bounds how long to wait for
/// `TransportEvent::Sent` before force-closing anyway (a send that
/// silently never completes must not leak the connection forever).
const REFUSAL_SENT_DEADLINE: Duration = Duration::from_secs(2);
/// AFTER `Sent` fires (the write has physically completed), an
/// additional grace window before actually closing — giving the client's
/// own `read()` a chance to drain the reply out of the pipe's buffer
/// before this end tears the connection down (Codex review round 1, CI
/// failure (b): a close immediately on `Sent` still occasionally raced
/// the client into `Undetermined` rather than `Foreign`, which is not
/// possible if the client already has the bytes in hand).
const REFUSAL_FLUSH_GRACE: Duration = Duration::from_millis(250);

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

/// Connect to the supervisor lane at `h` and run the FULL same-connection
/// challenge with a CALLER-CHOSEN build string, returning both the
/// still-open connection and the raw outcome — a test's own way to drive
/// the `hello`/`hello_ok`/`refused{version_skew}` exchange directly
/// (never by sending a SECOND `hello` after an already-successful
/// challenge already consumed the connection's one first-frame slot: the
/// lane closes on a second `hello` exactly as it would on any other
/// protocol violation). Gated behind `test-support` like
/// `probe::ScriptedProbeOps` — a REAL client composes this exact sequence
/// itself; this wrapper exists only so `tests/supervisor_win.rs` (a
/// separate integration-test crate, which can only ever reach `pub`
/// items) can exercise it without a second, divergent implementation.
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

/// As [`connect_and_challenge_with_build_for_test`], using this crate's
/// own build id (the correct-build, happy-path case every OTHER test
/// needs) and collapsing the outcome to the connection plus the proof —
/// `Err` for anything short of `Proven`.
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

/// Test-only: send one supervisor-lane request and read exactly one
/// reply within `deadline` — the same [`read_one_frame`] machinery the
/// module's own EndRun path uses, exposed so a test can drive the lane's
/// `status`/`command`/`query` protocol without a second frame-reading
/// implementation.
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

/// A stable hash of the canonicalized state-dir path (ADR 0041 Lifecycle
/// "Name and identity") — scopes the supervisor lane's pipe name and the
/// parent-death lease's own name identically, "the same thing that
/// scopes the pointer and the fence." `pub`: a real client (the launcher,
/// the FE, or `tests/supervisor_win.rs`) needs this to compute the same
/// lane name a supervisor for a given state-dir binds.
pub fn state_dir_hash(state_dir: &Path) -> String {
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

/// Truncate `detail` to fit within [`wire::MAX_SUPERVISOR_STRING_LEN`]
/// bytes, on a UTF-8 boundary. Every `Failed{detail}`/`Failed{detail}`
/// journal or wire record this module constructs goes through this
/// (Codex review round 1, finding 8): `detail` strings are built from
/// arbitrary `Display` output (an OS error, a nested `crate::Error`)
/// with no length guarantee of their own, and the wire's own encoder
/// REFUSES an oversized string rather than truncating it — an earlier
/// version's final `encode_supervisor_reply(&reply).expect(...)` assumed
/// every field was already bounded without enforcing it anywhere, so a
/// long enough error message could panic the whole authority.
fn bounded_detail(detail: impl Into<String>) -> String {
    let mut s = detail.into();
    if s.len() > wire::MAX_SUPERVISOR_STRING_LEN {
        s.truncate(wire::MAX_SUPERVISOR_STRING_LEN);
        while !s.is_char_boundary(s.len()) {
            s.pop();
        }
    }
    s
}

/// A stable hex digest of the WIRE command `operation_id` names (ADR
/// 0041: "the id, a canonical digest of the command"). SHA-256 over
/// [`wire::canonical_supervisor_op_bytes`]'s own canonical byte encoding
/// — never `format!("{op:?}")` (Rust's `Debug` output), which carries no
/// stability guarantee across compiler or dependency versions and would
/// make an id's own digest-conflict check spuriously flip across
/// unrelated toolchain upgrades (Codex review round 1, finding 6).
fn digest_of(op: &SupervisorOp) -> crate::Result<String> {
    use sha2::{Digest as _, Sha256};
    let bytes = wire::canonical_supervisor_op_bytes(op).map_err(|e| err_state(format!("{e}")))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// A fresh `drawer.voyage.reset-<nonce>` evidence filename — used both by
/// a live `reset` command's own admission (journaled BEFORE the rename it
/// names, so recovery can verify it happened) and by the no-supervisor
/// CLI path (which journals nothing, so it mints one for itself).
fn mint_aside_name() -> crate::Result<String> {
    let mut nonce_bytes = [0u8; 8];
    getrandom::fill(&mut nonce_bytes).map_err(std::io::Error::from)?;
    let nonce = u64::from_le_bytes(nonce_bytes);
    Ok(format!("drawer.voyage.reset-{nonce:016x}"))
}

// ---------------------------------------------------------------------
// Pointer discovery / mint (supervisor startup only)
// ---------------------------------------------------------------------

/// Discover the current voyage id, or mint the first-ever one. Every
/// path either yields an id or refuses loudly — there is no "proceed
/// with nothing" case here (see the module's own doc on why this is not
/// an `Option`).
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

/// The start-mode table's OWN "what to do about the latest leg" half (ADR
/// 0041 Lifecycle "Startup authorization is a mode, not an identity"),
/// consulted ONLY when no live capsule was adopted. Returns `true` to
/// spawn a fresh leg, `false` for the row that must not (`--resume`
/// finding the current leg already carries its own end-run marker).
///
/// Checks the marker on an UNSEALED leg too, not only a sealed one
/// (Codex review round 1, finding 3): the marker is written as part of a
/// leg's own graceful-teardown sequence, BEFORE it finishes sealing its
/// segment — a supervisor that crashes during that window and restarts
/// would otherwise see `Unsealed` and respawn a SECOND leg while the
/// first one is still mid-teardown.
fn should_spawn_after_absent(state_dir: &Path, voyage_id: &str, mode: StartMode) -> crate::Result<bool> {
    if mode == StartMode::Start {
        return Ok(true); // "anything -> adopt if live, else spawn"
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
    /// The challenge succeeded and the shutdown request reached
    /// `write_all` successfully, but its ack was never read back (a
    /// write failure past that point, an EOF, or a timeout) — the
    /// shutdown MAY have been delivered and acted on regardless. Never
    /// treated as a plain failure: [`finish_end_run`] reconciles via the
    /// leg's own durable marker before concluding anything (Codex review
    /// round 1, finding 2).
    AckUnknown(ChallengedProcess),
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
            if conn.write_all(&request).is_err() {
                return Ok(EndRunOutcome::AckUnknown(process));
            }
            // The reply's own content carries no new information (the v0
            // mgmt lane has no refusal shape at all — every reply tag
            // means success); reading it is only proof the ack was
            // physically delivered before this connection's own EOF. A
            // failure reading it does NOT mean the shutdown itself
            // failed — the request was already written successfully —
            // so this is `AckUnknown`, reconciled via the leg's own
            // marker, never an immediate `Failed`.
            match read_one_frame(&conn, Instant::now() + Duration::from_secs(5)) {
                Ok(_) => Ok(EndRunOutcome::Ended(process)),
                Err(_) => Ok(EndRunOutcome::AckUnknown(process)),
            }
        }
    }
}

/// Bring an `end_run` to its terminal fact, whether invoked LIVE (a
/// process handle from `end_run_over_mgmt_lane`'s own `Ended`/
/// `AckUnknown` outcome is available to wait on) or during STARTUP
/// RECOVERY (no handle — the prior incarnation that owned it is long
/// gone; only the leg's own durable marker can attest what happened).
/// ADR 0041 (Codex review round 1, finding 2): reconciles via
/// [`verify::leg_carries_run_end_marker`] rather than trusting a
/// post-send error, or the mgmt pipe's mere absence, as proof either way
/// — "never fabricate `record_closed` on pipe-absent."
///
/// A live process handle's WAIT result gates [`journal::mark_closed`]
/// (finding 2's third clause): `mark_closed` is only ever called once the
/// wait has CONFIRMED exit, never merely because a shutdown request was
/// sent. A wait that times out or fails is itself a `Failed` terminal —
/// never silently treated as success.
///
/// `op_id` is `None` for the no-supervisor CLI path (`endrun_inner`),
/// which journals nothing at all — `mark_closed`'s own intermediate
/// milestone exists only for a LATER `query{operation_id}` to observe,
/// and the CLI path has no such caller. Passing a placeholder id there
/// instead would leave permanent, meaningless journal residue under a
/// name no real operation ever owns.
fn finish_end_run(
    state_dir: &Path,
    op_id: Option<&str>,
    voyage_id: &str,
    epoch: Option<u64>,
    process: Option<ChallengedProcess>,
) -> crate::Result<journal::TerminalRecord> {
    if let Some(process) = process {
        match process.wait(SUPPORTED_HISTORY_BOUND + KILL_WAIT_BOUND) {
            Ok(true) => {}
            Ok(false) => {
                return Ok(journal::TerminalRecord::Failed {
                    detail: bounded_detail("the leg's process did not exit within its own teardown bound"),
                });
            }
            Err(e) => {
                return Ok(journal::TerminalRecord::Failed {
                    detail: bounded_detail(format!("waiting for the leg's process to exit: {e}")),
                });
            }
        }
    }
    let seg_dir = voyage_root_path(state_dir, voyage_id).join("seg");
    let epoch = match epoch {
        Some(e) => e,
        // "None is a real, recoverable case ... recovery falls back to
        // the CURRENT voyage's latest leg" (journal.rs's own doc on
        // `ActiveOp::EndRun::epoch`).
        None => match recovery::latest_leg_state(&seg_dir).map_err(crate::Error::Io)? {
            LatestLegState::Sealed { epoch } | LatestLegState::Unsealed { epoch } => epoch,
            LatestLegState::NoLeg => {
                return Ok(journal::TerminalRecord::Failed {
                    detail: bounded_detail("no leg exists for this voyage"),
                });
            }
        },
    };
    let marked = verify::leg_carries_run_end_marker(&seg_dir, voyage_id, epoch)?;
    if !marked {
        return Ok(journal::TerminalRecord::Failed { detail: bounded_detail("record_append") });
    }
    if let Some(op_id) = op_id {
        journal::mark_closed(state_dir, op_id)?;
    }
    let root = voyage_root_path(state_dir, voyage_id);
    Ok(match verify::verify_voyage(&root, voyage_id) {
        Ok(()) => journal::TerminalRecord::RecordVerified,
        Err(e) => journal::TerminalRecord::Failed { detail: bounded_detail(format!("verify_voyage: {e}")) },
    })
}

// ---------------------------------------------------------------------
// Reset
// ---------------------------------------------------------------------

/// Rename the current pointer aside (evidence-preserving, no-replace) if
/// one exists, then mint `new_voyage` fresh. `aside_name` is the exact
/// filename to rename it to: `Some(name)` for the live, journaled path
/// (chosen and recorded AT ADMISSION time, before the rename — Codex
/// review round 1, finding 4, "journal the aside pathname"); `None` for
/// the no-supervisor CLI path, which journals nothing and so mints its
/// own name here. Idempotent enough for recovery's own retry: a
/// `new_voyage` that already has a bootstrapped store is left alone
/// rather than re-bootstrapped, and a call with nothing left to rename
/// aside just proceeds straight to bootstrap+publish.
fn reset_pointer(state_dir: &Path, new_voyage: &str, aside_name: Option<&str>) -> crate::Result<()> {
    let live = pointer::pointer_path(state_dir);
    if live.exists() {
        // Restate the source's own durability before renaming it aside —
        // the pinned publication order's "source flush BEFORE the publish
        // rename" (its CONTENT is already durable from its original
        // publish, since nothing here rewrites it; this is belt-and-
        // braces restating that fact, matching every other caller's own
        // discipline rather than being the one that skips it).
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
// Journal recovery, run FIRST (ADR 0041 Lifecycle "Recovery is part of
// the transaction, and it runs FIRST")
// ---------------------------------------------------------------------

/// Recovers every active-but-unterminated journal entry. Returns `true`
/// iff any RECOVERED `end_run` targeted `current_voyage` — the caller
/// must then start directly in ended-no-respawn service (never spawn a
/// leg for a voyage this authority already told to end in a prior
/// incarnation) rather than exiting immediately (Codex review round 1,
/// finding 3: "never exit-immediately at startup without serving the
/// recovered query result" — a client polling `query` for that same
/// operation id right after the crash-restart must still find a
/// supervisor to ask).
fn reconcile_journal_on_startup(state_dir: &Path, current_voyage: &str) -> crate::Result<bool> {
    let mut ended_current_voyage = false;
    for op_id in journal::active_operations(state_dir)? {
        let Some(active) = journal::read_active(state_dir, &op_id)? else { continue };
        match &active.op {
            journal::ActiveOp::EndRun { voyage, epoch } => {
                reconcile_end_run(state_dir, &op_id, voyage, *epoch)?;
                if voyage == current_voyage {
                    ended_current_voyage = true;
                }
            }
            journal::ActiveOp::Reset { old_voyage, new_voyage, aside } => {
                reconcile_reset(state_dir, &op_id, new_voyage, old_voyage.as_deref(), aside.as_deref())?;
            }
            journal::ActiveOp::Stop => {
                // Reaching this line at all means the process restarted,
                // so the operator already knows the supervisor cycled —
                // finishing it as stopping is harmless bookkeeping, never
                // a second destructive act.
                journal::finish(state_dir, &op_id, &journal::TerminalRecord::Stopping)?;
            }
        }
    }
    Ok(ended_current_voyage)
}

fn reconcile_end_run(state_dir: &Path, op_id: &str, voyage_id: &str, epoch: Option<u64>) -> crate::Result<()> {
    let terminal = finish_end_run(state_dir, Some(op_id), voyage_id, epoch, None)?;
    journal::finish(state_dir, op_id, &terminal)
}

fn reconcile_reset(
    state_dir: &Path,
    op_id: &str,
    new_voyage: &str,
    old_voyage: Option<&str>,
    aside: Option<&str>,
) -> crate::Result<()> {
    match pointer::validate(state_dir) {
        PointerState::Valid(id) if id == new_voyage => {} // already fully done
        PointerState::Valid(id) if Some(id.as_str()) == old_voyage => {
            // The rename never took (or this is the very first attempt):
            // resume from the beginning.
            reset_pointer(state_dir, new_voyage, aside)?;
        }
        PointerState::NotFound => match (old_voyage, aside) {
            (Some(_), Some(aside_name)) => {
                // Verify the evidence rename actually happened before
                // treating the pointer's absence as "safe to resume from
                // publication" (Codex review round 1, finding 4): the
                // pointer being gone, by itself, proves nothing.
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
                // Nothing existed to rename aside in the first place —
                // absence is expected regardless of any aside.
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
    /// The ONE initial placement decision (adopt if live, else consult
    /// the start-mode table) — runs on a background thread so the lane
    /// is serviced for its own up-to-`PROBE_EPISODE` duration.
    InitialProbe { rx: mpsc::Receiver<ProbeOutcome<ChallengedProcess>> },
    /// A fresh owned-spawn attempt in flight on a background thread —
    /// every respawn after a leg ends reaches this, never
    /// `InitialProbe` again.
    Spawning { rx: mpsc::Receiver<ProbeOutcome<ChallengedProcess>> },
    /// A leg is live and proven; `ready_at` anchors the anti-flap
    /// stability window.
    Ready { process: ChallengedProcess, ready_at: Instant },
    /// An `end_run` is in flight on a background thread — the mgmt-lane
    /// exchange, the wait for the leg's own exit, and verification all
    /// happen there; this main loop never blocks on any of it.
    Ending { operation_id: String, rx: mpsc::Receiver<journal::TerminalRecord> },
    /// `end_run` completed (verified), or a leg never started because
    /// this voyage was already told to end (see
    /// `reconcile_journal_on_startup`'s own return value): no leg, never
    /// respawn, serve query/status/stop until told otherwise.
    EndedNoRespawn,
    /// A loud, non-restartable stop: an `end_run` verification failure or
    /// unconfirmed exit, a KILL+WAIT that never confirmed exit, an
    /// identity-mismatched Stage A4 challenge, a permanently dead accept
    /// loop, or a flap-threshold breach. `detail` is operator-facing
    /// diagnostic text (never wire-encoded verbatim — see
    /// `bounded_detail`).
    Terminal { detail: String },
}

impl Lifecycle {
    fn wire_phase(&self) -> SupervisorPhase {
        match self {
            Lifecycle::InitialProbe { .. } | Lifecycle::Spawning { .. } => SupervisorPhase::Starting,
            Lifecycle::Ready { .. } => SupervisorPhase::Ready,
            Lifecycle::Ending { .. } => SupervisorPhase::Ending,
            Lifecycle::EndedNoRespawn => SupervisorPhase::EndedNoRespawn,
            Lifecycle::Terminal { .. } => SupervisorPhase::Terminal,
        }
    }
}

// ---------------------------------------------------------------------
// The supervisor lane's own connection state machine
// ---------------------------------------------------------------------

/// A refusal reply queued as a connection's LAST word waits through TWO
/// stages before the connection actually closes (Codex review round 1,
/// finding 8 / CI failure (b)): first for `TransportEvent::Sent` (the
/// write has physically completed) or a bounded deadline if it never
/// arrives, THEN an additional flush-grace window so the client's own
/// `read()` has a real chance to drain the bytes before this end tears
/// the connection down.
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

/// Everything spawning a fresh leg needs, threaded through the lane's
/// own event handling so a live `reset` — which must respawn IMMEDIATELY
/// for its brand-new, definitely-empty voyage, never wait out a whole
/// `PROBE_EPISODE` adopt-only probe against a store nothing has ever
/// written to — can start one without re-deriving it from
/// `SuperviseConfig` at the call site.
struct SpawnCtx {
    capsule_exe: PathBuf,
    lease_name: String,
    cols: u16,
    rows: u16,
    producer_argv: Vec<String>,
}

/// Everything the lane's own command/query/status handling needs —
/// deliberately separate from the main loop's own `Lifecycle` so the
/// borrow-checker never has to reason about both at once inside one
/// giant function.
struct AuthorityState {
    state_dir: PathBuf,
    voyage_id: String,
    self_pid: u32,
    self_created: u64,
}

/// What `handle_command` decided to do — the CALLER (`handle_lane_bytes`)
/// applies the resulting `Lifecycle` transition inline, immediately
/// after `handle_command` returns and before processing any further
/// frame in the same read (this type carries no `Lifecycle` value
/// directly: `AuthorityState` does not own it) — closing what would
/// otherwise be a real admission race (two `command` frames landing in
/// the SAME read, both seeing the pre-transition `Lifecycle` if the
/// transition were deferred any later).
enum CommandEffect {
    /// No lifecycle transition; `reply` is final.
    Reply(SupervisorOperationState),
    /// Begin ending the current `Ready` leg: `operation_id`/`epoch`/
    /// `reason` are what `spawn_end_run` needs; `reply` (always
    /// `Accepted`) is what goes back on the wire immediately.
    BeginEndRun { operation_id: String, epoch: Option<u64>, reason: String, reply: SupervisorOperationState },
    /// The current voyage pointer was reset; `new_voyage` replaces
    /// `AuthorityState::voyage_id` and the authority must re-enter
    /// `Lifecycle::InitialProbe` for it (a reset only ever admits while
    /// no leg is live, so there is nothing else running to reconcile).
    ResetTo { new_voyage: String, reply: SupervisorOperationState },
    /// `stop` was accepted; the main loop should exit after replying.
    Stop { reply: SupervisorOperationState },
}

impl AuthorityState {
    /// `status` and `query` only — never `command`, which
    /// `handle_lane_bytes` calls directly through [`Self::handle_command`]
    /// so it can apply the resulting [`CommandEffect`]'s `Lifecycle`
    /// transition INLINE, before processing any further frame in the
    /// same read (see that type's own doc for why deferring the
    /// transition any later would be a real admission race).
    fn handle_status_or_query(&mut self, lifecycle: &Lifecycle, req: SupervisorRequest) -> SupervisorReply {
        match req {
            SupervisorRequest::Hello { .. } | SupervisorRequest::Command { .. } => {
                unreachable!("handled by the caller before this is reached")
            }
            SupervisorRequest::Status => SupervisorReply::StatusOk {
                pid: self.self_pid,
                created: self.self_created,
                voyage: Some(self.voyage_id.clone()),
                leg: match lifecycle {
                    Lifecycle::Ready { .. } | Lifecycle::Ending { .. } => leg_epoch_of(&self.state_dir, &self.voyage_id),
                    _ => None,
                },
                phase: lifecycle.wire_phase(),
            },
            SupervisorRequest::Query { operation_id } => {
                SupervisorReply::Operation(self.query_state(&operation_id))
            }
        }
    }

    /// `Err` is a plain reply with no transition (a refusal, a
    /// query-style answer, or a query-time journal error — the latter a
    /// LOUD STOP, not folded into an ordinary reply: see the
    /// `Err(SupervisorOperationState::Failed)` arms below, which the
    /// caller treats identically to any other journal read failure via
    /// [`is_journal_unreadable`]). `Ok` is a [`CommandEffect`] the caller
    /// applies immediately.
    fn handle_command(
        &mut self,
        lifecycle: &Lifecycle,
        operation_id: String,
        op: SupervisorOp,
    ) -> Result<CommandEffect, SupervisorOperationState> {
        // Lifecycle commands are VOYAGE-FENCED (ADR 0041): a mismatch is
        // `refused {stale_voyage}` with NO MUTATION — checked before the
        // journal is ever touched. `Reset{voyage: None}` is legal ONLY
        // when there is truly no live voyage to fence against, which
        // never happens once an authority is running with a voyage id at
        // all (Codex review round 1, finding 4) — so `None` here is
        // refused exactly like a wrong id would be.
        let fenced_ok = match &op {
            SupervisorOp::EndRun { voyage, .. } => *voyage == self.voyage_id,
            SupervisorOp::Reset { voyage: Some(v) } => *v == self.voyage_id,
            SupervisorOp::Reset { voyage: None } => false,
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
            // A query-time journal read failure is a LOUD STOP (Codex
            // review round 1, finding 5), never silently answered as an
            // ordinary Failed-and-continue reply — the caller
            // (`supervise_inner`) matches this exact shape to distinguish
            // "the operation itself failed" from "the journal itself is
            // unreadable."
            Err(e) => return Err(SupervisorOperationState::Failed { detail: bounded_detail(format!("journal unreadable: {e}")) }),
        }

        match op {
            SupervisorOp::EndRun { reason, .. } => {
                let Lifecycle::Ready { .. } = lifecycle else {
                    return Err(SupervisorOperationState::Failed { detail: bounded_detail("no leg is currently running") });
                };
                let epoch = leg_epoch_of(&self.state_dir, &self.voyage_id);
                let record = journal::ActiveRecord {
                    digest,
                    op: journal::ActiveOp::EndRun { voyage: self.voyage_id.clone(), epoch },
                };
                if let Err(e) = journal::begin(&self.state_dir, &operation_id, &record) {
                    return Err(SupervisorOperationState::Failed { detail: bounded_detail(format!("journal begin failed: {e}")) });
                }
                Ok(CommandEffect::BeginEndRun { operation_id, epoch, reason, reply: SupervisorOperationState::Accepted })
            }
            SupervisorOp::Reset { .. } => {
                if matches!(lifecycle, Lifecycle::Ready { .. } | Lifecycle::Ending { .. }) {
                    return Err(SupervisorOperationState::Failed {
                        detail: bounded_detail("a leg is currently live; end the run before resetting"),
                    });
                }
                let new_voyage = uuid::Uuid::now_v7().to_string();
                let old_voyage = Some(self.voyage_id.clone());
                let aside = Some(mint_aside_name().map_err(|e| SupervisorOperationState::Failed {
                    detail: bounded_detail(format!("{e}")),
                })?);
                let record = journal::ActiveRecord {
                    digest,
                    op: journal::ActiveOp::Reset { old_voyage: old_voyage.clone(), new_voyage: new_voyage.clone(), aside: aside.clone() },
                };
                if let Err(e) = journal::begin(&self.state_dir, &operation_id, &record) {
                    return Err(SupervisorOperationState::Failed { detail: bounded_detail(format!("journal begin failed: {e}")) });
                }
                match reset_pointer(&self.state_dir, &new_voyage, aside.as_deref()) {
                    Ok(()) => {
                        self.voyage_id = new_voyage.clone();
                        let t = journal::TerminalRecord::ResetDone { new_voyage: new_voyage.clone() };
                        let _ = journal::finish(&self.state_dir, &operation_id, &t);
                        Ok(CommandEffect::ResetTo { new_voyage, reply: terminal_to_wire(t) })
                    }
                    Err(e) => {
                        // Codex review round 1, finding 4: a half-reset
                        // failure stays ACTIVE (already journaled above)
                        // so a future restart's recovery sweep resumes
                        // it via the SAME idempotent `reset_pointer` —
                        // never written here as a permanent `Failed`,
                        // which would strand it (a `.terminal` file is
                        // never rewritten).
                        eprintln!(
                            "sot-capsule supervise: reset_pointer failed ({e}); operation {operation_id} \
                             remains active for the next restart's recovery sweep to resume"
                        );
                        Ok(CommandEffect::Reply(SupervisorOperationState::Accepted))
                    }
                }
            }
            SupervisorOp::Stop => {
                let t = journal::TerminalRecord::Stopping;
                let _ = journal::finish(&self.state_dir, &operation_id, &t);
                Ok(CommandEffect::Stop { reply: terminal_to_wire(t) })
            }
        }
    }

    fn query_state(&self, operation_id: &str) -> SupervisorOperationState {
        match journal::read_terminal(&self.state_dir, operation_id) {
            Ok(Some(t)) => return terminal_to_wire(t),
            Ok(None) => {}
            // Loud stop, not an ordinary reply (Codex review round 1,
            // finding 5) — `supervise_inner` matches this same shape.
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
/// LOUD STOP (Codex review round 1, finding 5) — `true` iff `reply`'s own
/// detail text names one (`query_state`/`handle_command`'s own
/// "journal unreadable: " prefix, minted nowhere else in this module).
fn is_journal_unreadable(reply: &SupervisorOperationState) -> bool {
    matches!(reply, SupervisorOperationState::Failed { detail } if detail.starts_with("journal unreadable: "))
}

fn encode_reply_or_fallback(reply: &SupervisorReply) -> Vec<u8> {
    wire::encode_supervisor_reply(reply).unwrap_or_else(|e| {
        // Every `detail` field goes through `bounded_detail` before this
        // point, so in practice this never fires — kept as a defensive
        // fallback rather than a panic (Codex review round 1, finding 8)
        // in case a FUTURE field is added here without going through
        // that same discipline.
        eprintln!("sot-capsule supervise: a reply failed to encode ({e}); substituting a minimal failure reply");
        wire::encode_supervisor_reply(&SupervisorReply::Operation(SupervisorOperationState::Failed {
            detail: "internal error".into(),
        }))
        .expect("this minimal fallback reply is always encodable")
    })
}

/// Bundles everything `handle_lane_bytes`/`service_lane` need beyond the
/// transport and per-connection state itself — keeps both functions'
/// own argument counts small regardless of how many pieces of authority
/// state a future finding adds (clippy's own `too_many_arguments`).
struct LaneCtx<'a> {
    authority: &'a mut AuthorityState,
    lifecycle: &'a mut Lifecycle,
    stop_requested: &'a mut bool,
    spawn_ctx: &'a SpawnCtx,
}

/// Services the lane's event queue once. Returns `true` iff the accept
/// loop has died PERMANENTLY (`TransportEvent::AcceptError` — the
/// transport's own doc: "stopped accepting new connections FOR GOOD"),
/// which the caller treats as terminal (Codex review round 1, finding 8:
/// an earlier version only logged this and kept running with a lane no
/// new client could ever reach again).
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
            TransportEvent::Sent(id, _marker) => {
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
                        // Sent, then closed only after BOTH the write
                        // physically completes AND a flush-grace window
                        // passes (see `PendingClose`'s own doc / CI
                        // failure (b)) — never immediately.
                        match lane.send(id, reply, Some(id)) {
                            Ok(()) => pending = Some(PendingClose::AwaitingSent { deadline: now + REFUSAL_SENT_DEADLINE }),
                            Err(_) => close_after = true, // nothing to wait for; the send itself was rejected
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
                    // `hello_ok` already true: a SECOND Hello is a
                    // protocol violation ("Hello MUST be the first frame
                    // of every connection") — close, never fall through
                    // to `handle_request` below, which treats a `Hello`
                    // reaching it as an internal invariant violation
                    // (`unreachable!()`).
                    close_after = true;
                    break;
                }
                DecodedFrame::SupervisorRequest(_) if !conn.hello_ok => {
                    // Hello must be the first frame of every connection.
                    close_after = true;
                    break;
                }
                DecodedFrame::SupervisorRequest(req @ (SupervisorRequest::Status | SupervisorRequest::Query { .. })) => {
                    let reply = ctx.authority.handle_status_or_query(ctx.lifecycle, req);
                    // A query-time journal read failure is a LOUD STOP
                    // for the WHOLE authority (Codex review round 1,
                    // finding 5), never merely one client's own Failed
                    // reply while everything else carries on as if
                    // nothing happened.
                    if let SupervisorReply::Operation(state) = &reply {
                        if is_journal_unreadable(state) {
                            *ctx.lifecycle = Lifecycle::Terminal { detail: "the operation journal became unreadable".into() };
                        }
                    }
                    let bytes = encode_reply_or_fallback(&reply);
                    let _ = lane.send(id, bytes, None);
                }
                DecodedFrame::SupervisorRequest(SupervisorRequest::Command { operation_id, op }) => {
                    let state = match ctx.authority.handle_command(ctx.lifecycle, operation_id, op) {
                        Ok(CommandEffect::Reply(r)) => r,
                        Ok(CommandEffect::BeginEndRun { operation_id, epoch, reason, reply }) => {
                            let rx = spawn_end_run(
                                ctx.authority.state_dir.clone(),
                                operation_id.clone(),
                                ctx.authority.voyage_id.clone(),
                                epoch,
                                reason,
                            );
                            *ctx.lifecycle = Lifecycle::Ending { operation_id, rx };
                            reply
                        }
                        Ok(CommandEffect::ResetTo { new_voyage, reply }) => {
                            // Spawn IMMEDIATELY for the new voyage, never
                            // an adopt-only probe first: `reset` only
                            // ever admits while no leg is live, and the
                            // freshly-minted voyage is definitely empty
                            // (nothing could possibly answer an adopt
                            // probe against it) — waiting out a whole
                            // `PROBE_EPISODE` first would be a pure,
                            // pointless delay.
                            let voyage_root = voyage_root_path(&ctx.authority.state_dir, &new_voyage);
                            let rx = spawn_owned_spawn_attempt(
                                ctx.spawn_ctx.capsule_exe.clone(),
                                voyage_root,
                                new_voyage,
                                ctx.spawn_ctx.cols,
                                ctx.spawn_ctx.rows,
                                ctx.spawn_ctx.lease_name.clone(),
                                ctx.spawn_ctx.producer_argv.clone(),
                            );
                            *ctx.lifecycle = Lifecycle::Spawning { rx };
                            reply
                        }
                        Ok(CommandEffect::Stop { reply }) => {
                            *ctx.stop_requested = true;
                            reply
                        }
                        Err(state) => state,
                    };
                    // See the `Status`/`Query` arm above: a journal read
                    // failure surfacing here is a loud stop for the whole
                    // authority (Codex review round 1, finding 5), not
                    // merely this one client's own Failed reply.
                    if is_journal_unreadable(&state) {
                        *ctx.lifecycle = Lifecycle::Terminal { detail: "the operation journal became unreadable".into() };
                    }
                    let reply = SupervisorReply::Operation(state);
                    let bytes = encode_reply_or_fallback(&reply);
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

/// Spawn a background thread running `probe_owned_spawn` (a fresh spawn
/// attempt) — the whole point of [`Lifecycle::Spawning`]: the lane is
/// serviced by the caller's own main loop WHILE this runs, never frozen
/// behind it (Codex review round 1, finding 1).
fn spawn_owned_spawn_attempt(
    capsule_exe: PathBuf,
    voyage_root: PathBuf,
    voyage_id: String,
    cols: u16,
    rows: u16,
    lease_name: String,
    producer_argv: Vec<String>,
) -> mpsc::Receiver<ProbeOutcome<ChallengedProcess>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
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
    rx
}

/// Spawn a background thread running the ONE initial adopt-only probe.
fn spawn_initial_probe(voyage_id: String, voyage_root: PathBuf) -> mpsc::Receiver<ProbeOutcome<ChallengedProcess>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let episode_deadline = Instant::now() + PROBE_EPISODE;
        let outcome =
            classify::probe_adopt_only(&RealProbeOps, &voyage_id, &voyage_root, episode_deadline, ATTEMPT_INTERVAL);
        let _ = tx.send(outcome);
    });
    rx
}

/// Spawn a background thread carrying an `end_run` to its terminal fact
/// — the mgmt-lane exchange, the wait for the leg's own exit, and
/// verification all happen here, off the main loop (Codex review round
/// 1, finding 1). Writes the journal's own terminal record itself before
/// reporting back, since this thread is this operation id's sole owner
/// for its whole life.
fn spawn_end_run(
    state_dir: PathBuf,
    operation_id: String,
    voyage_id: String,
    epoch: Option<u64>,
    reason: String,
) -> mpsc::Receiver<journal::TerminalRecord> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let terminal = match end_run_over_mgmt_lane(&voyage_id, &reason) {
            Ok(EndRunOutcome::Absent) => finish_end_run(&state_dir, Some(&operation_id), &voyage_id, epoch, None),
            Ok(EndRunOutcome::AckUnknown(process) | EndRunOutcome::Ended(process)) => {
                finish_end_run(&state_dir, Some(&operation_id), &voyage_id, epoch, Some(process))
            }
            Ok(EndRunOutcome::Foreign | EndRunOutcome::Pending) => Ok(journal::TerminalRecord::Failed {
                detail: bounded_detail("the capsule's mgmt lane is foreign or unresponsive"),
            }),
            Err(e) => Ok(journal::TerminalRecord::Failed { detail: bounded_detail(format!("{e}")) }),
        };
        let terminal = terminal.unwrap_or_else(|e| journal::TerminalRecord::Failed { detail: bounded_detail(format!("{e}")) });
        let terminal = match journal::finish(&state_dir, &operation_id, &terminal) {
            Ok(()) => terminal,
            Err(e) => journal::TerminalRecord::Failed { detail: bounded_detail(format!("journal finish failed: {e}")) },
        };
        let _ = tx.send(terminal);
    });
    rx
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

    let voyage_id = discover_or_mint_voyage(&config.state_dir, config.mode)?;

    // Recovery FIRST — before start-mode authorization, before admitting
    // any new command (ADR 0041 Lifecycle "Recovery is part of the
    // transaction, and it runs FIRST").
    let ended_current_voyage = reconcile_journal_on_startup(&config.state_dir, &voyage_id)?;

    let self_ids = self_pid_and_created().unwrap_or((0, 0));
    let mut authority = AuthorityState {
        state_dir: config.state_dir.clone(),
        voyage_id: voyage_id.clone(),
        self_pid: self_ids.0,
        self_created: self_ids.1,
    };
    let mut conns: HashMap<ConnId, Conn> = HashMap::new();
    let capsule_exe = std::env::current_exe().map_err(crate::Error::Io)?;
    let voyage_root = voyage_root_path(&config.state_dir, &voyage_id);
    let spawn_ctx = SpawnCtx {
        capsule_exe: capsule_exe.clone(),
        lease_name: lease_name.clone(),
        cols: config.cols,
        rows: config.rows,
        producer_argv: config.producer_argv.clone(),
    };

    // The FIRST placement decision (Codex review round 1, finding 3: a
    // voyage this authority already recovered as ENDED must go straight
    // to ended-no-respawn SERVICE, never an immediate exit that would
    // strand a client still polling `query` for the operation that ended
    // it).
    let mut lifecycle = if ended_current_voyage {
        Lifecycle::EndedNoRespawn
    } else {
        Lifecycle::InitialProbe { rx: spawn_initial_probe(voyage_id.clone(), voyage_root.clone()) }
    };

    let mut consecutive_unstable_legs: u32 = 0;
    let mut stop_requested = false;

    'authority: loop {
        let now = Instant::now();
        let mut lane_ctx = LaneCtx {
            authority: &mut authority,
            lifecycle: &mut lifecycle,
            stop_requested: &mut stop_requested,
            spawn_ctx: &spawn_ctx,
        };
        if service_lane(&lane, &mut conns, &mut lane_ctx, now) {
            lifecycle = Lifecycle::Terminal { detail: "supervisor lane accept loop failed permanently".into() };
            break 'authority;
        }
        if stop_requested {
            break 'authority;
        }

        match &mut lifecycle {
            Lifecycle::InitialProbe { rx } => match rx.try_recv() {
                Ok(ProbeOutcome::Adopted(process)) => {
                    lifecycle = Lifecycle::Ready { process, ready_at: Instant::now() };
                }
                Ok(ProbeOutcome::Absent) => {
                    lifecycle = if should_spawn_after_absent(&config.state_dir, &voyage_id, config.mode)? {
                        Lifecycle::Spawning {
                            rx: spawn_owned_spawn_attempt(
                                capsule_exe.clone(),
                                voyage_root.clone(),
                                voyage_id.clone(),
                                config.cols,
                                config.rows,
                                lease_name.clone(),
                                config.producer_argv.clone(),
                            ),
                        }
                    } else {
                        Lifecycle::EndedNoRespawn
                    };
                }
                Ok(ProbeOutcome::Foreign | ProbeOutcome::Wedged) => {
                    lifecycle = Lifecycle::Terminal { detail: "the voyage pipe is foreign or unreachable at startup".into() };
                }
                Ok(other) => return Err(err_state(format!("unexpected probe_adopt_only outcome at startup: {other:?}"))),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    lifecycle = Lifecycle::Terminal { detail: "the initial probe thread ended without a result".into() };
                }
            },
            Lifecycle::Spawning { rx } => match rx.try_recv() {
                Ok(ProbeOutcome::Ready(process)) => {
                    consecutive_unstable_legs = 0;
                    lifecycle = Lifecycle::Ready { process, ready_at: Instant::now() };
                }
                Ok(ProbeOutcome::SpawnFailed(e)) => {
                    eprintln!("sot-capsule supervise: spawn failed: {e}");
                    consecutive_unstable_legs += 1;
                    // Read the CURRENT voyage off `authority`, never the
                    // pre-loop `voyage_id`/`voyage_root` locals: a live
                    // `reset` mutates `authority.voyage_id` (see
                    // `handle_command`'s own Reset arm) and this respawn
                    // may be for a leg that outlived one.
                    let current_root = voyage_root_path(&authority.state_dir, &authority.voyage_id);
                    lifecycle = respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &current_root, &authority.voyage_id, &config, &lease_name);
                }
                Ok(ProbeOutcome::KilledAfterTimeout | ProbeOutcome::LegEnded | ProbeOutcome::Foreign) => {
                    consecutive_unstable_legs += 1;
                    // Read the CURRENT voyage off `authority`, never the
                    // pre-loop `voyage_id`/`voyage_root` locals: a live
                    // `reset` mutates `authority.voyage_id` (see
                    // `handle_command`'s own Reset arm) and this respawn
                    // may be for a leg that outlived one.
                    let current_root = voyage_root_path(&authority.state_dir, &authority.voyage_id);
                    lifecycle = respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &current_root, &authority.voyage_id, &config, &lease_name);
                }
                Ok(ProbeOutcome::KillOrWaitFailed(e)) => {
                    lifecycle = Lifecycle::Terminal { detail: bounded_detail(format!("kill/wait failed: {e}")) };
                }
                Ok(other) => return Err(err_state(format!("unexpected probe_owned_spawn outcome: {other:?}"))),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    lifecycle = Lifecycle::Terminal { detail: "the spawn thread ended without a result".into() };
                }
            },
            Lifecycle::Ready { process, ready_at } => {
                let ready_at = *ready_at;
                match process.wait(Duration::ZERO) {
                    Ok(true) => {
                        // The leg ended on its own (never via `end_run`,
                        // which transitions through `Ending` instead —
                        // reaching `Ready`'s own exit path at all means
                        // this was NOT a requested end, so it always
                        // counts toward the anti-flap bound: only a leg
                        // that outlived the stability interval resets it.
                        if ready_at.elapsed() < STABILITY_INTERVAL {
                            consecutive_unstable_legs += 1;
                        } else {
                            consecutive_unstable_legs = 0;
                        }
                        // Read the CURRENT voyage off `authority`, never the
                    // pre-loop `voyage_id`/`voyage_root` locals: a live
                    // `reset` mutates `authority.voyage_id` (see
                    // `handle_command`'s own Reset arm) and this respawn
                    // may be for a leg that outlived one.
                    let current_root = voyage_root_path(&authority.state_dir, &authority.voyage_id);
                    lifecycle = respawn_or_terminal(&mut consecutive_unstable_legs, &capsule_exe, &current_root, &authority.voyage_id, &config, &lease_name);
                    }
                    Ok(false) => {} // still running
                    Err(e) => {
                        lifecycle = Lifecycle::Terminal { detail: bounded_detail(format!("wait on the leg's process handle failed: {e}")) };
                    }
                }
            }
            Lifecycle::Ending { operation_id, rx } => match rx.try_recv() {
                Ok(journal::TerminalRecord::RecordVerified) => {
                    lifecycle = Lifecycle::EndedNoRespawn;
                }
                Ok(journal::TerminalRecord::Failed { detail }) => {
                    // Codex review round 1, finding 2: a verification
                    // failure (or an unconfirmed exit, or a foreign/
                    // unresponsive mgmt lane) is a loud, non-restartable
                    // stop — never silently folded back into ordinary
                    // service.
                    lifecycle = Lifecycle::Terminal { detail: format!("end_run {operation_id}: {detail}") };
                }
                Ok(other) => return Err(err_state(format!("unexpected end_run terminal record: {other:?}"))),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    lifecycle = Lifecycle::Terminal { detail: format!("the end_run thread for {operation_id} ended without a result") };
                }
            },
            Lifecycle::EndedNoRespawn | Lifecycle::Terminal { .. } => {
                // Keep serving query/status/stop until an explicit stop
                // or this process is killed — never exit on our own.
            }
        }

        std::thread::sleep(MAIN_LOOP_POLL);
    }

    // The main loop above only ever breaks on an explicit `stop` or a
    // dead accept loop; a `stop` received while `Terminal` still exits
    // non-zero (Codex review round 1, finding 2's fourth clause: "later
    // `stop` must not exit 0 from TERMINAL").
    let exit_code = if let Lifecycle::Terminal { detail } = &lifecycle {
        eprintln!("sot-capsule supervise: exiting terminal: {detail}");
        EXIT_TERMINAL
    } else {
        EXIT_CLEAN
    };

    // `lane`'s own `Drop` performs the teardown (`disconnect_listener`
    // then `join_workers` under `TEARDOWN_AGGREGATE_DEADLINE`) when it
    // goes out of scope below — no separate call needed. Any in-flight
    // background thread (`Spawning`/`Ending`/`InitialProbe`) is
    // deliberately NOT joined here: `EXIT_CLEAN`/`EXIT_TERMINAL` are only
    // ever reached from `EndedNoRespawn`/`Terminal`, neither of which has
    // one in flight.
    Ok(exit_code)
}

/// Shared "a leg just ended and no `end_run` was in progress" tail:
/// count against the flap bound, then either stay Terminal (flap
/// threshold breached) or start a fresh `Spawning` attempt.
fn respawn_or_terminal(
    consecutive_unstable_legs: &mut u32,
    capsule_exe: &Path,
    voyage_root: &Path,
    voyage_id: &str,
    config: &SuperviseConfig,
    lease_name: &str,
) -> Lifecycle {
    if *consecutive_unstable_legs >= FLAP_THRESHOLD {
        return Lifecycle::Terminal { detail: "the anti-flap bound was reached".into() };
    }
    Lifecycle::Spawning {
        rx: spawn_owned_spawn_attempt(
            capsule_exe.to_path_buf(),
            voyage_root.to_path_buf(),
            voyage_id.to_string(),
            config.cols,
            config.rows,
            lease_name.to_string(),
            config.producer_argv.clone(),
        ),
    }
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
            // `None`: this no-supervisor CLI path journals nothing at
            // all — there is no `query{operation_id}` caller for
            // `mark_closed`'s own intermediate milestone to ever serve.
            match finish_end_run(state_dir, None, &voyage_id, epoch, Some(process))? {
                journal::TerminalRecord::RecordVerified => {
                    eprintln!("sot-capsule endrun: record_verified");
                    Ok(EXIT_CLEAN)
                }
                journal::TerminalRecord::Failed { detail } => {
                    eprintln!("sot-capsule endrun: {detail}");
                    Ok(EXIT_TERMINAL)
                }
                other => Err(err_state(format!("unexpected end_run terminal record: {other:?}"))),
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
    // Codex review round 1, finding 4: an explicitly-given `--voyage`
    // must be compared against the CURRENT pointer before acting, never
    // just trusted — a caller supplying a stale or wrong id could
    // otherwise probe the wrong voyage's liveness and reset the actual
    // current one out from under a live process.
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

        // Row 2: pointer ABSENT with the evidence rename present --
        // resume from publication.
        std::fs::remove_file(pointer::pointer_path(dir.path())).unwrap();
        let third = uuid::Uuid::now_v7().to_string();
        let aside2 = "drawer.voyage.reset-0000000000000001".to_string();
        reconcile_reset(dir.path(), "op-4", &third, Some(&new_voyage), Some(&aside2)).unwrap();
        assert!(matches!(pointer::validate(dir.path()), PointerState::Valid(v) if v == third));
    }

    /// Codex review round 1, finding 4: the pointer's mere absence is
    /// NOT proof the rename-aside step completed — the recorded evidence
    /// file must actually exist.
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

    #[test]
    fn is_journal_unreadable_matches_only_that_shape() {
        assert!(is_journal_unreadable(&SupervisorOperationState::Failed { detail: "journal unreadable: boom".into() }));
        assert!(!is_journal_unreadable(&SupervisorOperationState::Failed { detail: "record_append".into() }));
        assert!(!is_journal_unreadable(&SupervisorOperationState::Accepted));
    }
}
