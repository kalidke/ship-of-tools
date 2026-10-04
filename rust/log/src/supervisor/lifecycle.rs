//! The Lifecycle state machine, its worker threads, leg retirement and the sticky Terminal.

use super::*;

pub(super) fn join_and_warn(handle: JoinHandle<()>, what: &str) {
    if let Err(panic) = handle.join() {
        note(format_args!("the {what} worker thread panicked: {panic:?}"));
    }
}

/// "watchdogs are not deadlines" — at
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
pub(super) fn abandon_worker(handle: JoinHandle<()>, what: &str) {
    note(format_args!(
        "the {what} worker's own watchdog expired; abandoning its thread WITHOUT waiting for it \
         (N7) — it will exit on its own or be torn down with the process"
    ));
    drop(handle);
}

pub(super) fn watchdog_expired(started_at: Instant, bound: Duration, now: Instant) -> bool {
    now.saturating_duration_since(started_at) >= bound
}

// ---------------------------------------------------------------------
// Lifecycle: the authority's own state machine (see module doc)
// ---------------------------------------------------------------------

pub(super) enum Lifecycle {
    /// Journal recovery + pointer discovery, folded into ONE
    /// non-blocking startup step (recovery runs BEFORE pointer
    /// discovery; neither may block the lane).
    Recovering { rx: mpsc::Receiver<RecoveryOutcome>, handle: JoinHandle<()>, started_at: Instant },
    /// The ONE initial placement decision (adopt if live, else consult
    /// the start-mode table).
    InitialProbe { rx: mpsc::Receiver<ProbeOutcome<Process>>, handle: JoinHandle<()>, started_at: Instant },
    /// A fresh owned-spawn attempt in flight — every respawn reaches
    /// this, never `InitialProbe` again.
    Spawning { rx: mpsc::Receiver<ProbeOutcome<Process>>, handle: JoinHandle<()>, started_at: Instant },
    /// A live leg. Stability is judged by [`leg_was_stable`] reading the
    /// leg's OWN recorded `producer_uptime_ms`, never by a
    /// wall-clock `ready_at` this variant no longer carries — an
    /// earlier version's `ready_at.elapsed()` measured THIS PROCESS's
    /// own observation window, which a slow capsule teardown could
    /// inflate past the stability interval with nothing to do with how
    /// long the producer itself actually ran.
    Ready { process: Process },
    /// An `end_run` is in flight. `pending_reply` is the connection
    /// awaiting the DEFERRED reply at `record_closed` — `None` once
    /// delivered, or if that connection disconnected first (fine: the
    /// journal carries the result for a later `query`).
    ///
    /// `process`: the SAME retained handle
    /// `Ready` carried, kept through the transition rather than dropped
    /// at it. `EndRun` is only ever admitted from `Ready` (`handle_command`'s
    /// own admission check), so the leg this handle identifies may exit
    /// on its own between authority ticks WHILE `end_run` is still en
    /// route to it — a race the worker's own re-challenge over the lane
    /// cannot always win. Before this field existed, that race left the
    /// leg an unreaped zombie for the rest of the supervisor's life:
    /// dropping `Ready`'s `process` at this exact transition was the
    /// ONLY reference to it, single-owner reaping having already removed the implicit `Drop`-triggered reap
    /// that used to paper over exactly this. Every place `Ending`
    /// resolves — into `EndedNoRespawn`, `Terminal`, or a respawn via
    /// [`respawn_or_terminal`] — retires this handle via [`retire_leg`]
    /// first, before it is ever dropped: reaped immediately (Linux) if
    /// [`Process::wait`] with a zero timeout confirms it already exited,
    /// a no-op check on Windows (which has no reap concept at all); if it
    /// has NOT exited yet — e.g. `Ending`
    /// resolving `PreBarrierFailed`, where the writer's lock release
    /// (proving the marker check can proceed) can precede the process's
    /// own actual exit — ownership MOVES into `AuthorityState::retired_legs`
    /// instead, polled to completion by [`reap_retired_legs`] once per
    /// main-loop tick. If the worker instead produced its OWN
    /// freshly-proven handle for the SAME leg (`EndRunWorkerResult::Ended`'s
    /// path through `finish_end_run_with_process`), that one is reaped
    /// there as always — a second `waitid` here, on an already-reaped
    /// pidfd, is `ECHILD`, harmless.
    Ending {
        operation_id: String,
        rx: mpsc::Receiver<EndingProgress>,
        handle: JoinHandle<()>,
        started_at: Instant,
        pending_reply: Option<ConnId>,
        process: Process,
    },
    /// A `reset` is in flight — admissible ONLY from `EndedNoRespawn`.
    Resetting { operation_id: String, rx: mpsc::Receiver<ResetWorkerResult>, handle: JoinHandle<()>, started_at: Instant },
    EndedNoRespawn,
    /// A loud, non-restartable stop. STICKY: no transition out of this
    /// variant exists ANYWHERE in this module — `reset` is refused from
    /// it (busy/stale), and `stop` no longer transitions the Lifecycle
    /// AT ALL (see [`AuthorityState::stop_requested`] and the module
    /// doc's own "Stop no longer owns a Lifecycle state" section).
    Terminal { detail: String, entered_at: Instant },
}

