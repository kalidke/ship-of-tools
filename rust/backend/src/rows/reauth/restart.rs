//! The restart runner: ends the old leg, waits for its authority to rest, mints the replacement voyage and
//! spawns the new leg, behind the `RestartEffects` seam the tests fake.

use super::*;

/// Whether an `end_run` outcome means the run is OVER, so a replacement
/// supervisor may be spawned for this row. Exhaustive on purpose: a new
/// `EndRunOutcome` variant must be classified here as deliberately as in
/// `handlers.rs`'s `capsule_destroy_outcome_of`, which partitions the same
/// enum for the same underlying question ("does anything still hold this
/// row?"). Anything not-over leaves the leg exactly as it is — still
/// running on the old login — so the caller rolls the RECORD back to that
/// same login: a row nobody could replace must not be a row whose record
/// already says it was.
fn run_ended(outcome: &crate::capsule_workspace::EndRunOutcome) -> Result<(), String> {
    use crate::capsule_workspace::EndRunOutcome as O;
    match outcome {
        O::RecordVerified | O::RecordClosed | O::AlreadyEnded | O::Terminal | O::Unheld | O::Orphaned => Ok(()),
        O::Starting => Err("the supervisor was still starting".to_string()),
        O::NotEnded(detail) => Err(detail.clone()),
    }
}

/// The four effects a revival is made of, behind one name so the ORDER they
/// happen in is pinned by a test instead of by adjacency.
///
/// INVARIANT THIS CARRIES: a revival is three effects — end the old leg,
/// spawn the replacement, mint its voyage — and a path that performs two of
/// them produces a row that LOGS as revived and has no leg. That failure is
/// an ABSENT call, and no pure function can observe an absence:
/// [`ready_to_mint`] can be entirely correct, fully tested, and never
/// invoked. This is the only thing in the module that can see that, and it
/// is the only reason it exists.
///
/// Every method mirrors the signature of the real function it forwards to,
/// so [`LiveSupervisor`]'s impl is a literal forward and nothing can drift
/// between the seam and the thing it stands for. `spawn_replacement` is the
/// exception: its real call takes eight arguments, all read off the plan.
pub(crate) trait RestartEffects {
    fn query_status(&self, state_dir: &Path) -> Result<sot_log::attach_client::supervisor_client::StatusReport, String>;
    fn end_run(
        &self,
        state_dir: &Path,
        reason: &str,
        root_canonicalized: bool,
    ) -> std::io::Result<crate::capsule_workspace::EndRunOutcome>;
    fn spawn_replacement(&self, plan: &ReauthRestart) -> Result<&'static str, String>;
    fn reset(&self, workspaces: &Workspaces, workspace_id: &str, state_dir: &Path) -> Result<String, String>;
}

/// The production impl, and the ONLY place in this module that names the
/// real `end_run`, `start_supervisor`, `query_status` or `reset_run`. A future
/// edit that calls one of them directly from [`restart_blocking`] defeats
/// the ordering test silently; the guard is that each of those four paths
/// appears exactly once in this file, inside this impl. Rust cannot enforce
/// that, so it is stated here and checked by grep in the lane's evidence.
pub(crate) struct LiveSupervisor;

impl RestartEffects for LiveSupervisor {
    fn query_status(&self, state_dir: &Path) -> Result<sot_log::attach_client::supervisor_client::StatusReport, String> {
        sot_log::attach_client::supervisor_client::query_status(state_dir).map(|(s, _)| s).map_err(|e| e.to_string())
    }
    fn end_run(
        &self,
        state_dir: &Path,
        reason: &str,
        root_canonicalized: bool,
    ) -> std::io::Result<crate::capsule_workspace::EndRunOutcome> {
        crate::capsule_workspace::end_run(state_dir, reason, root_canonicalized)
    }
    fn spawn_replacement(&self, plan: &ReauthRestart) -> Result<&'static str, String> {
        crate::capsule_workspace::start_supervisor(
            &plan.state_root,
            &plan.row.workspace_id,
            crate::capsule_workspace::StartMode::Resume,
            &plan.argv,
            &plan.row.project_root,
            &plan.row.agent_name(),
            &plan.row.slug,
            plan.workspaces.clone(),
        )
    }
    fn reset(&self, workspaces: &Workspaces, workspace_id: &str, state_dir: &Path) -> Result<String, String> {
        crate::capsule_workspace::reset_run(workspaces, workspace_id, state_dir)
    }
}

