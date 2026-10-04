//! Activation: the one boundary that starts, resumes or (for a selection) retires and resets a capsule row's run.

use super::observer::observe_with_adoption;
use super::probe::probe;
use super::start::{reset_run, start_supervisor};
use super::{FOREIGN_PHASE, NEVER_STARTED_PHASE, UNREACHABLE_PHASE};
use crate::agents::argv::agent_argv;
use crate::rows::spawn::detach::StartMode;
use crate::workspaces::Workspaces;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationIntent {
    /// May resume, and may retire+reset an ended run.
    Selection,
    /// May resume, but never resets an ended run.
    Reconnect,
}

/// Whether a capsule workspace's supervisor needs starting given an
/// ALREADY-PROBED `phase` — no I/O itself (switch-latency Phase 1:
/// the healthy already-running path used to call [`phase_of`] a
/// SECOND time here; reusing an already-fetched phase string costs
/// nothing beyond what an already-answered lane already told the
/// caller). [`NEVER_STARTED_PHASE`] (no published pointer) means
/// `Start`; [`UNREACHABLE_PHASE`] means `Resume`; any answered
/// lifecycle phase means a supervisor is already up — `None`.
fn start_mode_for_phase(phase: &str) -> Option<StartMode> {
    match phase {
        NEVER_STARTED_PHASE => Some(StartMode::Start),
        UNREACHABLE_PHASE => Some(StartMode::Resume),
        _ => None,
    }
}

/// Invariant (Codex review round 6/7 BLOCKERs): EVERY decision point
/// -- the initial probe AND the phase a spawn THIS call itself just
/// made settles at -- acts ONLY on a genuinely RESTING phase:
/// `ready`, `ended_no_respawn`, `terminal`, never-started
/// (`stopped`), `foreign` (a peer identity check already resolved
/// it, same as before this lane), or `unreachable` with NO live
/// watchdog (nothing else will ever act on it, so this caller must).
/// Every OTHER phase — `starting`, `ending`, or `unreachable` while a
/// watchdog owns it — is TRANSIENT: the row is mid-flight to
/// somewhere else, and deciding from a snapshot of it is exactly the
/// bug this closes (round 6: a Selection reading a fleeting
/// `starting` after the watchdog's own settle gave up; round 7: the
/// SAME snapshot read straight from this call's OWN spawn, with no
/// watchdog involved at all — an adopted row's dead supervisor,
/// `--resume`d fresh, can report `starting` through its own recovery
/// for many seconds). `ensure_started`'s own loop is what waits out
/// a transient phase; this function only tells the two apart.
fn is_resting_phase(phase: &str, watchdog_owns_it: bool) -> bool {
    phase == NEVER_STARTED_PHASE
        || phase == FOREIGN_PHASE
        || phase == UNREACHABLE_PHASE && !watchdog_owns_it
        || phase == super::phase_str(sot_log::wire::SupervisorPhase::Ready)
        || phase == super::phase_str(sot_log::wire::SupervisorPhase::EndedNoRespawn)
        || phase == super::phase_str(sot_log::wire::SupervisorPhase::Terminal)
}

