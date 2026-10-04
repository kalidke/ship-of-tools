//! The one-shot `endrun` and `reset` for fence-acquiring in-process callers.

use super::*;

// ---------------------------------------------------------------------
// endrun / reset: fence-acquiring in-process callers (no supervisor
// running) — "the same TRANSITION, not the same CAPABILITIES"
// ---------------------------------------------------------------------

pub(super) fn endrun_inner(state_dir: &Path, voyage: Option<String>, reason: String) -> crate::Result<i32> {
    let _fence = match crate::supervisor::journal::fence::lock_supervisor(state_dir) {
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
    let outcome = end_run_over_mgmt_lane(&crate::host::state_dir::state_dir_hash(state_dir), &voyage_id, &reason)?;
    match outcome {
        EndRunOutcome::Absent => {
            // raw pipe-NotFound alone is NOT
            // proof the writer is gone -- the capsule removes the pipe
            // NAME before its final writes, seal, and writer-lock
            // release.
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

/// routed through the SAME journaled Reset
/// transaction the live lane uses — "the same TRANSITION, not the same
/// CAPABILITIES" (ADR 0041's own words), applied for real. An earlier
/// version called `reset_pointer` directly with no journal entry at
/// all: a crash mid rename/bootstrap/publish left NOTHING for a LATER
/// `sot-capsule supervise`/`reset` invocation to reconcile against —
/// unlike `endrun_inner`, which stays deliberately journal-free because
/// the CAPSULE's own `run_end_requested` marker is ALREADY a complete,
/// independent crash-recovery mechanism for that operation; Reset has
/// no such secondary marker, so the journal is its ONLY recovery hook.
/// Recovery runs FIRST here too: a prior
/// crashed reset's own active journal entry is reconciled against the
/// world before this invocation reads the pointer or decides anything,
/// exactly like a fresh `supervise` startup — never minting a THIRD
/// identity over an unresolved one.
pub(super) fn reset_inner(state_dir: &Path, voyage: Option<String>) -> crate::Result<i32> {
    let _fence = match crate::supervisor::journal::fence::lock_supervisor(state_dir) {
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
    // a corrupt pointer is a LOUD REFUSAL, never silently treated as
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
    // the SAME journaled-reset transaction body the live lane's own
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