/// End the row's current leg and spawn its replacement on the new account.
/// BLOCKING — the caller runs it via `spawn_blocking`, AFTER the accept
/// frame is physically written. Nothing here can answer the caller (it is
/// the process being replaced), so every outcome is a log line — and every
/// outcome that leaves the OLD leg running is a [`ReauthRestart::rollback`]
/// too: the record may not claim a switch that did not happen. Once the leg
/// IS ended the record stands, whatever the replacement spawn does: every
/// later start path reads the account off the registry.
pub fn restart_blocking(plan: ReauthRestart, fx: &dyn RestartEffects) {
    let state_dir = crate::capsule_workspace::state_dir_for(&plan.state_root, &plan.row.workspace_id);
    // Read off the ROW, not off a copy taken before the ack: what the
    // replacement actually spends is whatever the registry says when
    // `spawn_and_watch` resolves it, so naming that same value here keeps
    // the log and the spawn from ever disagreeing.
    let account = plan.row.account();
    let reason = format!("reauth to account {:?}", discovery_name(&account));
    // Who holds the authority BEFORE anything is retired. Read here and
    // nowhere later: `end_run` is about to stop this process, so after it
    // there is nothing left to identify, and the whole point is to notice
    // afterwards if it is STILL the one answering. See
    // [`supervisor_identity`].
    let retired = supervisor_identity(fx, &state_dir);
    let outcome = match fx.end_run(&state_dir, &reason, plan.root_canonicalized) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(
                workspace_id = %plan.row.workspace_id, error = %e,
                "workspace.reauth: could not end the row's leg, so the switch did not happen; rolling the record back to the login the leg still spends"
            );
            return plan.rollback();
        }
    };
    if let Err(detail) = run_ended(&outcome) {
        tracing::warn!(
            workspace_id = %plan.row.workspace_id, detail = %detail,
            "workspace.reauth: the row's run did not end, so no replacement was spawned; rolling the record back to the login the leg still spends"
        );
        return plan.rollback();
    }
    // `StartMode::Resume` — a reauth is never a row's first-ever run, and
    // the account itself is read back off the registry inside this call
    // (`spawn_and_watch`), which is why the record had to move first.
    let phase = match fx.spawn_replacement(&plan) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                workspace_id = %plan.row.workspace_id, error = %e,
                "workspace.reauth: the replacement leg did not spawn; the row rests until it is opened again, on the new account"
            );
            return;
        }
    };
    // A spawn is only two thirds of a revival, and this is the third.
    // `StartMode::Resume` DELIBERATELY never resurrects an ended run (ADR
    // 0041's no-resurrection rule), so the replacement settles at
    // `ended_no_respawn` BY DESIGN: a live supervisor, holding this row's
    // producer argv, with no voyage and nothing on the pty. `reset` mints
    // the voyage the leg actually runs in. `ensure_started`'s `Selection`
    // arm does all three (retire, resume, reset); this path used to do the
    // first two and stop, which left a row that logged as spawned and had
    // no leg at all — data-safe, and silent, which is worse.
    //
    // Why the reset belongs HERE and may not be left to the next attach:
    // `plan.argv` carries `--resume <id>` from `claude_resume_argv`, built
    // for this one call, and that id is never persisted on the row. An
    // attach retires this supervisor and respawns from the ordinary
    // `agent_argv`, whose `--continue` selects by recency rather than by
    // id. `--resume` names the conversation outright, so the reset belongs
    // to the one call that still holds the id.
    //
    // But NOT at the instant the spawn returns. `start_supervisor`'s own
    // settle is best effort: it waits `SPAWN_SETTLE_DEADLINE` (2s) and on
    // timeout WARNS and hands back the transient phase anyway. A reauth's
    // state dir is the worst case for that — `end_run` has just run, so
    // the fresh authority has journal reconciliation to do before it can
    // rest, and the attach path's own comment records that such a recovery
    // "can report `starting` through its own recovery for many seconds".
    // `Reset` is admissible ONLY from `EndedNoRespawn`, so a reset fired
    // two seconds into a three-second recovery is REFUSED, leaving exactly
    // the live-supervisor-no-voyage-no-leg row this change exists to
    // prevent — with a red log instead of a green one, which is no better
    // for the conversation. So wait for the authority to rest on its own
    // clock, and then let the phase decide.
    let voyage = match mint_replacement_voyage(fx, &plan, &state_dir, retired) {
        Ok(voyage) => voyage,
        Err(MintRefusal::NeverAnswered(detail)) => {
            tracing::error!(
                workspace_id = %plan.row.workspace_id, account = %discovery_name(&account),
                spawn_phase = phase, detail = %detail,
                "workspace.reauth: the replacement authority never answered, so no voyage was minted and this row has no leg"
            );
            return;
        }
        Err(MintRefusal::Refused { settled, detail }) => {
            tracing::error!(
                workspace_id = %plan.row.workspace_id, account = %discovery_name(&account),
                phase = settled, detail = %detail,
                "workspace.reauth: refusing to mint a voyage on this authority, so the switch left no new leg"
            );
            return;
        }
        Err(MintRefusal::MintedNothing { settled, error }) => {
            tracing::error!(
                workspace_id = %plan.row.workspace_id, account = %discovery_name(&account),
                phase = settled, error = %error,
                "workspace.reauth: the reset minted no voyage, so this row has no leg; \
                 opening the row retires this authority and revives it on the conversation `--continue` selects"
            );
            return;
        }
    };
    tracing::info!(
        workspace_id = %plan.row.workspace_id, account = %discovery_name(&account),
        voyage = %voyage,
        // Deliberately narrower than "the leg is running": `reset`
        // answers as soon as the pointer moves, and the leg is spawned
        // after that, in the supervisor's own Resetting -> Spawning
        // transition, where it can still fail into `terminal`. A
        // minted voyage is evidence of the third step being taken, not
        // of a process on the pty.
        "workspace.reauth: a fresh voyage was minted for the replacement leg on the new account"
    );
}