/// The guard-HELD body shared by [`resume_if_absent`] (which takes
/// the row's guard itself, around this whole call), [`ensure_started`]
/// (which already holds it for its own whole call, on the
/// `UNREACHABLE_PHASE` arm), and `handlers.rs`'s
/// `destroy_capsule_workspace` (which holds the SAME row guard
/// across this call and its own following `end_run`, so it must
/// reach this guard-free inner directly rather than through
/// [`resume_if_absent`] — a second `blocking_lock` on a guard this
/// caller already holds would deadlock) — ADR 0043 decision 33.
/// `pub` for that cross-module reach; still crate-internal in effect
/// (`mod runtime` itself is private, re-exported only within this
/// crate via `capsule_workspace`'s own `pub use runtime::*`).
/// Rechecks, now that the guard is actually held: the row is still
/// registered (`Err` — "unknown workspace" — a concurrent remover
/// could have removed it while this call waited for the lock); if
/// the watchdog already observed it `Phase::Terminal` (latched), reports that phase without touching the lane
/// again. Otherwise probes once: any phase OTHER than
/// [`UNREACHABLE_PHASE`] is returned as-is — nothing to resume (in
/// particular, a missing state dir reads `NEVER_STARTED_PHASE` here
/// and this returns WITHOUT ever spawning — never a licence to
/// recreate one). Only a genuinely unreachable lane spawns, and only
/// with `StartMode::Resume` — this is the resume path, never the
/// create one — via [`start_supervisor`], which settles before
/// returning (`Ok`) or reports why it could not spawn at all
/// (`Err`). Never sends `reset`: an `EndedNoRespawn` settle is
/// reported as-is here — retiring it is attach's own job (R3), not
/// resume's.
pub fn resume_locked(
    state_root: &Path,
    workspace_id: &str,
    agent_kind: &str,
    agent_name: &str,
    slug: &str,
    project_root: &Path,
    workspaces: Workspaces,
) -> Result<&'static str, String> {
    let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
        return Err("unknown workspace".to_string());
    };
    if ws.phase() == crate::workspaces::Phase::Terminal {
        return Ok(super::phase_str(sot_log::wire::SupervisorPhase::Terminal));
    }
    let state_dir = super::state_dir_for(state_root, workspace_id);
    let (phase, observation) = probe(&state_dir);
    if phase != UNREACHABLE_PHASE {
        // Already answering -- no spawn, but still an adoption if this row had none yet.
        observe_with_adoption(&ws, observation);
        return Ok(phase);
    }
    // Ruling: the watchdog is the single writer of restarts for a
    // child this daemon spawned. A row whose watchdog is still alive
    // reports its own (unreachable) phase as-is -- the caller waits
    // as it would for Starting -- rather than racing a second spawn
    // in front of the watchdog's own backoff and restart budget. A
    // row with no watchdog (never started, terminal, or a live
    // authority merely ADOPTED at boot) reaches the spawn below
    // unchanged.
    if ws.watchdog_owner().is_some() {
        return Ok(phase);
    }
    let argv = agent_argv(agent_kind, Some(project_root))?;
    start_supervisor(state_root, workspace_id, StartMode::Resume, &argv, project_root, agent_name, slug, workspaces)
}

/// R2's own resume path (ADR 0043 decision 33): a row whose lane has
/// gone quiet is resumed, in place, by the operation that needed it
/// live — never by a list. BLOCKING (`phase_of`, `query_status`, and
/// — when it actually resumes — a process spawn are all real I/O):
/// callers run it via `spawn_blocking`. Takes this workspace's OWN
/// guard (`Workspaces::capsule_guard`) for its whole duration —
/// `Err("unknown workspace")` if the row is not currently registered
/// (that call itself refuses to mint an orphan guard entry, Codex
/// review 2026-09-11 — no lock to even take); [`resume_locked`]'s own
/// doc has what the SECOND recheck, once the lock is actually held,
/// covers. Headless callers (`handlers.rs`'s `pty.input`/
/// `pty.screen`) use this in place of a bare [`phase_of`] read so a
/// row whose supervisor died between two ops resumes itself rather
/// than answering `NotReady` forever; `pty.open`'s own attach path
/// keeps using [`ensure_started`] instead — it needs the `Start` arm
/// this function deliberately does not have (resume-only intent: it
/// never starts a row that has no pointer published at all, and it
/// never sends `reset`).
pub fn resume_if_absent(
    state_root: &Path,
    workspace_id: &str,
    agent_kind: &str,
    agent_name: &str,
    slug: &str,
    project_root: &Path,
    workspaces: Workspaces,
) -> Result<&'static str, String> {
    let Some(guard) = workspaces.capsule_guard(workspace_id) else {
        return Err("unknown workspace".to_string());
    };
    let _held = guard.blocking_lock();
    resume_locked(state_root, workspace_id, agent_kind, agent_name, slug, project_root, workspaces)
}