pub(super) enum RecoveryOutcome {
    Done { voyage_id: String, ended: bool },
    Fatal { detail: String },
}

pub(super) enum EndingProgress {
    RecordClosed,
    Final(EndRunWorkerResult),
}

pub(super) enum EndRunWorkerResult {
    Ended,
    /// Marker-absent pre-barrier failure: the caller applies the
    /// SAME anti-flap accounting a naturally-exited `Ready` leg gets,
    /// then decides respawn. Never `PendingWriter` — [`spawn_end_run`]
    /// absorbs every `PendingWriter` attempt into its OWN bounded retry
    /// loop and never surfaces it as a final result.
    PreBarrierFailed,
    Fatal(String),
}

pub(super) enum ResetWorkerResult {
    Done { new_voyage: String },
    Fatal(String),
}

impl Lifecycle {
    pub(super) fn wire_phase(&self) -> SupervisorPhase {
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
/// persists. `stop` no longer transitions the
/// Lifecycle at all — see [`AuthorityState::stop_requested`] — so this
/// is no longer also `Stop`'s own "carry the worker forward" mechanism;
/// it exists for `force_terminal` alone now.
///
/// Also retires a retained leg `process`: `Ready` and `Ending` both carry one, and
/// `force_terminal` can jump straight to `Terminal` from EITHER of them
/// (the SAME "outside that state's own transition arm" cases named
/// above) — without this, that `process` would be silently dropped here
/// via `..`, exactly the zombie-leaking gap single-owner reaping removed the implicit `Drop`-triggered reap that
/// used to paper over. [`retire_leg`] reaps it immediately if already
/// exited, otherwise moves it into `retired_legs` rather than dropping it
/// — see `AuthorityState::retired_legs`'s own doc.
fn take_worker_handle(lifecycle: &mut Lifecycle, retired_legs: &mut Vec<Process>) -> Option<JoinHandle<()>> {
    match std::mem::replace(lifecycle, Lifecycle::EndedNoRespawn) {
        Lifecycle::Recovering { handle, .. }
        | Lifecycle::InitialProbe { handle, .. }
        | Lifecycle::Spawning { handle, .. }
        | Lifecycle::Resetting { handle, .. } => Some(handle),
        Lifecycle::Ending { handle, process, .. } => {
            retire_leg(retired_legs, process);
            Some(handle)
        }
        Lifecycle::Ready { process } => {
            retire_leg(retired_legs, process);
            None
        }
        Lifecycle::EndedNoRespawn | Lifecycle::Terminal { .. } => None,
    }
}

/// Single-owner reaping: the retained leg `process` handle a [`Lifecycle::Ready`]/
/// [`Lifecycle::Ending`] state carries, at the moment that state is being
/// LEFT for something else. Reaps immediately (Linux) if a `wait` with a
/// ZERO timeout (never blocking the authority's own tick) confirms it
/// already exited — the common case. Otherwise the leg is still alive
/// RIGHT NOW: ownership MOVES into `retired_legs` rather than being
/// dropped (the bug this closes — a leg that exits a moment after its
/// state is left used to have no owner left at all). See
/// `AuthorityState::retired_legs`'s own doc for the full rationale and
/// [`reap_retired_legs`] for the other half (the main loop's own poll).
pub(super) fn retire_leg(retired_legs: &mut Vec<Process>, process: Process) {
    // `cfg(unix)` rather than `linux`: see `finish_end_run_with_process`'s
    // own reap comment — the gate names "this OS has zombies", and
    // spelling it `linux` made macOS silently skip the reap entirely.
    #[cfg(unix)]
    if matches!(process.wait(Duration::ZERO), Ok(true)) {
        process.reap();
        return;
    }
    #[cfg(windows)]
    if matches!(process.wait(Duration::ZERO), Ok(true)) {
        return;
    }
    retired_legs.push(process);
}

/// The other half of [`retire_leg`]: called once per main-loop tick
/// (`supervise_inner`'s own `MAIN_LOOP_POLL` cadence — no new timer) to
/// give every leg that outlived its own Lifecycle state a chance to be
/// observed dead and reaped. A non-blocking `wait` per entry; a confirmed
/// exit reaps it (Linux) and removes it from the vector, everything else
/// stays for the next tick.
pub(super) fn reap_retired_legs(retired_legs: &mut Vec<Process>) {
    retired_legs.retain(|process| {
        let exited = matches!(process.wait(Duration::ZERO), Ok(true));
        if exited {
            // `cfg(unix)`, not `linux` — same reason as `retire_leg`'s.
            #[cfg(unix)]
            process.reap();
        }
        !exited
    });
}

// ---------------------------------------------------------------------
// Background workers — every OS-facing wait runs on one of these,
// signature `-> ()` uniformly (the real result travels over the
// channel) so EVERY worker's `JoinHandle<()>` is the SAME type
// regardless of which phase spawned it.
// ---------------------------------------------------------------------

pub(super) fn spawn_recovery(state_dir: PathBuf, mode: StartMode) -> (mpsc::Receiver<RecoveryOutcome>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        // Test-only: artificially holds recovery (the wire phase
        // `Lifecycle::Recovering` reports as `Starting`) open past a
        // caller's own settle deadline, so a test can prove that caller
        // waits for the row to actually rest rather than deciding from
        // a snapshot mid-recovery. Inert unless `SOT_TEST_RECOVERY_
        // DELAY_MS` names a positive delay; its own env var, never
        // shared with any other test barrier. Applies to EVERY spawn.
        if let Ok(ms) = std::env::var("SOT_TEST_RECOVERY_DELAY_MS").unwrap_or_default().parse::<u64>() {
            std::thread::sleep(Duration::from_millis(ms));
        }
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

pub(super) fn spawn_initial_probe(
    voyage_id: String,
    voyage_root: PathBuf,
) -> (mpsc::Receiver<ProbeOutcome<Process>>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let episode_deadline = Instant::now() + PROBE_EPISODE;
        let outcome =
            classify::probe_adopt_only(&RealProbeOps, &voyage_id, &voyage_root, episode_deadline, ATTEMPT_INTERVAL);
        let _ = tx.send(outcome);
    });
    (rx, handle)
}

// Same 8th `survival` parameter (ADR 0042 L1a)
// and the same reasoning as `build_run_command`'s own attribute above.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_owned_spawn_attempt(
    capsule_exe: PathBuf,
    voyage_root: PathBuf,
    voyage_id: String,
    cols: u16,
    rows: u16,
    // ADR 0043 decision 21: owned, not borrowed -- moved into this
    // thread (`OwnedFd: Send`) so the Linux lease's own fd stays open in
    // THIS process for the whole `build_run_command`+`spawn` sequence
    // the child's `pre_exec` needs it for.
    lease: SpawnLease,
    survival: Survival,
    producer_argv: Vec<String>,
) -> (mpsc::Receiver<ProbeOutcome<Process>>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let readiness_cutoff = Instant::now() + READINESS_CUTOFF;
        let mut command =
            build_run_command(&capsule_exe, &voyage_root, &voyage_id, cols, rows, &lease, survival, &producer_argv);
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
/// (deferred-reply signal — on EVERY path that can reach it,
/// an earlier version hardcoded `None` for the no-process outcomes,
/// silently starving a `pending_reply` that would then wait forever).
/// Retries [`finish_end_run_without_process`] while it keeps returning
/// `PendingWriter` (the writer's own fence is still held or ambiguous), until it resolves to
/// `Ended`/`PreBarrierFailed` or errors. Bounded from OUTSIDE only — by whichever
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
/// a ONE-SHOT delivery attempt reopened the
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
pub(super) fn reissue_and_reconcile_end_run(
    state_dir: &Path,
    op_id: Option<&str>,
    voyage_id: &str,
    epoch: Option<u64>,
    reason: &str,
    on_closed: Option<&mpsc::Sender<EndingProgress>>,
) -> crate::Result<EndRunReconciliation> {
    // Computed once, outside the reissue loop: the row does not change
    // hash between retries.
    let h = crate::host::state_dir::state_dir_hash(state_dir);
    loop {
        match end_run_over_mgmt_lane(&h, voyage_id, reason) {
            Ok(EndRunOutcome::Ended(process)) => {
                return finish_end_run_with_process(state_dir, op_id, voyage_id, epoch, process, on_closed);
            }
            Ok(EndRunOutcome::Absent) => {
                // Provably nothing left to deliver to — hand off to the
                // wait-only reconcile, which proves the writer's own
                // fence is free before ever trusting the marker.
                return retry_until_writer_resolved(state_dir, op_id, voyage_id, epoch, on_closed);
            }
            Ok(EndRunOutcome::Foreign | EndRunOutcome::Pending) => {
                // Neither proves the capsule is gone — retry delivery,
                // not just the wait.
            }
            Err(e) => {
                // a generic mgmt-lane error
                // (e.g. a connect failure other than NotFound) proves
                // nothing about the capsule's own liveness either —
                // retry delivery rather than falling back to a
                // wait-only reconcile that would never re-send the
                // request this attempt never actually delivered.
                note(format_args!(
                    "end_run_over_mgmt_lane failed ({e}); retrying delivery rather than falling \
                     back to a wait-only reconcile (B4)"
                ));
            }
        }
        std::thread::sleep(ATTEMPT_INTERVAL);
    }
}

pub(super) fn spawn_end_run(
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
/// which calls this directly and synchronously).
pub(super) fn do_reset(state_dir: &Path, operation_id: &str, new_voyage: &str, aside: Option<&str>) -> ResetWorkerResult {
    match reset_pointer(state_dir, new_voyage, aside) {
        Ok(()) => {
            let t = journal::TerminalRecord::ResetDone { new_voyage: new_voyage.to_string() };
            match journal::finish(state_dir, operation_id, &t) {
                Ok(()) => ResetWorkerResult::Done { new_voyage: new_voyage.to_string() },
                Err(e) => ResetWorkerResult::Fatal(bounded_detail(format!("journal finish failed: {e}"))),
            }
        }
        Err(e) => {
            // a FAILED reset_pointer is Terminal -- a half-mutated
            // pointer is the same "operator must investigate" condition
            // this module's own recovery refusal already names for a
            // third, unexplained identity. This journal::finish's OWN
            // failure is never silently
            // ignored either -- logged loud, even though the overall
            // SEVERITY is unchanged either way (Fatal -> Terminal
            // regardless): an operator investigating this failure
            // deserves to know the journal record itself may be missing
            // too.
            let detail = bounded_detail(format!("{e}"));
            let t = journal::TerminalRecord::Failed { detail: detail.clone() };
            if let Err(finish_err) = journal::finish(state_dir, operation_id, &t) {
                note(format_args!(
                    "reset {operation_id} failed ({detail}), and recording that failure in the \
                     journal ALSO failed ({finish_err})"
                ));
            }
            ResetWorkerResult::Fatal(detail)
        }
    }
}

pub(super) fn spawn_reset(
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

/// Transitions `*lifecycle` to `Terminal`, first extracting (and
/// abandoning) any in-flight worker's `JoinHandle` rather than silently
/// dropping it as part of the same assignment that would otherwise
/// discard it unnoticed. Dropping a `JoinHandle` never kills its thread
/// — it keeps running detached until it finishes on its own or the
/// whole process exits — so this is a deliberate, LOGGED abandonment,
/// not a safety hazard; a worker mid-flight when something ELSE already
/// forces Terminal (a journal read failure, a dead accept loop) has
/// nothing further useful to report anyway.
/// abandoning the handle here — rather than
/// blocking to join it — is sound ONLY because `Terminal` is itself
/// unconditionally, boundedly exitable from this point on (the main
/// loop's own exit condition, below, reaches process exit within a
/// small fixed grace regardless of any further lane traffic): an
/// abandoned thread either finishes harmlessly on its own before that
/// happens, or is torn down WITH the process at exit, and nothing
/// downstream ever depends on its result once a FORCED Terminal has
/// already been decided by something else entirely (a dead accept loop,
/// an unreadable journal) — there is no result left to wait for.
/// [`take_worker_handle`] also reaps a retained leg `process` (`Ready`/
/// `Ending`) the SAME jump would otherwise silently drop unreaped — see
/// its own doc.
pub(super) fn force_terminal(lifecycle: &mut Lifecycle, retired_legs: &mut Vec<Process>, detail: String) {
    if let Some(handle) = take_worker_handle(lifecycle, retired_legs) {
        note(format_args!(
            "abandoning an in-flight worker thread while forcing a terminal state ({detail}) — its \
             thread will exit on its own or be torn down with the process"
        ));
        drop(handle);
    }
    *lifecycle = Lifecycle::Terminal { detail, entered_at: Instant::now() };
}

/// Shared "a leg just ended with no `end_run` involved" tail: count
/// against the flap bound, then either go `Terminal` or start a fresh
/// `Spawning` attempt for the CURRENT voyage — read fresh off
/// `authority` every time, never a value captured before the loop began
/// (a live `reset` can change it; a stale local was a real bug this
/// crate already shipped once). `unstable` is the SAME classification the
/// caller just counted against `consecutive_unstable_legs` — when `true`,
/// this respawn strips `config.first_leg_without`'s tokens too (the
/// self-heal: a leg that failed fast on stale argv gets one clean retry).
/// A stable leg's respawn never strips anything, so a healthy row that
/// happens to restart never loses argv it was never wrong to keep.
pub(super) fn respawn_or_terminal(
    consecutive_unstable_legs: &mut u32,
    capsule_exe: &Path,
    config: &SuperviseConfig,
    lease: &LegLease,
    authority: &AuthorityState,
    unstable: bool,
) -> Lifecycle {
    if *consecutive_unstable_legs >= FLAP_THRESHOLD {
        note(format_args!(
            "anti-flap bound reached (consecutive_unstable_legs={consecutive_unstable_legs} >= \
             {FLAP_THRESHOLD}); entering Terminal"
        ));
        return Lifecycle::Terminal { detail: "the anti-flap bound was reached".into(), entered_at: Instant::now() };
    }
    // ADR 0043 decision 21: a fresh handle for THIS spawn attempt --
    // Windows clones the lease's own name (infallible); Linux dups the
    // read end (an OS call, so genuinely fallible -- e.g. `EMFILE`),
    // which this treats as Terminal rather than silently spawning a leg
    // with no parent-death lease at all.
    let spawn_lease = match lease.for_spawn() {
        Ok(l) => l,
        Err(e) => {
            return Lifecycle::Terminal {
                detail: bounded_detail(format!("could not prepare the parent-death lease for a fresh spawn: {e}")),
                entered_at: Instant::now(),
            };
        }
    };
    let voyage_id = authority.voyage_id.clone().expect("respawn is only reachable once voyage_id is Some");
    let voyage_root = voyage_root_path(&authority.state_dir, &voyage_id);
    let argv = if unstable {
        strip_first_leg_tokens(&config.producer_argv, &config.first_leg_without)
    } else {
        config.producer_argv.clone()
    };
    let (rx, handle) = spawn_owned_spawn_attempt(
        capsule_exe.to_path_buf(),
        voyage_root,
        voyage_id,
        config.cols,
        config.rows,
        spawn_lease,
        config.survival,
        argv,
    );
    Lifecycle::Spawning { rx, handle, started_at: Instant::now() }
}


#[cfg(test)]
mod tests {
    use super::*;

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
    /// the worker while jumping straight to Terminal" (`stop` no longer uses this at all — only `force_terminal`
    /// does now). Constructing a real `Ready` variant needs a live,
    /// OS-proven `Process` this unit test has no safe way to
    /// fabricate (see `tests/supervisor/` for that half, exercised
    /// end-to-end against a real process); the worker-bearing states are
    /// what this function actually exists for and are fully exercisable
    /// here.
    #[test]
    fn take_worker_handle_extracts_the_handle_from_a_worker_bearing_state() {
        let mut retired_legs = Vec::new();
        let (_tx, rx) = mpsc::channel::<RecoveryOutcome>();
        let mut recovering = Lifecycle::Recovering { rx, handle: std::thread::spawn(|| {}), started_at: Instant::now() };
        assert!(take_worker_handle(&mut recovering, &mut retired_legs).is_some());

        let mut ended = Lifecycle::EndedNoRespawn;
        assert!(take_worker_handle(&mut ended, &mut retired_legs).is_none());
        assert!(retired_legs.is_empty(), "neither state above carries a leg process to retire");
    }
}