/// Why a revival's tail stopped short of the mint. One variant per line
/// [`restart_blocking`] owes its log: the three failures are not
/// interchangeable to whoever reads it — nothing answered, something
/// answered and was refused, or the mint itself answered nothing — and two
/// of them carry the phase the authority settled at, which only the tail
/// has read. THE LOG RECORDS ARE THE CONTRACT, and one `String` cannot
/// carry three of them.
enum MintRefusal {
    NeverAnswered(String),
    Refused { settled: &'static str, detail: String },
    MintedNothing { settled: &'static str, error: String },
}

/// The revival tail: wait for the replacement authority to come to rest on
/// its own clock, judge it, and mint its voyage. ONE name for the rule, so
/// [`restart_blocking`] above reads as exactly the three effects it is.
///
/// There is a SECOND reset in this tree — `ensure_started_locked`, in
/// `capsule_workspace.rs`, reached when a selection finds the row resting
/// at `ended_no_respawn`. The two are deliberately not shared, and the next
/// reader who finds two of them must not have to re-derive why:
///
///   1. How the predecessor is ended. Attach sends a WAITING stop, with a
///      confirmed exit; a reauth ends the RUN, which also closes the record
///      and whose stop is best effort — which is why only this path owes
///      the identity check [`supervisor_identity`] describes.
///   2. What the replacement spends. Attach resolves the row's ordinary
///      agent argv, whose `--continue` selects by recency; a reauth spends
///      the plan's `--resume <id>`, which selects by name and is never
///      persisted on the row, so this is the only call that can mint it.
///   3. How the wait is performed. Attach returns a wait step that RELEASES
///      the row guard and re-runs its whole decision from scratch, because
///      the watchdog takes that guard while restarting a crashed leg. This
///      path polls in place while HOLDING the guard, which is safe by
///      construction: a resumed run is never resurrected, so its
///      replacement rests with no leg at all and no leg can exit. Merging
///      the two waits would break one of them — that is the difference
///      that forecloses unification.
///   4. What identity means. Attach uses it as a fast path to SKIP the
///      retire; this path uses it as a REFUSAL. Opposite polarity, opposite
///      consequence.
fn mint_replacement_voyage(
    fx: &dyn RestartEffects,
    plan: &ReauthRestart,
    state_dir: &Path,
    retired: Option<(u32, u64)>,
) -> Result<String, MintRefusal> {
    let report = wait_until_resting(fx, state_dir).map_err(MintRefusal::NeverAnswered)?;
    let settled = crate::capsule_workspace::phase_str(report.phase);
    if let Err(detail) = ready_to_mint(report.phase, retired, (report.pid, report.created)) {
        return Err(MintRefusal::Refused { settled, detail });
    }
    match fx.reset(&plan.workspaces, &plan.row.workspace_id, state_dir) {
        Ok(voyage) if !voyage.trim().is_empty() => Ok(voyage),
        other => Err(MintRefusal::MintedNothing {
            settled,
            error: other.err().unwrap_or_else(|| "the reset answered an empty voyage".into()),
        }),
    }
}

/// How long to let the replacement authority recover before giving up on
/// it. `start_supervisor`'s own 2s settle is too short to be believed here
/// (see the call site), and this is a background task on the reauth op, so
/// the cost of waiting is latency nobody is watching, while the cost of
/// not waiting is a refused reset and a row with no leg.
const REPLACEMENT_SETTLE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);
const REPLACEMENT_SETTLE_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// The identity of whatever authority currently answers for this row, as
/// the supervisor itself reports it. `None` when nothing answers.
///
/// This exists for one failure this path cannot otherwise see. `end_run`'s
/// own stop is BEST EFFORT: when it leaks, the old supervisor stays
/// resident, this call's `sot-capsule` exits at the authority fence, the
/// spawn still reports `Ok`, and every probe afterwards reads that OLD
/// process resting at `ended_no_respawn` — indistinguishable from a
/// healthy replacement. Minting there would have the old authority spawn
/// the leg from ITS cached argv and ITS environment, the old account
/// included, and this path would log a success naming the new one. So
/// remember who was there before, and require that it changed.
fn supervisor_identity(fx: &dyn RestartEffects, state_dir: &std::path::Path) -> Option<(u32, u64)> {
    fx.query_status(state_dir).ok().map(|s| (s.pid, s.created))
}

/// Polls the replacement authority's own `status` until it rests, or until
/// [`REPLACEMENT_SETTLE_DEADLINE`]. Answers the last report it managed to
/// read; `Err` only when nothing ever answered, since a recovering
/// authority legitimately refuses connections for a while and a single
/// failed read says nothing.
fn wait_until_resting(
    fx: &dyn RestartEffects,
    state_dir: &std::path::Path,
) -> Result<sot_log::attach_client::supervisor_client::StatusReport, String> {
    let deadline = std::time::Instant::now() + REPLACEMENT_SETTLE_DEADLINE;
    let mut last: Option<sot_log::attach_client::supervisor_client::StatusReport> = None;
    loop {
        match fx.query_status(state_dir) {
            Ok(report) => {
                if phase_rests(report.phase) {
                    return Ok(report);
                }
                last = Some(report);
            }
            Err(e) => {
                if last.is_none() && std::time::Instant::now() >= deadline {
                    return Err(format!("no status answered within {REPLACEMENT_SETTLE_DEADLINE:?}: {e}"));
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return last.ok_or_else(|| format!("no status answered within {REPLACEMENT_SETTLE_DEADLINE:?}"));
        }
        std::thread::sleep(REPLACEMENT_SETTLE_POLL);
    }
}

/// Whether a phase is one the authority will stay at until somebody acts.
/// The same partition `capsule_workspace`'s own `is_resting_phase` draws,
/// restricted to the phases a live supervisor can report over `status` —
/// this caller has just spawned one and is holding its reply, so the
/// "nothing is there" phases that function also admits cannot arise here.
fn phase_rests(phase: sot_log::lane::wire::SupervisorPhase) -> bool {
    use sot_log::lane::wire::SupervisorPhase as P;
    matches!(phase, P::Ready | P::EndedNoRespawn | P::Terminal)
}

/// Whether this call may mint the replacement's voyage — from the phase
/// its authority settled at, and from whether that authority is actually a
/// NEW one. Pure and exhaustive so both rules are testable without a
/// supervisor, the same reason [`run_ended`] is factored out above.
///
/// This encodes the 0.6.6 defect it was written for and the two the review
/// of that fix found. `restart_blocking` used to report success on a
/// `start_supervisor` success ALONE, with the settled phase merely printed
/// in the same line; the first version of the fix then minted regardless
/// of the phase, merely warning when it was unexpected. Both are wrong in
/// the same direction — they announce a revival they have not established:
///
///   * `EndedNoRespawn` is the ONLY phase `Reset` is admissible from, so
///     it is the only one where minting can succeed at all;
///   * `Ready` means a leg is ALREADY live — an `end_run` whose stop ended
///     the authority but not the leg, which `--resume` then adopted — so
///     the row has a leg, on the OLD account, and minting is refused with
///     "a leg is currently live";
///   * `Terminal`, or any phase still transient at the deadline, is a
///     replacement that did not come up, and a reset would be refused.
///
/// The identity rule is separate and catches the leaked retire described
/// on [`supervisor_identity`]: an authority that is the same process the
/// switch was supposed to retire must never be minted on, whatever phase
/// it rests at.
fn ready_to_mint(
    phase: sot_log::lane::wire::SupervisorPhase,
    retired: Option<(u32, u64)>,
    resident: (u32, u64),
) -> Result<(), String> {
    use sot_log::lane::wire::SupervisorPhase as P;
    if retired == Some(resident) {
        return Err(format!(
            "the authority answering is the same process the switch was supposed to retire (pid {}), \
             so the retire leaked and its own cached argv and account would spawn the leg",
            resident.0
        ));
    }
    match phase {
        P::EndedNoRespawn => Ok(()),
        P::Ready => Err("a leg is already live on this row, so the retire did not take; it runs on the login the switch moved away from".into()),
        other => Err(format!(
            "the replacement authority rests at {:?} rather than the phase a resumed run must rest at, and a reset is admissible only from ended_no_respawn",
            crate::capsule_workspace::phase_str(other)
        )),
    }
}

#[cfg(test)]
#[path = "restart_tests.rs"]
mod tests;