/// Bounds how many re-probe PASSES [`ensure_started`] spends waiting
/// for a transient phase to settle (a watchdog mid-restart; or
/// simply a still-starting/still-recovering authority, watchdog or
/// not) before falling back to reporting the row's phase unacted-on,
/// exactly as it did before that wait existed. A pass count, not
/// wall-clock time (round 9: a wall-clock deadline, even one started
/// at the first `WaitForSettle`, still ticks down while this call
/// merely waits to ACQUIRE the guard -- behind a watchdog's own
/// 7/15/30s backoff, say -- time that was never this budget's to
/// spend either). NOT a tight bound in wall-clock terms: a pass that
/// re-enters the retire arm pays a `stop` plus a fresh
/// `SPAWN_SETTLE_DEADLINE` (2s) each time, so 50 passes can hold the
/// row's guard for minutes on a row whose recovery keeps outlasting
/// that deadline -- accepted rather than a second counter, since the
/// fallback is always the honest "still unacted-on" report this
/// function already gives, never a wrong decision.
const ACTIVATION_MAX_REPROBES: u32 = 50;

/// How often [`ensure_started`] re-probes a transient phase while
/// waiting. No progress signal to race against it (Codex review
/// round 8: a `Notify` here saved at most one interval's worth of
/// latency, never correctness, and cost a real subscription-ordering
/// hazard to close properly -- deleted).
const ACTIVATION_REPROBE_INTERVAL: Duration = Duration::from_millis(200);

/// The ONE shared activation boundary for every caller: guard, inert-anchor refusal, then start/resume/(Selection-only) retire+reset. BLOCKING.
pub fn ensure_started(
    state_root: &Path,
    workspace_id: &str,
    agent_kind: &str,
    agent_name: &str,
    slug: &str,
    project_root: &Path,
    intent: ActivationIntent,
    workspaces: Workspaces,
) -> Result<Option<()>, String> {
    let Some(guard) = workspaces.capsule_guard(workspace_id) else {
        return Err("unknown workspace".to_string());
    };
    let mut reprobes: u32 = 0;
    // Round-9 BLOCKER: the identity of the fresh authority THIS
    // activation itself spawned to retire an ended row, carried
    // across passes -- see `ensure_started_locked`'s own doc for why.
    let mut own_spawn: Option<crate::workspaces::SupervisorIdentity> = None;
    loop {
        let held = guard.blocking_lock();
        // Rechecked under the guard -- a concurrent remover could have removed the row while this call waited.
        let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
            return Err("unknown workspace".to_string());
        };
        // R1: the ONE place this check lives, under the guard against the CURRENT row.
        if workspaces.is_inert_default_anchor(&ws) {
            return Ok(None);
        }
        // Clear-on-attempt happens HERE inside the guard so two serialized attempts can't interleave.
        ws.set_activation_error(None);
        match ensure_started_locked(
            state_root, workspace_id, agent_kind, agent_name, slug, project_root, intent, workspaces.clone(),
            &mut own_spawn,
        ) {
            LockedStep::Done(result) => {
                if let Err(detail) = &result {
                    ws.set_activation_error(Some(detail.clone()));
                }
                return result;
            }
            // Ruling: never drop the caller's intent, and never
            // decide from a transient phase (see `is_resting_phase`).
            // Release the guard (a watchdog, or the authority's own
            // recovery, needs it free to make ANY progress) and
            // sleep one re-probe interval, then re-acquire and
            // re-run this WHOLE decision from scratch with the SAME
            // original intent -- so a Selection on a run that comes
            // back EndedNoRespawn still retires and resets, never
            // silently "succeeds" on a stale snapshot mid-transition.
            LockedStep::WaitForSettle => {
                // Test-only: one marker per reprobe cycle, so a test
                // can wait for (or count) this loop's own progress
                // instead of guessing a sleep duration. No-op unless
                // `SOT_TEST_ACTIVATION_BARRIER` is set.
                crate::server::record_test_activation_marker("waitforsettle");
                reprobes += 1;
                if reprobes > ACTIVATION_MAX_REPROBES {
                    // The budget is spent: report the phase as the
                    // pre-ruling code always did for this exact
                    // situation -- attempted, still unsettled, spawn
                    // deferred to whatever is already in flight.
                    // Pre-existing, filed for later (round 10 record):
                    // this `Ok(Some(()))` is indistinguishable on the
                    // wire from a genuine success, so `pty.open`
                    // answers `attach_direct` onto a row that may
                    // still be `ended_no_respawn`, with no
                    // `activation_error` set -- exhaustion itself is
                    // not surfaced as a caller-visible failure.
                    return Ok(Some(()));
                }
                drop(held);
                // BLOCKING (this whole function is): plain sleep, no
                // tokio runtime handle needed, same as
                // `settle_after_spawn`'s own wait.
                std::thread::sleep(ACTIVATION_REPROBE_INTERVAL);
            }
        }
    }
}

/// What one guard-held attempt at [`ensure_started_locked`] concluded.
enum LockedStep {
    /// Genuinely resolved -- nothing further to do differently.
    Done(Result<Option<()>, String>),
    /// The row's phase is transient, not a resting point a decision
    /// may be made from -- see [`is_resting_phase`]'s own doc for
    /// the invariant, and [`ensure_started`]'s own loop for the wait.
    WaitForSettle,
}

/// [`ensure_started`]'s guard-held body, after membership/inert-anchor/activation-error clear.
///
/// `own_spawn` is [`ensure_started`]'s own local, carried across
/// `WaitForSettle` passes (round-9 BLOCKER): the identity of the
/// fresh authority a PRIOR pass of THIS SAME activation spawned to
/// retire an ended row. Without it, a transient `retired_phase`
/// below returns `WaitForSettle`, the next pass re-runs this whole
/// function from scratch, sees `ended_no_respawn` again, and (with
/// no memory of the spawn it just did) retires AGAIN -- stopping the
/// authority this same activation only just spawned and spawning
/// another. A row whose `--resume` recovery takes longer than
/// `SPAWN_SETTLE_DEADLINE` on every attempt then cycles stop, spawn,
/// settle, wait, stop forever: no `reset` is ever reached, so the
/// row can never start a new run. Recognizing "the current resident
/// IS the fresh binary I already spawned" breaks that cycle: reset
/// it directly, no second stop, no second spawn.
fn ensure_started_locked(
    state_root: &Path,
    workspace_id: &str,
    agent_kind: &str,
    agent_name: &str,
    slug: &str,
    project_root: &Path,
    intent: ActivationIntent,
    workspaces: Workspaces,
    own_spawn: &mut Option<crate::workspaces::SupervisorIdentity>,
) -> LockedStep {
    let Some(ws) = workspaces.resolve(Some(workspace_id)) else {
        return LockedStep::Done(Err("unknown workspace".to_string()));
    };
    let state_dir = super::state_dir_for(state_root, workspace_id);
    let (initial_phase, initial_observation) = probe(&state_dir);
    observe_with_adoption(&ws, initial_observation);
    // Ruling: never drop the caller's intent, and never decide from
    // a transient snapshot -- see `is_resting_phase`'s own doc for
    // the invariant, and `ensure_started`'s own loop for what
    // happens on `WaitForSettle` (this checks BEFORE computing
    // `mode`: `start_mode_for_phase` already maps a transient
    // `starting` read to `None`, "nothing to do", which is exactly
    // the stale-snapshot bug this closes).
    if !is_resting_phase(initial_phase, ws.watchdog_owner().is_some()) {
        return LockedStep::WaitForSettle;
    }
    // R4c: Reconnect permits only StartMode::Resume -- never a row's first-ever start.
    let mode = start_mode_for_phase(initial_phase);
    let mode = if intent == ActivationIntent::Reconnect && mode != Some(StartMode::Resume) { None } else { mode };
    let (spawned, settled_phase) = match mode {
        Some(StartMode::Start) => {
            let argv = match agent_argv(agent_kind, Some(project_root)) {
                Ok(a) => a,
                Err(e) => return LockedStep::Done(Err(e)),
            };
            let phase = match start_supervisor(
                state_root, workspace_id, StartMode::Start, &argv, project_root, agent_name, slug, workspaces.clone(),
            ) {
                Ok(p) => p,
                Err(e) => return LockedStep::Done(Err(e)),
            };
            (Some(()), phase)
        }
        Some(StartMode::Resume) => {
            let phase = match resume_locked(
                state_root, workspace_id, agent_kind, agent_name, slug, project_root, workspaces.clone(),
            ) {
                Ok(p) => p,
                Err(e) => return LockedStep::Done(Err(e)),
            };
            (Some(()), phase)
        }
        None => (None, initial_phase),
    };
    // Ruling (round 7 BLOCKER): the phase a spawn THIS call itself
    // just made settles at is EXACTLY as liable to be transient as
    // the initial probe was -- a watchdog now owns the fresh child
    // either way (`Start`/`Resume` both install one via
    // `spawn_and_watch`), so only "resting or not" is left to ask.
    // `mode == None` means `settled_phase == initial_phase`, already
    // proven resting above -- asking again is cheap and uniform.
    // Round 10: NOT gated to Selection -- a round-9 attempt to skip
    // this wait for Reconnect broke a slow `--resume` recovery:
    // `settle_after_spawn` reads `unreachable` (the fresh child not
    // listening yet) past its own 2s deadline just as readily as
    // `starting`, so an ungated Reconnect returned `Ok` at once onto
    // a row with nothing actually resumed yet, and the bridge's next
    // connect failed `lane_absent` instead of converging -- the exact
    // scenario case 4 below exercises. Both intents wait.
    if !is_resting_phase(settled_phase, true) {
        return LockedStep::WaitForSettle;
    }
    // One answered phase still has no live leg to attach to:
    // `EndedNoRespawn` (`--resume`/`--start` deliberately never
    // resurrect it — ADR 0041's own no-resurrection rule). A new run
    // never starts on a resident authority; replacement requires a
    // confirmed stop (ADR 0043 decision 33's retirement clause).
    let ended_phase = super::phase_str(sot_log::wire::SupervisorPhase::EndedNoRespawn);
    if settled_phase == ended_phase {
        // RB: a passive Reconnect never resets an ended run -- only a real Selection may retire+reset it below.
        if intent == ActivationIntent::Reconnect {
            return LockedStep::Done(Ok(spawned));
        }
        // Round-9 BLOCKER fast path: a PRIOR pass of this SAME
        // activation already retired this row (see `own_spawn`'s own
        // doc above) and the resident authority is STILL that exact
        // fresh spawn -- reset it directly, no second stop, no
        // second spawn. Without this, a transient `retired_phase`
        // below sends this function back to `WaitForSettle`, and the
        // NEXT pass re-enters this exact branch from scratch with no
        // memory of the spawn it just made, stopping and respawning
        // AGAIN -- a row whose recovery consistently outlasts
        // `SPAWN_SETTLE_DEADLINE` would then never converge.
        let already_fresh = own_spawn.is_some_and(|identity| ws.current_supervisor() == Some(identity));
        if !already_fresh {
            // Retire the resting authority (attach's own job, not
            // resume's) before minting a new run over it: the WAITING
            // `stop` (confirmed exit, never `stop_and_warn`) first -- an
            // `Err` here leaves the row untouched, nothing replaced --
            // then a fresh spawn via the same guarded resume body every
            // other caller uses. Sending `reset` straight to the OLD
            // resident process (the prior behaviour) would let IT mint
            // the new voyage and spawn the new leg from whatever binary
            // it cached at its own start.
            if let Err(e) = sot_log::supervisor_client::stop(&state_dir) {
                return LockedStep::Done(Err(format!("capsule workspace retire (stop before reset) failed: {e}")));
            }
            let argv = match agent_argv(agent_kind, Some(project_root)) {
                Ok(a) => a,
                Err(e) => return LockedStep::Done(Err(e)),
            };
            let retired_phase = match start_supervisor(
                state_root, workspace_id, StartMode::Resume, &argv, project_root, agent_name, slug, workspaces.clone(),
            ) {
                Ok(p) => p,
                Err(e) => return LockedStep::Done(Err(e)),
            };
            // Remember which authority THIS pass just spawned (a
            // fresh probe, not `retired_phase` alone, since that
            // carries no identity) so a LATER pass -- if this one's
            // own settle below finds it still transient -- recognizes
            // its own child instead of retiring it all over again.
            // Always OVERWRITES, never merely sets: a probe that
            // comes back anything other than `Phase` (the spawn died
            // before this could even read it, say) must CLEAR the
            // old identity too, or a later pass could wrongly credit
            // this attempt with a PRIOR pass's now-dead spawn.
            use crate::workspaces::Observation;
            *own_spawn = match probe(&state_dir) {
                (_, Observation::Phase { supervisor, .. }) => Some(supervisor),
                _ => None,
            };
            // Ruling (round 8): the SAME rule applies to the retire
            // arm's own respawn -- a transient `retired_phase` (this
            // fresh authority still recovering) is not yet the
            // honest "it settled somewhere other than
            // ended_no_respawn" failure reported below; wait for it
            // to actually rest first, same as every other decision
            // point in this function.
            if !is_resting_phase(retired_phase, true) {
                return LockedStep::WaitForSettle;
            }
            if retired_phase != ended_phase {
                return LockedStep::Done(Err(format!(
                    "capsule workspace retire: resumed authority settled to {retired_phase} instead of ended_no_respawn"
                )));
            }
        }
        // The second of the two resets in this tree, and the other is
        // `reauth::mint_replacement_voyage`; both go through
        // [`reset_run`]. Its doc comment carries the long form.
        return LockedStep::Done(match reset_run(&workspaces, workspace_id, &state_dir) {
            // Mints a fresh voyage on the SAME epoch; the observer's next round supersedes the latch.
            Ok(_new_voyage) => Ok(Some(())),
            Err(e) => Err(format!("capsule workspace reset (after retiring an ended run) failed: {e}")),
        });
    }
    LockedStep::Done(Ok(spawned))
}

#[cfg(test)]
mod is_resting_phase_tests {
    use super::*;

    #[test]
    fn resting_phases_never_wait_regardless_of_watchdog() {
        for phase in [NEVER_STARTED_PHASE, FOREIGN_PHASE, "ready", "ended_no_respawn", "terminal"] {
            assert!(is_resting_phase(phase, true), "{phase} must be resting with a watchdog");
            assert!(is_resting_phase(phase, false), "{phase} must be resting with no watchdog");
        }
    }

    #[test]
    fn unreachable_rests_only_with_no_watchdog() {
        assert!(is_resting_phase(UNREACHABLE_PHASE, false), "nobody else will ever act -- this caller must");
        assert!(!is_resting_phase(UNREACHABLE_PHASE, true), "a live watchdog owns the restart -- wait for it");
    }

    #[test]
    fn transient_phases_always_wait_even_with_no_watchdog() {
        // Codex review round 6/7 BLOCKERs: a settle timeout (the
        // watchdog's own, OR this call's own fresh spawn) can return
        // "starting" -- this must NEVER be treated as a decision
        // point, watchdog or not (an adopted authority with no
        // watchdog at all is still mid-flight here too).
        for phase in ["starting", "ending"] {
            assert!(!is_resting_phase(phase, true), "{phase} is transient even with a watchdog");
            assert!(!is_resting_phase(phase, false), "{phase} is transient even with no watchdog");
        }
    }
}
